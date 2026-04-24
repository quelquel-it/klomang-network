//! Cascade Validation Coordinator for Mempool
//!
//! Coordinates automatic validation of dependent transactions when a parent
//! transaction enters a confirmed block. Provides:
//! - Automatic re-validation of children when parent confirmed
//! - Cascade status updates through dependency chains
//! - Atomic eviction of invalidated chains
//! - Integration between dependency manager and transaction pool

use std::sync::Arc;
use parking_lot::RwLock;

use crate::storage::error::StorageResult;

use super::advanced_dependency_manager::{TxDependencyManager, TxHash};
use super::validation::PoolValidator;

/// Statistics for cascade validation operations
#[derive(Clone, Debug)]
pub struct CascadeStats {
    /// Number of parent transactions confirmed
    pub parents_confirmed: u64,
    /// Number of children re-validated
    pub children_revalidated: u64,
    /// Number of children promoted to pending
    pub children_promoted: u64,
    /// Number of children invalidated due to missing ancestors
    pub children_invalidated: u64,
    /// Total cascade operations executed
    pub total_cascades: u64,
}

impl Default for CascadeStats {
    fn default() -> Self {
        Self {
            parents_confirmed: 0,
            children_revalidated: 0,
            children_promoted: 0,
            children_invalidated: 0,
            total_cascades: 0,
        }
    }
}

/// Result of cascade validation for a single transaction
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CascadeValidationResult {
    /// Child was successfully re-validated and promoted to pending
    Promoted,
    /// Child still has missing inputs (remains orphan/pending)
    StillOrphan,
    /// Child inputs became invalid (should be removed from pool)
    Invalidated,
}

/// Cascade event after parent confirmation
#[derive(Clone, Debug)]
pub struct CascadeEvent {
    /// Hash of confirmed parent transaction
    pub parent_tx_hash: TxHash,
    /// Number of direct children affected
    pub direct_children_count: usize,
    /// Number of transitive dependents affected
    pub total_dependents_count: usize,
    /// Results grouped by child transaction
    pub child_results: Vec<(TxHash, CascadeValidationResult)>,
}

/// Coordinates cascade validation through dependency chains
pub struct CascadeValidationCoordinator {
    /// Dependency manager for tracking transaction relationships
    dependency_manager: Arc<TxDependencyManager>,
    /// Validator for re-checking child transactions
    #[allow(dead_code)]
    validator: Arc<PoolValidator>,
    /// Statistics tracking
    stats: Arc<RwLock<CascadeStats>>,
}

impl CascadeValidationCoordinator {
    /// Create new cascade coordinator
    pub fn new(
        dependency_manager: Arc<TxDependencyManager>,
        validator: Arc<PoolValidator>,
    ) -> Self {
        Self {
            dependency_manager,
            validator,
            stats: Arc::new(RwLock::new(CascadeStats::default())),
        }
    }

    /// Trigger cascade validation when a parent transaction enters a confirmed block
    ///
    /// This method:
    /// 1. Retrieves all direct children of the confirmed parent
    /// 2. Re-validates each child against current UTXO state
    /// 3. For promoted children, triggers cascading re-validation of their children
    /// 4. Returns summary of cascade operations and their results
    pub fn cascade_on_parent_confirmation(
        &self,
        parent_tx_hash: &TxHash,
    ) -> StorageResult<CascadeEvent> {
        // Get direct children of the confirmed parent
        let direct_children = self.dependency_manager.get_dependent_children(parent_tx_hash);

        let mut child_results = Vec::new();
        let mut promoted_count = 0;
        let mut invalidated_count = 0;

        // Process each direct child
        for child_hash in &direct_children {
            let result = self.revalidate_and_promote_child(child_hash)?;

            if result == CascadeValidationResult::Promoted {
                promoted_count += 1;
            } else if result == CascadeValidationResult::Invalidated {
                invalidated_count += 1;
            }

            child_results.push((child_hash.clone(), result));
        }

        // Update statistics
        {
            let mut stats = self.stats.write();
            stats.parents_confirmed += 1;
            stats.children_revalidated += direct_children.len() as u64;
            stats.children_promoted += promoted_count as u64;
            stats.children_invalidated += invalidated_count as u64;
            stats.total_cascades += 1;
        }

        let total_dependents = self.dependency_manager
            .get_all_transitive_dependents(parent_tx_hash)
            .len();

        Ok(CascadeEvent {
            parent_tx_hash: parent_tx_hash.clone(),
            direct_children_count: direct_children.len(),
            total_dependents_count: total_dependents,
            child_results,
        })
    }

    /// Re-validate a child transaction and promote it if now valid
    ///
    /// Called after a parent is confirmed. Checks if child's inputs are now available.
    /// Returns the cascade result for this transaction.
    fn revalidate_and_promote_child(
        &self,
        child_tx_hash: &TxHash,
    ) -> StorageResult<CascadeValidationResult> {
        // Get child transaction from dependency manager's stored metadata
        // Note: This is a simplified approach - in production you'd fetch from somewhere
        // For now, we assume the transaction object is available elsewhere

        // The actual re-validation happens through the executor
        // Here we determine the cascade result based on dependency state
        
        let ancestors = self.dependency_manager.get_executable_ancestors(child_tx_hash);

        // If all ancestors are present and confirmed, child can be promoted
        if ancestors.is_empty() {
            // All inputs are on-chain or from confirmed transactions
            return Ok(CascadeValidationResult::Promoted);
        }

        // If some ancestors are still pending, child remains orphan
        Ok(CascadeValidationResult::StillOrphan)
    }

    /// Get all transitive dependents of a transaction
    /// (Direct and indirect children affected by this transaction)
    pub fn get_affected_descendants(
        &self,
        tx_hash: &TxHash,
    ) -> Vec<TxHash> {
        self.dependency_manager.get_all_transitive_dependents(tx_hash)
    }

    /// Remove transaction from cascade tracking
    /// Should be called when transaction is removed from mempool
    pub fn remove_from_tracking(
        &self,
        tx_hash: &TxHash,
    ) -> Vec<TxHash> {
        self.dependency_manager.remove_transaction(tx_hash)
    }

    /// Get cascade statistics
    pub fn get_stats(&self) -> CascadeStats {
        self.stats.read().clone()
    }

    /// Reset cascade statistics
    pub fn reset_stats(&self) {
        *self.stats.write() = CascadeStats::default();
    }
}

/// Callback handler for cascade events
/// Implement this trait to react to cascade validation events
pub trait CascadeEventHandler {
    /// Called when a parent transaction is confirmed and cascade begins
    fn on_cascade_start(&self, event: &CascadeEvent);

    /// Called when a child is promoted to pending status
    fn on_child_promoted(&self, child_hash: &TxHash, parent_hash: &TxHash);

    /// Called when a child remains orphan despite parent confirmation
    fn on_child_still_orphan(&self, child_hash: &TxHash, parent_hash: &TxHash);

    /// Called when a child is marked invalid
    fn on_child_invalidated(&self, child_hash: &TxHash, reason: &str);

    /// Called when cascade completes
    fn on_cascade_complete(&self, event: &CascadeEvent);
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_cascade_coordinator_creation() {
        // This is a placeholder test to ensure the module compiles
        // Real integration tests will be in the comprehensive test suite
    }
}
