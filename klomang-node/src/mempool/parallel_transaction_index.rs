//! High-Concurrency Transaction Index using DashMap
//!
//! Implementasi indeks transaksi dengan concurrent access tinggi menggunakan DashMap.
//! DashMap menggunakan sharded locking internal untuk mengurangi lock contention.
//! Setiap transaksi dibungkus dalam Arc untuk memungkinkan zero-copy read sharing
//! across multiple threads.

use dashmap::DashMap;
use parking_lot::RwLock;
use std::sync::Arc;

use klomang_core::core::state::transaction::Transaction;

/// Configuration untuk parallel transaction index
#[derive(Clone, Debug)]
pub struct ParallelIndexConfig {
    /// Jumlah internal shards dalam DashMap (power of 2 recommended)
    pub num_shards: usize,
    /// Maximum transactions yang boleh disimpan
    pub max_transactions: usize,
    /// Enable automatic cleanup dari transaksi lama
    pub enable_cleanup: bool,
}

impl Default for ParallelIndexConfig {
    fn default() -> Self {
        Self {
            num_shards: 16,
            max_transactions: 100_000,
            enable_cleanup: true,
        }
    }
}

/// Status dari transaksi dalam index
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IndexedTransactionStatus {
    /// Transaksi baru, sedang divalidasi
    Pending,
    /// Transaksi sudah divalidasi
    Validated,
    /// Transaksi invalid
    Invalid,
    /// Transaksi sudah dimasukkan ke block
    Confirmed,
}

/// Metadata transaksi dalam parallel index
#[derive(Clone, Debug)]
pub struct IndexedTransactionEntry {
    /// Arc-wrapped transaction untuk zero-copy sharing
    pub transaction: Arc<Transaction>,
    /// Status transaksi
    pub status: IndexedTransactionStatus,
    /// Timestamp saat pertama kali ditambahkan (nanoseconds)
    pub insertion_time_ns: u64,
    /// Size dalam bytes
    pub size_bytes: usize,
    /// Total fee dalam satoshis
    pub total_fee: u64,
}

impl IndexedTransactionEntry {
    /// Buat entry baru
    pub fn new(tx: Transaction, fee: u64, size: usize) -> Self {
        Self {
            transaction: Arc::new(tx),
            status: IndexedTransactionStatus::Pending,
            insertion_time_ns: (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()) as u64,
            size_bytes: size,
            total_fee: fee,
        }
    }

    /// Hitung fee rate (satoshi per byte)
    pub fn fee_rate(&self) -> u64 {
        if self.size_bytes == 0 {
            0
        } else {
            self.total_fee / self.size_bytes as u64
        }
    }

    /// Hitung umur transaksi dalam nanoseconds
    pub fn age_ns(&self) -> u64 {
        let now_ns = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()) as u64;
        now_ns.saturating_sub(self.insertion_time_ns)
    }
}

/// Statistics untuk parallel transaction index
#[derive(Clone, Debug, Default)]
pub struct IndexStats {
    /// Total transaksi dalam index
    pub total_transactions: usize,
    /// Jumlah transaksi pending
    pub pending_count: usize,
    /// Jumlah transaksi validated
    pub validated_count: usize,
    /// Jumlah transaksi invalid
    pub invalid_count: usize,
    /// Jumlah transaksi confirmed
    pub confirmed_count: usize,
    /// Total memory digunakan (bytes)
    pub total_memory_bytes: usize,
}

/// High-concurrency transaction index menggunakan DashMap
///
/// Fitur:
/// - Sharded locking untuk concurrent reads/writes
/// - Arc-wrapped transactions untuk zero-copy sharing
/// - O(1) lookup dan update operations
/// - Tidak perlu explicit locking untuk read operations
pub struct ParallelTransactionIndex {
    /// DashMap untuk O(1) concurrent access
    transactions: DashMap<Vec<u8>, IndexedTransactionEntry>,
    /// Configuration
    config: ParallelIndexConfig,
    /// Lock untuk statistics (minimal contention)
    stats: Arc<RwLock<IndexStats>>,
}

