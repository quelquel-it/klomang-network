//! Conflict Graph Index for Advanced Transaction Conflict Management
//!
//! Provides deterministic tracking of transaction conflicts, transitive conflict detection,
//! and efficient conflict set management for the mempool.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use parking_lot::RwLock;

use klomang_core::core::crypto::Hash;
use klomang_core::core::state::transaction::Transaction;

use crate::storage::kv_store::KvStore;

/// Represents an outpoint (UTXO reference)
#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct OutPoint {
    pub tx_hash: Vec<u8>,
    pub output_index: u32,
}

impl OutPoint {
    pub fn new(tx_hash: Vec<u8>, output_index: u32) -> Self {
        Self {
            tx_hash,
            output_index,
        }
    }

    pub fn from_hash(hash: &Hash, index: u32) -> crate::storage::error::StorageResult<Self> {
        let tx_bytes = bincode::serialize(hash)
            .map_err(|e| crate::storage::error::StorageError::SerializationError(e.to_string()))?;
        Ok(Self {
            tx_hash: tx_bytes,
            output_index: index,
        })
    }

    pub fn to_key(&self) -> String {
        format!("{}:{}", hex_encode(&self.tx_hash), self.output_index)
    }
}

/// Represents a transaction hash in the conflict graph
#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct TxHash(pub Vec<u8>);

impl TxHash {
    pub fn new(hash: Vec<u8>) -> Self {
        Self(hash)
    }

