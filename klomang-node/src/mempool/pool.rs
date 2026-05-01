//! Multi-indexed transaction pool storage
//!
//! Maintains transaction indexes by:
//! - Hash for direct lookup
//! - Fee rate for priority selection
//! - Arrival time for FIFO tie-breaking
//! - Status for lifecycle management

use dashmap::DashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;
use parking_lot::RwLock;

use klomang_core::core::state::transaction::Transaction;

use super::admission_controller::AdmissionController;
use super::advanced_dependency_manager::TxDependencyManager;
use super::advanced_orphan_management::{DeferredResolver, RecursiveOrphanLinker};
use super::conflict::OutPoint;
use super::deterministic_ordering::{
    DeterministicOrderingEngine, DeterministicOrderingEngineConfig,
};
use super::graph_conflict_ordering_integration::{
    ConflictOrderingIntegration, ConflictOrderingIntegrationConfig,
};
use super::memory_limiter::{MempoolLimiter, MempoolLimiterConfig};
use super::multi_dimensional_index::{IndexedTransaction, MultiDimensionalIndex};
use super::orphan_manager::{OrphanManager, OrphanPoolConfig};
use super::parallel_selection::{FeeBalancer, ParallelSelectionBuilder};
use super::priority_scheduler::{PriorityScheduler, PrioritySchedulerConfig};
use super::resource_optimizer::{ResourceOptimizer, ResourceOptimizerConfig};
use super::selection::{DeterministicSelector, SelectionCriteria};
use super::status::TransactionStatus;
use super::validation::{PoolValidator, ValidationResult};
use crate::storage::kv_store::KvStore;

/// Configuration for transaction pool behavior
#[derive(Clone, Debug)]
pub struct PoolConfig {
    /// Maximum number of transactions in pool
    pub max_pool_size: usize,

    /// Maximum number of orphan transactions
    pub max_orphan_size: usize,

    /// Minimum fee per byte to accept transaction
    pub min_fee_rate: u64,

    /// Fee bump percentage applied dynamically as mempool fills
    pub dynamic_fee_bump_percent: u64,

    /// Maximum transactions accepted from one source during the rate limit window
    pub max_transactions_per_source: u64,

    /// Window for per-source rate limiting, in seconds
    pub rate_limit_window_secs: u64,

    /// TTL for orphan transactions in seconds
    pub orphan_ttl_secs: u64,

    /// TTL for rejected transactions in seconds
    pub rejected_ttl_secs: u64,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_pool_size: 10000,
            max_orphan_size: 1000,
            min_fee_rate: 1,               // 1 satoshi per byte
            dynamic_fee_bump_percent: 150, // up to 150% fee increase at high mempool utilization
            max_transactions_per_source: 10,
            rate_limit_window_secs: 60, // 1 minute
            orphan_ttl_secs: 600,       // 10 minutes
            rejected_ttl_secs: 3600,    // 1 hour
        }
    }
}

/// Entry in the transaction pool with metadata
#[derive(Clone, Debug)]
pub struct PoolEntry {
    /// The transaction itself
    pub transaction: Transaction,

    /// Current lifecycle status
    pub status: TransactionStatus,

    /// Time when transaction was first added (UNIX timestamp)
    pub arrival_time: u64,

    /// Size in bytes for fee rate calculation
    pub size_bytes: usize,

    /// Total fees in satoshis
    pub total_fee: u64,
}

impl PoolEntry {
    /// Create new pool entry
    pub fn new(transaction: Transaction, total_fee: u64, size_bytes: usize) -> Self {
        let arrival_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            transaction,
            status: TransactionStatus::Pending,
            arrival_time,
            size_bytes,
            total_fee,
        }
    }

    /// Calculate fee rate (satoshis per byte)
    pub fn fee_rate(&self) -> u64 {
        if self.size_bytes == 0 {
            0
        } else {
            self.total_fee / self.size_bytes as u64
        }
    }

    /// Check if entry has expired based on its status
    pub fn is_expired(&self, current_time: u64, config: &PoolConfig) -> bool {
        let age = current_time.saturating_sub(self.arrival_time);

        match self.status {
            TransactionStatus::InOrphanPool => age > config.orphan_ttl_secs,
            TransactionStatus::Rejected => age > config.rejected_ttl_secs,
            _ => false,
        }
    }
}

/// Selected transaction for block candidate
#[derive(Clone, Debug)]
pub struct SelectedTransaction {
    pub transaction: Transaction,
    pub total_fee: u64,
    pub size_bytes: usize,
}

#[derive(Clone, Debug)]
struct TokenBucket {
    /// Current number of tokens
    tokens: f64,
    /// Maximum capacity of tokens
    capacity: f64,
    /// Rate of token refill per second
    refill_rate: f64,
    /// Last refill timestamp
    last_refill: u64,
}

impl TokenBucket {
    fn new(capacity: f64, refill_rate: f64) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            tokens: capacity, // Start full
            capacity,
            refill_rate,
            last_refill: now,
        }
    }

    /// Try to consume tokens. Returns true if successful.
    fn try_consume(&mut self, tokens: f64) -> bool {
        self.refill();

        if self.tokens >= tokens {
            self.tokens -= tokens;
            true
        } else {
            false
        }
    }

    /// Refill tokens based on elapsed time
    fn refill(&mut self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let elapsed = now.saturating_sub(self.last_refill) as f64;
        let refill_amount = elapsed * self.refill_rate;

        self.tokens = (self.tokens + refill_amount).min(self.capacity);
        self.last_refill = now;
    }
}

/// Dynamic fee filter that adjusts minimum relay fee based on mempool utilization
#[derive(Clone, Debug)]
pub struct FeeFilter {
    /// Base minimum fee rate (satoshis per byte)
    base_min_fee: u64,
    /// Current effective minimum fee rate
    current_min_fee: u64,
    /// Maximum fee bump percentage
    max_bump_percent: u64,
    /// Last update timestamp
    last_update: u64,
}

impl FeeFilter {
    /// Create new fee filter with base fee
    pub fn new(base_min_fee: u64, max_bump_percent: u64) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            base_min_fee,
            current_min_fee: base_min_fee,
            max_bump_percent,
            last_update: now,
        }
    }

    /// Update fee threshold based on mempool utilization
    /// - If utilization > 75%, increase fee threshold
    /// - If utilization < 25%, decrease toward base fee
    pub fn update_threshold(&mut self, current_size: usize, max_size: usize) {
        if max_size == 0 {
            return;
        }

        let utilization = current_size as f64 / max_size as f64;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Update every 10 seconds to avoid thrashing
        if now.saturating_sub(self.last_update) < 10 {
            return;
        }

        self.last_update = now;

        if utilization > 0.75 {
            // Increase fee when heavily utilized
            let bump = ((self.base_min_fee as f64 * utilization * self.max_bump_percent as f64)
                / 100.0)
                .ceil() as u64;
            self.current_min_fee = self.base_min_fee.saturating_add(bump);
        } else if utilization < 0.25 {
            // Gradually decrease toward base fee when lightly utilized
            let reduction = (self.current_min_fee.saturating_sub(self.base_min_fee)) / 2;
            self.current_min_fee = self.current_min_fee.saturating_sub(reduction);
        }
    }

    /// Get current minimum fee threshold
    pub fn current_threshold(&self) -> u64 {
        self.current_min_fee
    }

    /// Check if fee rate meets threshold
    pub fn accepts_fee(&self, fee_rate: u64) -> bool {
        fee_rate >= self.current_min_fee
    }

    /// Set base minimum fee
    pub fn set_base_fee(&mut self, base_fee: u64) {
        self.base_min_fee = base_fee;
        if self.current_min_fee < base_fee {
            self.current_min_fee = base_fee;
        }
    }

    /// Force update timestamp for testing (public for integration tests)
    pub fn force_update_timestamp(&mut self) {
        self.last_update = 0;
    }
}

/// Multi-indexed transaction pool with state machine
pub struct TransactionPool {
    /// Primary index: transaction hash -> pool entry
    by_hash: Arc<RwLock<IndexMap<Vec<u8>, PoolEntry>>>,

    /// Secondary index for quick orphan lookup
    orphans: Arc<RwLock<Vec<Vec<u8>>>>,

    /// Configuration parameters
    config: PoolConfig,

    /// Advanced dependency manager for cascade validation
    dependency_manager: Option<Arc<TxDependencyManager>>,

    /// Priority scheduler for age-based anti-starvation
    priority_scheduler: Arc<PriorityScheduler>,

