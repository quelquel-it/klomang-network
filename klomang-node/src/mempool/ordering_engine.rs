//! Fee-Rate Weighted Ordering Engine
//!
//! This module implements the main transaction ordering engine that combines:
//! - Priority pool for efficient transaction selection
//! - Dependency constraint enforcement
//! - UTXO validation for double-spend prevention
//! - Fee-weighted aggregation for block building
//!
//! Key guarantees:
//! - Topological ordering (parents always before children)
//! - Deterministic output (same input → same ordering on all nodes)
//! - Efficient O(log n) per insertion
//! - No duplicate transactions in results

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;

use klomang_core::core::state::transaction::Transaction;

use crate::storage::kv_store::KvStore;
use crate::storage::error::{StorageResult, StorageError};
use super::recursive_dependency_tracker::TxHash;
use super::recursive_dependency_manager::RecursiveDependencyManager;
use super::priority_pool::{PriorityPool, PriorityPoolStats};

/// Configuration for ordering engine
#[derive(Clone, Debug)]
pub struct OrderingEngineConfig {
    /// Minimum fee rate (satoshis per byte) required
    pub min_fee_rate: u64,

    /// Maximum transactions per ordering operation
    pub max_transactions: usize,

    /// Enable topological ordering enforcement
    pub enforce_topological: bool,

    /// Enable UTXO validation before returning
    pub validate_utxo: bool,

    /// Starvation prevention: age threshold in seconds
    pub starvation_threshold_secs: u64,
}

impl Default for OrderingEngineConfig {
    fn default() -> Self {
        Self {
            min_fee_rate: 1, // Minimum 1 satoshi per byte
            max_transactions: 10000,
            enforce_topological: true,
            validate_utxo: true,
            starvation_threshold_secs: 300, // 5 minutes
        }
    }
}

/// Statistics for ordering operations
#[derive(Clone, Debug, Default)]
pub struct OrderingStats {
    /// Total ordering operations
    pub total_orderings: u64,
    /// Average ordering time (microseconds)
    pub avg_ordering_time_us: u64,
    /// Transactions rejected due to min fee
    pub rejected_low_fee: u64,
    /// Transactions rejected due to validation
    pub rejected_validation: u64,
    /// Transactions rejected due to missing parents
    pub rejected_missing_parents: u64,
    /// Average number of transactions per ordering
    pub avg_tx_per_order: u64,
}

/// Transaction entry with ordering metadata
#[derive(Clone, Debug)]
pub struct OrderedTransaction {
    /// The transaction
    pub transaction: Transaction,
    /// Calculated fee rate (satoshis per byte)
    pub fee_rate: u64,
    /// Size in bytes
    pub size_bytes: usize,
    /// Total fees
    pub total_fee: u64,
    /// Execution depth from roots
    pub depth: u32,
    /// Number of immediate children
    pub children_count: u32,
    /// Arrival time (UNIX timestamp)
    pub arrival_time: u64,
}

impl OrderedTransaction {
    /// Create from transaction components
    pub fn new(
        transaction: Transaction,
        total_fee: u64,
        size_bytes: usize,
        depth: u32,
        children_count: u32,
        arrival_time: u64,
    ) -> Self {
        let fee_rate = if size_bytes > 0 {
            total_fee / size_bytes as u64
        } else {
            0
        };

        Self {
            transaction,
            fee_rate,
            size_bytes,
            total_fee,
            depth,
            children_count,
            arrival_time,
        }
    }

    /// Get age in seconds
    pub fn age_secs(&self) -> u64 {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        current_time.saturating_sub(self.arrival_time)
    }
}

/// Main ordering engine for mempool transaction selection
pub struct OrderingEngine {
    /// Priority pool for efficient transaction ordering
    priority_pool: Arc<PriorityPool>,

    /// Dependency manager for constraint checking
    dependency_manager: Arc<RecursiveDependencyManager>,

    /// KvStore for UTXO validation
    kv_store: Arc<KvStore>,

    /// Transaction lookup: hash -> metadata
    tx_cache: Arc<RwLock<HashMap<TxHash, OrderedTransaction>>>,

    /// Configuration
    config: Arc<RwLock<OrderingEngineConfig>>,

    /// Statistics
    stats: Arc<RwLock<OrderingStats>>,
}

impl OrderingEngine {
    /// Create new ordering engine
    pub fn new(
        priority_pool: Arc<PriorityPool>,
        dependency_manager: Arc<RecursiveDependencyManager>,
        kv_store: Arc<KvStore>,
    ) -> Self {
        Self::with_config(priority_pool, dependency_manager, kv_store, OrderingEngineConfig::default())
    }

    /// Create with custom configuration
    pub fn with_config(
        priority_pool: Arc<PriorityPool>,
        dependency_manager: Arc<RecursiveDependencyManager>,
        kv_store: Arc<KvStore>,
        config: OrderingEngineConfig,
    ) -> Self {
        Self {
            priority_pool,
            dependency_manager,
            kv_store,
            tx_cache: Arc::new(RwLock::new(HashMap::new())),
            config: Arc::new(RwLock::new(config)),
            stats: Arc::new(RwLock::new(OrderingStats::default())),
        }
    }

    /// Register transaction for ordering
    pub fn register_transaction(
        &self,
        tx_hash: TxHash,
        tx: OrderedTransaction,
    ) -> StorageResult<()> {
        // Cache transaction
        {
            let mut cache = self.tx_cache.write();
            cache.insert(tx_hash.clone(), tx.clone());
        }

        // Add to priority pool
        self.priority_pool.insert(
            tx_hash,
            tx.fee_rate,
            tx.arrival_time,
            tx.depth,
        )?;

        Ok(())
    }

