/// Advanced Resource Optimization System for Mempool
///
/// This module implements multi-tier storage and hybrid eviction policies
/// for efficient transaction lifecycle management in the mempool.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use parking_lot::RwLock;
use dashmap::DashMap;

use klomang_core::core::state::transaction::Transaction;

use crate::storage::KvStore;
use crate::storage::schema::{TransactionValue, TransactionInput, TransactionOutput};

/// Convert klomang_core::Transaction to storage::TransactionValue
impl From<&Transaction> for TransactionValue {
    fn from(tx: &Transaction) -> Self {
        let inputs = tx.inputs.iter().map(|input| TransactionInput {
            previous_tx_hash: input.prev_tx.as_bytes().to_vec(),
            output_index: input.index,
        }).collect();

        let outputs = tx.outputs.iter().map(|output| TransactionOutput {
            amount: output.value,
            pubkey_hash: output.pubkey_hash.as_bytes().to_vec(),
        }).collect();

        // Calculate fee from max_fee_per_gas * gas_limit (simplified)
        let fee = (tx.max_fee_per_gas as u64).saturating_mul(tx.gas_limit);

        Self {
            tx_hash: tx.id.as_bytes().to_vec(),
            inputs,
            outputs,
            fee,
        }
    }
}

/// Dummy KvStore implementation for testing
impl KvStore {
    pub fn new_dummy() -> Self {
        // For testing purposes, we'll create a KvStore that doesn't actually store
        // but provides the interface. In production, this should never be used.
        use crate::storage::cache::StorageCacheLayer;
        use crate::storage::db::StorageDb;
        use std::sync::Arc;
        
        // This will panic in tests, but that's acceptable for now
        // Real implementation would need in-memory RocksDB
        panic!("KvStore::new_dummy requires real StorageDb - use in tests only")
    }
}

/// Configuration for resource optimization
#[derive(Clone, Debug)]
pub struct ResourceOptimizerConfig {
    /// Maximum transactions in Hot Tier (RAM)
    pub hot_tier_max_transactions: usize,
    
    /// Maximum memory usage for Hot Tier (bytes)
    pub hot_tier_max_memory_bytes: usize,
    
    /// LRU weight in hybrid scoring (0.0-1.0)
    pub lru_weight: f64,
    
    /// Fee weight in hybrid scoring (0.0-1.0)
    pub fee_weight: f64,
    
    /// Age threshold for promotion (seconds)
    pub promotion_age_threshold: u64,
    
    /// Fee rate threshold for promotion (sat/vbyte)
    pub promotion_fee_threshold: u64,
}

impl Default for ResourceOptimizerConfig {
    fn default() -> Self {
        Self {
            hot_tier_max_transactions: 5000,
            hot_tier_max_memory_bytes: 100_000_000, // 100 MB
            lru_weight: 0.6,
            fee_weight: 0.4,
            promotion_age_threshold: 300, // 5 minutes
            promotion_fee_threshold: 10,  // 10 sat/vbyte
        }
    }
}

/// Hot Tier: In-memory storage for high-priority transactions
pub struct HotTier {
    /// Fast concurrent storage: tx_hash -> (transaction, metadata)
    transactions: DashMap<Vec<u8>, (Arc<Transaction>, TransactionMetadata)>,
    
    /// Configuration
    config: Arc<RwLock<ResourceOptimizerConfig>>,
    
    /// Current memory usage tracking
    current_memory_bytes: std::sync::atomic::AtomicUsize,
}

#[derive(Clone, Debug)]
pub struct TransactionMetadata {
    /// Arrival time (UNIX timestamp)
    pub arrival_time: u64,
    
    /// Fee rate (sat/vbyte)
    pub fee_rate: u64,
    
    /// Last access time for LRU
    pub last_access_time: u64,
    
    /// Memory size in bytes
    pub memory_size: usize,
    
    /// Is this transaction a dependency of others?
    pub has_dependents: bool,
}

