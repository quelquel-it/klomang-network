use std::path::Path;
use std::sync::Arc;

use rocksdb::{BlockBasedOptions, Cache, ColumnFamilyDescriptor, DB, DBCompactionStyle, DBCompressionType, Error, Options, Snapshot, WriteOptions, SliceTransform};

use crate::storage::batch::{WriteBatch, WriteOp};
use crate::storage::cf::{all_column_families, ColumnFamilyName};
use crate::storage::config::StorageConfig;
use crate::storage::metrics::{StorageMetrics, LatencyTimer};
use klomang_core::NoOpMetricsCollector;

// DB Configuration Constants
pub const MAX_OPEN_FILES: i32 = -1;
pub const USE_FSYNC: bool = false;
pub const WAL_TTL_SECONDS: i32 = 86_400; // 1 day
pub const WAL_SIZE_LIMIT_MB: u64 = 512;
pub const BYTES_PER_SYNC: u64 = 1 << 20; // 1 MiB
pub const MAX_BACKGROUND_JOBS: i32 = 4;
pub const DEFAULT_BLOCK_CACHE_SIZE: usize = 512 * 1024 * 1024; // 512 MB
pub const DEFAULT_WRITE_BUFFER_SIZE: usize = 64 * 1024 * 1024; // 64 MB
pub const DEFAULT_BLOCK_SIZE: usize = 32 * 1024; // 32 KB
pub const DEFAULT_BLOOM_BITS_PER_KEY: i32 = 10;
pub const DEFAULT_COMPRESSION_TYPE: rocksdb::DBCompressionType = rocksdb::DBCompressionType::Lz4;
pub const DEFAULT_COMPACTION_STYLE: DBCompactionStyle = DBCompactionStyle::Level;
pub const DEFAULT_TARGET_FILE_SIZE_BASE: u64 = 64 * 1024 * 1024; // 64 MB
pub const DEFAULT_MAX_BYTES_FOR_LEVEL_BASE: u64 = 256 * 1024 * 1024; // 256 MB
pub const DEFAULT_LEVEL_COMPACTION_DYNAMIC_LEVEL_BYTES: bool = true;

/// Configure column family options with prefix extractor for optimized reads
fn configure_cf_options(cf_name: ColumnFamilyName, base_options: &Options, config: &StorageConfig) -> Options {
    let mut cf_options = base_options.clone();

    // Enable prefix extractor for UTXO and related column families
    // This allows O(k) prefix seeks instead of full table scans
    match cf_name {
        ColumnFamilyName::Utxo | ColumnFamilyName::UtxoSpent => {
            // UTXO keys are: tx_hash (32 bytes) | output_index (4 bytes)
            // Extract first 32 bytes for prefix (transaction hash)
            let prefix_extractor = SliceTransform::create_fixed_prefix(32);
            cf_options.set_prefix_extractor(prefix_extractor);
        }
        ColumnFamilyName::Transactions => {
            // Transaction hash is 32 bytes, use whole key as prefix for efficiency
            let prefix_extractor = SliceTransform::create_fixed_prefix(32);
            cf_options.set_prefix_extractor(prefix_extractor);
        }
        _ => {
            // Other CFs don't use prefix extraction
        }
    }

    // Set block-based table factory with CF-specific optimizations
    let block_options = create_cf_block_based_options(cf_name, config);
    cf_options.set_block_based_table_factory(&block_options);

    // ✅ Compression strategy per column family
    // UTXO: No compression (hot random access needs speed)
    // UtxoSpent: Light compression (sequential scans benefit from compression)
    // Blocks/Headers/Transactions: Heavy compression (cold reference data)
    match cf_name {
        ColumnFamilyName::Utxo => {
            // Hot data: compression disabled for speed
            cf_options.set_compression_type(DBCompressionType::None);
        }
        ColumnFamilyName::UtxoSpent => {
            // Warm sequential data: light compression
            cf_options.set_compression_type(DBCompressionType::Lz4);
        }
        ColumnFamilyName::Blocks | ColumnFamilyName::Headers | ColumnFamilyName::Transactions => {
            // Cold reference data: use LZ4 for portability in test environments
            cf_options.set_compression_type(DBCompressionType::Lz4);
        }
        _ => {
            // Default compression
            cf_options.set_compression_type(DEFAULT_COMPRESSION_TYPE);
        }
    }

    cf_options
}

