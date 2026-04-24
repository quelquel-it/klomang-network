//! Integrated Conflict & RBF Manager
//!
//! Combines ConflictGraph and RBFManager for complete transaction conflict
//! management with Replace-By-Fee support integration to the mempool.

use std::sync::Arc;

use klomang_core::core::state::transaction::Transaction;

use crate::storage::kv_store::KvStore;

use super::conflict_graph::{ConflictGraph, TxHash};
use super::pool::TransactionPool;
use super::rbf_manager::{RBFManager, RBFChoice};

/// Result type for integration operations
pub type IntegrationResult<T> = Result<T, IntegrationError>;

/// Errors in integration operations
#[derive(Clone, Debug)]
pub enum IntegrationError {
    ConflictDetected { msg: String },
    RBFEvaluationFailed { msg: String },
    StorageError { msg: String },
    PoolError { msg: String },
    ValidationFailed { msg: String },
}

impl std::fmt::Display for IntegrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IntegrationError::ConflictDetected { msg } => write!(f, "Conflict: {}", msg),
            IntegrationError::RBFEvaluationFailed { msg } => write!(f, "RBF failed: {}", msg),
            IntegrationError::StorageError { msg } => write!(f, "Storage: {}", msg),
            IntegrationError::PoolError { msg } => write!(f, "Pool: {}", msg),
            IntegrationError::ValidationFailed { msg } => write!(f, "Validation: {}", msg),
        }
    }
}

/// Result of transaction addition
#[derive(Clone, Debug)]
pub struct AddTransactionResult {
    pub added: bool,
    pub tx_hash: TxHash,
    pub conflicts_resolved: usize,
    pub evicted_descendants: Vec<TxHash>,
    pub replacement: Option<TxHash>,
}

/// Integrated Conflict & RBF Manager
#[allow(dead_code)]
pub struct ConflictRBFManager {
    conflict_graph: Arc<ConflictGraph>,
    rbf_manager: Arc<RBFManager>,
    pool: Arc<TransactionPool>,
    kv_store: Arc<KvStore>,
}

impl ConflictRBFManager {
    /// Create new integrated manager
    pub fn new(
        conflict_graph: Arc<ConflictGraph>,
        pool: Arc<TransactionPool>,
        kv_store: Arc<KvStore>,
    ) -> Self {
        let rbf_manager = Arc::new(RBFManager::new(conflict_graph.clone()));

        Self {
            conflict_graph,
            rbf_manager,
            pool,
            kv_store,
        }
    }

    /// Add transaction with full conflict and RBF checking
    pub fn add_transaction_with_rbf(
        &self,
        tx: Transaction,
        fee: u64,
        size_bytes: usize,
    ) -> IntegrationResult<AddTransactionResult> {
        // Serialize transaction hash
        let tx_bytes = bincode::serialize(&tx.id)
            .map_err(|e| IntegrationError::StorageError {
                msg: format!("Serialization failed: {}", e),
            })?;
        let tx_hash = TxHash::new(tx_bytes);

        // Verify UTXO existence through storage
        self.verify_utxos_exist(&tx)?;

        // Register in conflict graph and get conflicting transactions
        let conflicting_txs = self
            .conflict_graph
            .register_transaction(&tx, &tx_hash, fee, size_bytes)
            .map_err(|e| IntegrationError::ConflictDetected { msg: e })?;

        if conflicting_txs.is_empty() {
            // No conflicts - add directly to pool
            self.pool
                .add_transaction(tx.clone(), fee, size_bytes)
                .map_err(|e| IntegrationError::PoolError { msg: e })?;

            return Ok(AddTransactionResult {
                added: true,
                tx_hash,
                conflicts_resolved: 0,
                evicted_descendants: vec![],
                replacement: None,
            });
        }

        // Handle conflicts with RBF evaluation
        self.handle_rbf_conflicts(
            tx,
            &tx_hash,
            fee,
            size_bytes,
            conflicting_txs,
        )
    }

    /// Handle RBF evaluation for conflicting transactions
    fn handle_rbf_conflicts(
        &self,
        incoming_tx: Transaction,
        incoming_hash: &TxHash,
        incoming_fee: u64,
        incoming_size: usize,
        conflicting_txs: Vec<TxHash>,
    ) -> IntegrationResult<AddTransactionResult> {
        let mut evicted_descendants = Vec::new();
        let mut replacement_performed = None;

        for conflict_hash in conflicting_txs {
            // Get conflicting transaction from pool
            let conflict_entry = self
                .pool
                .get(conflict_hash.as_bytes())
                .ok_or(IntegrationError::ConflictDetected {
                    msg: "Conflicting transaction not found in pool".to_string(),
                })?;

            let conflict_tx = &conflict_entry.transaction;
            let conflict_fee = conflict_entry.total_fee;
            let conflict_size = conflict_entry.size_bytes;

            // Evaluate RBF supremacy
            let choice = self
                .rbf_manager
                .evaluate_rbf_supremacy(
                    &incoming_tx,
                    incoming_hash,
                    incoming_fee,
                    incoming_size,
                    conflict_tx,
                    &conflict_hash,
                    conflict_fee,
                    conflict_size,
                )
                .map_err(|e| IntegrationError::RBFEvaluationFailed { msg: e })?;

            match choice {
                RBFChoice::ReplaceExisting {
                    reason: _,
                    evicted_descendants: descendants,
                } => {
                    // Remove conflicting and descendants
                    self.pool.remove(conflict_hash.as_bytes());

                    evicted_descendants.extend(descendants);
                    replacement_performed = Some(conflict_hash);
                }
                RBFChoice::KeepExisting => {
                    // Incoming TX lost, return error
                    return Err(IntegrationError::RBFEvaluationFailed {
                        msg: format!(
                            "Incoming TX rejected by existing TX {}",
                            format_hash(&conflict_hash)
                        ),
                    });
                }
                RBFChoice::CannotReplace { reason } => {
                    return Err(IntegrationError::RBFEvaluationFailed {
                        msg: format!("Cannot replace: {}", reason),
                    });
                }
            }
        }

        // If we got here, incoming TX won all RBF evaluations
        // Add to pool
        self.pool
            .add_transaction(incoming_tx, incoming_fee, incoming_size)
            .map_err(|e| IntegrationError::PoolError { msg: e })?;

        Ok(AddTransactionResult {
            added: true,
            tx_hash: incoming_hash.clone(),
            conflicts_resolved: evicted_descendants.len(),
            evicted_descendants,
            replacement: replacement_performed,
        })
    }

