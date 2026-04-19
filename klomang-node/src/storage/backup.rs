use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use rocksdb::backup::{BackupEngine, BackupEngineOptions, RestoreOptions};
use rocksdb::{IteratorMode, Snapshot};
use serde::{Deserialize, Serialize};

use crate::storage::db::StorageDb;
use crate::storage::error::{StorageError, StorageResult};
use crate::storage::cf::ColumnFamilyName;

use klomang_core::core::crypto::Hash;
use klomang_core::core::state_manager::StateManager;
use klomang_core::core::state::storage::Storage;

/// Metadata for backup validation and integrity checking
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BackupMetadata {
    pub backup_id: u32,
    pub timestamp: u64,
    pub last_block_hash: Hash,
    pub state_root: Hash,
    pub total_blocks: u64,
    pub total_transactions: u64,
    pub total_utxos: u64,
}

/// Consistent snapshot of database state
pub struct DatabaseSnapshot<'a> {
    snapshot: Snapshot<'a>,
    metadata: BackupMetadata,
}

impl<'a> DatabaseSnapshot<'a> {
    /// Create a consistent snapshot capturing current database state
    pub fn create(db: &'a StorageDb, state_manager: &mut dyn StateManagerInterface) -> StorageResult<Self> {
        // Create RocksDB snapshot for consistency
        let snapshot = db.snapshot();

        // Gather metadata from current state
        let metadata = Self::gather_metadata(db, state_manager)?;

        Ok(Self {
            snapshot,
            metadata,
        })
    }

    /// Gather comprehensive metadata for backup validation
    fn gather_metadata(db: &StorageDb, state_manager: &mut dyn StateManagerInterface) -> StorageResult<BackupMetadata> {
        // Get current state information from StateManager
        let state_root = state_manager.get_current_state_root();
        let last_block_hash = state_manager.get_last_block_hash();

        // Count records in each column family
        let total_blocks = Self::count_records(db, ColumnFamilyName::Blocks)?;
        let total_transactions = Self::count_records(db, ColumnFamilyName::Transactions)?;
        let total_utxos = Self::count_records(db, ColumnFamilyName::Utxo)?;

        let metadata = BackupMetadata {
            backup_id: 0, // Will be set by BackupEngine
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            last_block_hash,
            state_root,
            total_blocks,
            total_transactions,
            total_utxos,
        };

        Ok(metadata)
    }

    /// Count records in a specific column family
    fn count_records(db: &StorageDb, cf: ColumnFamilyName) -> StorageResult<u64> {
        let cf_handle = db.inner().cf_handle(cf.as_str())
            .ok_or_else(|| StorageError::InvalidColumnFamily(cf.as_str().to_string()))?;
        let iter = db.inner().iterator_cf(&cf_handle, IteratorMode::Start);

        let mut count = 0u64;
        for _ in iter {
            count += 1;
        }
        Ok(count)
    }

    /// Get the RocksDB snapshot
    pub fn snapshot(&self) -> &Snapshot<'a> {
        &self.snapshot
    }

    /// Get backup metadata
    pub fn metadata(&self) -> &BackupMetadata {
        &self.metadata
    }
}

/// Backup engine for managing database backups
pub struct DatabaseBackupEngine {
    engine: BackupEngine,
    backup_dir: PathBuf,
    max_backups: usize,
}

impl DatabaseBackupEngine {
    /// Create a new backup engine
    pub fn new(backup_dir: &Path, max_backups: usize) -> StorageResult<Self> {
        // Ensure backup directory exists
        fs::create_dir_all(backup_dir).map_err(|e| {
            StorageError::OperationFailed(format!("Failed to create backup directory: {}", e))
        })?;

        let options = BackupEngineOptions::new(backup_dir).map_err(|e| {
            StorageError::OperationFailed(format!("Failed to create backup engine options: {}", e))
        })?;
        let env = rocksdb::Env::new().map_err(|e| {
            StorageError::OperationFailed(format!("Failed to create RocksDB env: {}", e))
        })?;
        let engine = BackupEngine::open(&options, &env).map_err(|e| {
            StorageError::OperationFailed(format!("Failed to open backup engine: {}", e))
        })?;

        Ok(Self {
            engine,
            backup_dir: backup_dir.to_path_buf(),
            max_backups,
        })
    }