/// Creates BlockBasedOptions with CF-specific optimizations
/// 
/// Tunes each column family based on its access pattern:
/// - UTXO: Hot random access -> small blocks, high bloom bits, minimal compression
/// - UtxoSpent: Cold sequential scan -> large blocks, compression
/// - Blocks/Headers/Transactions: Cold reference data -> max compression, large blocks
fn create_cf_block_based_options(cf_name: ColumnFamilyName, config: &StorageConfig) -> BlockBasedOptions {
    let mut block_options = BlockBasedOptions::default();

    match cf_name {
        ColumnFamilyName::Utxo => {
            // ✅ Hot data: Aggressive caching for UTXO lookups (random access pattern)
            let cache = Cache::new_lru_cache(config.hot_data_cache_size / 2);
            block_options.set_block_cache(&cache);
            block_options.set_block_size(8 * 1024); // 8KB blocks for better cache hits
            block_options.set_bloom_filter(12.0, true); // Higher bloom bits for hot data
            block_options.set_cache_index_and_filter_blocks(true);
            block_options.set_pin_l0_filter_and_index_blocks_in_cache(true);
        },
        ColumnFamilyName::UtxoSpent => {
            // ✅ Cold sequential scan: Balance between cache and compression
            let cache = Cache::new_lru_cache(config.hot_data_cache_size / 2);
            block_options.set_block_cache(&cache);
            block_options.set_block_size(64 * 1024); // 64KB blocks for compression efficiency
            block_options.set_bloom_filter(8.0, true); // Moderate bloom bits
            block_options.set_cache_index_and_filter_blocks(true);
        },
        ColumnFamilyName::Blocks => {
            // ✅ Cold reference data: Maximize compression, reference access
            let cache = Cache::new_lru_cache(config.cold_data_cache_size / 3);
            block_options.set_block_cache(&cache);
            block_options.set_block_size(64 * 1024); // 64KB blocks
            block_options.set_bloom_filter(10.0, true);
            block_options.set_cache_index_and_filter_blocks(true);
        },
        ColumnFamilyName::Headers => {
            // ✅ Very cold reference: Minimal cache, maximum compression
            let cache = Cache::new_lru_cache(config.cold_data_cache_size / 3);
            block_options.set_block_cache(&cache);
            block_options.set_block_size(64 * 1024);
            block_options.set_bloom_filter(10.0, true);
        },
        ColumnFamilyName::Transactions => {
            // ✅ Cold reference data: Balance of compression and random access
            let cache = Cache::new_lru_cache(config.cold_data_cache_size / 3);
            block_options.set_block_cache(&cache);
            block_options.set_block_size(64 * 1024); // 64KB blocks
            block_options.set_bloom_filter(10.0, true);
            block_options.set_cache_index_and_filter_blocks(true);
        },
        _ => {
            // Default for other CFs
            let cache = Cache::new_lru_cache(config.block_cache_size);
            block_options.set_block_cache(&cache);
            block_options.set_block_size(config.block_size);
            block_options.set_bloom_filter(config.bloom_bits_per_key as f64, true);
        }
    }

    // Common optimizations for all column families
    block_options.set_cache_index_and_filter_blocks(true);
    block_options.set_pin_l0_filter_and_index_blocks_in_cache(true);
    block_options.set_whole_key_filtering(true);

    block_options
}

#[derive(Clone)]
pub struct StorageDb {
    inner: Arc<DB>,
    metrics: Arc<StorageMetrics>,
}

impl std::fmt::Debug for StorageDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageDb").finish()
    }
}

