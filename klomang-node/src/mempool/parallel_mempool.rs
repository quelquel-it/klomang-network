//! UTXO-Based Parallel Mempool with Per-Shard Locking
//!
//! This module implements:
//! - SubPool structure for individual validation shards
//! - ParallelMempool coordinating multiple SubPools
//! - Per-shard RwLock for true parallelism
//! - Async UTXO validation on each shard
//!
//! Key Feature:
//! - Write operations on Shard A don't block operations on Shard B
//! - Cross-shard reads can happen concurrently
//! - Independent UTXO validation per shard

use std::sync::Arc;
use parking_lot::RwLock;
use indexmap::IndexMap;
use klomang_core::core::state::transaction::Transaction;
use crate::storage::KvStore;
use super::conflict::OutPoint;
use super::parallel_partitioning::ConflictFreePartitioner;

/// Entry in a sub-pool with status tracking
#[derive(Clone, Debug)]
pub struct SubPoolEntry {
    pub transaction: Transaction,
    pub status: TransactionStatus,
    pub arrival_time: u64,
    pub size_bytes: usize,
    pub outpoints: Vec<OutPoint>,
}

/// Status of transaction in a shard
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TransactionStatus {
    /// Transaction awaiting validation
    Pending,
    /// Transaction passed validation
    Validated,
    /// Transaction failed validation
    Invalid,
    /// Transaction conflict detected
    Conflicted,
}

/// A single validation shard with independent locking
pub struct SubPool {
    /// Shard ID
    pub shard_id: usize,
    /// Transactions in this shard (hash -> entry)
    entries: Arc<RwLock<IndexMap<Vec<u8>, SubPoolEntry>>>,
    /// Outpoints used by transactions in this shard
    used_outpoints: Arc<RwLock<std::collections::HashSet<OutPoint>>>,
}

impl SubPool {
    /// Create new sub-pool for shard
    pub fn new(shard_id: usize) -> Self {
        Self {
            shard_id,
            entries: Arc::new(RwLock::new(IndexMap::new())),
            used_outpoints: Arc::new(RwLock::new(std::collections::HashSet::new())),
        }
    }

    /// Add transaction to shard
    pub fn add_transaction(&self, entry: SubPoolEntry) -> Result<(), String> {
        let tx_hash = bincode::serialize(&entry.transaction.id)
            .map_err(|e| format!("Serialization error: {}", e))?;

        let mut entries = self.entries.write();
        if entries.contains_key(&tx_hash) {
            return Err("Transaction already in shard".to_string());
        }

        // Update outpoints tracking
        let mut outpoints = self.used_outpoints.write();
        for outpoint in &entry.outpoints {
            if outpoints.contains(outpoint) {
                return Err("Outpoint conflict in shard".to_string());
            }
            outpoints.insert(outpoint.clone());
        }

        entries.insert(tx_hash, entry);
        Ok(())
    }

    /// Remove transaction from shard
    pub fn remove_transaction(&self, tx_hash: &[u8]) -> Option<SubPoolEntry> {
        let mut entries = self.entries.write();
        if let Some((_hash, entry)) = entries.shift_remove_entry(tx_hash) {
            // Update outpoints
            let mut outpoints = self.used_outpoints.write();
            for outpoint in &entry.outpoints {
                outpoints.remove(outpoint);
            }
            return Some(entry);
        }
        None
    }

    /// Update transaction status
    pub fn update_status(&self, tx_hash: &[u8], status: TransactionStatus) -> Result<(), String> {
        let mut entries = self.entries.write();
        let entry = entries
            .get_mut(tx_hash)
            .ok_or_else(|| "Transaction not found in shard".to_string())?;

        entry.status = status;
        Ok(())
    }

    /// Get transaction by hash
    pub fn get_transaction(&self, tx_hash: &[u8]) -> Option<SubPoolEntry> {
        self.entries.read().get(tx_hash).cloned()
    }