impl ParallelTransactionIndex {
    /// Buat index baru
    pub fn new(config: ParallelIndexConfig) -> Self {
        let transactions = DashMap::with_capacity(config.max_transactions / 4);

        Self {
            transactions,
            config,
            stats: Arc::new(RwLock::new(IndexStats::default())),
        }
    }

    /// Tambahkan transaksi ke index
    pub fn insert(
        &self,
        tx_hash: Vec<u8>,
        tx: Transaction,
        fee: u64,
        size_bytes: usize,
    ) -> Result<(), String> {
        // Check size limit
        if self.transactions.len() >= self.config.max_transactions {
            return Err("Transaction index is full".to_string());
        }

        let entry = IndexedTransactionEntry::new(tx, fee, size_bytes);
        self.transactions.insert(tx_hash, entry.clone());

        // Update statistics
        let mut stats = self.stats.write();
        stats.total_transactions = self.transactions.len();
        stats.pending_count += 1;
        stats.total_memory_bytes += size_bytes + 64; // overhead

        Ok(())
    }

    /// Get transaksi dari index (zero-copy dengan Arc)
    pub fn get(&self, tx_hash: &[u8]) -> Option<Arc<Transaction>> {
        self.transactions
            .get(tx_hash)
            .map(|entry| Arc::clone(&entry.transaction))
    }

    /// Get lengkap entry termasuk metadata
    pub fn get_entry(&self, tx_hash: &[u8]) -> Option<IndexedTransactionEntry> {
        self.transactions.get(tx_hash).map(|entry| entry.clone())
    }

    /// Update status transaksi
    pub fn update_status(
        &self,
        tx_hash: &[u8],
        new_status: IndexedTransactionStatus,
    ) -> Result<(), String> {
        if let Some(mut entry) = self.transactions.get_mut(tx_hash) {
            let old_status = entry.status;
            entry.status = new_status;

            // Update statistics
            let mut stats = self.stats.write();
            match old_status {
                IndexedTransactionStatus::Pending => {
                    stats.pending_count = stats.pending_count.saturating_sub(1)
                }
                IndexedTransactionStatus::Validated => {
                    stats.validated_count = stats.validated_count.saturating_sub(1)
                }
                IndexedTransactionStatus::Invalid => {
                    stats.invalid_count = stats.invalid_count.saturating_sub(1)
                }
                IndexedTransactionStatus::Confirmed => {
                    stats.confirmed_count = stats.confirmed_count.saturating_sub(1)
                }
            }

            match new_status {
                IndexedTransactionStatus::Pending => stats.pending_count += 1,
                IndexedTransactionStatus::Validated => stats.validated_count += 1,
                IndexedTransactionStatus::Invalid => stats.invalid_count += 1,
                IndexedTransactionStatus::Confirmed => stats.confirmed_count += 1,
            }

            Ok(())
        } else {
            Err("Transaction not found in index".to_string())
        }
    }

    /// Remove transaksi dari index (e.g., when included in block)
    pub fn remove(&self, tx_hash: &[u8]) -> Result<IndexedTransactionEntry, String> {
        if let Some((_, entry)) = self.transactions.remove(tx_hash) {
            let mut stats = self.stats.write();
            stats.total_transactions = self.transactions.len();

            match entry.status {
                IndexedTransactionStatus::Pending => {
                    stats.pending_count = stats.pending_count.saturating_sub(1)
                }
                IndexedTransactionStatus::Validated => {
                    stats.validated_count = stats.validated_count.saturating_sub(1)
                }
                IndexedTransactionStatus::Invalid => {
                    stats.invalid_count = stats.invalid_count.saturating_sub(1)
                }
                IndexedTransactionStatus::Confirmed => {
                    stats.confirmed_count = stats.confirmed_count.saturating_sub(1)
                }
            }
            stats.total_memory_bytes = stats
                .total_memory_bytes
                .saturating_sub(entry.size_bytes + 64);

            Ok(entry)
        } else {
            Err("Transaction not found in index".to_string())
        }
    }

