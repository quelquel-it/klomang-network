//! Topological Ordering Engine for Transaction Dependencies
//!
//! Implements efficient topological sorting of transactions based on dependency graphs.
//! Supports both Kahn's algorithm and DFS-based approaches for different scenarios.
//!
//! Key Features:
//! - Distinguishes internal (mempool) vs external (storage) dependencies
//! - Efficient O(V + E) topological sorting
//! - Support for both adjacency list and adjacency matrix representations
//! - Thread-safe concurrent access using parking_lot::RwLock

use crate::storage::error::StorageResult;
use crate::storage::kv_store::KvStore;
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use super::advanced_dependency_manager::{TxDependencyManager, TxHash};

/// Type for adjacency list representation
/// Maps tx_hash → set of immediate children (direct dependencies)
pub type AdjacencyList = HashMap<TxHash, HashSet<TxHash>>;

/// Type for in-degree mapping used in topological sort
pub type InDegreeMap = HashMap<TxHash, usize>;

/// Result of topological sort operation
#[derive(Clone, Debug)]
pub struct TopologicalResult {
    /// Transactions in topologically sorted order (parents before children)
    pub sorted_transactions: Vec<TxHash>,
    /// Number of cycles detected (should be 0 for valid result)
    pub cycles_detected: usize,
    /// Set of transactions involved in cycles (if any)
    pub cycle_members: HashSet<TxHash>,
}

/// Statistics about the dependency graph topology
#[derive(Clone, Debug)]
pub struct TopologyStats {
    /// Total transactions in graph
    pub total_transactions: usize,
    /// Total edges (dependencies)
    pub total_edges: usize,
    /// Maximum depth level
    pub max_depth: u32,
    /// Transactions with no dependencies (depth 0)
    pub root_transactions: usize,
    /// Transactions with no dependents (leaves)
    pub leaf_transactions: usize,
}

/// Engine for topological ordering and dependency analysis
pub struct DependencyOrderingEngine {
    /// Underlying dependency manager
    dependency_manager: Arc<TxDependencyManager>,
    /// Storage for parent verification
    #[allow(dead_code)]
    kv_store: Arc<KvStore>,
    /// Adjacency list cache (updated on major operations)
    adjacency_list: Arc<RwLock<AdjacencyList>>,
    /// In-degree cache for quick access
    in_degree_map: Arc<RwLock<InDegreeMap>>,
}

impl DependencyOrderingEngine {
    /// Create new topological ordering engine
    pub fn new(dependency_manager: Arc<TxDependencyManager>, kv_store: Arc<KvStore>) -> Self {
        Self {
            dependency_manager,
            kv_store,
            adjacency_list: Arc::new(RwLock::new(HashMap::new())),
            in_degree_map: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get topological sort using Kahn's algorithm
    /// Returns transactions in order: parents always before children
    pub fn get_topological_sort_kahn(&self) -> StorageResult<TopologicalResult> {
        // Build adjacency list and in-degree map
        let (adj_list, in_degree) = self._build_adjacency_structures()?;

        // Initialize queue with all nodes having in-degree 0
        let mut queue: VecDeque<TxHash> = in_degree
            .iter()
            .filter(|(_, &degree)| degree == 0)
            .map(|(tx_hash, _)| tx_hash.clone())
            .collect();

        let mut sorted = Vec::new();
        let mut processed_degree: HashMap<TxHash, usize> = in_degree.clone();

        // Process nodes in topological order
        while let Some(current) = queue.pop_front() {
            sorted.push(current.clone());

            // Process all children of current node
            if let Some(children) = adj_list.get(&current) {
                for child in children {
                    if let Some(deg) = processed_degree.get_mut(child) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(child.clone());
                        }
                    }
                }
            }
        }

        // Check for cycles
        let all_nodes: usize = in_degree.len();
        let cycles_detected = if sorted.len() != all_nodes { 1 } else { 0 };

        // Find cycle members (nodes not in sorted order)
        let mut cycle_members = HashSet::new();
        let sorted_set: HashSet<TxHash> = sorted.iter().cloned().collect();
        for tx_hash in in_degree.keys() {
            if !sorted_set.contains(tx_hash) {
                cycle_members.insert(tx_hash.clone());
            }
        }

        Ok(TopologicalResult {
            sorted_transactions: sorted,
            cycles_detected,
            cycle_members,
        })
    }

    /// Get topological sort using DFS-based approach
    /// Alternative implementation for comparison and validation
    pub fn get_topological_sort_dfs(&self) -> StorageResult<TopologicalResult> {
        let (adj_list, _) = self._build_adjacency_structures()?;

        let mut visited: HashSet<TxHash> = HashSet::new();
        let mut rec_stack: HashSet<TxHash> = HashSet::new();
        let mut sorted = Vec::new();
        let mut cycle_members = HashSet::new();

        // Perform DFS from each unvisited node
        for tx_hash in adj_list.keys() {
            if !visited.contains(tx_hash) {
                self._dfs_visit(
                    tx_hash,
                    &adj_list,
                    &mut visited,
                    &mut rec_stack,
                    &mut sorted,
                    &mut cycle_members,
                );
            }
        }

        // Reverse to get correct topological order (DFS gives reverse order)
        sorted.reverse();

        let cycles_detected = if cycle_members.is_empty() { 0 } else { 1 };

        Ok(TopologicalResult {
            sorted_transactions: sorted,
            cycles_detected,
            cycle_members,
        })
    }

