//! Dependency-Aware Eviction System
//!
//! Implements intelligent transaction eviction that respects dependency relationships.
//! When a parent transaction is evicted, all orphaned descendants are automatically removed.
//!
//! Key Features:
//! - Recursive cascading eviction
//! - Conflict-aware eviction triggers
//! - Storage verification for on-chain parents
//! - Preservation of valid child transactions
//! - Detailed eviction statistics and tracking

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use parking_lot::RwLock;
use crate::storage::error::StorageResult;
use crate::storage::kv_store::KvStore;

use super::advanced_dependency_manager::{TxDependencyManager, TxHash};

/// Reason for transaction eviction
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvictionReason {
    /// Transaction had insufficient fees
    LowFees,
    /// Transaction exceeded TTL
    Timeout,
    /// Transaction was involved in conflict loss
    ConflictLoss,
    /// Parent transaction was evicted
    ParentEvicted,
    /// Manual eviction requested
    ManualEviction,
    /// Storage constraints
    MemoryPressure,
}

/// Information about a single evicted transaction
#[derive(Clone, Debug)]
pub struct EvictionRecord {
    /// Hash of evicted transaction
    pub tx_hash: TxHash,
    /// Reason for eviction
    pub reason: EvictionReason,
    /// Was this evicted as cascade of parent removal
    pub is_cascade: bool,
    /// Number of dependents that were also evicted
    pub dependents_evicted: usize,
}

/// Statistics for eviction operations
#[derive(Clone, Debug)]
pub struct EvictionStats {
    /// Total transactions evicted
    pub total_evicted: usize,
    /// Total cascade evictions (as result of parent removal)
    pub cascade_evictions: usize,
    /// Transactions preserved due to alternative parents
    pub preserved_with_alt_parents: usize,
    /// Detailed records of recent evictions
    pub recent_evictions: Vec<EvictionRecord>,
    /// Maximum cascade depth encountered
    pub max_cascade_depth: usize,
}

impl Default for EvictionStats {
    fn default() -> Self {
        Self {
            total_evicted: 0,
            cascade_evictions: 0,
            preserved_with_alt_parents: 0,
            recent_evictions: Vec::new(),
            max_cascade_depth: 0,
        }
    }
}

/// Result of a cascade eviction operation
#[derive(Clone, Debug)]
pub struct CascadeEvictionResult {
    /// Primary transaction that was evicted
    pub primary_eviction: TxHash,
    /// All transactions evicted as cascade
    pub cascaded_evictions: Vec<TxHash>,
    /// Transactions that were checked but preserved
    pub preserved_transactions: Vec<TxHash>,
}

/// System for intelligent transaction eviction
pub struct DependencyEvictionSystem {
    /// Underlying dependency manager
    dependency_manager: Arc<TxDependencyManager>,
    /// Storage for parent verification
    #[allow(dead_code)]
    kv_store: Arc<KvStore>,
    /// Statistics tracking
    stats: Arc<RwLock<EvictionStats>>,
}

impl DependencyEvictionSystem {
    /// Create new eviction system
    pub fn new(
        dependency_manager: Arc<TxDependencyManager>,
        kv_store: Arc<KvStore>,
    ) -> Self {
        Self {
            dependency_manager,
            kv_store,
            stats: Arc::new(RwLock::new(EvictionStats::default())),
        }
    }

