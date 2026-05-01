//! Multi-Queue Admission System
//!
//! Implements layered queuing system with priority-based admission
//! and dynamic rebalancing for anti-starvation protection.

use crate::storage::KvStore;
use dashmap::DashMap;
use klomang_core::core::state::transaction::Transaction;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Queue entry with metadata
#[derive(Clone, Debug)]
pub struct QueueEntry {
    /// The transaction
    pub transaction: Arc<Transaction>,
    /// Entry timestamp
    pub timestamp: u64,
    /// Fee rate
    pub fee_rate: u64,
    /// Is system transaction (governance/staking)
    pub is_system: bool,
}

/// High priority queue for urgent transactions
pub struct HighPriorityQueue {
    /// Transactions in queue
    queue: DashMap<Vec<u8>, QueueEntry>,
    /// Maximum queue size
    max_size: usize,
}

impl HighPriorityQueue {
    pub fn new(max_size: usize) -> Self {
        Self {
            queue: DashMap::new(),
            max_size,
        }
    }

    pub fn add(&self, tx: Arc<Transaction>) -> Result<(), String> {
        let tx_hash =
            bincode::serialize(&tx.id).map_err(|e| format!("Serialization error: {}", e))?;

        if self.queue.len() >= self.max_size {
            return Err("High priority queue full".to_string());
        }

        let fee_rate = Self::calculate_fee_rate(&tx);
        let is_system = Self::is_system_transaction(&tx);

        let entry = QueueEntry {
            transaction: tx,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            fee_rate,
            is_system,
        };

        self.queue.insert(tx_hash, entry);
        Ok(())
    }

    pub fn remove(&self, tx_hash: &[u8]) -> Option<QueueEntry> {
        self.queue.remove(tx_hash).map(|(_, v)| v)
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    fn calculate_fee_rate(tx: &Transaction) -> u64 {
        // Simplified fee rate calculation
        (tx.inputs.len() + tx.outputs.len()) as u64 * 10
    }

    fn is_system_transaction(_tx: &Transaction) -> bool {
        // Placeholder: check if transaction is system/governance
        // In real implementation, check specific input addresses or metadata
        false
    }
}

/// Standard priority queue
pub struct StandardQueue {
    queue: DashMap<Vec<u8>, QueueEntry>,
    max_size: usize,
}

impl StandardQueue {
    pub fn new(max_size: usize) -> Self {
        Self {
            queue: DashMap::new(),
            max_size,
        }
    }

    pub fn add(&self, tx: Arc<Transaction>) -> Result<(), String> {
        let tx_hash =
            bincode::serialize(&tx.id).map_err(|e| format!("Serialization error: {}", e))?;

        if self.queue.len() >= self.max_size {
            return Err("Standard queue full".to_string());
        }

        let fee_rate = HighPriorityQueue::calculate_fee_rate(&tx);

        let entry = QueueEntry {
            transaction: tx,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            fee_rate,
            is_system: false,
        };

        self.queue.insert(tx_hash, entry);
        Ok(())
    }

    pub fn remove(&self, tx_hash: &[u8]) -> Option<QueueEntry> {
        self.queue.remove(tx_hash).map(|(_, v)| v)
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }
}

/// Low priority queue with anti-starvation
pub struct LowPriorityQueue {
    queue: DashMap<Vec<u8>, QueueEntry>,
    max_size: usize,
    /// Maximum age before promotion (milliseconds)
    max_age_ms: u64,
}

impl LowPriorityQueue {
    pub fn new(max_size: usize, max_age_ms: u64) -> Self {
        Self {
            queue: DashMap::new(),
            max_size,
            max_age_ms,
        }
    }

    pub fn add(&self, tx: Arc<Transaction>) -> Result<(), String> {
        let tx_hash =
            bincode::serialize(&tx.id).map_err(|e| format!("Serialization error: {}", e))?;

        if self.queue.len() >= self.max_size {
            return Err("Low priority queue full".to_string());
        }

        let fee_rate = HighPriorityQueue::calculate_fee_rate(&tx);

        let entry = QueueEntry {
            transaction: tx,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            fee_rate,
            is_system: false,
        };

        self.queue.insert(tx_hash, entry);
        Ok(())
    }

