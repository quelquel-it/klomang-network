/// Memory Management and Transaction Weight Accounting System
/// 
/// This module implements deterministic memory management for the transaction pool
/// with weight-based eviction. It ensures RAM usage stays within configurable bounds
/// through size-bounded mempool and cascade eviction strategies.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::collections::HashMap;
use parking_lot::RwLock;

use klomang_core::core::state::transaction::Transaction;

/// Configuration for mempool size limiting and memory accounting
#[derive(Clone, Debug)]
pub struct MempoolLimiterConfig {
    /// Maximum mempool size in bytes
    pub max_size_bytes: usize,
    
    /// Multiplier for computation overhead per input with complex scripts
    /// (e.g., 1.5 means 50% extra for complex inputs)
    pub computation_cost_multiplier: f64,
    
    /// Overhead per transaction entry in HashMap (pointer, metadata, etc.)
    pub transaction_entry_overhead_bytes: usize,
    
    /// Overhead per Arc<Transaction> clone
    pub arc_overhead_bytes: usize,
}

impl Default for MempoolLimiterConfig {
    fn default() -> Self {
        Self {
            max_size_bytes: 300_000_000, // 300 MB default
            computation_cost_multiplier: 1.0,
            transaction_entry_overhead_bytes: 256, // ~256 bytes per HashMap entry
            arc_overhead_bytes: 48, // Arc<T> overhead (~48 bytes)
        }
    }
}

/// Weight accounting for a single transaction
#[derive(Clone, Debug)]
pub struct TransactionWeight {
    /// Raw transaction serialization size (vsize)
    pub vsize: usize,
    
    /// Data structure overhead (HashMap entry + Arc)
    pub overhead: usize,
    
    /// Computation cost multiplier (for complex scripts)
    pub computation_cost: f64,
    
    /// Final calculated weight in bytes
    pub total_weight: usize,
}

/// Eviction candidate with priority score
#[derive(Clone, Debug)]
pub struct EvictionCandidate {
    /// Transaction hash
    pub tx_hash: Vec<u8>,
    
    /// Transaction weight in bytes
    pub weight: usize,
    
    /// Priority score (lower = higher priority for eviction)
    pub priority_score: f64,
    
    /// Number of dependent transactions (children)
    pub dependent_count: usize,
}

impl PartialEq for EvictionCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.tx_hash == other.tx_hash
    }
}

impl Eq for EvictionCandidate {}

impl Ord for EvictionCandidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Higher priority score => lower priority for eviction (keep in pool)
        // So: sort by descending score (lower score = higher eviction priority)
        let score_cmp = if self.priority_score > other.priority_score {
            std::cmp::Ordering::Less
        } else if self.priority_score < other.priority_score {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        };
        
        score_cmp.then_with(|| {
            // Tie-breaker: evict transactions with fewer dependents first
            // (to minimize cascade damage)
            self.dependent_count.cmp(&other.dependent_count)
        })
    }
}

impl PartialOrd for EvictionCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Eviction summary describing which transactions were removed and memory freed
#[derive(Debug, Clone)]
pub struct WeightEvictionResult {
    /// Hashes of transactions that were evicted
    pub evicted_hashes: Vec<Vec<u8>>,
    
    /// Total memory freed (bytes)
    pub freed_bytes: usize,
    
    /// Total weight freed including cascades
    pub freed_weight: usize,
    
    /// Number of cascade evictions (children removed due to parent removal)
    pub cascade_count: usize,
}

/// Thread-safe mempool limiter with deterministic eviction
pub struct MempoolLimiter {
    /// Configuration parameters
    config: Arc<RwLock<MempoolLimiterConfig>>,
    
    /// Current total weight of mempool (bytes, atomic for O(1) reads)
    current_weight: Arc<AtomicUsize>,
    
    /// Mapping of transaction hash -> weight
    tx_weights: Arc<RwLock<HashMap<Vec<u8>, TransactionWeight>>>,
    
    /// Mapping of parent tx_hash -> child tx_hashes (for cascade tracking)
    dependency_graph: Arc<RwLock<HashMap<Vec<u8>, Vec<Vec<u8>>>>>,
}

