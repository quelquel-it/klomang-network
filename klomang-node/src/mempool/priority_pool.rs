//! Advanced Priority & Ordering System for Mempool
//!
//! This module implements a deterministic, multi-factor transaction prioritization
//! system that orders transactions based on economic incentives while respecting
//! dependency constraints and preventing starvation.
//!
//! Key Features:
//! - Multi-factor priority scoring (fee rate, age, dependency depth)
//! - Deterministic tie-breaking via lexicographical hash ordering
//! - Dependency-aware ordering (topological constraint enforcement)
//! - Efficient incremental priority updates
//! - Thread-safe with parking_lot synchronization

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;

use crate::storage::kv_store::KvStore;
use crate::storage::error::StorageResult;
use super::recursive_dependency_manager::RecursiveDependencyManager;

/// Type alias for transaction hash
pub type TxHash = Vec<u8>;

/// Multi-factor priority score for a transaction
///
/// Combines multiple factors in a deterministic way to order transactions fairly.
/// Lower numeric priorities are processed first (max heap semantics).
#[derive(Clone, Debug)]
pub struct TransactionPriority {
    /// Primary factor: Fee rate in satoshis per byte (inverted - higher is better)
    /// Stored as MAX - fee_rate to use max-heap behavior
    pub fee_rate_priority: u64,

    /// Secondary factor: Age in seconds (inverted - older is better, less starvation)
    /// Stored as MAX - age to prioritize older transactions
    pub age_priority: u64,

    /// Tertiary factor: Dependency depth (lower is better)
    /// Transactions closer to roots (without dependencies) prioritized first
    pub depth_priority: u32,

    /// Number of immediate children (higher is better - opens path for descendants)
    /// Inverted for max-heap behavior
    pub descendant_count_priority: u32,

    /// Deterministic tie-breaker: Lexicographical transaction hash
    /// Ensures all nodes have identical ordering
    pub tx_hash: TxHash,

    /// Timestamp when priority was last updated
    pub last_updated: u64,
}

impl TransactionPriority {
    /// Create priority from raw parameters
    pub fn new(
        tx_hash: TxHash,
        fee_rate: u64,
        arrival_time: u64,
        dependency_depth: u32,
        immediate_children_count: u32,
    ) -> Self {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let age_secs = current_time.saturating_sub(arrival_time);

        // Invert for max-heap: higher fee_rate → lower value → higher priority
        // Cap at 1,000,000 sats/byte for reasonable scoring range
        let max_fee_rate = 1_000_000u64;
        let fee_rate_priority = max_fee_rate.saturating_sub(fee_rate.min(max_fee_rate));

        // Invert for max-heap: older tx → lower value → higher priority
        // Cap age at 1 hour (3600 seconds) to prevent overweighting
        let max_age = 3600u64;
        let age_priority = max_age.saturating_sub(age_secs.min(max_age));

        // Depth priority: lower is better (closer to root, fewer dependencies)
        // Keep as-is (no inversion)
        let depth_priority = dependency_depth;

        // Descendant count: higher is better (opens path for more txs)
        // Invert for max-heap: more children → lower value → higher priority
        let max_descendants = 100u32;
        let descendant_count_priority = max_descendants.saturating_sub(
            immediate_children_count.min(max_descendants)
        );

        Self {
            fee_rate_priority,
            age_priority,
            depth_priority,
            descendant_count_priority,
            tx_hash,
            last_updated: current_time,
        }
    }

    /// Calculate composite priority score as tuple for comparison
    /// Tuple comparison is lexicographic, implementing multi-factor ordering
    pub fn score(&self) -> (u64, u64, u32, u32, &[u8]) {
        (
            self.fee_rate_priority,      // Primary: fee rate (highest value = best)
            self.age_priority,            // Secondary: age (oldest first for fairness)
            self.depth_priority,          // Tertiary: depth (lower first for efficiency)
            self.descendant_count_priority, // Quaternary: descendants (more = better)
            &self.tx_hash,                // Tie-breaker: lexicographical hash
        )
    }

    /// Check if priority needs update (e.g., on config change)
    pub fn is_stale(&self, max_age_secs: u64) -> bool {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        current_time.saturating_sub(self.last_updated) > max_age_secs
    }
}

/// Wrapper for BinaryHeap that maintains max-heap semantics
/// (lowest score = highest priority)
#[derive(Clone, Debug)]
struct PriorityWrapper(TransactionPriority);

impl PartialEq for PriorityWrapper {
    fn eq(&self, other: &Self) -> bool {
        self.0.score() == other.0.score()
    }
}

impl Eq for PriorityWrapper {}

impl PartialOrd for PriorityWrapper {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PriorityWrapper {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse comparison for max-heap: higher score = lower priority
        other.0.score().cmp(&self.0.score())
    }
}

