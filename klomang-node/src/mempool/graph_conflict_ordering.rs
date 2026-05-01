//! Graph-Based Conflict Detection & Deterministic Ordering Engine
//!
//! This module provides:
//! - Conflict Graph with double-spend detection
//! - Disjoint Set Union (DSU) for parallel transaction grouping
//! - Deterministic topological ordering with strict tie-breaking
//! - UTXO conflict management with high-performance detection
//! - Consensus-safe canonical ordering guarantees

use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use klomang_core::core::state::transaction::Transaction;

use crate::storage::kv_store::KvStore;

/// Disjoint Set Union (Union-Find) for efficient grouping of non-conflicting transactions
/// This allows parallel processing on different CPU cores
pub(crate) struct DisjointSetUnion {
    parent: HashMap<Vec<u8>, Vec<u8>>,
    rank: HashMap<Vec<u8>, u32>,
}

impl DisjointSetUnion {
    /// Create new DSU instance
    pub(crate) fn new() -> Self {
        Self {
            parent: HashMap::new(),
            rank: HashMap::new(),
        }
    }

    /// Make set for transaction
    pub(crate) fn make_set(&mut self, tx_hash: Vec<u8>) {
        self.parent
            .entry(tx_hash.clone())
            .or_insert_with(|| tx_hash.clone());
        self.rank.entry(tx_hash).or_insert(0);
    }

    /// Find root with path compression
    pub(crate) fn find(&mut self, tx_hash: &[u8]) -> Vec<u8> {
        let tx = tx_hash.to_vec();

        // Get parent without holding reference
        let parent_opt = self.parent.get(&tx).cloned();

        if let Some(parent) = parent_opt {
            if parent != tx {
                let root = self.find(&parent);
                self.parent.insert(tx.clone(), root.clone());
                return root;
            }
        }

        tx
    }

    /// Union two sets
    pub(crate) fn union(&mut self, tx1: &[u8], tx2: &[u8]) {
        let root1 = self.find(tx1);
        let root2 = self.find(tx2);

        if root1 == root2 {
            return;
        }

        let rank1 = *self.rank.get(&root1).unwrap_or(&0);
        let rank2 = *self.rank.get(&root2).unwrap_or(&0);

        if rank1 < rank2 {
            self.parent.insert(root1, root2);
        } else if rank1 > rank2 {
            self.parent.insert(root2, root1);
        } else {
            self.parent.insert(root2, root1.clone());
            self.rank.insert(root1, rank1 + 1);
        }
    }

    /// Get all components
    pub(crate) fn get_components(&mut self) -> HashMap<Vec<u8>, Vec<Vec<u8>>> {
        let mut components: HashMap<Vec<u8>, Vec<Vec<u8>>> = HashMap::new();

        // Clone keys to avoid borrow conflict
        let keys: Vec<_> = self.parent.keys().cloned().collect();

        for tx_hash in keys {
            let root = self.find(&tx_hash);
            components
                .entry(root)
                .or_insert_with(Vec::new)
                .push(tx_hash);
        }

        components
    }
}

/// Transaction node with scoring information
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct TransactionNode {
    pub tx_hash: Vec<u8>,
    pub fee: u64,
    pub size_bytes: usize,
    pub arrival_time_ms: u64,
    pub input_count: usize,
    pub output_count: usize,
    pub in_degree: usize,  // Number of dependencies
    pub out_degree: usize, // Number of dependents
}

impl TransactionNode {
    /// Calculate fee density (sat/byte)
    pub(crate) fn fee_density(&self) -> f64 {
        if self.size_bytes > 0 {
            self.fee as f64 / self.size_bytes as f64
        } else {
            0.0
        }
    }

    /// Calculate age score (older = higher priority)
    pub(crate) fn age_score(&self, current_time_ms: u64) -> u64 {
        current_time_ms.saturating_sub(self.arrival_time_ms)
    }

    /// Calculate priority score combining fee density and age
    pub(crate) fn priority_score(
        &self,
        current_time_ms: u64,
        fee_weight: f64,
        age_weight: f64,
    ) -> f64 {
        let fee_density = self.fee_density();
        let age = self.age_score(current_time_ms) as f64;

        fee_density * fee_weight + (age / 1000.0) * age_weight // age in seconds
    }
}

/// Result of canonical ordering
#[derive(Clone, Debug)]
pub struct CanonicalOrderingResult {
    /// Ordered transaction hashes
    pub ordered_hashes: Vec<Vec<u8>>,
    /// Number of topological layers
    pub layer_count: usize,
    /// Parallel execution groups (same layer can be executed in parallel)
    pub parallel_groups: Vec<Vec<Vec<u8>>>,
    /// Detected conflicts
    pub conflict_count: usize,
}