    /// Get semua transaksi dengan status tertentu
    pub fn get_by_status(&self, status: IndexedTransactionStatus) -> Vec<Arc<Transaction>> {
        self.transactions
            .iter()
            .filter(|entry| entry.value().status == status)
            .map(|entry| Arc::clone(&entry.value().transaction))
            .collect()
    }

    /// Get count transaksi dengan status tertentu
    pub fn count_by_status(&self, status: IndexedTransactionStatus) -> usize {
        self.transactions
            .iter()
            .filter(|entry| entry.value().status == status)
            .count()
    }

    /// Check apakah transaksi ada dalam index
    pub fn contains(&self, tx_hash: &[u8]) -> bool {
        self.transactions.contains_key(tx_hash)
    }

    /// Get jumlah total transaksi
    pub fn len(&self) -> usize {
        self.transactions.len()
    }

    /// Check apakah index kosong
    pub fn is_empty(&self) -> bool {
        self.transactions.is_empty()
    }

    /// Clear semua transaksi dari index
    pub fn clear(&self) {
        self.transactions.clear();
        let mut stats = self.stats.write();
        *stats = IndexStats::default();
    }

    /// Get current statistics
    pub fn get_stats(&self) -> IndexStats {
        let stats = self.stats.read();
        IndexStats {
            total_transactions: self.transactions.len(),
            pending_count: stats.pending_count,
            validated_count: stats.validated_count,
            invalid_count: stats.invalid_count,
            confirmed_count: stats.confirmed_count,
            total_memory_bytes: stats.total_memory_bytes,
        }
    }

    /// Get transactions ordered by fee rate (highest first) - snapshot-based
    pub fn get_top_by_fee_rate(&self, limit: usize) -> Vec<Arc<Transaction>> {
        let mut entries: Vec<_> = self
            .transactions
            .iter()
            .map(|ref_multi| {
                let entry = ref_multi.value().clone();
                (entry.fee_rate(), Arc::clone(&entry.transaction))
            })
            .collect();

        entries.sort_by(|a, b| b.0.cmp(&a.0)); // Higher fee rate first
        entries.into_iter().take(limit).map(|(_, tx)| tx).collect()
    }

    /// Cleanup transaksi lama (berdasarkan age)
    pub fn cleanup_old_transactions(&self, max_age_ns: u64) -> usize {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        let to_remove: Vec<Vec<u8>> = self
            .transactions
            .iter()
            .filter(|entry| {
                let entry_time = entry.value().insertion_time_ns;
                now.saturating_sub(entry_time) > max_age_ns
            })
            .map(|entry| entry.key().clone())
            .collect();

        let mut removed_count = 0;
        for tx_hash in to_remove {
            if let Ok(_) = self.remove(&tx_hash) {
                removed_count += 1;
            }
        }

        removed_count
    }