    /// Get ordered transactions for block building
    ///
    /// This is the main API for block builder. Returns transactions in optimal
    /// order respecting fees, age, dependencies, and UTXO validity.
    pub fn get_ordered_transactions(&self, limit: Option<usize>) -> StorageResult<Vec<OrderedTransaction>> {
        let start_time = std::time::Instant::now();
        let config = self.config.read();

        let tx_limit = limit.unwrap_or(config.max_transactions);
        let cache = self.tx_cache.read();

        // Get ordered hashes from priority pool
        let ordered_hashes = if config.enforce_topological {
            self.priority_pool.get_topological_order(tx_limit)?
        } else {
            self.priority_pool.get_ordered_transactions(tx_limit, config.min_fee_rate)?
        };

        let mut result = Vec::new();
        let mut rejected_low_fee = 0u64;
        let mut rejected_validation = 0u64;
        let mut rejected_missing_parents = 0u64;

        for tx_hash in ordered_hashes {
            if result.len() >= tx_limit {
                break;
            }

            // Check fee rate
            if let Some(ordered_tx) = cache.get(&tx_hash) {
                if ordered_tx.fee_rate < config.min_fee_rate {
                    rejected_low_fee += 1;
                    continue;
                }

                // Check dependencies (all parents must be in result)
                if let Ok(ancestors) = self.dependency_manager.get_ancestors(&tx_hash) {
                    let result_hashes: Vec<TxHash> = result
                        .iter()
                        .filter_map(|rt: &OrderedTransaction| {
                            bincode::serialize(&rt.transaction.id).ok()
                        })
                        .collect();

                    let all_ancestors_present = ancestors.is_empty() || ancestors.iter().all(|a| {
                        result_hashes.contains(a)
                    });

                    if !all_ancestors_present {
                        rejected_missing_parents += 1;
                        continue;
                    }
                }

                // Optionally validate UTXO on-chain
                if config.validate_utxo {
                    let mut is_valid = true;
                    for input in &ordered_tx.transaction.inputs {
                        let parent_hash = bincode::serialize(&input.prev_tx)
                            .map_err(|e| StorageError::SerializationError(e.to_string()))?;

                        // Check if parent is on-chain (UTXO exists)
                        let on_chain = self.kv_store.utxo_exists(&parent_hash, input.index)?;

                        // Check if parent is in mempool
                        let in_mempool = cache.contains_key(&parent_hash);

                        if !on_chain && !in_mempool {
                            is_valid = false;
                            break;
                        }
                    }

                    if !is_valid {
                        rejected_validation += 1;
                        continue;
                    }
                }

                result.push(ordered_tx.clone());
            }
        }

        let elapsed = start_time.elapsed().as_micros() as u64;

        // Update statistics
        {
            let mut stats = self.stats.write();
            stats.total_orderings += 1;
            stats.rejected_low_fee += rejected_low_fee;
            stats.rejected_validation += rejected_validation;
            stats.rejected_missing_parents += rejected_missing_parents;

            let new_avg = if stats.avg_tx_per_order == 0 {
                result.len() as u64
            } else {
                (stats.avg_tx_per_order + result.len() as u64) / 2
            };
            stats.avg_tx_per_order = new_avg;

            if stats.avg_ordering_time_us == 0 {
                stats.avg_ordering_time_us = elapsed;
            } else {
                stats.avg_ordering_time_us = (stats.avg_ordering_time_us + elapsed) / 2;
            }
        }

        Ok(result)
    }

    /// Get high-priority transactions only (fast path for block building)
    pub fn get_high_priority(&self, limit: usize) -> StorageResult<Vec<OrderedTransaction>> {
        let config = self.config.read();
        let cache = self.tx_cache.read();

        // Get from pool with strict fee requirement
        let ordered_hashes = self.priority_pool
            .get_ordered_transactions(limit, config.min_fee_rate)?;

        let mut result: Vec<OrderedTransaction> = Vec::new();
        for tx_hash in ordered_hashes {
            if result.len() >= limit {
                break;
            }
            if let Some(tx) = cache.get(&tx_hash) {
                result.push(tx.clone());
            }
        }

        Ok(result)
    }

    /// Remove transaction from ordering
    pub fn remove_transaction(&self, tx_hash: &TxHash) -> StorageResult<()> {
        self.tx_cache.write().remove(tx_hash);
        self.priority_pool.remove(tx_hash)?;
        Ok(())
    }

    /// Clear all transactions
    pub fn clear(&self) {
        self.tx_cache.write().clear();
        self.priority_pool.clear();
    }

    /// Get current size
    pub fn size(&self) -> usize {
        self.priority_pool.size()
    }

    /// Update transaction priority (e.g., on re-validation)
    pub fn update_transaction(
        &self,
        tx_hash: &TxHash,
        new_fee_rate: u64,
        new_arrival_time: u64,
        new_depth: u32,
    ) -> StorageResult<()> {
        self.priority_pool.update_priority(
            tx_hash,
            new_fee_rate,
            new_arrival_time,
            new_depth,
        )?;

        Ok(())
    }

    /// Get ordering statistics
    pub fn get_stats(&self) -> OrderingStats {
        self.stats.read().clone()
    }

    /// Reset statistics
    pub fn reset_stats(&self) {
        *self.stats.write() = OrderingStats::default();
    }

    /// Get configuration
    pub fn get_config(&self) -> OrderingEngineConfig {
        self.config.read().clone()
    }

    /// Update configuration
    pub fn set_config(&self, config: OrderingEngineConfig) {
        *self.config.write() = config;
    }

    /// Get pool statistics
    pub fn get_pool_stats(&self) -> PriorityPoolStats {
        self.priority_pool.get_stats()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ordering_engine_creation() {
        // Placeholder for integration testing
        // Requires full setup with dependency manager, pool, and KvStore
    }
}