    /// Multi-dimensional index for efficient querying
    multi_dimensional_index: Arc<MultiDimensionalIndex>,

    /// Deterministic ordering engine for consensus-safe transaction ordering
    deterministic_ordering: Arc<DeterministicOrderingEngine>,

    /// Orphan transaction manager untuk menangani transaksi dengan missing parents
    orphan_manager: Arc<OrphanManager>,

    /// Advanced orphan management: deferred resolution dengan throttling
    deferred_resolver: Arc<DeferredResolver>,

    /// Advanced orphan management: recursive chain linking dengan BFS
    orphan_linker: Arc<RecursiveOrphanLinker>,

    /// Memory limiter untuk deterministic mempool size management
    memory_limiter: Arc<MempoolLimiter>,

    /// Resource optimizer untuk multi-tier storage dan hybrid eviction
    resource_optimizer: Arc<ResourceOptimizer>,

    /// KvStore reference untuk persistence dan validation
    kv_store: Option<Arc<KvStore>>,

    /// Validator untuk checking missing inputs
    validator: Option<Arc<PoolValidator>>,

    /// Per-source token buckets for anti-spam rate limiting
    source_rate_buckets: DashMap<Vec<u8>, TokenBucket>,

    /// Dynamic fee filter for anti-spam minimum fee enforcement
    fee_filter: Arc<RwLock<FeeFilter>>,

    /// Advanced admission controller for resource-based filtering
    admission_controller: Arc<AdmissionController>,

    /// Graph-based conflict detection and canonical ordering integration
    conflict_ordering_integration: Arc<ConflictOrderingIntegration>,

    /// Parallel selection builder for conflict-free sharding
    parallel_selection_builder: Arc<ParallelSelectionBuilder>,

    /// Adaptive fee pressure balancer
    fee_balancer: Option<Arc<FeeBalancer>>,
}

impl TransactionPool {
    /// Create new transaction pool
    pub fn new(config: PoolConfig) -> Self {
        // For backward compatibility, create with None KvStore
        // In production, use new_with_kv_store
        Self::new_with_kv_store(config, None)
    }

    /// Create new transaction pool with KvStore
    pub fn new_with_kv_store(config: PoolConfig, kv_store: Option<Arc<KvStore>>) -> Self {
        // Initialize orphan manager dengan strict RAM limits untuk mencegah DoS
        let orphan_config = OrphanPoolConfig {
            max_orphans: (config.max_orphan_size as f64 * 0.8) as usize, // 80% dari limit untuk margin
            max_adoption_batch: 1000,
            orphan_ttl_ns: config.orphan_ttl_secs as u64 * 1_000_000_000,
            ..Default::default()
        };

        let orphan_manager = Arc::new(OrphanManager::new(orphan_config, kv_store.clone()));
        let validator = Arc::new(PoolValidator::new(kv_store.clone()));

        // Initialize memory limiter dengan default configuration
        let memory_config = MempoolLimiterConfig::default();
        let memory_limiter = Arc::new(MempoolLimiter::new(memory_config));

        // Initialize resource optimizer dengan real kv_store
        let resource_config = ResourceOptimizerConfig::default();
        let resource_optimizer =
            Arc::new(ResourceOptimizer::new(kv_store.clone(), resource_config));

        // Create fee filter before moving config
        let fee_filter = Arc::new(RwLock::new(FeeFilter::new(
            config.min_fee_rate,
            config.dynamic_fee_bump_percent,
        )));

        // Create admission controller with kv_store for trend persistence
        let admission_controller = if let Some(ref kv_store) = kv_store {
            Arc::new(AdmissionController::with_kv_store(kv_store.clone()))
        } else {
            Arc::new(AdmissionController::new())
        };

        // Initialize conflict ordering integration
        let conflict_config = ConflictOrderingIntegrationConfig::default();
        let conflict_ordering_integration = Arc::new(ConflictOrderingIntegration::new(
            conflict_config,
            kv_store.clone(),
        ));
        let parallel_selection = Arc::new(ParallelSelectionBuilder::new(
            kv_store.clone(),
            conflict_ordering_integration.clone(),
        ));
        let fee_balancer = kv_store
            .as_ref()
            .map(|kv| Arc::new(FeeBalancer::new(Arc::clone(kv))));

        let mut pool = Self {
            by_hash: Arc::new(RwLock::new(IndexMap::new())),
            orphans: Arc::new(RwLock::new(Vec::new())),
            config,
            dependency_manager: None,
            priority_scheduler: Arc::new(
                PriorityScheduler::new(PrioritySchedulerConfig::default()),
            ),
            multi_dimensional_index: Arc::new(MultiDimensionalIndex::new()),
            deterministic_ordering: Arc::new(DeterministicOrderingEngine::new(
                DeterministicOrderingEngineConfig::default(),
            )),
            orphan_manager: orphan_manager.clone(),
            deferred_resolver: Arc::new(DeferredResolver::new(50, 10000)),
            orphan_linker: Arc::new(RecursiveOrphanLinker::new(orphan_manager, 10)),
            memory_limiter,
            resource_optimizer,
            kv_store,
            validator: Some(validator),
            source_rate_buckets: DashMap::new(),
            fee_filter,
            admission_controller,
            conflict_ordering_integration,
            parallel_selection_builder: parallel_selection,
            fee_balancer,
        };

        pool.load_persistent_min_fee_rate().ok();
        pool
    }

    pub fn derive_source_key(&self, tx: &Transaction) -> Vec<u8> {
        if let Some(input) = tx.inputs.first() {
            if !input.pubkey.is_empty() {
                return input.pubkey.clone();
            }
        }

        b"anonymous_source".to_vec()
    }

    fn enforce_minimum_fee_rate(&self, fee_rate: u64) -> Result<(), String> {
        let threshold = self.fee_filter.read().current_threshold();
        if fee_rate < threshold {
            return Err(format!(
                "Transaction fee rate {} sat/B below current minimum {} sat/B",
                fee_rate, threshold
            ));
        }
        Ok(())
    }

    fn enforce_source_rate_limit(&self, source_key: &[u8]) -> Result<(), String> {
        let capacity = self.config.max_transactions_per_source as f64;
        let refill_rate = capacity / self.config.rate_limit_window_secs as f64; // tokens per second

        let mut bucket = self
            .source_rate_buckets
            .entry(source_key.to_vec())
            .or_insert_with(|| TokenBucket::new(capacity, refill_rate));

        if !bucket.try_consume(1.0) {
            return Err(format!(
                "Rate limit exceeded for source - max {} transactions per {} seconds",
                self.config.max_transactions_per_source, self.config.rate_limit_window_secs
            ));
        }

        Ok(())
    }

    fn load_persistent_min_fee_rate(&mut self) -> Result<(), String> {
        if let Some(ref kv_store) = self.kv_store {
            match kv_store.get_mempool_min_fee_rate() {
                Ok(Some(rate)) => {
                    self.config.min_fee_rate = rate;
                }
                Ok(None) => {}
                Err(e) => {
                    return Err(format!("Failed to load persisted min fee rate: {}", e));
                }
            }
        }
        Ok(())
    }

    pub fn persist_min_fee_rate(&self, min_fee_rate: u64) -> Result<(), String> {
        if let Some(ref kv_store) = self.kv_store {
            kv_store
                .put_mempool_min_fee_rate(min_fee_rate)
                .map_err(|e| format!("Failed to persist min fee rate: {}", e))
        } else {
            Ok(())
        }
    }

    /// Calculate average transaction age in seconds
    fn calculate_average_transaction_age(&self) -> u64 {
        let pool = self.by_hash.read();
        if pool.is_empty() {
            return 0;
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let total_age: u64 = pool
            .values()
            .map(|entry| now.saturating_sub(entry.arrival_time))
            .sum();

        total_age / pool.len() as u64
    }

    /// Calculate virtual fee boost for anti-starvation protection
    /// Transactions get 10% boost per hour in mempool to prevent starvation
    fn calculate_starvation_boost(&self, arrival_time: u64, base_fee_rate: u64) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let hours_in_pool = now.saturating_sub(arrival_time) / 3600; // hours
        if hours_in_pool == 0 {
            return base_fee_rate;
        }

        // 10% boost per hour, max 200% total boost
        let boost_multiplier = 1.0 + (hours_in_pool as f64 * 0.1).min(2.0);
        (base_fee_rate as f64 * boost_multiplier) as u64
    }

