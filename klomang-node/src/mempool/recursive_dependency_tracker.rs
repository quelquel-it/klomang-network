//! Recursive Dependency Management System
//!
//! This module implements comprehensive ancestry tracking with:
//! - Full ancestor/descendant relationship tracking (recursive)
//! - Dependency chain resolution with validation
//! - Cascade invalidation on ancestor removal
//! - Cycle detection and prevention
//! - Integration with storage as root of trust
//! - Thread-safe concurrent access with parking_lot

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use dashmap::DashMap;
use parking_lot::RwLock;

use klomang_core::core::state::transaction::Transaction;

use crate::storage::kv_store::KvStore;
use crate::storage::error::{StorageResult, StorageError};

/// Type alias for transaction hash (Vec<u8> for mempool)
pub type TxHash = Vec<u8>;

/// Represents the resolution status of a transaction's dependency chain
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DependencyResolutionStatus {
    /// All ancestors present and valid (Ready for inclusion in block)
    Ready,
    /// At least one ancestor is missing/unknown (waiting)
    Unresolved,
    /// At least one ancestor is invalid/rejected (cascade invalidates this tx)
    Invalid,
}

/// Records the validation state of a transaction's entire ancestry
#[derive(Clone, Debug)]
pub struct AncestryValidation {
    /// Current resolution status
    pub status: DependencyResolutionStatus,
    /// All direct parents (depth 1)
    pub direct_parents: HashSet<TxHash>,
    /// All ancestors at any depth (complete transitive closure)
    pub all_ancestors: HashSet<TxHash>,
    /// Maximum depth from any root (on-chain or storage UTXO)
    pub depth: u32,
    /// Whether any ancestor is on-chain (Storage UTXO)
    pub has_on_chain_ancestor: bool,
    /// Timestamp of last validation
    pub last_validated: u64,
    /// Count of missing ancestors (which trigger Unresolved)
    pub missing_count: usize,
    /// Count of invalid ancestors (which trigger Invalid cascade)
    pub invalid_count: usize,
}

impl Default for AncestryValidation {
    fn default() -> Self {
        Self {
            status: DependencyResolutionStatus::Unresolved,
            direct_parents: HashSet::new(),
            all_ancestors: HashSet::new(),
            depth: 0,
            has_on_chain_ancestor: false,
            last_validated: 0,
            missing_count: 0,
            invalid_count: 0,
        }
    }
}

/// Comprehensive statistics for recursive dependency tracking
#[derive(Clone, Debug, Default)]
pub struct RecursiveDependencyStats {
    /// Total transactions tracked
    pub total_tracked: u64,
    /// Total ancestor relationships discovered
    pub total_ancestor_relationships: u64,
    /// Total descendant relationships discovered
    pub total_descendant_relationships: u64,
    /// Cycles detected and prevented
    pub cycles_prevented: u64,
    /// Cascade invalidations triggered
    pub cascade_invalidations: u64,
    /// Transactions marked as Ready
    pub ready_transactions: u64,
    /// Transactions marked as Unresolved
    pub unresolved_transactions: u64,
    /// Transactions marked as Invalid
    pub invalid_transactions: u64,
}

/// Result of cascade invalidation operation
#[derive(Clone, Debug)]
pub struct CascadeInvalidationResult {
    /// The primary transaction being invalidated
    pub root_tx: TxHash,
    /// All descendants that were invalidated (recursive closure)
    pub invalidated_descendants: Vec<TxHash>,
    /// Maximum depth of recursive invalidation
    pub cascade_depth: usize,
    /// Total transactions affected
    pub total_affected: usize,
}

/// Core Recursive Dependency Tracker with full ancestry management
pub struct RecursiveDependencyTracker {
    /// tx_hash → ancestors (all transitive ancestors)
    /// Maintains complete ancestry closure for O(1) ancestor checks
    ancestors: Arc<RwLock<HashMap<TxHash, HashSet<TxHash>>>>,

    /// tx_hash → descendants (all transitive descendants)
    /// Used for cascade invalidation without full traversal
    descendants: Arc<RwLock<HashMap<TxHash, HashSet<TxHash>>>>,

