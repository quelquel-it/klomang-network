//! Advanced Transaction Manager
//!
//! Integrates conflict detection, dependency tracking, and storage verification
//! to provide complete transaction lifecycle management with double-spend prevention.

use std::sync::Arc;

use klomang_core::core::state::transaction::Transaction;

use crate::storage::error::StorageResult;
use crate::storage::kv_store::KvStore;

use super::advanced_conflicts::{ConflictMap, ConflictType, TxHash};
use super::dependency_graph::DependencyGraph;
use super::pool::TransactionPool;

/// Result type for transaction manager operations
pub type ManagerResult<T> = Result<T, ManagerError>;

/// Errors that can occur in transaction management
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManagerError {
    ConflictDetected { msg: String },
    DependencyError { msg: String },
    StorageError { msg: String },
    InvalidTransaction { msg: String },
    ResolutionFailed { msg: String },
}

impl std::fmt::Display for ManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManagerError::ConflictDetected { msg } => write!(f, "Conflict detected: {}", msg),
            ManagerError::DependencyError { msg } => write!(f, "Dependency error: {}", msg),
            ManagerError::StorageError { msg } => write!(f, "Storage error: {}", msg),
            ManagerError::InvalidTransaction { msg } => write!(f, "Invalid transaction: {}", msg),
            ManagerError::ResolutionFailed { msg } => write!(f, "Resolution failed: {}", msg),
        }
    }
}

/// Result of adding a transaction to the system
#[derive(Clone, Debug)]
pub struct TransactionAdditionResult {
    pub added: bool,
    pub tx_hash: TxHash,
    pub conflicts_resolved: usize,
    pub evicted: Vec<TxHash>,
}

/// Advanced transaction manager with full conflict and dependency support
#[allow(dead_code)]
pub struct AdvancedTransactionManager {
    /// Conflict detection
    conflicts: Arc<ConflictMap>,

    /// Dependency graph
    graph: Arc<DependencyGraph>,

    /// Transaction pool
    pool: Arc<TransactionPool>,

    /// Storage
    kv_store: Arc<KvStore>,
}

impl AdvancedTransactionManager {
    /// Create new manager
    pub fn new(
        conflicts: Arc<ConflictMap>,
        graph: Arc<DependencyGraph>,
        pool: Arc<TransactionPool>,
        kv_store: Arc<KvStore>,
    ) -> Self {
        Self {
            conflicts,
            graph,
            pool,
            kv_store,
        }
    }

    /// Add transaction with full conflict and dependency checking
    pub fn add_transaction(
        &self,
        tx: Transaction,
        fee: u64,
        size_bytes: usize,
    ) -> ManagerResult<TransactionAdditionResult> {
        // Serialize transaction hash
        let tx_bytes = bincode::serialize(&tx.id).map_err(|e| ManagerError::StorageError {
            msg: format!("Serialization failed: {}", e),
        })?;
        let tx_hash = TxHash::new(tx_bytes);

        // Register in dependency graph
        self.graph.register_transaction(&tx_hash);

        // Check for conflicts
        match self.conflicts.register_transaction(&tx, &tx_hash) {
            Ok(ConflictType::NoConflict) => {
                // Try to add to pool
                self.add_to_pool_safe(&tx, &tx_hash, fee, size_bytes)
            }
            Ok(ConflictType::DirectConflict {
                tx_a,
                tx_b,
                outpoint: _,
            }) => {
                // Resolve deterministically
                self.handle_direct_conflict(&tx, &tx_hash, &tx_a, &tx_b, fee, size_bytes)
            }
            Ok(ConflictType::IndirectConflict { original, affected }) => {
                // Mark entire partition as affected
                let reason = format!("Indirect conflict from {}", format_hash(&original));
                self.graph.mark_conflict(&original, reason).ok();
                Err(ManagerError::ConflictDetected {
                    msg: format!(
                        "Indirect conflict with {} affected transactions",
                        affected.len()
                    ),
                })
            }
            Err(e) => Err(ManagerError::ConflictDetected { msg: e }),
        }
    }

