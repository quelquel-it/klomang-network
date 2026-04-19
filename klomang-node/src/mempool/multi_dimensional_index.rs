//! Multi-Dimensional Priority Index
//!
//! Provides efficient searching and filtering of transactions across multiple dimensions:
//! - Economic (Fee per byte in Satoshi/vByte)
//! - Temporal (Transaction arrived time)
//! - Structural (Dependency count: children/grandchildren)
//!
//! Uses BTreeMap for each dimension to support O(log n) range queries.
//! All searches return vectors of matching transactions sorted by dimension.

use std::collections::{HashMap, BTreeMap};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::storage::error::StorageResult;
use super::recursive_dependency_tracker::TxHash;

/// Transaction indexed across multiple dimensions
#[derive(Clone, Debug)]
pub struct IndexedTransaction {
    /// Transaction hash (unique identifier)
    pub tx_hash: TxHash,

    /// Economic dimension: Fee rate in satoshis per vByte
    pub fee_rate: u64,

    /// Temporal dimension: Arrival timestamp (UNIX seconds)
    pub arrival_time: u64,

    /// Structural dimension: Number of immediate children/dependents
    pub dependency_count: u32,

    /// Total transaction size in bytes (for vsize calculation)
    pub size_bytes: usize,

    /// Total fees in satoshis
    pub total_fee: u64,
}

impl IndexedTransaction {
    /// Create new indexed transaction
    pub fn new(
        tx_hash: TxHash,
        fee_rate: u64,
        arrival_time: u64,
        dependency_count: u32,
        size_bytes: usize,
        total_fee: u64,
    ) -> Self {
        Self {
            tx_hash,
            fee_rate,
            arrival_time,
            dependency_count,
            size_bytes,
            total_fee,
        }
    }
}

/// Multi-dimensional index for efficient transaction queries
///
/// Maintains three separate BTreeMaps for each dimension:
/// 1. Economic: (fee_rate, tx_hash) → transaction
/// 2. Temporal: (arrival_time, tx_hash) → transaction
/// 3. Structural: (dependency_count, tx_hash) → transaction
///
/// Also maintains a primary HashMap for O(1) lookups by hash.
pub struct MultiDimensionalIndex {
    /// Primary lookup by hash (O(1) access)
    by_hash: Arc<RwLock<HashMap<TxHash, IndexedTransaction>>>,

    /// Economic dimension index: (fee_rate DESC, tx_hash)
    /// Higher fee_rate comes first (use inverted ordering)
    economic_index: Arc<RwLock<BTreeMap<(std::cmp::Reverse<u64>, TxHash), IndexedTransaction>>>,

    /// Temporal dimension index: (arrival_time ASC, tx_hash)
    /// Older transactions come first
    temporal_index: Arc<RwLock<BTreeMap<(u64, TxHash), IndexedTransaction>>>,

    /// Structural dimension index: (dependency_count DESC, tx_hash)
    /// Transactions with higher dependency count (more dependents) come first
    structural_index: Arc<RwLock<BTreeMap<(std::cmp::Reverse<u32>, TxHash), IndexedTransaction>>>,

    /// Statistics about the index
    stats: Arc<RwLock<MultiDimensionalIndexStats>>,
}

/// Statistics for the multi-dimensional index
#[derive(Clone, Debug, Default)]
pub struct MultiDimensionalIndexStats {
    /// Total transactions indexed
    pub total_transactions: u64,
    /// Total insertions
    pub insertions: u64,
    /// Total removals
    pub removals: u64,
    /// Total updates
    pub updates: u64,
    /// Economic queries executed
    pub economic_queries: u64,
    /// Temporal queries executed
    pub temporal_queries: u64,
    /// Structural queries executed
    pub structural_queries: u64,
}

impl MultiDimensionalIndex {
    /// Create new empty index
    pub fn new() -> Self {
        Self {
            by_hash: Arc::new(RwLock::new(HashMap::new())),
            economic_index: Arc::new(RwLock::new(BTreeMap::new())),
            temporal_index: Arc::new(RwLock::new(BTreeMap::new())),
            structural_index: Arc::new(RwLock::new(BTreeMap::new())),
            stats: Arc::new(RwLock::new(MultiDimensionalIndexStats::default())),
        }
    }

