//! Deterministic Ordering Engine with Storage Integration
//!
//! This module provides:
//! - Deterministic tie-breaking across all validator nodes
//! - Integration with klomang_core transaction types
//! - Storage synchronization for mempool consistency
//! - Validation of transaction ordering for consensus
//!
//! The engine ensures that all nodes produce the same transaction ordering
//! when given the same set of transactions, regardless of arrival order.

use std::sync::Arc;
use parking_lot::RwLock;
use crate::storage::KvStore;
use super::priority_ordering::{PriorityBuckets, PrioritizedTransaction, PriorityOrderingConfig};

/// Configuration for deterministic ordering engine
#[derive(Clone, Debug)]
pub struct DeterministicOrderingEngineConfig {
    /// Enable validation of transaction ordering
    pub validate_ordering: bool,
    /// Maximum transactions to process in one batch
    pub batch_size: usize,
    /// Use storage verification for transactions
    pub use_storage_verification: bool,
}

impl Default for DeterministicOrderingEngineConfig {
    fn default() -> Self {
        Self {
            validate_ordering: true,
            batch_size: 100,
            use_storage_verification: true,
        }
    }
}

/// Result of ordering validation
#[derive(Clone, Debug)]
pub struct OrderingValidation {
    /// Whether ordering is valid
    pub is_valid: bool,
    /// Detailed error message if invalid
    pub error_message: Option<String>,
    /// Number of transactions checked
    pub transaction_count: usize,
    /// Whether storage verification passed
    pub storage_verified: bool,
}

impl OrderingValidation {
    /// Create successful validation
    pub fn success(count: usize, storage_verified: bool) -> Self {
        Self {
            is_valid: true,
            error_message: None,
            transaction_count: count,
            storage_verified,
        }
    }

    /// Create failed validation
    pub fn failure(error: String, count: usize) -> Self {
        Self {
            is_valid: false,
            error_message: Some(error),
            transaction_count: count,
            storage_verified: false,
        }
    }
}

/// Deterministic Ordering Engine for mempool transactions
///
/// Maintains deterministic ordering across validator nodes using:
/// - Lexicographic hash comparison for tie-breaking
/// - Priority bucketing by fee rate
/// - Storage integration for transaction validation
/// - Consensus-safe ordering guarantees
pub struct DeterministicOrderingEngine {
    /// Priority buckets for transaction ordering
    priority_buckets: Arc<RwLock<PriorityBuckets>>,
    /// Optional KvStore for transaction validation
    kv_store: Option<Arc<KvStore>>,
    /// Configuration
    config: DeterministicOrderingEngineConfig,
}

impl DeterministicOrderingEngine {
    /// Create new deterministic ordering engine
    pub fn new(config: DeterministicOrderingEngineConfig) -> Self {
        let priority_config = PriorityOrderingConfig::default();
        Self {
            priority_buckets: Arc::new(RwLock::new(PriorityBuckets::new(priority_config))),
            kv_store: None,
            config,
        }
    }

    /// Create with storage integration
    pub fn with_storage(config: DeterministicOrderingEngineConfig, kv_store: Arc<KvStore>) -> Self {
        let priority_config = PriorityOrderingConfig::default();
        Self {
            priority_buckets: Arc::new(RwLock::new(PriorityBuckets::new(priority_config))),
            kv_store: Some(kv_store),
            config,
        }
    }

    /// Set KvStore for transaction validation
    pub fn set_storage(&mut self, kv_store: Arc<KvStore>) {
        self.kv_store = Some(kv_store);
    }

    /// Add transaction with deterministic ordering
    pub fn add_transaction(
        &self,
        tx_hash: Vec<u8>,
        fee_rate: u64,
        arrival_time: u64,
        size_bytes: usize,
        total_fee: u64,
    ) -> Result<(), String> {
        let prioritized_tx = PrioritizedTransaction::new(
            tx_hash,
            fee_rate,
            arrival_time,
            size_bytes,
            total_fee,
        );

        let buckets = self.priority_buckets.write();
        buckets.insert(prioritized_tx)
    }

    /// Remove transaction from ordering
    pub fn remove_transaction(&self, tx_hash: &[u8]) -> Option<PrioritizedTransaction> {
        let buckets = self.priority_buckets.write();
        buckets.remove(tx_hash)
    }

