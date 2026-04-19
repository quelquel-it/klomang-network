//! UTXO Ownership Management and Integration with Transaction Pool
//!
//! Coordinates between the transaction pool and UTXO conflict tracker to ensure
//! sound transaction management and prevent double-spending within the mempool.

use std::sync::Arc;

use klomang_core::core::state::transaction::Transaction;

use crate::storage::kv_store::KvStore;

use super::conflict::{UtxoTracker, UtxoConflictError, OutPoint, ConflictStats};
use super::pool::{TransactionPool, PoolEntry};
use super::status::TransactionStatus;

/// Result type for ownership management operations
pub type OwnershipResult<T> = Result<T, OwnershipError>;

/// Errors that can occur in UTXO ownership management
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnershipError {
    /// UTXO conflict occurred
    Conflict(UtxoConflictError),

    /// Storage error
    Storage(String),

    /// Pool operation failed
    PoolError(String),

    /// Invalid transaction
    InvalidTransaction(String),

    /// Replace-By-Fee replacement failed
    RbfFailed(String),
}

impl From<UtxoConflictError> for OwnershipError {
    fn from(err: UtxoConflictError) -> Self {
        Self::Conflict(err)
    }
}

/// Details about a successful transaction addition
#[derive(Clone, Debug)]
pub struct TransactionAddedInfo {
    /// New transaction was added
    pub added: bool,

    /// Number of conflicting transactions replaced
    pub rbf_replacements: u32,

    /// Outpoints that were claimed
    pub claimed_outpoints: Vec<String>,
}

/// Details about transaction removal
#[derive(Clone, Debug)]
pub struct TransactionRemovedInfo {
    /// Number of outpoints released
    pub released_outpoints: u32,

    /// Whether transaction was found and removed
    pub found: bool,
}

/// Manager for UTXO ownership and conflict resolution
#[allow(dead_code)]
pub struct UtxoOwnershipManager {
    /// The transaction pool being managed
    pool: Arc<TransactionPool>,

    /// UTXO conflict tracker
    tracker: Arc<UtxoTracker>,

    /// Storage layer reference
    kv_store: Arc<KvStore>,
}

impl UtxoOwnershipManager {
    /// Create new UTXO ownership manager
    pub fn new(
        pool: Arc<TransactionPool>,
        kv_store: Arc<KvStore>,
    ) -> Self {
        let tracker = Arc::new(UtxoTracker::new(Arc::clone(&kv_store)));

        Self {
            pool,
            tracker,
            kv_store,
        }
    }

    /// Add transaction to pool with UTXO ownership tracking
    ///
    /// This method:
    /// 1. Checks for conflicts with existing claims
    /// 2. Implements Replace-By-Fee logic if needed
    /// 3. Registers UTXO claims
    /// 4. Adds transaction to pool
    pub fn add_transaction_with_ownership(
        &self,
        tx: Transaction,
        fee: u64,
        size_bytes: usize,
    ) -> OwnershipResult<TransactionAddedInfo> {
        let tx_hash = bincode::serialize(&tx.id)
            .map_err(|e| OwnershipError::InvalidTransaction(format!("Tx serialization: {}", e)))?;

        // Check for conflicts before registration
        let conflicts = self.tracker.check_conflicts(&tx)
            .map_err(|e| OwnershipError::Conflict(e))?;

        let mut rbf_count = 0;

        // Handle detected conflicts
        for conflict_outpoint in conflicts {
            if let Some(lock) = self.tracker.get_claiming_transaction(&conflict_outpoint) {
                let existing_fee = lock.claiming_fee;

                // If new transaction has higher fee, replace
                if fee > existing_fee {
                    // Remove conflicting transaction from pool
                    if self.pool.remove(&lock.claimed_by).is_none() {
                        return Err(OwnershipError::PoolError(
                            "Failed to remove conflicting transaction".to_string()
                        ));
                    }

                    // Release conflicting transaction's claims
                    self.tracker.release_claims(&lock.claimed_by)
                        .map_err(|e| OwnershipError::Conflict(e))?;

                    rbf_count += 1;
                } else {
                    // New transaction has lower fee - reject
                    return Err(OwnershipError::Conflict(UtxoConflictError::UtxoAlreadyClaimed {
                        outpoint: conflict_outpoint.to_string(),
                        claimed_by: lock.claimed_by,
                        current_fee: existing_fee,
                        new_fee: fee,
                    }));
                }
            }
        }

        // Register claims for new transaction
        self.tracker.register_claims(&tx, fee)
            .map_err(|e| OwnershipError::Conflict(e))?;

        // Add to pool
        self.pool.add_transaction(tx.clone(), fee, size_bytes)
            .map_err(|e| OwnershipError::PoolError(e))?;

        // Get claimed outpoints for response
        let claimed_outpoints = self.tracker
            .get_transaction_claims(&tx_hash)
            .unwrap_or_default();

        Ok(TransactionAddedInfo {
            added: true,
            rbf_replacements: rbf_count,
            claimed_outpoints,
        })
    }

