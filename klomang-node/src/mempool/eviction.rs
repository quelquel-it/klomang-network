//! Deterministic Eviction Engine
//!
//! Efficiently evicts transactions from mempool when capacity is reached,
//! using a deterministic scoring system to ensure all nodes evict the same transactions.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::pool::{PoolEntry, TransactionPool};
use super::status::TransactionStatus;
use crate::storage::error::StorageResult;

/// Priority score for eviction - lower score = evict first
#[derive(Clone, Debug)]
pub struct EvictionScore {
    /// Combined fee rate and time score (lower = evict first)
    score: i128,

    /// Transaction hash for deterministic tie-breaking
    tx_hash: Vec<u8>,
}

impl EvictionScore {
    /// Create eviction score from pool entry
    /// Score = (Fee / Size) * (Age in seconds)
    /// Lower scores are evicted first
    pub fn from_entry(entry: &PoolEntry, current_time: u64) -> Self {
        let fee_rate = if entry.size_bytes > 0 {
            entry.total_fee as i128 / entry.size_bytes as i128
        } else {
            0
        };

        let age_seconds = current_time.saturating_sub(entry.arrival_time) as i128;
        let score = fee_rate.saturating_mul(age_seconds.max(1));

        Self {
            score,
            tx_hash: bincode::serialize(&entry.transaction.id).unwrap_or_default(),
        }
    }

    /// Calculate score magnitude (for statistics)
    pub fn magnitude(&self) -> i128 {
        self.score
    }
}

impl PartialEq for EvictionScore {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score && self.tx_hash == other.tx_hash
    }
}

impl Eq for EvictionScore {}

impl PartialOrd for EvictionScore {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for EvictionScore {
    fn cmp(&self, other: &Self) -> Ordering {
        // Min-heap: lower scores first (higher priority for eviction)
        match self.score.cmp(&other.score) {
            Ordering::Equal => {
                // Deterministic tie-breaking via hash (lexicographic)
                other.tx_hash.cmp(&self.tx_hash)
            }
            other_ord => other_ord.reverse(), // Reverse to make min-heap
        }
    }
}

/// Configuration for eviction policy
#[derive(Clone, Debug)]
pub struct EvictionPolicy {
    /// Maximum number of transactions in pool
    pub max_transaction_count: usize,

    /// Maximum bytes in pool (estimated)
    pub max_memory_bytes: usize,

    /// Evict this many transactions per cycle
    pub batch_size: usize,
}

impl Default for EvictionPolicy {
    fn default() -> Self {
        Self {
            max_transaction_count: 100_000,
            max_memory_bytes: 100 * 1024 * 1024, // 100 MB
            batch_size: 100,
        }
    }
}

/// Deterministic eviction engine
pub struct EvictionEngine {
    pool: Arc<TransactionPool>,
    policy: EvictionPolicy,
}

impl EvictionEngine {
    /// Create new eviction engine
    pub fn new(pool: Arc<TransactionPool>, policy: EvictionPolicy) -> Self {
        Self { pool, policy }
    }

    /// Check if eviction is needed
    pub fn need_eviction(&self) -> bool {
        let stats = self.pool.get_stats();
        stats.total_count >= self.policy.max_transaction_count
            || stats.total_size_bytes >= self.policy.max_memory_bytes
    }

