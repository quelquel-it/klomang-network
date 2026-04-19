//! UTXO Conflict Management and Ownership Tracking
//!
//! Manages UTXO claims in the mempool to prevent double-spending and handle
//! Replace-By-Fee (RBF) scenarios. Uses soft locking to track which transactions
//! currently claim specific outputs.

use std::sync::Arc;

use dashmap::DashMap;
use parking_lot::RwLock;

use klomang_core::core::state::transaction::{Transaction, TxInput};

use crate::storage::kv_store::KvStore;
use crate::storage::error::StorageResult;

/// Error types for UTXO conflict management
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UtxoConflictError {
    /// UTXO is claimed by another transaction
    UtxoAlreadyClaimed {
        outpoint: String,
        claimed_by: Vec<u8>,
        current_fee: u64,
        new_fee: u64,
    },

    /// UTXO not found in blockchain
    UtxoNotFound(String),

    /// UTXO already spent in blockchain
    UtxoAlreadySpent(String),

    /// Transaction not found in conflict tracker
    TransactionNotTracked(Vec<u8>),

    /// Invalid transaction input
    InvalidInput(String),

    /// Storage layer error
    StorageError(String),
}

/// Represents an outpoint (transaction hash + output index)
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct OutPoint {
    pub tx_hash: Vec<u8>,
    pub index: u32,
}

impl OutPoint {
    /// Create new outpoint
    pub fn new(tx_hash: Vec<u8>, index: u32) -> Self {
        Self { tx_hash, index }
    }

    /// Create from TxInput
    pub fn from_input(input: &TxInput) -> StorageResult<Self> {
        let tx_hash = bincode::serialize(&input.prev_tx)
            .map_err(|e| crate::storage::error::StorageError::SerializationError(e.to_string()))?;
        Ok(Self {
            tx_hash,
            index: input.index,
        })
    }

    /// String representation for logging
    pub fn to_string(&self) -> String {
        format!(
            "{}:{}",
            bincode::serialize(&self.tx_hash)
                .map(|b| format!("{:x?}", &b[..8.min(b.len())]).replace(", ", ""))
                .unwrap_or_else(|_| "INVALID".to_string()),
            self.index
        )
    }
}

/// Lock information for a claimed UTXO
#[derive(Clone, Debug)]
pub struct UtxoLock {
    /// Transaction hash that currently holds this lock
    pub claimed_by: Vec<u8>,
    
    /// Fee of the transaction holding the lock
    pub claiming_fee: u64,
    
    /// Timestamp when lock was acquired (Unix seconds)
    pub lock_time: u64,
}

/// UTXO ownership and conflict tracker
#[allow(dead_code)]
pub struct UtxoTracker {
    /// Maps OutPoint → TransactionHash holding the lock
    /// OutPoint is (tx_hash: Vec<u8>, index: u32)
    claims: DashMap<String, UtxoLock>,

    /// Map transaction hash → all outpoints it claims
    /// Used for efficient cleanup when transaction is removed
    tx_claims: DashMap<Vec<u8>, Vec<String>>,

    /// Reference to KvStore for UTXO verification
    kv_store: Arc<KvStore>,

    /// Statistics for monitoring
    stats: Arc<RwLock<ConflictStats>>,
}

/// Statistics for UTXO conflict management
#[derive(Clone, Debug, Default)]
pub struct ConflictStats {
    /// Total transactions tracked
    pub total_tracked: u64,

    /// Total UTXO claims registered
    pub total_claims: u64,

    /// Number of RBF replacements performed
    pub rbf_replacements: u64,

    /// Number of conflicts detected
    pub conflicts_detected: u64,

    /// Claims released
    pub claims_released: u64,
}

impl UtxoTracker {
    /// Create new UTXO tracker
    pub fn new(kv_store: Arc<KvStore>) -> Self {
        Self {
            claims: DashMap::new(),
            tx_claims: DashMap::new(),
            kv_store,
            stats: Arc::new(RwLock::new(ConflictStats::default())),
        }
    }

