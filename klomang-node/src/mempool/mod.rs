//! Transaction Pool Core - State machine for managing transaction lifecycle
//!
//! This module implements a multi-indexed transaction pool with deterministic
//! transaction selection for block building, incremental revalidation on block receipt,
//! deterministic eviction under memory pressure, and UTXO conflict management with
//! Replace-By-Fee support.

pub mod admission_controller;
pub mod advanced_conflicts;
pub mod advanced_dependency_manager;
pub mod advanced_orphan_management;
pub mod advanced_transaction_manager;
pub mod background_batch_processor;
pub mod cascade_coordinator;
pub mod conflict;
pub mod conflict_graph;
pub mod conflict_rbf_integration;
pub mod conflict_rbf_tests;
pub mod dependency_eviction_system;
pub mod dependency_graph;
pub mod dependency_ordering_engine;
pub mod deterministic_ordering;
pub mod enhanced_validator;
pub mod eviction;
pub mod graph_conflict_ordering;
pub mod graph_conflict_ordering_integration;
pub mod lock_free_snapshot;
pub mod memory_limiter;
pub mod multi_dimensional_index;
pub mod multi_queue;
pub mod ordering_engine;
pub mod orphan_manager;
pub mod ownership;
pub mod parallel_mempool;
pub mod parallel_partitioning;
pub mod parallel_selection;
pub mod parallel_sync_storage;
pub mod parallel_transaction_index;
pub mod parallel_validator;
pub mod pool;
pub mod priority_ordering;
pub mod priority_pool;
pub mod priority_scheduler;
pub mod rbf_manager;
pub mod recursive_dependency_manager;
pub mod recursive_dependency_tracker;
pub mod resource_optimizer;
pub mod revalidation;
pub mod selection;
pub mod set_packer;
pub mod status;
pub mod storage_integration;
pub mod validation;

pub use advanced_conflicts::{
    ConflictMap, ConflictType, OutPoint as AdvancedOutPoint, ResolutionReason, ResolutionResult,
    TxHash,
};
pub use advanced_dependency_manager::{
    DependencyChain, DependencyLevel, DependencyStats, TxDependencyManager,
};
pub use advanced_orphan_management::{
    ChainIntegrityReport, ChainResolutionResult, DeferredResolver, LinkerStats, OrphanChainLink,
    RecursiveOrphanLinker, ResolutionStats, ResolutionTask,
};
pub use advanced_transaction_manager::{
    AdvancedTransactionManager, ConflictAnalysis as AdvancedConflictAnalysis, ConflictStatus,
    ManagerError, TransactionAdditionResult,
};
pub use background_batch_processor::{BackgroundBatchProcessor, BackgroundProcessorStats};
pub use cascade_coordinator::{
    CascadeEvent, CascadeEventHandler, CascadeStats, CascadeValidationCoordinator,
    CascadeValidationResult,
};
pub use conflict::{ConflictStats, OutPoint, UtxoConflictError, UtxoLock, UtxoTracker};
pub use conflict_graph::{ConflictGraph, ConflictGraphStats, ConflictNode};
pub use conflict_rbf_integration::{
    AddTransactionResult, ConflictAnalysis as IntegrationConflictAnalysis, ConflictRBFManager,
};
pub use dependency_eviction_system::{
    CascadeEvictionResult, DependencyEvictionSystem, EvictionReason, EvictionRecord, EvictionStats,
};
pub use dependency_graph::{
    ConflictPartition, DependencyGraph, DependencyGraphStats, TransactionDependency,
};
pub use dependency_ordering_engine::{
    AdjacencyList, DependencyOrderingEngine, InDegreeMap, TopologicalResult, TopologyStats,
};
pub use deterministic_ordering::{DeterministicOrderingEngine, OrderingValidation};
pub use enhanced_validator::{EnhancedPoolValidator, EnhancedValidationResult};
pub use eviction::{
    AgingProcessor, EvictionEngine, EvictionPolicy, EvictionPredictor, EvictionResult,
    EvictionScore, MempoolPressure,
};
pub use lock_free_snapshot::{LockFreeReadConfig, LockFreeReadLayer, MempoolSnapshot};
pub use memory_limiter::{
    EvictionCandidate, MemoryStats, MempoolLimiter, MempoolLimiterConfig, TransactionWeight,
    WeightEvictionResult,
};
pub use multi_dimensional_index::{
    IndexedTransaction, MultiDimensionalIndex, MultiDimensionalIndexStats,
};
pub use multi_queue::{MultiQueueAdmissionSystem, QueueStats};
pub use ordering_engine::{
    OrderedTransaction, OrderingEngine, OrderingEngineConfig, OrderingStats,
};
pub use orphan_manager::{
    AdoptionResult, OrphanEntry, OrphanEvictionPolicy, OrphanManager, OrphanPoolConfig, OrphanStats,
};
pub use ownership::{
    ConflictAnalysis, OwnershipError, TransactionAddedInfo, TransactionRemovedInfo,
    UtxoOwnershipManager,
};
pub use parallel_mempool::{ParallelAddResult, ParallelMempool, ShardStats, SubPool, SubPoolEntry};
pub use parallel_partitioning::{
    ConflictFreePartitioner, PartitionConfig, PartitionResult, PartitionStats,
};
pub use parallel_selection::{FeeBalancer, ParallelSelectionBuilder};
pub use parallel_sync_storage::{
    StatusReport, StorageSyncConfig, StorageSyncManager, StorageSyncResult, SyncMetadata,
    SyncStatus,
};
pub use parallel_transaction_index::{
    IndexStats, IndexedTransactionEntry, IndexedTransactionStatus, ParallelIndexConfig,
    ParallelTransactionIndex,
};
pub use parallel_validator::{
    ParallelValidator, ParallelValidatorConfig, ValidationResult, ValidationStats, ValidationTask,
};
pub use pool::{FeeFilter, PoolConfig, PoolEntry, PoolStats, TransactionPool};
pub use priority_ordering::{
    BucketInfo, BucketStatistics, CrossBucketIterator, PrioritizedTransaction, PriorityBuckets,
    PriorityOrderingConfig,
};
pub use priority_pool::{PriorityPool, PriorityPoolStats, TransactionPriority};
pub use priority_scheduler::{
    DynamicPriority, PriorityScheduler, PrioritySchedulerBuilder, PrioritySchedulerConfig,
};
pub use rbf_manager::{RBFChoice, RBFEvaluation, RBFManager, RBFReason};
pub use recursive_dependency_manager::{
    BulkResolutionResult, RecursiveDependencyManager, RecursiviveDependencyConfig,
    TransactionWithStatus,
};
pub use recursive_dependency_tracker::{
    AncestryValidation, CascadeInvalidationResult, DependencyResolutionStatus,
    RecursiveDependencyStats, RecursiveDependencyTracker, TxHash as RecursiveTxHash,
};
pub use resource_optimizer::{
    ColdTier, HotTier, HotTierStats, HybridPolicy, ResourceOptimizer, ResourceOptimizerConfig,
    ResourceOptimizerStats, TransactionMetadata,
};
pub use revalidation::{ConflictInfo, RevalidationEngine, RevalidationResult, RevalidationStats};
pub use selection::{DeterministicSelector, SelectionCriteria, SelectionStrategy};
pub use set_packer::{SetPacker, SovereignSet};
pub use status::{TransactionStatus, TransactionStatusError};
pub use storage_integration::{
    ParentClassification, ParentVerification, StorageIntegration, StorageIntegrationStats,
};
pub use validation::PoolValidator;