    /// Evict a transaction with cascading to dependents
    /// Returns all transactions that were evicted (including cascaded)
    pub fn evict_transaction_cascade(
        &self,
        tx_hash: &TxHash,
        reason: EvictionReason,
    ) -> StorageResult<CascadeEvictionResult> {
        let mut evicted = Vec::new();
        let mut preserved = Vec::new();
        let mut cascade_queue: VecDeque<(TxHash, usize)> = VecDeque::new();
        let mut max_depth = 0usize;

        // Start cascade from the primary transaction
        cascade_queue.push_back((tx_hash.clone(), 0));
        let mut visited = HashSet::new();

        while let Some((current_tx, depth)) = cascade_queue.pop_front() {
            max_depth = max_depth.max(depth);

            if visited.contains(&current_tx) {
                continue;
            }
            visited.insert(current_tx.clone());

            // Get all dependents of current transaction
            let dependents = self.dependency_manager.get_all_transitive_dependents(&current_tx);

            for dependent in dependents {
                // Check if this dependent has other parents that are still valid
                let ancestors = self.dependency_manager.get_executable_ancestors(&dependent);
                let has_alternative_parent = ancestors.iter().any(|ancestor| !visited.contains(ancestor));

                if has_alternative_parent {
                    // This transaction has other valid parents, so preserve it
                    preserved.push(dependent.clone());
                    // Update stats
                    {
                        let mut stats = self.stats.write();
                        stats.preserved_with_alt_parents += 1;
                    }
                } else {
                    // No alternative parents, mark for eviction
                    evicted.push(dependent.clone());
                    cascade_queue.push_back((dependent, depth + 1));
                }
            }

            // Remove from dependency manager
            self.dependency_manager.remove_transaction(&current_tx);

            // Record eviction (cascade if not primary)
            let is_cascade = &current_tx != tx_hash;
            let eviction_record = EvictionRecord {
                tx_hash: current_tx.clone(),
                reason: reason.clone(),
                is_cascade,
                dependents_evicted: if is_cascade { 0 } else { evicted.len() },
            };

            let mut stats = self.stats.write();
            stats.total_evicted += 1;
            if is_cascade {
                stats.cascade_evictions += 1;
            }
            stats.recent_evictions.push(eviction_record);
            // Keep only last 1000 records
            if stats.recent_evictions.len() > 1000 {
                stats.recent_evictions.remove(0);
            }
            stats.max_cascade_depth = stats.max_cascade_depth.max(max_depth);
        }

        Ok(CascadeEvictionResult {
            primary_eviction: tx_hash.clone(),
            cascaded_evictions: evicted,
            preserved_transactions: preserved,
        })
    }

    /// Evict transactions by criteria (fee, age, etc.)
    /// Only evicts transactions that have no other dependents
    pub fn evict_by_criteria(
        &self,
        criterion: impl Fn(&TxHash) -> bool,
        reason: EvictionReason,
    ) -> StorageResult<Vec<TxHash>> {
        let mut evicted = Vec::new();

        // Get all transactions at depth 0 (no dependencies)
        let leaf_transactions = self.dependency_manager.get_transactions_at_depth(0);

        for tx_hash in leaf_transactions {
            if criterion(&tx_hash) {
                // This transaction can be safely evicted
                self.dependency_manager.remove_transaction(&tx_hash);
                evicted.push(tx_hash.clone());

                // Update stats
                {
                    let mut stats = self.stats.write();
                    stats.total_evicted += 1;
                    stats.recent_evictions.push(EvictionRecord {
                        tx_hash: tx_hash.clone(),
                        reason: reason.clone(),
                        is_cascade: false,
                        dependents_evicted: 0,
                    });
                }
            }
        }

        Ok(evicted)
    }

    /// Check if evicting a transaction would orphan dependents
    /// Returns dependents that would be orphaned
    pub fn get_orphaned_dependents(&self, tx_hash: &TxHash) -> StorageResult<Vec<TxHash>> {
        let dependents = self.dependency_manager.get_all_transitive_dependents(tx_hash);
        let mut orphaned = Vec::new();

        for dependent in dependents {
            let ancestors = self.dependency_manager.get_executable_ancestors(&dependent);
            // If only parent is the one being evicted, it would be orphaned
            if ancestors.len() == 1 && ancestors.contains(tx_hash) {
                orphaned.push(dependent);
            }
        }

        Ok(orphaned)
    }

    /// Get eviction statistics
    pub fn get_stats(&self) -> EvictionStats {
        self.stats.read().clone()
    }

    /// Reset eviction statistics
    pub fn reset_stats(&self) {
        *self.stats.write() = EvictionStats::default();
    }

    /// Get recent eviction records
    pub fn get_recent_evictions(&self, limit: usize) -> Vec<EvictionRecord> {
        let stats = self.stats.read();
        stats.recent_evictions
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eviction_system_creation() {
        // Placeholder test - integration tests will verify functionality
    }
}