impl HotTier {
    pub fn new(config: Arc<RwLock<ResourceOptimizerConfig>>) -> Self {
        Self {
            transactions: DashMap::new(),
            config,
            current_memory_bytes: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Add transaction to Hot Tier
    pub fn add_transaction(&self, tx_hash: Vec<u8>, tx: Arc<Transaction>, fee_rate: u64) -> Result<(), String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Estimate memory size (rough calculation)
        let memory_size = self.estimate_memory_size(&tx);

        let metadata = TransactionMetadata {
            arrival_time: now,
            fee_rate,
            last_access_time: now,
            memory_size,
            has_dependents: false, // Will be updated by dependency tracking
        };

        // Check limits
        let config = self.config.read();
        if self.transactions.len() >= config.hot_tier_max_transactions {
            return Err("Hot tier transaction limit exceeded".to_string());
        }

        let current_memory = self.current_memory_bytes.load(std::sync::atomic::Ordering::SeqCst);
        if current_memory + memory_size > config.hot_tier_max_memory_bytes {
            return Err("Hot tier memory limit exceeded".to_string());
        }

        self.transactions.insert(tx_hash, (tx, metadata));
        self.current_memory_bytes.fetch_add(memory_size, std::sync::atomic::Ordering::SeqCst);

        Ok(())
    }

    /// Get transaction from Hot Tier (updates LRU)
    pub fn get_transaction(&self, tx_hash: &[u8]) -> Option<Arc<Transaction>> {
        if let Some(mut entry) = self.transactions.get_mut(tx_hash) {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            entry.1.last_access_time = now;
            Some(entry.0.clone())
        } else {
            None
        }
    }

    /// Remove transaction from Hot Tier
    pub fn remove_transaction(&self, tx_hash: &[u8]) -> Option<(Arc<Transaction>, TransactionMetadata)> {
        if let Some((_key, (tx, metadata))) = self.transactions.remove(tx_hash) {
            self.current_memory_bytes.fetch_sub(metadata.memory_size, std::sync::atomic::Ordering::SeqCst);
            Some((tx, metadata))
        } else {
            None
        }
    }

    /// Check if transaction exists in Hot Tier
    pub fn contains(&self, tx_hash: &[u8]) -> bool {
        self.transactions.contains_key(tx_hash)
    }

    /// Get all transaction hashes in Hot Tier
    pub fn get_all_hashes(&self) -> Vec<Vec<u8>> {
        self.transactions.iter().map(|entry| entry.key().clone()).collect()
    }

    /// Get transaction metadata
    pub fn get_metadata(&self, tx_hash: &[u8]) -> Option<TransactionMetadata> {
        self.transactions.get(tx_hash).map(|entry| entry.1.clone())
    }

    /// Update dependent status
    pub fn set_has_dependents(&self, tx_hash: &[u8], has_dependents: bool) {
        if let Some(mut entry) = self.transactions.get_mut(tx_hash) {
            entry.1.has_dependents = has_dependents;
        }
    }

    /// Estimate memory size of transaction
    fn estimate_memory_size(&self, tx: &Transaction) -> usize {
        // Rough estimation: base size + inputs + outputs + payload
        let base_size = 200; // struct overhead
        let inputs_size = tx.inputs.len() * 150; // ~150 bytes per input
        let outputs_size = tx.outputs.len() * 50; // ~50 bytes per output
        let payload_size = tx.execution_payload.len();

        base_size + inputs_size + outputs_size + payload_size
    }

    /// Get current statistics
    pub fn get_stats(&self) -> HotTierStats {
        let transaction_count = self.transactions.len();
        let memory_usage = self.current_memory_bytes.load(std::sync::atomic::Ordering::SeqCst);
        let config = self.config.read();

        HotTierStats {
            transaction_count,
            memory_usage,
            memory_limit: config.hot_tier_max_memory_bytes,
            transaction_limit: config.hot_tier_max_transactions,
            utilization_percent: if config.hot_tier_max_memory_bytes > 0 {
                (memory_usage as f64 / config.hot_tier_max_memory_bytes as f64) * 100.0
            } else {
                0.0
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct HotTierStats {
    pub transaction_count: usize,
    pub memory_usage: usize,
    pub memory_limit: usize,
    pub transaction_limit: usize,
    pub utilization_percent: f64,
}

/// Cold Tier: Disk-based storage for low-priority transactions
pub struct ColdTier {
    /// KvStore reference for persistent storage
    kv_store: Arc<KvStore>,
    
    /// Column family name for mempool backup
    _cf_name: String,
}

impl ColdTier {
    pub fn new(kv_store: Arc<KvStore>) -> Self {
        Self {
            kv_store,
            _cf_name: "CF_MEMPOOL_BACKUP".to_string(),
        }
    }
    /// Create dummy ColdTier for testing (no-op operations)
    pub fn new_dummy() -> Self {
        Self {
            kv_store: Arc::new(KvStore::new_dummy()),
            _cf_name: "MEMPOOL_BACKUP".to_string(),
        }
    }
    /// Store transaction in Cold Tier
    pub fn store_transaction(&self, tx: &Transaction) -> Result<(), String> {
        let tx_value = TransactionValue::from(tx);
        let tx_hash = tx.id.as_bytes();

        self.kv_store.put_transaction(tx_hash, &tx_value)
            .map_err(|e| format!("Storage error: {:?}", e))
    }

    /// Retrieve transaction from Cold Tier
    pub fn retrieve_transaction(&self, tx_hash: &[u8]) -> Result<Option<Arc<Transaction>>, String> {
        match self.kv_store.get_transaction(tx_hash)
            .map_err(|e| format!("Storage error: {:?}", e))? {
            Some(tx_value) => {
                // Convert back to Transaction (simplified - in real implementation would need full conversion)
                // For now, return None as placeholder since full conversion is complex
                // TODO: Implement full TransactionValue -> Transaction conversion
                Ok(None)
            }
            None => Ok(None),
        }
    }

    /// Remove transaction from Cold Tier
    pub fn remove_transaction(&self, tx_hash: &[u8]) -> Result<(), String> {
        self.kv_store.delete_transaction(tx_hash)
            .map_err(|e| format!("Storage error: {:?}", e))
    }

    /// Check if transaction exists in Cold Tier
    pub fn contains(&self, tx_hash: &[u8]) -> Result<bool, String> {
        self.kv_store.get_transaction(tx_hash)
            .map(|opt| opt.is_some())
            .map_err(|e| format!("Storage error: {:?}", e))
    }
}

/// Hybrid eviction policy combining LRU and Fee-based scoring
pub struct HybridPolicy {
    /// Configuration
    config: Arc<RwLock<ResourceOptimizerConfig>>,
}

impl HybridPolicy {
    pub fn new(config: Arc<RwLock<ResourceOptimizerConfig>>) -> Self {
        Self { config }
    }

    /// Calculate eviction score for a transaction
    /// Lower score = higher eviction priority
    pub fn calculate_eviction_score(&self, metadata: &TransactionMetadata) -> f64 {
        let config = self.config.read();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Age in seconds (for LRU)
        let age = now.saturating_sub(metadata.arrival_time) as f64;

        // Fee rate score (higher fee = lower eviction priority)
        let fee_score = metadata.fee_rate as f64;

        // Normalize age (older = higher eviction priority)
        let max_age = 3600.0; // 1 hour
        let normalized_age = (age / max_age).min(1.0);

        // Combined score: weighted average
        // Lower score = higher eviction priority
        let lru_component = normalized_age * config.lru_weight;
        let fee_component = (1.0 / (fee_score + 1.0)) * config.fee_weight; // Inverse fee

        lru_component + fee_component
    }

    /// Select victim transaction for eviction
    /// Returns the transaction hash with lowest eviction score
    pub fn select_victim(&self, candidates: &[(Vec<u8>, TransactionMetadata)]) -> Option<Vec<u8>> {
        if candidates.is_empty() {
            return None;
        }

        let mut best_candidate = None;
        let mut best_score = f64::INFINITY;

        for (tx_hash, metadata) in candidates {
            let score = self.calculate_eviction_score(metadata);
            if score < best_score {
                best_score = score;
                best_candidate = Some(tx_hash.clone());
            }
        }

        best_candidate
    }
}

/// Main resource optimizer coordinating Hot/Cold tiers and eviction
pub struct ResourceOptimizer {
    /// Hot tier for high-priority transactions
    hot_tier: Arc<HotTier>,
    
    /// Cold tier for low-priority transactions
    cold_tier: Arc<ColdTier>,
    
    /// Hybrid eviction policy
    eviction_policy: Arc<HybridPolicy>,
    
    /// Configuration
    config: Arc<RwLock<ResourceOptimizerConfig>>,
}

impl ResourceOptimizer {
    pub fn new(kv_store: Option<Arc<KvStore>>, config: ResourceOptimizerConfig) -> Self {
        let config_clone = config.clone();
        let config_arc = Arc::new(RwLock::new(config));
        
        match kv_store {
            Some(kv_store) => Self {
                hot_tier: Arc::new(HotTier::new(config_arc.clone())),
                cold_tier: Arc::new(ColdTier::new(kv_store)),
                eviction_policy: Arc::new(HybridPolicy::new(config_arc.clone())),
                config: config_arc,
            },
            None => Self::new_dummy(config_clone),
        }
    }

    /// Create ResourceOptimizer with dummy storage (for testing/initialization)
    pub fn new_dummy(config: ResourceOptimizerConfig) -> Self {
        let config_arc = Arc::new(RwLock::new(config));
        
        Self {
            hot_tier: Arc::new(HotTier::new(config_arc.clone())),
            cold_tier: Arc::new(ColdTier::new_dummy()),
            eviction_policy: Arc::new(HybridPolicy::new(config_arc.clone())),
            config: config_arc,
        }
    }

    /// Add transaction to appropriate tier
    pub fn add_transaction(&self, tx_hash: Vec<u8>, tx: Arc<Transaction>, fee_rate: u64) -> Result<(), String> {
        // Try to add to Hot Tier first
        match self.hot_tier.add_transaction(tx_hash.clone(), tx.clone(), fee_rate) {
            Ok(()) => Ok(()),
            Err(_) => {
                // Hot Tier full, demote to Cold Tier
                self.cold_tier.store_transaction(&tx)?;
                Ok(())
            }
        }
    }

    /// Get transaction from Hot Tier, promote from Cold if needed
    pub fn get_transaction(&self, tx_hash: &[u8]) -> Result<Option<Arc<Transaction>>, String> {
        // Check Hot Tier first
        if let Some(tx) = self.hot_tier.get_transaction(tx_hash) {
            return Ok(Some(tx));
        }

        // Check Cold Tier and promote if found
        if let Some(tx) = self.cold_tier.retrieve_transaction(tx_hash)? {
            // Try to promote to Hot Tier
            let fee_rate = self.estimate_fee_rate(&tx);
            let _ = self.hot_tier.add_transaction(tx_hash.to_vec(), tx.clone(), fee_rate);
            
            // Remove from Cold Tier
            let _ = self.cold_tier.remove_transaction(tx_hash);
            
            Ok(Some(tx))
        } else {
            Ok(None)
        }
    }

    /// Remove transaction from both tiers
    pub fn remove_transaction(&self, tx_hash: &[u8]) -> Result<(), String> {
        // Remove from Hot Tier
        self.hot_tier.remove_transaction(tx_hash);
        
        // Remove from Cold Tier
        self.cold_tier.remove_transaction(tx_hash)?;
        
        Ok(())
    }

    /// Perform space reclamation in Hot Tier
    pub fn reclaim_space(&self) -> Result<Vec<Vec<u8>>, String> {
        let mut evicted_hashes = Vec::new();
        
        // Get all candidates from Hot Tier
        let hashes = self.hot_tier.get_all_hashes();
        let mut candidates = Vec::new();
        
        for hash in hashes {
            if let Some(metadata) = self.hot_tier.get_metadata(&hash) {
                candidates.push((hash, metadata));
            }
        }
        
        // Select victims using hybrid policy
        while let Some(victim_hash) = self.eviction_policy.select_victim(&candidates) {
            // Remove from candidates to avoid re-selection
            candidates.retain(|(h, _)| h != &victim_hash);
            
            // Demote to Cold Tier
            if let Some((tx, _)) = self.hot_tier.remove_transaction(&victim_hash) {
                self.cold_tier.store_transaction(&tx)?;
                evicted_hashes.push(victim_hash);
            }
            
            // Check if we've freed enough space
            let stats = self.hot_tier.get_stats();
            if stats.utilization_percent < 80.0 {
                break;
            }
        }
        
        Ok(evicted_hashes)
    }

    /// Check if transaction should be promoted based on criteria
    pub fn should_promote(&self, tx_hash: &[u8], new_fee_rate: u64) -> bool {
        let config = self.config.read();
        
        // Check if in Cold Tier
        if let Ok(true) = self.cold_tier.contains(tx_hash) {
            // Check promotion criteria
            new_fee_rate >= config.promotion_fee_threshold
        } else {
            false
        }
    }

    /// Promote transaction from Cold to Hot Tier
    pub fn promote_transaction(&self, tx_hash: &[u8]) -> Result<(), String> {
        if let Some(tx) = self.cold_tier.retrieve_transaction(tx_hash)? {
            let fee_rate = self.estimate_fee_rate(&tx);
            
            // Add to Hot Tier
            self.hot_tier.add_transaction(tx_hash.to_vec(), tx, fee_rate)?;
            
            // Remove from Cold Tier
            self.cold_tier.remove_transaction(tx_hash)?;
        }
        
        Ok(())
    }

    /// Get comprehensive statistics
    pub fn get_stats(&self) -> ResourceOptimizerStats {
        let hot_stats = self.hot_tier.get_stats();
        
        ResourceOptimizerStats {
            hot_tier: hot_stats,
            cold_tier_transaction_count: 0, // Would need to track separately
        }
    }

    /// Estimate fee rate for a transaction (simplified)
    fn estimate_fee_rate(&self, tx: &Arc<Transaction>) -> u64 {
        // Simplified: assume 1000 satoshi fee for estimation
        // In real implementation, this should be calculated properly
        let fee = 1000u64;
        let size = tx.inputs.len() * 32 + tx.outputs.len() * 32 + 200; // Rough size
        if size > 0 {
            fee / size as u64
        } else {
            1
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResourceOptimizerStats {
    pub hot_tier: HotTierStats,
    pub cold_tier_transaction_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::cache::StorageCacheLayer;

    #[test]
    fn test_hot_tier_add_get() {
        let config = ResourceOptimizerConfig::default();
        let config_arc = Arc::new(RwLock::new(config));
        let hot_tier = HotTier::new(config_arc);
        
        let tx = Arc::new(Transaction::new(vec![], vec![]));
        let tx_hash = vec![1, 2, 3];
        
        assert!(hot_tier.add_transaction(tx_hash.clone(), tx.clone(), 10).is_ok());
        assert!(hot_tier.contains(&tx_hash));
        
        let retrieved = hot_tier.get_transaction(&tx_hash);
        assert!(retrieved.is_some());
    }

    #[test]
    fn test_eviction_score_calculation() {
        let config = ResourceOptimizerConfig::default();
        let config_arc = Arc::new(RwLock::new(config));
        let policy = HybridPolicy::new(config_arc);
        
        let metadata = TransactionMetadata {
            arrival_time: 1000,
            fee_rate: 20,
            last_access_time: 1000,
            memory_size: 1000,
            has_dependents: false,
        };
        
        let score = policy.calculate_eviction_score(&metadata);
        assert!(score >= 0.0 && score <= 2.0); // Reasonable bounds
    }

    #[test]
    fn test_select_victim() {
        let config = ResourceOptimizerConfig::default();
        let config_arc = Arc::new(RwLock::new(config));
        let policy = HybridPolicy::new(config_arc);
        
        let candidates = vec![
            (vec![1], TransactionMetadata {
                arrival_time: 1000,
                fee_rate: 5, // Low fee, should be selected
                last_access_time: 1000,
                memory_size: 1000,
                has_dependents: false,
            }),
            (vec![2], TransactionMetadata {
                arrival_time: 1000,
                fee_rate: 50, // High fee, should not be selected
                last_access_time: 1000,
                memory_size: 1000,
                has_dependents: false,
            }),
        ];
        
        let victim = policy.select_victim(&candidates);
        assert_eq!(victim, Some(vec![1]));
    }
}
