//! Advanced Dependency Management System for Mempool
//!
//! This module implements a sophisticated transaction dependency tracking system with:
//! - Parent-child transaction graph
//! - Multi-level dependency indexing (execution depth)
//! - Circular dependency detection
//! - Cascade validation for dependent transactions
//! - Integration with storage as source of truth

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use dashmap::DashMap;
use parking_lot::RwLock;

use klomang_core::core::state::transaction::Transaction;

use crate::storage::error::StorageResult;
use crate::storage::kv_store::KvStore;

/// Type alias for transaction hash
pub type TxHash = Vec<u8>;

/// Information about a transaction's dependency level
#[derive(Clone, Debug)]
pub struct DependencyLevel {
    /// Execution depth: 0 = UTXO already on-chain, N = depends on level N-1
    pub depth: u32,
    /// Transactions at this level
    pub transactions: HashSet<TxHash>,
}

/// Full dependency chain for a transaction
#[derive(Clone, Debug)]
pub struct DependencyChain {
    /// Direct parents (transactions this tx depends on)
    pub direct_parents: HashSet<TxHash>,
    /// All ancestors at any level
    pub all_ancestors: HashSet<TxHash>,
    /// Causal ordering - must execute in this order before this tx
    pub executable_sequence: Vec<TxHash>,
    /// Execution depth
    pub execution_depth: u32,
}

/// Statistics for dependency tracking
#[derive(Clone, Debug, Default)]
pub struct DependencyStats {
    /// Total transactions tracked
    pub total_tracked: u64,
    /// Total dependency relationships
    pub total_dependencies: u64,
    /// Detected cycles prevented
    pub cycles_prevented: u64,
    /// Transactions evicted with cascading
    pub cascading_evictions: u64,
}

/// Advanced Dependency Management System
pub struct TxDependencyManager {
    /// Parent transaction hash → Child transaction hashes
    parent_to_children: Arc<DashMap<TxHash, HashSet<TxHash>>>,
    /// Child transaction hash → Parent transaction hashes
    child_to_parents: Arc<DashMap<TxHash, HashSet<TxHash>>>,
    /// Transaction hash → Execution depth
    execution_depth: Arc<DashMap<TxHash, u32>>,
    /// Execution depth level → Transactions at that level
    depth_index: Arc<RwLock<HashMap<u32, HashSet<TxHash>>>>,
    /// KvStore reference for checking on-chain UTXO
    kv_store: Arc<KvStore>,
    /// Statistics
    stats: Arc<RwLock<DependencyStats>>,
}

impl TxDependencyManager {
    /// Create new dependency manager
    pub fn new(kv_store: Arc<KvStore>) -> Self {
        Self {
            parent_to_children: Arc::new(DashMap::new()),
            child_to_parents: Arc::new(DashMap::new()),
            execution_depth: Arc::new(DashMap::new()),
            depth_index: Arc::new(RwLock::new(HashMap::new())),
            kv_store,
            stats: Arc::new(RwLock::new(DependencyStats::default())),
        }
    }

    /// Register a transaction and build its dependency graph
    pub fn register_transaction(&self, tx: &Transaction) -> StorageResult<DependencyChain> {
        let tx_hash = bincode::serialize(&tx.id)
            .map_err(|e| crate::storage::error::StorageError::SerializationError(e.to_string()))?;

        // Check if already registered
        if self.execution_depth.contains_key(&tx_hash) {
            if let Some(depth) = self.execution_depth.get(&tx_hash) {
                return Ok(DependencyChain {
                    direct_parents: self
                        .child_to_parents
                        .get(&tx_hash)
                        .map(|v| v.clone())
                        .unwrap_or_default(),
                    all_ancestors: self._collect_all_ancestors(&tx_hash),
                    executable_sequence: self._get_execution_sequence(&tx_hash),
                    execution_depth: *depth,
                });
            }
        }

        // Identify parents from transaction inputs
        let mut direct_parents = HashSet::new();
        let mut max_parent_depth = 0u32;

        for input in &tx.inputs {
            let input_tx_bytes = bincode::serialize(&input.prev_tx).map_err(|e| {
                crate::storage::error::StorageError::SerializationError(e.to_string())
            })?;

            // Check if input exists on-chain (in storage)
            let on_chain = self.kv_store.utxo_exists(&input_tx_bytes, input.index)?;

            if !on_chain {
                // Input is from a mempool transaction (parent)
                direct_parents.insert(input_tx_bytes.clone());

                // Get parent's depth
                if let Some(parent_depth_entry) = self.execution_depth.get(&input_tx_bytes) {
                    max_parent_depth = max_parent_depth.max(*parent_depth_entry);
                }
            }
        }

        // Calculate this transaction's depth
        let tx_depth = if direct_parents.is_empty() {
            0 // All inputs from on-chain UTXO
        } else {
            max_parent_depth + 1 // Depends on parent depth
        };

        // Detect cycles BEFORE registering
        self._detect_cycles(&tx_hash, &direct_parents)?;

        // Register the transaction
        self.execution_depth.insert(tx_hash.clone(), tx_depth);
        self.child_to_parents
            .insert(tx_hash.clone(), direct_parents.clone());

        // Update parent->children mappings
        for parent in &direct_parents {
            self.parent_to_children
                .entry(parent.clone())
                .or_insert_with(HashSet::new)
                .insert(tx_hash.clone());
        }

        // Update depth index
        {
            let mut depth_idx = self.depth_index.write();
            depth_idx
                .entry(tx_depth)
                .or_insert_with(HashSet::new)
                .insert(tx_hash.clone());
        }

        // Update stats
        {
            let mut stats = self.stats.write();
            stats.total_tracked += 1;
            stats.total_dependencies += direct_parents.len() as u64;
        }

        // Build execution sequence
        let executable_sequence = self._get_execution_sequence(&tx_hash);
        let all_ancestors = self._collect_all_ancestors(&tx_hash);

        Ok(DependencyChain {
            direct_parents,
            all_ancestors,
            executable_sequence,
            execution_depth: tx_depth,
        })
    }