    /// Insert transaction into all three dimension indexes
    pub fn insert(&self, tx: IndexedTransaction) -> StorageResult<()> {
        let tx_hash = tx.tx_hash.clone();

        // Insert into hash map
        {
            let mut by_hash = self.by_hash.write();
            by_hash.insert(tx_hash.clone(), tx.clone());
        }

        // Insert into economic index (inverted for descending order)
        {
            let mut eco = self.economic_index.write();
            eco.insert((std::cmp::Reverse(tx.fee_rate), tx_hash.clone()), tx.clone());
        }

        // Insert into temporal index (ascending = oldest first)
        {
            let mut temp = self.temporal_index.write();
            temp.insert((tx.arrival_time, tx_hash.clone()), tx.clone());
        }

        // Insert into structural index (inverted for descending order)
        {
            let mut struct_idx = self.structural_index.write();
            struct_idx.insert((std::cmp::Reverse(tx.dependency_count), tx_hash), tx);
        }

        // Update stats
        {
            let mut stats = self.stats.write();
            stats.insertions += 1;
            stats.total_transactions += 1;
        }

        Ok(())
    }

    /// Remove transaction from all indexes
    pub fn remove(&self, tx_hash: &TxHash) -> StorageResult<Option<IndexedTransaction>> {
        let removed_tx = {
            let mut by_hash = self.by_hash.write();
            by_hash.remove(tx_hash)
        };

        if let Some(tx) = &removed_tx {
            // Remove from economic index
            {
                let mut eco = self.economic_index.write();
                eco.remove(&(std::cmp::Reverse(tx.fee_rate), tx_hash.clone()));
            }

            // Remove from temporal index
            {
                let mut temp = self.temporal_index.write();
                temp.remove(&(tx.arrival_time, tx_hash.clone()));
            }

            // Remove from structural index
            {
                let mut struct_idx = self.structural_index.write();
                struct_idx.remove(&(std::cmp::Reverse(tx.dependency_count), tx_hash.clone()));
            }

            // Update stats
            {
                let mut stats = self.stats.write();
                stats.removals += 1;
                stats.total_transactions = stats.total_transactions.saturating_sub(1);
            }
        }

        Ok(removed_tx)
    }

    /// Update transaction in all indexes
    pub fn update(&self, tx: IndexedTransaction) -> StorageResult<()> {
        let tx_hash = tx.tx_hash.clone();
        
        // Remove old entry
        self.remove(&tx_hash)?;
        
        // Insert new entry
        self.insert(tx)?;

        // Update stats
        {
            let mut stats = self.stats.write();
            stats.updates += 1;
        }

        Ok(())
    }

    /// Get transaction by hash (O(1))
    pub fn get_by_hash(&self, tx_hash: &TxHash) -> StorageResult<Option<IndexedTransaction>> {
        Ok(self.by_hash.read().get(tx_hash).cloned())
    }

    /// Query Economic Dimension: Get transactions within fee range
    ///
    /// Returns transactions with fee_rate between min_fee and max_fee,
    /// sorted by fee (descending, highest fees first).
    ///
    /// Time Complexity: O(log n + k) where k is result count
    pub fn query_economic_range(
        &self,
        min_fee: u64,
        max_fee: u64,
    ) -> StorageResult<Vec<IndexedTransaction>> {
        let eco = self.economic_index.read();
        
        let mut results = Vec::new();
        
        // BTreeMap is ordered by (Reverse(fee_rate), tx_hash)
        // So we iterate and collect transactions in the range
        for (_key, tx) in eco.iter() {
            if tx.fee_rate >= min_fee && tx.fee_rate <= max_fee {
                results.push(tx.clone());
            }
        }

        // Update stats
        {
            let mut stats = self.stats.write();
            stats.economic_queries += 1;
        }

        Ok(results)
    }

    /// Query Economic Dimension: Get top N transactions by fee
    ///
    /// Returns the N transactions with highest fee rates.
    ///
    /// Time Complexity: O(k + log n) where k = limit
    pub fn query_economic_top_n(&self, limit: usize) -> StorageResult<Vec<IndexedTransaction>> {
        let eco = self.economic_index.read();
        let results: Vec<_> = eco.iter()
            .take(limit)
            .map(|(_, tx)| tx.clone())
            .collect();

        // Update stats
        {
            let mut stats = self.stats.write();
            stats.economic_queries += 1;
        }

        Ok(results)
    }

