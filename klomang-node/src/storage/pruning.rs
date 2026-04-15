use crate::storage::db::StorageDb;
use klomang_core::core::state_manager::StateManager;
use crate::storage::schema::HeaderValue;
use rocksdb::IteratorMode;

/// Pruning manager for tracking and triggering maintenance operations
#[derive(Debug)]
pub struct PruningManager {
    pub pruned_count: usize,
}

impl PruningManager {
    pub fn new() -> Self {
        Self { pruned_count: 0 }
    }

    pub fn record_pruning(&mut self) {
        self.pruned_count = self.pruned_count.saturating_add(1);
    }

    /// Trigger manual compaction after pruning operations to optimize SST files
    ///
    /// This should be called after significant pruning to balance data distribution
    /// and improve read performance.
    pub fn trigger_compaction_after_pruning(&self, db: &StorageDb) -> Result<(), rocksdb::Error> {
        db.trigger_manual_compaction()
    }

    /// Prune old blocks beyond the specified depth while preserving headers for verification
    ///
    /// This removes serialized block data from CF Blocks but keeps headers in CF Headers
    /// to maintain historical verification capabilities.
    ///
    /// # Arguments
    /// * `db` - Storage database instance
    /// * `current_height` - Current blockchain height
    /// * `depth` - Number of recent blocks to keep (e.g., 100000)
    /// * `finality_threshold` - Minimum confirmations required before pruning (e.g., 100)
    ///
    /// # Returns
    /// Number of blocks pruned
    pub fn prune_blocks(&mut self, db: &StorageDb, current_height: u64, depth: u64, finality_threshold: u64) -> Result<usize, rocksdb::Error> {
        let cutoff_height = current_height.saturating_sub(depth).saturating_sub(finality_threshold);
        let mut pruned = 0;
        let mut batch = crate::storage::batch::WriteBatch::new();

        // Iterate through all headers to find blocks to prune
        let iter = db.inner().iterator_cf(
            db.inner().cf_handle(crate::storage::cf::ColumnFamilyName::Headers.as_str()).unwrap(),
            IteratorMode::Start,
        );

        for item in iter {
            let (key, value) = item?;
            let header: HeaderValue = bincode::deserialize(&value)?;
            
            if header.height < cutoff_height {
                // Remove block data but keep header
                batch.delete(crate::storage::cf::ColumnFamilyName::Blocks, &key);
                pruned += 1;
            }
        }

        // Execute batch deletion atomically
        db.write_batch(batch)?;
        self.pruned_count = self.pruned_count.saturating_add(pruned);
        Ok(pruned)
    }

    /// Rebuild Verkle state root and prune old proofs
    ///
    /// This consolidates the current Verkle state and removes outdated proof data
    /// to optimize storage space.
    ///
    /// # Arguments
    /// * `state_manager` - State manager instance from core
    ///
    /// # Returns
    /// Number of proof entries pruned
    pub fn rebuild_state_root(&mut self, state_manager: &mut StateManager) -> Result<usize, rocksdb::Error> {
        // Call core method to rebuild and prune
        state_manager.rebuild_state_root_and_prune_proofs()
            .map_err(|e| rocksdb::Error::new(rocksdb::ErrorKind::Other, format!("State rebuild error: {:?}", e)))
    }
}