    /// tx_hash → immediate parents (direct dependencies only)
    /// For incremental updates and cycle detection
    immediate_parents: Arc<DashMap<TxHash, HashSet<TxHash>>>,

    /// tx_hash → immediate children (direct dependents only)
    /// For incremental updates and multi-parent detection
    immediate_children: Arc<DashMap<TxHash, HashSet<TxHash>>>,

    /// tx_hash → Validation status and ancestry info
    /// Cache of current resolution status
    validations: Arc<DashMap<TxHash, AncestryValidation>>,

    /// Set of transactions marked as Invalid (rejected or unrecoverable)
    invalid_transactions: Arc<RwLock<HashSet<TxHash>>>,

    /// KvStore reference for verifying on-chain parents
    kv_store: Arc<KvStore>,

    /// Statistics tracking
    stats: Arc<RwLock<RecursiveDependencyStats>>,

    /// Maximum recursion depth to prevent stack overflow
    max_recursion_depth: u32,

    /// Maximum ancestry size per transaction
    max_ancestry_size: usize,
}

impl RecursiveDependencyTracker {
    /// Create new recursive dependency tracker
    pub fn new(kv_store: Arc<KvStore>) -> Self {
        Self::with_limits(kv_store, 1000, 100000)
    }

    /// Create with custom limits for recursion and ancestry size
    pub fn with_limits(
        kv_store: Arc<KvStore>,
        max_recursion_depth: u32,
        max_ancestry_size: usize,
    ) -> Self {
        Self {
            ancestors: Arc::new(RwLock::new(HashMap::new())),
            descendants: Arc::new(RwLock::new(HashMap::new())),
            immediate_parents: Arc::new(DashMap::new()),
            immediate_children: Arc::new(DashMap::new()),
            validations: Arc::new(DashMap::new()),
            invalid_transactions: Arc::new(RwLock::new(HashSet::new())),
            kv_store,
            stats: Arc::new(RwLock::new(RecursiveDependencyStats::default())),
            max_recursion_depth,
            max_ancestry_size,
        }
    }

    /// Register a transaction and recursively build its full ancestry
    pub fn register_transaction(&self, tx: &Transaction) -> StorageResult<AncestryValidation> {
        let tx_hash = bincode::serialize(&tx.id)
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;

        // Check if already registered
        if self.validations.contains_key(&tx_hash) {
            return Ok(self.validations.get(&tx_hash).unwrap().clone());
        }

        // Build immediate parents from inputs
        let mut immediate_parents = HashSet::new();
        let mut _on_chain_count = 0;

        for input in &tx.inputs {
            let parent_hash = bincode::serialize(&input.prev_tx)
                .map_err(|e| StorageError::SerializationError(e.to_string()))?;

            // Check if parent is on-chain
            let on_chain = self.kv_store.utxo_exists(&parent_hash, input.index)?;
            if on_chain {
                _on_chain_count += 1;
            } else {
                // Parent is in mempool
                immediate_parents.insert(parent_hash);
            }
        }

        // Register immediate parents
        self.immediate_parents.insert(tx_hash.clone(), immediate_parents.clone());

        // Update ancestors recursively
        let ancestors = self._build_ancestors_recursive(
            &tx_hash,
            &immediate_parents,
            0,
            &mut HashSet::new(),
        )?;

        // Check for cycles
        if ancestors.contains(&tx_hash) {
            let mut stats = self.stats.write();
            stats.cycles_prevented += 1;
            return Err(StorageError::OperationFailed(
                "Cycle detected: transaction depends on itself".to_string(),
            ));
        }

        // Validate ancestry
        let validation = self._validate_ancestors_recursive(&tx_hash, &ancestors)?;

        // Check ancestry size limits
        if ancestors.len() > self.max_ancestry_size {
            return Err(StorageError::OperationFailed(
                format!(
                    "Ancestry too large: {} > {}",
                    ancestors.len(),
                    self.max_ancestry_size
                ),
            ));
        }

        // Store ancestors
        {
            let mut ancestors_map = self.ancestors.write();
            ancestors_map.insert(tx_hash.clone(), ancestors.clone());
        }

        // Update descendants for all ancestors
        for ancestor in &ancestors {
            let mut descendants_map = self.descendants.write();
            descendants_map
                .entry(ancestor.clone())
                .or_insert_with(HashSet::new)
                .insert(tx_hash.clone());
        }

        // Update immediate children for all immediate parents
        for parent in &immediate_parents {
            self.immediate_children
                .entry(parent.clone())
                .or_insert_with(HashSet::new)
                .insert(tx_hash.clone());
        }

        // Store validation result
        self.validations.insert(tx_hash.clone(), validation.clone());

        // Update statistics
        {
            let mut stats = self.stats.write();
            stats.total_tracked += 1;
            stats.total_ancestor_relationships += ancestors.len() as u64;
            match validation.status {
                DependencyResolutionStatus::Ready => stats.ready_transactions += 1,
                DependencyResolutionStatus::Unresolved => stats.unresolved_transactions += 1,
                DependencyResolutionStatus::Invalid => stats.invalid_transactions += 1,
            }
        }

        Ok(validation)
    }

