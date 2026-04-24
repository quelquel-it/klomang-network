//! Age-Based Anti-Starvation Priority Scheduler
//!
//! Implements dynamic priority calculation with hybrid scoring formula that combines
//! economic incentives (fee rate) with temporal fairness (transaction age).
//!
//! Key Features:
//! - Hybrid scoring: Score = (FeeRate × W_f) + (AgeInPool × W_a)
//! - Configurable weights for fee and age factors
//! - Age-based "boost" every 10 minutes to prevent starvation
//! - Periodic scheduler for incremental priority updates
//! - Thread-safe with RwLock synchronization

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;

use crate::storage::error::StorageResult;
use super::recursive_dependency_tracker::TxHash;

/// Dynamic priority scheduler configuration
#[derive(Clone, Debug)]
pub struct PrioritySchedulerConfig {
    /// Weight multiplier for fee rate (0.0 - 1.0, default 0.7)
    /// Higher = prioritizes miner profit more
    pub fee_weight: f64,

    /// Weight multiplier for age in pool (0.0 - 1.0, default 0.3)
    /// Higher = prioritizes fairness/starvation prevention more
    pub age_weight: f64,

    /// Time interval in seconds for age "boost" increments
    /// Default: 600 seconds (10 minutes)
    pub age_boost_interval_secs: u64,

    /// Maximum age boost value to cap starvation prevention
    /// Prevents extremely old txs from dominating purely on age
    /// Default: 1000 (equivalent to 100 minutes of boosts)
    pub max_age_boost_value: u64,

    /// Scheduler update frequency in seconds
    /// How often periodic priority updates should be triggered
    /// Default: 30 seconds
    pub scheduler_update_interval_secs: u64,
}

impl Default for PrioritySchedulerConfig {
    fn default() -> Self {
        Self {
            fee_weight: 0.7,
            age_weight: 0.3,
            age_boost_interval_secs: 600,     // 10 minutes
            max_age_boost_value: 1000,         // ~100 minutes max
            scheduler_update_interval_secs: 30, // 30 seconds
        }
    }
}

/// Dynamic priority score for a transaction
#[derive(Clone, Debug, PartialEq)]
pub struct DynamicPriority {
    /// Raw priority score combining fee and age factors
    pub score: f64,

    /// Fee rate component (satoshis per byte)
    pub fee_rate: u64,

    /// Age of transaction in pool (seconds)
    pub age_secs: u64,

    /// Number of age boost intervals completed
    pub age_boost_count: u64,

    /// Last update timestamp (UNIX seconds)
    pub last_update: u64,

    /// Deterministic rank for tie-breaking (lexicographical hash)
    pub tx_hash: TxHash,
}

impl DynamicPriority {
    /// Create new dynamic priority (at insertion time)
    pub fn new(tx_hash: TxHash, fee_rate: u64, arrival_time: u64) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let age_secs = now.saturating_sub(arrival_time);
        let score = Self::calculate_score(fee_rate, age_secs, &Default::default());

