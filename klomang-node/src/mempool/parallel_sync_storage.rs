//! Parallel Storage Synchronization
//!
//! Menghubungkan mempool dengan klomang-node storage untuk automatic cleanup
//! ketika transaksi sudah dikonfirmasi dalam blockchain.

use parking_lot::RwLock;
use std::sync::Arc;

use crate::storage::kv_store::KvStore;

use super::parallel_transaction_index::{IndexedTransactionStatus, ParallelTransactionIndex};

/// Status sinkronisasi antara mempool dan storage
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SyncStatus {
    /// Belum disinkronkan
    Unsynced,
    /// Sedang disinkronkan
    Syncing,
    /// Sudah disinkronkan
    Synced,
    /// Ada error dalam sinkronisasi
    Error,
}

/// Metadata untuk tracking sinkronisasi
#[derive(Clone, Debug)]
pub struct SyncMetadata {
    /// Status sinkronisasi
    pub status: SyncStatus,
    /// Last sync time (nanoseconds)
    pub last_sync_ns: u64,
    /// Transactions yang sudah diremove dari index karena ada di storage
    pub cleaned_count: u64,
    /// Transactions yang masih pending
    pub pending_count: u64,
}

impl Default for SyncMetadata {
    fn default() -> Self {
        Self {
            status: SyncStatus::Unsynced,
            last_sync_ns: 0,
            cleaned_count: 0,
            pending_count: 0,
        }
    }
}

/// Configuration untuk storage synchronization
#[derive(Clone, Debug)]
pub struct StorageSyncConfig {
    /// Enable automatic cleanup dari confirmed transactions
    pub enable_auto_cleanup: bool,
    /// Interval untuk periodic sync (nanoseconds)
    pub sync_interval_ns: u64,
    /// Maximum transactions to cleanup per sync
    pub max_cleanup_per_sync: usize,
    /// Enable logging untuk debug
    pub enable_debug_logging: bool,
}

impl Default for StorageSyncConfig {
    fn default() -> Self {
        Self {
            enable_auto_cleanup: true,
            sync_interval_ns: 5_000_000_000, // 5 seconds
            max_cleanup_per_sync: 1000,
            enable_debug_logging: false,
        }
    }
}

/// Storage Integration Result
#[derive(Clone, Debug)]
pub struct StorageSyncResult {
    /// Number of transactions cleaned
    pub cleaned: usize,
    /// Number of transactions still in mempool
    pub remaining: usize,
    /// Transactions yang sudah confirmed
    pub confirmed_tx_hashes: Vec<Vec<u8>>,
    /// Transactions yang not found in storage
    pub not_found: Vec<Vec<u8>>,
    /// Metadata update
    pub metadata: SyncMetadata,
}

/// Synchronize mempool dengan storage
///
/// Fungsi:
/// - Detect transaksi yang sudah confirmed dalam storage
/// - Automatic cleanup dari confirmed transactions
/// - Maintain consistency antara mempool dan storage
pub struct StorageSyncManager {
    /// Reference ke parallel index
    index: Arc<ParallelTransactionIndex>,
    /// Reference ke KvStore
    kv_store: Option<Arc<KvStore>>,
    /// Configuration
    config: StorageSyncConfig,
    /// Tracking metadata
    metadata: Arc<RwLock<SyncMetadata>>,
    /// Last sync time
    last_sync_ns: Arc<parking_lot::Mutex<u64>>,
}