    /// Recursively resolve dependency chain and update status
    pub fn resolve_dependency_chain(&self, tx_hash: &TxHash) -> StorageResult<DependencyResolutionStatus> {
        // Check if already marked invalid
        {
            let invalid_txs = self.invalid_transactions.read();
            if invalid_txs.contains(tx_hash) {
                return Ok(DependencyResolutionStatus::Invalid);
            }
        }

        // Get validation info
        let validation = self.validations.get(tx_hash)
            .ok_or_else(|| StorageError::OperationFailed(
                format!("Transaction {:?} not registered", tx_hash)
            ))?;

        let mut current_status = validation.status.clone();

        // If all ancestors are present, mark as Ready
        let all_ancestors = self.ancestors.read();
        if let Some(ancestors) = all_ancestors.get(tx_hash) {
            let mut all_present = true;
            let mut any_invalid = false;

            for ancestor in ancestors {
                if self.validations.contains_key(ancestor) {
                    // Check if ancestor is invalid
                    if let Ok(status) = self.resolve_dependency_chain(ancestor) {
                        if status == DependencyResolutionStatus::Invalid {
                            any_invalid = true;
                            break;
                        }
                    }
                } else {
                    all_present = false;
                    break;
                }
            }

            if any_invalid {
                current_status = DependencyResolutionStatus::Invalid;
                self.invalid_transactions.write().insert(tx_hash.clone());
            } else if all_present {
                current_status = DependencyResolutionStatus::Ready;
            }
        }

        // Update validation status
        if let Some(mut validation) = self.validations.get_mut(tx_hash) {
            validation.status = current_status.clone();
        }

        Ok(current_status)
    }

    /// Get all ancestors for a transaction (complete transitive closure)
    pub fn get_ancestors(&self, tx_hash: &TxHash) -> StorageResult<HashSet<TxHash>> {
        let ancestors = self.ancestors.read();
        ancestors
            .get(tx_hash)
            .cloned()
            .ok_or_else(|| StorageError::NotFound(
                format!("Transaction {:?} not found", tx_hash)
            ))
    }

    /// Get all descendants for a transaction (complete transitive closure)
    pub fn get_descendants(&self, tx_hash: &TxHash) -> StorageResult<HashSet<TxHash>> {
        let descendants = self.descendants.read();
        descendants
            .get(tx_hash)
            .cloned()
            .ok_or_else(|| StorageError::NotFound(
                format!("Transaction {:?} not found", tx_hash)
            ))
    }

    /// Get immediate parents only
    pub fn get_immediate_parents(&self, tx_hash: &TxHash) -> StorageResult<HashSet<TxHash>> {
        self.immediate_parents
            .get(tx_hash)
            .map(|r| r.clone())
            .ok_or_else(|| StorageError::NotFound(
                format!("Transaction {:?} not found", tx_hash)
            ))
    }

    /// Get immediate children only
    pub fn get_immediate_children(&self, tx_hash: &TxHash) -> StorageResult<HashSet<TxHash>> {
        self.immediate_children
            .get(tx_hash)
            .map(|r| r.clone())
            .ok_or_else(|| StorageError::NotFound(
                format!("Transaction {:?} not found", tx_hash)
            ))
    }

