//! Orphan Transaction Management
//!
//! Sistem untuk menampung dan mengelola transaksi yang valid secara format
//! tetapi memiliki input yang belum dikenal (missing parents).
//!
//! Fitur:
//! - OrphanPool: DashMap-based storage dengan capacity limit
//! - MissingInputIndex: Parent-child relationship mapping
//! - Automatic re-validation saat parent tiba
//! - FIFO atau Fee-based eviction policies

use std::sync::Arc;
use std::collections::HashMap;

use dashmap::DashMap;
use parking_lot::RwLock;
use indexmap::IndexMap;

use klomang_core::core::state::transaction::Transaction;

use super::conflict::OutPoint;
use crate::storage::kv_store::KvStore;

/// Eviction policy untuk orphan pool
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrphanEvictionPolicy {
    /// First-In-First-Out: remove oldest transaction saat full
    Fifo,
    /// Fee-Based: remove transaction dengan fee rate terendah
    FeeBased,
}

/// Configuration untuk Orphan Pool
#[derive(Clone, Debug)]
pub struct OrphanPoolConfig {
    /// Maximum transaksi dalam orphan pool
    pub max_orphans: usize,
    /// Maximum transaksi yang bisa diadopsi dari orphan pool sekaligus
    pub max_adoption_batch: usize,
    /// TTL untuk orphan transactions (nanoseconds)
    pub orphan_ttl_ns: u64,
    /// Eviction policy saat pool penuh
    pub eviction_policy: OrphanEvictionPolicy,
}

impl Default for OrphanPoolConfig {
    fn default() -> Self {
        Self {
            max_orphans: 5000,
            max_adoption_batch: 1000,
            orphan_ttl_ns: 20 * 60 * 1_000_000_000, // 20 minutes
            eviction_policy: OrphanEvictionPolicy::FeeBased,
        }
    }
}

/// Metadata untuk orphan transaction entry
#[derive(Clone, Debug)]
pub struct OrphanEntry {
    /// Transaction yang disimpan
    pub transaction: Arc<Transaction>,
    /// Timestamp saat dimasukkan ke orphan pool (nanoseconds)
    pub insertion_time_ns: u64,
    /// Daftar OutPoint inputs yang missing
    pub missing_inputs: Vec<OutPoint>,
    /// Size dalam bytes
    pub size_bytes: usize,
    /// Total fee dalam satoshis
    pub total_fee: u64,
}

impl OrphanEntry {
    /// Buat orphan entry baru
    pub fn new(
        tx: Arc<Transaction>,
        missing_inputs: Vec<OutPoint>,
        size_bytes: usize,
        total_fee: u64,
    ) -> Self {
        let now = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()) as u64;

        Self {
            transaction: tx,
            insertion_time_ns: now,
            missing_inputs,
            size_bytes,
            total_fee,
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

    /// Hitung umur dalam nanoseconds
    pub fn age_ns(&self) -> u64 {
        let now = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()) as u64;
        now.saturating_sub(self.insertion_time_ns)
    }

    /// Check apakah sudah expired
    pub fn is_expired(&self, ttl_ns: u64) -> bool {
        self.age_ns() > ttl_ns
    }
}

/// Statistics untuk orphan pool
#[derive(Clone, Debug, Default)]
pub struct OrphanStats {
    /// Total orphan transactions
    pub total_orphans: usize,
    /// Total memory bytes digunakan
    pub total_memory_bytes: usize,
    /// Jumlah unique missing parents
    pub unique_missing_parents: usize,
    /// Transaksi yang sudah diadopsi
    pub adoption_count: u64,
    /// Transaksi yang di-evict
    pub evicted_count: u64,
    /// Transaksi yang expired
    pub expired_count: u64,
}

/// Result dari adoption operation
#[derive(Clone, Debug)]
pub struct AdoptionResult {
    /// Transaction hashes yang siap untuk validation
    pub adopted_txs: Vec<Arc<Transaction>>,
    /// Jumlah transaksi yang diadopsi
    pub adoption_count: usize,
    /// Jumlah transaksi yang masih waiting
    pub remaining_orphans: usize,
}