impl StorageSyncManager {
    /// Buat storage sync manager
    pub fn new(
        index: Arc<ParallelTransactionIndex>,
        kv_store: Option<Arc<KvStore>>,
        config: StorageSyncConfig,
    ) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        Self {
            index,
            kv_store,
            config,
            metadata: Arc::new(RwLock::new(SyncMetadata::default())),
            last_sync_ns: Arc::new(parking_lot::Mutex::new(now)),
        }
    }

    /// Check apakah sync diperlukan
    pub fn needs_sync(&self) -> bool {
        if !self.config.enable_auto_cleanup {
            return false;
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        let last_sync = *self.last_sync_ns.lock();
        now.saturating_sub(last_sync) > self.config.sync_interval_ns
    }

    /// Sync transaksi dari index dengan storage
    /// Otomatis remove transaksi yang sudah ada di storage
    pub fn sync_with_storage(&self) -> Result<StorageSyncResult, String> {
        // Set status ke Syncing
        {
            let mut metadata = self.metadata.write();
            metadata.status = SyncStatus::Syncing;
        }

        let result = match &self.kv_store {
            Some(kv_store) => self._do_sync_with_storage(kv_store),
            None => {
                // No storage backend, just update metadata
                Ok(StorageSyncResult {
                    cleaned: 0,
                    remaining: self.index.len(),
                    confirmed_tx_hashes: vec![],
                    not_found: vec![],
                    metadata: SyncMetadata {
                        status: SyncStatus::Synced,
                        last_sync_ns: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos() as u64,
                        cleaned_count: 0,
                        pending_count: self.index.len() as u64,
                    },
                })
            }
        };

        // Update metadata berdasarkan result
        if let Ok(sync_result) = &result {
            let mut metadata = self.metadata.write();
            metadata.status = SyncStatus::Synced;
            metadata.last_sync_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            metadata.cleaned_count = metadata
                .cleaned_count
                .saturating_add(sync_result.cleaned as u64);
            metadata.pending_count = sync_result.remaining as u64;
        } else {
            let mut metadata = self.metadata.write();
            metadata.status = SyncStatus::Error;
        }

        *self.last_sync_ns.lock() = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        result
    }

    fn _do_sync_with_storage(&self, kv_store: &Arc<KvStore>) -> Result<StorageSyncResult, String> {
        let mut confirmed_tx_hashes = vec![];
        let mut not_found = vec![];
        let mut cleaned_count = 0;

        // Get all pending transactions dari index
        let pending_txs = self.index.get_by_status(IndexedTransactionStatus::Pending);

        // Limit cleanup to avoid too much work at once
        let to_check = pending_txs.iter().take(self.config.max_cleanup_per_sync);

        for tx in to_check {
            let tx_hash =
                bincode::serialize(&tx.id).map_err(|e| format!("Serialization error: {}", e))?;

            // Check apakah transaksi sudah ada dalam storage
            match kv_store.get_mempool_transaction(&tx_hash) {
                Some(_) => {
                    // Transaksi ditemukan di storage, hapus dari index
                    confirmed_tx_hashes.push(tx_hash.clone());

                    // Remove dari index
                    if let Ok(_) = self.index.remove(&tx_hash) {
                        cleaned_count += 1;
                    }
                }
                None => {
                    // Transaction not in storage, keep in mempool
                    not_found.push(tx_hash);
                }
            }
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        let remaining = self.index.len();

        Ok(StorageSyncResult {
            cleaned: cleaned_count,
            remaining,
            confirmed_tx_hashes,
            not_found,
            metadata: SyncMetadata {
                status: SyncStatus::Synced,
                last_sync_ns: now,
                cleaned_count: cleaned_count as u64,
                pending_count: remaining as u64,
            },
        })
    }

    /// Notify storage manager bahwa transaksi included dalam block
    pub fn notify_transaction_confirmed(&self, tx_hash: &[u8]) -> Result<(), String> {
        // Update status ke Confirmed dalam index
        self.index
            .update_status(tx_hash, IndexedTransactionStatus::Confirmed)?;

        // Remove dari index
        let _ = self.index.remove(tx_hash); // Ignore errors if already removed

        Ok(())
    }

    /// Batch notify multiple transactions confirm
    pub fn notify_transactions_confirmed(&self, tx_hashes: &[Vec<u8>]) -> Result<usize, String> {
        let mut confirmed_count = 0;

        for tx_hash in tx_hashes {
            if let Ok(_) = self.notify_transaction_confirmed(tx_hash) {
                confirmed_count += 1;
            }
        }

        Ok(confirmed_count)
    }

    /// Get current sync metadata
    pub fn get_metadata(&self) -> SyncMetadata {
        self.metadata.read().clone()
    }

    /// Get count dari cleaned transactions
    pub fn get_cleaned_count(&self) -> u64 {
        self.metadata.read().cleaned_count
    }

    /// Reset sync metadata
    pub fn reset(&self) {
        let mut metadata = self.metadata.write();
        metadata.status = SyncStatus::Unsynced;
        metadata.cleaned_count = 0;
        metadata.pending_count = self.index.len() as u64;
        metadata.last_sync_ns = 0;
    }

    /// Verify synchronization integrity
    pub fn verify_sync_consistency(&self) -> Result<(), String> {
        let metadata = self.metadata.read();

        if metadata.status == SyncStatus::Syncing {
            return Err("Sync already in progress".to_string());
        }

        let index_stats = self.index.get_stats();

        if index_stats.total_transactions as u64 != metadata.pending_count {
            return Err("Index transaction count mismatch".to_string());
        }

        Ok(())
    }

    /// Get comprehensive status report
    pub fn get_status_report(&self) -> StatusReport {
        let metadata = self.metadata.read();
        let index_stats = self.index.get_stats();

        StatusReport {
            sync_status: metadata.status,
            mempool_total: index_stats.total_transactions,
            mempool_pending: index_stats.pending_count,
            mempool_validated: index_stats.validated_count,
            mempool_invalid: index_stats.invalid_count,
            total_cleaned: metadata.cleaned_count,
            last_sync_ns: metadata.last_sync_ns,
        }
    }
}

/// Comprehensive status report
#[derive(Clone, Debug)]
pub struct StatusReport {
    pub sync_status: SyncStatus,
    pub mempool_total: usize,
    pub mempool_pending: usize,
    pub mempool_validated: usize,
    pub mempool_invalid: usize,
    pub total_cleaned: u64,
    pub last_sync_ns: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_sync_without_kv_store() {
        let index = Arc::new(ParallelTransactionIndex::new(Default::default()));
        let manager = StorageSyncManager::new(index, None, StorageSyncConfig::default());

        let result = manager.sync_with_storage();
        assert!(result.is_ok());

        let sync_result = result.unwrap();
        assert_eq!(sync_result.cleaned, 0);
        assert_eq!(sync_result.remaining, 0);
    }

    #[test]
    fn test_needs_sync() {
        let index = Arc::new(ParallelTransactionIndex::new(Default::default()));
        let config = StorageSyncConfig {
            sync_interval_ns: 1000,
            ..Default::default()
        };
        let manager = StorageSyncManager::new(index, None, config);

        assert!(!manager.needs_sync()); // Just created
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(manager.needs_sync());
    }

    #[test]
    fn test_notify_confirmed() {
        let index = Arc::new(ParallelTransactionIndex::new(Default::default()));
        let manager = StorageSyncManager::new(index, None, StorageSyncConfig::default());

        let tx_hash = vec![1, 2, 3, 4];
        let result = manager.notify_transaction_confirmed(&tx_hash);

        // Should fail because tx not in index
        assert!(result.is_err());
    }

    #[test]
    fn test_metadata_tracking() {
        let index = Arc::new(ParallelTransactionIndex::new(Default::default()));
        let manager = StorageSyncManager::new(index, None, StorageSyncConfig::default());

        let metadata = manager.get_metadata();
        assert_eq!(metadata.status, SyncStatus::Unsynced);
        assert_eq!(metadata.cleaned_count, 0);
    }

    #[test]
    fn test_sync_consistency_verification() {
        let index = Arc::new(ParallelTransactionIndex::new(Default::default()));
        let manager = StorageSyncManager::new(index, None, StorageSyncConfig::default());

        assert!(manager.verify_sync_consistency().is_ok());
    }
}