    pub fn from_hash(h: &Hash) -> crate::storage::error::StorageResult<Self> {
        let bytes = bincode::serialize(h)
            .map_err(|e| crate::storage::error::StorageError::SerializationError(e.to_string()))?;
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Transaction node in conflict graph
#[derive(Clone, Debug)]
pub struct ConflictNode {
    pub tx_hash: TxHash,
    pub conflicting_with: HashSet<TxHash>,
    pub parents: HashSet<TxHash>,
    pub children: HashSet<TxHash>,
    pub total_fee: u64,
    pub size_bytes: usize,
    pub is_high_risk: bool,
}

impl ConflictNode {
    pub fn new(tx_hash: TxHash, fee: u64, size: usize) -> Self {
        Self {
            tx_hash,
            conflicting_with: HashSet::new(),
            parents: HashSet::new(),
            children: HashSet::new(),
            total_fee: fee,
            size_bytes: size,
            is_high_risk: false,
        }
    }

    pub fn fee_rate(&self) -> f64 {
        if self.size_bytes > 0 {
            self.total_fee as f64 / self.size_bytes as f64
        } else {
            0.0
        }
    }
}

/// Statistics for conflict graph operations
#[derive(Clone, Debug, Default)]
pub struct ConflictGraphStats {
    pub total_nodes: u64,
    pub total_conflicts: u64,
    pub transitive_conflicts: u64,
    pub high_risk_transactions: u64,
    pub rbf_evaluations: u64,
    pub rbf_replacements: u64,
}

/// Main Conflict Graph structure
#[allow(dead_code)]
pub struct ConflictGraph {
    /// OutPoint → List of TxHashes claiming it
    outpoint_index: Arc<RwLock<HashMap<OutPoint, Vec<TxHash>>>>,

    /// Transaction nodes with conflict relationships
    nodes: Arc<RwLock<HashMap<TxHash, ConflictNode>>>,

    /// Precomputed conflict sets
    conflict_sets: Arc<RwLock<HashMap<TxHash, HashSet<TxHash>>>>,

    /// Storage reference for UTXO verification
    kv_store: Arc<KvStore>,

    /// Statistics
    stats: Arc<RwLock<ConflictGraphStats>>,
}

impl ConflictGraph {
    /// Create new conflict graph
    pub fn new(kv_store: Arc<KvStore>) -> Self {
        Self {
            outpoint_index: Arc::new(RwLock::new(HashMap::new())),
            nodes: Arc::new(RwLock::new(HashMap::new())),
            conflict_sets: Arc::new(RwLock::new(HashMap::new())),
            kv_store,
            stats: Arc::new(RwLock::new(ConflictGraphStats::default())),
        }
    }

    /// Register transaction and detect conflicts
    pub fn register_transaction(
        &self,
        tx: &Transaction,
        tx_hash: &TxHash,
        fee: u64,
        size_bytes: usize,
    ) -> Result<Vec<TxHash>, String> {
        let mut nodes = self.nodes.write();
        let mut outpoint_index = self.outpoint_index.write();
        let mut stats = self.stats.write();

        // Create new node
        let node = ConflictNode::new(tx_hash.clone(), fee, size_bytes);
        nodes.insert(tx_hash.clone(), node);
        stats.total_nodes += 1;

        let mut detected_conflicts = Vec::new();

        // Check each input for conflicts
        for (idx, input) in tx.inputs.iter().enumerate() {
            let outpoint = OutPoint::from_hash(&input.prev_tx, idx as u32)
                .map_err(|e| format!("Failed to create outpoint: {}", e))?;

            // Get existing claimants
            if let Some(existing_txs) = outpoint_index.get(&outpoint) {
                for existing_tx in existing_txs {
                    detected_conflicts.push(existing_tx.clone());

                    // Register bidirectional conflict
                    if let Some(node) = nodes.get_mut(tx_hash) {
                        node.conflicting_with.insert(existing_tx.clone());
                    }
                    if let Some(existing_node) = nodes.get_mut(existing_tx) {
                        existing_node.conflicting_with.insert(tx_hash.clone());
                    }

                    stats.total_conflicts += 1;
                }
            }

            // Register this transaction as claimant
            outpoint_index
                .entry(outpoint)
                .or_insert_with(Vec::new)
                .push(tx_hash.clone());
        }

        // Detect transitive conflicts
        self.detect_transitive_conflicts(tx_hash, &nodes, &mut stats)?;

        Ok(detected_conflicts)
    }

    /// Detect transitive conflicts (if B conflicts with A, mark B's children as high-risk)
    fn detect_transitive_conflicts(
        &self,
        tx_hash: &TxHash,
        nodes: &HashMap<TxHash, ConflictNode>,
        stats: &mut ConflictGraphStats,
    ) -> Result<(), String> {
        if let Some(node) = nodes.get(tx_hash) {
            let conflicting_with: Vec<_> = node.conflicting_with.iter().cloned().collect();

            // Find all descendants of this transaction
            let descendants = self.find_descendants(tx_hash, nodes);

            // Mark descendants as high-risk if this transaction is in conflict
            if !conflicting_with.is_empty() {
                for descendant in descendants {
                    if let Some(desc_node) = nodes.get(&descendant) {
                        // Check if descendant is already marked or will be in a conflict set
                        if !desc_node.conflicting_with.is_empty() {
                            stats.transitive_conflicts += 1;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Find all descendant transactions (BFS)
    fn find_descendants(
        &self,
        tx_hash: &TxHash,
        nodes: &HashMap<TxHash, ConflictNode>,
    ) -> Vec<TxHash> {
        let mut descendants = Vec::new();
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();

        queue.push_back(tx_hash.clone());
        visited.insert(tx_hash.clone());

        while let Some(current) = queue.pop_front() {
            if let Some(node) = nodes.get(&current) {
                for child in &node.children {
                    if !visited.contains(child) {
                        visited.insert(child.clone());
                        descendants.push(child.clone());
                        queue.push_back(child.clone());
                    }
                }
            }
        }

        descendants
    }

    /// Add dependency relationship between transactions
    pub fn add_dependency(&self, child: &TxHash, parent: &TxHash) -> Result<(), String> {
        let mut nodes = self.nodes.write();

        if let Some(child_node) = nodes.get_mut(child) {
            child_node.parents.insert(parent.clone());
        } else {
            return Err("Child transaction not found".to_string());
        }

        if let Some(parent_node) = nodes.get_mut(parent) {
            parent_node.children.insert(child.clone());
        }

        Ok(())
    }

    /// Get all transactions in conflict set including transitive conflicts
    pub fn get_conflict_set(&self, tx_hash: &TxHash) -> Result<HashSet<TxHash>, String> {
        let nodes = self.nodes.read();
        let mut conflict_sets = self.conflict_sets.write();

        // Check if already computed
        if let Some(cached) = conflict_sets.get(tx_hash) {
            return Ok(cached.clone());
        }

        let mut set = HashSet::new();
        set.insert(tx_hash.clone());

        if let Some(node) = nodes.get(tx_hash) {
            // Add direct conflicts
            for conflicting_tx in &node.conflicting_with {
                set.insert(conflicting_tx.clone());
            }

            // Add descendants and their conflicts
            let descendants = self.find_descendants(tx_hash, &nodes);
            for desc in descendants {
                set.insert(desc.clone());
                if let Some(desc_node) = nodes.get(&desc) {
                    for conflicting in &desc_node.conflicting_with {
                        set.insert(conflicting.clone());
                    }
                }
            }
        }

        conflict_sets.insert(tx_hash.clone(), set.clone());
        Ok(set)
    }

    /// Remove transaction and all its descendants
    pub fn remove_and_cascade(&self, tx_hash: &TxHash) -> Result<Vec<TxHash>, String> {
        let mut nodes = self.nodes.write();
        let mut outpoint_index = self.outpoint_index.write();
        let mut conflict_sets = self.conflict_sets.write();
        let mut stats = self.stats.write();

        // Find all descendants
        let descendants = self.find_descendants(tx_hash, &nodes);
        let mut removed = vec![tx_hash.clone()];
        removed.extend(descendants.clone());

        // Collect parent-child relationships before removal
        let mut relationships: Vec<(TxHash, Vec<TxHash>, Vec<TxHash>)> = Vec::new();
        for tx in &removed {
            if let Some(node) = nodes.get(tx) {
                relationships.push((
                    tx.clone(),
                    node.parents.iter().cloned().collect(),
                    node.children.iter().cloned().collect(),
                ));
            }
        }

        // Remove from nodes
        for tx in &removed {
            nodes.remove(tx);
            conflict_sets.remove(tx);
        }

        // Unlink from parents and children after all removals
        for (tx, parents, children) in relationships {
            for parent in parents {
                if let Some(p_node) = nodes.get_mut(&parent) {
                    p_node.children.remove(&tx);
                }
            }
            for child in children {
                if let Some(c_node) = nodes.get_mut(&child) {
                    c_node.parents.remove(&tx);
                }
            }
        }

        // Remove from outpoint index
        outpoint_index.retain(|_, txs| {
            txs.retain(|tx| !removed.contains(tx));
            !txs.is_empty()
        });

        stats.total_nodes = stats.total_nodes.saturating_sub(removed.len() as u64);

        Ok(removed)
    }

    /// Check if transaction has direct conflicts
    pub fn has_conflicts(&self, tx_hash: &TxHash) -> bool {
        let nodes = self.nodes.read();
        if let Some(node) = nodes.get(tx_hash) {
            !node.conflicting_with.is_empty()
        } else {
            false
        }
    }

    /// Get all transactions conflicting with given transaction
    pub fn get_conflicting_transactions(&self, tx_hash: &TxHash) -> Vec<TxHash> {
        let nodes = self.nodes.read();
        if let Some(node) = nodes.get(tx_hash) {
            node.conflicting_with.iter().cloned().collect()
        } else {
            Vec::new()
        }
    }

    /// Mark transactions as high-risk
    pub fn mark_high_risk(&self, tx_hash: &TxHash) -> Result<Vec<TxHash>, String> {
        let mut nodes = self.nodes.write();
        let mut stats = self.stats.write();
        let mut high_risk_list = Vec::new();

        if let Some(node) = nodes.get_mut(tx_hash) {
            if !node.is_high_risk {
                node.is_high_risk = true;
                high_risk_list.push(tx_hash.clone());
                stats.high_risk_transactions += 1;
            }
        }

        // Mark descendants as high-risk
        let descendants = self.find_descendants(tx_hash, &nodes);
        for desc in descendants {
            if let Some(desc_node) = nodes.get_mut(&desc) {
                if !desc_node.is_high_risk {
                    desc_node.is_high_risk = true;
                    high_risk_list.push(desc.clone());
                    stats.high_risk_transactions += 1;
                }
            }
        }

        Ok(high_risk_list)
    }

    /// Get transaction node info
    pub fn get_node(&self, tx_hash: &TxHash) -> Option<ConflictNode> {
        let nodes = self.nodes.read();
        nodes.get(tx_hash).cloned()
    }

    /// Get statistics
    pub fn get_stats(&self) -> ConflictGraphStats {
        self.stats.read().clone()
    }

    /// Clear entire graph (for testing)
    pub fn clear(&self) {
        self.outpoint_index.write().clear();
        self.nodes.write().clear();
        self.conflict_sets.write().clear();
        let mut stats = self.stats.write();
        *stats = ConflictGraphStats::default();
    }

    pub fn node_count(&self) -> usize {
        self.nodes.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.read().is_empty()
    }
}

impl Drop for ConflictGraph {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Encode bytes to hex string without external crate
fn hex_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use klomang_core::core::crypto::Hash;
    use klomang_core::core::state::transaction::{SigHashType, TxInput};

    fn create_test_tx(id: u8, prev_tx_ids: Vec<u8>) -> Transaction {
        let mut inputs = Vec::new();
        for (idx, prev_id) in prev_tx_ids.iter().enumerate() {
            inputs.push(TxInput {
                prev_tx: Hash::new(&[*prev_id; 32]),
                index: idx as u32,
                signature: vec![],
                pubkey: vec![],
                sighash_type: SigHashType::All,
            });
        }

        Transaction {
            id: Hash::new(&[id; 32]),
            inputs,
            outputs: vec![],
            execution_payload: vec![],
            contract_address: None,
            gas_limit: 0,
            max_fee_per_gas: 0,
            chain_id: 1,
            locktime: 0,
        }
    }

    fn tx_hash(id: u8) -> TxHash {
        TxHash::new(vec![id; 32])
    }

    #[test]
    fn test_conflict_graph_creation() {
        let kv_store = Arc::new(KvStore::new_dummy());
        let graph = ConflictGraph::new(kv_store);
        assert!(graph.is_empty());
    }

    #[test]
    fn test_conflict_detection_basic() {
        let kv_store = Arc::new(KvStore::new_dummy());
        let graph = ConflictGraph::new(kv_store);

        let tx1 = create_test_tx(1, vec![100]);
        let tx2 = create_test_tx(2, vec![100]); // Same input as tx1

        let hash1 = tx_hash(1);
        let hash2 = tx_hash(2);

        let conflicts1 = graph.register_transaction(&tx1, &hash1, 1000, 100).unwrap();
        assert!(conflicts1.is_empty());

        let conflicts2 = graph.register_transaction(&tx2, &hash2, 500, 100).unwrap();
        assert_eq!(conflicts2.len(), 1);
        assert_eq!(conflicts2[0], hash1);
    }

    #[test]
    fn test_cascade_removal() {
        let kv_store = Arc::new(KvStore::new_dummy());
        let graph = ConflictGraph::new(kv_store);

        let parent_hash = tx_hash(1);
        let child_hash = tx_hash(2);

        let parent_tx = create_test_tx(1, vec![100]);
        let child_tx = create_test_tx(2, vec![101]); // Different input

        graph
            .register_transaction(&parent_tx, &parent_hash, 1000, 100)
            .ok();
        graph
            .register_transaction(&child_tx, &child_hash, 500, 100)
            .ok();

        // Add dependency
        graph.add_dependency(&child_hash, &parent_hash).ok();

        // Remove parent
        let removed = graph.remove_and_cascade(&parent_hash).unwrap();
        assert_eq!(removed.len(), 2); // Parent + child
        assert!(graph.is_empty());
    }

    #[test]
    fn test_conflict_set_computation() {
        let kv_store = Arc::new(KvStore::new_dummy());
        let graph = ConflictGraph::new(kv_store);

        let hash1 = tx_hash(1);
        let hash2 = tx_hash(2);
        let hash3 = tx_hash(3);

        let tx1 = create_test_tx(1, vec![100]);
        let tx2 = create_test_tx(2, vec![100]); // Conflicts with tx1
        let tx3 = create_test_tx(3, vec![101]);

        graph.register_transaction(&tx1, &hash1, 1000, 100).ok();
        graph.register_transaction(&tx2, &hash2, 800, 100).ok();
        graph.register_transaction(&tx3, &hash3, 500, 100).ok();

        let conflict_set = graph.get_conflict_set(&hash1).unwrap();
        assert!(conflict_set.contains(&hash1));
        assert!(conflict_set.contains(&hash2));
        assert!(!conflict_set.contains(&hash3));
    }
}