    /// Get all transactions in shard
    pub fn get_all_transactions(&self) -> Vec<SubPoolEntry> {
        self.entries.read().values().cloned().collect()
    }

    /// Get transactions with specific status
    pub fn get_by_status(&self, status: TransactionStatus) -> Vec<SubPoolEntry> {
        self.entries
            .read()
            .values()
            .filter(|e| e.status == status)
            .cloned()
            .collect()
    }

    /// Check if transaction exists
    pub fn contains(&self, tx_hash: &[u8]) -> bool {
        self.entries.read().contains_key(tx_hash)
    }

    /// Get shard size
    pub fn size(&self) -> usize {
        self.entries.read().len()
    }

    /// Get used outpoints
    pub fn get_outpoints(&self) -> Vec<OutPoint> {
        self.used_outpoints.read().iter().cloned().collect()
    }

    /// Clear shard
    pub fn clear(&self) {
        self.entries.write().clear();
        self.used_outpoints.write().clear();
    }

    /// Verify shard consistency
    /// Verify shard consistency
    pub fn verify_consistency(&self) -> Result<(), String> {
        let entries = self.entries.read();
        let outpoints = self.used_outpoints.read();

        // Check each transaction's outpoints are tracked
        let mut seen = std::collections::HashSet::new();
        for (_, entry) in entries.iter() {
            for outpoint in &entry.outpoints {
                if !outpoints.contains(outpoint) {
                    return Err("Tracked outpoint missing from set".to_string());
                }
                if !seen.insert(outpoint.clone()) {
                    return Err("Duplicate outpoint in shard".to_string());
                }
            }
        }

        Ok(())
    }

}

impl Clone for SubPool {
    fn clone(&self) -> Self {
        Self {
            shard_id: self.shard_id,
            entries: Arc::clone(&self.entries),
            used_outpoints: Arc::clone(&self.used_outpoints),
        }
    }
}

/// Parallel Mempool with multiple independent sub-pools
///
/// This structure manages multiple validation shards, each with:
/// - Independent RwLock for concurrent access
/// - Separate transaction storage
/// - Per-shard UTXO tracking
///
/// Write operations on Shard A do not block reads/writes on Shard B
pub struct ParallelMempool {
    /// Sub-pools for each shard
    sub_pools: Vec<SubPool>,
    /// Partitioner for assigning transactions to shards
    partitioner: Arc<ConflictFreePartitioner>,
    /// Optional KvStore for UTXO validation
    kv_store: Option<Arc<KvStore>>,
    /// Number of shards
    num_shards: usize,
}

impl ParallelMempool {
    /// Create new parallel mempool
    pub fn new(num_shards: usize) -> Self {
        let config = super::parallel_partitioning::PartitionConfig {
            num_shards,
            detect_conflicts: true,
            max_per_shard: 5000,
        };

        let sub_pools = (0..num_shards).map(SubPool::new).collect();

        Self {
            sub_pools,
            partitioner: Arc::new(super::parallel_partitioning::ConflictFreePartitioner::with_config(config)),
            kv_store: None,
            num_shards,
        }
    }

    /// Create with KvStore for UTXO validation
    pub fn with_storage(num_shards: usize, kv_store: Arc<KvStore>) -> Self {
        let mut mempool = Self::new(num_shards);
        mempool.kv_store = Some(kv_store);
        mempool
    }

