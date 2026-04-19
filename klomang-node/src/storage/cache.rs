use std::num::NonZeroUsize;
use std::sync::Arc;

use dashmap::DashMap;
use lru::LruCache;
use parking_lot::RwLock;
use rocksdb::{Direction, IteratorMode};

use crate::storage::cf::ColumnFamilyName;
use crate::storage::concurrency::{StorageWriteCommand, StorageWriter};
use crate::storage::db::StorageDb;
use crate::storage::error::{StorageError, StorageResult};
use crate::storage::schema::{
    BlockValue, DagNodeValue, DagTipsValue, HeaderValue, TransactionValue, UtxoSpentValue,
    make_utxo_key, parse_utxo_key, VerkleStateValue, UtxoValue,
};

const RECENT_BLOCK_CACHE_CAPACITY: usize = 10_000;
const UTXO_HOT_CACHE_CAPACITY: usize = 20_000;

/// Thread-safe UTXO cache with LRU eviction policy
/// 
/// Uses single RwLock to prevent race conditions between cache operations
/// and LRU tracking. This ensures consistency: if key is in cache, it's in LRU.
#[derive(Clone, Debug)]
pub struct UtxoHotCache {
    cache: Arc<RwLock<LruCache<Vec<u8>, Arc<UtxoValue>>>>,
}

impl UtxoHotCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            cache: Arc::new(RwLock::new(
                LruCache::new(NonZeroUsize::new(capacity).unwrap())
            )),
        }
    }

    /// Insert or update a UTXO in the cache
    /// 
    /// If cache is at capacity, the least recently used item is evicted.
    pub fn insert(&self, key: Vec<u8>, value: UtxoValue) {
        let value_arc = Arc::new(value);
        let mut cache = self.cache.write();
        cache.put(key, value_arc);
        // LRU is automatically updated by cache.put()
    }

    /// Get a UTXO from cache if it exists
    /// 
    /// Returns None if key is not in cache. Updates LRU tracking on hit.
    pub fn get(&self, key: &[u8]) -> Option<Arc<UtxoValue>> {
        let mut cache = self.cache.write();
        cache.get(key).cloned()
    }

    /// Remove a UTXO from cache and return it if present
    pub fn remove(&self, key: &[u8]) -> Option<Arc<UtxoValue>> {
        let mut cache = self.cache.write();
        cache.pop(key)
    }

    /// Clear all items from the cache
    pub fn clear(&self) {
        self.cache.write().clear();
    }

    /// Get current number of items in cache
    pub fn len(&self) -> usize {
        self.cache.read().len()
    }

    /// Check if cache is empty
    pub fn is_empty(&self) -> bool {
        self.cache.read().is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct RecentBlockCache {
    inner: Arc<RwLock<LruCache<Vec<u8>, Arc<BlockValue>>>>,
}

impl RecentBlockCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(LruCache::new(NonZeroUsize::new(capacity).unwrap()))),
        }
    }

    pub fn insert(&self, key: Vec<u8>, block: BlockValue) {
        let mut cache = self.inner.write();
        cache.put(key, Arc::new(block));
    }

    pub fn get(&self, key: &[u8]) -> Option<Arc<BlockValue>> {
        let mut cache = self.inner.write();
        cache.get(key).cloned()
    }

    pub fn remove(&self, key: &[u8]) {
        self.inner.write().pop(key);
    }
}

#[derive(Clone, Debug)]
pub struct MempoolCache {
    inner: Arc<DashMap<Vec<u8>, Arc<TransactionValue>>>,
}

impl MempoolCache {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    pub fn insert(&self, key: Vec<u8>, transaction: TransactionValue) {
        self.inner.insert(key, Arc::new(transaction));
    }

    pub fn get(&self, key: &[u8]) -> Option<Arc<TransactionValue>> {
        self.inner.get(key).map(|value| value.clone())
    }

    pub fn remove(&self, key: &[u8]) {
        self.inner.remove(key);
    }
}