    /// Create a backup from a database snapshot
    pub fn create_backup(&mut self, db: &StorageDb, snapshot: &DatabaseSnapshot) -> StorageResult<u32> {
        self.engine.create_new_backup_flush(db.inner(), true).map_err(|e| {
            StorageError::OperationFailed(format!("Failed to create backup: {}", e))
        })?;

        let backup_id = self.engine.get_backup_info()
            .iter()
            .map(|info| info.backup_id)
            .max()
            .ok_or_else(|| StorageError::OperationFailed("Unable to resolve backup id after backup creation".to_string()))?;

        self.engine.verify_backup(backup_id).map_err(|e| {
            StorageError::OperationFailed(format!("Backup verification failed for backup {}: {}", backup_id, e))
        })?;

        self.save_backup_metadata(backup_id, snapshot.metadata())?;
        self.cleanup_old_backups()?;

        Ok(backup_id)
    }

    /// Save backup metadata to a JSON file
    fn save_backup_metadata(&self, backup_id: u32, metadata: &BackupMetadata) -> StorageResult<()> {
        let mut metadata_with_id = metadata.clone();
        metadata_with_id.backup_id = backup_id;

        let metadata_path = self.backup_dir.join(format!("backup_{}_metadata.json", backup_id));
        let json = serde_json::to_string_pretty(&metadata_with_id).map_err(|e| {
            StorageError::SerializationError(format!("Failed to serialize metadata: {}", e))
        })?;

        fs::write(&metadata_path, json).map_err(|e| {
            StorageError::OperationFailed(format!("Failed to write metadata file: {}", e))
        })?;

        Ok(())
    }