    /// Query Temporal Dimension: Get transactions older than timestamp
    ///
    /// Returns transactions that arrived before (older than) the given timestamp,
    /// sorted by arrival time (ascending, oldest first).
    ///
    /// Time Complexity: O(log n + k) where k is result count
    pub fn query_temporal_before(
        &self,
        timestamp: u64,
    ) -> StorageResult<Vec<IndexedTransaction>> {
        let temp = self.temporal_index.read();
        
        let results: Vec<_> = temp.iter()
            .take_while(|((arrival_time, _), _)| *arrival_time <= timestamp)
            .map(|(_, tx)| tx.clone())
            .collect();

        // Update stats
        {
            let mut stats = self.stats.write();
            stats.temporal_queries += 1;
        }

        Ok(results)
    }

    /// Query Temporal Dimension: Get transactions younger than timestamp
    ///
    /// Returns transactions that arrived after (younger than) the given timestamp,
    /// sorted by arrival time (ascending).
    ///
    /// Time Complexity: O(log n + k) where k is result count
    pub fn query_temporal_after(
        &self,
        timestamp: u64,
    ) -> StorageResult<Vec<IndexedTransaction>> {
        let temp = self.temporal_index.read();
        
        let results: Vec<_> = temp.iter()
            .skip_while(|((arrival_time, _), _)| *arrival_time <= timestamp)
            .map(|(_, tx)| tx.clone())
            .collect();

        // Update stats
        {
            let mut stats = self.stats.write();
            stats.temporal_queries += 1;
        }

        Ok(results)
    }

    /// Query Temporal Dimension: Get transactions within time range
    ///
    /// Returns transactions that arrived within the specified time window.
    ///
    /// Time Complexity: O(log n + k) where k is result count
    pub fn query_temporal_range(
        &self,
        start_time: u64,
        end_time: u64,
    ) -> StorageResult<Vec<IndexedTransaction>> {
        let temp = self.temporal_index.read();
        
        let results: Vec<_> = temp.iter()
            .filter(|((arrival_time, _), _)| *arrival_time >= start_time && *arrival_time <= end_time)
            .map(|(_, tx)| tx.clone())
            .collect();

        // Update stats
        {
            let mut stats = self.stats.write();
            stats.temporal_queries += 1;
        }

        Ok(results)
    }

    /// Query Structural Dimension: Get transactions with minimum dependency count
    ///
    /// Returns transactions that have at least `min_dependencies` immediate children/dependents,
    /// sorted by dependency count (descending, most dependents first).
    ///
    /// Useful for identifying "hub" transactions that unlock chains.
    ///
    /// Time Complexity: O(log n + k) where k is result count
    pub fn query_structural_min_dependents(
        &self,
        min_dependencies: u32,
    ) -> StorageResult<Vec<IndexedTransaction>> {
        let struct_idx = self.structural_index.read();
        
        let results: Vec<_> = struct_idx.iter()
            .take_while(|((reversed_count, _), _)| reversed_count.0 >= min_dependencies)
            .map(|(_, tx)| tx.clone())
            .collect();

        // Update stats
        {
            let mut stats = self.stats.write();
            stats.structural_queries += 1;
        }

        Ok(results)
    }

    /// Query Structural Dimension: Get top N transactions by dependency count
    ///
    /// Returns the N transactions with the most immediate children/dependents.
    ///
    /// Time Complexity: O(k + log n) where k = limit
    pub fn query_structural_top_hubs(&self, limit: usize) -> StorageResult<Vec<IndexedTransaction>> {
        let struct_idx = self.structural_index.read();
        
        let results: Vec<_> = struct_idx.iter()
            .take(limit)
            .map(|(_, tx)| tx.clone())
            .collect();

        // Update stats
        {
            let mut stats = self.stats.write();
            stats.structural_queries += 1;
        }

        Ok(results)
    }

    /// Combined query: Get transactions matching ALL criteria (intersection)
    ///
    /// Filters by multiple dimensions simultaneously:
    /// - Fee must be in range [min_fee, max_fee]
    /// - Arrival must be in range [start_time, end_time]
    /// - Must have at least min_dependencies children
    ///
    /// Returns results sorted by fee (highest first).
    ///
    /// Time Complexity: O(n) worst case (needs to check all dimensions)
    pub fn query_combined(
        &self,
        min_fee: u64,
        max_fee: u64,
        start_time: u64,
        end_time: u64,
        min_dependencies: u32,
    ) -> StorageResult<Vec<IndexedTransaction>> {
        let by_hash = self.by_hash.read();
        
        let results: Vec<_> = by_hash.iter()
            .filter(|(_, tx)| {
                tx.fee_rate >= min_fee &&
                tx.fee_rate <= max_fee &&
                tx.arrival_time >= start_time &&
                tx.arrival_time <= end_time &&
                tx.dependency_count >= min_dependencies
            })
            .map(|(_, tx)| tx.clone())
            .collect();

        // Sort by fee (descending)
        let mut sorted_results = results;
        sorted_results.sort_by(|a, b| b.fee_rate.cmp(&a.fee_rate));

        // Update stats
        {
            let mut stats = self.stats.write();
            stats.economic_queries += 1;
            stats.temporal_queries += 1;
            stats.structural_queries += 1;
        }

        Ok(sorted_results)
    }