#[derive(Clone, Debug)]
pub struct StorageCacheLayer {
    db: Arc<StorageDb>,
    writer: Arc<StorageWriter>,
    utxo_hot_cache: UtxoHotCache,
    recent_block_cache: RecentBlockCache,
    mempool_cache: MempoolCache,
}

impl StorageCacheLayer {
    pub fn new(db: StorageDb) -> Self {
        let db = Arc::new(db);
        let writer = Arc::new(StorageWriter::new(Arc::clone(&db)));

        Self {
            db,
            writer,
            utxo_hot_cache: UtxoHotCache::new(UTXO_HOT_CACHE_CAPACITY),
            recent_block_cache: RecentBlockCache::new(RECENT_BLOCK_CACHE_CAPACITY),
            mempool_cache: MempoolCache::new(),
        }
    }

    pub fn new_with_writer(db: Arc<StorageDb>, writer: Arc<StorageWriter>) -> Self {
        Self {
            db,
            writer,
            utxo_hot_cache: UtxoHotCache::new(UTXO_HOT_CACHE_CAPACITY),
            recent_block_cache: RecentBlockCache::new(RECENT_BLOCK_CACHE_CAPACITY),
            mempool_cache: MempoolCache::new(),
        }
    }

    pub fn db(&self) -> &StorageDb {
        &self.db
    }

    fn enqueue_write(&self, command: StorageWriteCommand) -> StorageResult<()> {
        self.writer.enqueue(vec![command])
    }

    pub fn get_utxo(&self, tx_hash: &[u8], output_index: u32) -> StorageResult<Option<UtxoValue>> {
        let key = make_utxo_key(tx_hash, output_index);

        if let Some(value) = self.utxo_hot_cache.get(&key) {
            return Ok(Some((*value).clone()));
        }

        match self.db.get(ColumnFamilyName::Utxo, &key)? {
            Some(raw) => {
                let utxo = UtxoValue::from_bytes(&raw)?;
                self.utxo_hot_cache.insert(key, utxo.clone());
                Ok(Some(utxo))
            }
            None => Ok(None),
        }
    }

    pub fn put_utxo(&self, tx_hash: &[u8], output_index: u32, utxo: &UtxoValue) -> StorageResult<()> {
        let key = make_utxo_key(tx_hash, output_index);
        let value = utxo.to_bytes()?;
        self.enqueue_write(StorageWriteCommand::Put {
            cf: ColumnFamilyName::Utxo,
            key: key.clone(),
            value,
        })?;
        self.utxo_hot_cache.insert(key, utxo.clone());
        Ok(())
    }

    pub fn delete_utxo(&self, tx_hash: &[u8], output_index: u32) -> StorageResult<()> {
        let key = make_utxo_key(tx_hash, output_index);
        self.enqueue_write(StorageWriteCommand::Delete {
            cf: ColumnFamilyName::Utxo,
            key: key.clone(),
        })?;
        self.utxo_hot_cache.remove(&key);
        Ok(())
    }

    pub fn cache_utxo(&self, tx_hash: &[u8], output_index: u32, utxo: &UtxoValue) {
        let key = make_utxo_key(tx_hash, output_index);
        self.utxo_hot_cache.insert(key, utxo.clone());
    }

    pub fn remove_utxo_from_cache(&self, tx_hash: &[u8], output_index: u32) {
        let key = make_utxo_key(tx_hash, output_index);
        self.utxo_hot_cache.remove(&key);
    }

    pub fn get_block(&self, block_hash: &[u8]) -> StorageResult<Option<BlockValue>> {
        if let Some(value) = self.recent_block_cache.get(block_hash) {
            return Ok(Some((*value).clone()));
        }

        match self.db.get(ColumnFamilyName::Blocks, block_hash)? {
            Some(raw) => {
                let block = BlockValue::from_bytes(&raw)?;
                self.recent_block_cache.insert(block_hash.to_vec(), block.clone());
                Ok(Some(block))
            }
            None => Ok(None),
        }
    }