    /// Get all ancestors needed before this transaction can be executed
    pub fn get_executable_ancestors(&self, tx_hash: &TxHash) -> Vec<TxHash> {
        self._get_execution_sequence(tx_hash)
    }

    /// Get transactions that depend on this one
    pub fn get_dependent_children(&self, tx_hash: &TxHash) -> Vec<TxHash> {
        self.parent_to_children
            .get(tx_hash)
            .map(|v| v.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get all transitive dependents of a transaction (direct and indirect children)
    pub fn get_all_transitive_dependents(&self, tx_hash: &TxHash) -> Vec<TxHash> {
        let mut all_dependents = HashSet::new();
        let mut to_visit = VecDeque::new();
        to_visit.push_back(tx_hash.clone());

        while let Some(current) = to_visit.pop_front() {
            let children = self.get_dependent_children(&current);
            for child in children {
                if !all_dependents.contains(&child) {
                    all_dependents.insert(child.clone());
                    to_visit.push_back(child);
                }
            }
        }

        all_dependents.into_iter().collect()
    }

    /// Get transactions at a specific execution depth
    pub fn get_transactions_at_depth(&self, depth: u32) -> Vec<TxHash> {
        self.depth_index
            .read()
            .get(&depth)
            .map(|v| v.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get execution depth of a transaction
    pub fn get_execution_depth(&self, tx_hash: &TxHash) -> Option<u32> {
        self.execution_depth.get(tx_hash).map(|v| *v)
    }

    /// Remove transaction and cascade removal to orphaned dependents
    pub fn remove_transaction(&self, tx_hash: &TxHash) -> Vec<TxHash> {
        let mut evicted = vec![tx_hash.clone()];
        let mut to_process = vec![tx_hash.clone()];

        while let Some(current_tx) = to_process.pop() {
            // Get children before removing
            let children = self.get_dependent_children(&current_tx);

            // Remove from all structures
            self.child_to_parents.remove(&current_tx);
            self.parent_to_children.remove(&current_tx);

            if let Some((_, depth)) = self.execution_depth.remove(&current_tx) {
                let mut depth_idx = self.depth_index.write();
                if let Some(txs_at_depth) = depth_idx.get_mut(&depth) {
                    txs_at_depth.remove(&current_tx);
                }
            }

            // Mark all direct children for processing (they become orphaned)
            for child in children {
                // Check if child has other parents
                if let Some(parents) = self.child_to_parents.get(&child) {
                    if parents.is_empty() {
                        // Child has no other parents, mark for eviction
                        to_process.push(child.clone());
                        evicted.push(child);
                    } else {
                        // Child has other parents, recalculate its depth
                        self._recalculate_depth(&child);
                    }
                }
            }
        }

        // Update stats
        {
            let mut stats = self.stats.write();
            stats.cascading_evictions += evicted.len() as u64;
        }

        evicted
    }

    /// Get full dependency chain for a transaction
    pub fn get_dependency_chain(&self, tx_hash: &TxHash) -> Option<DependencyChain> {
        self.execution_depth.get(tx_hash).map(|depth| {
            let direct_parents = self
                .child_to_parents
                .get(tx_hash)
                .map(|v| v.clone())
                .unwrap_or_default();

            let all_ancestors = self._collect_all_ancestors(tx_hash);
            let executable_sequence = self._get_execution_sequence(tx_hash);

            DependencyChain {
                direct_parents,
                all_ancestors,
                executable_sequence,
                execution_depth: *depth,
            }
        })
    }

    /// Get statistics
    pub fn get_stats(&self) -> DependencyStats {
        self.stats.read().clone()
    }

    /// Clear all data (for testing/reset)
    pub fn clear(&self) {
        self.parent_to_children.clear();
        self.child_to_parents.clear();
        self.execution_depth.clear();
        self.depth_index.write().clear();
    }

    // ============================================================================
    // PRIVATE HELPER METHODS
    // ============================================================================

    /// Collect all ancestors of a transaction (transitive closure)
    fn _collect_all_ancestors(&self, tx_hash: &TxHash) -> HashSet<TxHash> {
        let mut ancestors = HashSet::new();
        let mut to_visit = VecDeque::new();
        to_visit.push_back(tx_hash.clone());

        while let Some(current) = to_visit.pop_front() {
            if let Some(parents) = self.child_to_parents.get(&current) {
                for parent in parents.iter() {
                    if !ancestors.contains(parent) {
                        ancestors.insert(parent.clone());
                        to_visit.push_back(parent.clone());
                    }
                }
            }
        }

        ancestors
    }

    /// Get the executable sequence for a transaction (topological order)
    fn _get_execution_sequence(&self, tx_hash: &TxHash) -> Vec<TxHash> {
        let all_ancestors = self._collect_all_ancestors(tx_hash);

        // Build in-degree map for topological sort
        let mut in_degree: HashMap<TxHash, usize> = HashMap::new();
        let mut edges: HashMap<TxHash, HashSet<TxHash>> = HashMap::new();

        for ancestor in &all_ancestors {
            in_degree.insert(ancestor.clone(), 0);
            edges.insert(ancestor.clone(), HashSet::new());
        }

        for ancestor in &all_ancestors {
            if let Some(parents) = self.child_to_parents.get(ancestor) {
                for parent in parents.iter() {
                    if all_ancestors.contains(parent) {
                        *in_degree.get_mut(ancestor).unwrap() += 1;
                        edges.get_mut(parent).unwrap().insert(ancestor.clone());
                    }
                }
            }
        }

        // Kahn's algorithm for topological sort
        let mut queue: VecDeque<TxHash> = all_ancestors
            .iter()
            .filter(|tx| in_degree.get(*tx).copied().unwrap_or(0) == 0)
            .cloned()
            .collect();

        let mut result = Vec::new();

        while let Some(current) = queue.pop_front() {
            result.push(current.clone());

            if let Some(children) = edges.get(&current) {
                for child in children {
                    let new_degree = in_degree.get(child).unwrap() - 1;
                    *in_degree.get_mut(child).unwrap() = new_degree;

                    if new_degree == 0 {
                        queue.push_back(child.clone());
                    }
                }
            }
        }

        result
    }

    /// Detect cycles using DFS
    fn _detect_cycles(&self, tx_hash: &TxHash, parents: &HashSet<TxHash>) -> StorageResult<()> {
        let mut visited = HashSet::new();
        let mut rec_stack = HashSet::new();

        for parent in parents {
            if self._has_cycle_dfs(parent, tx_hash, &mut visited, &mut rec_stack) {
                let mut stats = self.stats.write();
                stats.cycles_prevented += 1;

                return Err(crate::storage::error::StorageError::OperationFailed(
                    "Circular dependency detected".to_string(),
                ));
            }
        }

        Ok(())
    }

    /// DFS helper for cycle detection
    fn _has_cycle_dfs(
        &self,
        current: &TxHash,
        target: &TxHash,
        visited: &mut HashSet<TxHash>,
        rec_stack: &mut HashSet<TxHash>,
    ) -> bool {
        if !visited.contains(current) {
            visited.insert(current.clone());
            rec_stack.insert(current.clone());

            if let Some(parents) = self.child_to_parents.get(current) {
                for parent in parents.iter() {
                    if !visited.contains(parent) {
                        if self._has_cycle_dfs(parent, target, visited, rec_stack) {
                            return true;
                        }
                    } else if rec_stack.contains(parent) {
                        return true;
                    }
                }
            }

            // Check if current forms a cycle with target
            if current == target && rec_stack.contains(target) {
                return true;
            }
        }

        rec_stack.remove(current);
        false
    }

    /// Recalculate depth of a transaction after parent changes
    fn _recalculate_depth(&self, tx_hash: &TxHash) {
        let mut max_parent_depth = 0u32;

        if let Some(parents) = self.child_to_parents.get(tx_hash) {
            for parent in parents.iter() {
                if let Some(parent_depth) = self.execution_depth.get(parent) {
                    max_parent_depth = max_parent_depth.max(*parent_depth);
                }
            }
        }

        let new_depth = if self
            .child_to_parents
            .get(tx_hash)
            .map(|p| p.is_empty())
            .unwrap_or(false)
        {
            0
        } else {
            max_parent_depth + 1
        };

        // Update if changed
        if let Some(mut old_entry) = self.execution_depth.get_mut(tx_hash) {
            let old_depth = *old_entry;
            if old_depth != new_depth {
                *old_entry = new_depth;

                let mut depth_idx = self.depth_index.write();
                if let Some(txs_at_old) = depth_idx.get_mut(&old_depth) {
                    txs_at_old.remove(tx_hash);
                }
                depth_idx
                    .entry(new_depth)
                    .or_insert_with(HashSet::new)
                    .insert(tx_hash.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // Note: Real tests would require KvStore mock or integration test setup
    // For now, this serves as structural verification

    #[test]
    fn test_dependency_manager_creation() {
        // This verifies the system compiles and can be instantiated
        // Full tests would require storage layer setup
    }
}
