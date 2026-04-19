//! Recursive Dependency Manager - Orchestrator for Ancestry Management
//!
//! This module provides the main coordinating interface for recursive dependency
//! management. It integrates the RecursiveDependencyTracker with transaction status
//! management and provides high-level operations for mempool management.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::RwLock;

use klomang_core::core::state::transaction::Transaction;

use crate::storage::kv_store::KvStore;
use crate::storage::error::{StorageResult, StorageError};
use super::recursive_dependency_tracker::{
    RecursiveDependencyTracker, TxHash, DependencyResolutionStatus,
    AncestryValidation, CascadeInvalidationResult, RecursiveDependencyStats,
};
use super::status::TransactionStatus;

/// Advanced configuration for recursive dependency management
#[derive(Clone, Debug)]
pub struct RecursiviveDependencyConfig {
    /// Maximum recursion depth to prevent stack overflow
    pub max_recursion_depth: u32,
    /// Maximum ancestry size per transaction
    pub max_ancestry_size: usize,
    /// Automatically cascade invalidate on ancestor removal
    pub auto_cascade_invalidate: bool,
    /// Enable cycle detection
    pub enable_cycle_detection: bool,
}

impl Default for RecursiviveDependencyConfig {
    fn default() -> Self {
        Self {
            max_recursion_depth: 1000,
            max_ancestry_size: 100000,
            auto_cascade_invalidate: true,
            enable_cycle_detection: true,
        }
    }
}

/// Transaction with resolution status
#[derive(Clone, Debug)]
pub struct TransactionWithStatus {
    /// Transaction hash
    pub tx_hash: TxHash,
    /// Current resolution status
    pub resolution_status: DependencyResolutionStatus,
    /// Full ancestry validation info
    pub validation: AncestryValidation,
    /// Depth from nearest root
    pub execution_depth: u32,
    /// Is this transaction itself marked invalid
    pub is_invalid: bool,
}

/// Result of bulk resolution operation
#[derive(Clone, Debug)]
pub struct BulkResolutionResult {
    /// Transactions that became Ready
    pub newly_ready: Vec<TxHash>,
    /// Transactions that became Invalid
    pub newly_invalid: Vec<TxHash>,
    /// Transactions still Unresolved
    pub still_unresolved: Vec<TxHash>,
}

/// Main Recursive Dependency Manager
pub struct RecursiveDependencyManager {
    /// Core dependency tracker
    tracker: Arc<RecursiveDependencyTracker>,
    /// Configuration
    config: Arc<RwLock<RecursiviveDependencyConfig>>,
    /// Status cache: tx_hash -> TransactionStatus from mempool
    status_cache: Arc<RwLock<HashMap<TxHash, TransactionStatus>>>,
}

impl RecursiveDependencyManager {
    /// Create new manager with default config
    pub fn new(kv_store: Arc<KvStore>) -> Self {
        Self::with_config(kv_store, RecursiviveDependencyConfig::default())
    }

