//! Integration layer between GraphConflictOrderingEngine and TransactionPool
//!
//! This module provides integration utilities for:
//! - Syncing conflict graph with mempool state
//! - Building canonical blocks with deterministic ordering
//! - Validating UTXO state against on-chain data
//! - Coordinating with dependency manager for cascade validation

use std::sync::Arc;

use klomang_core::core::crypto::Hash;
use klomang_core::core::state::transaction::Transaction;

use super::graph_conflict_ordering::{CanonicalOrderingResult, GraphConflictOrderingEngine};
use crate::storage::kv_store::KvStore;

/// Configuration for conflict & ordering integration
#[derive(Clone, Debug)]
pub struct ConflictOrderingIntegrationConfig {
    /// Enable UTXO state validation from on-chain storage
    pub validate_utxo_state: bool,

    /// Enable cascade validation when removing transactions
    pub enable_cascade_validation: bool,

    /// Maximum transactions to process in one ordering computation
    pub max_ordering_batch: usize,

    /// Fee weight for priority scoring (0.0-1.0)
    pub fee_weight: f64,

    /// Age weight for priority scoring (0.0-1.0)
    pub age_weight: f64,
}

impl Default for ConflictOrderingIntegrationConfig {
    fn default() -> Self {
        Self {
            validate_utxo_state: true,
            enable_cascade_validation: true,
            max_ordering_batch: 5000,
            fee_weight: 0.7,
            age_weight: 0.3,
        }
    }
}

/// Validation result for UTXO state
#[derive(Clone, Debug)]
pub struct UtxoValidationResult {
    pub is_valid: bool,
    pub unspent_inputs: usize,
    pub conflicting_outputs: usize,
    pub validation_errors: Vec<String>,
}

/// Block building result with canonical ordering
#[derive(Clone, Debug)]
pub struct BlockBuildingResult {
    /// Ordered transaction hashes for block
    pub transactions: Vec<Vec<u8>>,

    /// Topological layers for parallel block validation
    pub parallel_layers: Vec<Vec<Vec<u8>>>,

    /// Total fees
    pub total_fees: u64,

    /// Block weight estimate
    pub total_weight: usize,

    /// Transactions excluded due to conflicts
    pub excluded_transactions: Vec<Vec<u8>>,
}

/// Integration manager for conflict graph and ordering
pub struct ConflictOrderingIntegration {
    engine: GraphConflictOrderingEngine,
    config: ConflictOrderingIntegrationConfig,
    kv_store: Option<Arc<KvStore>>,
}

impl ConflictOrderingIntegration {
    /// Create new integration manager
    pub fn new(config: ConflictOrderingIntegrationConfig, kv_store: Option<Arc<KvStore>>) -> Self {
        let mut engine = GraphConflictOrderingEngine::new(kv_store.clone());
        engine.set_priority_weights(config.fee_weight, config.age_weight);

        Self {
            engine,
            config,
            kv_store,
        }
    }

    /// Register transaction and detect conflicts
    pub fn register_transaction(
        &self,
        tx: &Transaction,
        tx_hash: Vec<u8>,
        fee: u64,
        arrival_time_ms: u64,
    ) -> Result<ConflictDetectionResult, String> {
        let conflicts =
            self.engine
                .register_transaction(tx, tx_hash.clone(), fee, arrival_time_ms)?;

        let has_double_spend = self.engine.detect_double_spend(&tx_hash)?;

        Ok(ConflictDetectionResult {
            tx_hash,
            detected_conflicts: conflicts,
            has_double_spend,
            is_valid: !has_double_spend,
        })
    }

    /// Validate transaction UTXO state against on-chain data
    pub fn validate_utxo_state(&self, tx: &Transaction) -> Result<UtxoValidationResult, String> {
        if !self.config.validate_utxo_state {
            return Ok(UtxoValidationResult {
                is_valid: true,
                unspent_inputs: tx.inputs.len(),
                conflicting_outputs: 0,
                validation_errors: Vec::new(),
            });
        }

        let mut validation_errors = Vec::new();
        let unspent_inputs = tx.inputs.len();
        let conflicting_outputs = 0;

        // Validate against on-chain state if kv_store is available
        if let Some(kv_store) = &self.kv_store {
            for input in &tx.inputs {
                // Check if input exists in UTXO set
                // This would normally query the kv_store for UTXO state
                // For now, we mark it as a placeholder validation point

                if let Err(e) = validate_input_existence(kv_store, &input.prev_tx) {
                    validation_errors.push(format!("UTXO validation error: {}", e));
                }
            }
        }

        Ok(UtxoValidationResult {
            is_valid: validation_errors.is_empty(),
            unspent_inputs,
            conflicting_outputs,
            validation_errors,
        })
    }