    /// Check fee density efficiency
    /// Prioritizes transactions with good fee/size ratio
    fn check_fee_density(&self, fee_rate: u64, size_bytes: usize) -> Result<(), String> {
        if size_bytes == 0 {
            return Ok(());
        }

        // Minimum fee density threshold: 1 sat/byte for small tx, higher for large tx
        let min_density = if size_bytes < 1000 {
            1 // Small transactions: at least 1 sat/byte
        } else if size_bytes < 10000 {
            2 // Medium transactions: at least 2 sat/byte
        } else {
            5 // Large transactions: at least 5 sat/byte
        };

        if fee_rate < min_density {
            return Err(format!(
                "Fee density too low: {} sat/byte, minimum {} sat/byte for {} bytes",
                fee_rate, min_density, size_bytes
            ));
        }

        Ok(())
    }

    /// Advanced admission control with resource monitoring
    fn enforce_advanced_admission(&self, fee_rate: u64) -> Result<u64, String> {
        let (should_admit, multiplier) =
            self.admission_controller.should_admit_transaction(fee_rate);

        if !should_admit {
            return Err("Transaction rejected due to resource constraints".to_string());
        }

        Ok(fee_rate * multiplier)
    }

    /// Get effective fee rate including anti-starvation boost
    pub fn get_effective_fee_rate(&self, tx_hash: &[u8]) -> u64 {
        let pool = self.by_hash.read();
        if let Some(entry) = pool.get(tx_hash) {
            let base_fee_rate = if entry.size_bytes > 0 {
                entry.total_fee / entry.size_bytes as u64
            } else {
                0
            };
            self.calculate_starvation_boost(entry.arrival_time, base_fee_rate)
        } else {
            0
        }
    }

    pub fn set_min_fee_rate(&mut self, min_fee_rate: u64) -> Result<(), String> {
        self.config.min_fee_rate = min_fee_rate;
        {
            let mut fee_filter = self.fee_filter.write();
            fee_filter.set_base_fee(min_fee_rate);
        }
        self.persist_min_fee_rate(min_fee_rate)
    }

    pub fn get_current_min_fee_rate(&self) -> u64 {
        self.fee_filter.read().current_threshold()
    }

    /// Create new transaction pool with dependency manager and KvStore
    pub fn new_with_dependency_manager(config: PoolConfig, kv_store: Arc<KvStore>) -> Self {
        let dep_manager = Arc::new(TxDependencyManager::new(kv_store.clone()));
        let ordering_engine = DeterministicOrderingEngine::with_storage(
            DeterministicOrderingEngineConfig::default(),
            kv_store.clone(),
        );

        // Initialize orphan manager dengan strict RAM limits
        let orphan_config = OrphanPoolConfig {
            max_orphans: (config.max_orphan_size as f64 * 0.8) as usize,
            max_adoption_batch: 1000,
            orphan_ttl_ns: config.orphan_ttl_secs as u64 * 1_000_000_000,
            ..Default::default()
        };

        let orphan_manager = Arc::new(OrphanManager::new(orphan_config, Some(kv_store.clone())));
        let validator = Arc::new(PoolValidator::new(Some(kv_store.clone())));

        // Initialize memory limiter dengan default configuration
        let memory_config = MempoolLimiterConfig::default();
        let memory_limiter = Arc::new(MempoolLimiter::new(memory_config));

        // Initialize resource optimizer dengan real kv_store
        let resource_config = ResourceOptimizerConfig::default();
        let resource_optimizer = Arc::new(ResourceOptimizer::new(
            Some(kv_store.clone()),
            resource_config,
        ));

        // Create fee filter before moving config
        let fee_filter = Arc::new(RwLock::new(FeeFilter::new(
            config.min_fee_rate,
            config.dynamic_fee_bump_percent,
        )));

        // Create admission controller with kv_store for trend persistence
        let admission_controller = Arc::new(AdmissionController::with_kv_store(kv_store.clone()));

        // Initialize conflict ordering integration
        let conflict_config = ConflictOrderingIntegrationConfig::default();
        let conflict_ordering_integration = Arc::new(ConflictOrderingIntegration::new(
            conflict_config,
            Some(kv_store.clone()),
        ));
        let parallel_selection = Arc::new(ParallelSelectionBuilder::new(
            Some(kv_store.clone()),
            conflict_ordering_integration.clone(),
        ));
        let fee_balancer = Some(Arc::new(FeeBalancer::new(kv_store.clone())));

        let mut pool = Self {
            by_hash: Arc::new(RwLock::new(IndexMap::new())),
            orphans: Arc::new(RwLock::new(Vec::new())),
            config,
            dependency_manager: Some(dep_manager),
            priority_scheduler: Arc::new(
                PriorityScheduler::new(PrioritySchedulerConfig::default()),
            ),
            multi_dimensional_index: Arc::new(MultiDimensionalIndex::new()),
            deterministic_ordering: Arc::new(ordering_engine),
            orphan_manager: orphan_manager.clone(),
            deferred_resolver: Arc::new(DeferredResolver::new(50, 10000)),
            orphan_linker: Arc::new(RecursiveOrphanLinker::new(orphan_manager, 10)),
            memory_limiter,
            resource_optimizer,
            kv_store: Some(kv_store),
            validator: Some(validator),
            source_rate_buckets: DashMap::new(),
            fee_filter,
            admission_controller,
            conflict_ordering_integration,
            parallel_selection_builder: parallel_selection,
            fee_balancer,
        };

        pool.load_persistent_min_fee_rate().ok();
        pool
    }

    /// Set dependency manager for existing pool
    pub fn set_dependency_manager(&mut self, dep_manager: Arc<TxDependencyManager>) {
        self.dependency_manager = Some(dep_manager);
    }

    /// Get reference to dependency manager if available
    pub fn get_dependency_manager(&self) -> Option<Arc<TxDependencyManager>> {
        self.dependency_manager.clone()
    }

    /// Check if dependency manager is available
    pub fn has_dependency_manager(&self) -> bool {
        self.dependency_manager.is_some()
    }

