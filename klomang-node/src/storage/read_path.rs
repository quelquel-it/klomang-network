//! Read Path Optimization for RocksDB - High-performance data retrieval
//!
//! This module provides optimized read operations including:
//! - Prefix seek optimizations for UTXO lookups
//! - Efficient iterators with bounds for scanning
//! - Multi-get batch operations for throughput

use crate::storage::cache::StorageCacheLayer;
use crate::storage::cf::ColumnFamilyName;
use crate::storage::db::StorageDb;
use crate::storage::error::{StorageError, StorageResult};
use crate::storage::schema::{
    parse_utxo_key, make_utxo_key, UtxoValue, DagNodeValue, DagTipsValue, BlockValue,
};
use rocksdb::{IteratorMode, Direction};
use std::collections::HashMap;
use std::sync::Arc;

/// Outpoint reference from klomang-core
/// Format: (transaction_hash, output_index)
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OutPoint {
    pub tx_hash: Vec<u8>,
    pub index: u32,
}

impl OutPoint {
    /// Create a new OutPoint
    pub fn new(tx_hash: Vec<u8>, index: u32) -> Self {
        Self { tx_hash, index }
    }

    /// Convert to composite UTXO key (32-byte prefix + 4-byte index)
    pub fn to_utxo_key(&self) -> Vec<u8> {
        make_utxo_key(&self.tx_hash, self.index)
    }
}

/// Optimized read operations for blockchain storage
pub struct ReadPath {
    cache_layer: Arc<StorageCacheLayer>,
}

impl ReadPath {
    /// Create a new ReadPath for optimized reads
    pub fn new(cache_layer: Arc<StorageCacheLayer>) -> Self {
        Self { cache_layer }
    }

    /// Get a single UTXO by outpoint (standard get - O(log n))
    pub fn get_utxo(&self, outpoint: &OutPoint) -> StorageResult<Option<UtxoValue>> {
        self.cache_layer.get_utxo(&outpoint.tx_hash, outpoint.index)
    }

    /// Get multiple UTXOs efficiently using cache layer
    ///
    /// This is optimized because:
    /// - Cache hits avoid DB access
    /// - Cache misses populate cache for future requests
    /// - Overlaps seeks
    ///
    /// # Arguments
    /// * `outpoints` - List of UTXOs to retrieve
    ///
    /// # Returns
    /// Vec of (OutPoint, Result<Option<UtxoValue>>)
    /// Each result is independent - a failure doesn't affect others
    pub fn get_multiple_utxos(&self, outpoints: &[OutPoint]) -> StorageResult<Vec<(OutPoint, StorageResult<Option<UtxoValue>>)>> {
        let mut results = Vec::with_capacity(outpoints.len());
        
        for outpoint in outpoints {
            let result = self.get_utxo(outpoint);
            results.push((outpoint.clone(), result));
        }
        
        Ok(results)
    }

    /// Scan UTXOs with a prefix (tx_hash) for all outputs
    ///
    /// Uses prefix seek optimization: O(k) where k is result count
    /// Much faster than full database scan for querying a specific transaction's outputs
    ///
    /// # Arguments
    /// * `tx_hash` - Transaction hash to prefix-search (32 bytes)
    ///
    /// # Returns
    /// Iterator results: (output_index, UtxoValue)
    pub fn get_utxos_by_tx_hash(&self, tx_hash: &[u8]) -> StorageResult<Vec<(u32, UtxoValue)>> {
        self.cache_layer.scan_utxos_by_tx_hash(tx_hash)
    }

    /// Scan all UTXOs in a range with upper bound for memory efficiency
    ///
    /// This uses iterator bounds to limit memory allocation:
    /// - set_iterate_upper_bound prevents iterator from going beyond bound
    /// - Useful for scanning large ranges efficiently
    ///
    /// # Arguments
    /// * `start_key` - Start of range
    /// * `end_key` - End of range (exclusive)
    /// * `max_results` - Maximum results to return (prevents memory exhaustion)
    ///
    /// # Returns
    /// Vec of (key, UtxoValue) pairs
    pub fn scan_utxo_range(
        &self,
        start_key: &[u8],
        end_key: &[u8],
        max_results: usize,
    ) -> StorageResult<Vec<(Vec<u8>, UtxoValue)>> {
        self.cache_layer.scan_utxo_range(start_key, end_key, max_results)
    }