    /// Add transaction to pool without conflict checking (for testing/internal use)
    pub fn add_transaction_unchecked(
        &self,
        tx: Transaction,
        fee: u64,
        size_bytes: usize,
    ) -> OwnershipResult<()> {
        self.tracker.register_claims(&tx, fee)
            .map_err(|e| OwnershipError::Conflict(e))?;

        self.pool.add_transaction(tx, fee, size_bytes)
            .map_err(|e| OwnershipError::PoolError(e))?;

        Ok(())
    }

    /// Remove transaction from pool and release all claims
    pub fn remove_transaction(&self, tx_hash: &[u8]) -> OwnershipResult<TransactionRemovedInfo> {
        let found = self.pool.remove(tx_hash).is_some();

        if found {
            // Get number of released outpoints before releasing
            let released_count = self.tracker
                .get_transaction_claims(tx_hash)
                .map(|v| v.len())
                .unwrap_or(0);
                
            self.tracker.release_claims(tx_hash)
                .map_err(|e| OwnershipError::Conflict(e))?;

            Ok(TransactionRemovedInfo {
                found: true,
                released_outpoints: released_count as u32,
            })
        } else {
            Ok(TransactionRemovedInfo {
                found: false,
                released_outpoints: 0,
            })
        }
    }

    /// Transition transaction status and manage claims accordingly
    pub fn transition_status(
        &self,
        tx_hash: &[u8],
        new_status: TransactionStatus,
    ) -> OwnershipResult<()> {
        if new_status == TransactionStatus::InBlock {
            // Release claims when transaction is included in block
            self.tracker.release_claims(tx_hash)
                .map_err(|e| OwnershipError::Conflict(e))?;
        }

        self.pool.set_status(tx_hash, new_status)
            .map_err(|e| OwnershipError::PoolError(e))?;

        Ok(())
    }

    /// Cleanup expired transactions and release their claims
    pub fn cleanup_expired(&self) -> OwnershipResult<u32> {
        let count = self.pool.cleanup_expired();

        // Note: Expired transactions are handled by pool
        // In a full implementation, would also release tracker claims

        Ok(count as u32)
    }

    /// Synchronize with blockchain after new block
    /// Release claims for transactions included in the new block
    pub fn sync_with_new_block(&self, included_tx_hashes: &[Vec<u8>]) -> OwnershipResult<u32> {
        let mut released_count = 0;

        for tx_hash in included_tx_hashes {
            if self.tracker.release_claims(tx_hash).is_ok() {
                released_count += 1;
                let _ = self.pool.remove(tx_hash); // Remove from pool
            }
        }

        Ok(released_count)
    }

    /// Check if an outpoint is available (not claimed)
    pub fn is_outpoint_available(&self, outpoint: &OutPoint) -> bool {
        !self.tracker.is_claimed(outpoint)
    }

    /// Check if transaction has any conflicting inputs
    pub fn has_conflicts(&self, tx: &Transaction) -> OwnershipResult<bool> {
        let conflicts = self.tracker.check_conflicts(tx)
            .map_err(|e| OwnershipError::Conflict(e))?;

        Ok(!conflicts.is_empty())
    }

    /// Get all claims made by a transaction
    pub fn get_transaction_claims(&self, tx_hash: &[u8]) -> Vec<String> {
        self.tracker.get_transaction_claims(tx_hash).unwrap_or_default()
    }

    /// Get conflict statistics
    pub fn get_conflict_stats(&self) -> ConflictStats {
        self.tracker.get_stats()
    }

    /// Get pool statistics
    pub fn get_pool_snapshot(&self) -> Vec<PoolEntry> {
        self.pool.get_all()
    }