impl MempoolLimiter {
    /// Create new mempool limiter with given configuration
    pub fn new(config: MempoolLimiterConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            current_weight: Arc::new(AtomicUsize::new(0)),
            tx_weights: Arc::new(RwLock::new(HashMap::new())),
            dependency_graph: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Calculate weight for a transaction including overhead
    pub fn calculate_tx_weight(&self, tx: &Transaction) -> TransactionWeight {
        let config = self.config.read();
        
        // 1. Calculate vsize (virtual size / serialized size)
        let vsize = Self::calculate_vsize(tx);
        
        // 2. Calculate overhead
        let overhead = config.transaction_entry_overhead_bytes + config.arc_overhead_bytes;
        
        // 3. Calculate computation cost multiplier
        // For complex scripts: multiply by computation_cost_multiplier
        let mut computation_cost = 1.0;
        
        // Detect complex inputs (heuristic: > 100 bytes script or > 5 inputs)
        let has_complex_inputs = tx.inputs.len() > 5 || 
            tx.inputs.iter().any(|input| input.pubkey.len() > 100);
        
        // Detect complex execution (smart contract calls)
        let has_complex_execution = !tx.execution_payload.is_empty() && 
            tx.execution_payload.len() > 1000;
        
        if has_complex_inputs || has_complex_execution {
            computation_cost = config.computation_cost_multiplier;
        }
        
        // 4. Calculate total weight
        let base_weight = vsize + overhead;
        let total_weight = (base_weight as f64 * computation_cost).ceil() as usize;
        
        TransactionWeight {
            vsize,
            overhead,
            computation_cost,
            total_weight,
        }
    }

    /// Calculate vsize (virtual size) for a transaction
    /// This is similar to BTC concept: legacy tx size is all 4x'd
    fn calculate_vsize(tx: &Transaction) -> usize {
        // Simplified vsize calculation:
        // Base components
        let chain_id_size = 4;
        let locktime_size = 4;
        let gas_limit_size = 8;
        let max_fee_size = 16;
        let payload_size = tx.execution_payload.len();
        
        // Contract address (Option<Address>)
        let contract_addr_size = if tx.contract_address.is_some() { 32 } else { 1 };
        
        // Input calculations
        let mut inputs_size = 0;
        for input in &tx.inputs {
            // prev_tx hash: 32 bytes
            // index: 4 bytes
            // pubkey: variable, typically 64 bytes (compressed 33)
            // sighash_type: 1 byte
            inputs_size += 32 + 4 + input.pubkey.len() + 1;
        }
        
        // Output calculations
        let mut outputs_size = 0;
        for output in &tx.outputs {
            // value: 8 bytes
            // pubkey_hash: 32 bytes (typically)
            outputs_size += 8 + output.pubkey_hash.as_bytes().len();
        }
        
        // Transaction ID hash: 32 bytes
        let tx_id_size = 32;
        
        // Total size with basic overhead
        let total_size = chain_id_size + locktime_size + gas_limit_size + max_fee_size +
                        payload_size + contract_addr_size + inputs_size + outputs_size + tx_id_size;
        
        // Add minimum packet overhead (~40 bytes for network headers)
        total_size + 40
    }

    /// Add transaction weight to limiter tracking
    pub fn add_tx_weight(&self, tx_hash: Vec<u8>, weight: TransactionWeight) -> Result<(), String> {
        let mut weights = self.tx_weights.write();
        let total = weight.total_weight;
        
        weights.insert(tx_hash, weight);
        
        // Update atomic weight counter
        self.current_weight.fetch_add(total, Ordering::SeqCst);
        
        Ok(())
    }

    /// Remove transaction weight and return the amount freed
    pub fn remove_tx_weight(&self, tx_hash: &[u8]) -> usize {
        let mut weights = self.tx_weights.write();
        
        if let Some(weight) = weights.remove(tx_hash) {
            let freed = weight.total_weight;
            self.current_weight.fetch_sub(freed, Ordering::SeqCst);
            freed
        } else {
            0
        }
    }

    /// Register parent-child dependency for cascade tracking
    pub fn register_dependency(&self, parent_hash: Vec<u8>, child_hash: Vec<u8>) {
        let mut graph = self.dependency_graph.write();
        graph.entry(parent_hash)
            .or_insert_with(Vec::new)
            .push(child_hash);
    }

    /// Unregister parent-child dependency when child is removed
    pub fn unregister_dependency(&self, parent_hash: &[u8], child_hash: &[u8]) {
        let mut graph = self.dependency_graph.write();
        if let Some(children) = graph.get_mut(parent_hash) {
            children.retain(|h| h != child_hash);
            if children.is_empty() {
                graph.remove(parent_hash);
            }
        }
    }

    /// Check if adding a transaction would exceed size limit
    pub fn would_exceed_limit(&self, tx_weight: usize) -> bool {
        let config = self.config.read();
        let current = self.current_weight.load(Ordering::SeqCst);
        current + tx_weight > config.max_size_bytes
    }

    /// Get current mempool weight
    pub fn current_weight(&self) -> usize {
        self.current_weight.load(Ordering::SeqCst)
    }

    /// Get maximum allowed weight
    pub fn max_weight(&self) -> usize {
        self.config.read().max_size_bytes
    }

    /// Get percentage of mempool utilization
    pub fn utilization_percent(&self) -> f64 {
        let current = self.current_weight();
        let max = self.max_weight();
        if max == 0 {
            0.0
        } else {
            (current as f64 / max as f64) * 100.0
        }
    }

    /// Get all eviction candidates ordered by eviction priority
    /// Returns list of candidates ready to be evicted (lowest priority first)
    pub fn get_eviction_candidates(&self, priority_map: &HashMap<Vec<u8>, f64>) -> Vec<EvictionCandidate> {
        let weights = self.tx_weights.read();
        let graph = self.dependency_graph.read();
        
        let mut candidates: Vec<EvictionCandidate> = weights
            .iter()
            .map(|(tx_hash, weight)| {
                let priority_score = priority_map
                    .get(tx_hash)
                    .copied()
                    .unwrap_or(0.0);
                
                let dependent_count = graph
                    .get(tx_hash)
                    .map(|children| children.len())
                    .unwrap_or(0);
                
                EvictionCandidate {
                    tx_hash: tx_hash.clone(),
                    weight: weight.total_weight,
                    priority_score,
                    dependent_count,
                }
            })
            .collect();
        
        // Sort by eviction priority (lowest priority first)
        candidates.sort();
        candidates
    }

    /// Collect hashes of dependent transactions via DFS
    fn collect_dependents(&self, tx_hash: &[u8]) -> Vec<Vec<u8>> {
        let graph = self.dependency_graph.read();
        let mut dependents = Vec::new();
        let mut stack = vec![tx_hash.to_vec()];
        let mut visited = std::collections::HashSet::new();
        
        while let Some(current) = stack.pop() {
            if visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());
            
            if let Some(children) = graph.get(&current) {
                for child in children {
                    dependents.push(child.clone());
                    stack.push(child.clone());
                }
            }
        }
        
        dependents
    }