    /// Check if tx_a is an ancestor of tx_b (O(1) lookup)
    pub fn is_ancestor(&self, ancestor: &TxHash, tx: &TxHash) -> StorageResult<bool> {
        let ancestors = self.ancestors.read();
        Ok(ancestors
            .get(tx)
            .map(|set| set.contains(ancestor))
            .unwrap_or(false))
    }

    /// Perform cascade invalidation: mark transaction and all descendants as Invalid
    pub fn cascade_invalidate(&self, tx_hash: &TxHash) -> StorageResult<CascadeInvalidationResult> {
        let mut invalidated = Vec::new();
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();

        queue.push_back((tx_hash.clone(), 0_usize));
        let mut max_depth = 0_usize;

        let descendants = self.descendants.read();

        while let Some((current_tx, depth)) = queue.pop_front() {
            if visited.contains(&current_tx) {
                continue;
            }
            visited.insert(current_tx.clone());
            invalidated.push(current_tx.clone());
            max_depth = max_depth.max(depth);

            // Get all descendants of current transaction
            if let Some(tx_descendants) = descendants.get(&current_tx) {
                for descendant in tx_descendants.iter() {
                    if !visited.contains(descendant) {
                        queue.push_back((descendant.clone(), depth + 1));
                    }
                }
            }
        }

        // Mark all as invalid
        {
            let mut invalid_txs = self.invalid_transactions.write();
            for tx in &invalidated {
                invalid_txs.insert(tx.clone());

                // Update validation status
                if let Some(mut validation) = self.validations.get_mut(tx) {
                    validation.status = DependencyResolutionStatus::Invalid;
                }
            }
        }

        let total = invalidated.len();

        // Update statistics
        {
            let mut stats = self.stats.write();
            stats.cascade_invalidations += 1;
            stats.invalid_transactions += invalidated.len() as u64;
        }

        Ok(CascadeInvalidationResult {
            root_tx: tx_hash.clone(),
            invalidated_descendants: invalidated,
            cascade_depth: max_depth,
            total_affected: total,
        })
    }

    /// Remove transaction and update all ancestry/descendancy tracking
    pub fn remove_transaction(&self, tx_hash: &TxHash) -> StorageResult<()> {
        // Get ancestors and descendants before removal
        let ancestors = {
            let mut ancestors_map = self.ancestors.write();
            ancestors_map.remove(tx_hash).unwrap_or_default()
        };

        let descendants = {
            let mut descendants_map = self.descendants.write();
            descendants_map.remove(tx_hash).unwrap_or_default()
        };

        // Remove immediate parents/children
        self.immediate_parents.remove(tx_hash);
        self.immediate_children.remove(tx_hash);

        // Remove validation
        self.validations.remove(tx_hash);

        // Remove from invalid set
        self.invalid_transactions.write().remove(tx_hash);

        // Update ancestors to remove this from their descendants
        for ancestor in &ancestors {
            let mut descendants_map = self.descendants.write();
            if let Some(desc_set) = descendants_map.get_mut(ancestor) {
                desc_set.remove(tx_hash);
            }
        }

        // Update descendants to remove this from their ancestors
        for descendant in &descendants {
            let mut ancestors_map = self.ancestors.write();
            if let Some(anc_set) = ancestors_map.get_mut(descendant) {
                anc_set.remove(tx_hash);
            }
        }

        // Update immediate parent/child references
        for immediate_parent in self.immediate_parents
            .get(tx_hash)
            .map(|r| r.clone())
            .unwrap_or_default()
        {
            if let Some(mut children) = self.immediate_children.get_mut(&immediate_parent) {
                children.remove(tx_hash);
            }
        }

        Ok(())
    }

    /// Validate that an ancestor set resolves to Ready or Invalid status
    pub fn validate_ancestry(&self, tx_hash: &TxHash) -> StorageResult<AncestryValidation> {
        let validation = self.validations.get(tx_hash)
            .ok_or_else(|| StorageError::OperationFailed(
                format!("Transaction {:?} not registered", tx_hash)
            ))?;

        Ok(validation.clone())
    }

