//! Replace-By-Fee (RBF) Manager with Deterministic Supremacy
//!
//! Implements RBF logic with three-tier deterministic rules to ensure
//! consensus across all nodes while maintaining economic incentives.

use std::sync::Arc;

use klomang_core::core::state::transaction::Transaction;

use super::conflict_graph::{ConflictGraph, TxHash};

/// Minimum relay fee in satoshis per byte
#[allow(dead_code)]
const MINIMUM_RELAY_FEE_RATE: f64 = 1.0;

/// Minimum fee increment as absolute value
const MINIMUM_FEE_INCREMENT: u64 = 1;

/// Result of RBF evaluation
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RBFChoice {
    /// Keep existing transaction, reject incoming
    KeepExisting,

    /// Replace existing with incoming
    ReplaceExisting {
        reason: RBFReason,
        evicted_descendants: Vec<TxHash>,
    },

    /// Cannot replace (conflict from different TX)
    CannotReplace { reason: String },
}

/// Reason why replacement was chosen
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RBFReason {
    /// Incoming has higher absolute fee
    HigherAbsoluteFee,

    /// Incoming has higher fee rate
    HigherFeeRate,

    /// Incoming has higher fee rate AND meets minimum threshold
    HigherFeeRateWithThreshold,

    /// Hash tiebreaker (deterministic)
    DeterministicTiebreaker,
}

/// Statistics for RBF operations
#[derive(Clone, Debug, Default)]
pub struct RBFStats {
    pub total_evaluations: u64,
    pub replacements_performed: u64,
    pub rejections: u64,
    pub fee_rate_wins: u64,
    pub tiebreaker_wins: u64,
}

/// RBF evaluation result with details
#[derive(Clone, Debug)]
pub struct RBFEvaluation {
    pub choice: RBFChoice,
    pub incoming_fee: u64,
    pub incoming_size: usize,
    pub existing_fee: u64,
    pub existing_size: usize,
    pub incoming_rate: f64,
    pub existing_rate: f64,
    pub incoming_tx_hash: TxHash,
    pub existing_tx_hash: TxHash,
}

impl RBFEvaluation {
    pub fn winner_hash(&self) -> Option<&TxHash> {
        match &self.choice {
            RBFChoice::ReplaceExisting { .. } => Some(&self.incoming_tx_hash),
            RBFChoice::KeepExisting => Some(&self.existing_tx_hash),
            RBFChoice::CannotReplace { .. } => None,
        }
    }
}

/// Replace-By-Fee Manager
pub struct RBFManager {
    conflict_graph: Arc<ConflictGraph>,
    stats: Arc<parking_lot::Mutex<RBFStats>>,
}

impl RBFManager {
    /// Create new RBF manager
    pub fn new(conflict_graph: Arc<ConflictGraph>) -> Self {
        Self {
            conflict_graph,
            stats: Arc::new(parking_lot::Mutex::new(RBFStats::default())),
        }
    }

    /// Evaluate RBF supremacy between incoming and existing transaction
    pub fn evaluate_rbf_supremacy(
        &self,
        _incoming_tx: &Transaction,
        incoming_hash: &TxHash,
        incoming_fee: u64,
        incoming_size: usize,

        _existing_tx: &Transaction,
        existing_hash: &TxHash,
        existing_fee: u64,
        existing_size: usize,
    ) -> Result<RBFChoice, String> {
        let mut stats = self.stats.lock();
        stats.total_evaluations += 1;

        // Calculate fee rates
        let incoming_rate = if incoming_size > 0 {
            incoming_fee as f64 / incoming_size as f64
        } else {
            return Err("Incoming transaction has zero size".to_string());
        };

        let existing_rate = if existing_size > 0 {
            existing_fee as f64 / existing_size as f64
        } else {
            return Err("Existing transaction has zero size".to_string());
        };

        // Rule 1: Check absolute fee threshold
        // Incoming must be greater than existing + minimum relay fee for all inputs
        let minimum_additional_fee = (existing_size as u64) * MINIMUM_FEE_INCREMENT;
        let required_fee = existing_fee + minimum_additional_fee;

        if incoming_fee <= required_fee {
            stats.rejections += 1;
            return Ok(RBFChoice::KeepExisting);
        }

        // Rule 2: Check incremental fee rate
        // Incoming fee rate must be strictly higher than existing rate
        let fee_rate_diff = (incoming_rate - existing_rate).abs();

        if fee_rate_diff < 0.01 {
            // Fee rates are effectively equal - use tiebreaker
            return self.apply_deterministic_tiebreaker(
                incoming_hash,
                existing_hash,
                &mut stats,
            );
        }

        if incoming_rate > existing_rate {
            // Incoming has higher fee rate AND meets absolute threshold
            stats.fee_rate_wins += 1;
            stats.replacements_performed += 1;

            let evicted = self
                .conflict_graph
                .remove_and_cascade(existing_hash)
                .unwrap_or_default();

            return Ok(RBFChoice::ReplaceExisting {
                reason: RBFReason::HigherFeeRateWithThreshold,
                evicted_descendants: evicted,
            });
        }

        // Incoming rate is not higher
        stats.rejections += 1;
        Ok(RBFChoice::KeepExisting)
    }