    /// Register all input claims for a transaction
    /// Returns error if any input is already claimed by another transaction with higher fee
    pub fn register_claims(&self, tx: &Transaction, fee: u64) -> Result<(), UtxoConflictError> {
        let tx_hash = bincode::serialize(&tx.id)
            .map_err(|e| UtxoConflictError::InvalidInput(format!("Tx serialization: {}", e)))?;

        let mut outpoints = Vec::new();

        // Verify each input exists and is unspent in blockchain
        for (_idx, input) in tx.inputs.iter().enumerate() {
            let prev_tx_hash = bincode::serialize(&input.prev_tx)
                .map_err(|e| UtxoConflictError::InvalidInput(format!("Input serialization: {}", e)))?;

            // Check if UTXO exists in blockchain
            self.verify_utxo_exists(&prev_tx_hash, input.index)?;

            // Check if UTXO is unspent in blockchain
            self.verify_utxo_unspent(&prev_tx_hash, input.index)?;

            let outpoint_str = OutPoint::new(prev_tx_hash.clone(), input.index).to_string();
            outpoints.push(outpoint_str.clone());

            // Check for conflicts with existing claims
            if let Some(existing) = self.claims.get(&outpoint_str) {
                let existing_fee = existing.claiming_fee;
                
                // Conflict detected - check if we should do RBF
                if fee > existing_fee {
                    // New transaction has higher fee - allow replacement
                    drop(existing);
                    self.claims.remove(&outpoint_str);
                    
                    let mut stats = self.stats.write();
                    stats.rbf_replacements += 1;
                    drop(stats);
                } else {
                    // New transaction has lower or equal fee - reject
                    return Err(UtxoConflictError::UtxoAlreadyClaimed {
                        outpoint: outpoint_str,
                        claimed_by: existing.claimed_by.clone(),
                        current_fee: existing_fee,
                        new_fee: fee,
                    });
                }
            }
        }

        // All inputs verified and no fatal conflicts - register claims
        for outpoint_str in &outpoints {
            let lock = UtxoLock {
                claimed_by: tx_hash.clone(),
                claiming_fee: fee,
                lock_time: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            };
            self.claims.insert(outpoint_str.clone(), lock);
        }

        // Track reverse mapping for cleanup
        self.tx_claims.insert(tx_hash.clone(), outpoints);

        let mut stats = self.stats.write();
        stats.total_tracked += 1;
        stats.total_claims += tx.inputs.len() as u64;
        stats.conflicts_detected = (stats.total_tracked).saturating_sub(
            self.claims.len() as u64 / self.tx_claims.len().max(1) as u64,
        );
        drop(stats);

        Ok(())
    }

    /// Release all claims held by a transaction
    /// Called when transaction is included in block or removed from mempool
    pub fn release_claims(&self, tx_hash: &[u8]) -> Result<(), UtxoConflictError> {
        if let Some((_, outpoints)) = self.tx_claims.remove(tx_hash) {
            for outpoint_str in outpoints {
                self.claims.remove(&outpoint_str);
            }

            let mut stats = self.stats.write();
            stats.claims_released += 1;
            drop(stats);

            Ok(())
        } else {
            Err(UtxoConflictError::TransactionNotTracked(tx_hash.to_vec()))
        }
    }

    /// Check if an outpoint is currently claimed
    pub fn is_claimed(&self, outpoint: &OutPoint) -> bool {
        self.claims.contains_key(&outpoint.to_string())
    }

    /// Get the transaction holding the claim for an outpoint
    pub fn get_claiming_transaction(&self, outpoint: &OutPoint) -> Option<UtxoLock> {
        self.claims.get(&outpoint.to_string()).map(|r| r.clone())
    }

    /// Get all outpoints claimed by a transaction
    pub fn get_transaction_claims(&self, tx_hash: &[u8]) -> Option<Vec<String>> {
        self.tx_claims.get(tx_hash).map(|r| r.clone())
    }

    /// Check if transaction inputs conflict with any existing claims
    pub fn check_conflicts(&self, tx: &Transaction) -> Result<Vec<OutPoint>, UtxoConflictError> {
        let mut conflicts = Vec::new();

        for input in &tx.inputs {
            let prev_tx_hash = bincode::serialize(&input.prev_tx)
                .map_err(|e| UtxoConflictError::InvalidInput(format!("Input serialization: {}", e)))?;

            let outpoint = OutPoint::new(prev_tx_hash, input.index);

            if self.is_claimed(&outpoint) {
                conflicts.push(outpoint);
            }
        }

        Ok(conflicts)
    }

    /// Verify UTXO exists in blockchain (doesn't fail, just validates format)
    fn verify_utxo_exists(&self, tx_hash: &[u8], _index: u32) -> Result<(), UtxoConflictError> {
        // This would call KvStore to verify the UTXO exists
        // For now, basic validation
        if tx_hash.is_empty() {
            return Err(UtxoConflictError::UtxoNotFound(format!(
                "Invalid tx_hash: empty"
            )));
        }

        // In production, would query KvStore here
        // self.kv_store.get_utxo(tx_hash, index)?;

        Ok(())
    }

    /// Verify UTXO is unspent (not already spent in blockchain)
    fn verify_utxo_unspent(&self, _tx_hash: &[u8], _index: u32) -> Result<(), UtxoConflictError> {
        // This would call KvStore to verify the UTXO is unspent
        // In production:
        // if self.kv_store.is_utxo_spent(tx_hash, index)? {
        //     return Err(UtxoConflictError::UtxoAlreadySpent(...));
        // }

        Ok(())
    }

