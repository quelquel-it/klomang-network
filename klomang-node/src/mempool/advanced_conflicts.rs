//! Advanced Double-Spending Detection and Conflict Resolution
//!
//! Provides deterministic conflict resolution for double-spending scenarios,
//! supporting multi-input tracking and consistent resolution across all nodes.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;

use klomang_core::core::crypto::Hash;
use klomang_core::core::state::transaction::Transaction;

use crate::storage::kv_store::KvStore;
use crate::storage::error::StorageResult;

/// Represents an outpoint that can be claimed by multiple transactions
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct OutPoint {
    pub tx_id: Vec<u8>,
    pub index: u32,
}

impl OutPoint {
    pub fn new(tx_id: Vec<u8>, index: u32) -> Self {
        Self { tx_id, index }
    }

    pub fn from_hash(hash: &Hash, index: u32) -> StorageResult<Self> {
        let tx_id = bincode::serialize(hash)
            .map_err(|e| crate::storage::error::StorageError::SerializationError(e.to_string()))?;
        Ok(Self { tx_id, index })
    }
}

/// Transaction hash wrapper for conflict tracking
#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct TxHash(pub Vec<u8>);

impl TxHash {
    pub fn new(hash: Vec<u8>) -> Self {
        Self(hash)
    }

    pub fn from_hash(h: &Hash) -> StorageResult<Self> {
        let bytes = bincode::serialize(h)
            .map_err(|e| crate::storage::error::StorageError::SerializationError(e.to_string()))?;
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Result of double-spend conflict detection
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConflictType {
    /// Two transactions claim the same input
    DirectConflict {
        tx_a: TxHash,
        tx_b: TxHash,
        outpoint: OutPoint,
    },

    /// Indirect conflict via dependency chain
    IndirectConflict {
        original: TxHash,
        affected: Vec<TxHash>,
    },

    /// No conflict detected
    NoConflict,
}

/// Resolution result for conflicting transactions
#[derive(Clone, Debug)]
pub struct ResolutionResult {
    /// Winner transaction (should be kept)
    pub winner: TxHash,

    /// Loser transaction (should be evicted)
    pub loser: TxHash,

    /// Reason for resolution
    pub reason: ResolutionReason,

    /// All affected transactions in dependency chain
    pub affected: Vec<TxHash>,
}

/// Reason why a transaction won the conflict resolution
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolutionReason {
    HigherFeeRate,
    EarlierArrival,
    LexicographicalHash,
}

/// Statistics for conflict operations
#[derive(Clone, Debug, Default)]
pub struct ConflictStats {
    pub total_conflicts_detected: u64,
    pub total_resolutions: u64,
    pub total_evictions: u64,
    pub direct_conflicts: u64,
    pub indirect_conflicts: u64,
}

/// Multi-input conflict tracking map
#[allow(dead_code)]
pub struct ConflictMap {
    /// OutPoint → Set of TxHashes claiming it
    conflicts: Arc<Mutex<HashMap<OutPoint, HashSet<TxHash>>>>,

    /// Track transaction arrival times for deterministic ordering
    arrival_times: Arc<Mutex<HashMap<TxHash, u64>>>,

    /// Statistics
    stats: Arc<Mutex<ConflictStats>>,

    /// Reference to storage for UTXO verification
    kv_store: Arc<KvStore>,
}

impl ConflictMap {
    /// Create new conflict map
    pub fn new(kv_store: Arc<KvStore>) -> Self {
        Self {
            conflicts: Arc::new(Mutex::new(HashMap::new())),
            arrival_times: Arc::new(Mutex::new(HashMap::new())),
            stats: Arc::new(Mutex::new(ConflictStats::default())),
            kv_store,
        }
    }

    /// Register a transaction and detect conflicts
    pub fn register_transaction(
        &self,
        tx: &Transaction,
        tx_hash: &TxHash,
    ) -> Result<ConflictType, String> {
        let mut conflicts = self.conflicts.lock();
        let mut arrival_times = self.arrival_times.lock();

        // Record arrival time
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        arrival_times.insert(tx_hash.clone(), current_time);

        // Check each input for conflicts
        for (idx, input) in tx.inputs.iter().enumerate() {
            let outpoint = OutPoint::from_hash(&input.prev_tx, idx as u32)
                .map_err(|e| format!("Failed to create outpoint: {}", e))?;

            // Check if this outpoint is already claimed
            if let Some(claiming_txs) = conflicts.get_mut(&outpoint) {
                if !claiming_txs.is_empty() {
                    let conflict_tx = claiming_txs.iter().next().unwrap().clone();

                    let mut stats = self.stats.lock();
                    stats.direct_conflicts += 1;
                    stats.total_conflicts_detected += 1;

                    return Ok(ConflictType::DirectConflict {
                        tx_a: conflict_tx,
                        tx_b: tx_hash.clone(),
                        outpoint,
                    });
                }
            } else {
                conflicts.insert(outpoint.clone(), HashSet::new());
            }

            // Register this transaction's claim
            conflicts
                .get_mut(&outpoint)
                .unwrap()
                .insert(tx_hash.clone());
        }

        Ok(ConflictType::NoConflict)
    }

    /// Resolve conflict between two transactions deterministically
    pub fn resolve_conflict(
        &self,
        _tx_a: &Transaction,
        _tx_b: &Transaction,
        tx_a_hash: &TxHash,
        tx_b_hash: &TxHash,
        tx_a_size: usize,
        tx_b_size: usize,
        tx_a_fee: u64,
        tx_b_fee: u64,
    ) -> Result<ResolutionResult, String> {
        let arrival_times = self.arrival_times.lock();

        let time_a = arrival_times
            .get(tx_a_hash)
            .copied()
            .unwrap_or(u64::MAX);
        let time_b = arrival_times
            .get(tx_b_hash)
            .copied()
            .unwrap_or(u64::MAX);

        drop(arrival_times);

        // Rule 1: Higher fee rate (satoshis per byte)
        let fee_rate_a = if tx_a_size > 0 {
            tx_a_fee as f64 / tx_a_size as f64
        } else {
            0.0
        };

        let fee_rate_b = if tx_b_size > 0 {
            tx_b_fee as f64 / tx_b_size as f64
        } else {
            0.0
        };

        let (winner, loser, reason) = if (fee_rate_a - fee_rate_b).abs() > 0.01 {
            // Rule 1: Fee rate differs
            if fee_rate_a > fee_rate_b {
                (
                    tx_a_hash.clone(),
                    tx_b_hash.clone(),
                    ResolutionReason::HigherFeeRate,
                )
            } else {
                (
                    tx_b_hash.clone(),
                    tx_a_hash.clone(),
                    ResolutionReason::HigherFeeRate,
                )
            }
        } else if time_a != time_b {
            // Rule 2: Timestamp differs - earlier wins
            if time_a < time_b {
                (
                    tx_a_hash.clone(),
                    tx_b_hash.clone(),
                    ResolutionReason::EarlierArrival,
                )
            } else {
                (
                    tx_b_hash.clone(),
                    tx_a_hash.clone(),
                    ResolutionReason::EarlierArrival,
                )
            }
        } else {
            // Rule 3: Lexicographical hash comparison
            if tx_a_hash.as_bytes() < tx_b_hash.as_bytes() {
                (
                    tx_a_hash.clone(),
                    tx_b_hash.clone(),
                    ResolutionReason::LexicographicalHash,
                )
            } else {
                (
                    tx_b_hash.clone(),
                    tx_a_hash.clone(),
                    ResolutionReason::LexicographicalHash,
                )
            }
        };

        let mut stats = self.stats.lock();
        stats.total_resolutions += 1;
        stats.total_evictions += 1;

        Ok(ResolutionResult {
            winner,
            loser,
            reason,
            affected: vec![],
        })
    }

    /// Resolve conflict using provided resolution logic
    ///
    /// This function uses deterministic rules to decide between conflicting transactions
    pub fn resolve_deterministic(
        &self,
        conflicts: ConflictType,
        tx_a: &Transaction,
        tx_b: &Transaction,
        tx_a_hash: &TxHash,
        tx_b_hash: &TxHash,
        tx_a_size: usize,
        tx_b_size: usize,
        tx_a_fee: u64,
        tx_b_fee: u64,
    ) -> Result<ResolutionResult, String> {
        match conflicts {
            ConflictType::DirectConflict { .. } => {
                self.resolve_conflict(
                    tx_a, tx_b, tx_a_hash, tx_b_hash, tx_a_size, tx_b_size, tx_a_fee, tx_b_fee,
                )
            }
            _ => Err("Cannot resolve non-direct conflicts here".to_string()),
        }
    }

    /// Remove transaction from conflict tracking
    pub fn remove_transaction(&self, tx_hash: &TxHash) -> Result<usize, String> {
        let mut conflicts = self.conflicts.lock();
        let mut arrival_times = self.arrival_times.lock();

        let mut removed_count = 0;

        // Find all outpoints claimed by this transaction
        let outpoints_to_clean: Vec<_> = conflicts
            .iter()
            .filter_map(|(outpoint, claiming_txs)| {
                if claiming_txs.contains(tx_hash) {
                    Some(outpoint.clone())
                } else {
                    None
                }
            })
            .collect();

        // Remove this transaction from all outpoints
        for outpoint in outpoints_to_clean {
            if let Some(claiming_txs) = conflicts.get_mut(&outpoint) {
                claiming_txs.remove(tx_hash);
                removed_count += 1;

                // If no more transactions claim this outpoint, remove the entry
                if claiming_txs.is_empty() {
                    conflicts.remove(&outpoint);
                }
            }
        }

        // Remove arrival time
        arrival_times.remove(tx_hash);

        Ok(removed_count)
    }

    /// Get all transactions claiming a specific outpoint
    pub fn get_claiming_transactions(&self, outpoint: &OutPoint) -> Vec<TxHash> {
        let conflicts = self.conflicts.lock();
        conflicts
            .get(outpoint)
            .map(|txs| txs.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Check if outpoint has conflicts
    pub fn has_conflicts(&self, outpoint: &OutPoint) -> bool {
        let conflicts = self.conflicts.lock();
        conflicts
            .get(outpoint)
            .map(|txs| txs.len() > 1)
            .unwrap_or(false)
    }

    /// Get all conflicted outpoints
    pub fn get_conflicted_outpoints(&self) -> Vec<OutPoint> {
        let conflicts = self.conflicts.lock();
        conflicts
            .iter()
            .filter(|(_, txs)| txs.len() > 1)
            .map(|(outpoint, _)| outpoint.clone())
            .collect()
    }

    /// Get statistics
    pub fn get_stats(&self) -> ConflictStats {
        self.stats.lock().clone()
    }

    /// Clear all conflicts (for testing)
    pub fn clear(&self) {
        self.conflicts.lock().clear();
        self.arrival_times.lock().clear();
    }

    /// Get size of conflict map
    pub fn len(&self) -> usize {
        self.conflicts.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.conflicts.lock().is_empty()
    }
}

impl Drop for ConflictMap {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klomang_core::core::state::transaction::TxInput;

    fn create_test_tx(id: u8, inputs: usize) -> Transaction {
        let mut tx_inputs = Vec::new();
        for i in 0..inputs {
            tx_inputs.push(TxInput {
                prev_tx: Hash::new(&[(id as u8).wrapping_add(i as u8); 32]),
                index: i as u32,
            });
        }

        Transaction {
            id: Hash::new(&[id; 32]),
            inputs: tx_inputs,
            outputs: vec![],
            execution_payload: vec![],
            contract_address: None,
            gas_limit: 0,
            max_fee_per_gas: 0,
            chain_id: 1,
            locktime: 0,
        }
    }

    #[test]
    fn test_conflict_map_creation() {
        let kv_store = Arc::new(KvStore::new_test());
        let map = ConflictMap::new(kv_store);
        assert!(map.is_empty());
    }

    #[test]
    fn test_register_transaction_no_conflict() {
        let kv_store = Arc::new(KvStore::new_test());
        let map = ConflictMap::new(kv_store);

        let tx = create_test_tx(1, 1);
        let tx_hash = TxHash::new(bincode::serialize(&tx.id).unwrap());

        let result = map.register_transaction(&tx, &tx_hash);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ConflictType::NoConflict);
    }

    #[test]
    fn test_direct_conflict_detection() {
        let kv_store = Arc::new(KvStore::new_test());
        let map = ConflictMap::new(kv_store);

        let tx1 = create_test_tx(1, 1);
        let tx2 = create_test_tx(2, 1); // Uses same prev_tx as tx1

        let tx1_hash = TxHash::new(bincode::serialize(&tx1.id).unwrap());
        let tx2_hash = TxHash::new(bincode::serialize(&tx2.id).unwrap());

        // Register first transaction
        let result1 = map.register_transaction(&tx1, &tx1_hash);
        assert_eq!(result1.unwrap(), ConflictType::NoConflict);

        // Register second transaction - should detect conflict
        let result2 = map.register_transaction(&tx2, &tx2_hash);
        match result2 {
            Ok(ConflictType::DirectConflict { tx_a, tx_b, .. }) => {
                assert_eq!(tx_a, tx1_hash);
                assert_eq!(tx_b, tx2_hash);
            }
            _ => panic!("Expected direct conflict"),
        }
    }

    #[test]
    fn test_deterministic_resolution_fee_rate() {
        let kv_store = Arc::new(KvStore::new_test());
        let map = ConflictMap::new(kv_store);

        let tx_a = create_test_tx(1, 1);
        let tx_b = create_test_tx(2, 1);

        let tx_a_hash = TxHash::new(bincode::serialize(&tx_a.id).unwrap());
        let tx_b_hash = TxHash::new(bincode::serialize(&tx_b.id).unwrap());

        map.register_transaction(&tx_a, &tx_a_hash).ok();
        map.register_transaction(&tx_b, &tx_b_hash).ok();

        // TX-A: 2000 fee / 100 bytes = 20 sat/byte
        // TX-B: 1000 fee / 100 bytes = 10 sat/byte
        let result = map.resolve_conflict(
            &tx_a, &tx_b, &tx_a_hash, &tx_b_hash, 100, 100, 2000, 1000,
        );

        assert!(result.is_ok());
        let res = result.unwrap();
        assert_eq!(res.winner, tx_a_hash);
        assert_eq!(res.loser, tx_b_hash);
        assert_eq!(res.reason, ResolutionReason::HigherFeeRate);
    }

    #[test]
    fn test_remove_transaction() {
        let kv_store = Arc::new(KvStore::new_test());
        let map = ConflictMap::new(kv_store);

        let tx = create_test_tx(1, 2);
        let tx_hash = TxHash::new(bincode::serialize(&tx.id).unwrap());

        map.register_transaction(&tx, &tx_hash).ok();
        assert!(!map.is_empty());

        let result = map.remove_transaction(&tx_hash);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 2); // 2 inputs removed
    }

    #[test]
    fn test_conflicted_outpoints() {
        let kv_store = Arc::new(KvStore::new_test());
        let map = ConflictMap::new(kv_store);

        let tx1 = create_test_tx(1, 1);
        let tx2 = create_test_tx(2, 1);

        let tx1_hash = TxHash::new(bincode::serialize(&tx1.id).unwrap());
        let tx2_hash = TxHash::new(bincode::serialize(&tx2.id).unwrap());

        map.register_transaction(&tx1, &tx1_hash).ok();
        map.register_transaction(&tx2, &tx2_hash).ok();

        let conflicted = map.get_conflicted_outpoints();
        assert_eq!(conflicted.len(), 1);
    }
}