    /// Apply deterministic tiebreaker when fees are identical
    fn apply_deterministic_tiebreaker(
        &self,
        incoming_hash: &TxHash,
        existing_hash: &TxHash,
        stats: &mut RBFStats,
    ) -> Result<RBFChoice, String> {
        // Rule 3: Lexicographical hash comparison
        // Smaller hash wins (deterministic, prevents consensus forks)
        if incoming_hash.as_bytes() < existing_hash.as_bytes() {
            stats.tiebreaker_wins += 1;
            stats.replacements_performed += 1;

            let evicted = self
                .conflict_graph
                .remove_and_cascade(existing_hash)
                .unwrap_or_default();

            Ok(RBFChoice::ReplaceExisting {
                reason: RBFReason::DeterministicTiebreaker,
                evicted_descendants: evicted,
            })
        } else {
            stats.rejections += 1;
            Ok(RBFChoice::KeepExisting)
        }
    }

    /// Batch evaluate RBF for multiple conflicts
    pub fn evaluate_rbf_conflicts(
        &self,
        incoming_tx: &Transaction,
        incoming_hash: &TxHash,
        incoming_fee: u64,
        incoming_size: usize,
        conflicting_hashes: &[TxHash],
        conflict_txs: &[(Transaction, u64, usize)],
    ) -> Result<Vec<RBFChoice>, String> {
        if conflicting_hashes.len() != conflict_txs.len() {
            return Err("Mismatched number of conflicting transactions".to_string());
        }

        let mut choices = Vec::new();

        for (i, conflict_hash) in conflicting_hashes.iter().enumerate() {
            let (conflict_tx, conflict_fee, conflict_size) = &conflict_txs[i];

            let choice = self.evaluate_rbf_supremacy(
                incoming_tx,
                incoming_hash,
                incoming_fee,
                incoming_size,
                conflict_tx,
                conflict_hash,
                *conflict_fee,
                *conflict_size,
            )?;

            choices.push(choice);
        }

        Ok(choices)
    }

    /// Get RBF statistics
    pub fn get_stats(&self) -> RBFStats {
        self.stats.lock().clone()
    }

    /// Reset statistics
    pub fn reset_stats(&self) {
        *self.stats.lock() = RBFStats::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klomang_core::core::crypto::Hash;
    use klomang_core::core::state::transaction::{SigHashType, TxInput};

    fn create_test_tx(id: u8, prev_ids: Vec<u8>) -> Transaction {
        let mut inputs = Vec::new();
        for (idx, prev_id) in prev_ids.iter().enumerate() {
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
    fn test_rbf_higher_fee_rate() {
        let kv_store = Arc::new(crate::storage::kv_store::KvStore::new_dummy());
        let graph = Arc::new(ConflictGraph::new(kv_store));
        let rbf = RBFManager::new(graph);

        let tx_old = create_test_tx(1, vec![100]);
        let tx_new = create_test_tx(2, vec![100]);

        let hash_old = tx_hash(1);
        let hash_new = tx_hash(2);

        // Old: 1000 fee / 200 bytes = 5 sat/byte
        // New: 2000 fee / 100 bytes = 20 sat/byte (should replace)
        let choice = rbf
            .evaluate_rbf_supremacy(&tx_new, &hash_new, 2000, 100, &tx_old, &hash_old, 1000, 200)
            .unwrap();

        match choice {
            RBFChoice::ReplaceExisting { reason, .. } => {
                assert_eq!(reason, RBFReason::HigherFeeRateWithThreshold);
            }
            _ => panic!("Expected replacement"),
        }
    }

    #[test]
    fn test_rbf_insufficient_fee() {
        let kv_store = Arc::new(crate::storage::kv_store::KvStore::new_dummy());
        let graph = Arc::new(ConflictGraph::new(kv_store));
        let rbf = RBFManager::new(graph);

        let tx_old = create_test_tx(1, vec![100]);
        let tx_new = create_test_tx(2, vec![100]);

        let hash_old = tx_hash(1);
        let hash_new = tx_hash(2);

        // New fee is not sufficient (only 1001 vs required higher)
        let choice = rbf
            .evaluate_rbf_supremacy(&tx_new, &hash_new, 1001, 100, &tx_old, &hash_old, 1000, 100)
            .unwrap();

        assert_eq!(choice, RBFChoice::KeepExisting);
    }

    #[test]
    fn test_rbf_tiebreaker() {
        let kv_store = Arc::new(crate::storage::kv_store::KvStore::new_dummy());
        let graph = Arc::new(ConflictGraph::new(kv_store));
        let rbf = RBFManager::new(graph);

        let tx_old = create_test_tx(1, vec![100]);
        let tx_new = create_test_tx(2, vec![100]);

        // With lexicographically smaller hash
        let hash_old = tx_hash(0xFF); // Larger hash
        let hash_new = tx_hash(0x01); // Smaller hash

        // Same fee rate
        let choice = rbf
            .evaluate_rbf_supremacy(&tx_new, &hash_new, 1000, 100, &tx_old, &hash_old, 1000, 100)
            .unwrap();

        match choice {
            RBFChoice::ReplaceExisting { reason, .. } => {
                assert_eq!(reason, RBFReason::DeterministicTiebreaker);
            }
            _ => panic!("Expected replacement via tiebreaker"),
        }
    }
}
