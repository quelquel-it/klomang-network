# Backup & Recovery System Documentation

## Overview

The Backup & Recovery system provides robust data persistence and disaster recovery capabilities for the klomang-node using RocksDB's native backup features. The system ensures data consistency during backups and provides fast restoration capabilities.

## Architecture Components

### DatabaseSnapshot
- **Purpose**: Creates consistent snapshots of database state at a specific point in time
- **Implementation**: Uses RocksDB snapshots to capture DAG, UTXO, and Block state atomically
- **Metadata**: Includes state root, last block hash, record counts, and timestamps

### DatabaseBackupEngine
- **Purpose**: Manages incremental backups using RocksDB BackupEngine
- **Features**:
  - Incremental backups (only changed data)
  - Automatic cleanup of old backups (configurable retention)
  - Metadata storage alongside backups
- **Configuration**: Max backups limit, backup directory path

### DatabaseRestoreEngine
- **Purpose**: Handles database restoration from backups
- **Features**:
  - Full database restoration
  - Integrity validation post-restore
  - Atomic restore operations

### BackupManager
- **Purpose**: High-level coordinator for backup and recovery operations
- **Integration**: Connects with klomang-core StateManager for validation

## Key Features

### Consistent Snapshots
- Uses RocksDB snapshots for point-in-time consistency
- Captures all column families simultaneously
- Prevents partial state during concurrent writes

### Incremental Backups
- Only backs up changed SST files
- Reduces storage requirements and backup time
- Maintains full restore capability

### Metadata Validation
- Stores state root and last block hash with each backup
- Validates integrity after restoration
- Ensures restored database matches backup state

### WAL Durability Integration
- Complements existing WAL-based crash recovery
- Provides additional recovery layers
- Enables point-in-time recovery

## Usage Examples

### Creating a Backup
```rust
use klomang_node::storage::{BackupManager, StorageEngine};
use klomang_core::core::state::state_manager::StateManager;

// Initialize components
let storage = Arc::new(StorageEngine::new(db)?);
let state_manager = Arc::new(StateManager::new(tree)?);
let backup_manager = BackupManager::new("/path/to/backups", 5, state_manager)?;

// Create consistent backup
let backup_id = backup_manager.create_consistent_backup(&storage.db())?;
println!("Backup created with ID: {}", backup_id);
```

### Restoring from Backup
```rust
// Restore and validate
backup_manager.restore_and_validate(
    "/path/to/backups",
    "/path/to/restore/db",
    backup_id,
    &storage.db()
)?;
println!("Database restored and validated successfully");
```

### Managing Backups
```rust
// Get backup information
let backups = backup_manager.get_backup_info();
for backup in backups {
    println!("Backup ID: {}, Size: {} bytes", backup.backup_id, backup.size);
}
```

## Configuration

### Backup Directory
- Must be writable by the node process
- Should have sufficient space for multiple backups
- Recommended: Use separate disk/partition for backups

### Retention Policy
- Configurable maximum number of backups to retain
- Automatic cleanup of oldest backups
- Balance between recovery points and storage usage

### Performance Considerations
- Backups are incremental but still I/O intensive
- Schedule during low-traffic periods
- Monitor disk space and backup completion times

## Error Handling

### Backup Failures
- Directory permission issues
- Insufficient disk space
- RocksDB internal errors
- State manager validation failures

### Restore Failures
- Corrupted backup files
- Metadata validation mismatches
- Target directory conflicts
- Permission issues

### Recovery Strategies
- Retry with different backup ID
- Manual intervention for corrupted backups
- Fallback to WAL-based recovery

## Integration with Core Components

### StateManager Integration
- Provides `get_current_state_root()` and `get_last_block_hash()` methods
- Validates backup integrity against current state
- Ensures restored state matches backup metadata

### Storage Engine Compatibility
- Works with concurrent read/write operations
- Compatible with existing caching and batching
- Maintains atomicity guarantees

## Monitoring and Maintenance

### Backup Health Checks
- Verify backup directory accessibility
- Check backup file integrity
- Validate metadata consistency

### Storage Management
- Monitor backup directory size
- Implement backup rotation policies
- Regular integrity testing of backups

### Performance Monitoring
- Track backup creation times
- Monitor restore operation durations
- Log backup success/failure events

## Security Considerations

### Access Control
- Backup directory permissions
- Encryption of backup data (future enhancement)
- Secure backup storage locations

### Data Privacy
- Backup contents include all blockchain data
- Consider encryption for sensitive deployments
- Secure backup transport and storage

## Disaster Recovery Procedures

### Node Failure Recovery
1. Stop the failed node
2. Identify latest valid backup
3. Restore database from backup
4. Validate restored state
5. Restart node with restored data

### Data Corruption Recovery
1. Detect corruption through validation failures
2. Identify last known good backup
3. Restore to clean state
4. Replay transactions from last good state

### Emergency Procedures
- Multiple backup locations
- Offsite backup storage
- Regular backup testing procedures