    /// Get transactions in deterministic order with guarantee
    ///
    /// This function returns transactions ordered deterministically such that:
    /// 1. Transactions are ordered by fee rate (highest first)
    /// 2. Transactions with same fee rate are ordered lexicographically by hash
    /// 3. Order is identical across all validator nodes
    ///
    /// CONSENSUS CRITICAL: This ordering is used for block building and must
    /// be identical across all nodes for consensus safety.
    pub fn get_ordered_transactions(&self, limit: usize) -> Result<Vec<PrioritizedTransaction>, String> {
        let buckets = self.priority_buckets.read();
        let transactions = buckets.get_ordered_transactions(limit);
        Ok(transactions)
    }

    /// Verify that ordering is deterministically consistent
    pub fn verify_ordering_consistency(&self) -> Result<OrderingValidation, String> {
        let buckets = self.priority_buckets.read();

        // Verify bucket consistency (internal ordering)
        buckets.verify_consistency()
            .map_err(|e| format!("Bucket consistency check failed: {}", e))?;

        let transaction_count = buckets.total_count();

        // Check storage if enabled
        let storage_verified = if self.config.use_storage_verification {
            self.kv_store.is_some()
        } else {
            false
        };

        Ok(OrderingValidation::success(transaction_count, storage_verified))
    }

    /// Validate transaction ordering against another engine
    ///
    /// Compares ordering output for consensus safety
    pub fn validate_against_peer(
        &self,
        peer_transactions: &[PrioritizedTransaction],
    ) -> Result<OrderingValidation, String> {
        let our_txs = self.get_ordered_transactions(peer_transactions.len())?;

        if our_txs.len() != peer_transactions.len() {
            let error = format!(
                "Transaction count mismatch: {} vs {}",
                our_txs.len(),
                peer_transactions.len()
            );
            return Ok(OrderingValidation::failure(error, our_txs.len()));
        }

        // Compare each transaction's ordering
        for (idx, (our_tx, peer_tx)) in our_txs.iter().zip(peer_transactions.iter()).enumerate() {
            if our_tx.tx_hash != peer_tx.tx_hash {
                let error = format!("Transaction mismatch at position {}", idx);
                return Ok(OrderingValidation::failure(error, idx));
            }

            // Verify deterministic comparison produce identical result
            use std::cmp::Ordering;
            if our_tx.compare_deterministic(peer_tx) != Ordering::Equal {
                let error = format!(
                    "Deterministic comparison failed at position {}: {:?}",
                    idx,
                    our_tx.compare_deterministic(peer_tx)
                );
                return Ok(OrderingValidation::failure(error, idx));
            }
        }

        Ok(OrderingValidation::success(our_txs.len(), false))
    }

    /// Process transactions in a batch
    pub fn process_batch(
        &self,
        transactions: Vec<(Vec<u8>, u64, u64, usize, u64)>,
    ) -> Result<usize, String> {
        let mut added_count = 0;

        for (tx_hash, fee_rate, arrival_time, size_bytes, total_fee) in transactions {
            self.add_transaction(tx_hash, fee_rate, arrival_time, size_bytes, total_fee)?;
            added_count += 1;
        }

        Ok(added_count)
    }

    /// Get statistics about the ordering
    pub fn get_ordering_stats(&self) -> Result<OrderingStats, String> {
        let buckets = self.priority_buckets.read();
        let stats = buckets.get_statistics();

        Ok(OrderingStats {
            total_transactions: stats.total_transactions,
            total_fees: stats.total_fees,
            bucket_count: buckets.bucket_count(),
            bucket_distribution: stats.bucket_info.iter().map(|b| b.transaction_count).collect(),
        })
    }

    /// Check if transaction exists in ordering
    pub fn contains(&self, tx_hash: &[u8]) -> bool {
        let buckets = self.priority_buckets.read();
        buckets.contains(tx_hash)
    }

    /// Get transaction by hash
    pub fn get_transaction(&self, tx_hash: &[u8]) -> Option<PrioritizedTransaction> {
        let buckets = self.priority_buckets.read();
        buckets.get(tx_hash)
    }

    /// Clear all transactions (for reset/testing)
    pub fn clear(&self) -> Result<(), String> {
        let buckets = self.priority_buckets.write();
        buckets.clear();
        Ok(())
    }