    /// Verify consistency - semua entries valid
    pub fn verify_consistency(&self) -> Result<(), String> {
        let stats = self.stats.read();

        let actual_pending: usize = self
            .transactions
            .iter()
            .filter(|e| e.value().status == IndexedTransactionStatus::Pending)
            .count();

        let actual_validated: usize = self
            .transactions
            .iter()
            .filter(|e| e.value().status == IndexedTransactionStatus::Validated)
            .count();

        let actual_invalid: usize = self
            .transactions
            .iter()
            .filter(|e| e.value().status == IndexedTransactionStatus::Invalid)
            .count();

        let actual_confirmed: usize = self
            .transactions
            .iter()
            .filter(|e| e.value().status == IndexedTransactionStatus::Confirmed)
            .count();

        if actual_pending != stats.pending_count
            || actual_validated != stats.validated_count
            || actual_invalid != stats.invalid_count
            || actual_confirmed != stats.confirmed_count
        {
            return Err("Index statistics mismatch".to_string());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_transaction() -> Transaction {
        Transaction {
            id: klomang_core::core::crypto::Hash::new(&[1, 2, 3, 4]),
            inputs: vec![],
            outputs: vec![],
            execution_payload: vec![],
            contract_address: None,
            gas_limit: 0,
            max_fee_per_gas: 0,
            chain_id: 1,
            locktime: 0,
        }
    }

    #[test]
    fn test_insert_and_get() {
        let index = ParallelTransactionIndex::new(ParallelIndexConfig::default());
        let tx = create_test_transaction();
        let tx_hash = bincode::serialize(&tx.id).unwrap();

        assert!(index.insert(tx_hash.clone(), tx.clone(), 1000, 256).is_ok());
        assert!(index.contains(&tx_hash));
        assert_eq!(index.len(), 1);

        let retrieved = index.get(&tx_hash);
        assert!(retrieved.is_some());
    }

    #[test]
    fn test_update_status() {
        let index = ParallelTransactionIndex::new(ParallelIndexConfig::default());
        let tx = create_test_transaction();
        let tx_hash = bincode::serialize(&tx.id).unwrap();

        index.insert(tx_hash.clone(), tx, 1000, 256).unwrap();
        assert_eq!(
            index.get_entry(&tx_hash).unwrap().status,
            IndexedTransactionStatus::Pending
        );

        index
            .update_status(&tx_hash, IndexedTransactionStatus::Validated)
            .unwrap();
        assert_eq!(
            index.get_entry(&tx_hash).unwrap().status,
            IndexedTransactionStatus::Validated
        );

        let stats = index.get_stats();
        assert_eq!(stats.validated_count, 1);
        assert_eq!(stats.pending_count, 0);
    }

    #[test]
    fn test_remove_transaction() {
        let index = ParallelTransactionIndex::new(ParallelIndexConfig::default());
        let tx = create_test_transaction();
        let tx_hash = bincode::serialize(&tx.id).unwrap();

        index.insert(tx_hash.clone(), tx, 1000, 256).unwrap();
        assert_eq!(index.len(), 1);

        let removed = index.remove(&tx_hash);
        assert!(removed.is_ok());
        assert_eq!(index.len(), 0);
        assert!(!index.contains(&tx_hash));
    }

    #[test]
    fn test_get_by_status() {
        let index = ParallelTransactionIndex::new(ParallelIndexConfig::default());

        for i in 0..5 {
            let tx = create_test_transaction();
            let tx_hash = bincode::serialize(&[i as u8]).unwrap();
            index
                .insert(tx_hash.clone(), tx, 1000 + i as u64, 256)
                .unwrap();

            if i % 2 == 1 {
                index
                    .update_status(&tx_hash, IndexedTransactionStatus::Validated)
                    .unwrap();
            }
        }

        let pending = index.get_by_status(IndexedTransactionStatus::Pending);
        let validated = index.get_by_status(IndexedTransactionStatus::Validated);

        assert_eq!(pending.len(), 3);
        assert_eq!(validated.len(), 2);
    }

    #[test]
    fn test_get_top_by_fee_rate() {
        let index = ParallelTransactionIndex::new(ParallelIndexConfig::default());

        for i in 0..5 {
            let tx = create_test_transaction();
            let tx_hash = bincode::serialize(&[i as u8]).unwrap();
            let fee = 5000 - (i as u64 * 1000); // decreasing fees
            let size = 256;
            index.insert(tx_hash, tx, fee, size).unwrap();
        }

        let top3 = index.get_top_by_fee_rate(3);
        assert_eq!(top3.len(), 3);
    }

    #[test]
    fn test_statistics_consistency() {
        let index = ParallelTransactionIndex::new(ParallelIndexConfig::default());
        let tx = create_test_transaction();
        let tx_hash = bincode::serialize(&tx.id).unwrap();

        index.insert(tx_hash.clone(), tx, 1000, 256).unwrap();
        assert!(index.verify_consistency().is_ok());

        index
            .update_status(&tx_hash, IndexedTransactionStatus::Validated)
            .unwrap();
        assert!(index.verify_consistency().is_ok());

        index.remove(&tx_hash).unwrap();
        assert!(index.verify_consistency().is_ok());
    }
}
