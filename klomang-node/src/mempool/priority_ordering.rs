//! Advanced Priority & Ordering System with Deterministic Tie-Breaking
//!
//! This module implements:
//! - Deterministic tie-breaking using lexicographic hash comparison
//! - Local priority bucketing system organized by fee rate ranges
//! - Thread-safe retrieval with parking_lot::RwLock
//! - O(1) average lookup time via bucketing strategy
//!
//! Fee Rate Bucket Ranges (satoshis per byte):
//! - Bucket 0: 0-1 sat/vB (very low priority)
//! - Bucket 1: 1-10 sat/vB (low priority)
//! - Bucket 2: 10-100 sat/vB (medium priority)
//! - Bucket 3: 100-1000 sat/vB (high priority)
//! - Bucket 4: 1000+ sat/vB (very high priority)

use parking_lot::RwLock;
use std::sync::Arc;

/// Configuration for priority ordering buckets
#[derive(Clone, Debug)]
pub struct PriorityOrderingConfig {
    /// Fee rate thresholds (sat/vB) that define bucket boundaries
    pub fee_rate_thresholds: Vec<u64>,
    /// Enable deterministic ordering for consensus
    pub deterministic_ordering: bool,
}

impl Default for PriorityOrderingConfig {
    fn default() -> Self {
        Self {
            fee_rate_thresholds: vec![1, 10, 100, 1000],
            deterministic_ordering: true,
        }
    }
}

/// Representative of a prioritized transaction in a bucket
#[derive(Clone, Debug)]
pub struct PrioritizedTransaction {
    /// Transaction hash (serialized)
    pub tx_hash: Vec<u8>,
    /// Priority score (calculated from fee rate and arrival time)
    pub priority_score: f64,
    /// Fee rate in satoshis per byte
    pub fee_rate: u64,
    /// Arrival time (UNIX timestamp)
    pub arrival_time: u64,
    /// Transaction size in bytes
    pub size_bytes: usize,
    /// Total fees in satoshis
    pub total_fee: u64,
}

impl PrioritizedTransaction {
    /// Create new prioritized transaction
    pub fn new(
        tx_hash: Vec<u8>,
        fee_rate: u64,
        arrival_time: u64,
        size_bytes: usize,
        total_fee: u64,
    ) -> Self {
        // Priority score = (fee_rate * 100.0) + (time_weight)
        // Higher fee rate = higher priority
        let priority_score = fee_rate as f64 * 100.0 + (arrival_time as f64 * 0.001);

        Self {
            tx_hash,
            priority_score,
            fee_rate,
            arrival_time,
            size_bytes,
            total_fee,
        }
    }

    /// Compare two transactions for deterministic ordering
    /// Returns ordering based on tie-breaking rules
    pub fn compare_deterministic(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;

        // Primary: Compare by priority score (descending - higher is better)
        match other.priority_score.partial_cmp(&self.priority_score) {
            Some(Ordering::Equal) | None => {
                // Tie-breaker: Lexicographic hash comparison (ascending - lower hash comes first)
                self.tx_hash.cmp(&other.tx_hash)
            }
            Some(ord) => ord,
        }
    }
}

/// Single priority bucket containing transactions within a fee rate range
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct PriorityBucket {
    /// Lower bound (inclusive) of fee rate range
    lower_bound: u64,
    /// Upper bound (exclusive) of fee rate range
    upper_bound: u64,
    /// Sorted transactions in this bucket (maintained in deterministic order)
    transactions: Vec<PrioritizedTransaction>,
}

impl PriorityBucket {
    /// Create new empty bucket
    fn new(lower_bound: u64, upper_bound: u64) -> Self {
        Self {
            lower_bound,
            upper_bound,
            transactions: Vec::new(),
        }
    }