    /// Add batch of transactions with automatic partitioning
    pub fn add_transactions_batch(&self, transactions: Vec<Transaction>) -> Result<ParallelAddResult, String> {
        let result = self.partitioner.partition(transactions)?;

        // Verify partition safety
        ConflictFreePartitioner::verify_partition_safety(&result)?;

        let mut add_result = ParallelAddResult::default();

        // Add transactions to shards
        for (shard_id, txs) in result.shards.iter().enumerate() {
            for tx in txs {
                let tx_hash = bincode::serialize(&tx.id)
                    .map_err(|e| format!("Serialization error: {}", e))?;

                let outpoints = ConflictFreePartitioner::extract_outpoints_from_tx(tx);

                let entry = SubPoolEntry {
                    transaction: tx.clone(),
                    status: TransactionStatus::Pending,
                    arrival_time: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    size_bytes: bincode::serialized_size(tx).unwrap_or(0) as usize,
                    outpoints,
                };

                match self.sub_pools[shard_id].add_transaction(entry) {
                    Ok(_) => add_result.successful += 1,
                    Err(e) => {
                        add_result.failed += 1;
                        add_result.errors.push(format!(
                            "Failed to add tx ({}bytes) to shard {}: {}",
                            tx_hash.len(),
                            shard_id,
                            e
                        ));
                    }
                }
            }
        }

        add_result.conflicts = result.conflicts.len();

        Ok(add_result)
    }

    /// Get transaction from any shard
    pub fn get_transaction(&self, tx_hash: &[u8]) -> Option<SubPoolEntry> {
        for shard in &self.sub_pools {
            if let Some(tx) = shard.get_transaction(tx_hash) {
                return Some(tx);
            }
        }
        None
    }

    /// Remove transaction from mempool
    pub fn remove_transaction(&self, tx_hash: &[u8]) -> Option<SubPoolEntry> {
        for shard in &self.sub_pools {
            if let Some(tx) = shard.remove_transaction(tx_hash) {
                return Some(tx);
            }
        }
        None
    }

    /// Get shard for transaction (without modifying)
    pub fn get_shard_for_tx(&self, tx_hash: &[u8]) -> Option<Arc<RwLock<IndexMap<Vec<u8>, SubPoolEntry>>>> {
        for shard in &self.sub_pools {
            if shard.contains(tx_hash) {
                return Some(Arc::clone(&shard.entries));
            }
        }
        None
    }

    /// Get all transactions from all shards
    pub fn get_all_transactions(&self) -> Vec<SubPoolEntry> {
        let mut all_txs = Vec::new();
        for shard in &self.sub_pools {
            all_txs.extend(shard.get_all_transactions());
        }
        all_txs
    }

    /// Get transactions by status from all shards
    pub fn get_by_status(&self, status: TransactionStatus) -> Vec<SubPoolEntry> {
        let mut result = Vec::new();
        for shard in &self.sub_pools {
            result.extend(shard.get_by_status(status));
        }
        result
    }

    /// Get total size across all shards
    pub fn total_size(&self) -> usize {
        self.sub_pools.iter().map(|s| s.size()).sum()
    }

    /// Get shard statistics
    pub fn get_shard_stats(&self) -> Vec<ShardStats> {
        self.sub_pools
            .iter()
            .map(|shard| ShardStats {
                shard_id: shard.shard_id,
                transaction_count: shard.size(),
                outpoint_count: shard.get_outpoints().len(),
                pending: shard.get_by_status(TransactionStatus::Pending).len(),
                validated: shard.get_by_status(TransactionStatus::Validated).len(),
                invalid: shard.get_by_status(TransactionStatus::Invalid).len(),
                conflicted: shard.get_by_status(TransactionStatus::Conflicted).len(),
            })
            .collect()
    }

    /// Verify all shards consistency
    pub fn verify_all_shards(&self) -> Result<(), String> {
        for shard in &self.sub_pools {
            shard.verify_consistency()?;
        }
        Ok(())
    }

    /// Clear all shards
    pub fn clear_all(&self) {
        for shard in &self.sub_pools {
            shard.clear();
        }
    }

    /// Get reference to specific shard
    pub fn get_shard(&self, shard_id: usize) -> Option<&SubPool> {
        self.sub_pools.get(shard_id)
    }

    /// Get all shards
    pub fn get_all_shards(&self) -> &[SubPool] {
        &self.sub_pools
    }

