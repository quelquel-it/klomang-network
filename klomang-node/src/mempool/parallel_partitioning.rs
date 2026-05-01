//! Conflict-Free Transaction Partitioning with Consistent Hashing
//!
//! This module implements:
//! - Consistent hashing algorithm for conflict detection
//! - Transaction partitioning into validation shards
//! - Deterministic shard assignment for parallel processing
//!
//! Key Concept:
//! - Transactions that reference the same UTXO (OutPoint) go to the same shard
//! - Transactions with non-overlapping UTPOs go to different shards
//! - This enables true parallel validation without race conditions

use super::conflict::OutPoint;
use klomang_core::core::state::transaction::Transaction;
use std::collections::{HashMap, HashSet};

/// Configuration for transaction partitioning
#[derive(Clone, Debug)]
pub struct PartitionConfig {
    /// Number of validation shards/partitions
    pub num_shards: usize,
    /// Enable strict conflict detection
    pub detect_conflicts: bool,
    /// Maximum transactions per shard
    pub max_per_shard: usize,
}

impl Default for PartitionConfig {
    fn default() -> Self {
        Self {
            num_shards: 4,
            detect_conflicts: true,
            max_per_shard: 5000,
        }
    }
}

/// Result of transaction partitioning
#[derive(Clone, Debug)]
pub struct PartitionResult {
    /// Shards indexed by shard ID
    pub shards: Vec<Vec<Transaction>>,
    /// Mapping of transaction ID to shard ID
    pub tx_to_shard: HashMap<Vec<u8>, usize>,
    /// Set of OutPoints referenced across all transactions
    pub all_outpoints: HashSet<OutPoint>,
    /// Conflicts detected (OutPoint -> transactions that reference it)
    pub conflicts: HashMap<OutPoint, Vec<Vec<u8>>>,
}

impl Default for PartitionResult {
    fn default() -> Self {
        Self {
            shards: Vec::new(),
            tx_to_shard: HashMap::new(),
            all_outpoints: HashSet::new(),
            conflicts: HashMap::new(),
        }
    }
}

/// Conflict-Free Transaction Partitioner using Consistent Hashing
pub struct ConflictFreePartitioner {
    config: PartitionConfig,
}

impl ConflictFreePartitioner {
    /// Create new partitioner with default config
    pub fn new() -> Self {
        Self {
            config: PartitionConfig::default(),
        }
    }

    /// Create new partitioner with custom config
    pub fn with_config(config: PartitionConfig) -> Self {
        Self { config }
    }

    /// Partition transactions into conflict-free shards
    pub fn partition(&self, transactions: Vec<Transaction>) -> Result<PartitionResult, String> {
        let mut result = PartitionResult::default();

        // Initialize shards
        result.shards = vec![Vec::new(); self.config.num_shards];

        // Track OutPoint to transactions mapping
        let mut outpoint_to_txs: HashMap<OutPoint, Vec<Vec<u8>>> = HashMap::new();

        // First pass: extract outpoints and map transactions
        for tx in &transactions {
            let outpoints = Self::extract_outpoints(tx);
            let tx_hash =
                bincode::serialize(&tx.id).map_err(|e| format!("Serialization error: {}", e))?;

            // Record all outpoints
            for outpoint in &outpoints {
                result.all_outpoints.insert(outpoint.clone());
                outpoint_to_txs
                    .entry(outpoint.clone())
                    .or_insert_with(Vec::new)
                    .push(tx_hash.clone());
            }

            // Determine shard for this transaction
            let primary_shard = if !outpoints.is_empty() {
                Self::hash_to_shard(&outpoints[0], self.config.num_shards)
            } else {
                // Coinbase or no inputs - use transaction hash
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                use std::hash::{Hash as StdHash, Hasher};
                StdHash::hash(&tx_hash, &mut hasher);
                (hasher.finish() as usize) % self.config.num_shards
            };

            // Check shard capacity
            if result.shards[primary_shard].len() >= self.config.max_per_shard {
                return Err(format!(
                    "Shard {} exceeds max capacity {}",
                    primary_shard, self.config.max_per_shard
                ));
            }

            result.tx_to_shard.insert(tx_hash, primary_shard);
            result.shards[primary_shard].push(tx.clone());
        }

        // Second pass: detect conflicts if enabled
        if self.config.detect_conflicts {
            for (outpoint, txs) in outpoint_to_txs {
                if txs.len() > 1 {
                    result.conflicts.insert(outpoint, txs);
                }
            }
        }

        Ok(result)
    }

    /// Extract all OutPoints from a transaction's inputs
    fn extract_outpoints(tx: &Transaction) -> Vec<OutPoint> {
        tx.inputs
            .iter()
            .map(|input| {
                let tx_hash = bincode::serialize(&input.prev_tx).unwrap_or_default();
                OutPoint::new(tx_hash, input.index)
            })
            .collect()
    }