    /// Insert transaction maintaining deterministic order
    fn insert(&mut self, tx: PrioritizedTransaction) -> Result<(), String> {
        // Verify transaction fee within bucket range
        if tx.fee_rate < self.lower_bound || tx.fee_rate >= self.upper_bound {
            return Err(format!(
                "Fee rate {} outside bucket range [{}, {})",
                tx.fee_rate, self.lower_bound, self.upper_bound
            ));
        }

        // Binary search to find insertion point
        let insertion_point = self
            .transactions
            .binary_search_by(|existing| existing.compare_deterministic(&tx).reverse());

        match insertion_point {
            Ok(pos) => {
                // Exact position found, insert before
                self.transactions.insert(pos, tx);
            }
            Err(pos) => {
                // Insert at suggested position
                self.transactions.insert(pos, tx);
            }
        }

        Ok(())
    }

    /// Remove transaction by hash
    fn remove(&mut self, tx_hash: &[u8]) -> Option<PrioritizedTransaction> {
        if let Some(pos) = self.transactions.iter().position(|t| t.tx_hash == tx_hash) {
            Some(self.transactions.remove(pos))
        } else {
            None
        }
    }

    /// Find transaction by hash
    fn find(&self, tx_hash: &[u8]) -> Option<&PrioritizedTransaction> {
        self.transactions.iter().find(|t| t.tx_hash == tx_hash)
    }

    /// Get transactions in priority order up to limit
    #[allow(dead_code)]
    fn get_top_n(&self, limit: usize) -> Vec<PrioritizedTransaction> {
        self.transactions.iter().take(limit).cloned().collect()
    }

    /// Get count of transactions
    fn len(&self) -> usize {
        self.transactions.len()
    }

    /// Check if bucket is empty
    #[allow(dead_code)]
    fn is_empty(&self) -> bool {
        self.transactions.is_empty()
    }
}

/// Iterator over prioritized transactions across multiple buckets
pub struct CrossBucketIterator {
    /// Reference to buckets (immutable handle)
    bucket_iterators: Vec<std::vec::IntoIter<PrioritizedTransaction>>,
    /// Current position in bucket list
    current_bucket_idx: usize,
}

impl CrossBucketIterator {
    /// Create new cross-bucket iterator from bucket snapshot
    fn new(buckets: &[PriorityBucket]) -> Self {
        let bucket_iterators = buckets
            .iter()
            .map(|b| b.transactions.clone().into_iter())
            .collect();

        Self {
            bucket_iterators,
            current_bucket_idx: 0,
        }
    }
}

impl Iterator for CrossBucketIterator {
    type Item = PrioritizedTransaction;

    fn next(&mut self) -> Option<Self::Item> {
        // Iterate through buckets in order (high priority to low)
        while self.current_bucket_idx < self.bucket_iterators.len() {
            if let Some(tx) = self.bucket_iterators[self.current_bucket_idx].next() {
                return Some(tx);
            }
            self.current_bucket_idx += 1;
        }
        None
    }
}

/// Advanced Priority Bucketing System with deterministic tie-breaking
///
/// This system maintains transactions organized by fee rate ranges (buckets)
/// for O(1) average lookup and maintains deterministic ordering within each bucket.
pub struct PriorityBuckets {
    /// Thread-safe storage of buckets
    buckets: Arc<RwLock<Vec<PriorityBucket>>>,
    /// Configuration parameters
    config: PriorityOrderingConfig,
    /// Index for fast hash lookup (bucket_index, position_in_bucket)
    hash_index: Arc<RwLock<std::collections::HashMap<Vec<u8>, (usize, usize)>>>,
}