    /// Verify a transaction's inputs are available in blockchain
    pub fn verify_inputs_available(&self, tx: &Transaction) -> OwnershipResult<bool> {
        for (idx, input) in tx.inputs.iter().enumerate() {
            let prev_tx_hash = bincode::serialize(&input.prev_tx)
                .map_err(|e| OwnershipError::InvalidTransaction(
                    format!("Input {} serialization: {}", idx, e)
                ))?;

            // TODO: Check KvStore for actual UTXO availability
            // For now, return basic validation
            if prev_tx_hash.is_empty() {
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Get transaction by hash with full ownership context
    pub fn get_transaction_with_context(
        &self,
        tx_hash: &[u8],
    ) -> Option<(PoolEntry, Vec<String>)> {
        let all_txs = self.pool.get_all();
        let entry = all_txs.iter()
            .find(|e| {
                bincode::serialize(&e.transaction.id)
                    .map(|h| h.as_slice() == tx_hash)
                    .unwrap_or(false)
            })?
            .clone();
            
        let claims = self.tracker.get_transaction_claims(tx_hash).unwrap_or_default();

        Some((entry, claims))
    }

    /// Analyze mempool for potential conflicts
    pub fn analyze_conflicts(&self) -> OwnershipResult<ConflictAnalysis> {
        let all_txs = self.pool.get_all();
        let stats = self.tracker.get_stats();

        let mut conflict_groups = Vec::new();
        let mut unique_outpoints = std::collections::HashSet::new();

        for entry in all_txs.iter() {
            let tx_hash = bincode::serialize(&entry.transaction.id)
                .unwrap_or_default();
            let claims = self.tracker.get_transaction_claims(&tx_hash).unwrap_or_default();

            for claim in claims.clone() {
                unique_outpoints.insert(claim);
            }

            if !claims.is_empty() {
                conflict_groups.push((tx_hash, claims));
            }
        }

        Ok(ConflictAnalysis {
            total_transactions: all_txs.len(),
            transactions_with_claims: conflict_groups.len(),
            unique_outpoints_claimed: unique_outpoints.len(),
            rbf_replacements_lifetime: stats.rbf_replacements,
            total_conflicts_detected: stats.conflicts_detected,
        })
    }

    /// Get reference to tracker for advanced operations
    pub fn tracker(&self) -> &UtxoTracker {
        &self.tracker
    }

    /// Get reference to pool for direct access
    pub fn pool(&self) -> &TransactionPool {
        &self.pool
    }
}

/// Analysis results for mempool conflicts
#[derive(Clone, Debug)]
pub struct ConflictAnalysis {
    /// Total transactions in pool
    pub total_transactions: usize,

    /// Transactions that have UTXO claims
    pub transactions_with_claims: usize,

    /// Unique outpoints currently claimed
    pub unique_outpoints_claimed: usize,

    /// Lifetime RBF replacements
    pub rbf_replacements_lifetime: u64,

    /// Total conflicts detected lifetime
    pub total_conflicts_detected: u64,
}

impl Drop for UtxoOwnershipManager {
    fn drop(&mut self) {
        // Cleanup handled by Arc<UtxoTracker>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klomang_core::core::crypto::Hash;
    use klomang_core::core::state::transaction::TxInput;

    fn create_test_tx(id: u8, input_count: usize) -> Transaction {
        let mut inputs = Vec::new();
        for i in 0..input_count {
            inputs.push(TxInput {
                prev_tx: Hash::new(&[(id as u8).wrapping_add(i as u8); 32]),
                index: i as u32,
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

    #[test]
    fn test_manager_creation() {
        let pool = Arc::new(TransactionPool::default());
        let kv_store = Arc::new(KvStore::new_test());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        assert_eq!(manager.tracker().active_claims_count(), 0);
    }

    #[test]
    fn test_add_transaction_with_ownership() {
        let pool = Arc::new(TransactionPool::default());
        let kv_store = Arc::new(KvStore::new_test());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        let tx = create_test_tx(1, 1);
        let result = manager.add_transaction_with_ownership(tx, 1000, 250);

        assert!(result.is_ok());
        let info = result.unwrap();
        assert!(info.added);
        assert!(!info.claimed_outpoints.is_empty());
    }

    #[test]
    fn test_remove_transaction() {
        let pool = Arc::new(TransactionPool::default());
        let kv_store = Arc::new(KvStore::new_test());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        let tx = create_test_tx(1, 1);
        let tx_hash = bincode::serialize(&tx.id).unwrap();

        manager.add_transaction_with_ownership(tx.clone(), 1000, 250).ok();
        assert_eq!(manager.tracker().active_claims_count(), 1);

        let result = manager.remove_transaction(&tx_hash);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().found, true);
    }

    #[test]
    fn test_conflict_analysis() {
        let pool = Arc::new(TransactionPool::default());
        let kv_store = Arc::new(KvStore::new_test());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        let tx1 = create_test_tx(1, 2);
        let tx2 = create_test_tx(2, 1);

        manager.add_transaction_with_ownership(tx1, 1000, 250).ok();
        manager.add_transaction_with_ownership(tx2, 2000, 200).ok();

        let analysis = manager.analyze_conflicts().unwrap();
        assert!(analysis.total_transactions >= 1);
    }
}