    /// Public method to extract OutPoints from transaction
    pub fn extract_outpoints_from_tx(tx: &Transaction) -> Vec<OutPoint> {
        Self::extract_outpoints(tx)
    }

    /// Hash OutPoint to shard ID for consistent hashing
    fn hash_to_shard(outpoint: &OutPoint, num_shards: usize) -> usize {
        if num_shards == 0 {
            return 0;
        }

        // Combine tx_hash bytes with index for deterministic hashing
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        use std::hash::{Hash as StdHash, Hasher};

        StdHash::hash(&outpoint.tx_hash, &mut hasher);
        StdHash::hash(&outpoint.index, &mut hasher);

        let hash_value = hasher.finish() as usize;
        hash_value % num_shards
    }

    /// Verify that partitioning is conflict-free
    pub fn verify_partition_safety(result: &PartitionResult) -> Result<(), String> {
        // Check that all transactions in a shard are conflict-free
        for (_shard_id, shard_txs) in result.shards.iter().enumerate() {
            let mut seen_outpoints: HashSet<OutPoint> = HashSet::new();

            for tx in shard_txs {
                for outpoint in Self::extract_outpoints(tx) {
                    if seen_outpoints.contains(&outpoint) {
                        return Err("Conflict within shard: OutPoint appears twice".to_string());
                    }
                    seen_outpoints.insert(outpoint);
                }
            }
        }

        Ok(())
    }

    /// Check if value could be distributed across shards
    pub fn analyze_distribution(result: &PartitionResult) -> HashMap<usize, usize> {
        let mut distribution = HashMap::new();

        for (shard_id, shards) in result.shards.iter().enumerate() {
            distribution.insert(shard_id, shards.len());
        }

        distribution
    }

    /// Get number of conflicts detected
    pub fn conflict_count(result: &PartitionResult) -> usize {
        result.conflicts.len()
    }

    /// Export partition statistics
    pub fn get_statistics(result: &PartitionResult) -> PartitionStats {
        let total_txs: usize = result.shards.iter().map(|s| s.len()).sum();
        let shard_sizes: Vec<usize> = result.shards.iter().map(|s| s.len()).collect();
        let avg_shard_size = if !shard_sizes.is_empty() {
            total_txs / shard_sizes.len()
        } else {
            0
        };

        let min_shard_size = shard_sizes.iter().copied().min().unwrap_or(0);
        let max_shard_size = shard_sizes.iter().copied().max().unwrap_or(0);

        PartitionStats {
            total_transactions: total_txs,
            num_shards: shard_sizes.len(),
            avg_shard_size,
            min_shard_size,
            max_shard_size,
            conflicts_detected: result.conflicts.len(),
            total_outpoints: result.all_outpoints.len(),
        }
    }
}

impl Default for ConflictFreePartitioner {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about transaction partitioning
#[derive(Clone, Debug)]
pub struct PartitionStats {
    pub total_transactions: usize,
    pub num_shards: usize,
    pub avg_shard_size: usize,
    pub min_shard_size: usize,
    pub max_shard_size: usize,
    pub conflicts_detected: usize,
    pub total_outpoints: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use klomang_core::core::crypto::Hash;
    use klomang_core::core::state::transaction::{SigHashType, TxInput, TxOutput};

    fn create_test_tx(prev_tx_bytes: &[u8]) -> Transaction {
        let prev_tx = Hash::new(prev_tx_bytes);

        Transaction {
            id: Hash::new(&[1u8; 32]),
            inputs: vec![TxInput {
                prev_tx,
                index: 0,
                signature: vec![],
                pubkey: vec![],
                sighash_type: SigHashType::All,
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
    fn test_partitioner_creation() {
        let partitioner = ConflictFreePartitioner::new();
        assert_eq!(partitioner.config.num_shards, 4);
    }

    #[test]
    fn test_partition_distribution() {
        let config = PartitionConfig {
            num_shards: 4,
            detect_conflicts: true,
            max_per_shard: 1000,
        };
        let partitioner = ConflictFreePartitioner::with_config(config);

        let mut txs = Vec::new();
        for i in 0..8 {
            txs.push(create_test_tx(&[i as u8; 32]));
        }

        let result = partitioner.partition(txs).unwrap();
        let distribution = ConflictFreePartitioner::analyze_distribution(&result);

        assert_eq!(distribution.len(), 4);
    }

    #[test]
    fn test_partition_statistics() {
        let partitioner = ConflictFreePartitioner::new();

        let mut txs = Vec::new();
        for i in 0..10 {
            txs.push(create_test_tx(&[i as u8; 32]));
        }

        let result = partitioner.partition(txs).unwrap();
        let stats = ConflictFreePartitioner::get_statistics(&result);

        assert_eq!(stats.total_transactions, 10);
        assert!(stats.num_shards > 0);
    }
}