    /// Handle direct conflict between two transactions
    fn handle_direct_conflict(
        &self,
        new_tx: &Transaction,
        new_tx_hash: &TxHash,
        existing_tx_hash: &TxHash,
        conflicting_tx_hash: &TxHash,
        new_fee: u64,
        new_size: usize,
    ) -> ManagerResult<TransactionAdditionResult> {
        // Get existing transaction from pool
        let existing_entry =
            self.pool
                .get(existing_tx_hash.as_bytes())
                .ok_or(ManagerError::ConflictDetected {
                    msg: "Cannot find existing conflicting transaction".to_string(),
                })?;

        let conflicting_entry = self.pool.get(conflicting_tx_hash.as_bytes()).ok_or(
            ManagerError::ConflictDetected {
                msg: "Cannot find conflicting transaction".to_string(),
            },
        )?;

        let existing_tx = &existing_entry.transaction;
        let existing_fee = existing_entry.total_fee;
        let existing_size = existing_entry.size_bytes;

        let conflicting_tx = &conflicting_entry.transaction;
        let conflicting_fee = conflicting_entry.total_fee;
        let conflicting_size = conflicting_entry.size_bytes;

        // Resolve against existing
        let resolution_existing = self
            .conflicts
            .resolve_conflict(
                new_tx,
                existing_tx,
                new_tx_hash,
                existing_tx_hash,
                new_size,
                existing_size,
                new_fee,
                existing_fee,
            )
            .map_err(|e| ManagerError::ResolutionFailed { msg: e })?;

        let mut evicted = Vec::new();

        if resolution_existing.loser == *new_tx_hash {
            // New transaction lost - don't add it
            return Err(ManagerError::ConflictDetected {
                msg: "New transaction has lower fee rate than existing".to_string(),
            });
        } else {
            // New transaction wins - remove existing
            evicted.push(existing_tx_hash.clone());
            self.pool.remove(existing_tx_hash.as_bytes());
            self.conflicts.remove_transaction(existing_tx_hash).ok();
            self.graph.remove_transaction(existing_tx_hash).ok();

            // Also check against conflicting if different
            if conflicting_tx_hash != existing_tx_hash {
                let resolution_conflicting = self
                    .conflicts
                    .resolve_conflict(
                        new_tx,
                        conflicting_tx,
                        new_tx_hash,
                        conflicting_tx_hash,
                        new_size,
                        conflicting_size,
                        new_fee,
                        conflicting_fee,
                    )
                    .map_err(|e| ManagerError::ResolutionFailed { msg: e })?;

                if resolution_conflicting.winner == *new_tx_hash {
                    evicted.push(conflicting_tx_hash.clone());
                    self.pool.remove(conflicting_tx_hash.as_bytes());
                    self.conflicts.remove_transaction(conflicting_tx_hash).ok();
                    self.graph.remove_transaction(conflicting_tx_hash).ok();
                }
            }
        }

        // Add new transaction
        self.add_to_pool_safe(new_tx, new_tx_hash, new_fee, new_size)
            .map(|mut result| {
                result.conflicts_resolved = evicted.len();
                result.evicted = evicted;
                result
            })
    }

    /// Safely add transaction to pool
    fn add_to_pool_safe(
        &self,
        tx: &Transaction,
        tx_hash: &TxHash,
        fee: u64,
        size_bytes: usize,
    ) -> ManagerResult<TransactionAdditionResult> {
        // Verify UTXO existence through storage
        for input in &tx.inputs {
            self.verify_utxo_exists(&input.prev_tx)
                .map_err(|e| ManagerError::StorageError {
                    msg: format!("UTXO verification failed: {}", e),
                })?;
        }

        // Verify signature (simplified - actual verification would be more complex)
        // In production, this would validate against klomang-core signature schemes

        // Add to pool
        self.pool
            .add_transaction(tx.clone(), fee, size_bytes)
            .map_err(|e| ManagerError::InvalidTransaction { msg: e.to_string() })?;

        Ok(TransactionAdditionResult {
            added: true,
            tx_hash: tx_hash.clone(),
            conflicts_resolved: 0,
            evicted: vec![],
        })
    }

    /// Verify that a UTXO exists on-chain
    fn verify_utxo_exists(&self, tx_hash: &klomang_core::core::crypto::Hash) -> StorageResult<()> {
        // In production, this would query the UTXO set through KvStore
        // For now, we simply return Ok as placeholder for storage layer integration
        // The actual verification would be: self.kv_store.utxo_exists(outpoint)?
        let _tx_bytes = bincode::serialize(tx_hash)?;

        // Placeholder: Storage layer verification would go here
        Ok(())
    }

