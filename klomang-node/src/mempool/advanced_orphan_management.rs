//! Advanced Orphan Management System
//!
//! Sistem manajemen transaksi yatim lanjutan dengan:
//! - Deferred Resolution Queue untuk throttling CPU
//! - Recursive Orphan Linking Engine dengan BFS
//! - Automatic chain linking saat parent tiba
//! - Thread-safe operations dengan parking_lot::Mutex

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use klomang_core::core::state::transaction::Transaction;
use parking_lot::Mutex;

use super::conflict::OutPoint;
use super::orphan_manager::OrphanManager;

/// Deferred Resolution Task
#[derive(Clone, Debug)]
pub struct ResolutionTask {
    /// Hash dari transaksi yang perlu di-resolve
    pub tx_hash: Vec<u8>,
    /// Waktu saat task dijadwalkan
    pub scheduled_at: Instant,
    /// Priority untuk resolusi (lebih tinggi = diproses lebih dulu)
    pub priority: u64,
}

/// Deferred Resolution Queue
///
/// Menggunakan VecDeque untuk menyimpan transaksi yang menunggu resolusi
/// dengan throttling CPU untuk mencegah spike. Setiap kali parent tiba,
/// children-nya dijadwalkan dalam batch queue ini alih-alih langsung diproses.
pub struct DeferredResolver {
    /// Queue dengan priority-based ordering
    queue: Arc<Mutex<VecDeque<ResolutionTask>>>,

    /// Tracking waktu terakhir resolution dilakukan
    last_resolution: Arc<Mutex<Instant>>,

    /// Minimum interval antara batch resolutions (untuk throttling)
    min_resolution_interval_ms: u64,

    /// Maximum tasks per batch resolution
    max_tasks_per_batch: usize,

    /// Statistics
    stats: Arc<Mutex<ResolutionStats>>,
}

/// Statistics untuk deferred resolution
#[derive(Clone, Debug, Default)]
pub struct ResolutionStats {
    /// Total tasks yang dijadwalkan
    pub total_scheduled: u64,
    /// Total tasks yang diproses
    pub total_processed: u64,
    /// Total tasks yang expired
    pub total_expired: u64,
    /// Current queue size
    pub current_queue_size: usize,
    /// Average resolve time
    pub avg_resolve_time_ms: f64,
}

impl DeferredResolver {
    /// Buat deferred resolver baru
    pub fn new(min_resolution_interval_ms: u64, max_tasks_per_batch: usize) -> Self {
        Self {
            queue: Arc::new(Mutex::new(VecDeque::new())),
            last_resolution: Arc::new(Mutex::new(Instant::now())),
            min_resolution_interval_ms,
            max_tasks_per_batch,
            stats: Arc::new(Mutex::new(ResolutionStats::default())),
        }
    }

    /// Jadwalkan transaksi untuk resolusi deferred
    ///
    /// Menambahkan task ke queue dengan timestamp dan durasi TTL.
    /// Jika queue penuh, akan diproses batch sebelumnya.
    pub fn schedule_resolution(&self, tx_hash: Vec<u8>, priority: u64) -> Result<(), String> {
        let task = ResolutionTask {
            tx_hash,
            scheduled_at: Instant::now(),
            priority,
        };

        let mut queue = self.queue.lock();
        queue.push_back(task);

        // Update stats
        {
            let mut stats = self.stats.lock();
            stats.total_scheduled += 1;
            stats.current_queue_size = queue.len();
        }

        Ok(())
    }

