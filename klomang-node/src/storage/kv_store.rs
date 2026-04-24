use std::convert::TryInto;
use std::sync::Arc;

use crate::storage::cf::ColumnFamilyName;
use crate::storage::cache::StorageCacheLayer;
use crate::storage::config::StorageConfig;
use crate::storage::db::StorageDb;
use crate::storage::error::{StorageError, StorageResult};
use crate::storage::atomic_write::{AtomicBlockWriter, BlockTransactionBatch};
use crate::storage::metrics::StorageMetrics;
use crate::storage::schema::{BlockValue, HeaderValue, TransactionValue, UtxoValue, UtxoSpentValue, VerkleStateValue, DagNodeValue, DagTipsValue};
use klomang_core::NoOpMetricsCollector;
use tempfile::TempDir;

/// Strongly-typed key-value store operations for Klomang blockchain storage
pub struct KvStore {
    cache_layer: Arc<StorageCacheLayer>,
    _temp_dir: Option<TempDir>,
}

impl KvStore {
    pub fn new(cache_layer: Arc<StorageCacheLayer>) -> Self {
        Self { cache_layer, _temp_dir: None }
    }

    pub fn new_dummy() -> Self {
        let temp_dir = TempDir::new().expect("Failed to create dummy KvStore temp dir");
        let db_path = temp_dir.path().join("kvstore_db");
        let wal_path = temp_dir.path().join("kvstore_wal");
        let config = StorageConfig::new(&db_path).with_wal_dir(&wal_path);
        let metrics = Arc::new(StorageMetrics::new(Box::new(NoOpMetricsCollector)));
        let db = StorageDb::open_with_config(&config, metrics)
            .expect("Failed to create dummy KvStore storage");
        let cache_layer = Arc::new(StorageCacheLayer::new(db));

        Self {
            cache_layer,
            _temp_dir: Some(temp_dir),
        }
    }

    // ============================
    // BLOCK OPERATIONS
    // ============================

    pub fn put_block(&self, block_hash: &[u8], block: &BlockValue) -> StorageResult<()> {
        self.cache_layer.put_block(block_hash, block)
    }

    pub fn get_block(&self, block_hash: &[u8]) -> StorageResult<Option<BlockValue>> {
        self.cache_layer.get_block(block_hash)
    }

    pub fn delete_block(&self, block_hash: &[u8]) -> StorageResult<()> {
        self.cache_layer.delete_block(block_hash)
    }

    // ============================
    // HEADER OPERATIONS
    // ============================

    pub fn put_header(&self, block_hash: &[u8], header: &HeaderValue) -> StorageResult<()> {
        self.cache_layer.put_header(block_hash, header)
    }

    pub fn get_header(&self, block_hash: &[u8]) -> StorageResult<Option<HeaderValue>> {
        self.cache_layer.get_header(block_hash)
    }

    pub fn delete_header(&self, block_hash: &[u8]) -> StorageResult<()> {
        self.cache_layer.delete_header(block_hash)
    }

    // ============================
    // TRANSACTION OPERATIONS
    // ============================

    pub fn put_transaction(&self, tx_hash: &[u8], tx: &TransactionValue) -> StorageResult<()> {
        self.cache_layer.put_transaction(tx_hash, tx)
    }

    pub fn get_transaction(&self, tx_hash: &[u8]) -> StorageResult<Option<TransactionValue>> {
        self.cache_layer.get_transaction(tx_hash)
    }

    pub fn delete_transaction(&self, tx_hash: &[u8]) -> StorageResult<()> {
        self.cache_layer.delete_transaction(tx_hash)
    }

    pub fn put_mempool_transaction(&self, tx_hash: Vec<u8>, tx: TransactionValue) {
        self.cache_layer.insert_mempool_transaction(tx_hash, tx);
    }

    pub fn put_mempool_min_fee_rate(&self, min_fee_rate: u64) -> StorageResult<()> {
        self.cache_layer
            .db()
            .put(ColumnFamilyName::Default, b"mempool:min_fee_rate", &min_fee_rate.to_be_bytes())
            .map_err(|e| StorageError::from(e))
    }