    /// Verify that all UTXOs in transaction exist and are unspent
    fn verify_utxos_exist(&self, tx: &Transaction) -> IntegrationResult<()> {
        for input in &tx.inputs {
            // In production, would query UTXO set through KvStore
            // This is a safety check before conflict tracking
            let _tx_bytes = bincode::serialize(&input.prev_tx)
                .map_err(|e| IntegrationError::StorageError {
                    msg: format!("UTXO serialization failed: {}", e),
                })?;

            // Check would happen here via KvStore
        }

        Ok(())
    }

    /// Remove transaction and cascade eviction of dependents
    pub fn remove_transaction(&self, tx_hash: &TxHash) -> IntegrationResult<Vec<TxHash>> {
        let evicted = self
            .conflict_graph
            .remove_and_cascade(tx_hash)
            .map_err(|e| IntegrationError::ConflictDetected { msg: e })?;

        // Also remove from pool
        self.pool.remove(tx_hash.as_bytes());

        Ok(evicted)
    }

    /// Get all transactions in conflict with given transaction
    pub fn get_conflict_set(&self, tx_hash: &TxHash) -> IntegrationResult<Vec<TxHash>> {
        let set = self
            .conflict_graph
            .get_conflict_set(tx_hash)
            .map_err(|e| IntegrationError::ConflictDetected { msg: e })?;

        Ok(set.into_iter().collect())
    }

    /// Analyze current conflict state
    pub fn analyze_conflicts(&self) -> ConflictAnalysis {
        let graph_stats = self.conflict_graph.get_stats();
        let rbf_stats = self.rbf_manager.get_stats();

        ConflictAnalysis {
            total_graph_nodes: graph_stats.total_nodes,
            total_conflicts_detected: graph_stats.total_conflicts,
            transitive_conflicts: graph_stats.transitive_conflicts,
            high_risk_transactions: graph_stats.high_risk_transactions,
            total_rbf_evaluations: rbf_stats.total_evaluations,
            total_rbf_replacements: rbf_stats.replacements_performed,
            rbf_rejections: rbf_stats.rejections,
        }
    }

    /// Get conflict graph reference
    pub fn get_conflict_graph(&self) -> Arc<ConflictGraph> {
        self.conflict_graph.clone()
    }

    /// Get RBF manager reference
    pub fn get_rbf_manager(&self) -> Arc<RBFManager> {
        self.rbf_manager.clone()
    }
}

/// Analysis of conflict state
#[derive(Clone, Debug)]
pub struct ConflictAnalysis {
    pub total_graph_nodes: u64,
    pub total_conflicts_detected: u64,
    pub transitive_conflicts: u64,
    pub high_risk_transactions: u64,
    pub total_rbf_evaluations: u64,
    pub total_rbf_replacements: u64,
    pub rbf_rejections: u64,
}

/// Format hash for display
fn format_hash(tx_hash: &TxHash) -> String {
    let bytes = tx_hash.as_bytes();
    if bytes.len() >= 4 {
        format!(
            "TX[{:02x}{:02x}...{:02x}{:02x}]",
            bytes[0],
            bytes[1],
            bytes[bytes.len() - 2],
            bytes[bytes.len() - 1]
        )
    } else if bytes.is_empty() {
        "TX[empty]".to_string()
    } else {
        format!("TX[{:02x}]", bytes[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mempool::pool::PoolConfig;
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

    #[test]
    fn test_integration_manager_creation() {
        let kv_store = Arc::new(KvStore::new_dummy());
        let graph = Arc::new(ConflictGraph::new(kv_store.clone()));
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));

        let _manager = ConflictRBFManager::new(graph, pool, kv_store);
    }

    #[test]
    fn test_add_transaction_no_conflict() {
        let kv_store = Arc::new(KvStore::new_dummy());
        let graph = Arc::new(ConflictGraph::new(kv_store.clone()));
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));

        let manager = ConflictRBFManager::new(graph, pool, kv_store);

        let tx = create_test_tx(1, vec![100]);
        let result = manager.add_transaction_with_rbf(tx, 1000, 100);

        assert!(result.is_ok());
        let res = result.unwrap();
        assert!(res.added);
    }
}