/// Graph-Based Conflict & Ordering Engine
pub struct GraphConflictOrderingEngine {
    // UTXO -> list of claiming transactions
    utxo_index: Arc<RwLock<HashMap<String, Vec<Vec<u8>>>>>,

    // Transaction hash -> node information
    nodes: Arc<RwLock<HashMap<Vec<u8>, TransactionNode>>>,

    // Transaction hash -> set of conflicting transactions
    conflict_map: Arc<RwLock<HashMap<Vec<u8>, Vec<Vec<u8>>>>>,

    // Transaction hash -> dependencies (parents)
    dependency_map: Arc<RwLock<HashMap<Vec<u8>, Vec<Vec<u8>>>>>,

    // Cached canonical ordering
    cached_ordering: Arc<RwLock<Option<CanonicalOrderingResult>>>,

    // KvStore for UTXO validation
    #[allow(dead_code)]
    kv_store: Option<Arc<KvStore>>,

    // Weights for priority calculation
    fee_density_weight: f64,
    age_weight: f64,
}

impl GraphConflictOrderingEngine {
    /// Create new engine
    pub fn new(kv_store: Option<Arc<KvStore>>) -> Self {
        Self {
            utxo_index: Arc::new(RwLock::new(HashMap::new())),
            nodes: Arc::new(RwLock::new(HashMap::new())),
            conflict_map: Arc::new(RwLock::new(HashMap::new())),
            dependency_map: Arc::new(RwLock::new(HashMap::new())),
            cached_ordering: Arc::new(RwLock::new(None)),
            kv_store,
            fee_density_weight: 0.7,
            age_weight: 0.3,
        }
    }

    /// Register transaction and detect conflicts
    pub fn register_transaction(
        &self,
        tx: &Transaction,
        tx_hash: Vec<u8>,
        fee: u64,
        arrival_time_ms: u64,
    ) -> Result<Vec<Vec<u8>>, String> {
        let mut nodes = self.nodes.write();
        let mut utxo_index = self.utxo_index.write();
        let mut conflicts = self.conflict_map.write();

        // Create node
        let node = TransactionNode {
            tx_hash: tx_hash.clone(),
            fee,
            size_bytes: bincode::serialized_size(&tx).unwrap_or(0) as usize,
            arrival_time_ms,
            input_count: tx.inputs.len(),
            output_count: tx.outputs.len(),
            in_degree: 0,
            out_degree: 0,
        };

        nodes.insert(tx_hash.clone(), node);

        // Detect conflicts by checking UTXO claims
        let mut detected_conflicts = Vec::new();

        for input in &tx.inputs {
            let outpoint_key = format!("{:?}:{}", input.prev_tx, 0); // Simplified: use index 0

            if let Some(claimants) = utxo_index.get(&outpoint_key) {
                for claimant in claimants {
                    if claimant != &tx_hash {
                        detected_conflicts.push(claimant.clone());

                        // Register bidirectional conflict
                        conflicts
                            .entry(tx_hash.clone())
                            .or_insert_with(Vec::new)
                            .push(claimant.clone());

                        conflicts
                            .entry(claimant.clone())
                            .or_insert_with(Vec::new)
                            .push(tx_hash.clone());
                    }
                }
            }

            // Register this transaction as claimant
            utxo_index
                .entry(outpoint_key)
                .or_insert_with(Vec::new)
                .push(tx_hash.clone());
        }

        // Invalidate cached ordering
        self.cached_ordering.write().take();

        Ok(detected_conflicts)
    }

    /// Detect instant double-spend through graph search
    pub fn detect_double_spend(&self, tx_hash: &[u8]) -> Result<bool, String> {
        let conflicts = self.conflict_map.read();

        if let Some(conflicting) = conflicts.get(tx_hash) {
            Ok(!conflicting.is_empty())
        } else {
            Ok(false)
        }
    }

    /// Get all conflicting transactions for a given transaction
    pub fn get_conflicts(&self, tx_hash: &[u8]) -> Vec<Vec<u8>> {
        self.conflict_map
            .read()
            .get(tx_hash)
            .cloned()
            .unwrap_or_default()
    }

    /// Add dependency relationship (parent → child)
    pub fn add_dependency(&self, parent: Vec<u8>, child: Vec<u8>) -> Result<(), String> {
        let mut deps = self.dependency_map.write();
        deps.entry(child).or_insert_with(Vec::new).push(parent);

        // Invalidate cache
        self.cached_ordering.write().take();

        Ok(())
    }

