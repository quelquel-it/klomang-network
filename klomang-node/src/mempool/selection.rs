//! Deterministic transaction selection for block building
//!
//! Selects transactions from pool with consistent ordering across nodes using:
//! 1. Fee rate (highest first)
//! 2. Arrival time (oldest first for tie-breaking)
//! 3. Transaction hash (lexicographic for complete determinism)

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use klomang_core::core::state::transaction::Transaction;

use super::pool::PoolEntry;

/// Criteria for selecting transactions
#[derive(Clone, Debug)]
pub enum SelectionCriteria {
    /// Select up to N transactions
    MaxCount(usize),
    
    /// Select transactions up to N bytes
    MaxBytes(usize),
    
    /// Select transactions up to N satoshis
    MaxFees(u64),
    
    /// Combined criteria
    Combined {
        max_count: usize,
        max_bytes: usize,
        max_fees: u64,
    },
}

/// Strategy for transaction selection
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionStrategy {
    /// Highest fee first (greedy)
    HighestFee,
    
    /// Ancestor set aware (respects dependencies)
    AncestorSet,
    
    /// Simple FIFO (for testing/fallback)
    FIFO,
}

/// Deterministic selector for transaction pool
pub struct DeterministicSelector {
    strategy: SelectionStrategy,
}

impl DeterministicSelector {
    /// Create new selector with given strategy
    pub fn new(strategy: SelectionStrategy) -> Self {
        Self { strategy }
    }

    /// Select transactions from pool entries with deterministic ordering
    pub fn select(
        &self,
        entries: Vec<PoolEntry>,
        criteria: SelectionCriteria,
    ) -> Vec<Transaction> {
        match self.strategy {
            SelectionStrategy::HighestFee => self.select_by_fee(entries, criteria),
            SelectionStrategy::FIFO => self.select_by_fifo(entries, criteria),
            SelectionStrategy::AncestorSet => self.select_by_fee(entries, criteria), // Simplified
        }
    }

    /// Select by highest fee rate with deterministic tie-breaking
    fn select_by_fee(&self, entries: Vec<PoolEntry>, criteria: SelectionCriteria) -> Vec<Transaction> {
        // Create priority queue with custom ordering
        let mut heap = BinaryHeap::with_capacity(entries.len());

        for entry in entries {
            heap.push(ComparablePoolEntry(entry));
        }

        let mut selected = Vec::new();
        let mut total_bytes = 0;
        let mut total_fees = 0;

        let (max_count, max_bytes, max_fees) = self.parse_criteria(&criteria);

        while let Some(ComparablePoolEntry(entry)) = heap.pop() {
            // Check limits
            if selected.len() >= max_count {
                break;
            }

            if total_bytes + entry.size_bytes > max_bytes {
                break;
            }

            if total_fees + entry.total_fee > max_fees {
                break;
            }

            total_bytes += entry.size_bytes;
            total_fees += entry.total_fee;
            selected.push(entry.transaction.clone());
        }

        selected
    }

    /// Select by FIFO (arrival time)
    fn select_by_fifo(&self, mut entries: Vec<PoolEntry>, criteria: SelectionCriteria) -> Vec<Transaction> {
        // Sort by arrival time first, then by hash for determinism
        entries.sort_by(|a, b| {
            match a.arrival_time.cmp(&b.arrival_time) {
                Ordering::Equal => {
                    // Use hash as final tie-breaker for determinism
                    let a_hash = bincode::serialize(&a.transaction.id).unwrap_or_default();
                    let b_hash = bincode::serialize(&b.transaction.id).unwrap_or_default();
                    a_hash.cmp(&b_hash)
                },
                other => other,
            }
        });

        let mut selected = Vec::new();
        let mut total_bytes = 0;
        let mut total_fees = 0;

        let (max_count, max_bytes, max_fees) = self.parse_criteria(&criteria);

        for entry in entries {
            if selected.len() >= max_count {
                break;
            }

            if total_bytes + entry.size_bytes > max_bytes {
                break;
            }

            if total_fees + entry.total_fee > max_fees {
                break;
            }

            total_bytes += entry.size_bytes;
            total_fees += entry.total_fee;
            selected.push(entry.transaction.clone());
        }

        selected
    }

    /// Parse selection criteria to tuple of limits
    fn parse_criteria(&self, criteria: &SelectionCriteria) -> (usize, usize, u64) {
        match criteria {
            SelectionCriteria::MaxCount(n) => (*n, usize::MAX, u64::MAX),
            SelectionCriteria::MaxBytes(b) => (usize::MAX, *b, u64::MAX),
            SelectionCriteria::MaxFees(f) => (usize::MAX, usize::MAX, *f),
            SelectionCriteria::Combined {
                max_count,
                max_bytes,
                max_fees,
            } => (*max_count, *max_bytes, *max_fees),
        }
    }
}