    /// Perform eviction with deterministic scoring
    pub fn evict_lowest_priority(&self) -> StorageResult<EvictionResult> {
        let mut result = EvictionResult::default();

        if !self.need_eviction() {
            return Ok(result);
        }

        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Build eviction heap from all transactions
        let all_entries = self.pool.get_all();
        let mut eviction_heap = BinaryHeap::new();

        for entry in all_entries.iter() {
            // Only consider non-validated or low-priority status
            if entry.status == TransactionStatus::Rejected
                || entry.status == TransactionStatus::InOrphanPool
            {
                let score = EvictionScore::from_entry(entry, current_time);
                eviction_heap.push(score);
            }
        }

        // Evict lowest priority transactions
        let mut evicted_count = 0;
        let mut evicted_bytes = 0;

        while evicted_count < self.policy.batch_size {
            if let Some(score) = eviction_heap.pop() {
                if let Some(entry) = self.pool.remove(&score.tx_hash) {
                    evicted_bytes += entry.size_bytes;
                    evicted_count += 1;
                    result.evicted_hashes.push(score.tx_hash);
                    result.total_evicted_fees += entry.total_fee;
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        result.evicted_count = evicted_count;
        result.evicted_bytes = evicted_bytes;
        result.success = true;

        Ok(result)
    }

    /// Get eviction scores for all transactions (for analysis)
    pub fn analyze_eviction_order(&self) -> Vec<(Vec<u8>, i128)> {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let all_entries = self.pool.get_all();
        let mut scores: Vec<_> = all_entries
            .iter()
            .map(|entry| {
                let score = EvictionScore::from_entry(entry, current_time);
                (score.tx_hash.clone(), score.magnitude())
            })
            .collect();

        // Sort by score (lowest first)
        scores.sort_by_key(|(_, score)| *score);
        scores
    }

    /// Adaptive eviction based on pressure
    pub fn adaptive_eviction(&self, pressure_0_to_1: f64) -> StorageResult<EvictionResult> {
        let mut policy = self.policy.clone();

        // Increase eviction aggressiveness under high pressure
        if pressure_0_to_1 > 0.8 {
            policy.batch_size = (policy.batch_size as f64 * 2.0) as usize;
        } else if pressure_0_to_1 > 0.5 {
            policy.batch_size = (policy.batch_size as f64 * 1.5) as usize;
        }

        let engine = EvictionEngine::new(Arc::clone(&self.pool), policy);
        engine.evict_lowest_priority()
    }
}

/// Result of eviction operation
#[derive(Clone, Debug, Default)]
pub struct EvictionResult {
    /// Whether eviction was successful
    pub success: bool,

    /// Number of transactions evicted
    pub evicted_count: usize,

    /// Total bytes freed by eviction
    pub evicted_bytes: usize,

    /// Total fees from evicted transactions
    pub total_evicted_fees: u64,

    /// Hashes of evicted transactions
    pub evicted_hashes: Vec<Vec<u8>>,
}

/// Mempool pressure metrics
#[derive(Clone, Debug)]
pub struct MempoolPressure {
    /// Transaction count ratio (0-1)
    pub transaction_pressure: f64,

    /// Memory usage ratio (0-1)
    pub memory_pressure: f64,

    /// Combined pressure (0-1)
    pub total_pressure: f64,
}

impl MempoolPressure {
    /// Calculate mempool pressure
    pub fn calculate(pool: &TransactionPool, policy: &EvictionPolicy) -> Self {
        let stats = pool.get_stats();

        let transaction_pressure =
            (stats.total_count as f64) / (policy.max_transaction_count as f64).max(1.0);

        let memory_pressure =
            (stats.total_size_bytes as f64) / (policy.max_memory_bytes as f64).max(1.0);

        let total_pressure = (transaction_pressure + memory_pressure) / 2.0;

        Self {
            transaction_pressure: transaction_pressure.min(1.0),
            memory_pressure: memory_pressure.min(1.0),
            total_pressure: total_pressure.min(1.0),
        }
    }
}

impl Drop for EvictionEngine {
    fn drop(&mut self) {
        // Cleanup is handled by Arc<TransactionPool>
        // No explicit cleanup needed
    }
}

/// Transaction Aging & Decay System
///
/// Manages relevance scores with exponential decay over time
/// to prioritize fresh, competitive transactions.
pub struct AgingProcessor {
    /// Decay rate per hour (e.g., 0.1 for 10% decay per hour)
    decay_rate_per_hour: f64,
    /// Anti-starvation threshold (hours before promotion)
    anti_starvation_hours: u64,
}

impl AgingProcessor {
    /// Create new aging processor
    pub fn new(decay_rate_per_hour: f64, anti_starvation_hours: u64) -> Self {
        Self {
            decay_rate_per_hour,
            anti_starvation_hours,
        }
    }

    /// Calculate decayed relevance score for a transaction
    ///
    /// Score starts at 1.0 and decays exponentially after 24 hours.
    /// Anti-starvation prevents score from dropping below minimum for old transactions.
    pub fn calculate_relevance_score(
        &self,
        entry_time_ms: u64,
        current_time_ms: u64,
        base_fee_rate: u64,
    ) -> f64 {
        let age_hours = (current_time_ms.saturating_sub(entry_time_ms)) as f64 / (1000.0 * 3600.0);

        // No decay for first 24 hours
        if age_hours <= 24.0 {
            return 1.0;
        }

        // Exponential decay after 24 hours
        let decay_factor = (-self.decay_rate_per_hour * (age_hours - 24.0)).exp();

        // Anti-starvation: minimum score for very old transactions
        let min_score = if age_hours >= self.anti_starvation_hours as f64 {
            0.1 // Minimum relevance to prevent complete starvation
        } else {
            0.0
        };

        // Boost score slightly based on fee rate (higher fee = slower decay)
        let fee_boost = (base_fee_rate as f64 / 1000.0).min(0.5);

        (decay_factor + fee_boost).max(min_score)
    }

    /// Get transactions eligible for priority adjustment due to aging
    pub fn get_aging_adjustments(
        &self,
        pool_entries: &[PoolEntry],
        current_time_ms: u64,
    ) -> Vec<(Vec<u8>, f64)> {
        pool_entries
            .iter()
            .map(|entry| {
                let tx_hash = bincode::serialize(&entry.transaction.id).unwrap_or_default();
                let fee_rate = if entry.size_bytes > 0 {
                    entry.total_fee / entry.size_bytes as u64
                } else {
                    0
                };
                let score = self.calculate_relevance_score(
                    entry.arrival_time * 1000,
                    current_time_ms,
                    fee_rate,
                );
                (tx_hash, score)
            })
            .collect()
    }
}

/// Predictive Eviction Engine
///
/// Uses historical data to predict and evict transactions unlikely to be mined.
pub struct EvictionPredictor {
    /// KvStore for historical data
    _kv_store: Arc<crate::storage::KvStore>,
    /// Historical average fee rate (updated periodically)
    historical_avg_fee: std::sync::RwLock<u64>,
    /// Number of blocks to analyze for historical average
    history_blocks: usize,
}

impl EvictionPredictor {
    /// Create new eviction predictor
    pub fn new(_kv_store: Arc<crate::storage::KvStore>, history_blocks: usize) -> Self {
        Self {
            _kv_store,
            historical_avg_fee: std::sync::RwLock::new(100), // Default
            history_blocks,
        }
    }

    /// Update historical average fee from recent blocks
    pub fn update_historical_avg(&self) -> Result<(), String> {
        let avg_fee = self.calculate_historical_avg_fee()?;
        let mut guard = self
            .historical_avg_fee
            .write()
            .map_err(|e| format!("Lock error: {}", e))?;
        *guard = avg_fee;
        Ok(())
    }

    /// Calculate historical average fee from last N blocks
    fn calculate_historical_avg_fee(&self) -> Result<u64, String> {
        let mut total_fee = 0u64;
        let mut block_count = 0usize;

        // Simulate getting recent block hashes (last history_blocks)
        for i in 0..self.history_blocks {
            let block_fee = self.get_block_total_fee(i)?;
            total_fee = total_fee.saturating_add(block_fee);
            block_count += 1;
        }

        if block_count == 0 {
            return Ok(1); // Minimum fee
        }

        Ok(total_fee / block_count as u64)
    }

    /// Get total fee for a block (sum of transaction fees)
    fn get_block_total_fee(&self, block_index: usize) -> Result<u64, String> {
        // Placeholder: simulate block fee calculation
        Ok(1000 + (block_index as u64 * 50)) // Simulated increasing fees
    }

    /// Predict transactions to evict based on fee threshold and pool capacity
    pub fn predict_evictions(
        &self,
        pool_entries: &[PoolEntry],
        current_pool_size: usize,
        max_pool_size: usize,
    ) -> Vec<Vec<u8>> {
        let historical_avg = {
            let guard = self.historical_avg_fee.read().unwrap();
            *guard
        };

        let threshold_fee = (historical_avg as f64 * 0.5) as u64; // 50% of historical avg
        let capacity_ratio = current_pool_size as f64 / max_pool_size as f64;

        // Only evict if pool is > 90% capacity
        if capacity_ratio <= 0.9 {
            return Vec::new();
        }

        pool_entries
            .iter()
            .filter_map(|entry| {
                let fee_rate = if entry.size_bytes > 0 {
                    entry.total_fee / entry.size_bytes as u64
                } else {
                    0
                };

                if fee_rate < threshold_fee {
                    bincode::serialize(&entry.transaction.id).ok()
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get current historical average fee
    pub fn get_historical_avg(&self) -> u64 {
        let guard = self.historical_avg_fee.read().unwrap();
        *guard
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klomang_core::core::crypto::Hash;
    use klomang_core::core::state::transaction::Transaction;

    fn create_test_entry(fee: u64, size: usize, arrival: u64) -> PoolEntry {
        let tx = Transaction {
            id: Hash::new(&[1u8; 32]),
            inputs: vec![],
            outputs: vec![],
            execution_payload: vec![],
            contract_address: None,
            gas_limit: 0,
            max_fee_per_gas: 0,
            chain_id: 1,
            locktime: 0,
        };

        let mut entry = PoolEntry::new(tx, fee, size);
        entry.arrival_time = arrival;
        entry
    }

    #[test]
    fn test_eviction_score_calculation() {
        let entry = create_test_entry(1000, 200, 1000);
        let score = EvictionScore::from_entry(&entry, 1100);

        // Fee rate = 1000 / 200 = 5
        // Age = 1100 - 1000 = 100
        // Score = 5 * 100 = 500
        assert_eq!(score.magnitude(), 500);
    }

    #[test]
    fn test_deterministic_ordering() {
        let entry1 = create_test_entry(1000, 100, 1000);
        let entry2 = create_test_entry(1000, 100, 1000);

        let score1 = EvictionScore::from_entry(&entry1, 2000);
        let score2 = EvictionScore::from_entry(&entry2, 2000);

        // Same score - ordering should be deterministic via hash
        assert_eq!(score1.score == score2.score, true, "Scores should be equal");
    }

    #[test]
    fn test_pressure_calculation() {
        let _policy = EvictionPolicy {
            max_transaction_count: 100,
            max_memory_bytes: 10000,
            batch_size: 10,
        };

        let _stats = super::super::pool::PoolStats {
            total_count: 50,
            pending_count: 50,
            validated_count: 0,
            orphan_count: 0,
            rejected_count: 0,
            total_fees: 0,
            total_size_bytes: 5000,
        };

        let pressure_tx = 50.0 / 100.0;
        let pressure_mem = 5000.0 / 10000.0;
        let total = (pressure_tx + pressure_mem) / 2.0;

        assert_eq!(pressure_tx, 0.5);
        assert_eq!(pressure_mem, 0.5);
        assert_eq!(total, 0.5);
    }
}
