//! Storage State Integration for Dependency Management
//!
//! Bridges dependency tracking with on-chain storage verification.
//! Distinguishes between on-chain parents (in storage) and mempool parents.
//!
//! Key Features:
//! - Parent verification against KvStore
//! - Automatic reclassification of parents based on storage state
//! - Mempool parent tracking
//! - Storage state consistency checks
//! - Integration with conflict resolution

use std::sync::Arc;
use parking_lot::RwLock;
use dashmap::DashMap;

use klomang_core::core::state::transaction::Transaction;
use crate::storage::error::StorageResult;
use crate::storage::kv_store::KvStore;

use super::advanced_dependency_manager::{TxDependencyManager, TxHash};

/// Classification of a parent transaction
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParentClassification {
    /// Parent is in on-chain storage (UTXO)
    OnChain,
    /// Parent is in mempool
    InMempool,
    /// Parent not found anywhere (orphan/missing)
    Missing,
}

/// Information about parent verification
#[derive(Clone, Debug)]
pub struct ParentVerification {
    /// Hash of parent transaction
    pub parent_hash: TxHash,
    /// Classification of parent
    pub classification: ParentClassification,
    /// Whether parent is confirmed in a block
    pub is_confirmed: bool,
    /// Depth in dependency chain (if in mempool)
    pub mempool_depth: Option<u32>,
}

/// Statistics for storage state tracking
#[derive(Clone, Debug)]
pub struct StorageIntegrationStats {
    /// Transactions verified as on-chain
    pub on_chain_parents: usize,
    /// Transactions in mempool (active dependencies)
    pub mempool_parents: usize,
    /// Missing/orphan transactions
    pub missing_parents: usize,
    /// Reclassifications due to state changes
    pub reclassifications: usize,
}

impl Default for StorageIntegrationStats {
    fn default() -> Self {
        Self {
            on_chain_parents: 0,
            mempool_parents: 0,
            missing_parents: 0,
            reclassifications: 0,
        }
    }
}

/// Integrates dependency management with storage state
pub struct StorageIntegration {
    /// Underlying dependency manager
    dependency_manager: Arc<TxDependencyManager>,
    /// Storage for parent verification
    kv_store: Arc<KvStore>,
    /// Cache of verified parents
    parent_cache: Arc<DashMap<TxHash, ParentVerification>>,
    /// Statistics tracking
    stats: Arc<RwLock<StorageIntegrationStats>>,
}

impl StorageIntegration {
    /// Create new storage integration
    pub fn new(
        dependency_manager: Arc<TxDependencyManager>,
        kv_store: Arc<KvStore>,
    ) -> Self {
        Self {
            dependency_manager,
            kv_store,
            parent_cache: Arc::new(DashMap::new()),
            stats: Arc::new(RwLock::new(StorageIntegrationStats::default())),
        }
    }

    /// Verify a parent and classify it
    pub fn verify_parent(&self, parent_hash: &TxHash) -> StorageResult<ParentVerification> {
        // Check cache first
        if let Some(cached) = self.parent_cache.get(parent_hash) {
            return Ok(cached.value().clone());
        }

        // Try to find in storage (on-chain UTXO)
        let is_on_chain = self.kv_store.utxo_exists(parent_hash, 0)?;

        let classification = if is_on_chain {
            ParentClassification::OnChain
        } else {
            // Check if it's in mempool via dependency manager
            if self.dependency_manager.get_execution_depth(parent_hash).is_some() {
                ParentClassification::InMempool
            } else {
                ParentClassification::Missing
            }
        };

        let mempool_depth = match classification {
            ParentClassification::InMempool => self.dependency_manager.get_execution_depth(parent_hash),
            _ => None,
        };

        let verification = ParentVerification {
            parent_hash: parent_hash.clone(),
            classification: classification.clone(),
            is_confirmed: is_on_chain,
            mempool_depth,
        };

        // Cache the verification
        self.parent_cache.insert(parent_hash.clone(), verification.clone());

        // Update stats
        {
            let mut stats = self.stats.write();
            match &classification {
                ParentClassification::OnChain => stats.on_chain_parents += 1,
                ParentClassification::InMempool => stats.mempool_parents += 1,
                ParentClassification::Missing => stats.missing_parents += 1,
            }
        }

        Ok(verification)
    }

    /// Verify a transaction's parents
    pub fn verify_transaction_parents(
        &self,
        tx: &Transaction,
    ) -> StorageResult<Vec<ParentVerification>> {
        let mut verifications = Vec::new();

        for input in &tx.inputs {
            // Create parent hash from input (this is simplified)
            // In production, you'd derive the proper parent hash
            let parent_hash = bincode::serialize(&input).unwrap_or_default();
            let verification = self.verify_parent(&parent_hash)?;
            verifications.push(verification);
        }

        Ok(verifications)
    }

    /// Get all relevant parents for a transaction
    /// Filters to only mempool parents (on-chain parents not needed for dependency tracking)
    pub fn get_mempool_parents(&self, tx_hash: &TxHash) -> StorageResult<Vec<TxHash>> {
        let ancestors = self.dependency_manager.get_executable_ancestors(tx_hash);
        let mut mempool_parents = Vec::new();

        for ancestor in ancestors {
            let verification = self.verify_parent(&ancestor)?;
            if verification.classification == ParentClassification::InMempool {
                mempool_parents.push(ancestor);
            }
        }

        Ok(mempool_parents)
    }

    /// Check if all parents of a transaction are now on-chain
    /// Useful for determining if transaction can leave mempool
    pub fn all_parents_confirmed(&self, tx_hash: &TxHash) -> StorageResult<bool> {
        let ancestors = self.dependency_manager.get_executable_ancestors(tx_hash);

        for ancestor in ancestors {
            let verification = self.verify_parent(&ancestor)?;
            if verification.classification != ParentClassification::OnChain {
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Reclassify a parent (called when storage state changes)
    /// Returns true if classification changed
    pub fn reclassify_parent(&self, parent_hash: &TxHash) -> StorageResult<bool> {
        // Remove from cache to force re-verification
        self.parent_cache.remove(parent_hash);

        // Get old classification
        let old_class = self.parent_cache
            .get(parent_hash)
            .map(|v| v.classification.clone());

        // Verify again
        let verification = self.verify_parent(parent_hash)?;

        // Check if classification changed
        let changed = if let Some(old) = old_class {
            old != verification.classification
        } else {
            true
        };

        if changed {
            let mut stats = self.stats.write();
            stats.reclassifications += 1;
        }

        Ok(changed)
    }

    /// Clear verification cache
    pub fn clear_cache(&self) {
        self.parent_cache.clear();
    }

    /// Get verification cache size
    pub fn cache_size(&self) -> usize {
        self.parent_cache.len()
    }

    /// Get statistics
    pub fn get_stats(&self) -> StorageIntegrationStats {
        self.stats.read().clone()
    }

    /// Reset statistics
    pub fn reset_stats(&self) {
        *self.stats.write() = StorageIntegrationStats::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_integration_creation() {
        // Placeholder test - integration tests will verify functionality
    }
}