impl StorageDb {
    pub fn open<P: AsRef<Path>, Q: AsRef<Path>>(path: P, wal_dir: Q) -> Result<Self, Error> {
        let config = StorageConfig::new(path.as_ref()).with_wal_dir(wal_dir.as_ref().to_path_buf());
        let metrics = Arc::new(StorageMetrics::new(Box::new(NoOpMetricsCollector)));
        Self::open_with_config(&config, metrics)
    }

    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let wal_dir = path.as_ref().join("wal");
        let _metrics = Arc::new(StorageMetrics::new(Box::new(NoOpMetricsCollector)));
        Self::open(path.as_ref(), wal_dir)
    }

    pub fn open_with_config(config: &StorageConfig, metrics: Arc<StorageMetrics>) -> Result<Self, Error> {
        let mut options = Options::default();
        options.create_if_missing(true);
        options.create_missing_column_families(true);
        options.set_max_open_files(config.max_open_files);
        options.set_use_fsync(config.use_fsync);
        options.set_bytes_per_sync(config.bytes_per_sync);
        options.set_max_background_jobs(config.max_background_jobs);
        options.set_wal_dir(&config.wal_dir);
        options.set_wal_ttl_seconds(config.wal_ttl_seconds as u64);
        options.set_wal_size_limit_mb(config.wal_size_limit_mb);
        options.set_write_buffer_size(config.write_buffer_size);
        options.set_compaction_style(config.compaction_style);
        options.set_target_file_size_base(config.target_file_size_base);
        options.set_max_bytes_for_level_base(config.max_bytes_for_level_base);
        options.set_level_compaction_dynamic_level_bytes(config.level_compaction_dynamic_level_bytes);

        // Advanced optimizations
        if config.enable_direct_io {
            options.set_use_direct_reads(true);
            options.set_use_direct_io_for_flush_and_compaction(true);
        }
        if config.enable_pipelined_write {
            options.set_enable_pipelined_write(true);
        }
        options.set_ratelimiter(config.rate_limiter_bytes_per_second, 100 * 1000, 10); // 100ms refill, 10 fairness

        let cfs = all_column_families();
        let descriptors: Vec<ColumnFamilyDescriptor> = cfs
            .iter()
            .map(|name| {
                let cf_opts = configure_cf_options(*name, &options, config);
                ColumnFamilyDescriptor::new(name.as_str(), cf_opts)
            })
            .collect();

        let db = DB::open_cf_descriptors(&options, &config.db_path, descriptors)?;
        Ok(Self {
            inner: Arc::new(db),
            metrics,
        })
    }

    pub fn get(&self, cf_name: ColumnFamilyName, key: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        let metrics = Arc::clone(&self.metrics);
        let _timer = LatencyTimer::new(move |d| metrics.record_read_latency(d));
        let cf_handle = self
            .inner
            .cf_handle(cf_name.as_str())
            .expect("column family handle not found");

        self.inner.get_cf(&cf_handle, key)
    }

    pub fn put(&self, cf_name: ColumnFamilyName, key: &[u8], value: &[u8]) -> Result<(), Error> {
        let metrics = Arc::clone(&self.metrics);
        let _timer = LatencyTimer::new(move |d| metrics.record_write_latency(d));
        let cf_handle = self
            .inner
            .cf_handle(cf_name.as_str())
            .expect("column family handle not found");

        self.inner.put_cf(&cf_handle, key, value)
    }

    pub fn write_batch(&self, batch: WriteBatch) -> Result<(), Error> {
        let metrics = Arc::clone(&self.metrics);
        let _timer = LatencyTimer::new(move |d| metrics.record_write_latency(d));
        
        let mut rb = rocksdb::WriteBatch::default();
        for op in batch.into_inner() {
            match op {
                WriteOp::Put(key, value) => rb.put(&key, &value),
                WriteOp::Delete(key) => rb.delete(&key),
                WriteOp::PutCf(cf_name, key, value) => {
                    if let Some(cf_handle) = self.inner.cf_handle(&cf_name) {
                        rb.put_cf(&cf_handle, &key, &value);
                    }
                }
                WriteOp::DeleteCf(cf_name, key) => {
                    if let Some(cf_handle) = self.inner.cf_handle(&cf_name) {
                        rb.delete_cf(&cf_handle, &key);
                    }
                }
            }
        }
        self.inner.write(rb)
    }

    pub fn write_batch_no_wal(&self, batch: WriteBatch) -> Result<(), Error> {
        let metrics = Arc::clone(&self.metrics);
        let _timer = LatencyTimer::new(move |d| metrics.record_write_latency(d));
        
        let mut rb = rocksdb::WriteBatch::default();
        for op in batch.into_inner() {
            match op {
                WriteOp::Put(key, value) => rb.put(&key, &value),
                WriteOp::Delete(key) => rb.delete(&key),
                WriteOp::PutCf(cf_name, key, value) => {
                    if let Some(cf_handle) = self.inner.cf_handle(&cf_name) {
                        rb.put_cf(&cf_handle, &key, &value);
                    }
                }
                WriteOp::DeleteCf(cf_name, key) => {
                    if let Some(cf_handle) = self.inner.cf_handle(&cf_name) {
                        rb.delete_cf(&cf_handle, &key);
                    }
                }
            }
        }
        let mut write_options = WriteOptions::default();
        write_options.disable_wal(true);
        self.inner.write_opt(rb, &write_options)
    }

    pub fn delete(&self, cf_name: ColumnFamilyName, key: &[u8]) -> Result<(), Error> {
        let cf_handle = self
            .inner
            .cf_handle(cf_name.as_str())
            .expect("column family handle not found");

        self.inner.delete_cf(&cf_handle, key)
    }

    pub fn delete_range(
        &self,
        cf_name: ColumnFamilyName,
        start_key: &[u8],
        end_key: &[u8],
    ) -> Result<(), Error> {
        let cf_handle = self
            .inner
            .cf_handle(cf_name.as_str())
            .expect("column family handle not found");

        self.inner.delete_range_cf(&cf_handle, start_key, end_key)
    }

    pub fn exists(&self, cf_name: ColumnFamilyName, key: &[u8]) -> Result<bool, Error> {
        match self.get(cf_name, key)? {
            Some(_) => Ok(true),
            None => Ok(false),
        }
    }

    pub fn get_property(&self, property: &str) -> Result<Option<String>, Error> {
        self.inner.property_value(property)
    }

    pub fn flush(&self) -> Result<(), Error> {
        self.inner.flush()
    }

    pub fn compact_range(&self, cf_name: ColumnFamilyName, start_key: Option<&[u8]>, end_key: Option<&[u8]>) -> Result<(), Error> {
        let cf_handle = self
            .inner
            .cf_handle(cf_name.as_str())
            .expect("column family handle not found");

        self.inner.compact_range_cf(&cf_handle, start_key, end_key);
        Ok(())
    }

    /// Trigger manual compaction for all configured column families.
    ///
    /// This is intended for long-lived node maintenance points such as:
    /// - state pruning completion
    /// - initial block download / sync completion
    ///
    /// Manual compaction can help keep SST files balanced after large deletes or bulk imports.
    pub fn trigger_manual_compaction(&self) -> Result<(), Error> {
        for cf_name in all_column_families() {
            self.compact_range(*cf_name, None, None)?;
        }
        Ok(())
    }

    pub fn db_statistics(&self) -> Result<String, Error> {
        match self.inner.property_value("rocksdb.stats")? {
            Some(stats) => Ok(stats),
            None => Ok(String::from("No statistics available")),
        }
    }

    /// Extract raw data from RocksDB ticker and histogram statistics.
    /// Returns a map of statistic names to their values.
    pub fn get_storage_stats(&self) -> Result<std::collections::HashMap<String, u64>, Error> {
        let mut stats = std::collections::HashMap::new();

        // Extract ticker statistics
        let tickers = vec![
            ("block_cache_hit", "rocksdb.block.cache.hit"),
            ("block_cache_miss", "rocksdb.block.cache.miss"),
            ("bytes_written", "rocksdb.bytes.written"),
            ("bytes_read", "rocksdb.bytes.read"),
            ("number_keys_written", "rocksdb.number.keys.written"),
            ("number_keys_read", "rocksdb.number.keys.read"),
            ("write_done_by_self", "rocksdb.write.done.by.self"),
            ("write_done_by_other", "rocksdb.write.done.by.other"),
            ("write_timedout", "rocksdb.write.timedout"),
            ("write_with_wal", "rocksdb.write.wal"),
            ("compact_read_bytes", "rocksdb.compact.read.bytes"),
            ("compact_write_bytes", "rocksdb.compact.write.bytes"),
            ("flush_write_bytes", "rocksdb.flush.write.bytes"),
        ];

        for (key, property) in tickers {
            if let Some(value_str) = self.inner.property_value(property)? {
                if let Ok(value) = value_str.parse::<u64>() {
                    stats.insert(key.to_string(), value);
                }
            }
        }

        // Extract histogram statistics (average values)
        let histograms = vec![
            ("db_write", "rocksdb.db.write.micros"),
            ("db_get", "rocksdb.db.get.micros"),
            ("db_multiget", "rocksdb.db.multiget.micros"),
            ("compaction_time", "rocksdb.compaction.times.micros"),
        ];

        for (key, property) in histograms {
            if let Some(value_str) = self.inner.property_value(property)? {
                // Parse histogram format: "Count: X, Sum: Y, Avg: Z, ..."
                if let Some(avg_part) = value_str.split(", ").find(|s| s.starts_with("Avg:")) {
                    if let Some(avg_str) = avg_part.strip_prefix("Avg: ") {
                        if let Ok(avg) = avg_str.parse::<u64>() {
                            stats.insert(key.to_string(), avg);
                        }
                    }
                }
            }
        }


        Ok(stats)
    }

    /// Update cache hit ratio metrics berdasarkan data dari RocksDB statistics.
    pub fn update_cache_metrics(&self) -> Result<(), Error> {
        let stats = self.get_storage_stats()?;
        if let (Some(hits), Some(misses)) = (stats.get("block_cache_hit"), stats.get("block_cache_miss")) {
            let total = hits + misses;
            if total > 0 {
                let ratio = *hits as f64 / total as f64;
                self.metrics.update_cache_hit_ratio(ratio);
            }
        }
        Ok(())
    }

    /// Get reference to storage metrics for external access.
    pub fn metrics(&self) -> &Arc<StorageMetrics> {
        &self.metrics
    }

    /// Get reference to the underlying RocksDB instance.
    pub fn inner(&self) -> &DB {
        &self.inner
    }

    /// Get Arc reference to the underlying RocksDB instance for safe sharing
    /// across threads without additional cloning overhead.
    pub fn inner_arc(&self) -> Arc<DB> {
        Arc::clone(&self.inner)
    }

    /// Get reference to the underlying RocksDB instance (alias).
    pub fn rocksdb(&self) -> &DB {
        &self.inner
    }

    /// Create a consistent RocksDB snapshot for point-in-time backup and validation.
    pub fn snapshot(&self) -> Snapshot<'_> {
        self.inner.snapshot()
    }

    /// Clear all data and reset database to clean state.
    /// ⚠️ DESTRUCTIVE: Deletes all content from all column families.
    /// Useful for testing, corruption recovery, or full data reset.
    pub fn clear_and_reset(&self) -> Result<(), Error> {
        let cfs = all_column_families();
        for &cf_name in cfs {
            self.delete_range(cf_name, &[], &[0xFF; 255])?;
        }
        
        // Flush to persist deletions
        self.inner.flush()?;
        Ok(())
    }
}