    pub fn put_block(&self, block_hash: &[u8], block: &BlockValue) -> StorageResult<()> {
        let value = block.to_bytes()?;
        self.enqueue_write(StorageWriteCommand::Put {
            cf: ColumnFamilyName::Blocks,
            key: block_hash.to_vec(),
            value,
        })?;
        self.recent_block_cache.insert(block_hash.to_vec(), block.clone());
        Ok(())
    }

    pub fn delete_block(&self, block_hash: &[u8]) -> StorageResult<()> {
        self.enqueue_write(StorageWriteCommand::Delete {
            cf: ColumnFamilyName::Blocks,
            key: block_hash.to_vec(),
        })?;
        self.recent_block_cache.remove(block_hash);
        Ok(())
    }

    pub fn get_transaction(&self, tx_hash: &[u8]) -> StorageResult<Option<TransactionValue>> {
        match self.db.get(ColumnFamilyName::Transactions, tx_hash)? {
            Some(raw) => Ok(Some(TransactionValue::from_bytes(&raw)?)),
            None => Ok(None),
        }
    }

    pub fn put_transaction(&self, tx_hash: &[u8], transaction: &TransactionValue) -> StorageResult<()> {
        let value = transaction.to_bytes()?;
        self.enqueue_write(StorageWriteCommand::Put {
            cf: ColumnFamilyName::Transactions,
            key: tx_hash.to_vec(),
            value,
        })
    }

    pub fn delete_transaction(&self, tx_hash: &[u8]) -> StorageResult<()> {
        self.enqueue_write(StorageWriteCommand::Delete {
            cf: ColumnFamilyName::Transactions,
            key: tx_hash.to_vec(),
        })
    }

    pub fn put_header(&self, block_hash: &[u8], header: &HeaderValue) -> StorageResult<()> {
        let value = header.to_bytes()?;
        self.enqueue_write(StorageWriteCommand::Put {
            cf: ColumnFamilyName::Headers,
            key: block_hash.to_vec(),
            value,
        })
    }

    pub fn delete_header(&self, block_hash: &[u8]) -> StorageResult<()> {
        self.enqueue_write(StorageWriteCommand::Delete {
            cf: ColumnFamilyName::Headers,
            key: block_hash.to_vec(),
        })
    }

    pub fn get_header(&self, block_hash: &[u8]) -> StorageResult<Option<HeaderValue>> {
        match self.db.get(ColumnFamilyName::Headers, block_hash)? {
            Some(raw) => Ok(Some(HeaderValue::from_bytes(&raw)?)),
            None => Ok(None),
        }
    }

    pub fn put_utxo_spent(&self, tx_hash: &[u8], output_index: u32, spent: &UtxoSpentValue) -> StorageResult<()> {
        let key = make_utxo_key(tx_hash, output_index);
        let value = spent.to_bytes()?;
        self.enqueue_write(StorageWriteCommand::Put {
            cf: ColumnFamilyName::UtxoSpent,
            key,
            value,
        })
    }

    pub fn delete_utxo_spent(&self, tx_hash: &[u8], output_index: u32) -> StorageResult<()> {
        let key = make_utxo_key(tx_hash, output_index);
        self.enqueue_write(StorageWriteCommand::Delete {
            cf: ColumnFamilyName::UtxoSpent,
            key,
        })
    }

    pub fn put_verkle_state(&self, path: &[u8], state: &VerkleStateValue) -> StorageResult<()> {
        let value = state.to_bytes()?;
        self.enqueue_write(StorageWriteCommand::Put {
            cf: ColumnFamilyName::VerkleState,
            key: path.to_vec(),
            value,
        })
    }

    pub fn delete_verkle_state(&self, path: &[u8]) -> StorageResult<()> {
        self.enqueue_write(StorageWriteCommand::Delete {
            cf: ColumnFamilyName::VerkleState,
            key: path.to_vec(),
        })
    }