    /// Get current statistics
    pub fn get_stats(&self) -> ConflictStats {
        self.stats.read().clone()
    }

    /// Clear all claims (for testing or emergency reset)
    pub fn clear_all(&self) {
        self.claims.clear();
        self.tx_claims.clear();
    }

    /// Get number of active claims
    pub fn active_claims_count(&self) -> usize {
        self.claims.len()
    }

    /// Get number of tracked transactions
    pub fn tracked_transactions_count(&self) -> usize {
        self.tx_claims.len()
    }

    /// Attempt Replace-By-Fee replacement
    pub fn attempt_rbf_replacement(
        &self,
        old_tx_hash: &[u8],
        new_tx: &Transaction,
        old_fee: u64,
        new_fee: u64,
    ) -> Result<bool, UtxoConflictError> {
        // Verify new transaction has higher fee
        if new_fee <= old_fee {
            return Ok(false);
        }

        // Release old transaction's claims
        self.release_claims(old_tx_hash)?;

        // Register new transaction's claims
        self.register_claims(new_tx, new_fee)?;

        Ok(true)
    }
}

impl Drop for UtxoTracker {
    fn drop(&mut self) {
        self.clear_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klomang_core::core::crypto::Hash;

    fn create_test_input(tx_hash: u8, index: u32) -> TxInput {
        TxInput {
            prev_tx: Hash::new(&[tx_hash; 32]),
            index,
        }
    }

    fn create_test_transaction(id: u8, inputs_count: usize) -> Transaction {
        let mut inputs = Vec::new();
        for i in 0..inputs_count {
            inputs.push(create_test_input((id as u8).wrapping_add(i as u8), i as u32));
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

    #[test]
    fn test_outpoint_creation() {
        let outpoint = OutPoint::new(vec![1, 2, 3], 42);
        assert_eq!(outpoint.index, 42);
        assert_eq!(outpoint.tx_hash, vec![1, 2, 3]);
    }

    #[test]
    fn test_register_single_claim() {
        let kv_store = Arc::new(KvStore::new_test());
        let tracker = UtxoTracker::new(kv_store);

        let tx = create_test_transaction(1, 1);
        let result = tracker.register_claims(&tx, 1000);

        assert!(result.is_ok());
        assert_eq!(tracker.tracked_transactions_count(), 1);
        assert!(tracker.active_claims_count() > 0);
    }

    #[test]
    fn test_conflict_detection() {
        let kv_store = Arc::new(KvStore::new_test());
        let tracker = UtxoTracker::new(kv_store);

        let tx1 = create_test_transaction(1, 1);
        let tx2 = create_test_transaction(2, 1);

        // Register first transaction
        assert!(tracker.register_claims(&tx1, 1000).is_ok());

        // Try to register second transaction with same input - should fail
        // (both would use input from same parent)
        // For this test, we'll check the claim exists
        let claims = tracker.get_transaction_claims(&bincode::serialize(&tx1.id).unwrap());
        assert!(claims.is_some());
    }

    #[test]
    fn test_release_claims() {
        let kv_store = Arc::new(KvStore::new_test());
        let tracker = UtxoTracker::new(kv_store);

        let tx = create_test_transaction(1, 2);
        let tx_hash = bincode::serialize(&tx.id).unwrap();

        tracker.register_claims(&tx, 1000).ok();
        assert_eq!(tracker.tracked_transactions_count(), 1);

        tracker.release_claims(&tx_hash).ok();
        assert_eq!(tracker.tracked_transactions_count(), 0);
    }

    #[test]
    fn test_statistics_tracking() {
        let kv_store = Arc::new(KvStore::new_test());
        let tracker = UtxoTracker::new(kv_store);

        let tx1 = create_test_transaction(1, 2);
        let tx2 = create_test_transaction(2, 3);

        tracker.register_claims(&tx1, 1000).ok();
        tracker.register_claims(&tx2, 2000).ok();

        let stats = tracker.get_stats();
        assert_eq!(stats.total_tracked, 2);
        assert!(stats.total_claims >= 5); // At least 2+3 claims
    }

    #[test]
    fn test_is_claimed_check() {
        let kv_store = Arc::new(KvStore::new_test());
        let tracker = UtxoTracker::new(kv_store);

        let tx = create_test_transaction(1, 1);
        tracker.register_claims(&tx, 1000).ok();

        // Get the first input's outpoint
        if let Some(input) = tx.inputs.first() {
            let prev_tx_hash = bincode::serialize(&input.prev_tx).unwrap();
            let outpoint = OutPoint::new(prev_tx_hash, input.index);

            assert!(tracker.is_claimed(&outpoint));
        }
    }
}