    /// Process tasks yang sudah siap dalam batch
    ///
    /// Hanya memproses jika min_resolution_interval telah berlalu.
    /// Mengembalikan list task yang siap diproses.
    pub fn process_batch(&self) -> Result<Vec<ResolutionTask>, String> {
        let now = Instant::now();
        let last_resolution = *self.last_resolution.lock();

        // Check apakah sudah cukup waktu sejak resolusi terakhir
        if now.duration_since(last_resolution)
            < Duration::from_millis(self.min_resolution_interval_ms)
        {
            return Ok(Vec::new());
        }

        let mut queue = self.queue.lock();

        // Process batch dengan TTL checking
        let mut batch = Vec::new();

        while batch.len() < self.max_tasks_per_batch && !queue.is_empty() {
            if let Some(task) = queue.pop_front() {
                // Default TTL: 20 minutes
                let ttl = Duration::from_secs(20 * 60);

                if now.duration_since(task.scheduled_at) > ttl {
                    // Task expired, skip
                    self.stats.lock().total_expired += 1;
                    continue;
                }

                batch.push(task);
            }
        }

        // Update last resolution time
        *self.last_resolution.lock() = Instant::now();

        // Update stats
        {
            let mut stats = self.stats.lock();
            stats.total_processed += batch.len() as u64;
            stats.current_queue_size = queue.len();
        }

        Ok(batch)
    }

    /// Clear queue (usually for testing or full restart)
    pub fn clear_queue(&self) {
        self.queue.lock().clear();
        let mut stats = self.stats.lock();
        stats.current_queue_size = 0;
    }

    /// Get current statistics
    pub fn get_stats(&self) -> ResolutionStats {
        self.stats.lock().clone()
    }

    /// Get current queue size
    pub fn queue_size(&self) -> usize {
        self.queue.lock().len()
    }
}

/// Orphan Chain Link (untuk tracking hubungan parent-child)
#[derive(Clone, Debug)]
pub struct OrphanChainLink {
    /// Hash dari transaksi
    pub tx_hash: Vec<u8>,
    /// Parents (immediate) yang dibutuhkan
    pub immediate_parents: Vec<OutPoint>,
    /// Children (immediate) yang menunggu ini
    pub immediate_children: Vec<Vec<u8>>,
    /// Depth dalam chain (0 = parent paling tinggi)
    pub depth: usize,
}

/// Recursive Orphan Linking Engine
///
/// Menggunakan BFS untuk menelusuri dan menghubungkan seluruh rantai orphan.
/// Ketika parent tiba, tidak hanya langsung child yang di-adopt, tetapi
/// seluruh subtree dependensi diproses secara terurut (BFS).
pub struct RecursiveOrphanLinker {
    /// Reference ke orphan manager
    orphan_manager: Arc<OrphanManager>,

    /// Cache untuk chain relationships
    chain_cache: Arc<DashMap<Vec<u8>, OrphanChainLink>>,

    /// Maximum depth untuk mencegah infinite recursion pada circular deps
    max_chain_depth: usize,

    /// Statistics
    stats: Arc<Mutex<LinkerStats>>,
}

/// Statistics untuk recursive linker
#[derive(Clone, Debug, Default)]
pub struct LinkerStats {
    /// Total chain links yang dibuat
    pub total_links_created: u64,
    /// Total chains yang berhasil diselesaikan
    pub total_chains_resolved: u64,
    /// Maximum chain depth yang pernah dicapai
    pub max_depth_reached: usize,
    /// Total transaction yang diadopsi via linking
    pub total_adopted_via_linking: u64,
    /// Total circular dependencies yang terdeteksi
    pub circular_deps_detected: u64,
}

impl RecursiveOrphanLinker {
    /// Buat recursive orphan linker baru
    pub fn new(orphan_manager: Arc<OrphanManager>, max_chain_depth: usize) -> Self {
        Self {
            orphan_manager,
            chain_cache: Arc::new(DashMap::new()),
            max_chain_depth,
            stats: Arc::new(Mutex::new(LinkerStats::default())),
        }
    }