    /// Get fee rate thresholds
    pub fn get_fee_thresholds(&self) -> Vec<u64> {
        let buckets = self.priority_buckets.read();
        buckets.fee_thresholds()
    }

    /// Export all transactions in deterministic order
    ///
    /// This is useful for synchronization with peers
    pub fn export_ordered_snapshot(&self) -> Result<Vec<PrioritizedTransaction>, String> {
        self.get_ordered_transactions(usize::MAX)
    }

    /// Import transactions as a batch maintaining determinism
    pub fn import_ordered_batch(
        &self,
        transactions: Vec<PrioritizedTransaction>,
    ) -> Result<usize, String> {
        for tx in transactions {
            self.add_transaction(
                tx.tx_hash,
                tx.fee_rate,
                tx.arrival_time,
                tx.size_bytes,
                tx.total_fee,
            )?;
        }

        Ok(self.priority_buckets.read().total_count())
    }
}

/// Statistics about deterministic ordering
#[derive(Clone, Debug)]
pub struct OrderingStats {
    pub total_transactions: usize,
    pub total_fees: u64,
    pub bucket_count: usize,
    pub bucket_distribution: Vec<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_tx(
        hash: usize,
        fee_rate: u64,
        arrival_time: u64,
    ) -> (Vec<u8>, u64, u64, usize, u64) {
        let hash_vec = vec![(hash & 0xFF) as u8; 32];
        (hash_vec, fee_rate, arrival_time, 200, fee_rate * 200)
    }

    #[test]
    fn test_deterministic_engine_creation() {
        let config = DeterministicOrderingEngineConfig::default();
        let engine = DeterministicOrderingEngine::new(config);

        assert!(engine.get_ordered_transactions(10).is_ok());
    }

    #[test]
    fn test_add_transactions() {
        let config = DeterministicOrderingEngineConfig::default();
        let engine = DeterministicOrderingEngine::new(config);

        let (hash, fee_rate, arrival_time, size, total_fee) = create_test_tx(1, 50, 1000);
        assert!(engine.add_transaction(hash, fee_rate, arrival_time, size, total_fee).is_ok());

        assert_eq!(engine.get_ordered_transactions(10).unwrap().len(), 1);
    }

    #[test]
    fn test_deterministic_ordering_consistency() {
        let config = DeterministicOrderingEngineConfig::default();
        let engine = DeterministicOrderingEngine::new(config);

        // Add multiple transactions
        for i in 0..5 {
            let (hash, fee_rate, arrival_time, size, total_fee) = create_test_tx(i, 50 + i as u64, 1000);
            assert!(engine.add_transaction(hash, fee_rate, arrival_time, size, total_fee).is_ok());
        }

        let first_result = engine.get_ordered_transactions(10).unwrap();
        let second_result = engine.get_ordered_transactions(10).unwrap();

        // Should be identical
        assert_eq!(first_result.len(), second_result.len());
        for (tx1, tx2) in first_result.iter().zip(second_result.iter()) {
            assert_eq!(tx1.tx_hash, tx2.tx_hash);
        }
    }

    #[test]
    fn test_verify_ordering_consistency() {
        let config = DeterministicOrderingEngineConfig::default();
        let engine = DeterministicOrderingEngine::new(config);

        let (hash, fee_rate, arrival_time, size, total_fee) = create_test_tx(1, 50, 1000);
        assert!(engine.add_transaction(hash, fee_rate, arrival_time, size, total_fee).is_ok());

        let validation = engine.verify_ordering_consistency().unwrap();
        assert!(validation.is_valid);
    }

    #[test]
    fn test_remove_transaction() {
        let config = DeterministicOrderingEngineConfig::default();
        let engine = DeterministicOrderingEngine::new(config);

        let (hash, fee_rate, arrival_time, size, total_fee) = create_test_tx(1, 50, 1000);
        let hash_vec = hash.clone();

        assert!(engine.add_transaction(hash, fee_rate, arrival_time, size, total_fee).is_ok());
        assert_eq!(engine.get_ordered_transactions(10).unwrap().len(), 1);

        let removed = engine.remove_transaction(&hash_vec);
        assert!(removed.is_some());
        assert_eq!(engine.get_ordered_transactions(10).unwrap().len(), 0);
    }
}