    /// Evict a single transaction and all its dependents (cascade)
    /// Returns the eviction result with freed memory
    pub fn evict_transaction_cascade(&self, tx_hash: &[u8]) -> WeightEvictionResult {
        let mut evicted_hashes = vec![tx_hash.to_vec()];
        let mut freed_weight = self.remove_tx_weight(tx_hash);
        
        // Collect and evict all dependents (cascade eviction)
        let dependents = self.collect_dependents(tx_hash);
        let cascade_count = dependents.len();
        
        for dependent_hash in dependents {
            let dependent_freed = self.remove_tx_weight(&dependent_hash);
            freed_weight += dependent_freed;
            evicted_hashes.push(dependent_hash);
        }
        
        // Clean up dependency graph entries
        let mut graph = self.dependency_graph.write();
        graph.remove(tx_hash);
        
        // Remove this tx from any parent's children list
        let parents: Vec<Vec<u8>> = graph
            .iter()
            .filter_map(|(parent, children)| {
                if children.iter().any(|c| c == tx_hash) {
                    Some(parent.clone())
                } else {
                    None
                }
            })
            .collect();
        
        drop(graph);
        
        for parent in parents {
            self.unregister_dependency(&parent, tx_hash);
        }
        
        WeightEvictionResult {
            freed_bytes: freed_weight,
            freed_weight,
            evicted_hashes,
            cascade_count,
        }
    }

    /// Perform space reclamation if needed to fit new transaction
    /// Returns list of evicted transaction hashes and freed bytes
    pub fn make_space_for(&self, required_weight: usize, priority_map: &HashMap<Vec<u8>, f64>) -> WeightEvictionResult {
        if !self.would_exceed_limit(required_weight) {
            return WeightEvictionResult {
                evicted_hashes: Vec::new(),
                freed_bytes: 0,
                freed_weight: 0,
                cascade_count: 0,
            };
        }
        
        let config = self.config.read();
        let max_size = config.max_size_bytes;
        let current = self.current_weight.load(Ordering::SeqCst);
        let threshold = max_size / 2; // Start evicting if > 50% utilization + new tx
        drop(config);
        
        let mut total_evicted_hashes = Vec::new();
        let mut total_freed_weight = 0;
        let mut total_cascade_count = 0;
        
        let candidates = self.get_eviction_candidates(priority_map);
        
        for candidate in candidates {
            if current + required_weight - total_freed_weight <= threshold {
                break;
            }
            
            let result = self.evict_transaction_cascade(&candidate.tx_hash);
            total_freed_weight += result.freed_weight;
            total_cascade_count += result.cascade_count;
            total_evicted_hashes.extend(result.evicted_hashes);
        }
        
        WeightEvictionResult {
            freed_bytes: total_freed_weight,
            freed_weight: total_freed_weight,
            evicted_hashes: total_evicted_hashes,
            cascade_count: total_cascade_count,
        }
    }