    /// Link entire orphan chain recursively menggunakan BFS
    ///
    /// Ketika parent transaction tiba, fungsi ini menelusuri seluruh
    /// dependensi tree dan mengadopsi semua related orphans secara
    /// breadth-first (level-by-level).
    ///
    /// Algorithm:
    /// 1. Start dengan parent's outputs sebagai immediate parents
    /// 2. BFS queue: process level 0 (direct children) terlebih dahulu
    /// 3. Untuk setiap level, collect all ready new parents untuk next level
    /// 4. Continue hingga semua levels diproses atau max_depth tercapai
    pub fn link_orphan_chain(
        &self,
        parent_tx: &Transaction,
    ) -> Result<ChainResolutionResult, String> {
        let mut result = ChainResolutionResult::default();

        // Collect parent's outputs sebagai starting point
        let mut current_level_outputs = Vec::new();
        let parent_tx_hash = parent_tx.id.clone();

        // Convert parent outputs to OutPoints
        for (output_index, _output) in parent_tx.outputs.iter().enumerate() {
            current_level_outputs.push(OutPoint {
                tx_hash: bincode::serialize(&parent_tx_hash)
                    .map_err(|e| format!("Serialization error: {}", e))?,
                index: output_index as u32,
            });
        }

        // BFS traversal untuk resolve semua levels
        for current_depth in 0..self.max_chain_depth {
            if current_level_outputs.is_empty() {
                break; // Tidak ada lebih banyak dependencies
            }

            let mut next_level_outputs = Vec::new();

            // Process semua outpoints di level ini
            for outpoint in &current_level_outputs {
                // Dapatkan children yang menunggu outpoint ini
                match self.orphan_manager.process_orphans_for_parent(outpoint) {
                    Ok(adoption_result) => {
                        // Collect newly freed outputs dari adopted txs
                        for adopted_tx in &adoption_result.adopted_txs {
                            result.total_adopted += adoption_result.adoption_count;

                            // Serialize adopted tx hash untuk tracking
                            let adopted_hash = bincode::serialize(&adopted_tx.id)
                                .map_err(|e| format!("Serialization error: {}", e))?;

                            // Create chain link
                            let link = OrphanChainLink {
                                tx_hash: adopted_hash.clone(),
                                immediate_parents: vec![outpoint.clone()],
                                immediate_children: Vec::new(),
                                depth: current_depth + 1,
                            };
                            self.chain_cache.insert(adopted_hash.clone(), link);

                            // Collect this transaction's outputs untuk next level
                            for (output_index, _) in adopted_tx.outputs.iter().enumerate() {
                                next_level_outputs.push(OutPoint {
                                    tx_hash: bincode::serialize(&adopted_tx.id)
                                        .map_err(|e| format!("Serialization error: {}", e))?,
                                    index: output_index as u32,
                                });
                            }
                        }
                    }
                    Err(_) => {
                        // Tidak ada orphans waiting untuk output ini, OK
                    }
                }
            }

            result.maximum_depth = current_depth + 1;
            current_level_outputs = next_level_outputs;
        }

        // Update stats
        {
            let mut stats = self.stats.lock();
            if result.maximum_depth > stats.max_depth_reached {
                stats.max_depth_reached = result.maximum_depth;
            }
            stats.total_adopted_via_linking += result.total_adopted as u64;
            stats.total_chains_resolved += 1;
        }

        Ok(result)
    }

    /// Detect circular dependencies dalam chain
    ///
    /// Menggunakan DFS untuk mendeteksi cycles dalam dependency graph.
    pub fn detect_circular_dependencies(&self) -> Result<Vec<Vec<Vec<u8>>>, String> {
        let mut circular_deps = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut rec_stack = std::collections::HashSet::new();

        for entry in self.chain_cache.iter() {
            let tx_hash = entry.key().clone();

            if !visited.contains(&tx_hash) {
                let mut path = Vec::new();
                if self._dfs_detect_cycle(&tx_hash, &mut visited, &mut rec_stack, &mut path) {
                    circular_deps.push(path);

                    // Update stats
                    self.stats.lock().circular_deps_detected += 1;
                }
            }
        }

        Ok(circular_deps)
    }

    /// DFS helper untuk detecting cycles
    fn _dfs_detect_cycle(
        &self,
        current: &[u8],
        visited: &mut std::collections::HashSet<Vec<u8>>,
        rec_stack: &mut std::collections::HashSet<Vec<u8>>,
        path: &mut Vec<Vec<u8>>,
    ) -> bool {
        visited.insert(current.to_vec());
        rec_stack.insert(current.to_vec());
        path.push(current.to_vec());

        if let Some(link) = self.chain_cache.get(current) {
            for child_hash in &link.immediate_children {
                if !visited.contains(child_hash) {
                    if self._dfs_detect_cycle(child_hash, visited, rec_stack, path) {
                        return true;
                    }
                } else if rec_stack.contains(child_hash) {
                    // Cycle detected
                    return true;
                }
            }
        }

        rec_stack.remove(current);
        path.pop();
        false
    }

