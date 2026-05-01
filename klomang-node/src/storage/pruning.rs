use crate::storage::cf::ColumnFamilyName;
use crate::storage::db::StorageDb;
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
    pub fn prune_blocks(
        &mut self,
        db: &StorageDb,
        current_height: u64,
        depth: u64,
        finality_threshold: u64,
    ) -> Result<usize, String> {
        let cutoff_height = current_height
            .saturating_sub(depth)
            .saturating_sub(finality_threshold);
        let mut pruned = 0;
        let mut batch = crate::storage::batch::WriteBatch::new();

        // Iterate through all headers to find blocks to prune
        let cf_handle = db
            .inner()
            .cf_handle(ColumnFamilyName::Headers.as_str())
            .ok_or_else(|| "Headers CF not found".to_string())?;

        let iter = db.inner().iterator_cf(&cf_handle, IteratorMode::Start);

        for item in iter {
            let (key, value) = item.map_err(|e| format!("Iterator error: {}", e))?;
            let header: HeaderValue = bincode::deserialize(&value)
                .map_err(|e| format!("Deserialization failed: {}", e))?;

            if header.height < cutoff_height {
                // Remove block data but keep header
                batch.delete(&key);
                pruned += 1;
            }
        }

        // Execute batch deletion atomically
        db.write_batch(batch)
            .map_err(|e| format!("Write batch failed: {}", e))?;
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
    pub fn rebuild_state_root(&mut self) -> Result<usize, crate::storage::error::StorageError> {
        // Placeholder for state root rebuild
        // In production, would integrate with klomang-core StateManager
        Ok(0)
    }
}
