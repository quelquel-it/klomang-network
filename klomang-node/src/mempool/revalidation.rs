//! Incremental Revalidation Engine
//!
//! Efficiently revalidates mempool transactions when new blocks arrive,
//! only checking affected transactions instead of the entire pool.

use std::collections::HashSet;
use std::sync::Arc;

use klomang_core::core::dag::BlockNode;
use klomang_core::core::state::transaction::Transaction;

use crate::storage::kv_store::KvStore;
use crate::storage::error::StorageResult;

use super::pool::{TransactionPool, PoolEntry};
use super::status::TransactionStatus;
use super::validation::{PoolValidator, ValidationResult};

/// Information about transaction conflicts in a block
#[derive(Clone, Debug)]
pub struct ConflictInfo {
    /// Transaction hashes that are in the block
    pub block_tx_hashes: HashSet<Vec<u8>>,
    
    /// UTXOs spent by block transactions
    pub spent_utxos: HashSet<(Vec<u8>, u32)>,
}

impl ConflictInfo {
    /// Create conflict info from a block
    pub fn from_block(block: &BlockNode) -> Self {
        let mut block_tx_hashes = HashSet::new();
        let mut spent_utxos = HashSet::new();

        for tx in &block.transactions {
            let tx_hash = bincode::serialize(&tx.id).unwrap_or_default();
            block_tx_hashes.insert(tx_hash);

            // Collect UTXOs spent by this transaction
            for input in &tx.inputs {
                let input_tx_hash = bincode::serialize(&input.prev_tx).unwrap_or_default();
                spent_utxos.insert((input_tx_hash, input.index));
            }
        }

        Self {
            block_tx_hashes,
            spent_utxos,
        }
    }

    /// Check if transaction is affected by block
    pub fn is_transaction_affected(&self, tx: &Transaction) -> bool {
        // Check if any inputs were spent by block transactions
        for input in &tx.inputs {
            let input_tx_hash = bincode::serialize(&input.prev_tx).unwrap_or_default();
            if self.spent_utxos.contains(&(input_tx_hash, input.index)) {
                return true;
            }
        }
        false
    }

    /// Get affected transactions from pool
    pub fn get_affected_txs(&self, pool_entries: &[PoolEntry]) -> Vec<Vec<u8>> {
        pool_entries
            .iter()
            .filter(|entry| self.is_transaction_affected(&entry.transaction))
            .map(|entry| bincode::serialize(&entry.transaction.id).unwrap_or_default())
            .collect()
    }
}

/// Result of revalidation for a transaction
#[derive(Clone, Debug)]
pub enum RevalidationResult {
    /// Transaction still valid
    StillValid,
    
    /// Transaction became invalid (double-spent by block)
    NowInvalid,
    
    /// Transaction moved from orphan to valid (dependencies now available)
    OrphanResolved,
    
    /// Transaction already in block (no change)
    InBlock,
}

/// Incremental revalidation engine for mempool
#[allow(dead_code)]
pub struct RevalidationEngine {
    pool: Arc<TransactionPool>,
    validator: Arc<PoolValidator>,
    kv_store: Arc<KvStore>,
}

impl RevalidationEngine {
    /// Create new revalidation engine
    pub fn new(
        pool: Arc<TransactionPool>,
        validator: Arc<PoolValidator>,
        kv_store: Arc<KvStore>,
    ) -> Self {
        Self {
            pool,
            validator,
            kv_store,
        }
    }

    /// Revalidate pool after new block arrival
    /// 
    /// Returns number of transactions affected
    pub fn revalidate_on_block(&self, new_block: &BlockNode) -> StorageResult<RevalidationStats> {
        let conflict_info = ConflictInfo::from_block(new_block);
        let mut stats = RevalidationStats::default();

        // Get all pool entries and identify affected ones
        let all_entries = self.pool.get_all();
        let affected_hashes = conflict_info.get_affected_txs(&all_entries);

        // For each affected transaction, revalidate
        for tx_hash in affected_hashes {
            if let Some(entry) = self.pool.get(&tx_hash) {
                stats.total_revalidated += 1;

                match self.revalidate_single_transaction(&entry, &conflict_info)? {
                    RevalidationResult::StillValid => {
                        stats.still_valid += 1;
                    }
                    RevalidationResult::NowInvalid => {
                        // Remove double-spent transaction
                        self.pool.remove(&tx_hash);
                        stats.removed_double_spent += 1;
                    }
                    RevalidationResult::OrphanResolved => {
                        // Move from orphan to validated
                        let _ = self.pool.set_status(&tx_hash, TransactionStatus::Validated);
                        stats.orphan_resolved += 1;
                    }
                    RevalidationResult::InBlock => {
                        // Transaction is in the block, mark as InBlock
                        let _ = self.pool.set_status(&tx_hash, TransactionStatus::InBlock);
                        stats.in_block += 1;
                    }
                }
            }
        }

        // After revalidation, check orphan pool for newly resolved dependencies
        let orphan_entries = self.pool.get_orphans();
        for entry in orphan_entries {
            if let Ok(true) = self.validator.try_validate_orphan(&entry.transaction) {
                let tx_hash = bincode::serialize(&entry.transaction.id).unwrap_or_default();
                let _ = self.pool.set_status(&tx_hash, TransactionStatus::Validated);
                stats.orphan_resolved += 1;
            }
        }

        Ok(stats)
    }