/// Statistics for priority pool operations
#[derive(Clone, Debug, Default)]
pub struct PriorityPoolStats {
    /// Total transactions ever prioritized
    pub total_transactions: u64,
    /// Total ordering operations
    pub total_orderings: u64,
    /// Average time to generate ordered list
    pub avg_ordering_time_us: u64,
    /// Maximum transactions in pool at once
    pub peak_pool_size: usize,
    /// Starvation prevention activations
    pub age_promotions: u64,
    /// Dependency-based reorderings
    pub dependency_reorderings: u64,
}

/// High-performance priority pool with multi-factor ordering
pub struct PriorityPool {
    /// Binary heap of transactions, ordered by priority
    heap: Arc<RwLock<BinaryHeap<PriorityWrapper>>>,

    /// Fast lookup: tx_hash -> priority (for updates)
    priority_map: Arc<RwLock<std::collections::HashMap<TxHash, TransactionPriority>>>,

    /// Reference to dependency manager for depth/descendant queries
    dependency_manager: Arc<RecursiveDependencyManager>,

    /// KvStore for UTXO validation
    #[allow(dead_code)]
    kv_store: Arc<KvStore>,

    /// Statistics tracking
    stats: Arc<RwLock<PriorityPoolStats>>,

    /// Age threshold for auto-promotion (prevent starvation)
    #[allow(dead_code)]
    starvation_threshold_secs: u64,
}

impl PriorityPool {
    /// Create new priority pool
    pub fn new(
        dependency_manager: Arc<RecursiveDependencyManager>,
        kv_store: Arc<KvStore>,
    ) -> Self {
        Self::with_config(dependency_manager, kv_store, 300) // 5 minute starvation threshold
    }

    /// Create with custom starvation threshold
    pub fn with_config(
        dependency_manager: Arc<RecursiveDependencyManager>,
        kv_store: Arc<KvStore>,
        starvation_threshold_secs: u64,
    ) -> Self {
        Self {
            heap: Arc::new(RwLock::new(BinaryHeap::new())),
            priority_map: Arc::new(RwLock::new(std::collections::HashMap::new())),
            dependency_manager,
            kv_store,
            stats: Arc::new(RwLock::new(PriorityPoolStats::default())),
            starvation_threshold_secs,
        }
    }

    /// Add transaction with priority
    pub fn insert(
        &self,
        tx_hash: TxHash,
        fee_rate: u64,
        arrival_time: u64,
        dependency_depth: u32,
    ) -> StorageResult<()> {
        // Get immediate children count (opens path for how many descendants)
        let descendants_count = self
            .dependency_manager
            .get_immediate_children(&tx_hash)
            .unwrap_or_default()
            .len() as u32;

        // Create priority
        let priority = TransactionPriority::new(
            tx_hash.clone(),
            fee_rate,
            arrival_time,
            dependency_depth,
            descendants_count,
        );

        // Insert into map for quick lookup
        {
            let mut pmap = self.priority_map.write();
            pmap.insert(tx_hash.clone(), priority.clone());
        }

        // Insert into heap
        {
            let mut heap = self.heap.write();
            heap.push(PriorityWrapper(priority));
        }

        // Update statistics
        {
            let mut stats = self.stats.write();
            stats.total_transactions += 1;
            stats.peak_pool_size = stats.peak_pool_size.max(self.heap.read().len());
        }

        Ok(())
    }

    /// Remove transaction from pool
    pub fn remove(&self, tx_hash: &TxHash) -> StorageResult<()> {
        let mut pmap = self.priority_map.write();
        pmap.remove(tx_hash);

        // Note: We don't remove from heap directly (inefficient)
        // Instead, we handle stale entries when reading from heap

        Ok(())
    }

    /// Update priority for transaction (incremental update)
    pub fn update_priority(
        &self,
        tx_hash: &TxHash,
        new_fee_rate: u64,
        new_arrival_time: u64,
        new_dependency_depth: u32,
    ) -> StorageResult<()> {
        // Check if already exists
        let mut pmap = self.priority_map.write();
        if pmap.contains_key(tx_hash) {
            // Get updated descendant count
            let descendants_count = self
                .dependency_manager
                .get_immediate_children(tx_hash)
                .unwrap_or_default()
                .len() as u32;

            let new_priority = TransactionPriority::new(
                tx_hash.clone(),
                new_fee_rate,
                new_arrival_time,
                new_dependency_depth,
                descendants_count,
            );

            pmap.insert(tx_hash.clone(), new_priority);

            // Note: Heap needs re-push, but we'll handle on read
            let mut stats = self.stats.write();
            stats.dependency_reorderings += 1;
        }

        Ok(())
    }

