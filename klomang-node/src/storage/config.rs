use klomang_core::Config as CoreConfig;
use rocksdb::DBCompactionStyle;
use std::path::PathBuf;

use super::db::{
    BYTES_PER_SYNC, DEFAULT_BLOCK_CACHE_SIZE, DEFAULT_BLOCK_SIZE, DEFAULT_BLOOM_BITS_PER_KEY,
    DEFAULT_COMPACTION_STYLE, DEFAULT_LEVEL_COMPACTION_DYNAMIC_LEVEL_BYTES,
    DEFAULT_MAX_BYTES_FOR_LEVEL_BASE, DEFAULT_TARGET_FILE_SIZE_BASE, DEFAULT_WRITE_BUFFER_SIZE,
    MAX_BACKGROUND_JOBS, MAX_OPEN_FILES, USE_FSYNC, WAL_SIZE_LIMIT_MB, WAL_TTL_SECONDS,
};

/// Configuration for RocksDB storage backend
#[derive(Debug, Clone)]
pub struct StorageConfig {
    pub db_path: PathBuf,
    pub wal_dir: PathBuf,
    pub max_open_files: i32,
    pub use_fsync: bool,
    pub wal_ttl_seconds: i32,
    pub wal_size_limit_mb: u64,
    pub bytes_per_sync: u64,
    pub max_background_jobs: i32,
    pub write_buffer_size: usize,
    pub block_cache_size: usize,
    pub block_size: usize,
    pub bloom_bits_per_key: i32,
    pub compaction_style: DBCompactionStyle,
    pub target_file_size_base: u64,
    pub max_bytes_for_level_base: u64,
    pub level_compaction_dynamic_level_bytes: bool,
    // Advanced optimization fields
    pub rate_limiter_bytes_per_second: i64,
    pub enable_direct_io: bool,
    pub enable_pipelined_write: bool,
    pub hot_data_cache_size: usize,
    pub cold_data_cache_size: usize,
}

impl StorageConfig {
    pub fn new<P: Into<PathBuf>>(db_path: P) -> Self {
        let db_path = db_path.into();
        let wal_dir = db_path.join("wal");

        let base_config = Self {
            db_path,
            wal_dir,
            max_open_files: MAX_OPEN_FILES,
            use_fsync: USE_FSYNC,
            wal_ttl_seconds: WAL_TTL_SECONDS,
            wal_size_limit_mb: WAL_SIZE_LIMIT_MB,
            bytes_per_sync: BYTES_PER_SYNC,
            max_background_jobs: MAX_BACKGROUND_JOBS,
            write_buffer_size: DEFAULT_WRITE_BUFFER_SIZE,
            block_cache_size: DEFAULT_BLOCK_CACHE_SIZE,
            block_size: DEFAULT_BLOCK_SIZE,
            bloom_bits_per_key: DEFAULT_BLOOM_BITS_PER_KEY,
            compaction_style: DEFAULT_COMPACTION_STYLE,
            target_file_size_base: DEFAULT_TARGET_FILE_SIZE_BASE,
            max_bytes_for_level_base: DEFAULT_MAX_BYTES_FOR_LEVEL_BASE,
            level_compaction_dynamic_level_bytes: DEFAULT_LEVEL_COMPACTION_DYNAMIC_LEVEL_BYTES,
            rate_limiter_bytes_per_second: 100 * 1024 * 1024, // 100 MB/s
            enable_direct_io: true,
            enable_pipelined_write: true,
            hot_data_cache_size: 1024 * 1024 * 1024, // 1 GB for hot data
            cold_data_cache_size: 128 * 1024 * 1024, // 128 MB for cold data
        };

        base_config.with_core_config(&CoreConfig::default())
    }

    pub fn with_core_config(mut self, core_config: &CoreConfig) -> Self {
        // Adjust based on hardware capabilities declared by core.
        self.max_background_jobs = std::cmp::max(1, core_config.num_cpus as i32);
        self.rate_limiter_bytes_per_second =
            (core_config.disk_write_bandwidth_mbps * 1024 * 1024) as i64;
        self.hot_data_cache_size = (core_config.total_memory_mb / 4) * 1024 * 1024; // 25% of RAM for hot data
        self.cold_data_cache_size = (core_config.total_memory_mb / 16) * 1024 * 1024; // 6.25% of RAM for cold data
        self
    }

    pub fn with_wal_dir<P: Into<PathBuf>>(mut self, wal_dir: P) -> Self {
        self.wal_dir = wal_dir.into();
        self
    }

    pub fn with_max_open_files(mut self, max_files: i32) -> Self {
        self.max_open_files = max_files;
        self
    }

    pub fn with_wal_ttl_seconds(mut self, seconds: i32) -> Self {
        self.wal_ttl_seconds = seconds;
        self
    }

    pub fn with_wal_size_limit_mb(mut self, mb: u64) -> Self {
        self.wal_size_limit_mb = mb;
        self
    }

    pub fn with_bytes_per_sync(mut self, bytes: u64) -> Self {
        self.bytes_per_sync = bytes;
        self
    }

    pub fn with_max_background_jobs(mut self, jobs: i32) -> Self {
        self.max_background_jobs = jobs;
        self
    }

    pub fn with_write_buffer_size(mut self, size: usize) -> Self {
        self.write_buffer_size = size;
        self
    }

    pub fn with_block_cache_size(mut self, size: usize) -> Self {
        self.block_cache_size = size;
        self
    }

    pub fn with_block_size(mut self, size: usize) -> Self {
        self.block_size = size;
        self
    }

    pub fn with_bloom_bits_per_key(mut self, bits: i32) -> Self {
        self.bloom_bits_per_key = bits;
        self
    }

    pub fn with_compaction_style(mut self, style: DBCompactionStyle) -> Self {
        self.compaction_style = style;
        self
    }

    pub fn with_target_file_size_base(mut self, size: u64) -> Self {
        self.target_file_size_base = size;
        self
    }

    pub fn with_max_bytes_for_level_base(mut self, size: u64) -> Self {
        self.max_bytes_for_level_base = size;
        self
    }

    pub fn with_level_compaction_dynamic_level_bytes(mut self, enabled: bool) -> Self {
        self.level_compaction_dynamic_level_bytes = enabled;
        self
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self::new("./data")
    }
}
