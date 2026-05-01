//! Lock-Free Read Access Layer dengan Copy-On-Write (COW) Pattern
//!
//! Implementasi snapshot-based read access yang memungkinkan readers
//! untuk membaca data consistently tanpa pernah diblokir oleh writers.
//! Menggunakan arc-swap untuk atomic pointer swaps dengan membaca konsisten.

use arc_swap::ArcSwap;
use parking_lot::RwLock;
use std::sync::Arc;

use klomang_core::core::state::transaction::Transaction;

use super::parallel_transaction_index::{IndexedTransactionEntry, IndexedTransactionStatus};

/// Snapshot dari mempool pada saat tertentu.
/// Memberikan view konsisten dari state mempool tanpa mengunci operasi write.
#[derive(Clone, Debug)]
pub struct MempoolSnapshot {
    /// Snapshot dari transactions (Vec untuk ordering consistency)
    pub transactions: Vec<(Vec<u8>, Arc<Transaction>)>,
    /// Metadata transactions
    pub metadata: Vec<(Vec<u8>, IndexedTransactionEntry)>,
    /// Timestamp saat snapshot dibuat (nanoseconds)
    pub snapshot_time_ns: u64,
    /// Statistics pada waktu snapshot
    pub pending_count: usize,
    pub validated_count: usize,
    pub invalid_count: usize,
    pub total_transactions: usize,
}

impl MempoolSnapshot {
    /// Get transaksi dari snapshot by hash
    pub fn get_transaction(&self, tx_hash: &[u8]) -> Option<Arc<Transaction>> {
        self.transactions
            .iter()
            .find(|(hash, _)| hash == tx_hash)
            .map(|(_, tx)| Arc::clone(tx))
    }

    /// Get top N transactions by fee rate
    pub fn get_top_by_fee(&self, limit: usize) -> Vec<Arc<Transaction>> {
        let mut entries: Vec<_> = self
            .metadata
            .iter()
            .map(|(_, entry)| (entry.fee_rate(), entry.clone()))
            .collect();

        entries.sort_by(|a, b| b.0.cmp(&a.0));

        entries
            .into_iter()
            .take(limit)
            .filter_map(|(_, entry)| {
                self.transactions
                    .iter()
                    .find(|(hash, _)| {
                        bincode::serialize(&entry.transaction.id)
                            .map(|h| h == *hash)
                            .unwrap_or(false)
                    })
                    .map(|(_, tx)| Arc::clone(tx))
            })
            .collect()
    }

    /// Filter transactions oleh status
    pub fn filter_by_status(&self, status: IndexedTransactionStatus) -> Vec<Arc<Transaction>> {
        self.metadata
            .iter()
            .filter(|(_, entry)| entry.status == status)
            .filter_map(|(_, entry)| {
                self.transactions
                    .iter()
                    .find(|(_, tx)| {
                        bincode::serialize(&tx.id)
                            .map(|h| {
                                bincode::serialize(&entry.transaction.id)
                                    .map(|eh| h == eh)
                                    .unwrap_or(false)
                            })
                            .unwrap_or(false)
                    })
                    .map(|(_, tx)| Arc::clone(tx))
            })
            .collect()
    }

    /// Umur snapshot dalam nanoseconds
    pub fn age_ns(&self) -> u64 {
        let now_ns = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()) as u64;
        now_ns.saturating_sub(self.snapshot_time_ns)
    }
}

/// Configuration untuk lock-free read layer
#[derive(Clone, Debug)]
pub struct LockFreeReadConfig {
    /// Maximum age dari snapshot sebelum force refresh (nanoseconds)
    pub max_snapshot_age_ns: u64,
    /// Maximum number dari snapshots to keep in history
    pub max_history_snapshots: usize,
    /// Enable periodic snapshot updates
    pub enable_periodic_snapshots: bool,
}

impl Default for LockFreeReadConfig {
    fn default() -> Self {
        Self {
            max_snapshot_age_ns: 1_000_000_000, // 1 second
            max_history_snapshots: 10,
            enable_periodic_snapshots: true,
        }
    }
}

/// Lock-Free Read Access Layer
///
/// Fitur:
/// - Readers tidak pernah diblokir oleh writers
/// - Atomic pointer swaps untuk consistent snapshots
/// - Optional history untuk rollback atau analysis
/// - Periodic snapshot refresh untuk freshness
pub struct LockFreeReadLayer {
    /// Current snapshot, wrapped in ArcSwap untuk lock-free reads
    current_snapshot: ArcSwap<MempoolSnapshot>,
    /// History dari snapshots (optional)
    snapshot_history: Arc<RwLock<Vec<Arc<MempoolSnapshot>>>>,
    /// Configuration
    config: LockFreeReadConfig,
    /// Last snapshot update time
    last_update_ns: Arc<parking_lot::Mutex<u64>>,
}

impl LockFreeReadLayer {
    /// Buat lock-free read layer
    pub fn new(config: LockFreeReadConfig) -> Self {
        let initial_snapshot = Arc::new(MempoolSnapshot {
            transactions: vec![],
            metadata: vec![],
            snapshot_time_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64,
            pending_count: 0,
            validated_count: 0,
            invalid_count: 0,
            total_transactions: 0,
        });

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        Self {
            current_snapshot: ArcSwap::new(initial_snapshot),
            snapshot_history: Arc::new(RwLock::new(Vec::new())),
            config,
            last_update_ns: Arc::new(parking_lot::Mutex::new(now)),
        }
    }

    /// Get current snapshot without blocking
    /// Lock-free operation menggunakan ArcSwap internal optimizations
    pub fn get_snapshot(&self) -> Arc<MempoolSnapshot> {
        // Load atomic pointer - tidak ada locking sama sekali
        self.current_snapshot.load_full()
    }