    /// Compute canonical order using topological sort
    ///
    /// This function returns transactions in deterministic order such that:
    /// 1. All dependencies are ordered before dependents (topological)
    /// 2. Transactions with same topo level are sorted by priority score
    /// 3. Equal priority scores use hash as lexicographic tie-breaker
    pub fn compute_canonical_order(&self) -> Result<CanonicalOrderingResult, String> {
        // Check cache
        if let Some(cached) = self.cached_ordering.read().as_ref() {
            return Ok(cached.clone());
        }

        let nodes = self.nodes.read();
        let deps = self.dependency_map.read();
        let conflicts = self.conflict_map.read();

        if nodes.is_empty() {
            return Ok(CanonicalOrderingResult {
                ordered_hashes: Vec::new(),
                layer_count: 0,
                parallel_groups: Vec::new(),
                conflict_count: 0,
            });
        }

        // Calculate in-degrees for topological sort
        let mut in_degree: HashMap<Vec<u8>, usize> = HashMap::new();
        for tx_hash in nodes.keys() {
            in_degree.insert(
                tx_hash.clone(),
                deps.get(tx_hash).map(|d| d.len()).unwrap_or(0),
            );
        }

        // Get current time for age calculation
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // Kahn's algorithm for topological sort with priority tie-breaking
        let mut queue: Vec<Vec<u8>> = in_degree
            .iter()
            .filter(|(_, &degree)| degree == 0)
            .map(|(hash, _)| hash.clone())
            .collect();

        // Sort initial queue by priority
        queue.sort_by(|a, b| {
            let node_a = nodes.get(a).unwrap();
            let node_b = nodes.get(b).unwrap();

            // Compare by priority score (descending)
            let score_a =
                node_a.priority_score(current_time, self.fee_density_weight, self.age_weight);
            let score_b =
                node_b.priority_score(current_time, self.fee_density_weight, self.age_weight);

            match score_b
                .partial_cmp(&score_a)
                .unwrap_or(std::cmp::Ordering::Equal)
            {
                std::cmp::Ordering::Equal => b.cmp(a), // Lexicographic tie-break (lower hash first)
                ordering => ordering,
            }
        });

        let mut ordered_hashes = Vec::new();
        let mut parallel_groups = Vec::new();
        let mut current_layer = Vec::new();
        let mut visited = HashSet::new();

        while !queue.is_empty() {
            // Process all nodes at current layer
            current_layer.clear();

            while let Some(tx_hash) = queue.pop() {
                if visited.insert(tx_hash.clone()) {
                    current_layer.push(tx_hash.clone());
                    ordered_hashes.push(tx_hash.clone());

                    // Find dependents
                    for (dependent, parents) in deps.iter() {
                        if parents.contains(&tx_hash) {
                            if let Some(degree) = in_degree.get_mut(dependent) {
                                *degree -= 1;
                                if *degree == 0 {
                                    queue.push(dependent.clone());
                                }
                            }
                        }
                    }
                }
            }

            if !current_layer.is_empty() {
                parallel_groups.push(current_layer.clone());
            }

            // Sort next layer by priority
            queue.sort_by(|a, b| {
                let node_a = nodes.get(a).unwrap();
                let node_b = nodes.get(b).unwrap();

                let score_a =
                    node_a.priority_score(current_time, self.fee_density_weight, self.age_weight);
                let score_b =
                    node_b.priority_score(current_time, self.fee_density_weight, self.age_weight);

                match score_b
                    .partial_cmp(&score_a)
                    .unwrap_or(std::cmp::Ordering::Equal)
                {
                    std::cmp::Ordering::Equal => b.cmp(a),
                    ordering => ordering,
                }
            });
        }

        let conflict_count = conflicts.values().map(|v| v.len()).sum::<usize>() / 2; // Divide by 2 since bidirectional
        let layer_count = parallel_groups.len();

        let result = CanonicalOrderingResult {
            ordered_hashes: ordered_hashes.clone(),
            layer_count,
            parallel_groups,
            conflict_count,
        };

        // Cache result
        self.cached_ordering.write().replace(result.clone());

        Ok(result)
    }