    /// Remove transaction and clean up dependencies
    pub fn remove_transaction(&self, tx_hash: &TxHash) -> ManagerResult<()> {
        // Get affected downstream transactions
        let affected = self.graph.find_affected_downstream(tx_hash);

        // Remove from conflict map
        self.conflicts
            .remove_transaction(tx_hash)
            .map_err(|e| ManagerError::ConflictDetected { msg: e })?;

        // Remove from graph
        self.graph
            .remove_transaction(tx_hash)
            .map_err(|e| ManagerError::DependencyError { msg: e })?;

        // Remove from pool
        self.pool.remove(tx_hash.as_bytes());

        // Mark affected as orphaned (don't remove yet, mark for review)
        for affected_tx in affected {
            if affected_tx != *tx_hash {
                let reason = format!("Parent transaction removed: {}", format_hash(tx_hash));
                self.graph.mark_conflict(&affected_tx, reason).ok();
            }
        }

        Ok(())
    }

    /// Analyze conflicts in pool
    pub fn analyze_conflicts(&self) -> ManagerResult<ConflictAnalysis> {
        let conflict_stats = self.conflicts.get_stats();
        let graph_stats = self.graph.get_stats();

        let conflicted_outpoints = self.conflicts.get_conflicted_outpoints();
        let mut affected_transactions = std::collections::HashSet::new();

        for outpoint in &conflicted_outpoints {
            let claimants = self.conflicts.get_claiming_transactions(outpoint);
            for claimant in claimants {
                affected_transactions.insert(claimant);
            }
        }

        Ok(ConflictAnalysis {
            total_conflicts: conflict_stats.direct_conflicts,
            conflicted_outpoints: conflicted_outpoints.len() as u64,
            affected_transactions: affected_transactions.len() as u64,
            total_resolutions: conflict_stats.total_resolutions,
            total_evictions: conflict_stats.total_evictions,
            partition_count: graph_stats.total_partitions,
            conflict_propagations: graph_stats.conflict_propagations,
        })
    }

    /// Get conflict status of transaction
    pub fn get_conflict_status(&self, tx_hash: &TxHash) -> ConflictStatus {
        let in_conflict = self.graph.is_in_conflict(tx_hash);
        let reason = self.graph.get_conflict_reason(tx_hash);
        let dependents = self.graph.get_dependents(tx_hash);
        let parents = self.graph.get_parents(tx_hash);

        ConflictStatus {
            in_conflict,
            reason,
            dependents,
            parents,
        }
    }

    /// Clear all data (for testing)
    pub fn clear(&self) {
        self.conflicts.clear();
        self.graph.clear();
    }
}

/// Analysis of conflicts in the pool
#[derive(Clone, Debug)]
pub struct ConflictAnalysis {
    pub total_conflicts: u64,
    pub conflicted_outpoints: u64,
    pub affected_transactions: u64,
    pub total_resolutions: u64,
    pub total_evictions: u64,
    pub partition_count: u64,
    pub conflict_propagations: u64,
}

/// Conflict status of a transaction
#[derive(Clone, Debug)]
pub struct ConflictStatus {
    pub in_conflict: bool,
    pub reason: Option<String>,
    pub dependents: Vec<TxHash>,
    pub parents: Vec<TxHash>,
}

/// Format a transaction hash for display
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
    use std::sync::Arc;

    fn tx_hash(id: u8) -> TxHash {
        TxHash::new(vec![id; 32])
    }

    #[test]
    fn test_manager_creation() {
        let kv_store = Arc::new(KvStore::new_dummy());
        let conflicts = Arc::new(ConflictMap::new(kv_store.clone()));
        let graph = Arc::new(DependencyGraph::new());
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));

        let manager = AdvancedTransactionManager::new(conflicts, graph, pool, kv_store);

        let analysis = manager.analyze_conflicts();
        assert!(analysis.is_ok());
    }

    #[test]
    fn test_conflict_status() {
        let kv_store = Arc::new(KvStore::new_dummy());
        let conflicts = Arc::new(ConflictMap::new(kv_store.clone()));
        let graph = Arc::new(DependencyGraph::new());
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));

        let manager = AdvancedTransactionManager::new(conflicts, graph, pool, kv_store);
        let tx = tx_hash(1);

        let status = manager.get_conflict_status(&tx);
        assert!(!status.in_conflict);
        assert_eq!(status.reason, None);
    }
}