    /// Get statistics about memory usage
    pub fn get_stats(&self) -> MemoryStats {
        let current = self.current_weight();
        let max = self.max_weight();
        let weights = self.tx_weights.read();
        let tx_count = weights.len();
        
        let avg_weight = if tx_count > 0 {
            current / tx_count
        } else {
            0
        };
        
        MemoryStats {
            current_weight: current,
            max_weight: max,
            utilization_percent: (current as f64 / max as f64 * 100.0),
            transaction_count: tx_count,
            average_weight_per_tx: avg_weight,
        }
    }

    /// Clear all tracking (for testing or reset)
    pub fn clear(&self) {
        self.current_weight.store(0, Ordering::SeqCst);
        self.tx_weights.write().clear();
        self.dependency_graph.write().clear();
    }
}

/// Memory statistics
#[derive(Debug, Clone)]
pub struct MemoryStats {
    pub current_weight: usize,
    pub max_weight: usize,
    pub utilization_percent: f64,
    pub transaction_count: usize,
    pub average_weight_per_tx: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mempool_limiter_creation() {
        let limiter = MempoolLimiter::new(MempoolLimiterConfig::default());
        assert_eq!(limiter.current_weight(), 0);
        assert_eq!(limiter.utilization_percent(), 0.0);
    }

    #[test]
    fn test_calculate_vsize() {
        // Create a simple transaction for testing
        let tx = Transaction::new(vec![], vec![]);
        let vsize = MempoolLimiter::calculate_vsize(&tx);
        
        // Should have minimum overhead + basic fields
        assert!(vsize > 0);
        assert!(vsize < 1000); // Reasonable bounds for empty tx
    }

    #[test]
    fn test_add_remove_tx_weight() {
        let limiter = MempoolLimiter::new(MempoolLimiterConfig::default());
        let tx_hash = vec![1, 2, 3, 4];
        let weight = TransactionWeight {
            vsize: 100,
            overhead: 50,
            computation_cost: 1.0,
            total_weight: 150,
        };
        
        assert!(limiter.add_tx_weight(tx_hash.clone(), weight).is_ok());
        assert_eq!(limiter.current_weight(), 150);
        
        let freed = limiter.remove_tx_weight(&tx_hash);
        assert_eq!(freed, 150);
        assert_eq!(limiter.current_weight(), 0);
    }

    #[test]
    fn test_eviction_candidate_ordering() {
        let c1 = EvictionCandidate {
            tx_hash: vec![1],
            weight: 100,
            priority_score: 1.0,
            dependent_count: 0,
        };
        
        let c2 = EvictionCandidate {
            tx_hash: vec![2],
            weight: 100,
            priority_score: 5.0, // Higher priority (should be kept)
            dependent_count: 0,
        };
        
        // Lower priority score should come first (evict first)
        let mut candidates = vec![c2.clone(), c1.clone()];
        candidates.sort();
        
        assert_eq!(candidates[0].tx_hash, c1.tx_hash);
        assert_eq!(candidates[1].tx_hash, c2.tx_hash);
    }

    #[test]
    fn test_memory_stats() {
        let limiter = MempoolLimiter::new(MempoolLimiterConfig {
            max_size_bytes: 1000,
            ..Default::default()
        });
        
        let tx_hash = vec![1, 2, 3];
        let weight = TransactionWeight {
            vsize: 100,
            overhead: 50,
            computation_cost: 1.0,
            total_weight: 150,
        };
        
        let _ = limiter.add_tx_weight(tx_hash, weight);
        let stats = limiter.get_stats();
        
        assert_eq!(stats.current_weight, 150);
        assert_eq!(stats.max_weight, 1000);
        assert_eq!(stats.transaction_count, 1);
        assert!(stats.utilization_percent > 14.0 && stats.utilization_percent < 16.0);
    }
}