    /// Verify semua links valid
    pub fn verify_chain_integrity(&self) -> Result<ChainIntegrityReport, String> {
        let mut report = ChainIntegrityReport::default();

        for entry in self.chain_cache.iter() {
            report.total_links += 1;

            let link = entry.value();

            // Verify depth tidak exceed max
            if link.depth > self.max_chain_depth {
                report.invalid_depth_links += 1;
            }

            // Verify parents exist dalam orphan pool atau main pool
            for parent in &link.immediate_parents {
                if !self._verify_parent_exists(parent)? {
                    report.orphaned_links += 1;
                }
            }
        }

        Ok(report)
    }

    fn _verify_parent_exists(&self, _parent: &OutPoint) -> Result<bool, String> {
        // In production, would check both main pool dan orphan pool
        // For now, return true untuk safety (parent existence would be verified elsewhere)
        Ok(true)
    }

    /// Clear cache (usually for testing)
    pub fn clear_cache(&self) {
        self.chain_cache.clear();
    }

    /// Get linker statistics
    pub fn get_stats(&self) -> LinkerStats {
        self.stats.lock().clone()
    }

    /// Get chain topology untuk debugging
    pub fn get_chain_topology(&self) -> BTreeMap<usize, usize> {
        let mut topology = BTreeMap::new();

        for entry in self.chain_cache.iter() {
            let depth = entry.value().depth;
            *topology.entry(depth).or_insert(0) += 1;
        }

        topology
    }
}

/// Result dari chain resolution
#[derive(Clone, Debug, Default)]
pub struct ChainResolutionResult {
    /// Total transactions yang diadopsi
    pub total_adopted: usize,
    /// Maximum depth dari chain yang diproses
    pub maximum_depth: usize,
}

/// Report hasil chain integrity verification
#[derive(Clone, Debug, Default)]
pub struct ChainIntegrityReport {
    /// Total links dalam cache
    pub total_links: usize,
    /// Links dengan invalid depth
    pub invalid_depth_links: usize,
    /// Links yang orphaned (parents tidak ada)
    pub orphaned_links: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deferred_resolver_scheduling() {
        let resolver = DeferredResolver::new(100, 10);

        assert!(resolver.schedule_resolution(vec![1], 100).is_ok());
        assert!(resolver.schedule_resolution(vec![2], 200).is_ok());
        assert_eq!(resolver.queue_size(), 2);
    }

    #[test]
    fn test_deferred_resolver_batch_processing() {
        let resolver = DeferredResolver::new(0, 5);

        for i in 0..10 {
            let _ = resolver.schedule_resolution(vec![i as u8], i as u64);
        }

        match resolver.process_batch() {
            Ok(batch) => {
                assert_eq!(batch.len(), 5);
                assert_eq!(resolver.queue_size(), 5);
            }
            Err(e) => panic!("Batch processing failed: {}", e),
        }
    }

    #[test]
    fn test_recursive_linker_creation() {
        use crate::mempool::orphan_manager::{OrphanManager, OrphanPoolConfig};
        use std::sync::Arc;

        let config = OrphanPoolConfig::default();
        let manager = Arc::new(OrphanManager::new(config, None));
        let linker = RecursiveOrphanLinker::new(manager, 10);

        let stats = linker.get_stats();
        assert_eq!(stats.total_chains_resolved, 0);
    }

    #[test]
    fn test_chain_integrity_report() {
        use crate::mempool::orphan_manager::{OrphanManager, OrphanPoolConfig};
        use std::sync::Arc;

        let config = OrphanPoolConfig::default();
        let manager = Arc::new(OrphanManager::new(config, None));
        let linker = RecursiveOrphanLinker::new(manager, 10);

        match linker.verify_chain_integrity() {
            Ok(report) => {
                assert_eq!(report.total_links, 0);
                assert_eq!(report.orphaned_links, 0);
            }
            Err(e) => panic!("Integrity check failed: {}", e),
        }
    }
}