    /// Revalidate single transaction after block
    fn revalidate_single_transaction(
        &self,
        entry: &PoolEntry,
        conflict: &ConflictInfo,
    ) -> StorageResult<RevalidationResult> {
        let tx = &entry.transaction;

        // Check if transaction is in the block itself
        if conflict
            .block_tx_hashes
            .contains(&bincode::serialize(&tx.id).unwrap_or_default())
        {
            return Ok(RevalidationResult::InBlock);
        }

        // Check if transaction was affected by block
        if !conflict.is_transaction_affected(tx) {
            return Ok(RevalidationResult::StillValid);
        }

        // Revalidate against storage (after block committed)
        match self.validator.validate_transaction(tx)? {
            ValidationResult::Valid => {
                // Still valid even after block (different UTXOs)
                Ok(RevalidationResult::StillValid)
            }
            ValidationResult::MissingInputs(_) => {
                // Now we have missing inputs - was using spending UTXOs from block
                // Mark as orphan or invalid depending on context
                if entry.status == TransactionStatus::InOrphanPool {
                    Ok(RevalidationResult::OrphanResolved)
                } else {
                    // Was in validated but now invalid = double-spend
                    Ok(RevalidationResult::NowInvalid)
                }
            }
            _ => Ok(RevalidationResult::NowInvalid),
        }
    }

    /// Get statistics about affected transactions
    pub fn analyze_pool_impact(&self, new_block: &BlockNode) -> RevalidationImpactAnalysis {
        let conflict_info = ConflictInfo::from_block(new_block);
        let all_entries = self.pool.get_all();

        let affected = conflict_info.get_affected_txs(&all_entries);
        let affected_count = affected.len();
        let total_count = all_entries.len();

        let affected_fee: u64 = affected
            .iter()
            .filter_map(|hash| self.pool.get(hash).map(|e| e.total_fee))
            .sum();

        RevalidationImpactAnalysis {
            total_pool_size: total_count,
            affected_count,
            affected_percentage: if total_count > 0 {
                (affected_count as f64 / total_count as f64) * 100.0
            } else {
                0.0
            },
            affected_fees: affected_fee,
            block_transactions: new_block.transactions.len(),
        }
    }
}

/// Statistics from revalidation
#[derive(Clone, Debug, Default)]
pub struct RevalidationStats {
    /// Total transactions revalidated
    pub total_revalidated: usize,
    
    /// Transactions still valid after block
    pub still_valid: usize,
    
    /// Transactions removed due to double-spend
    pub removed_double_spent: usize,
    
    /// Orphan transactions resolved
    pub orphan_resolved: usize,
    
    /// Transactions found in block
    pub in_block: usize,
}

/// Analysis of block impact on pool
#[derive(Clone, Debug)]
pub struct RevalidationImpactAnalysis {
    pub total_pool_size: usize,
    pub affected_count: usize,
    pub affected_percentage: f64,
    pub affected_fees: u64,
    pub block_transactions: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use klomang_core::core::crypto::Hash;
    use klomang_core::core::state::transaction::{TxInput, TxOutput, SigHashType};
    use std::collections::HashSet;

    fn create_test_tx(id_seed: u8, inputs: Vec<(u8, u32)>) -> Transaction {
        let input_list = inputs
            .iter()
            .map(|(seed, index)| TxInput {
                prev_tx: Hash::new(&[*seed; 32]),
                index: *index,
                signature: vec![],
                pubkey: vec![],
                sighash_type: SigHashType::All,
            })
            .collect();

        Transaction {
            id: Hash::new(&[id_seed; 32]),
            inputs: input_list,
            outputs: vec![TxOutput {
                value: 1000,
                pubkey_hash: Hash::new(&[99u8; 32]),
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
    fn test_conflict_detection() {
        let block_tx = create_test_tx(1, vec![]);
        
        let mut block = BlockNode {
            header: klomang_core::core::dag::BlockHeader {
                id: Hash::new(&[0u8; 32]),
                parents: HashSet::new(),
                timestamp: 0,
                difficulty: 0,
                nonce: 0,
                verkle_root: Hash::new(&[0u8; 32]),
                verkle_proofs: None,
                signature: None,
            },
            children: HashSet::new(),
            selected_parent: None,
            blue_set: HashSet::new(),
            red_set: HashSet::new(),
            blue_score: 0,
            transactions: vec![block_tx],
        };

        let conflict_info = ConflictInfo::from_block(&block);
        assert_eq!(conflict_info.block_tx_hashes.len(), 1);
    }

    #[test]
    fn test_affected_transaction_detection() {
        let block_tx = create_test_tx(1, vec![(2, 0)]);
        
        let mut block = BlockNode {
            header: klomang_core::core::dag::BlockHeader {
                id: Hash::new(&[0u8; 32]),
                parents: HashSet::new(),
                timestamp: 0,
                difficulty: 0,
                nonce: 0,
                verkle_root: Hash::new(&[0u8; 32]),
                verkle_proofs: None,
                signature: None,
            },
            children: HashSet::new(),
            selected_parent: None,
            blue_set: HashSet::new(),
            red_set: HashSet::new(),
            blue_score: 0,
            transactions: vec![block_tx],
        };

        let conflict_info = ConflictInfo::from_block(&block);

        // Transaction that uses same input as block
        let pool_tx = create_test_tx(3, vec![(2, 0)]);
        assert!(conflict_info.is_transaction_affected(&pool_tx));

        // Transaction that doesn't conflict
        let safe_tx = create_test_tx(4, vec![(5, 0)]);
        assert!(!conflict_info.is_transaction_affected(&safe_tx));
    }
}