    pub fn put_dag_node(&self, block_hash: &[u8], node: &DagNodeValue) -> StorageResult<()> {
        let value = node.to_bytes()?;
        self.enqueue_write(StorageWriteCommand::Put {
            cf: ColumnFamilyName::Dag,
            key: block_hash.to_vec(),
            value,
        })
    }

    pub fn delete_dag_node(&self, block_hash: &[u8]) -> StorageResult<()> {
        self.enqueue_write(StorageWriteCommand::Delete {
            cf: ColumnFamilyName::Dag,
            key: block_hash.to_vec(),
        })
    }

    pub fn put_dag_tips(&self, key: &[u8], tips: &DagTipsValue) -> StorageResult<()> {
        let value = tips.to_bytes()?;
        self.enqueue_write(StorageWriteCommand::Put {
            cf: ColumnFamilyName::DagTips,
            key: key.to_vec(),
            value,
        })
    }

    pub fn put_current_dag_tips(&self, tips: &DagTipsValue) -> StorageResult<()> {
        self.put_dag_tips(b"current_tips", tips)
    }

    pub fn delete_dag_tips(&self, key: &[u8]) -> StorageResult<()> {
        self.enqueue_write(StorageWriteCommand::Delete {
            cf: ColumnFamilyName::DagTips,
            key: key.to_vec(),
        })
    }

    pub fn insert_mempool_transaction(&self, tx_hash: Vec<u8>, transaction: TransactionValue) {
        self.mempool_cache.insert(tx_hash, transaction);
    }

    pub fn get_mempool_transaction(&self, tx_hash: &[u8]) -> Option<TransactionValue> {
        self.mempool_cache
            .get(tx_hash)
            .map(|value| (*value).clone())
    }

    pub fn remove_mempool_transaction(&self, tx_hash: &[u8]) {
        self.mempool_cache.remove(tx_hash);
    }

    pub fn get_utxo_spent(
        &self,
        tx_hash: &[u8],
        output_index: u32,
    ) -> StorageResult<Option<UtxoSpentValue>> {
        let key = make_utxo_key(tx_hash, output_index);
        match self.db.get(ColumnFamilyName::UtxoSpent, &key)? {
            Some(raw) => Ok(Some(UtxoSpentValue::from_bytes(&raw)?)),
            None => Ok(None),
        }
    }

    pub fn get_verkle_state(&self, path: &[u8]) -> StorageResult<Option<VerkleStateValue>> {
        match self.db.get(ColumnFamilyName::VerkleState, path)? {
            Some(raw) => Ok(Some(VerkleStateValue::from_bytes(&raw)?)),
            None => Ok(None),
        }
    }

    pub fn get_dag_node(&self, block_hash: &[u8]) -> StorageResult<Option<DagNodeValue>> {
        match self.db.get(ColumnFamilyName::Dag, block_hash)? {
            Some(raw) => Ok(Some(DagNodeValue::from_bytes(&raw)?)),
            None => Ok(None),
        }
    }

    pub fn get_dag_tips(&self, key: &[u8]) -> StorageResult<Option<DagTipsValue>> {
        match self.db.get(ColumnFamilyName::DagTips, key)? {
            Some(raw) => Ok(Some(DagTipsValue::from_bytes(&raw)?)),
            None => Ok(None),
        }
    }

    pub fn get_current_dag_tips(&self) -> StorageResult<Option<DagTipsValue>> {
        match self.db.get(ColumnFamilyName::DagTips, b"current_tips")? {
            Some(raw) => Ok(Some(DagTipsValue::from_bytes(&raw)?)),
            None => Ok(None),
        }
    }