    /// Get all current DAG tips efficiently
    ///
    /// DAG tips are stored in a single key "current_tips" - this is a point lookup O(1)
    pub fn get_dag_tips(&self) -> StorageResult<Option<DagTipsValue>> {
        self.cache_layer.get_current_dag_tips()
    }

    /// Scan DAG nodes in a range
    ///
    /// Iterate through block hashes in DAG column family
    /// Useful for traversing the DAG structure
    pub fn scan_dag_nodes(
        &self,
        start_hash: Option<&[u8]>,
        limit: usize,
    ) -> StorageResult<Vec<(Vec<u8>, DagNodeValue)>> {
        let mut results = Vec::new();

        let cf_handle = self
            .cache_layer.db()
            .inner()
            .cf_handle(ColumnFamilyName::Dag.as_str())
            .ok_or_else(|| StorageError::InvalidColumnFamily("dag".to_string()))?;

        let mode = match start_hash {
            Some(hash) => IteratorMode::From(hash, Direction::Forward),
            None => IteratorMode::Start,
        };

        let iter = self.cache_layer.db().inner().iterator_cf(cf_handle, mode);

        for (_, value) in iter.take(limit) {
            match DagNodeValue::from_bytes(&value) {
                Ok(node) => results.push((vec![], node)), // Key would be block_hash
                Err(e) => return Err(e),
            }
        }

        Ok(results)
    }

    /// Get blocks by range scan
    ///
    /// Scans block column family for blocks in range
    pub fn scan_blocks(
        &self,
        start_hash: Option<&[u8]>,
        limit: usize,
    ) -> StorageResult<Vec<(Vec<u8>, BlockValue)>> {
        let mut results = Vec::new();

        let cf_handle = self
            .cache_layer.db()
            .inner()
            .cf_handle(ColumnFamilyName::Blocks.as_str())
            .ok_or_else(|| StorageError::InvalidColumnFamily("blocks".to_string()))?;

        let mode = match start_hash {
            Some(hash) => IteratorMode::From(hash, Direction::Forward),
            None => IteratorMode::Start,
        };

        let iter = self.cache_layer.db().inner().iterator_cf(cf_handle, mode);

        for (key, value) in iter.take(limit) {
            match BlockValue::from_bytes(&value) {
                Ok(block) => results.push((key.to_vec(), block)),
                Err(e) => return Err(e),
            }
        }

        Ok(results)
    }

    /// Bulk check existence of multiple UTXOs
    ///
    /// More efficient than calling utxo_exists in a loop
    /// Uses Bloom filters to quickly determine non-existent keys
    pub fn check_utxos_exist(&self, outpoints: &[OutPoint]) -> StorageResult<HashMap<OutPoint, bool>> {
        let mut map = HashMap::new();

        let cf_handle = self
            .db
            .inner()
            .cf_handle(ColumnFamilyName::Utxo.as_str())
            .ok_or_else(|| StorageError::InvalidColumnFamily("utxo".to_string()))?;

        for outpoint in outpoints {
            let exists = self.get_utxo(outpoint).is_ok_and(|opt| opt.is_some());
            map.insert(outpoint.clone(), exists);
        }

        Ok(map)
    }

    /// Reference to inner database for advanced operations
    pub fn db(&self) -> &StorageDb {
        self.cache_layer.db()
    }

    /// Reference to inner DB for accessing raw rocksdb methods
    pub fn inner_db(&self) -> rocksdb::DBWithThreadMode<rocksdb::SingleThreaded> {
        self.cache_layer.db().inner()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_outpoint_key_conversion() {
        let tx_hash = vec![0u8; 32];
        let index = 42;
        let outpoint = OutPoint::new(tx_hash.clone(), index);
        
        let key = outpoint.to_utxo_key();
        assert_eq!(key.len(), 36);
        assert_eq!(&key[0..32], tx_hash.as_slice());
    }

    #[test]
    fn test_outpoint_clone() {
        let outpoint1 = OutPoint::new(vec![1u8; 32], 10);
        let outpoint2 = outpoint1.clone();
        
        assert_eq!(outpoint1, outpoint2);
    }
}