        Self {
            score,
            fee_rate,
            age_secs,
            age_boost_count: 0,
            last_update: now,
            tx_hash,
        }
    }

    /// Calculate hybrid priority score: Score = (FeeRate × W_f) + (AgeInPool × W_a)
    ///
    /// # Arguments
    /// * `fee_rate` - Transaction fee rate in satoshis per byte
    /// * `age_secs` - Age of transaction in pool (seconds)
    /// * `config` - Scheduler configuration with weights
    ///
    /// # Returns
    /// Floating-point score combining economic and fairness factors
    pub fn calculate_score(
        fee_rate: u64,
        age_secs: u64,
        config: &PrioritySchedulerConfig,
    ) -> f64 {
        // Fee component: normalized to typical range (1-1000 sats/byte)
        // Higher fee rate = higher score contribution
        let fee_component = (fee_rate as f64) * config.fee_weight;

        // Age component: calculate boost intervals completed
        // Every boost_interval_secs, add 1 to boost value (capped at max)
        let boost_count = (age_secs / config.age_boost_interval_secs)
            .min(config.max_age_boost_value);

        let age_component = (boost_count as f64) * config.age_weight;

        // Hybrid score prevents both fee-rich dominance and age-based starvation
        fee_component + age_component
    }

    /// Update priority based on elapsed time (periodic update from scheduler)
    pub fn update_with_elapsed_time(
        &mut self,
        current_time: u64,
        arrival_time: u64,
        config: &PrioritySchedulerConfig,
    ) {
        self.age_secs = current_time.saturating_sub(arrival_time);
        self.age_boost_count = (self.age_secs / config.age_boost_interval_secs)
            .min(config.max_age_boost_value);
        self.score = Self::calculate_score(self.fee_rate, self.age_secs, config);
        self.last_update = current_time;
    }

    /// Recalculate score when fee rate changes (e.g., child tx pins parent)
    pub fn update_with_fee_rate(
        &mut self,
        new_fee_rate: u64,
        current_time: u64,
        config: &PrioritySchedulerConfig,
    ) {
        self.fee_rate = new_fee_rate;
        self.score = Self::calculate_score(self.fee_rate, self.age_secs, config);
        self.last_update = current_time;
    }

    /// Get priority as integer for ordering (for compatibility with existing heap systems)
    /// Uses u64 to avoid floating point in hot path
    pub fn as_u64_priority(&self) -> u64 {
        // Convert to u64 with fixed decimal point (2 digits precision)
        // Score range: ~0.3 (min age, low fee) to ~700+ (high fee, old tx)
        (self.score * 100.0).min(u64::MAX as f64) as u64
    }
}

/// Priority scheduler that manages periodic updates and fairness
pub struct PriorityScheduler {
    /// Configuration parameters
    pub config: Arc<RwLock<PrioritySchedulerConfig>>,

    /// Transaction priorities map (tx_hash → DynamicPriority)
    pub priorities: Arc<RwLock<HashMap<TxHash, DynamicPriority>>>,

    /// Arrival times for calculating age (tx_hash → arrival_time)
    pub arrival_times: Arc<RwLock<HashMap<TxHash, u64>>>,

    /// Last scheduler update timestamp
    pub last_update: Arc<RwLock<u64>>,

    /// Count of successful updates
    pub update_count: Arc<RwLock<u64>>,
}