/// Orphan Transaction Manager
///
/// Mengelola transaksi orphan dan memfasilitasi parent-child relationships.
///
/// Fitur:
/// - Thread-safe orphan storage
/// - Efficient parent-child lookup
/// - Automatic adoption saat parent tiba
/// - Configurable eviction policies
pub struct OrphanManager {
    /// DashMap untuk orphan transactions, indexed by tx hash
    orphan_pool: DashMap<Vec<u8>, OrphanEntry>,
    
    /// Index dari OutPoint (parent) ke vec of orphan tx hashes (children)
    missing_input_index: Arc<RwLock<HashMap<OutPoint, Vec<Vec<u8>>>>>,
    
    /// Tracking insertion order untuk FIFO eviction
    insertion_order: Arc<RwLock<IndexMap<Vec<u8>, u64>>>,
    
    /// Configuration
    config: OrphanPoolConfig,
    
    /// Statistics
    stats: Arc<RwLock<OrphanStats>>,
    
    /// KvStore reference untuk existence checking (reserved for future use)
    _kv_store: Option<Arc<KvStore>>,
}

impl OrphanManager {
    /// Buat orphan manager baru
    pub fn new(config: OrphanPoolConfig, kv_store: Option<Arc<KvStore>>) -> Self {
        Self {
            orphan_pool: DashMap::new(),
            missing_input_index: Arc::new(RwLock::new(HashMap::new())),
            insertion_order: Arc::new(RwLock::new(IndexMap::new())),
            config,
            stats: Arc::new(RwLock::new(OrphanStats::default())),
            _kv_store: kv_store,
        }
    }

    /// Tambahkan transaksi ke orphan pool
    pub fn add_orphan(
        &self,
        tx_hash: Vec<u8>,
        tx: Arc<Transaction>,
        missing_inputs: Vec<OutPoint>,
        size_bytes: usize,
        total_fee: u64,
    ) -> Result<(), String> {
        // Check pool capacity
        if self.orphan_pool.len() >= self.config.max_orphans {
            // Apply eviction policy
            self._evict_one()?;
        }

        let entry = OrphanEntry::new(tx.clone(), missing_inputs.clone(), size_bytes, total_fee);

        // Insert into orphan pool
        self.orphan_pool.insert(tx_hash.clone(), entry.clone());

        // Update insertion order
        let now = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()) as u64;
        {
            let mut order = self.insertion_order.write();
            order.insert(tx_hash.clone(), now);
        }

        // Update missing input index
        {
            let mut index = self.missing_input_index.write();
            for missing_input in missing_inputs {
                index
                    .entry(missing_input)
                    .or_insert_with(Vec::new)
                    .push(tx_hash.clone());
            }
        }

        // Update statistics
        {
            let mut stats = self.stats.write();
            stats.total_orphans = self.orphan_pool.len();
            stats.total_memory_bytes += size_bytes;
            stats.unique_missing_parents = self.missing_input_index.read().len();
        }