    /// Update snapshot dengan implementasi COW
    /// Hanya write path yang diblokir singkat, reads tetap lock-free
    pub fn update_snapshot(
        &self,
        transactions: Vec<(Vec<u8>, Arc<Transaction>)>,
        metadata: Vec<(Vec<u8>, IndexedTransactionEntry)>,
    ) -> Arc<MempoolSnapshot> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        // Calculate statistics dari metadata
        let mut pending_count = 0;
        let mut validated_count = 0;
        let mut invalid_count = 0;

        for (_, entry) in &metadata {
            match entry.status {
                IndexedTransactionStatus::Pending => pending_count += 1,
                IndexedTransactionStatus::Validated => validated_count += 1,
                IndexedTransactionStatus::Invalid => invalid_count += 1,
                IndexedTransactionStatus::Confirmed => {}
            }
        }

        let snapshot = Arc::new(MempoolSnapshot {
            transactions,
            metadata,
            snapshot_time_ns: now,
            pending_count,
            validated_count,
            invalid_count,
            total_transactions: pending_count + validated_count + invalid_count,
        });

        // Atomic swap - very brief lock-free operation
        let old_snapshot = self.current_snapshot.swap(Arc::clone(&snapshot));

        // Update history (dengan minimal blocking)
        if self.config.enable_periodic_snapshots {
            let mut history = self.snapshot_history.write();
            history.push(old_snapshot);

            // Keep only recent N snapshots to avoid memory bloat
            while history.len() > self.config.max_history_snapshots {
                history.remove(0);
            }
        }

        // Update last_update_ns
        *self.last_update_ns.lock() = now;

        snapshot
    }

    /// Check apakah snapshot perlu di-refresh
    pub fn needs_refresh(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        let last_update = *self.last_update_ns.lock();
        now.saturating_sub(last_update) > self.config.max_snapshot_age_ns
    }

    /// Get snapshot age dalam nanoseconds
    pub fn snapshot_age_ns(&self) -> u64 {
        let snapshot = self.get_snapshot();
        snapshot.age_ns()
    }

    /// Get snapshot history (immutable view)
    pub fn get_history(&self) -> Vec<Arc<MempoolSnapshot>> {
        self.snapshot_history.read().clone()
    }

    /// Clear history untuk save memory
    pub fn clear_history(&self) {
        self.snapshot_history.write().clear();
    }

    /// Rollback ke snapshot tertentu dari history
    pub fn rollback_to_history_entry(&self, index: usize) -> Result<Arc<MempoolSnapshot>, String> {
        let history = self.snapshot_history.read();

        if index >= history.len() {
            return Err("History index out of bounds".to_string());
        }

        let snapshot = Arc::clone(&history[index]);
        drop(history); // Explicit drop untuk release lock

        // Swap to previous snapshot
        self.current_snapshot.swap(snapshot.clone());

        Ok(snapshot)
    }

    /// Get statistics dari current snapshot
    pub fn get_current_stats(&self) -> (usize, usize, usize) {
        let snapshot = self.get_snapshot();
        (
            snapshot.pending_count,
            snapshot.validated_count,
            snapshot.invalid_count,
        )
    }

    /// Verify integrity dari snapshot
    pub fn verify_snapshot_consistency(&self) -> Result<(), String> {
        let snapshot = self.get_snapshot();

        if snapshot.transactions.len() != snapshot.metadata.len() {
            return Err("Transaction count mismatch between data and metadata".to_string());
        }

        let total_by_status =
            snapshot.pending_count + snapshot.validated_count + snapshot.invalid_count;

        if total_by_status != snapshot.total_transactions {
            return Err("Status count mismatch".to_string());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    fn create_test_snapshot() -> MempoolSnapshot {
        MempoolSnapshot {
            transactions: vec![],
            metadata: vec![],
            snapshot_time_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64,
            pending_count: 0,
            validated_count: 0,
            invalid_count: 0,
            total_transactions: 0,
        }
    }

    #[test]
    fn test_lock_free_read() {
        let layer = LockFreeReadLayer::new(LockFreeReadConfig::default());

        let snapshot1 = layer.get_snapshot();
        let snapshot2 = layer.get_snapshot();

        // Should be same snapshot instance
        assert_eq!(snapshot1.snapshot_time_ns, snapshot2.snapshot_time_ns);
    }

    #[test]
    fn test_snapshot_update() {
        let layer = LockFreeReadLayer::new(LockFreeReadConfig::default());

        let snapshot = layer.update_snapshot(vec![], vec![]);
        assert_eq!(snapshot.total_transactions, 0);

        let current = layer.get_snapshot();
        assert_eq!(current.total_transactions, 0);
    }

    #[test]
    fn test_needs_refresh() {
        let config = LockFreeReadConfig {
            max_snapshot_age_ns: 1000,
            ..Default::default()
        };
        let layer = LockFreeReadLayer::new(config);

        assert!(!layer.needs_refresh()); // Just created

        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(layer.needs_refresh()); // Should be old now
    }

    #[test]
    fn test_snapshot_history() {
        let layer = LockFreeReadLayer::new(LockFreeReadConfig::default());

        layer.update_snapshot(vec![], vec![]);
        layer.update_snapshot(vec![], vec![]);
        layer.update_snapshot(vec![], vec![]);

        let history = layer.get_history();
        assert!(history.len() > 0);
    }

    #[test]
    fn test_verify_consistency() {
        let layer = LockFreeReadLayer::new(LockFreeReadConfig::default());
        layer.update_snapshot(vec![], vec![]);

        assert!(layer.verify_snapshot_consistency().is_ok());
    }
}