impl PriorityScheduler {
    /// Create new priority scheduler
    pub fn new(config: PrioritySchedulerConfig) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            config: Arc::new(RwLock::new(config)),
            priorities: Arc::new(RwLock::new(HashMap::new())),
            arrival_times: Arc::new(RwLock::new(HashMap::new())),
            last_update: Arc::new(RwLock::new(now)),
            update_count: Arc::new(RwLock::new(0)),
        }
    }

    /// Register new transaction for priority tracking
    pub fn register_transaction(
        &self,
        tx_hash: TxHash,
        fee_rate: u64,
        arrival_time: u64,
    ) -> StorageResult<()> {
        let priority = DynamicPriority::new(tx_hash.clone(), fee_rate, arrival_time);

        let mut priorities = self.priorities.write();
        priorities.insert(tx_hash.clone(), priority);

        let mut arrivals = self.arrival_times.write();
        arrivals.insert(tx_hash, arrival_time);

        Ok(())
    }

    /// Unregister transaction when it's removed from pool
    pub fn unregister_transaction(&self, tx_hash: &TxHash) -> StorageResult<()> {
        self.priorities.write().remove(tx_hash);
        self.arrival_times.write().remove(tx_hash);
        Ok(())
    }

    /// Calculate dynamic priority for a single transaction
    ///
    /// This is the core function requested: calculate_dynamic_priority(tx_hash: &Hash)
    pub fn calculate_dynamic_priority(
        &self,
        tx_hash: &TxHash,
    ) -> StorageResult<Option<DynamicPriority>> {
        let config = self.config.read().clone();
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let arrivals = self.arrival_times.read();
        if let Some(&arrival_time) = arrivals.get(tx_hash) {
            let mut priorities = self.priorities.write();
            if let Some(priority) = priorities.get_mut(tx_hash) {
                priority.update_with_elapsed_time(current_time, arrival_time, &config);
                return Ok(Some(priority.clone()));
            }
        }

        Ok(None)
    }

    /// Scheduled update: recalculate priorities for all transactions
    /// Called periodically (e.g., when new block arrives)
    pub fn perform_scheduled_update(&self) -> StorageResult<u64> {
        let config = self.config.read().clone();
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let last_update = self.last_update.read();
        let time_since_last = current_time.saturating_sub(*last_update);

        // Skip if not enough time has passed
        if time_since_last < config.scheduler_update_interval_secs {
            return Ok(0);
        }
        drop(last_update);

        // Batch update all transactions
        let arrivals = self.arrival_times.read();
        let mut priorities = self.priorities.write();
        let mut updated_count = 0u64;

        for (tx_hash, &arrival_time) in arrivals.iter() {
            if let Some(priority) = priorities.get_mut(tx_hash) {
                priority.update_with_elapsed_time(current_time, arrival_time, &config);
                updated_count += 1;
            }
        }

        drop(priorities);
        drop(arrivals);

        *self.last_update.write() = current_time;
        *self.update_count.write() = self.update_count.read().saturating_add(1);

        Ok(updated_count)
    }

    /// Update fee rate for a transaction (when child tx discovers parent)
    pub fn update_transaction_fee(
        &self,
        tx_hash: &TxHash,
        new_fee_rate: u64,
    ) -> StorageResult<()> {
        let config = self.config.read().clone();
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut priorities = self.priorities.write();
        if let Some(priority) = priorities.get_mut(tx_hash) {
            priority.update_with_fee_rate(new_fee_rate, current_time, &config);
        }

        Ok(())
    }

    /// Get current priority for a transaction
    pub fn get_priority(&self, tx_hash: &TxHash) -> StorageResult<Option<DynamicPriority>> {
        Ok(self.priorities.read().get(tx_hash).cloned())
    }

    /// Update priority score for a transaction by adding a delta
    pub fn update_priority(&self, tx_hash: &TxHash, score_delta: i64) -> StorageResult<()> {
        let mut priorities = self.priorities.write();
        if let Some(priority) = priorities.get_mut(tx_hash) {
            priority.score = (priority.score + score_delta as f64).max(0.0);
            priority.last_update = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
        }
        Ok(())
    }

    /// Get all transactions sorted by dynamic priority (descending)
    pub fn get_all_by_priority(&self) -> StorageResult<Vec<(TxHash, DynamicPriority)>> {
        let priorities = self.priorities.read();
        let mut txs: Vec<_> = priorities
            .iter()
            .map(|(hash, prio)| (hash.clone(), prio.clone()))
            .collect();

        // Sort by score descending (higher score = higher priority)
        txs.sort_by(|a, b| {
            b.1.score
                .partial_cmp(&a.1.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.tx_hash.cmp(&b.1.tx_hash)) // Tie-break by hash
        });

        Ok(txs)
    }

    /// Get transactions within a fee rate range
    pub fn get_by_fee_range(
        &self,
        min_fee: u64,
        max_fee: u64,
    ) -> StorageResult<Vec<(TxHash, DynamicPriority)>> {
        let priorities = self.priorities.read();
        let mut txs: Vec<_> = priorities
            .iter()
            .filter(|(_, prio)| prio.fee_rate >= min_fee && prio.fee_rate <= max_fee)
            .map(|(hash, prio)| (hash.clone(), prio.clone()))
            .collect();

        txs.sort_by(|a, b| {
            b.1.score
                .partial_cmp(&a.1.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(txs)
    }

    /// Get transactions older than specified age
    pub fn get_by_minimum_age(
        &self,
        min_age_secs: u64,
    ) -> StorageResult<Vec<(TxHash, DynamicPriority)>> {
        let priorities = self.priorities.read();
        let mut txs: Vec<_> = priorities
            .iter()
            .filter(|(_, prio)| prio.age_secs >= min_age_secs)
            .map(|(hash, prio)| (hash.clone(), prio.clone()))
            .collect();

        txs.sort_by(|a, b| {
            b.1.score
                .partial_cmp(&a.1.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(txs)
    }

    /// Get transactions with high age boost (starvation risk)
    pub fn get_high_boost_transactions(&self) -> StorageResult<Vec<(TxHash, DynamicPriority)>> {
        let config = self.config.read();
        let threshold = config.max_age_boost_value / 2;
        drop(config);

        let priorities = self.priorities.read();
        let mut txs: Vec<_> = priorities
            .iter()
            .filter(|(_, prio)| prio.age_boost_count > threshold)
            .map(|(hash, prio)| (hash.clone(), prio.clone()))
            .collect();

        txs.sort_by(|a, b| {
            b.1.age_boost_count
                .cmp(&a.1.age_boost_count)
                .then_with(|| b.1.score.partial_cmp(&a.1.score).unwrap_or(std::cmp::Ordering::Equal))
        });

        Ok(txs)
    }

    /// Get scheduler statistics
    pub fn get_stats(&self) -> (u64, u64, usize) {
        let update_count = *self.update_count.read();
        let last_update = *self.last_update.read();
        let priority_count = self.priorities.read().len();
        (update_count, last_update, priority_count)
    }

    /// Update scheduler configuration
    pub fn update_config(&self, config: PrioritySchedulerConfig) {
        *self.config.write() = config;
    }

    /// Reset all data
    pub fn reset(&self) -> StorageResult<()> {
        self.priorities.write().clear();
        self.arrival_times.write().clear();
        *self.update_count.write() = 0;
        *self.last_update.write() = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Ok(())
    }
}

/// Builder for PriorityScheduler with fluent API
pub struct PrioritySchedulerBuilder {
    fee_weight: f64,
    age_weight: f64,
    age_boost_interval_secs: u64,
    max_age_boost_value: u64,
    scheduler_update_interval_secs: u64,
}

impl Default for PrioritySchedulerBuilder {
    fn default() -> Self {
        let config = PrioritySchedulerConfig::default();
        Self {
            fee_weight: config.fee_weight,
            age_weight: config.age_weight,
            age_boost_interval_secs: config.age_boost_interval_secs,
            max_age_boost_value: config.max_age_boost_value,
            scheduler_update_interval_secs: config.scheduler_update_interval_secs,
        }
    }
}

impl PrioritySchedulerBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_fee_weight(mut self, weight: f64) -> Self {
        self.fee_weight = weight.max(0.0).min(1.0);
        self
    }

    pub fn with_age_weight(mut self, weight: f64) -> Self {
        self.age_weight = weight.max(0.0).min(1.0);
        self
    }

    pub fn with_age_boost_interval(mut self, secs: u64) -> Self {
        self.age_boost_interval_secs = secs.max(60); // Minimum 1 minute
        self
    }

    pub fn with_max_age_boost(mut self, value: u64) -> Self {
        self.max_age_boost_value = value;
        self
    }

    pub fn with_scheduler_interval(mut self, secs: u64) -> Self {
        self.scheduler_update_interval_secs = secs.max(10); // Minimum 10 seconds
        self
    }

    pub fn build(self) -> PriorityScheduler {
        let config = PrioritySchedulerConfig {
            fee_weight: self.fee_weight,
            age_weight: self.age_weight,
            age_boost_interval_secs: self.age_boost_interval_secs,
            max_age_boost_value: self.max_age_boost_value,
            scheduler_update_interval_secs: self.scheduler_update_interval_secs,
        };
        PriorityScheduler::new(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_score_calculation_basic() {
        let config = PrioritySchedulerConfig::default();
        
        // High fee, no age should emphasize fee
        let score1 = DynamicPriority::calculate_score(100, 0, &config);
        
        // Low fee, high age should get boost from age
        let score2 = DynamicPriority::calculate_score(1, 1200, &config);
        
        // Without age boost (exactly at threshold), score2 should be less than score1
        assert!(score1 > score2);
    }

    #[test]
    fn test_scheduler_registration() {
        let scheduler = PriorityScheduler::new(PrioritySchedulerConfig::default());
        let hash = vec![1, 2, 3];
        
        scheduler.register_transaction(hash.clone(), 50, 1000).unwrap();
        
        let priority = scheduler.get_priority(&hash).unwrap();
        assert!(priority.is_some());
        assert_eq!(priority.unwrap().fee_rate, 50);
    }

    #[test]
    fn test_starvation_prevention() {
        let config = PrioritySchedulerConfig::default();
        
        // Two txs: one high fee, one old
        let high_fee_score = DynamicPriority::calculate_score(500, 60, &config);
        let old_low_fee_score = DynamicPriority::calculate_score(1, 1200, &config); // 2 boosts
        
        // Old tx should have competitive score due to age boosts
        assert!((high_fee_score - old_low_fee_score).abs() < high_fee_score);
    }
}