    /// Clean up old backups, keeping only the most recent N backups
    fn cleanup_old_backups(&mut self) -> StorageResult<()> {
        self.engine.purge_old_backups(self.max_backups).map_err(|e| {
            StorageError::OperationFailed(format!("Failed to purge old backups: {}", e))
        })?;

        let keep_ids: HashSet<u32> = self.engine.get_backup_info().iter().map(|info| info.backup_id).collect();

        for entry in fs::read_dir(&self.backup_dir).map_err(|e| {
            StorageError::OperationFailed(format!("Failed to read backup directory for cleanup: {}", e))
        })? {
            let entry = entry.map_err(|e| StorageError::OperationFailed(format!("Failed to read backup directory entry: {}", e)))?;
            let file_name = entry.file_name().into_string().map_err(|_| {
                StorageError::OperationFailed("Invalid backup metadata filename".into())
            })?;

            if file_name.starts_with("backup_") && file_name.ends_with("_metadata.json") {
                if let Some(id_token) = file_name.strip_prefix("backup_").and_then(|suffix| suffix.strip_suffix("_metadata.json")) {
                    if let Ok(backup_id) = id_token.parse::<u32>() {
                        if !keep_ids.contains(&backup_id) {
                            fs::remove_file(entry.path()).map_err(|e| {
                                StorageError::OperationFailed(format!("Failed to remove old metadata file: {}", e))
                            })?;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Get information about all available backups
    pub fn get_backup_info(&self) -> Vec<rocksdb::backup::BackupEngineInfo> {
        self.engine.get_backup_info()
    }

    /// Load backup metadata
    pub fn load_backup_metadata(&self, backup_id: u32) -> StorageResult<BackupMetadata> {
        let metadata_path = self.backup_dir.join(format!("backup_{}_metadata.json", backup_id));
        let json = fs::read_to_string(&metadata_path).map_err(|e| {
            StorageError::OperationFailed(format!("Failed to read metadata file: {}", e))
        })?;

        serde_json::from_str(&json).map_err(|e| {
            StorageError::SerializationError(format!("Failed to deserialize metadata: {}", e))
        })
    }
}

/// Restore engine for database recovery
pub struct DatabaseRestoreEngine;

impl DatabaseRestoreEngine {
    /// Restore database from backup and return backup metadata for validation.
    pub fn restore_from_backup(backup_path: &Path, restore_path: &Path, backup_id: u32) -> StorageResult<BackupMetadata> {
        // Ensure restore directory exists and is empty
        if restore_path.exists() {
            fs::remove_dir_all(restore_path).map_err(|e| {
                StorageError::OperationFailed(format!("Failed to clean restore directory: {}", e))
            })?;
        }
        fs::create_dir_all(restore_path).map_err(|e| {
            StorageError::OperationFailed(format!("Failed to create restore directory: {}", e))
        })?;

        let wal_restore_dir = restore_path.join("wal");
        fs::create_dir_all(&wal_restore_dir).map_err(|e| {
            StorageError::OperationFailed(format!("Failed to create WAL restore directory: {}", e))
        })?;

        // Open backup engine
        let options = BackupEngineOptions::new(backup_path).map_err(|e| {
            StorageError::OperationFailed(format!("Failed to create backup engine options: {}", e))
        })?;
        let env = rocksdb::Env::new().map_err(|e| {
            StorageError::OperationFailed(format!("Failed to create RocksDB env: {}", e))
        })?;
        let mut engine = BackupEngine::open(&options, &env).map_err(|e| {
            StorageError::OperationFailed(format!("Failed to open backup engine: {}", e))
        })?;

        // Restore from specific backup
        let restore_options = RestoreOptions::default();
        engine.restore_from_backup(restore_path, &wal_restore_dir, &restore_options, backup_id).map_err(|e| {
            StorageError::OperationFailed(format!("Failed to restore from backup: {}", e))
        })?;

        engine.verify_backup(backup_id).map_err(|e| {
            StorageError::OperationFailed(format!("Backup verification failed during restore: {}", e))
        })?;

        let metadata_path = backup_path.join(format!("backup_{}_metadata.json", backup_id));
        let metadata_json = fs::read_to_string(&metadata_path).map_err(|e| {
            StorageError::OperationFailed(format!("Failed to read backup metadata: {}", e))
        })?;

        let metadata: BackupMetadata = serde_json::from_str(&metadata_json).map_err(|e| {
            StorageError::SerializationError(format!("Failed to parse backup metadata: {}", e))
        })?;

        Ok(metadata)
    }

    /// Validate restored database integrity
    pub fn validate_restored_database(db: &StorageDb, expected_metadata: &BackupMetadata, state_manager: &mut dyn StateManagerInterface) -> StorageResult<()> {
        // Verify state root matches using core verification logic
        state_manager.verify_state_root(expected_metadata.state_root.clone())?;

        // Verify last block hash matches
        let current_last_block = state_manager.get_last_block_hash();
        if current_last_block != expected_metadata.last_block_hash {
            return Err(StorageError::OperationFailed(
                format!("Last block hash mismatch: expected {:?}, got {:?}", expected_metadata.last_block_hash, current_last_block)
            ));
        }

        // Verify record counts
        let actual_blocks = DatabaseSnapshot::count_records(db, ColumnFamilyName::Blocks)?;
        if actual_blocks != expected_metadata.total_blocks {
            return Err(StorageError::OperationFailed(
                format!("Block count mismatch: expected {}, got {}", expected_metadata.total_blocks, actual_blocks)
            ));
        }

        let actual_transactions = DatabaseSnapshot::count_records(db, ColumnFamilyName::Transactions)?;
        if actual_transactions != expected_metadata.total_transactions {
            return Err(StorageError::OperationFailed(
                format!("Transaction count mismatch: expected {}, got {}", expected_metadata.total_transactions, actual_transactions)
            ));
        }

        let actual_utxos = DatabaseSnapshot::count_records(db, ColumnFamilyName::Utxo)?;
        if actual_utxos != expected_metadata.total_utxos {
            return Err(StorageError::OperationFailed(
                format!("UTXO count mismatch: expected {}, got {}", expected_metadata.total_utxos, actual_utxos)
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    use klomang_core::core::crypto::Hash;
    use klomang_core::core::state::storage::MemoryStorage;
    use klomang_core::core::state::v_trie::VerkleTree;
    use klomang_core::core::StateManager;

    use crate::storage::db::StorageDb;
    use crate::storage::concurrency::StorageEngine;
    use crate::storage::cf::ColumnFamilyName;
    use crate::storage::schema::BlockValue;

    fn create_test_setup() -> (TempDir, Arc<StorageEngine>, StateManager<MemoryStorage>) {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let db_path = temp_dir.path().join("test.db");
        let db = StorageDb::new(&db_path).expect("Failed to create test database");
        let storage = Arc::new(StorageEngine::new(db).expect("Failed to create storage engine"));

        // Create StateManager
        let mem_storage = MemoryStorage::new();
        let tree = VerkleTree::new(mem_storage).expect("Failed to create VerkleTree");
    let state_manager = StateManager::new(tree).expect("Failed to create StateManager");
        let block_value = BlockValue {
            hash: Hash([1u8; 32]).0.to_vec(),
            header_bytes: vec![1, 2, 3],
            transactions: vec![vec![4, 5, 6]],
            timestamp: 1000,
        };
        storage.writer.enqueue(vec![crate::storage::concurrency::StorageWriteCommand::Put {
            cf: ColumnFamilyName::Blocks,
            key: Hash([1u8; 32]).0.to_vec(),
            value: block_value.to_bytes().unwrap(),
        }]);

        // Create snapshot
        let snapshot = DatabaseSnapshot::create(&storage.cache_layer.db(), &mut state_manager).unwrap();

        // Verify metadata
        let metadata = snapshot.metadata();
        assert_eq!(metadata.total_blocks, 1);
        assert!(metadata.timestamp > 0);
    }

    #[test]
    fn test_backup_engine_operations() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let backup_dir = temp_dir.path().join("backups");

        let (_temp_dir, storage, mut state_manager) = create_test_setup();

        // Create backup engine
        let backup_engine = DatabaseBackupEngine::new(&backup_dir, 3).unwrap();

        // Create snapshot
        let snapshot = DatabaseSnapshot::create(&storage.cache_layer.db(), &mut state_manager).unwrap();

        // Create backup
        let backup_id = (&mut backup_engine).create_backup(&storage.cache_layer.db(), &snapshot).unwrap();

        // Verify backup was created
        let backups = backup_engine.get_backup_info();
        assert_eq!(backups.len(), 1);
        assert_eq!(backups[0].backup_id, backup_id);

        // Verify metadata file exists
        let metadata_path = backup_dir.join(format!("backup_{}_metadata.json", backup_id));
        assert!(metadata_path.exists());

        // Load and verify metadata
        let loaded_metadata = backup_engine.load_backup_metadata(backup_id).unwrap();
        assert_eq!(loaded_metadata.backup_id, backup_id);
    }

    #[test]
    fn test_backup_cleanup() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let backup_dir = temp_dir.path().join("backups");

        let (_temp_dir, storage, mut state_manager) = create_test_setup();

        // Create backup engine with max 2 backups
        let backup_engine = DatabaseBackupEngine::new(&backup_dir, 2).unwrap();

        // Create multiple backups
        for i in 0..4 {
            // Add some data variation
            let block_value = BlockValue {
                hash: Hash([i as u8; 32]).0.to_vec(),
                header_bytes: vec![i as u8, 2, 3],
                transactions: vec![vec![4, 5, i as u8]],
                timestamp: 1000 + i as u64,
            };
            storage.writer.enqueue(vec![crate::storage::concurrency::StorageWriteCommand::Put {
                cf: ColumnFamilyName::Blocks,
                key: Hash([i as u8; 32]).0.to_vec(),
                value: block_value.to_bytes().unwrap(),
            }]);

            let snapshot = DatabaseSnapshot::create(&storage.cache_layer.db(), &mut state_manager).unwrap();
            (&mut backup_engine).create_backup(&storage.cache_layer.db(), &snapshot).unwrap();
        }

        // Should only keep 2 most recent backups
        let backups = backup_engine.get_backup_info();
        assert_eq!(backups.len(), 2);

        // Verify metadata files for kept backups exist
        for backup in &backups {
            let metadata_path = backup_dir.join(format!("backup_{}_metadata.json", backup.backup_id));
            assert!(metadata_path.exists());
        }
    }

    #[test]
    fn test_restore_from_backup() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let backup_dir = temp_dir.path().join("backups");
        let restore_dir = temp_dir.path().join("restore");

        let (_temp_dir, storage, mut state_manager) = create_test_setup();

        // Create backup
        let mut backup_engine = DatabaseBackupEngine::new(&backup_dir, 5).unwrap();
        let snapshot = DatabaseSnapshot::create(&storage.cache_layer.db(), &mut state_manager).unwrap();
        let backup_id = backup_engine.create_backup(&storage.cache_layer.db(), &snapshot).unwrap();

        // Restore to new location
        DatabaseRestoreEngine::restore_from_backup(&backup_dir, &restore_dir, backup_id).unwrap();

        // Verify restore directory exists and has database files
        assert!(restore_dir.exists());
        // Note: In a real test, we'd open the restored database and verify contents
    }

    #[test]
    fn test_backup_manager_integration() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let backup_dir = temp_dir.path().join("backups");

        let (_temp_dir, storage, mut state_manager) = create_test_setup();

        // Create backup manager
        let mut backup_manager = BackupManager::new(&backup_dir, 5).unwrap();

        // Create backup
        let backup_id = backup_manager.create_consistent_backup(&storage.cache_layer.db(), &mut state_manager).unwrap();

        // Verify backup exists
        let backups = backup_manager.get_backup_info();
        assert!(!backups.is_empty());
        assert!(backups.iter().any(|b| b.backup_id == backup_id));
    }

    #[test]
    fn test_state_manager_interface() {
        let mem_storage = MemoryStorage::new();
        let tree = VerkleTree::new(mem_storage).expect("Failed to create VerkleTree");
        let state_manager = StateManager::new(tree).expect("Failed to create StateManager");

        // Test interface methods
        let _state_root = state_manager.get_current_state_root();
        let _last_block = state_manager.get_last_block_hash();

        // Methods should not panic
        assert!(true);
    }
}

/// High-level backup manager coordinating all backup operations
pub struct BackupManager {
    backup_engine: DatabaseBackupEngine,
}

impl BackupManager {
    pub fn new(backup_dir: &Path, max_backups: usize) -> StorageResult<Self> {
        let backup_engine = DatabaseBackupEngine::new(backup_dir, max_backups)?;

        Ok(Self {
            backup_engine,
        })
    }

    /// Create a complete backup with consistency guarantees
    pub fn create_consistent_backup(&mut self, db: &StorageDb, state_manager: &mut dyn StateManagerInterface) -> StorageResult<u32> {
        let snapshot = DatabaseSnapshot::create(db, state_manager)?;
        self.backup_engine.create_backup(db, &snapshot)
    }

    /// Restore from backup and validate integrity
    pub fn restore_and_validate(&self, backup_path: &Path, restore_path: &Path, backup_id: u32, restored_db: &StorageDb, state_manager: &mut dyn StateManagerInterface) -> StorageResult<()> {
        let expected_metadata = DatabaseRestoreEngine::restore_from_backup(backup_path, restore_path, backup_id)?;

        DatabaseRestoreEngine::validate_restored_database(restored_db, &expected_metadata, state_manager)
    }

    /// Get backup information
    pub fn get_backup_info(&self) -> Vec<rocksdb::backup::BackupEngineInfo> {
        self.backup_engine.get_backup_info()
    }
}

/// Trait for state manager interface to avoid circular dependencies
pub trait StateManagerInterface {
    fn get_current_state_root(&mut self) -> Hash;
    fn get_last_block_hash(&self) -> Hash;
    fn verify_state_root(&mut self, expected_root: Hash) -> Result<(), StorageError>;
}

impl<S: Storage + Clone> StateManagerInterface for StateManager<S> {
    fn get_current_state_root(&mut self) -> Hash {
        let root_bytes = self.tree.get_root().unwrap_or([0u8; 32]);
        Hash::from_bytes(&root_bytes)
    }

    fn get_last_block_hash(&self) -> Hash {
        self.current_block_hash.clone().unwrap_or_else(|| Hash::from_bytes(&[0u8; 32]))
    }

    fn verify_state_root(&mut self, expected_root: Hash) -> Result<(), StorageError> {
        let current = self.get_current_state_root();
        if current == expected_root {
            Ok(())
        } else {
            Err(StorageError::OperationFailed(format!(
                "State root mismatch: expected {:?}, got {:?}",
                expected_root, current
            )))
        }
    }
}