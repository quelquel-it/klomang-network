//! Transaction Pool Core - State machine for managing transaction lifecycle
//! 
//! This module implements a multi-indexed transaction pool with deterministic
//! transaction selection for block building, incremental revalidation on block receipt,
//! deterministic eviction under memory pressure, and UTXO conflict management with
//! Replace-By-Fee support.

pub mod status;
pub mod pool;
pub mod validation;
pub mod selection;
pub mod revalidation;
pub mod eviction;
pub mod conflict;
pub mod ownership;
pub mod advanced_conflicts;
pub mod dependency_graph;
pub mod advanced_transaction_manager;
pub mod conflict_graph;
pub mod rbf_manager;
pub mod conflict_rbf_integration;
pub mod conflict_rbf_tests;
pub mod advanced_dependency_manager;
pub mod cascade_coordinator;
pub mod enhanced_validator;
pub mod dependency_ordering_engine;
pub mod dependency_eviction_system;
pub mod storage_integration;
pub mod recursive_dependency_tracker;
pub mod recursive_dependency_manager;
pub mod priority_pool;
pub mod ordering_engine;
pub mod priority_scheduler;
pub mod multi_dimensional_index;
pub mod priority_ordering;
pub mod deterministic_ordering;
pub mod parallel_partitioning;
pub mod parallel_mempool;
pub mod parallel_validator;
pub mod parallel_transaction_index;
pub mod lock_free_snapshot;
pub mod parallel_sync_storage;
pub mod orphan_manager;
pub mod advanced_orphan_management;
pub mod background_batch_processor;
pub mod memory_limiter;
pub mod resource_optimizer;
pub mod admission_controller;
pub mod graph_conflict_ordering;
pub mod graph_conflict_ordering_integration;
pub mod parallel_selection;

pub use status::{TransactionStatus, TransactionStatusError};
pub use pool::{TransactionPool, PoolConfig, PoolEntry, PoolStats, FeeFilter};
pub use validation::PoolValidator;
pub use selection::{SelectionCriteria, SelectionStrategy, DeterministicSelector};
pub use revalidation::{RevalidationEngine, RevalidationStats, ConflictInfo, RevalidationResult};
pub use eviction::{EvictionEngine, EvictionPolicy, EvictionResult, EvictionScore, MempoolPressure};
pub use conflict::{UtxoTracker, UtxoConflictError, OutPoint, UtxoLock, ConflictStats};
pub use ownership::{UtxoOwnershipManager, OwnershipError, TransactionAddedInfo, TransactionRemovedInfo, ConflictAnalysis};
pub use advanced_conflicts::{ConflictMap, TxHash, ConflictType, ResolutionResult, ResolutionReason, OutPoint as AdvancedOutPoint};
pub use dependency_graph::{DependencyGraph, TransactionDependency, ConflictPartition, DependencyGraphStats};
pub use advanced_transaction_manager::{AdvancedTransactionManager, ManagerError, TransactionAdditionResult, ConflictAnalysis as AdvancedConflictAnalysis, ConflictStatus};
pub use conflict_graph::{ConflictGraph, ConflictGraphStats, ConflictNode};
pub use rbf_manager::{RBFManager, RBFChoice, RBFReason, RBFEvaluation};
pub use conflict_rbf_integration::{ConflictRBFManager, AddTransactionResult, ConflictAnalysis as IntegrationConflictAnalysis};
pub use advanced_dependency_manager::{TxDependencyManager, DependencyLevel, DependencyChain, DependencyStats};
pub use cascade_coordinator::{CascadeValidationCoordinator, CascadeEvent, CascadeStats, CascadeValidationResult, CascadeEventHandler};
pub use enhanced_validator::{EnhancedPoolValidator, EnhancedValidationResult};
pub use dependency_ordering_engine::{DependencyOrderingEngine, TopologicalResult, TopologyStats, AdjacencyList, InDegreeMap};
pub use dependency_eviction_system::{DependencyEvictionSystem, EvictionReason, EvictionRecord, EvictionStats, CascadeEvictionResult};
pub use storage_integration::{StorageIntegration, ParentClassification, ParentVerification, StorageIntegrationStats};
pub use recursive_dependency_tracker::{
    RecursiveDependencyTracker, TxHash as RecursiveTxHash, DependencyResolutionStatus, AncestryValidation,
    RecursiveDependencyStats, CascadeInvalidationResult,
};
pub use recursive_dependency_manager::{
    RecursiveDependencyManager, RecursiviveDependencyConfig, TransactionWithStatus,
    BulkResolutionResult,
};
pub use priority_pool::{
    PriorityPool, TransactionPriority, PriorityPoolStats,
};
pub use ordering_engine::{
    OrderingEngine, OrderingEngineConfig, OrderedTransaction, OrderingStats,
};
pub use priority_scheduler::{
    PriorityScheduler, PrioritySchedulerConfig, DynamicPriority, PrioritySchedulerBuilder,
};
pub use multi_dimensional_index::{
    MultiDimensionalIndex, IndexedTransaction, MultiDimensionalIndexStats,
};
pub use priority_ordering::{
    PriorityBuckets, PrioritizedTransaction, PriorityOrderingConfig,
    CrossBucketIterator, BucketStatistics, BucketInfo,
};
pub use deterministic_ordering::{
    DeterministicOrderingEngine, OrderingValidation,
};
pub use parallel_partitioning::{
    ConflictFreePartitioner, PartitionConfig, PartitionResult, PartitionStats,
};
pub use parallel_mempool::{
    ParallelMempool, SubPool, SubPoolEntry, ParallelAddResult, ShardStats,
};
pub use parallel_validator::{
    ParallelValidator, ParallelValidatorConfig, ValidationTask, ValidationResult, ValidationStats,
};
pub use parallel_transaction_index::{
    ParallelTransactionIndex, ParallelIndexConfig, IndexedTransactionEntry, IndexedTransactionStatus, IndexStats,
};
pub use lock_free_snapshot::{
    LockFreeReadLayer, MempoolSnapshot, LockFreeReadConfig,
};
pub use parallel_sync_storage::{
    StorageSyncManager, StorageSyncConfig, StorageSyncResult, SyncStatus, SyncMetadata, StatusReport,
};
pub use orphan_manager::{
    OrphanManager, OrphanPoolConfig, OrphanEntry, OrphanStats, AdoptionResult, OrphanEvictionPolicy,
};
pub use advanced_orphan_management::{
    DeferredResolver, ResolutionTask, ResolutionStats,
    RecursiveOrphanLinker, OrphanChainLink, LinkerStats,
    ChainResolutionResult, ChainIntegrityReport,
};
pub use background_batch_processor::{
    BackgroundBatchProcessor, BackgroundProcessorStats,
};
pub use memory_limiter::{
    MempoolLimiter, MempoolLimiterConfig, TransactionWeight,
    EvictionCandidate, WeightEvictionResult, MemoryStats,
};
pub use resource_optimizer::{
    ResourceOptimizer, ResourceOptimizerConfig, HotTier, ColdTier,
    HybridPolicy, TransactionMetadata, HotTierStats, ResourceOptimizerStats,
};
pub use parallel_selection::{
    ParallelSelectionBuilder, FeeBalancer,
};