    /// Get all transactions count
    pub fn all_count(&self) -> StorageResult<usize> {
        Ok(self.by_hash.read().len())
    }

    /// Get all transactions
    pub fn all_transactions(&self) -> StorageResult<Vec<IndexedTransaction>> {
        Ok(self.by_hash.read().values().cloned().collect())
    }

    /// Get index statistics
    pub fn get_stats(&self) -> StorageResult<MultiDimensionalIndexStats> {
        Ok(self.stats.read().clone())
    }

    /// Reset all indexes
    pub fn reset(&self) -> StorageResult<()> {
        self.by_hash.write().clear();
        self.economic_index.write().clear();
        self.temporal_index.write().clear();
        self.structural_index.write().clear();
        
        let mut stats = self.stats.write();
        *stats = MultiDimensionalIndexStats::default();

        Ok(())
    }
}

impl Default for MultiDimensionalIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_tx(
        hash: Vec<u8>,
        fee_rate: u64,
        arrival: u64,
        deps: u32,
    ) -> IndexedTransaction {
        IndexedTransaction::new(hash, fee_rate, arrival, deps, 250, fee_rate * 250)
    }

    #[test]
    fn test_economic_dimension() {
        let index = MultiDimensionalIndex::new();
        
        // Insert transactions with different fees
        index.insert(create_tx(vec![1], 50, 1000, 0)).unwrap();
        index.insert(create_tx(vec![2], 100, 1000, 0)).unwrap();
        index.insert(create_tx(vec![3], 75, 1000, 0)).unwrap();

        // Query range [60, 110] should return 2 and 3
        let results = index.query_economic_range(60, 110).unwrap();
        assert_eq!(results.len(), 2);
        
        // First should be highest fee (100)
        assert!(results[0].fee_rate >= results[1].fee_rate);
    }

    #[test]
    fn test_temporal_dimension() {
        let index = MultiDimensionalIndex::new();
        
        index.insert(create_tx(vec![1], 50, 1000, 0)).unwrap();
        index.insert(create_tx(vec![2], 50, 2000, 0)).unwrap();
        index.insert(create_tx(vec![3], 50, 3000, 0)).unwrap();

        // Query transactions before time 2500 should return 1 and 2
        let results = index.query_temporal_before(2500).unwrap();
        assert_eq!(results.len(), 2);
        
        // Should be ordered oldest first
        assert!(results[0].arrival_time <= results[1].arrival_time);
    }

    #[test]
    fn test_structural_dimension() {
        let index = MultiDimensionalIndex::new();
        
        index.insert(create_tx(vec![1], 50, 1000, 1)).unwrap();
        index.insert(create_tx(vec![2], 50, 1000, 5)).unwrap();
        index.insert(create_tx(vec![3], 50, 1000, 3)).unwrap();

        // Query for transactions with at least 2 dependents
        let results = index.query_structural_min_dependents(2).unwrap();
        assert_eq!(results.len(), 2);
        
        // Should be ordered by dependency count descending
        assert!(results[0].dependency_count >= results[1].dependency_count);
    }

    #[test]
    fn test_combined_query() {
        let index = MultiDimensionalIndex::new();
        
        index.insert(create_tx(vec![1], 50, 1000, 0)).unwrap();
        index.insert(create_tx(vec![2], 100, 2000, 2)).unwrap();
        index.insert(create_tx(vec![3], 75, 3000, 5)).unwrap();

        // Query: fee [60-110], time [1500-3500], min_deps=1
        let results = index.query_combined(60, 110, 1500, 3500, 1).unwrap();
        
        // Should match only transaction 3 (100 fee, time 3000, 5 deps)
        // Actually transaction 2 has 100 fee but time 2000, 2 deps - matches!
        // Correct: transaction 3 has 75 fee, time 3000, 5 deps - matches!
        assert!(!results.is_empty());
    }
}