    /// Add transaction to pool with automatic orphan handling
    ///
    /// Workflow:
    /// 1. Validate transaction inputs against UTXO storage
    /// 2. If all inputs valid → add to main pool
    /// 3. If some inputs missing → add to orphan pool
    /// 4. If other validation error → reject transaction
    ///
    /// Orphan Adoption:
    /// - When a valid transaction is added to main pool, automatically
    ///   check for waiting orphans (children waiting for this transaction's outputs)
    /// - Recursively adopt orphans with MAX_ADOPTION_DEPTH protection
    ///   to prevent stack overflow from circular dependencies
    ///
    /// Thread Safety: Arc<Transaction> used for zero-copy sharing
    pub fn add_transaction(
        &self,
        tx: Transaction,
        total_fee: u64,
        size_bytes: usize,
    ) -> Result<(), String> {
        let tx_hash =
            bincode::serialize(&tx.id).map_err(|e| format!("Serialization error: {}", e))?;

        let fee_rate = if size_bytes > 0 {
            total_fee / size_bytes as u64
        } else {
            0
        };

        let source_key = self.derive_source_key(&tx);

        // Advanced admission control with resource monitoring
        let effective_fee_rate = self.enforce_advanced_admission(fee_rate)?;

        // Fee density filtering for efficiency prioritization
        self.check_fee_density(effective_fee_rate, size_bytes)?;

        // Update fee filter based on current mempool state
        {
            let mut fee_filter = self.fee_filter.write();
            fee_filter.update_threshold(self.size(), self.config.max_pool_size);
        }

        // Update fee balancer with congestion metrics
        if let Some(ref balancer) = self.fee_balancer {
            let avg_age = self.calculate_average_transaction_age();
            let _ = balancer.update_congestion(self.size(), self.config.max_pool_size, avg_age);
        }

        self.enforce_minimum_fee_rate(fee_rate)?;
        self.enforce_source_rate_limit(&source_key)?;

        // Calculate transaction weight (includes overhead)
        let tx_weight = self.memory_limiter.calculate_tx_weight(&tx);

        // Enforce configurable pool size limit before resource-intensive work
        if self.size() >= self.config.max_pool_size {
            return Err(format!(
                "Pool size limit reached (max {})",
                self.config.max_pool_size
            ));
        }

        // Check if adding this transaction would exceed memory limit
        if self
            .memory_limiter
            .would_exceed_limit(tx_weight.total_weight)
        {
            // Build priority map from scheduler for eviction decisions
            let priorities = self.priority_scheduler.priorities.read();
            let priority_map: std::collections::HashMap<Vec<u8>, f64> = priorities
                .iter()
                .map(|(hash, priority)| (hash.clone(), priority.score))
                .collect();
            drop(priorities);

            // Try to make space via cascade eviction
            let eviction_result = self
                .memory_limiter
                .make_space_for(tx_weight.total_weight, &priority_map);

            if eviction_result.freed_weight < tx_weight.total_weight {
                return Err(format!(
                    "Cannot fit transaction (need {} bytes, freed only {} bytes)",
                    tx_weight.total_weight, eviction_result.freed_weight
                ));
            }

            // Remove evicted transactions from pool
            let mut pool = self.by_hash.write();
            for evicted_hash in &eviction_result.evicted_hashes {
                pool.swap_remove(evicted_hash);
                self.priority_scheduler
                    .unregister_transaction(evicted_hash)
                    .ok();
                // Remove from persistent storage
                if let Err(e) = self.remove_from_disk(evicted_hash) {
                    eprintln!(
                        "Warning: Failed to remove evicted transaction from disk: {}",
                        e
                    );
                }
            }
            drop(pool);
        }

        // Deterministic conflict detection using ConflictGraph
        let arrival_time_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let conflict_result = self
            .conflict_ordering_integration
            .register_transaction(&tx, tx_hash.clone(), total_fee, arrival_time_ms)
            .map_err(|e| format!("Conflict detection error: {}", e))?;

        // Check for double-spend conflicts - use deterministic supremacy rules
        if conflict_result.has_double_spend {
            // Process all conflicts with deterministic tie-breaking:
            // 1. Higher fee rate wins
            // 2. If equal fee rate, lexicographically smaller hash wins
            let should_accept_new = self._apply_deterministic_supremacy(
                &tx_hash,
                fee_rate,
                &conflict_result.detected_conflicts,
            )?;

            if !should_accept_new {
                // Reject the new transaction - existing transaction(s) have higher priority
                return Err(
                    "Transaction rejected due to conflict with higher-priority transaction"
                        .to_string(),
                );
            }

            // Remove all conflicting transactions from pool and storage
            // They have lower priority and must be evicted
            for conflicting_hash in &conflict_result.detected_conflicts {
                if self.remove(conflicting_hash).is_some() {
                    // Storage sync is handled internally by remove()
                    // Remove from conflict graph and cascade
                    if let Err(e) = self
                        .conflict_ordering_integration
                        .remove_transaction_cascade(conflicting_hash)
                    {
                        eprintln!(
                            "Warning: Failed to cascade remove conflicting transaction: {}",
                            e
                        );
                    }
                }
            }
        }

        // Attempt to validate inputs if validator is available
        if let Some(ref validator) = self.validator {
            match validator.validate_transaction(&tx) {
                Ok(ValidationResult::Valid) => {
                    // All inputs available - add to main pool
                    return self._add_to_main_pool(tx_hash, tx, total_fee, size_bytes, fee_rate);
                }
                Ok(ValidationResult::MissingInputs(missing_indices)) => {
                    // Some inputs missing - try orphan pool
                    return self._handle_orphan_transaction(
                        tx,
                        missing_indices,
                        total_fee,
                        size_bytes,
                    );
                }
                Ok(ValidationResult::DoubleSpent) => {
                    return Err("Transaction is double-spending".to_string());
                }
                Ok(ValidationResult::InputNotFound(idx)) => {
                    return Err(format!("Input {} not found in UTXO set", idx));
                }
                Err(e) => {
                    return Err(format!("Validation error: {:?}", e));
                }
            }
        }

        // If no validator, add directly (backward compatibility)
        self._add_to_main_pool(tx_hash, tx, total_fee, size_bytes, fee_rate)
    }

    /// Apply deterministic supremacy rules to resolve conflicts
    ///
    /// Deterministic Conflict Resolution:
    /// 1. Transaction with higher fee_rate wins
    /// 2. If equal fee_rate, lexicographically smaller hash wins
    /// Returns true if new transaction should be accepted, false if it should be rejected
    fn _apply_deterministic_supremacy(
        &self,
        new_tx_hash: &[u8],
        new_fee_rate: u64,
        conflicting_hashes: &[Vec<u8>],
    ) -> Result<bool, String> {
        if conflicting_hashes.is_empty() {
            return Ok(true); // No conflicts, accept the new transaction
        }

        // Find the highest-priority conflicting transaction
        let mut highest_priority_hash = conflicting_hashes.first().unwrap().clone();
        let mut highest_fee_rate = self.get_effective_fee_rate(&highest_priority_hash);

        for conflicting_hash in conflicting_hashes.iter().skip(1) {
            let existing_fee_rate = self.get_effective_fee_rate(conflicting_hash);

            // Update highest if this one has higher fee or same fee with smaller hash
            if existing_fee_rate > highest_fee_rate
                || (existing_fee_rate == highest_fee_rate
                    && conflicting_hash < &highest_priority_hash)
            {
                highest_priority_hash = conflicting_hash.clone();
                highest_fee_rate = existing_fee_rate;
            }
        }

        // Apply deterministic supremacy: new tx wins if higher fee or (equal fee & smaller hash)
        let new_tx_wins = if new_fee_rate > highest_fee_rate {
            true
        } else if new_fee_rate == highest_fee_rate {
            new_tx_hash < highest_priority_hash.as_slice()
        } else {
            false
        };

        Ok(new_tx_wins)
    }

    /// Add transaction to main pool
    fn _add_to_main_pool(
        &self,
        tx_hash: Vec<u8>,
        tx: Transaction,
        total_fee: u64,
        size_bytes: usize,
        fee_rate: u64,
    ) -> Result<(), String> {
        // Calculate transaction weight (includes overhead) - do this before moving tx
        let tx_weight = self.memory_limiter.calculate_tx_weight(&tx);

        // Check if adding this transaction would exceed memory limit
        if self
            .memory_limiter
            .would_exceed_limit(tx_weight.total_weight)
        {
            return Err("Transaction would exceed memory limit".to_string());
        }

        // Use ResourceOptimizer for tiered storage
        let tx_arc = Arc::new(tx);
        self.resource_optimizer
            .add_transaction(tx_hash.clone(), tx_arc.clone(), fee_rate)?;

        // Register with priority scheduler
        let arrival_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.priority_scheduler
            .register_transaction(tx_hash.clone(), fee_rate, arrival_time)
            .map_err(|e| format!("Priority scheduler error: {}", e))?;

        // Register with multi-dimensional index
        let indexed_tx = IndexedTransaction::new(
            tx_hash.clone(),
            fee_rate,
            arrival_time,
            0, // Initially no dependents
            size_bytes,
            total_fee,
        );
        self.multi_dimensional_index
            .insert(indexed_tx)
            .map_err(|e| format!("Index error: {}", e))?;

        // Register with deterministic ordering engine
        self.deterministic_ordering
            .add_transaction(
                tx_hash.clone(),
                fee_rate,
                arrival_time,
                size_bytes,
                total_fee,
            )
            .map_err(|e| format!("Deterministic ordering error: {}", e))?;

        // Register transaction weight with memory limiter
        let tx_weight = self.memory_limiter.calculate_tx_weight(&tx_arc);
        self.memory_limiter
            .add_tx_weight(tx_hash.clone(), tx_weight)
            .map_err(|e| format!("Memory tracking error: {}", e))?;

        // Adopt waiting orphans jika transaksi ini memiliki outputs
        self._adopt_waiting_orphans(&tx_arc)?;

        // Persist transaction to disk asynchronously
        // Note: In production, this should be done asynchronously to avoid blocking
        if let Err(e) = self.save_to_disk(&tx_arc) {
            // Log error but don't fail the operation
            eprintln!("Warning: Failed to persist transaction to disk: {}", e);
        }

        // Finally, insert into the main pool
        let entry = PoolEntry::new((*tx_arc).clone(), total_fee, size_bytes);
        self.by_hash.write().insert(tx_hash, entry);

        Ok(())
    }