    pub fn get_mempool_min_fee_rate(&self) -> StorageResult<Option<u64>> {
        match self
            .cache_layer
            .db()
            .get(ColumnFamilyName::Default, b"mempool:min_fee_rate")?
        {
            Some(raw) if raw.len() == 8 => {
                let bytes: [u8; 8] = raw.as_slice().try_into().map_err(|_| {
                    StorageError::DbError("invalid persisted min fee rate bytes".to_string())
                })?;
                Ok(Some(u64::from_be_bytes(bytes)))
            }
            Some(_) => Err(StorageError::DbError(
                "invalid persisted min fee rate length".to_string(),
            )),
            None => Ok(None),
        }
    }

    /// Store historical system load metrics for trend analysis
    pub fn put_system_load_trend(&self, timestamp: u64, cpu_percent: f64, ram_percent: f64, tx_count: usize) -> StorageResult<()> {
        let key = format!("system_load:{}", timestamp);
        let value = bincode::serialize(&(cpu_percent, ram_percent, tx_count))
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;

        self.cache_layer
            .db()
            .put(ColumnFamilyName::Default, key.as_bytes(), &value)
            .map_err(|e| StorageError::from(e))
    }

    /// Get historical system load metrics
    pub fn get_system_load_trend(&self, timestamp: u64) -> StorageResult<Option<(f64, f64, usize)>> {
        let key = format!("system_load:{}", timestamp);
        match self
            .cache_layer
            .db()
            .get(ColumnFamilyName::Default, key.as_bytes())?
        {
            Some(raw) => {
                let (cpu_percent, ram_percent, tx_count): (f64, f64, usize) = bincode::deserialize(&raw)
                    .map_err(|e| StorageError::SerializationError(e.to_string()))?;
                Ok(Some((cpu_percent, ram_percent, tx_count)))
            }
            None => Ok(None),
        }
    }

    /// Get average system load over the last N hours
    pub fn get_average_system_load(&self, hours: u64) -> StorageResult<(f64, f64)> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let _start_time = now.saturating_sub(hours * 3600);
        let mut total_cpu = 0.0;
        let mut total_ram = 0.0;
        let mut count = 0;

        // Scan recent load entries (simplified - in production, use proper time range queries)
        for hour_offset in 0..hours {
            let timestamp = now.saturating_sub(hour_offset * 3600);
            if let Some((cpu, ram, _)) = self.get_system_load_trend(timestamp)? {
                total_cpu += cpu;
                total_ram += ram;
                count += 1;
            }
        }