    pub fn scan_utxos_by_tx_hash(&self, tx_hash: &[u8]) -> StorageResult<Vec<(u32, UtxoValue)>> {
        let start_key = make_utxo_key(tx_hash, 0);
        let cf_handle = self.db.as_ref().inner()
            .cf_handle(ColumnFamilyName::Utxo.as_str())
            .ok_or_else(|| StorageError::DbError("missing utxo column family".to_string()))?;

        let mut results = Vec::new();
        let iter = self.db.as_ref().inner().iterator_cf(
            &cf_handle,
            IteratorMode::From(&start_key, Direction::Forward),
        );

        for item in iter {
            let (key, value): (Box<[u8]>, Box<[u8]>) = item?;
            let key = key.to_vec();
            let value = value.to_vec();
            if key.len() < 32 || &key[0..32] != tx_hash {
                break;
            }

            if let Some((parsed_hash, output_index)) = parse_utxo_key(&key) {
                if parsed_hash == tx_hash {
                    let utxo = UtxoValue::from_bytes(&value)?;
                    self.utxo_hot_cache.insert(key.to_vec(), utxo.clone());
                    results.push((output_index, utxo));
                } else {
                    break;
                }
            }
        }

        Ok(results)
    }

    pub fn scan_utxo_range(
        &self,
        start_key: &[u8],
        _end_key: &[u8],
        max_results: usize,
    ) -> StorageResult<Vec<(Vec<u8>, UtxoValue)>> {
        let cf_handle = self.db.as_ref().inner()
            .cf_handle(ColumnFamilyName::Utxo.as_str())
            .ok_or_else(|| StorageError::DbError("missing utxo column family".to_string()))?;

        let iter = self.db.as_ref().inner().iterator_cf(&cf_handle, IteratorMode::From(start_key, Direction::Forward));

        let mut results = Vec::new();
        for item in iter {
            let (key, value): (Box<[u8]>, Box<[u8]>) = item?;
            if results.len() >= max_results {
                break;
            }

            let utxo = UtxoValue::from_bytes(&value)?;
            self.utxo_hot_cache.insert(key.to_vec(), utxo.clone());
            results.push((key.to_vec(), utxo));
        }

        Ok(results)
    }

    pub fn scan_dag_nodes(&self, limit: usize) -> StorageResult<Vec<DagNodeValue>> {
        let cf_handle = self.db.as_ref().inner()
            .cf_handle(ColumnFamilyName::Dag.as_str())
            .ok_or_else(|| StorageError::DbError("missing dag column family".to_string()))?;

        let iter = self.db.as_ref().inner().iterator_cf(&cf_handle, IteratorMode::Start);
        let mut results = Vec::new();

        for item in iter {
            let (_key, value): (Box<[u8]>, Box<[u8]>) = item?;
            if results.len() >= limit {
                break;
            }

            let node = DagNodeValue::from_bytes(&value)?;
            results.push(node);
        }

        Ok(results)
    }

    pub fn scan_blocks(&self, limit: usize) -> StorageResult<Vec<BlockValue>> {
        let cf_handle = self.db.as_ref().inner()
            .cf_handle(ColumnFamilyName::Blocks.as_str())
            .ok_or_else(|| StorageError::DbError("missing blocks column family".to_string()))?;

        let iter = self.db.as_ref().inner().iterator_cf(&cf_handle, IteratorMode::Start);
        let mut results = Vec::new();

        for item in iter {
            let (key, value) = item?;
            if results.len() >= limit {
                break;
            }

            let block = BlockValue::from_bytes(&value)?;
            self.recent_block_cache.insert(key.to_vec(), block.clone());
            results.push(block);
        }

        Ok(results)
    }

}



#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::config::StorageConfig;
    use crate::storage::db::StorageDb;
    use tempfile::TempDir;

    #[test]
    fn test_utxo_hot_cache_round_trip() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let config = StorageConfig::new(temp_dir.path());
        let db = StorageDb::open_with_config(&config).expect("open db");
        let cache = StorageCacheLayer::new(db);

        let tx_hash = vec![0u8; 32];
        let utxo = UtxoValue::new(100, vec![1], vec![2], 1);

        cache.put_utxo(&tx_hash, 0, &utxo).expect("put utxo");
        let loaded = cache.get_utxo(&tx_hash, 0).expect("get utxo");

        assert_eq!(loaded, Some(utxo));
    }
}