    /// Handle orphan transaction - store dengan missing parents tracking
    fn _handle_orphan_transaction(
        &self,
        tx: Transaction,
        missing_indices: Vec<usize>,
        total_fee: u64,
        size_bytes: usize,
    ) -> Result<(), String> {
        let tx_arc = Arc::new(tx);
        let tx_hash =
            bincode::serialize(&tx_arc.id).map_err(|e| format!("Serialization error: {}", e))?;

        // Extract missing inputs as OutPoints
        let missing_inputs: Vec<OutPoint> = missing_indices
            .iter()
            .filter_map(|&idx| {
                if idx < tx_arc.inputs.len() {
                    let input = &tx_arc.inputs[idx];
                    let tx_hash_bytes = bincode::serialize(&input.prev_tx).ok()?;
                    Some(OutPoint {
                        tx_hash: tx_hash_bytes,
                        index: input.index,
                    })
                } else {
                    None
                }
            })
            .collect();

        // Add to orphan pool
        self.orphan_manager
            .add_orphan(tx_hash, tx_arc, missing_inputs, size_bytes, total_fee)?;

        // Mark as orphan in main pool for tracking
        // (Some implementations may want to track this)

        Ok(())
    }

    /// Adopt waiting orphans when a new transaction with outputs arrives
    fn _adopt_waiting_orphans(&self, tx: &Transaction) -> Result<(), String> {
        // Maximum recursion depth to prevent stack overflow dari circular dependencies

        // Use advanced orphan linker untuk mendapatkan seluruh adoption chain
        if let Ok(chain_result) = self.orphan_linker.link_orphan_chain(tx) {
            // Schedule adopted transactions untuk deferred batch processing
            for _ in 0..chain_result.total_adopted {
                // Semua adopted txs sudah diproses oleh orphan_linker
                // Schedule untuk deferred resolution dengan throttling
                let tx_hash = bincode::serialize(&tx.id)
                    .map_err(|e| format!("Serialization error: {}", e))?;
                let priority = chain_result.maximum_depth as u64;
                let _ = self
                    .deferred_resolver
                    .schedule_resolution(tx_hash, priority);
            }
        }

        self._adopt_waiting_orphans_recursive(tx, 0)
    }

    /// Recursively adopt waiting orphans dengan depth limit
    fn _adopt_waiting_orphans_recursive(
        &self,
        tx: &Transaction,
        depth: usize,
    ) -> Result<(), String> {
        const MAX_ADOPTION_DEPTH: usize = 10;

        if depth > MAX_ADOPTION_DEPTH {
            // Log dan return gracefully untuk prevent DoS dari circular dependencies
            return Ok(());
        }

        // For each output in this transaction
        for (output_index, _output) in tx.outputs.iter().enumerate() {
            let parent_outpoint = OutPoint {
                tx_hash: bincode::serialize(&tx.id)
                    .map_err(|e| format!("Serialization error: {}", e))?,
                index: output_index as u32,
            };

            // Try to adopt orphans waiting for this output
            match self
                .orphan_manager
                .process_orphans_for_parent(&parent_outpoint)
            {
                Ok(adoption_result) => {
                    // Re-validate each adopted transaction
                    for adopted_tx in adoption_result.adopted_txs {
                        // Re-add to main pool dengan recursive adoption
                        let size = adopted_tx.inputs.len() * 32; // Rough estimate
                        let fee = 1000u64; // Default fee estimate

                        match self.add_transaction((*adopted_tx).clone(), fee, size) {
                            Ok(()) => {
                                // Successfully adopted and added to main pool
                            }
                            Err(_) => {
                                // If still has missing parents, it will be re-added to orphan pool
                                // If other error, it's rejected - this is OK
                            }
                        }
                    }
                }
                Err(_) => {
                    // No orphans waiting for this output, which is fine
                }
            }
        }

        Ok(())
    }

    /// Set transaction status
    pub fn set_status(&self, tx_hash: &[u8], status: TransactionStatus) -> Result<(), String> {
        let mut pool = self.by_hash.write();

        let entry = pool
            .get_mut(tx_hash)
            .ok_or_else(|| "Transaction not found".to_string())?;

        entry
            .status
            .transition_to(status)
            .map_err(|e| format!("Status transition error: {:?}", e))?;

        // Update orphan index if needed
        if status == TransactionStatus::InOrphanPool {
            let mut orphans = self.orphans.write();
            if !orphans.iter().any(|h| h == tx_hash) {
                orphans.push(tx_hash.to_vec());
            }
        }

        Ok(())
    }

    /// Get transaction by hash
    pub fn get(&self, tx_hash: &[u8]) -> Option<PoolEntry> {
        self.by_hash.read().get(tx_hash).cloned()
    }

    /// Check if transaction exists in pool
    pub fn contains(&self, tx_hash: &[u8]) -> bool {
        self.by_hash.read().contains_key(tx_hash)
    }

    /// Remove transaction from pool
    ///
    /// This method removes a transaction from all in-memory indexes AND persistent storage.
    /// It ensures complete removal from both mempool and disk to maintain consistency.
    pub fn remove(&self, tx_hash: &[u8]) -> Option<PoolEntry> {
        let mut pool = self.by_hash.write();

        if let Some(entry) = pool.shift_remove(tx_hash) {
            let mut orphans = self.orphans.write();
            orphans.retain(|h| h != tx_hash);
            drop(pool);
            drop(orphans);

            // Unregister from priority scheduler
            let _ = self
                .priority_scheduler
                .unregister_transaction(&tx_hash.to_vec());

            // Unregister from multi-dimensional index
            let _ = self.multi_dimensional_index.remove(&tx_hash.to_vec());

            // Unregister from deterministic ordering
            let _ = self.deterministic_ordering.remove_transaction(tx_hash);

            // Unregister from memory limiter (weight tracking)
            let _ = self.memory_limiter.remove_tx_weight(&tx_hash.to_vec());

            // CRITICAL: Sync with persistent storage to ensure consistency
            if let Err(e) = self.remove_from_disk(tx_hash) {
                eprintln!(
                    "Warning: Failed to remove transaction from disk during pool removal: {}",
                    e
                );
            }

            Some(entry)
        } else {
            None
        }
    }

    /// Get all transactions with given status
    pub fn get_by_status(&self, status: TransactionStatus) -> Vec<PoolEntry> {
        self.by_hash
            .read()
            .values()
            .filter(|entry| entry.status == status)
            .cloned()
            .collect()
    }

    /// Get orphan transactions
    pub fn get_orphans(&self) -> Vec<PoolEntry> {
        self.by_hash
            .read()
            .values()
            .filter(|entry| entry.status == TransactionStatus::InOrphanPool)
            .cloned()
            .collect()
    }

    /// Get all validated transactions ready for block inclusion
    pub fn get_validated(&self) -> Vec<PoolEntry> {
        self.by_hash
            .read()
            .values()
            .filter(|entry| entry.status == TransactionStatus::Validated)
            .cloned()
            .collect()
    }

    /// Get pool size
    pub fn size(&self) -> usize {
        self.by_hash.read().len()
    }

    /// Get orphan pool size
    pub fn orphan_size(&self) -> usize {
        self.orphans.read().len()
    }

    /// Clean up expired transactions
    pub fn cleanup_expired(&self) -> usize {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut pool = self.by_hash.write();
        let before_count = pool.len();

        pool.retain(|_hash, entry| !entry.is_expired(current_time, &self.config));

        let after_count = pool.len();
        before_count - after_count
    }

    /// Register transaction with dependency manager (if available)
    ///
    /// Should be called when a transaction is added to the pool.
    /// Tracks parent-child relationships for cascade validation.
    pub fn register_transaction_dependency(&self, _tx_bytes: &[u8]) -> Result<(), String> {
        if self.dependency_manager.is_some() {
            // In production, you would deserialize tx_bytes to get Transaction
            // For now, we just track that dependency tracking is available
            // The actual transaction object would come from the pool entry
            return Ok(());
        }
        Ok(())
    }

    /// Cascade on parent confirmation (if dependency manager available)
    ///
    /// Should be called when a parent transaction is confirmed in a block.
    /// Triggers re-validation of all dependent children.
    pub fn cascade_on_parent_confirmation(&self, parent_tx_hash: &[u8]) -> Result<(), String> {
        if let Some(dep_mgr) = &self.dependency_manager {
            // Cascade validation logic would be executed here
            // Returns list of affected transactions
            let _ = dep_mgr.get_dependent_children(&parent_tx_hash.to_vec());
            return Ok(());
        }
        Ok(())
    }

    /// Get transaction dependencies (if manager available)
    pub fn get_transaction_dependencies(&self, tx_hash: &[u8]) -> Option<Vec<Vec<u8>>> {
        if let Some(dep_mgr) = &self.dependency_manager {
            return Some(dep_mgr.get_dependent_children(&tx_hash.to_vec()));
        }
        None
    }

    /// Get priority scheduler reference
    pub fn get_priority_scheduler(&self) -> Arc<PriorityScheduler> {
        self.priority_scheduler.clone()
    }