    /// Build block with canonical ordering
    ///
    /// This function constructs a block by:
    /// 1. Computing canonical order of all transactions
    /// 2. Excluding conflicting transactions (keeping highest-fee variant)
    /// 3. Respecting topological ordering for dependencies
    /// 4. Returning parallel layers for block validation
    pub fn build_block_canonical(
        &self,
        max_block_weight: usize,
    ) -> Result<BlockBuildingResult, String> {
        let ordering = self.engine.compute_canonical_order()?;

        let mut transactions = Vec::new();
        let mut total_fees = 0u64;
        let mut total_weight = 0usize;
        let mut excluded = Vec::new();

        // Process transactions in canonical order, skipping conflicts
        for tx_hash in ordering.ordered_hashes {
            if total_weight >= max_block_weight {
                break;
            }

            // Check if this transaction conflicts with already-selected ones
            let conflicts = self.engine.get_conflicts(&tx_hash);
            let has_conflict = conflicts.iter().any(|c| transactions.contains(c));

            if has_conflict {
                excluded.push(tx_hash);
                continue;
            }

            if let Some(node) = self.engine.get_node(&tx_hash) {
                let tx_weight = node.size_bytes;
                if total_weight + tx_weight <= max_block_weight {
                    transactions.push(tx_hash.clone());
                    total_fees += node.fee;
                    total_weight += tx_weight;
                }
            }
        }

        // Build parallel layers based on canonical ordering
        // Filter to include only layers that have transactions in the final block
        let parallel_layers = ordering
            .parallel_groups
            .into_iter()
            .filter(|layer| layer.iter().any(|tx| transactions.contains(tx)))
            .collect();

        Ok(BlockBuildingResult {
            transactions,
            parallel_layers,
            total_fees,
            total_weight,
            excluded_transactions: excluded,
        })
    }

    /// Add transaction dependency relationship
    pub fn add_dependency(&self, parent: Vec<u8>, child: Vec<u8>) -> Result<(), String> {
        self.engine.add_dependency(parent, child)
    }

    /// Get parallel execution groups for block validation
    pub fn get_parallel_validation_groups(&self) -> Result<Vec<Vec<Vec<u8>>>, String> {
        self.engine.get_parallel_execution_groups()
    }

    /// Remove transaction and cascade effects
    pub fn remove_transaction_cascade(
        &self,
        tx_hash: &[u8],
    ) -> Result<CascadeRemovalResult, String> {
        let removed = self.engine.remove_transaction_cascade(tx_hash)?;

        Ok(CascadeRemovalResult {
            primary_tx: tx_hash.to_vec(),
            removed_dependents: removed.iter().skip(1).cloned().collect(),
            total_removed: removed.len(),
        })
    }

    /// Get current ordering snapshot
    pub fn get_current_ordering(&self) -> Result<CanonicalOrderingResult, String> {
        self.engine.compute_canonical_order()
    }

    /// Clear all state (for reset/testing)
    pub fn clear(&self) {
        self.engine.clear();
    }

    /// Get statistics
    pub fn get_stats(&self) -> ConflictOrderingStats {
        ConflictOrderingStats {
            transaction_count: self.engine.transaction_count(),
            conflict_count: self.engine.get_conflict_count(),
            config: self.config.clone(),
        }
    }

    /// Get engine reference for advanced operations
    pub fn engine(&self) -> &GraphConflictOrderingEngine {
        &self.engine
    }
}

/// Conflict detection result
#[derive(Clone, Debug)]
pub struct ConflictDetectionResult {
    pub tx_hash: Vec<u8>,
    pub detected_conflicts: Vec<Vec<u8>>,
    pub has_double_spend: bool,
    pub is_valid: bool,
}

/// Cascade removal result
#[derive(Clone, Debug)]
pub struct CascadeRemovalResult {
    pub primary_tx: Vec<u8>,
    pub removed_dependents: Vec<Vec<u8>>,
    pub total_removed: usize,
}

/// Statistics for conflict & ordering system
#[derive(Clone, Debug)]
pub struct ConflictOrderingStats {
    pub transaction_count: usize,
    pub conflict_count: usize,
    pub config: ConflictOrderingIntegrationConfig,
}

/// Helper function to validate input existence in UTXO set
fn validate_input_existence(_kv_store: &Arc<KvStore>, tx_hash: &Hash) -> Result<(), String> {
    // This is a placeholder function that would:
    // 1. Check if the UTXO exists in the on-chain state
    // 2. Verify it's not already spent
    // 3. Return error if checks fail

    // For now, always succeed since we're in a mock environment
    let _hash_bytes =
        bincode::serialize(tx_hash).map_err(|e| format!("Serialization error: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_integration_config_default() {
        let config = ConflictOrderingIntegrationConfig::default();

        assert!(config.validate_utxo_state);
        assert!(config.enable_cascade_validation);
        assert!(config.fee_weight > 0.0);
        assert!(config.age_weight > 0.0);
    }

    #[test]
    fn test_integration_creation() {
        let config = ConflictOrderingIntegrationConfig::default();
        let integration = ConflictOrderingIntegration::new(config, None);

        assert_eq!(integration.get_stats().transaction_count, 0);
    }

    #[test]
    fn test_utxo_validation_disabled() {
        let mut config = ConflictOrderingIntegrationConfig::default();
        config.validate_utxo_state = false;

        let integration = ConflictOrderingIntegration::new(config, None);

        // Create dummy transaction
        let tx = create_dummy_transaction();
        let result = integration.validate_utxo_state(&tx).unwrap();

        assert!(result.is_valid);
        assert_eq!(result.validation_errors.len(), 0);
    }

    #[test]
    fn test_block_building_empty() {
        let config = ConflictOrderingIntegrationConfig::default();
        let integration = ConflictOrderingIntegration::new(config, None);

        let result = integration.build_block_canonical(1000000).unwrap();
        assert_eq!(result.transactions.len(), 0);
    }

    fn create_dummy_transaction() -> Transaction {
        Transaction {
            id: Hash::new(&[0; 32]),
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
}