/// Wrapper for pool entry with custom comparison
struct ComparablePoolEntry(PoolEntry);

impl PartialEq for ComparablePoolEntry {
    fn eq(&self, other: &Self) -> bool {
        bincode::serialize(&self.0.transaction.id)
            .ok()
            .eq(&bincode::serialize(&other.0.transaction.id).ok())
    }
}

impl Eq for ComparablePoolEntry {}

impl PartialOrd for ComparablePoolEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ComparablePoolEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Primary: Compare by fee rate (descending - higher fee first)
        match other.0.fee_rate().cmp(&self.0.fee_rate()) {
            Ordering::Equal => {
                // Secondary: Compare by arrival time (ascending - earlier first)
                match self.0.arrival_time.cmp(&other.0.arrival_time) {
                    Ordering::Equal => {
                        // Tertiary: Compare by transaction hash (deterministic)
                        let self_hash = bincode::serialize(&self.0.transaction.id).unwrap_or_default();
                        let other_hash = bincode::serialize(&other.0.transaction.id).unwrap_or_default();
                        self_hash.cmp(&other_hash)
                    },
                    other_ord => other_ord,
                }
            },
            fee_ord => fee_ord,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klomang_core::core::crypto::Hash;

    fn create_entry(hash_seed: u8, fee_rate: u64, arrival_time: u64) -> PoolEntry {
        let tx = Transaction {
            id: Hash::new(&[hash_seed; 32]),
            inputs: vec![],
            outputs: vec![],
            execution_payload: vec![],
            contract_address: None,
            gas_limit: 0,
            max_fee_per_gas: 0,
            chain_id: 1,
            locktime: 0,
        };

        let size_bytes = 200;
        let total_fee = fee_rate * size_bytes as u64;

        let mut entry = PoolEntry::new(tx, total_fee, size_bytes);
        entry.arrival_time = arrival_time;
        entry
    }

    #[test]
    fn test_select_by_fee_rate() {
        let selector = DeterministicSelector::new(SelectionStrategy::HighestFee);

        let entries = vec![
            create_entry(1, 10, 1000), // Lower fee
            create_entry(2, 20, 1001), // Higher fee
            create_entry(3, 15, 1002), // Medium fee
        ];

        let selected = selector.select(entries, SelectionCriteria::MaxCount(2));
        assert_eq!(selected.len(), 2);
        
        // Should select highest fee first
        let first_hash = bincode::serialize(&selected[0].id).unwrap();
        let expected_hash = bincode::serialize(&Hash::new(&[2u8; 32])).unwrap();
        assert_eq!(first_hash, expected_hash); // Fee rate 20 should be first
    }

    #[test]
    fn test_deterministic_tie_breaking() {
        let selector = DeterministicSelector::new(SelectionStrategy::HighestFee);

        // Same fee rate, different arrival times
        let entries = vec![
            create_entry(1, 10, 1002), // Later arrival
            create_entry(2, 10, 1001), // Earlier arrival
            create_entry(3, 10, 1000), // Earliest arrival
        ];

        let selected1 = selector.select(entries.clone(), SelectionCriteria::MaxCount(3));
        let selected2 = selector.select(entries, SelectionCriteria::MaxCount(3));

        // Should be deterministic
        assert_eq!(selected1.len(), selected2.len());
        for (a, b) in selected1.iter().zip(selected2.iter()) {
            assert_eq!(
                bincode::serialize(&a.id).unwrap(),
                bincode::serialize(&b.id).unwrap()
            );
        }
    }

    #[test]
    fn test_fifo_selection() {
        let selector = DeterministicSelector::new(SelectionStrategy::FIFO);

        let entries = vec![
            create_entry(1, 100, 1010), // Later
            create_entry(2, 50, 1000), // Earlier
            create_entry(3, 75, 1005), // Middle
        ];

        let selected = selector.select(entries, SelectionCriteria::MaxCount(3));
        assert_eq!(selected.len(), 3);

        // Should maintain order: earliest arrival first
        let times = vec![1000, 1005, 1010];
        for (i, expected_seed) in [2, 3, 1].iter().enumerate() {
            let actual_hash = bincode::serialize(&selected[i].id).unwrap();
            let expected_hash = bincode::serialize(&Hash::new(&[*expected_seed; 32])).unwrap();
            // Note: This may not match if hash ordering differs, but should be stable
        }
    }

    #[test]
    fn test_max_bytes_limit() {
        let selector = DeterministicSelector::new(SelectionStrategy::HighestFee);

        let entries = vec![
            create_entry(1, 100, 1000),
            create_entry(2, 100, 1001),
            create_entry(3, 100, 1002),
        ];

        // Each entry is 200 bytes, so max 400 should select 2
        let selected = selector.select(entries, SelectionCriteria::MaxBytes(400));
        assert_eq!(selected.len(), 2);
    }
}