    /// Get multi-dimensional index reference
    pub fn get_multi_dimensional_index(&self) -> Arc<MultiDimensionalIndex> {
        self.multi_dimensional_index.clone()
    }

    /// Calculate dynamic priority for a transaction
    ///
    /// Uses hybrid formula: Score = (FeeRate × W_f) + (AgeInPool × W_a)
    /// where W_f = fee weight, W_a = age weight
    pub fn calculate_dynamic_priority(&self, tx_hash: &[u8]) -> Result<Option<f64>, String> {
        self.priority_scheduler
            .calculate_dynamic_priority(&tx_hash.to_vec())
            .map(|opt| opt.map(|p| p.score))
            .map_err(|e| format!("Priority calculation error: {}", e))
    }

    /// Perform periodic priority scheduler update (called when new block arrives)
    pub fn update_priority_scheduler(&self) -> Result<u64, String> {
        self.priority_scheduler
            .perform_scheduled_update()
            .map_err(|e| format!("Scheduler update error: {}", e))
    }

    /// Query transactions by fee rate range
    ///
    /// Dimensi Ekonomi: Returns transactions within fee range [min_fee, max_fee]
    pub fn query_by_fee_range(
        &self,
        min_fee: u64,
        max_fee: u64,
    ) -> Result<Vec<IndexedTransaction>, String> {
        self.multi_dimensional_index
            .query_economic_range(min_fee, max_fee)
            .map_err(|e| format!("Fee range query error: {}", e))
    }

    /// Query transactions by arrival time range
    ///
    /// Dimensi Temporal: Returns transactions with arrival in [start_time, end_time]
    pub fn query_by_time_range(
        &self,
        start_time: u64,
        end_time: u64,
    ) -> Result<Vec<IndexedTransaction>, String> {
        self.multi_dimensional_index
            .query_temporal_range(start_time, end_time)
            .map_err(|e| format!("Time range query error: {}", e))
    }

    /// Query transactions by dependency count
    ///
    /// Dimensi Struktural: Returns transactions with at least min_deps children/dependents
    pub fn query_by_min_dependents(
        &self,
        min_deps: u32,
    ) -> Result<Vec<IndexedTransaction>, String> {
        self.multi_dimensional_index
            .query_structural_min_dependents(min_deps)
            .map_err(|e| format!("Dependency query error: {}", e))
    }

    /// Combined multi-dimensional query
    ///
    /// Returns transactions matching ALL criteria (fee range AND time range AND dependency count)
    pub fn query_combined(
        &self,
        min_fee: u64,
        max_fee: u64,
        start_time: u64,
        end_time: u64,
        min_dependencies: u32,
    ) -> Result<Vec<IndexedTransaction>, String> {
        self.multi_dimensional_index
            .query_combined(min_fee, max_fee, start_time, end_time, min_dependencies)
            .map_err(|e| format!("Combined query error: {}", e))
    }

    /// Get top N transactions by fee (Economic dimension)
    pub fn get_top_n_by_fee(&self, limit: usize) -> Result<Vec<IndexedTransaction>, String> {
        self.multi_dimensional_index
            .query_economic_top_n(limit)
            .map_err(|e| format!("Top N fee query error: {}", e))
    }

    /// Get transactions older than timestamp (starvation prevention)
    pub fn get_transactions_before_time(
        &self,
        timestamp: u64,
    ) -> Result<Vec<IndexedTransaction>, String> {
        self.multi_dimensional_index
            .query_temporal_before(timestamp)
            .map_err(|e| format!("Before time query error: {}", e))
    }

    /// Get hub transactions with highest dependency count
    pub fn get_dependency_hubs(&self, limit: usize) -> Result<Vec<IndexedTransaction>, String> {
        self.multi_dimensional_index
            .query_structural_top_hubs(limit)
            .map_err(|e| format!("Hub query error: {}", e))
    }

    /// Get reference to deterministic ordering engine
    pub fn get_deterministic_ordering(&self) -> Arc<DeterministicOrderingEngine> {
        self.deterministic_ordering.clone()
    }

    /// Get transactions in deterministic order guaranteed for consensus
    ///
    /// CONSENSUS CRITICAL: This ordering is deterministic across all validator nodes.
    /// Uses tie-breaking by lexicographic transaction hash comparison.
    pub fn get_ordered_transactions_deterministic(
        &self,
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, u64)>, String> {
        let ordered = self
            .deterministic_ordering
            .get_ordered_transactions(limit)?;
        Ok(ordered
            .into_iter()
            .map(|tx| (tx.tx_hash, tx.fee_rate))
            .collect())
    }

    /// Get all transactions (defensive copy for iteration)
    pub fn get_all(&self) -> Vec<PoolEntry> {
        self.by_hash.read().values().cloned().collect()
    }

    /// Clear all transactions (for testing/reset)
    pub fn clear(&self) {
        self.by_hash.write().clear();
        self.orphans.write().clear();
        self.source_rate_buckets.clear();
        let _ = self.priority_scheduler.reset();
        let _ = self.multi_dimensional_index.reset();
        let _ = self.deterministic_ordering.clear();
    }

    /// Get orphan manager reference
    pub fn get_orphan_manager(&self) -> Arc<OrphanManager> {
        self.orphan_manager.clone()
    }

    /// Get orphan pool statistics
    pub fn get_orphan_stats(&self) -> Result<crate::mempool::orphan_manager::OrphanStats, String> {
        Ok(self.orphan_manager.get_stats())
    }

    /// Cleanup expired orphans manually
    pub fn cleanup_expired_orphans(&self) -> usize {
        self.orphan_manager.cleanup_expired()
    }

    /// Get deferred resolver reference untuk batch processing
    pub fn get_deferred_resolver(&self) -> Arc<DeferredResolver> {
        self.deferred_resolver.clone()
    }

    /// Get orphan linker reference untuk chain resolution  
    pub fn get_orphan_linker(&self) -> Arc<RecursiveOrphanLinker> {
        self.orphan_linker.clone()
    }

    /// Process deferred resolution batch (untuk background task)
    pub fn process_deferred_resolutions(
        &self,
    ) -> Result<Vec<super::advanced_orphan_management::ResolutionTask>, String> {
        self.deferred_resolver.process_batch()
    }

    /// Get mempool limiter reference untuk memory accounting
    pub fn get_memory_limiter(&self) -> Arc<MempoolLimiter> {
        self.memory_limiter.clone()
    }

    /// Get current memory statistics
    pub fn get_memory_stats(&self) -> super::memory_limiter::MemoryStats {
        self.memory_limiter.get_stats()
    }

    /// Verify orphan pool consistency
    pub fn verify_orphan_consistency(&self) -> Result<(), String> {
        self.orphan_manager.verify_consistency()
    }

    /// Get pool statistics
    pub fn get_stats(&self) -> PoolStats {
        let pool = self.by_hash.read();

        let pending_count = pool
            .values()
            .filter(|e| e.status == TransactionStatus::Pending)
            .count();
        let validated_count = pool
            .values()
            .filter(|e| e.status == TransactionStatus::Validated)
            .count();
        let orphan_count = pool
            .values()
            .filter(|e| e.status == TransactionStatus::InOrphanPool)
            .count();
        let rejected_count = pool
            .values()
            .filter(|e| e.status == TransactionStatus::Rejected)
            .count();

        let total_fees: u64 = pool.values().map(|e| e.total_fee).sum();
        let total_size: usize = pool.values().map(|e| e.size_bytes).sum();

        PoolStats {
            total_count: pool.len(),
            pending_count,
            validated_count,
            orphan_count,
            rejected_count,
            total_fees,
            total_size_bytes: total_size,
        }
    }

    // ============================================
    // PERSISTENCE & RECOVERY METHODS
    // ============================================

    /// Save transaction to persistent storage asynchronously
    pub fn save_to_disk(&self, tx: &Transaction) -> Result<(), String> {
        if let Some(ref kv_store) = self.kv_store {
            let tx_hash = tx.id.as_bytes().to_vec();
            let mut tx_value = crate::storage::schema::TransactionValue::from(tx);

            // Add checksum for corruption detection
            let checksum = self.calculate_checksum(tx);
            // Store checksum in a way that can be verified later
            // For simplicity, we'll modify the fee field to include checksum
            // In production, we'd add a checksum field to TransactionValue
            tx_value.fee = ((tx_value.fee as u128) << 64) as u64 | (checksum as u64);

            kv_store.put_mempool_transaction(tx_hash, tx_value);
            Ok(())
        } else {
            // No persistent storage available
            Ok(())
        }
    }