    /// Update transaction status in any shard
    pub fn update_status(&self, tx_hash: &[u8], status: TransactionStatus) -> Result<(), String> {
        for shard in &self.sub_pools {
            if shard.contains(tx_hash) {
                return shard.update_status(tx_hash, status);
            }
        }
        Err("Transaction not found in any shard".to_string())
    }
}

impl Clone for ParallelMempool {
    fn clone(&self) -> Self {
        Self {
            sub_pools: self.sub_pools.clone(),
            partitioner: Arc::clone(&self.partitioner),
            kv_store: self.kv_store.clone(),
            num_shards: self.num_shards,
        }
    }
}

/// Result of batch add operation
#[derive(Clone, Debug, Default)]
pub struct ParallelAddResult {
    pub successful: usize,
    pub failed: usize,
    pub conflicts: usize,
    pub errors: Vec<String>,
}

/// Statistics for a single shard
#[derive(Clone, Debug)]
pub struct ShardStats {
    pub shard_id: usize,
    pub transaction_count: usize,
    pub outpoint_count: usize,
    pub pending: usize,
    pub validated: usize,
    pub invalid: usize,
    pub conflicted: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use klomang_core::core::crypto::Hash;
    use klomang_core::core::state::transaction::{TxInput, TxOutput};

    fn create_test_tx() -> Transaction {
        Transaction {
            id: Hash::new(&[1u8; 32]),
            inputs: vec![TxInput {
                prev_tx: Hash::new(&[1u8; 32]),
                index: 0,
                signature: vec![],
                pubkey: vec![],
                sighash_type: klomang_core::core::state::transaction::SigHashType::All,
            }],
            outputs: vec![TxOutput {
                value: 1000,
                pubkey_hash: Hash::new(&[2u8; 32]),
            }],
            execution_payload: vec![],
            contract_address: None,
            gas_limit: 0,
            max_fee_per_gas: 0,
            chain_id: 1,
            locktime: 0,
        }
    }

    #[test]
    fn test_subpool_creation() {
        let shard = SubPool::new(0);
        assert_eq!(shard.shard_id, 0);
        assert_eq!(shard.size(), 0);
    }

    #[test]
    fn test_parallel_mempool_creation() {
        let mempool = ParallelMempool::new(4);
        assert_eq!(mempool.num_shards, 4);
        assert_eq!(mempool.total_size(), 0);
    }

    #[test]
    fn test_mempool_add_transaction() {
        let mempool = ParallelMempool::new(4);
        let tx = create_test_tx();

        let result = mempool.add_transactions_batch(vec![tx]).unwrap();
        assert!(result.successful > 0);
    }

    #[test]
    fn test_mempool_get_transaction() {
        let mempool = ParallelMempool::new(4);
        let mut tx = create_test_tx();
        tx.id = Hash::new(&[99u8; 32]);

        mempool.add_transactions_batch(vec![tx.clone()]).unwrap();

        let tx_hash = bincode::serialize(&tx.id).unwrap();
        let retrieved = mempool.get_transaction(&tx_hash);
        assert!(retrieved.is_some());
    }

    #[test]
    fn test_mempool_shard_stats() {
        let mempool = ParallelMempool::new(4);
        let mut txs = Vec::new();

        for i in 0..4 {
            let mut tx = create_test_tx();
            tx.id = Hash::new(&[i as u8; 32]);
            txs.push(tx);
        }

        mempool.add_transactions_batch(txs).unwrap();

        let stats = mempool.get_shard_stats();
        assert!(stats.len() > 0);
        let total: usize = stats.iter().map(|s| s.transaction_count).sum();
        assert_eq!(total, 4);
    }

    #[test]
    fn test_mempool_verify_consistency() {
        let mempool = ParallelMempool::new(4);
        let tx = create_test_tx();

        mempool.add_transactions_batch(vec![tx]).unwrap();
        assert!(mempool.verify_all_shards().is_ok());
    }
}