        if count == 0 {
            Ok((0.0, 0.0))
        } else {
            Ok((total_cpu / count as f64, total_ram / count as f64))
        }
    }

    pub fn get_mempool_transaction(&self, tx_hash: &[u8]) -> Option<TransactionValue> {
        self.cache_layer.get_mempool_transaction(tx_hash)
    }

    pub fn remove_mempool_transaction(&self, tx_hash: &[u8]) {
        self.cache_layer.remove_mempool_transaction(tx_hash);
    }

    // ============================
    // UTXO OPERATIONS
    // ============================

    pub fn put_utxo(&self, tx_hash: &[u8], output_index: u32, utxo: &UtxoValue) -> StorageResult<()> {
        self.cache_layer.put_utxo(tx_hash, output_index, utxo)
    }

    pub fn get_utxo(&self, tx_hash: &[u8], output_index: u32) -> StorageResult<Option<UtxoValue>> {
        self.cache_layer.get_utxo(tx_hash, output_index)
    }

    pub fn delete_utxo(&self, tx_hash: &[u8], output_index: u32) -> StorageResult<()> {
        self.cache_layer.delete_utxo(tx_hash, output_index)
    }

    pub fn utxo_exists(&self, tx_hash: &[u8], output_index: u32) -> StorageResult<bool> {
        Ok(self
            .cache_layer
            .get_utxo(tx_hash, output_index)?
            .is_some())
    }

    // ============================
    // UTXO SPENT INDEX OPERATIONS
    // ============================

    pub fn put_utxo_spent(
        &self,
        tx_hash: &[u8],
        output_index: u32,
        spent: &UtxoSpentValue,
    ) -> StorageResult<()> {
        self.cache_layer.put_utxo_spent(tx_hash, output_index, spent)
    }

    pub fn get_utxo_spent(
        &self,
        tx_hash: &[u8],
        output_index: u32,
    ) -> StorageResult<Option<UtxoSpentValue>> {
        self.cache_layer.get_utxo_spent(tx_hash, output_index)
    }

    pub fn delete_utxo_spent(&self, tx_hash: &[u8], output_index: u32) -> StorageResult<()> {
        self.cache_layer.delete_utxo_spent(tx_hash, output_index)
    }

    // ============================
    // VERKLE STATE OPERATIONS
    // ============================

    pub fn put_verkle_state(&self, path: &[u8], state: &VerkleStateValue) -> StorageResult<()> {
        self.cache_layer.put_verkle_state(path, state)
    }

    pub fn get_verkle_state(&self, path: &[u8]) -> StorageResult<Option<VerkleStateValue>> {
        self.cache_layer.get_verkle_state(path)
    }

    pub fn delete_verkle_state(&self, path: &[u8]) -> StorageResult<()> {
        self.cache_layer.delete_verkle_state(path)
    }

    // ============================
    // DAG OPERATIONS
    // ============================

    pub fn put_dag_node(&self, block_hash: &[u8], node: &DagNodeValue) -> StorageResult<()> {
        self.cache_layer.put_dag_node(block_hash, node)
    }

    pub fn get_dag_node(&self, block_hash: &[u8]) -> StorageResult<Option<DagNodeValue>> {
        self.cache_layer.get_dag_node(block_hash)
    }

    pub fn delete_dag_node(&self, block_hash: &[u8]) -> StorageResult<()> {
        self.cache_layer.delete_dag_node(block_hash)
    }

    // ============================
    // DAG TIPS OPERATIONS
    // ============================

    pub fn put_dag_tips(&self, key: &[u8], tips: &DagTipsValue) -> StorageResult<()> {
        self.cache_layer.put_dag_tips(key, tips)
    }

    pub fn get_dag_tips(&self, key: &[u8]) -> StorageResult<Option<DagTipsValue>> {
        self.cache_layer.get_dag_tips(key)
    }

    pub fn delete_dag_tips(&self, key: &[u8]) -> StorageResult<()> {
        self.cache_layer.delete_dag_tips(key)
    }

    pub fn get_current_dag_tips(&self) -> StorageResult<Option<DagTipsValue>> {
        self.get_dag_tips(b"current_tips")
    }

    pub fn put_current_dag_tips(&self, tips: &DagTipsValue) -> StorageResult<()> {
        self.put_dag_tips(b"current_tips", tips)
    }

    // ============================
    // BATCH OPERATIONS
    // ============================

    pub fn flush(&self) -> StorageResult<()> {
        self.cache_layer.db().flush().map_err(|e| StorageError::from(e))
    }

    // ============================
    // ATOMIC BLOCK OPERATIONS
    // ============================

    /// Atomically commit a block with all its transactions and UTXO changes to storage.
    ///
    /// This method ensures all-or-nothing semantics using RocksDB WriteBatch:
    /// - Block and header data are stored
    /// - All transactions are persisted
    /// - UTXO state is updated (spent UTXOs removed, new UTXOs added)
    /// - DAG structure is updated (parent-child links, tips)
    ///
    /// If any step fails during batch preparation, the entire operation is
    /// rolled back and an error is returned. No partial data is written.
    pub fn commit_block_atomic(
        &self,
        block_hash: &[u8],
        block_value: &BlockValue,
        header_value: &HeaderValue,
        transactions: Vec<BlockTransactionBatch>,
        dag_node: &DagNodeValue,
        dag_tips: &DagTipsValue,
    ) -> StorageResult<()> {
        AtomicBlockWriter::commit_block_to_storage(
            self.cache_layer.db(),
            block_hash,
            block_value,
            header_value,
            transactions,
            dag_node,
            dag_tips,
        )
    }

    /// Atomically commit a block without write-ahead log (WAL).
    ///
    /// # Warning
    ///
    /// Only use for non-critical operations like snapshots or bulk sync where
    /// data can be recomputed. This does not guarantee durability in case of crash.
    pub fn commit_block_atomic_no_wal(
        &self,
        block_hash: &[u8],
        block_value: &BlockValue,
        header_value: &HeaderValue,
        transactions: Vec<BlockTransactionBatch>,
        dag_node: &DagNodeValue,
        dag_tips: &DagTipsValue,
    ) -> StorageResult<()> {
        AtomicBlockWriter::commit_block_to_storage_no_wal(
            self.cache_layer.db(),
            block_hash,
            block_value,
            header_value,
            transactions,
            dag_node,
            dag_tips,
        )
    }
}