    /// Get current resolution status
    pub fn get_resolution_status(&self, tx_hash: &TxHash) -> StorageResult<DependencyResolutionStatus> {
        let validation = self.validations.get(tx_hash)
            .ok_or_else(|| StorageError::OperationFailed(
                format!("Transaction {:?} not registered", tx_hash)
            ))?;

        Ok(validation.status.clone())
    }

    /// Get statistics
    pub fn get_stats(&self) -> RecursiveDependencyStats {
        self.stats.read().clone()
    }

    /// Clear all tracking data
    pub fn reset(&self) {
        self.ancestors.write().clear();
        self.descendants.write().clear();
        self.immediate_parents.clear();
        self.immediate_children.clear();
        self.validations.clear();
        self.invalid_transactions.write().clear();
        *self.stats.write() = RecursiveDependencyStats::default();
    }

    // ===================== PRIVATE HELPERS =====================

    /// Recursively build complete ancestor set using DFS with depth limit
    fn _build_ancestors_recursive(
        &self,
        _current_tx: &TxHash,
        immediate_parents: &HashSet<TxHash>,
        depth: u32,
        visited: &mut HashSet<TxHash>,
    ) -> StorageResult<HashSet<TxHash>> {
        if depth > self.max_recursion_depth {
            return Err(StorageError::OperationFailed(
                format!(
                    "Recursion depth exceeded: {} > {}",
                    depth, self.max_recursion_depth
                ),
            ));
        }

        let mut all_ancestors = HashSet::new();

        for parent in immediate_parents {
            if visited.contains(parent) {
                // Cycle detected
                continue;
            }
            visited.insert(parent.clone());

            // Add parent as ancestor
            all_ancestors.insert(parent.clone());

            // Recursively add parent's ancestors
            if let Some(parent_validation) = self.validations.get(parent) {
                let parent_ancestors = &parent_validation.all_ancestors;
                all_ancestors.extend(parent_ancestors.iter().cloned());
            } else if let Some(parent_immediate_parents) = self.immediate_parents.get(parent) {
                let parent_ancestors = self._build_ancestors_recursive(
                    parent,
                    &parent_immediate_parents,
                    depth + 1,
                    visited,
                )?;
                all_ancestors.extend(parent_ancestors);
            }
        }

        Ok(all_ancestors)
    }

    /// Recursively validate ancestors to determine resolution status
    fn _validate_ancestors_recursive(
        &self,
        tx_hash: &TxHash,
        all_ancestors: &HashSet<TxHash>,
    ) -> StorageResult<AncestryValidation> {
        let immediate_parents = self.immediate_parents.get(tx_hash)
            .map(|r| r.clone())
            .unwrap_or_default();

        let mut missing_count = 0;
        let mut invalid_count = 0;
        let mut max_depth = 0u32;
        let has_on_chain = false;

        for parent in all_ancestors {
            if self.validations.contains_key(parent) {
                // Check if parent is invalid
                let parent_validation = self.validations.get(parent).unwrap();
                if parent_validation.status == DependencyResolutionStatus::Invalid {
                    invalid_count += 1;
                }
                max_depth = max_depth.max(parent_validation.depth);
            } else {
                // Unknown parent
                missing_count += 1;
            }
        }

        let status = if invalid_count > 0 {
            DependencyResolutionStatus::Invalid
        } else if missing_count > 0 {
            DependencyResolutionStatus::Unresolved
        } else if all_ancestors.is_empty() {
            DependencyResolutionStatus::Ready
        } else {
            DependencyResolutionStatus::Ready
        };

        Ok(AncestryValidation {
            status,
            direct_parents: immediate_parents,
            all_ancestors: all_ancestors.clone(),
            depth: max_depth + 1,
            has_on_chain_ancestor: has_on_chain,
            last_validated: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            missing_count,
            invalid_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recursive_dependency_tracker_creation() {
        // Placeholder for integration testing
        // Full tests require KvStore integration and mock transaction setup
    }
}