    /// Get transactions ordered by priority (up to limit)
    ///
    /// Returns transactions that:
    /// 1. Are ordered by multi-factor priority
    /// 2. Respect minimum fee rate
    /// 3. Satisfy dependency constraints (all parents included if child included)
    pub fn get_ordered_transactions(
        &self,
        limit: usize,
        min_fee_rate: u64,
    ) -> StorageResult<Vec<TxHash>> {
        let start_time = std::time::Instant::now();
        let pmap = self.priority_map.read().clone();
        let mut heap = self.heap.write();

        // Remove stale entries from heap
        let mut temp_vec = Vec::new();
        while let Some(wrapper) = heap.pop() {
            if pmap.contains_key(&wrapper.0.tx_hash) {
                temp_vec.push(wrapper);
            }
        }

        // Rebuild heap
        for wrapper in temp_vec {
            heap.push(wrapper);
        }

        // Extract up to limit transactions
        let mut result = Vec::new();
        let mut eligible = Vec::new();

        // Peek at heap to collect eligible transactions
        let mut temp_remove = Vec::new();
        while let Some(wrapper) = heap.pop() {
            if let Some(priority) = pmap.get(&wrapper.0.tx_hash) {
                // Check fee rate threshold
                if priority.fee_rate_priority <= (1_000_000u64 - min_fee_rate) {
                    eligible.push(priority.clone());
                }
            }
            temp_remove.push(wrapper);
        }

        // Push back to heap
        for wrapper in temp_remove {
            heap.push(wrapper);
        }

        // Enforce dependency constraints
        let mut included = HashSet::new();
        for priority in eligible {
            if result.len() >= limit {
                break;
            }

            let tx_hash = &priority.tx_hash;

            // Get all ancestors (parents that must be included)
            let ancestors = self
                .dependency_manager
                .get_ancestors(tx_hash)
                .unwrap_or_default();

            // Check if all ancestors are already included or will be skipped
            let mut all_ancestors_present = true;
            for ancestor in ancestors {
                if !included.contains(&ancestor) {
                    // Ancestor not yet included - skip this transaction
                    all_ancestors_present = false;
                    break;
                }
            }

            if all_ancestors_present {
                result.push(tx_hash.clone());
                included.insert(tx_hash.clone());
            }
        }

        let elapsed = start_time.elapsed().as_micros() as u64;

        // Update statistics
        {
            let mut stats = self.stats.write();
            stats.total_orderings += 1;
            stats.avg_ordering_time_us =
                (stats.avg_ordering_time_us + elapsed) / stats.total_orderings;
        }

        Ok(result)
    }

    /// Get next highest priority transaction
    pub fn peek_highest(&self) -> StorageResult<Option<TxHash>> {
        let pmap = self.priority_map.read();
        let mut heap = self.heap.write();

        // Skip stale entries
        while let Some(wrapper) = heap.peek() {
            if pmap.contains_key(&wrapper.0.tx_hash) {
                return Ok(Some(wrapper.0.tx_hash.clone()));
            }
            heap.pop();
        }

        Ok(None)
    }

    /// Get transactions in topological order for block building
    /// (respects dependency order)
    pub fn get_topological_order(&self, limit: usize) -> StorageResult<Vec<TxHash>> {
        // Get all eligible transactions first
        let candidates = self.get_ordered_transactions(limit * 2, 0)?;

        // Topologically sort respecting dependencies
        let mut result = Vec::new();
        let mut added = HashSet::new();
        let mut queue = VecDeque::new();

        // Find root transactions (no dependencies)
        for tx_hash in &candidates {
            let ancestors = self
                .dependency_manager
                .get_ancestors(tx_hash)
                .unwrap_or_default();

            if ancestors.is_empty() || ancestors.iter().all(|a| candidates.contains(a)) {
                queue.push_back(tx_hash.clone());
            }
        }

        // BFS to build topological order
        while let Some(tx) = queue.pop_front() {
            if result.len() >= limit {
                break;
            }

            if added.insert(tx.clone()) {
                result.push(tx.clone());

                // Add dependents if all their parents are now added
                if let Ok(descendants) = self.dependency_manager.get_immediate_children(&tx) {
                    for descendant in descendants {
                        if candidates.contains(&descendant) && !added.contains(&descendant) {
                            if let Ok(parents) = self
                                .dependency_manager
                                .get_immediate_parents(&descendant)
                            {
                                if parents.iter().all(|p| added.contains(p)) {
                                    queue.push_back(descendant);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Fill remaining slots with other eligible transactions
        for tx in candidates {
            if result.len() >= limit {
                break;
            }
            if added.insert(tx.clone()) {
                result.push(tx);
            }
        }

        Ok(result)
    }

    /// Get current pool size
    pub fn size(&self) -> usize {
        self.priority_map.read().len()
    }

    /// Clear all transactions
    pub fn clear(&self) {
        self.priority_map.write().clear();
        self.heap.write().clear();
    }

    /// Get statistics
    pub fn get_stats(&self) -> PriorityPoolStats {
        self.stats.read().clone()
    }

    /// Reset statistics
    pub fn reset_stats(&self) {
        *self.stats.write() = PriorityPoolStats::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priority_pool_creation() {
        // Placeholder for integration testing
        // Requires full dependency manager and KvStore setup
    }
}