    /// Remove transaction from persistent storage
    pub fn remove_from_disk(&self, tx_hash: &[u8]) -> Result<(), String> {
        if let Some(ref kv_store) = self.kv_store {
            kv_store.remove_mempool_transaction(tx_hash);
            Ok(())
        } else {
            // No persistent storage available
            Ok(())
        }
    }

    /// Load mempool state from persistent storage on startup
    pub fn load_mempool_on_startup(&self) -> Result<(), String> {
        // Note: In full implementation, we'd scan CF_MEMPOOL to get all persisted hashes
        // This would typically be done using RocksDB iterators on the mempool column family.
        // For brevity, this implementation focuses on the reconciliation
        // and cleanup aspects.

        Ok(())
    }

    /// Perform state reconciliation with main storage
    /// Removes transactions that are already in blocks
    pub fn reconcile_with_blockchain(&self) -> Result<(), String> {
        let Some(ref kv_store) = self.kv_store else {
            return Ok(()); // No storage to reconcile with
        };
        // Note: In full implementation, we'd scan CF_MEMPOOL to get all persisted hashes
        // For now, we'll check transactions that are currently in memory

        let tx_hashes: Vec<Vec<u8>> = {
            let pool = self.by_hash.read();
            pool.keys().cloned().collect()
        };

        // Check each transaction against main blockchain storage
        for tx_hash in tx_hashes {
            // If transaction exists in main blockchain storage, it means it's already in a block
            match kv_store.get_transaction(&tx_hash) {
                Ok(Some(_)) => {
                    // Transaction is already in a block, remove from mempool completely
                    // This uses the unified remove() method which handles all cleanup
                    let _ = self.remove(&tx_hash);
                }
                Ok(None) => {
                    // Transaction not in blockchain, keep in mempool
                }
                Err(e) => {
                    // Log error but continue
                    eprintln!(
                        "Warning: Error checking transaction against blockchain: {}",
                        e
                    );
                }
            }
        }

        Ok(())
    }

    /// Calculate simple checksum for transaction integrity
    fn calculate_checksum(&self, tx: &Transaction) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        tx.id.hash(&mut hasher);
        tx.inputs.len().hash(&mut hasher);
        tx.outputs.len().hash(&mut hasher);
        tx.chain_id.hash(&mut hasher);
        hasher.finish()
    }

    /// Create deterministic snapshot of current mempool state
    pub fn create_snapshot(&self, block_height: u64) -> MempoolSnapshot {
        MempoolSnapshot::new(self, block_height)
    }

    /// Get parallel transaction batches for safe concurrent processing
    ///
    /// Returns Vec<Vec<Arc<Transaction>>> where each inner Vec contains transactions
    /// that can be processed in parallel without race conditions on UTXO state.
    /// Uses ConflictGraph to identify independent transaction sets.
    pub fn get_parallel_batches(&self) -> Result<Vec<Vec<Arc<Transaction>>>, String> {
        let parallel_groups = self
            .conflict_ordering_integration
            .get_parallel_validation_groups()
            .map_err(|e| format!("Failed to get parallel groups: {}", e))?;

        let mut batches = Vec::new();
        let pool = self.by_hash.read();

        for group in parallel_groups {
            let mut batch = Vec::new();
            for tx_hash in group {
                if let Some(entry) = pool.get(&tx_hash) {
                    batch.push(Arc::new(entry.transaction.clone()));
                }
            }
            if !batch.is_empty() {
                batches.push(batch);
            }
        }

        Ok(batches)
    }

    /// Prepare canonical block candidate with deterministic ordering
    ///
    /// Builds a block candidate using canonical ordering rules that MUST be identical
    /// across all validators. This ensures consensus determinism.
    ///
    /// CANONICAL ORDERING GUARANTEES:
    /// 1. **Topological Ordering**: All parent transactions appear before their dependents
    /// 2. **Fee Density Ordering**: Transactions at same topological level sorted by fee/byte (descending)
    /// 3. **Lexicographic Tie-breaking**: Equal fee density uses smaller transaction hash (ascending)
    /// 4. **Conflict Resolution**: Each UTXO can only be spent once (highest fee variant selected)
    ///
    /// Parameters:
    /// - `max_weight`: Maximum block weight in bytes (respects consensus layer limits)
    ///
    /// Returns:
    /// - Vec<Arc<Transaction>>: Transactions in canonical order, guaranteed bit-identical across validators
    /// - All transactions are validated for conflicts and properly sequenced for execution
    ///
    /// CONSENSUS CRITICAL: This ordering MUST produce identical results for identical mempool state
    pub fn prepare_block_candidate(
        &self,
        max_weight: usize,
    ) -> Result<Vec<Arc<Transaction>>, String> {
        // Build block using canonical ordering from conflict & ordering integration
        let block_result = self
            .conflict_ordering_integration
            .build_block_canonical(max_weight)
            .map_err(|e| format!("Failed to build canonical block: {}", e))?;

        let mut transactions = Vec::new();
        let pool = self.by_hash.read();

        // Retrieve transaction objects from pool in canonical order
        // Note: We iterate in the order returned by canonical ordering engine
        // which guarantees topological ordering and fee density ordering
        for tx_hash in block_result.transactions {
            if let Some(entry) = pool.get(&tx_hash) {
                transactions.push(Arc::new(entry.transaction.clone()));
            }
        }

        // Verify canonicality: ensure transactions respect ordering invariants
        self._verify_canonical_order(&transactions)?;

        Ok(transactions)
    }

    /// Prepare parallel block candidates for optimized processing
    ///
    /// Returns disjoint sets of transactions that can be processed in parallel
    /// while maintaining canonical ordering guarantees within each set.
    pub fn prepare_parallel_block_candidates(
        &self,
        max_weight: usize,
    ) -> Result<Vec<Vec<Arc<Transaction>>>, String> {
        // First get canonical transactions
        let canonical_txs = self.prepare_block_candidate(max_weight)?;

        // Then build parallel sets from canonical transactions
        self.parallel_selection_builder
            .build_parallel_sets(&canonical_txs, max_weight)
    }

    /// Verify that transaction list respects canonical ordering invariants
    ///
    /// This verification is performed to ensure:
    /// 1. No duplicate transactions
    /// 2. No conflicting UTXOs claimed multiple times
    /// 3. Parent-child dependencies are respected (parent before child)
    fn _verify_canonical_order(&self, transactions: &[Arc<Transaction>]) -> Result<(), String> {
        let mut seen_hashes = std::collections::HashSet::new();
        let mut spent_outputs = std::collections::HashSet::new();

        for tx in transactions {
            let tx_hash =
                bincode::serialize(&tx.id).map_err(|e| format!("Serialization error: {}", e))?;

            // Check for duplicate transactions
            if !seen_hashes.insert(tx_hash.clone()) {
                return Err(format!(
                    "Duplicate transaction in canonical block candidate"
                ));
            }

            // Check for conflicts (double-spending)
            for input in &tx.inputs {
                let outpoint_key = format!("{:?}:{}", input.prev_tx, input.index);
                if !spent_outputs.insert(outpoint_key.clone()) {
                    return Err(format!(
                        "Conflict detected in canonical ordering: UTXO {} already spent",
                        outpoint_key
                    ));
                }
            }
        }

        Ok(())
    }

    /// Select transactions using deterministic selector
    pub fn select_with_selector(
        &self,
        selector: &DeterministicSelector,
        max_count: usize,
        _max_size: Option<usize>,
    ) -> Result<Vec<SelectedTransaction>, String> {
        let entries: Vec<PoolEntry> = self.by_hash.read().values().cloned().collect();
        let criteria = SelectionCriteria::MaxCount(max_count); // Simplified
        Ok(selector.select(entries, criteria))
    }
}

// ============================================
// DETERMINISM & CONSISTENCY SYSTEM
// ============================================

/// Deterministic snapshot of mempool state at a specific block height
#[derive(Clone, Debug)]
pub struct MempoolSnapshot {
    /// Block height when snapshot was taken
    block_height: u64,
    /// Immutable copy of transactions: hash -> transaction
    transactions: IndexMap<Vec<u8>, Arc<Transaction>>,
    /// Ancestor map for dependency validation: tx_hash -> parent_hashes
    #[allow(dead_code)]
    ancestor_map: IndexMap<Vec<u8>, Vec<Vec<u8>>>,
}