        Ok(())
    }

    /// Check apakah transaksi ada di orphan pool
    pub fn contains(&self, tx_hash: &[u8]) -> bool {
        self.orphan_pool.contains_key(tx_hash)
    }

    /// Get orphan transaction
    pub fn get_orphan(&self, tx_hash: &[u8]) -> Option<Arc<Transaction>> {
        self.orphan_pool
            .get(tx_hash)
            .map(|entry| Arc::clone(&entry.transaction))
    }

    /// Get orphan entry dengan metadata
    pub fn get_orphan_entry(&self, tx_hash: &[u8]) -> Option<OrphanEntry> {
        self.orphan_pool
            .get(tx_hash)
            .map(|entry| entry.clone())
    }

    /// Process orphans untuk parent yang baru tiba
    ///
    /// Ketika parent transaction tiba, cari semua children yang menunggu
    /// dan return mereka untuk di-adopt ke main mempool
    pub fn process_orphans_for_parent(&self, parent_outpoint: &OutPoint) -> Result<AdoptionResult, String> {
        let mut adopted_txs = Vec::new();

        // Get children yang menunggu parent ini
        let children_hashes = {
            let mut index = self.missing_input_index.write();
            index.remove(parent_outpoint).unwrap_or_default()
        };

        // Move children dari orphan pool ke result
        for child_hash in children_hashes {
            if let Some((_, entry)) = self.orphan_pool.remove(&child_hash) {
                adopted_txs.push(Arc::clone(&entry.transaction));

                // Update insertion order
                {
                    let mut order = self.insertion_order.write();
                    order.shift_remove(&child_hash);
                }

                // Update statistics
                {
                    let mut stats = self.stats.write();
                    stats.total_orphans = self.orphan_pool.len();
                    stats.total_memory_bytes = stats.total_memory_bytes.saturating_sub(entry.size_bytes);
                    stats.adoption_count += 1;
                }
            }
        }

        // Update missing parents index
        {
            let mut index = self.missing_input_index.write();
            index.retain(|_, children| !children.is_empty());
        }

        let remaining = self.orphan_pool.len();

        Ok(AdoptionResult {
            adoption_count: adopted_txs.len(),
            adopted_txs,
            remaining_orphans: remaining,
        })
    }

    /// Process orchans untuk multiple parents
    pub fn process_orphans_for_parents(
        &self,
        parent_outpoints: &[OutPoint],
    ) -> Result<AdoptionResult, String> {
        let mut all_adopted = Vec::new();

        for parent_outpoint in parent_outpoints {
            let result = self.process_orphans_for_parent(parent_outpoint)?;
            all_adopted.extend(result.adopted_txs);
        }

        Ok(AdoptionResult {
            adoption_count: all_adopted.len(),
            adopted_txs: all_adopted,
            remaining_orphans: self.orphan_pool.len(),
        })
    }

    /// Remove transaksi dari orphan pool secara explicit
    pub fn remove_orphan(&self, tx_hash: &[u8]) -> Result<Arc<Transaction>, String> {
        if let Some((_, entry)) = self.orphan_pool.remove(tx_hash) {
            // Remove dari insertion order
            {
                let mut order = self.insertion_order.write();
                order.shift_remove(tx_hash);
            }

            // Remove dari missing input index
            {
                let mut index = self.missing_input_index.write();
                for missing_input in &entry.missing_inputs {
                    if let Some(children) = index.get_mut(missing_input) {
                        children.retain(|h| h != tx_hash);
                    }
                }
                index.retain(|_, children| !children.is_empty());
            }

            // Update statistics
            {
                let mut stats = self.stats.write();
                stats.total_orphans = self.orphan_pool.len();
                stats.total_memory_bytes = stats.total_memory_bytes.saturating_sub(entry.size_bytes);
            }

            Ok(Arc::clone(&entry.transaction))
        } else {
            Err("Orphan transaction not found".to_string())
        }
    }

    /// Cleanup expired orphan transactions
    pub fn cleanup_expired(&self) -> usize {
        let expired_hashes: Vec<Vec<u8>> = self.orphan_pool
            .iter()
            .filter(|entry| entry.value().is_expired(self.config.orphan_ttl_ns))
            .map(|entry| entry.key().clone())
            .collect();

        let mut expired_count = 0;
        for tx_hash in expired_hashes {
            if let Ok(_) = self.remove_orphan(&tx_hash) {
                expired_count += 1;
            }
        }

        expired_count
    }

    /// Get semua orphan transactions dengan status tertentu
    pub fn get_orphans_by_age_range(&self, min_age_ns: u64, max_age_ns: u64) -> Vec<Arc<Transaction>> {
        self.orphan_pool
            .iter()
            .filter(|entry| {
                let age = entry.value().age_ns();
                age >= min_age_ns && age <= max_age_ns
            })
            .map(|entry| Arc::clone(&entry.value().transaction))
            .collect()
    }

    /// Get orphan transactions yang dependency pada specific parent
    pub fn get_orphans_waiting_for(&self, parent_outpoint: &OutPoint) -> Vec<Arc<Transaction>> {
        let index = self.missing_input_index.read();
        let children_hashes = index.get(parent_outpoint).cloned().unwrap_or_default();

        children_hashes
            .iter()
            .filter_map(|hash| {
                self.orphan_pool
                    .get(hash)
                    .map(|entry| Arc::clone(&entry.value().transaction))
            })
            .collect()
    }

    /// Get current statistics
    pub fn get_stats(&self) -> OrphanStats {
        let mut stats = self.stats.read().clone();
        stats.total_orphans = self.orphan_pool.len();
        stats.unique_missing_parents = self.missing_input_index.read().len();
        stats
    }

    /// Get jumlah total orphan transactions
    pub fn len(&self) -> usize {
        self.orphan_pool.len()
    }

    /// Check apakah orphan pool kosong
    pub fn is_empty(&self) -> bool {
        self.orphan_pool.is_empty()
    }

    /// Clear semua orphan transactions
    pub fn clear(&self) {
        self.orphan_pool.clear();
        self.missing_input_index.write().clear();
        self.insertion_order.write().clear();
        
        let mut stats = self.stats.write();
        *stats = OrphanStats::default();
    }

    /// Verify consistency antara orphan pool dan indexes
    pub fn verify_consistency(&self) -> Result<(), String> {
        let index = self.missing_input_index.read();

        // Check bahwa semua children references valid
        for (_, children_hashes) in index.iter() {
            for child_hash in children_hashes {
                if !self.orphan_pool.contains_key(child_hash) {
                    return Err("Orphan child reference mismatch in missing input index".to_string());
                }
            }
        }

        Ok(())
    }

    /// Evict satu transaksi berdasarkan policy
    fn _evict_one(&self) -> Result<(), String> {
        let to_evict = match self.config.eviction_policy {
            OrphanEvictionPolicy::Fifo => self._select_fifo_victim(),
            OrphanEvictionPolicy::FeeBased => self._select_fee_victim(),
        };

        if let Some(tx_hash) = to_evict {
            if let Ok(_) = self.remove_orphan(&tx_hash) {
                let mut stats = self.stats.write();
                stats.evicted_count += 1;
            }
            Ok(())
        } else {
            Err("No transaction available for eviction".to_string())
        }
    }

    fn _select_fifo_victim(&self) -> Option<Vec<u8>> {
        let order = self.insertion_order.read();
        order.first().map(|(hash, _)| hash.clone())
    }

    fn _select_fee_victim(&self) -> Option<Vec<u8>> {
        let mut lowest_fee_hash = None;
        let mut lowest_fee_rate = u64::MAX;

        for entry in self.orphan_pool.iter() {
            let fee_rate = entry.value().fee_rate();
            if fee_rate < lowest_fee_rate {
                lowest_fee_rate = fee_rate;
                lowest_fee_hash = Some(entry.key().clone());
            }
        }

        lowest_fee_hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_transaction() -> Arc<Transaction> {
        Arc::new(Transaction {
            id: klomang_core::core::crypto::Hash::new(&[1, 2, 3, 4]),
            inputs: vec![],
            outputs: vec![],
            lock_time: 0,
            version: 1,
        })
    }

    #[test]
    fn test_add_and_get_orphan() {
        let manager = OrphanManager::new(OrphanPoolConfig::default(), None);
        let tx = create_test_transaction();
        let tx_hash = vec![1, 2, 3, 4];
        let missing = vec![OutPoint::new(vec![5, 6, 7, 8], 0)];

        assert!(manager.add_orphan(tx_hash.clone(), tx.clone(), missing, 256, 1000).is_ok());
        assert!(manager.contains(&tx_hash));

        let retrieved = manager.get_orphan(&tx_hash);
        assert!(retrieved.is_some());
    }

    #[test]
    fn test_missing_input_index() {
        let manager = OrphanManager::new(OrphanPoolConfig::default(), None);
        let tx = create_test_transaction();
        let tx_hash = vec![1, 2, 3, 4];
        let parent_outpoint = OutPoint::new(vec![5, 6, 7, 8], 0);
        let missing = vec![parent_outpoint.clone()];

        manager
            .add_orphan(tx_hash.clone(), tx, missing, 256, 1000)
            .unwrap();

        let children = manager.get_orphans_waiting_for(&parent_outpoint);
        assert_eq!(children.len(), 1);
    }

    #[test]
    fn test_process_orphans_for_parent() {
        let manager = OrphanManager::new(OrphanPoolConfig::default(), None);
        let tx = create_test_transaction();
        let tx_hash = vec![1, 2, 3, 4];
        let parent_outpoint = OutPoint::new(vec![5, 6, 7, 8], 0);
        let missing = vec![parent_outpoint.clone()];

        manager
            .add_orphan(tx_hash.clone(), tx, missing, 256, 1000)
            .unwrap();

        assert_eq!(manager.len(), 1);

        let result = manager.process_orphans_for_parent(&parent_outpoint).unwrap();
        assert_eq!(result.adoption_count, 1);
        assert_eq!(manager.len(), 0);
    }

    #[test]
    fn test_remove_orphan() {
        let manager = OrphanManager::new(OrphanPoolConfig::default(), None);
        let tx = create_test_transaction();
        let tx_hash = vec![1, 2, 3, 4];
        let missing = vec![OutPoint::new(vec![5, 6, 7, 8], 0)];

        manager
            .add_orphan(tx_hash.clone(), tx, missing, 256, 1000)
            .unwrap();
        assert_eq!(manager.len(), 1);

        let removed = manager.remove_orphan(&tx_hash);
        assert!(removed.is_ok());
        assert_eq!(manager.len(), 0);
    }

    #[test]
    fn test_eviction_fifo() {
        let config = OrphanPoolConfig {
            max_orphans: 2,
            eviction_policy: OrphanEvictionPolicy::Fifo,
            ..Default::default()
        };
        let manager = OrphanManager::new(config, None);

        let tx1 = create_test_transaction();
        let tx2 = create_test_transaction();
        let tx3 = create_test_transaction();

        let hash1 = vec![1];
        let hash2 = vec![2];
        let hash3 = vec![3];
        let missing = vec![OutPoint::new(vec![99], 0)];

        manager.add_orphan(hash1, tx1, missing.clone(), 256, 1000).ok();
        std::thread::sleep(std::time::Duration::from_millis(10));
        manager.add_orphan(hash2, tx2, missing.clone(), 256, 1000).ok();
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Adding third should trigger eviction of first (FIFO)
        manager.add_orphan(hash3, tx3, missing.clone(), 256, 1000).ok();

        assert_eq!(manager.len(), 2);
    }

    #[test]
    fn test_cleanup_expired() {
        let config = OrphanPoolConfig {
            orphan_ttl_ns: 1_000_000, // 1 ms
            ..Default::default()
        };
        let manager = OrphanManager::new(config, None);
        let tx = create_test_transaction();
        let missing = vec![OutPoint::new(vec![5], 0)];

        manager.add_orphan(vec![1], tx, missing, 256, 1000).ok();
        assert_eq!(manager.len(), 1);

        std::thread::sleep(std::time::Duration::from_millis(2));
        let expired_count = manager.cleanup_expired();

        assert_eq!(expired_count, 1);
        assert_eq!(manager.len(), 0);
    }

    #[test]
    fn test_statistics() {
        let manager = OrphanManager::new(OrphanPoolConfig::default(), None);
        let tx = create_test_transaction();
        let missing = vec![OutPoint::new(vec![5], 0)];

        manager.add_orphan(vec![1], tx, missing, 256, 1000).ok();

        let stats = manager.get_stats();
        assert_eq!(stats.total_orphans, 1);
        assert_eq!(stats.total_memory_bytes, 256);
        assert_eq!(stats.unique_missing_parents, 1);
    }

    #[test]
    fn test_verify_consistency() {
        let manager = OrphanManager::new(OrphanPoolConfig::default(), None);
        let tx = create_test_transaction();
        let missing = vec![OutPoint::new(vec![5], 0)];

        manager.add_orphan(vec![1], tx, missing, 256, 1000).ok();
        assert!(manager.verify_consistency().is_ok());
    }
}