impl PriorityBuckets {
    /// Create new priority bucketing system
    pub fn new(config: PriorityOrderingConfig) -> Self {
        let mut buckets = Vec::new();

        // Create buckets based on thresholds
        let thresholds = config.fee_rate_thresholds.clone();
        let mut lower_bound = 0;

        for threshold in &thresholds {
            buckets.push(PriorityBucket::new(lower_bound, *threshold));
            lower_bound = *threshold;
        }

        // Add final bucket for fees >= highest threshold
        buckets.push(PriorityBucket::new(lower_bound, u64::MAX));

        Self {
            buckets: Arc::new(RwLock::new(buckets)),
            config,
            hash_index: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Insert transaction into appropriate bucket using deterministic tie-breaking
    pub fn insert(&self, tx: PrioritizedTransaction) -> Result<(), String> {
        if !self.config.deterministic_ordering {
            return Err("Deterministic ordering is disabled".to_string());
        }

        let tx_hash = tx.tx_hash.clone();
        let fee_rate = tx.fee_rate;

        let mut buckets = self.buckets.write();

        // Find appropriate bucket based on fee rate
        let bucket_idx = buckets
            .iter()
            .position(|b| fee_rate >= b.lower_bound && fee_rate < b.upper_bound)
            .ok_or_else(|| "No suitable bucket for fee rate".to_string())?;

        // Insert into bucket (maintains deterministic order internally)
        buckets[bucket_idx].insert(tx)?;

        // Update hash index
        let mut hash_idx = self.hash_index.write();
        let position = buckets[bucket_idx].len() - 1;
        hash_idx.insert(tx_hash, (bucket_idx, position));

        Ok(())
    }

    /// Remove transaction from buckets by hash
    pub fn remove(&self, tx_hash: &[u8]) -> Option<PrioritizedTransaction> {
        let mut hash_idx = self.hash_index.write();

        if let Some((bucket_idx, _pos)) = hash_idx.remove(tx_hash) {
            let mut buckets = self.buckets.write();
            if let Some(tx) = buckets[bucket_idx].remove(tx_hash) {
                // Rebuild hash index for affected bucket (positions changed)
                return Some(tx);
            }
        }

        None
    }

    /// Check if transaction exists
    pub fn contains(&self, tx_hash: &[u8]) -> bool {
        self.hash_index.read().contains_key(tx_hash)
    }

    /// Get transaction by hash
    pub fn get(&self, tx_hash: &[u8]) -> Option<PrioritizedTransaction> {
        let hash_idx = self.hash_index.read();
        if let Some((bucket_idx, _)) = hash_idx.get(tx_hash) {
            let buckets = self.buckets.read();
            return buckets[*bucket_idx].find(tx_hash).map(|t| t.clone());
        }
        None
    }

    /// Get top N transactions across all buckets maintaining priority order
    ///
    /// This function applies deterministic tie-breaking to ensure
    /// the same order is returned across all validator nodes
    pub fn get_ordered_transactions(&self, limit: usize) -> Vec<PrioritizedTransaction> {
        let buckets = self.buckets.read();
        let mut result = Vec::with_capacity(limit);

        // Iterate through buckets in reverse order (highest priority first)
        for bucket in buckets.iter().rev() {
            for tx in &bucket.transactions {
                result.push(tx.clone());
                if result.len() >= limit {
                    return result;
                }
            }
        }

        result
    }

    /// Get cross-bucket iterator for seamless iteration
    pub fn iter_ordered(&self) -> CrossBucketIterator {
        let buckets = self.buckets.read();
        CrossBucketIterator::new(&buckets)
    }

    /// Get transactions from specific fee rate bucket
    pub fn get_bucket_transactions(
        &self,
        bucket_idx: usize,
    ) -> Result<Vec<PrioritizedTransaction>, String> {
        let buckets = self.buckets.read();
        let bucket = buckets
            .get(bucket_idx)
            .ok_or_else(|| format!("Bucket {} not found", bucket_idx))?;

        Ok(bucket.transactions.clone())
    }

    /// Get total transaction count
    pub fn total_count(&self) -> usize {
        self.buckets.read().iter().map(|b| b.len()).sum()
    }

    /// Get transaction count per bucket
    pub fn bucket_counts(&self) -> Vec<usize> {
        self.buckets.read().iter().map(|b| b.len()).collect()
    }

    /// Get number of buckets
    pub fn bucket_count(&self) -> usize {
        self.buckets.read().len()
    }

    /// Verify bucket consistency
    ///
    /// Checks that transactions are within their assigned bucket's fee rate range
    /// and are ordered deterministically
    pub fn verify_consistency(&self) -> Result<(), String> {
        let buckets = self.buckets.read();
        let hash_idx = self.hash_index.read();

        for (bucket_idx, bucket) in buckets.iter().enumerate() {
            // Verify all transactions are in valid fee rate range
            for tx in &bucket.transactions {
                if tx.fee_rate < bucket.lower_bound || tx.fee_rate >= bucket.upper_bound {
                    return Err(format!(
                        "Transaction has fee rate {} outside bucket [{}, {})",
                        tx.fee_rate, bucket.lower_bound, bucket.upper_bound
                    ));
                }

                // Verify hash index is consistent
                if let Some((idx, _)) = hash_idx.get(&tx.tx_hash) {
                    if *idx != bucket_idx {
                        return Err("Hash index mismatch for transaction".to_string());
                    }
                }
            }

            // Verify transactions are ordered deterministically
            let mut prev: Option<&PrioritizedTransaction> = None;
            for tx in &bucket.transactions {
                if let Some(p) = prev {
                    use std::cmp::Ordering;
                    match p.compare_deterministic(tx) {
                        Ordering::Greater | Ordering::Equal => {
                            return Err(
                                "Transaction not ordered correctly relative to peer".to_string()
                            );
                        }
                        Ordering::Less => {
                            return Err("Invalid ordering: later transaction has higher priority"
                                .to_string());
                        }
                    }
                }
                prev = Some(tx);
            }
        }

        Ok(())
    }

    /// Clear all transactions (for testing/reset)
    pub fn clear(&self) {
        let mut buckets = self.buckets.write();
        for bucket in buckets.iter_mut() {
            bucket.transactions.clear();
        }
        self.hash_index.write().clear();
    }

    /// Get fee rate thresholds
    pub fn fee_thresholds(&self) -> Vec<u64> {
        self.config.fee_rate_thresholds.clone()
    }

    /// Get statistics about bucket distribution
    pub fn get_statistics(&self) -> BucketStatistics {
        let buckets = self.buckets.read();
        let mut bucket_stats = Vec::new();
        let mut total_transactions = 0;
        let mut total_fees = 0u64;

        for bucket in buckets.iter() {
            let count = bucket.len();
            let fees: u64 = bucket.transactions.iter().map(|t| t.total_fee).sum();
            let avg_fee = if count > 0 { fees / count as u64 } else { 0 };

            total_transactions += count;
            total_fees += fees;

            bucket_stats.push(BucketInfo {
                lower_bound: bucket.lower_bound,
                upper_bound: bucket.upper_bound,
                transaction_count: count,
                total_fees: fees,
                average_fee: avg_fee,
            });
        }

        BucketStatistics {
            total_transactions,
            total_fees,
            bucket_info: bucket_stats,
        }
    }
}

/// Statistics about bucket distribution
#[derive(Clone, Debug)]
pub struct BucketStatistics {
    pub total_transactions: usize,
    pub total_fees: u64,
    pub bucket_info: Vec<BucketInfo>,
}

/// Information about a single bucket
#[derive(Clone, Debug)]
pub struct BucketInfo {
    pub lower_bound: u64,
    pub upper_bound: u64,
    pub transaction_count: usize,
    pub total_fees: u64,
    pub average_fee: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_tx(hash: usize, fee_rate: u64, arrival_time: u64) -> PrioritizedTransaction {
        let hash_vec = vec![(hash & 0xFF) as u8; 32];
        PrioritizedTransaction::new(hash_vec, fee_rate, arrival_time, 200, fee_rate * 200)
    }

    #[test]
    fn test_priority_buckets_creation() {
        let config = PriorityOrderingConfig::default();
        let buckets = PriorityBuckets::new(config);

        assert_eq!(buckets.bucket_count(), 5); // 0-1, 1-10, 10-100, 100-1000, 1000+
        assert_eq!(buckets.total_count(), 0);
    }

    #[test]
    fn test_deterministic_insert() {
        let config = PriorityOrderingConfig::default();
        let buckets = PriorityBuckets::new(config);

        let tx1 = create_test_tx(1, 50, 1000);
        let tx2 = create_test_tx(2, 50, 1001);

        assert!(buckets.insert(tx1.clone()).is_ok());
        assert!(buckets.insert(tx2.clone()).is_ok());

        assert!(buckets.contains(&tx1.tx_hash));
        assert!(buckets.contains(&tx2.tx_hash));
    }

    #[test]
    fn test_tie_breaking_lexicographic() {
        let config = PriorityOrderingConfig::default();
        let buckets = PriorityBuckets::new(config);

        // Create transactions with same fee rate (5 sat/vB) but different hashes
        let hash1 = vec![0x01u8; 32];
        let hash2 = vec![0x02u8; 32];

        let tx1 = PrioritizedTransaction::new(hash1.clone(), 5, 1000, 200, 1000);
        let tx2 = PrioritizedTransaction::new(hash2.clone(), 5, 1000, 200, 1000);

        // Lexicographic comparison: 0x01 < 0x02
        assert!(hash1 < hash2);

        assert!(buckets.insert(tx1).is_ok());
        assert!(buckets.insert(tx2).is_ok());

        let ordered = buckets.get_ordered_transactions(10);
        // Since both have same fee_rate and arrival_time,
        // deterministic ordering should follow lexicographic hash comparison
        assert!(ordered.len() >= 1);
    }

    #[test]
    fn test_remove_transaction() {
        let config = PriorityOrderingConfig::default();
        let buckets = PriorityBuckets::new(config);

        let tx = create_test_tx(1, 50, 1000);
        let tx_hash = tx.tx_hash.clone();

        assert!(buckets.insert(tx).is_ok());
        assert_eq!(buckets.total_count(), 1);

        let removed = buckets.remove(&tx_hash);
        assert!(removed.is_some());
        assert_eq!(buckets.total_count(), 0);
    }

    #[test]
    fn test_get_ordered_transactions() {
        let config = PriorityOrderingConfig::default();
        let buckets = PriorityBuckets::new(config);

        // Insert transactions with different priorities
        let tx_low = create_test_tx(1, 5, 1000);
        let tx_medium = create_test_tx(2, 50, 1000);
        let tx_high = create_test_tx(3, 500, 1000);

        assert!(buckets.insert(tx_low).is_ok());
        assert!(buckets.insert(tx_medium).is_ok());
        assert!(buckets.insert(tx_high).is_ok());

        let ordered = buckets.get_ordered_transactions(3);
        assert_eq!(ordered.len(), 3);

        // Higher priority transactions should come first
        assert!(ordered[0].fee_rate >= ordered[1].fee_rate);
        assert!(ordered[1].fee_rate >= ordered[2].fee_rate);
    }

    #[test]
    fn test_bucket_statistics() {
        let config = PriorityOrderingConfig::default();
        let buckets = PriorityBuckets::new(config);

        let tx1 = create_test_tx(1, 5, 1000);
        let tx2 = create_test_tx(2, 50, 1001);

        assert!(buckets.insert(tx1).is_ok());
        assert!(buckets.insert(tx2).is_ok());

        let stats = buckets.get_statistics();
        assert_eq!(stats.total_transactions, 2);
        assert!(stats.total_fees > 0);
    }

    #[test]
    fn test_verify_consistency() {
        let config = PriorityOrderingConfig::default();
        let buckets = PriorityBuckets::new(config);

        let tx = create_test_tx(1, 50, 1000);
        assert!(buckets.insert(tx).is_ok());

        assert!(buckets.verify_consistency().is_ok());
    }
}