    pub fn remove(&self, tx_hash: &[u8]) -> Option<QueueEntry> {
        self.queue.remove(tx_hash).map(|(_, v)| v)
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Get transactions eligible for promotion due to age
    pub fn get_promotion_candidates(&self) -> Vec<(Vec<u8>, QueueEntry)> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        self.queue
            .iter()
            .filter_map(|entry| {
                let age = now.saturating_sub(entry.timestamp);
                if age >= self.max_age_ms {
                    Some((entry.key().clone(), entry.value().clone()))
                } else {
                    None
                }
            })
            .collect()
    }
}

/// Multi-Queue Admission System
///
/// Manages layered admission queues with dynamic rebalancing
/// and persistent state recovery.
pub struct MultiQueueAdmissionSystem {
    /// High priority queue
    high_queue: Arc<HighPriorityQueue>,
    /// Standard priority queue
    standard_queue: Arc<StandardQueue>,
    /// Low priority queue
    low_queue: Arc<LowPriorityQueue>,
    /// KvStore for persistence
    _kv_store: Arc<KvStore>,
    /// Market average fee rate
    market_avg_fee: RwLock<u64>,
}

impl MultiQueueAdmissionSystem {
    /// Create new multi-queue system
    pub fn new(_kv_store: Arc<KvStore>) -> Self {
        Self {
            high_queue: Arc::new(HighPriorityQueue::new(1000)),
            standard_queue: Arc::new(StandardQueue::new(5000)),
            low_queue: Arc::new(LowPriorityQueue::new(10000, 300000)), // 5 minutes
            _kv_store,
            market_avg_fee: RwLock::new(100), // Default
        }
    }

    /// Admit transaction to appropriate queue
    pub fn admit_transaction(&self, tx: Arc<Transaction>) -> Result<(), String> {
        let fee_rate = HighPriorityQueue::calculate_fee_rate(&tx);
        let is_system = HighPriorityQueue::is_system_transaction(&tx);
        let market_avg = *self.market_avg_fee.read();

        if is_system || fee_rate > market_avg {
            self.high_queue.add(tx)?;
        } else if fee_rate >= market_avg / 2 {
            self.standard_queue.add(tx)?;
        } else {
            self.low_queue.add(tx)?;
        }

        Ok(())
    }

    /// Perform dynamic rebalancing (anti-starvation)
    pub fn rebalance_queues(&self) -> Result<(), String> {
        // Promote aged transactions from low to standard
        let candidates = self.low_queue.get_promotion_candidates();

        for (tx_hash, entry) in candidates {
            // Remove from low queue
            self.low_queue.remove(&tx_hash);

            // Add to standard queue
            self.standard_queue
                .add(Arc::clone(&entry.transaction))
                .map_err(|e| format!("Failed to promote transaction: {}", e))?;
        }

        Ok(())
    }

    /// Update market average fee
    pub fn update_market_avg(&self, new_avg: u64) {
        *self.market_avg_fee.write() = new_avg;
    }

    /// Persist queue metadata to storage
    pub fn persist_state(&self) -> Result<(), String> {
        // Persist queue sizes and metadata
        let metadata = QueueMetadata {
            high_queue_size: self.high_queue.len(),
            standard_queue_size: self.standard_queue.len(),
            low_queue_size: self.low_queue.len(),
            market_avg_fee: *self.market_avg_fee.read(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        };

        let _key = b"queue_metadata";
        let _value =
            bincode::serialize(&metadata).map_err(|e| format!("Serialization error: {}", e))?;

        // Use kv_store to persist (assuming it has a put method)
        // For now, placeholder - in real implementation, use appropriate storage method
        // self.kv_store.put(key, &value).map_err(|e| e.to_string())?;

        Ok(())
    }

    /// Restore queue state from storage
    pub fn restore_state(&self) -> Result<(), String> {
        // Restore metadata from storage
        // Placeholder implementation
        Ok(())
    }

    /// Get queue statistics
    pub fn get_stats(&self) -> QueueStats {
        QueueStats {
            high_queue_size: self.high_queue.len(),
            standard_queue_size: self.standard_queue.len(),
            low_queue_size: self.low_queue.len(),
            market_avg_fee: *self.market_avg_fee.read(),
        }
    }
}

/// Queue metadata for persistence
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct QueueMetadata {
    pub high_queue_size: usize,
    pub standard_queue_size: usize,
    pub low_queue_size: usize,
    pub market_avg_fee: u64,
    pub timestamp: u64,
}

/// Queue statistics
#[derive(Clone, Debug)]
pub struct QueueStats {
    pub high_queue_size: usize,
    pub standard_queue_size: usize,
    pub low_queue_size: usize,
    pub market_avg_fee: u64,
}