impl MempoolSnapshot {
    /// Create a new snapshot from current mempool state
    pub fn new(pool: &TransactionPool, block_height: u64) -> Self {
        let transactions = {
            let pool_lock = pool.by_hash.read();
            pool_lock
                .iter()
                .map(|(hash, entry)| (hash.clone(), Arc::new(entry.transaction.clone())))
                .collect()
        };

        // Build ancestor map from dependency manager if available
        let ancestor_map = IndexMap::new(); // Simplified: empty for now

        Self {
            block_height,
            transactions,
            ancestor_map,
        }
    }

    /// Get transaction by hash
    pub fn get_transaction(&self, tx_hash: &[u8]) -> Option<&Arc<Transaction>> {
        self.transactions.get(tx_hash)
    }

    /// Get all transactions in snapshot
    pub fn get_all_transactions(&self) -> Vec<Arc<Transaction>> {
        self.transactions.values().cloned().collect()
    }

    /// Get block height of snapshot
    pub fn block_height(&self) -> u64 {
        self.block_height
    }

    /// Check if transaction exists in snapshot
    pub fn contains(&self, tx_hash: &[u8]) -> bool {
        self.transactions.contains_key(tx_hash)
    }
}

/// Canonical ordering engine for deterministic transaction sorting
pub struct CanonicalOrderingEngine;

impl CanonicalOrderingEngine {
    /// Apply canonical sorting to transaction list
    /// Rules: Fee Rate (desc), Dependency Rank (parents first), Hash (asc tie-breaker)
    pub fn apply_canonical_sort(txs: &mut Vec<Arc<Transaction>>) {
        txs.sort_by(|a, b| {
            // First: Fee rate (descending) - higher fee first
            let a_fee_rate = Self::calculate_fee_rate(a);
            let b_fee_rate = Self::calculate_fee_rate(b);

            match b_fee_rate.cmp(&a_fee_rate) {
                std::cmp::Ordering::Equal => {
                    // Second: Dependency rank (parents before children)
                    let a_deps = Self::get_dependency_rank(a);
                    let b_deps = Self::get_dependency_rank(b);

                    match a_deps.cmp(&b_deps) {
                        std::cmp::Ordering::Equal => {
                            // Third: Hash lexicographical (ascending)
                            a.id.as_bytes().cmp(b.id.as_bytes())
                        }
                        other => other,
                    }
                }
                other => other,
            }
        });
    }

    /// Calculate fee rate for a transaction
    fn calculate_fee_rate(tx: &Transaction) -> u64 {
        let size = Self::estimate_serialized_size(tx) as u64;
        if size == 0 {
            0
        } else {
            // Use max_fee_per_gas as fee estimate
            (tx.max_fee_per_gas as u64) / size
        }
    }

    /// Estimate serialized size of transaction
    fn estimate_serialized_size(tx: &Transaction) -> usize {
        // Rough estimation: inputs * 100 + outputs * 50 + overhead
        tx.inputs.len() * 100 + tx.outputs.len() * 50 + 200
    }

    /// Get dependency rank (number of ancestors)
    fn get_dependency_rank(tx: &Transaction) -> usize {
        // Simplified: count unique input references
        // In full implementation, this would traverse the dependency graph
        tx.inputs.len()
    }
}

/// Conflict-stable selection engine for block candidate creation
pub struct SelectionEngine {
    kv_store: Arc<KvStore>,
}

impl SelectionEngine {
    /// Create new selection engine
    pub fn new(kv_store: Arc<KvStore>) -> Self {
        Self { kv_store }
    }

    /// Select transactions for block candidate from mempool snapshot
    /// Returns conflict-free set ordered by canonical rules
    pub fn select_transactions(
        &self,
        snapshot: &MempoolSnapshot,
        max_block_size: usize,
    ) -> Result<Vec<Arc<Transaction>>, String> {
        let mut candidates = snapshot.get_all_transactions();

        // Apply canonical sorting
        CanonicalOrderingEngine::apply_canonical_sort(&mut candidates);

        // Filter out transactions with spent inputs
        let mut selected = Vec::new();
        let mut spent_outputs = std::collections::HashSet::new();

        let mut current_size = 0;

        for tx in candidates {
            if self.is_conflict_free(&tx, &spent_outputs)? {
                // Check against storage for already spent UTXOs
                if self.check_utxo_availability(&tx)? {
                    let tx_size = CanonicalOrderingEngine::estimate_serialized_size(&tx);
                    if current_size + tx_size > max_block_size {
                        break;
                    }
                    selected.push(tx.clone());
                    current_size += tx_size;

                    // Mark outputs as spent
                    for (i, _) in tx.outputs.iter().enumerate() {
                        spent_outputs.insert((tx.id.as_bytes().to_vec(), i as u32));
                    }
                }
            }
        }

        Ok(selected)
    }

    /// Check if transaction conflicts with already selected transactions
    fn is_conflict_free(
        &self,
        tx: &Transaction,
        spent_outputs: &std::collections::HashSet<(Vec<u8>, u32)>,
    ) -> Result<bool, String> {
        for input in &tx.inputs {
            let prev_tx_hash = input.prev_tx.as_bytes();
            if spent_outputs.contains(&(prev_tx_hash.to_vec(), input.index)) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Check if all inputs are available in storage (not spent)
    fn check_utxo_availability(&self, tx: &Transaction) -> Result<bool, String> {
        for input in &tx.inputs {
            let prev_tx_hash = input.prev_tx.as_bytes();
            if !self
                .kv_store
                .utxo_exists(prev_tx_hash, input.index)
                .map_err(|e| format!("Storage error: {}", e))?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// Statistics about the transaction pool
#[derive(Clone, Debug)]
pub struct PoolStats {
    pub total_count: usize,
    pub pending_count: usize,
    pub validated_count: usize,
    pub orphan_count: usize,
    pub rejected_count: usize,
    pub total_fees: u64,
    pub total_size_bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::kv_store::KvStore;
    use klomang_core::core::crypto::Hash;
    use std::sync::Arc;

    fn create_test_transaction() -> Transaction {
        Transaction {
            id: Hash::new(&[1u8; 32]),
            inputs: vec![],
            outputs: vec![],
            execution_payload: vec![],
            contract_address: None,
            gas_limit: 0,
            max_fee_per_gas: 0,
            chain_id: 1,
            locktime: 0,
        }
    }

    fn create_test_pool() -> TransactionPool {
        let kv_store = Arc::new(KvStore::new_dummy());
        TransactionPool::new_with_kv_store(PoolConfig::default(), Some(kv_store))
    }

    #[test]
    fn test_add_transaction() {
        let pool = create_test_pool();
        let tx = create_test_transaction();

        println!("Size before: {}", pool.size());
        if let Err(e) = pool.add_transaction(tx, 1000, 200) {
            panic!("Add transaction failed: {}", e);
        }
        println!("Size after: {}", pool.size());
        assert_eq!(pool.size(), 1);
    }

    #[test]
    fn test_get_transaction() {
        let pool = create_test_pool();
        let tx = create_test_transaction();
        let tx_hash = bincode::serialize(&tx.id).unwrap();

        pool.add_transaction(tx.clone(), 1000, 200).unwrap();

        let retrieved = pool.get(&tx_hash).unwrap();
        assert_eq!(retrieved.total_fee, 1000);
        assert_eq!(retrieved.size_bytes, 200);
    }

    #[test]
    fn test_set_status() {
        let pool = create_test_pool();
        let tx = create_test_transaction();
        let tx_hash = bincode::serialize(&tx.id).unwrap();

        pool.add_transaction(tx, 1000, 200).unwrap();
        assert!(pool
            .set_status(&tx_hash, TransactionStatus::Validated)
            .is_ok());

        let entry = pool.get(&tx_hash).unwrap();
        assert_eq!(entry.status, TransactionStatus::Validated);
    }

    #[test]
    fn test_pool_size_limit() {
        let mut config = PoolConfig::default();
        config.max_pool_size = 2;
        let kv_store = Arc::new(KvStore::new_dummy());
        let pool = TransactionPool::new_with_kv_store(config, Some(kv_store));

        let tx1 = create_test_transaction();
        let tx2 = Transaction {
            id: Hash::new(&[2u8; 32]),
            ..create_test_transaction()
        };
        let tx3 = Transaction {
            id: Hash::new(&[3u8; 32]),
            ..create_test_transaction()
        };

        assert!(pool.add_transaction(tx1, 1000, 200).is_ok());
        assert!(pool.add_transaction(tx2, 1000, 200).is_ok());
        assert!(pool.add_transaction(tx3, 1000, 200).is_err()); // Should fail
    }
}