    /// Create with custom configuration
    pub fn with_config(kv_store: Arc<KvStore>, config: RecursiviveDependencyConfig) -> Self {
        let tracker = Arc::new(RecursiveDependencyTracker::with_limits(
            kv_store,
            config.max_recursion_depth,
            config.max_ancestry_size,
        ));

        Self {
            tracker,
            config: Arc::new(RwLock::new(config)),
            status_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register transaction with full recursive ancestry tracking
    /// 
    /// This is the main entry point for adding a transaction to mempool.
    /// Automatically builds complete ancestor/descendant closure and
    /// validates dependency chain.
    pub fn register_transaction(
        &self,
        tx: &Transaction,
        status: TransactionStatus,
    ) -> StorageResult<TransactionWithStatus> {
        // Serialize transaction hash
        let tx_hash = bincode::serialize(&tx.id)
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;

        // Register with tracker
        let validation = self.tracker.register_transaction(tx)?;

        // Resolve and update status
        let resolution_status = self.tracker.resolve_dependency_chain(&tx_hash)?;

        // Update status cache
        {
            let mut cache = self.status_cache.write();
            cache.insert(tx_hash.clone(), status);
        }

        // Build result
        let result = TransactionWithStatus {
            tx_hash: tx_hash.clone(),
            resolution_status: resolution_status.clone(),
            validation: validation.clone(),
            execution_depth: validation.depth,
            is_invalid: resolution_status == DependencyResolutionStatus::Invalid,
        };

        // If newly invalid and auto-cascade enabled, cascade invalidate
        if result.is_invalid && self.config.read().auto_cascade_invalidate {
            let _ = self.tracker.cascade_invalidate(&tx_hash);
        }

        Ok(result)
    }

    /// Resolve entire dependency chain for a transaction
    /// 
    /// Checks all ancestors recursively and determines if transaction
    /// can be executed (all ancestors present and valid).
    pub fn resolve_dependency_chain(&self, tx_hash: &TxHash) -> StorageResult<DependencyResolutionStatus> {
        self.tracker.resolve_dependency_chain(tx_hash)
    }

    /// Mark ancestor as invalid, cascade invalidate all dependents
    /// 
    /// This is called when an ancestor transaction is rejected, conflicted,
    /// or otherwise becomes invalid. All its descendants are automatically
    /// marked invalid recursively.
    pub fn mark_ancestor_invalid(&self, tx_hash: &TxHash) -> StorageResult<CascadeInvalidationResult> {
        self.tracker.cascade_invalidate(tx_hash)
    }

    /// Perform validation on full ancestry tree
    /// 
    /// Returns detailed validation state including:
    /// - Current resolution status
    /// - Missing ancestors count
    /// - Invalid ancestors count
    /// - Full ancestor/descendant sets
    pub fn validate_full_ancestry(&self, tx_hash: &TxHash) -> StorageResult<AncestryValidation> {
        self.tracker.validate_ancestry(tx_hash)
    }

    /// Get all transactions ready for block inclusion
    /// 
    /// Performs topological sort on ready transactions by execution depth.
    /// Returns them in valid execution order.
    pub fn get_ready_transactions(&self) -> StorageResult<Vec<TxHash>> {
        let cache = self.status_cache.read();
        let mut ready_txs: Vec<TxHash> = Vec::new();

        for (tx_hash, status) in cache.iter() {
            if *status == TransactionStatus::Validated {
                if let Ok(resolution) = self.tracker.get_resolution_status(tx_hash) {
                    if resolution == DependencyResolutionStatus::Ready {
                        ready_txs.push(tx_hash.clone());
                    }
                }
            }
        }

        // Sort by execution depth for topological order
        ready_txs.sort_by_key(|tx| {
            self.tracker
                .validate_ancestry(tx)
                .map(|v| v.depth)
                .unwrap_or(0)
        });

        Ok(ready_txs)
    }

    /// Get all transactions with unresolved dependencies
    pub fn get_unresolved_transactions(&self) -> StorageResult<Vec<TxHash>> {
        let cache = self.status_cache.read();
        let mut unresolved = Vec::new();

        for (tx_hash, status) in cache.iter() {
            if *status != TransactionStatus::Rejected && *status != TransactionStatus::InBlock {
                if let Ok(resolution) = self.tracker.get_resolution_status(tx_hash) {
                    if resolution == DependencyResolutionStatus::Unresolved {
                        unresolved.push(tx_hash.clone());
                    }
                }
            }
        }

        Ok(unresolved)
    }

    /// Get all transactions marked as invalid
    pub fn get_invalid_transactions(&self) -> StorageResult<Vec<TxHash>> {
        let cache = self.status_cache.read();
        let mut invalid = Vec::new();

        for (tx_hash, status) in cache.iter() {
            if *status == TransactionStatus::Rejected {
                invalid.push(tx_hash.clone());
            } else if let Ok(resolution) = self.tracker.get_resolution_status(tx_hash) {
                if resolution == DependencyResolutionStatus::Invalid {
                    invalid.push(tx_hash.clone());
                }
            }
        }

        Ok(invalid)
    }

    /// Perform bulk resolution on transaction set
    /// 
    /// Given a set of transactions (e.g., after ancestor becomes available),
    /// resolve all of them and return which ones changed status.
    pub fn bulk_resolve_transactions(&self, tx_hashes: &[TxHash]) -> StorageResult<BulkResolutionResult> {
        let mut newly_ready = Vec::new();
        let mut newly_invalid = Vec::new();
        let mut still_unresolved = Vec::new();

        for tx_hash in tx_hashes {
            let status = self.tracker.resolve_dependency_chain(tx_hash)?;

            match status {
                DependencyResolutionStatus::Ready => newly_ready.push(tx_hash.clone()),
                DependencyResolutionStatus::Invalid => newly_invalid.push(tx_hash.clone()),
                DependencyResolutionStatus::Unresolved => still_unresolved.push(tx_hash.clone()),
            }
        }

        Ok(BulkResolutionResult {
            newly_ready,
            newly_invalid,
            still_unresolved,
        })
    }

    /// Remove transaction completely from tracking
    /// 
    /// This is called when transaction is included in block or explicitly evicted.
    /// Also removes all references in ancestor/descendant relationships.
    pub fn remove_transaction(&self, tx_hash: &TxHash) -> StorageResult<()> {
        // Remove from tracker
        self.tracker.remove_transaction(tx_hash)?;

        // Optionally cascade invalidate if configured
        if self.config.read().auto_cascade_invalidate {
            // Descendants will be invalidated automatically in tracker
            let descendants = self.tracker.get_descendants(tx_hash).unwrap_or_default();
            for descendant in descendants {
                if let Ok(resolution) = self.tracker.get_resolution_status(&descendant) {
                    if resolution == DependencyResolutionStatus::Unresolved {
                        // Recalculate status after parent removal
                        let _ = self.tracker.resolve_dependency_chain(&descendant);
                    }
                }
            }
        }

        // Remove from status cache
        {
            let mut cache = self.status_cache.write();
            cache.remove(tx_hash);
        }

        Ok(())
    }

    /// Check if transaction A is ancestor of transaction B
    pub fn is_ancestor(&self, ancestor: &TxHash, tx: &TxHash) -> StorageResult<bool> {
        self.tracker.is_ancestor(ancestor, tx)
    }

    /// Get all ancestors for transaction
    pub fn get_ancestors(&self, tx_hash: &TxHash) -> StorageResult<HashSet<TxHash>> {
        self.tracker.get_ancestors(tx_hash)
    }

    /// Get all descendants for transaction
    pub fn get_descendants(&self, tx_hash: &TxHash) -> StorageResult<HashSet<TxHash>> {
        self.tracker.get_descendants(tx_hash)
    }

    /// Get immediate parents (direct dependencies only)
    pub fn get_immediate_parents(&self, tx_hash: &TxHash) -> StorageResult<HashSet<TxHash>> {
        self.tracker.get_immediate_parents(tx_hash)
    }

    /// Get immediate children (direct dependents only)
    pub fn get_immediate_children(&self, tx_hash: &TxHash) -> StorageResult<HashSet<TxHash>> {
        self.tracker.get_immediate_children(tx_hash)
    }

    /// Get current memory usage statistics
    pub fn get_memory_stats(&self) -> StorageResult<RecursiveDependencyStats> {
        Ok(self.tracker.get_stats())
    }

    /// Update transaction status in cache
    pub fn update_transaction_status(
        &self,
        tx_hash: &TxHash,
        status: TransactionStatus,
    ) -> StorageResult<()> {
        let mut cache = self.status_cache.write();
        cache.insert(tx_hash.clone(), status);
        Ok(())
    }

    /// Get transaction's current cached status
    pub fn get_transaction_status(&self, tx_hash: &TxHash) -> StorageResult<TransactionStatus> {
        let cache = self.status_cache.read();
        cache
            .get(tx_hash)
            .copied()
            .ok_or_else(|| StorageError::NotFound(
                format!("Transaction {:?} not in cache", tx_hash)
            ))
    }

    /// Check if transaction is in Ready state
    pub fn is_ready(&self, tx_hash: &TxHash) -> StorageResult<bool> {
        let status = self.tracker.get_resolution_status(tx_hash)?;
        Ok(status == DependencyResolutionStatus::Ready)
    }

    /// Check if transaction is in Invalid state
    pub fn is_invalid(&self, tx_hash: &TxHash) -> StorageResult<bool> {
        let status = self.tracker.get_resolution_status(tx_hash)?;
        Ok(status == DependencyResolutionStatus::Invalid)
    }

    /// Check if transaction is still Unresolved
    pub fn is_unresolved(&self, tx_hash: &TxHash) -> StorageResult<bool> {
        let status = self.tracker.get_resolution_status(tx_hash)?;
        Ok(status == DependencyResolutionStatus::Unresolved)
    }

    /// Reset all tracking data (careful: destructive operation)
    pub fn reset_all(&self) {
        self.tracker.reset();
        self.status_cache.write().clear();
    }

    /// Get current configuration
    pub fn get_config(&self) -> RecursiviveDependencyConfig {
        self.config.read().clone()
    }

    /// Update configuration
    pub fn set_config(&self, config: RecursiviveDependencyConfig) {
        *self.config.write() = config;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recursive_dependency_manager_creation() {
        // Placeholder for integration testing
        // Full tests require KvStore integration and comprehensive transaction setup
    }
}