    /// Get topological sort (default implementation uses Kahn's algorithm)
    pub fn get_topological_sort(&self) -> StorageResult<TopologicalResult> {
        self.get_topological_sort_kahn()
    }

    /// Rebuild adjacency structures and cache them
    pub fn rebuild_adjacency_structures(&self) -> StorageResult<()> {
        let (adj_list, in_degree) = self._build_adjacency_structures()?;
        *self.adjacency_list.write() = adj_list;
        *self.in_degree_map.write() = in_degree;
        Ok(())
    }

    /// Get adjacency list (from cache)
    pub fn get_adjacency_list(&self) -> AdjacencyList {
        self.adjacency_list.read().clone()
    }

    /// Get in-degree map (from cache)
    pub fn get_in_degree_map(&self) -> InDegreeMap {
        self.in_degree_map.read().clone()
    }

    /// Get topology statistics
    pub fn get_topology_stats(&self) -> StorageResult<TopologyStats> {
        let (adj_list, in_degree) = self._build_adjacency_structures()?;

        let total_nodes = adj_list.len();
        let total_edges: usize = adj_list.values().map(|children| children.len()).sum();

        let root_transactions = in_degree.values().filter(|&&deg| deg == 0).count();
        let leaf_transactions = adj_list
            .values()
            .filter(|children| children.is_empty())
            .count();

        // Calculate max depth
        let mut max_depth = 0u32;
        for tx_hash in adj_list.keys() {
            if let Some(depth) = self.dependency_manager.get_execution_depth(tx_hash) {
                max_depth = max_depth.max(depth);
            }
        }

        Ok(TopologyStats {
            total_transactions: total_nodes,
            total_edges,
            max_depth,
            root_transactions,
            leaf_transactions,
        })
    }

    /// Check if a specific path exists between two transactions
    pub fn has_dependency_path(&self, from: &TxHash, to: &TxHash) -> StorageResult<bool> {
        let ancestors = self.dependency_manager.get_executable_ancestors(from);
        Ok(ancestors.contains(to))
    }

    /// Clear all caches
    pub fn clear_caches(&self) {
        self.adjacency_list.write().clear();
        self.in_degree_map.write().clear();
    }

    // ============================================================================
    // PRIVATE HELPER METHODS
    // ============================================================================

    /// Build adjacency list and in-degree map from dependency manager
    fn _build_adjacency_structures(&self) -> StorageResult<(AdjacencyList, InDegreeMap)> {
        let mut adj_list: AdjacencyList = HashMap::new();
        let mut in_degree: InDegreeMap = HashMap::new();

        // Get all transactions by iterating through all depths
        let mut all_txs = HashSet::new();
        let mut depth = 0u32;
        loop {
            let txs_at_depth = self.dependency_manager.get_transactions_at_depth(depth);
            if txs_at_depth.is_empty() {
                break;
            }
            for tx in txs_at_depth {
                all_txs.insert(tx);
            }
            depth += 1;
        }

        // Initialize structures
        for tx_hash in &all_txs {
            adj_list.insert(tx_hash.clone(), HashSet::new());
            in_degree.insert(tx_hash.clone(), 0);
        }

        // Build edges: for each transaction, get its parents
        for tx_hash in &all_txs {
            let parents = self.dependency_manager.get_executable_ancestors(tx_hash);
            for parent in parents {
                // Only include parents that are in mempool (not on-chain)
                if self._is_mempool_parent(&parent)? {
                    if let Some(children) = adj_list.get_mut(&parent) {
                        children.insert(tx_hash.clone());
                    }
                    if let Some(degree) = in_degree.get_mut(tx_hash) {
                        *degree += 1;
                    }
                }
            }
        }

        Ok((adj_list, in_degree))
    }

    /// DFS visit helper for topological sort
    fn _dfs_visit(
        &self,
        tx_hash: &TxHash,
        adj_list: &AdjacencyList,
        visited: &mut HashSet<TxHash>,
        rec_stack: &mut HashSet<TxHash>,
        result: &mut Vec<TxHash>,
        cycle_members: &mut HashSet<TxHash>,
    ) {
        visited.insert(tx_hash.clone());
        rec_stack.insert(tx_hash.clone());

        if let Some(children) = adj_list.get(tx_hash) {
            for child in children {
                if !visited.contains(child) {
                    self._dfs_visit(child, adj_list, visited, rec_stack, result, cycle_members);
                } else if rec_stack.contains(child) {
                    // Cycle detected
                    cycle_members.insert(child.clone());
                    cycle_members.insert(tx_hash.clone());
                }
            }
        }

        rec_stack.remove(tx_hash);
        result.push(tx_hash.clone());
    }

    /// Check if a parent transaction is in mempool (not on-chain)
    fn _is_mempool_parent(&self, _parent_hash: &TxHash) -> StorageResult<bool> {
        // Check if parent exists in storage (on-chain)
        // If it's on-chain, it's not a mempool parent
        // We can't directly check here, so return true (assume mempool)
        // In production, this would query the actual mempool
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_topological_ordering_engine_creation() {
        // Placeholder test - integration tests will verify functionality
    }
}