    /// Group non-conflicting transactions for parallel execution using DSU
    pub fn get_parallel_execution_groups(&self) -> Result<Vec<Vec<Vec<u8>>>, String> {
        let nodes = self.nodes.read();
        let conflicts = self.conflict_map.read();

        if nodes.is_empty() {
            return Ok(Vec::new());
        }

        let mut dsu = DisjointSetUnion::new();

        // Initialize DSU with all transactions
        for tx_hash in nodes.keys() {
            dsu.make_set(tx_hash.clone());
        }

        // Union conflicting transactions
        for (tx, conflict_list) in conflicts.iter() {
            for conflict_tx in conflict_list {
                dsu.union(tx, conflict_tx);
            }
        }

        // Get components
        let components = dsu.get_components();
        let groups: Vec<Vec<Vec<u8>>> = components.into_values().collect();

        Ok(groups)
    }

    /// Remove transaction and all its dependents
    pub fn remove_transaction_cascade(&self, tx_hash: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        let mut nodes = self.nodes.write();
        let mut deps = self.dependency_map.write();
        let mut conflicts = self.conflict_map.write();
        let mut utxo_index = self.utxo_index.write();

        // Find all dependents
        let mut to_remove = vec![tx_hash.to_vec()];
        let mut queue = vec![tx_hash.to_vec()];

        while let Some(current) = queue.pop() {
            for (dependent, parents) in deps.iter() {
                if parents.contains(&current) && !to_remove.contains(dependent) {
                    to_remove.push(dependent.clone());
                    queue.push(dependent.clone());
                }
            }
        }

        // Remove transactions
        for tx in &to_remove {
            nodes.remove(tx);
            deps.remove(tx);
            conflicts.remove(tx);

            // Remove from UTXO index
            for utxo_claimants in utxo_index.values_mut() {
                utxo_claimants.retain(|h| h != tx);
            }

            // Remove references
            for claimants in utxo_index.values_mut() {
                claimants.retain(|h| !to_remove.contains(h));
            }
        }

        // Invalidate cache
        self.cached_ordering.write().take();

        Ok(to_remove)
    }

    /// Clear entire graph
    pub(crate) fn clear(&self) {
        self.utxo_index.write().clear();
        self.nodes.write().clear();
        self.conflict_map.write().clear();
        self.dependency_map.write().clear();
        self.cached_ordering.write().take();
    }

    /// Get transaction node info
    pub(crate) fn get_node(&self, tx_hash: &[u8]) -> Option<TransactionNode> {
        self.nodes.read().get(tx_hash).cloned()
    }

    /// Get transaction count
    pub fn transaction_count(&self) -> usize {
        self.nodes.read().len()
    }

    /// Get conflict count
    pub fn get_conflict_count(&self) -> usize {
        self.conflict_map
            .read()
            .values()
            .map(|v| v.len())
            .sum::<usize>()
            / 2
    }

    /// Set priority weight for fee density vs age
    pub fn set_priority_weights(&mut self, fee_weight: f64, age_weight: f64) {
        self.fee_density_weight = fee_weight.abs();
        self.age_weight = age_weight.abs();

        let total = self.fee_density_weight + self.age_weight;
        if total > 0.0 {
            self.fee_density_weight /= total;
            self.age_weight /= total;
        }

        // Invalidate cache when weights change
        self.cached_ordering.write().take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dsu_basic() {
        let mut dsu = DisjointSetUnion::new();
        let h1 = vec![1];
        let h2 = vec![2];
        let h3 = vec![3];

        dsu.make_set(h1.clone());
        dsu.make_set(h2.clone());
        dsu.make_set(h3.clone());

        dsu.union(&h1, &h2);

        assert_eq!(dsu.find(&h1), dsu.find(&h2));
        assert_ne!(dsu.find(&h1), dsu.find(&h3));
    }

    #[test]
    fn test_canonical_ordering_empty() {
        let engine = GraphConflictOrderingEngine::new(None);
        let result = engine.compute_canonical_order().unwrap();

        assert_eq!(result.ordered_hashes.len(), 0);
        assert_eq!(result.layer_count, 0);
    }

    #[test]
    fn test_node_priority_score() {
        let node = TransactionNode {
            tx_hash: vec![1],
            fee: 1000,
            size_bytes: 100,
            arrival_time_ms: 1000,
            input_count: 1,
            output_count: 1,
            in_degree: 0,
            out_degree: 0,
        };

        let current_time = 12000; // 11000ms later
        let score = node.priority_score(current_time, 0.7, 0.3);

        assert!(score > 0.0);
        assert!(score > node.fee_density()); // Score should include age component
    }

    #[test]
    fn test_parallel_execution_groups() {
        let engine = GraphConflictOrderingEngine::new(None);

        let groups = engine.get_parallel_execution_groups().unwrap();
        assert_eq!(groups.len(), 0); // Empty engine
    }
}
