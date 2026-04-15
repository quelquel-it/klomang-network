// Integration test example for RocksDB storage

#[cfg(test)]
mod tests {
    use klomang_node::storage::{StorageDb, StorageConfig, ColumnFamilyName, WriteBatch, StorageCacheLayer, KvStore, ReadPath};
    use std::path::PathBuf;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[test]
    fn test_storage_initialization() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let db_path = temp_dir.path();
        let wal_dir = db_path.join("wal");

        let result = StorageDb::open(&db_path, &wal_dir);
        assert!(result.is_ok(), "Failed to initialize StorageDb");
    }

    #[test]
    fn test_storage_config() {
        let config = StorageConfig::new("./test_data");
        assert_eq!(config.max_open_files, -1);
        assert_eq!(config.use_fsync, false);
        assert_eq!(config.wal_ttl_seconds, 86_400);
        assert_eq!(config.wal_size_limit_mb, 512);
    }

    #[test]
    fn test_storage_config_builder() {
        let config = StorageConfig::new("./test_data")
            .with_wal_ttl_seconds(172_800)
            .with_wal_size_limit_mb(1024)
            .with_max_background_jobs(8);

        assert_eq!(config.wal_ttl_seconds, 172_800);
        assert_eq!(config.wal_size_limit_mb, 1024);
        assert_eq!(config.max_background_jobs, 8);
    }

    #[test]
    fn test_write_batch_creation() {
        let mut batch = WriteBatch::new();
        batch.put(b"key1", b"value1");
        batch.put_cf("default", b"key2", b"value2");
        batch.delete(b"key3");
        batch.delete_cf("default", b"key4");
        // Batch is successfully created and operations are recorded
    }

    #[test]
    fn test_cache_layer_initialization() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let db_path = temp_dir.path();
        let wal_dir = db_path.join("wal");

        let db = StorageDb::open(&db_path, &wal_dir).expect("Failed to open DB");
        let cache_layer = StorageCacheLayer::new(db);
        
        // Cache layer should be created successfully
        assert!(true); // If we reach here, initialization worked
    }

    #[test]
    fn test_kv_store_with_cache() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let db_path = temp_dir.path();
        let wal_dir = db_path.join("wal");

        let db = StorageDb::open(&db_path, &wal_dir).expect("Failed to open DB");
        let cache_layer = StorageCacheLayer::new(db);
        let kv_store = KvStore::new(Arc::new(cache_layer));
        
        // KvStore should be created successfully
        assert!(true);
    }

    #[test]
    fn test_read_path_with_cache() {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let db_path = temp_dir.path();
        let wal_dir = db_path.join("wal");

        let db = StorageDb::open(&db_path, &wal_dir).expect("Failed to open DB");
        let cache_layer = StorageCacheLayer::new(db);
        let read_path = ReadPath::new(Arc::new(cache_layer));
        
        // ReadPath should be created successfully
        assert!(true);
    }
}
