//! Parallel Validation Worker with Rayon Thread Pool
//!
//! This module implements:
//! - Parallel validation using rayon threads
//! - Per-shard validation tasks
//! - Batch processing across multiple cores
//! - Result aggregation
//!
//! Key Feature:
//! - Leverage all CPU cores for validation
//! - Each shard processes independently
//! - Deterministic results aggregation

use std::sync::Arc;
use rayon::prelude::*;
use crate::storage::KvStore;
use super::parallel_mempool::{ParallelMempool, TransactionStatus, SubPoolEntry};

/// Validation task for a single transaction
#[derive(Clone, Debug)]
pub struct ValidationTask {
    pub tx_entry: SubPoolEntry,
    pub shard_id: usize,
}

/// Result of parallel validation
#[derive(Clone, Debug)]
pub struct ValidationResult {
    pub tx_hash: Vec<u8>,
    pub shard_id: usize,
    pub status: TransactionStatus,
    pub error: Option<String>,
}

/// Configuration for parallel validator
#[derive(Clone, Debug)]
pub struct ParallelValidatorConfig {
    /// Number of worker threads
    pub num_workers: usize,
    /// Use UTXO validation from storage
    pub validate_utxos: bool,
    /// Enable strict validation
    pub strict_validation: bool,
}

impl Default for ParallelValidatorConfig {
    fn default() -> Self {
        Self {
            num_workers: 4,  // Default to 4 workers
            validate_utxos: true,
            strict_validation: true,
        }
    }
}

/// Parallel Validator for transaction validation across shards
pub struct ParallelValidator {
    config: ParallelValidatorConfig,
    kv_store: Option<Arc<KvStore>>,
}

impl ParallelValidator {
    /// Create new validator
    pub fn new() -> Self {
        Self {
            config: ParallelValidatorConfig::default(),
            kv_store: None,
        }
    }

    /// Create validator with storage
    pub fn with_storage(kv_store: Arc<KvStore>) -> Self {
        Self {
            config: ParallelValidatorConfig::default(),
            kv_store: Some(kv_store),
        }
    }

    /// Create validator with custom config
    pub fn with_config(config: ParallelValidatorConfig) -> Self {
        Self {
            config,
            kv_store: None,
        }
    }

    /// Validate all pending transactions in parallel
    pub fn validate_parallel(&self, mempool: &ParallelMempool) -> Result<Vec<ValidationResult>, String> {
        // Collect all pending transactions
        let pending = mempool.get_by_status(TransactionStatus::Pending);

        if pending.is_empty() {
            return Ok(Vec::new());
        }

        // Create tasks
        let tasks: Vec<ValidationTask> = pending
            .iter()
            .map(|entry| {
                // Determine shard ID
                let shard_id = {
                    let tx_hash = bincode::serialize(&entry.transaction.id).unwrap_or_default();
                    // Find which shard it's in
                    let mut found_shard = 0;
                    for (idx, shard) in mempool.get_all_shards().iter().enumerate() {
                        if shard.contains(&tx_hash) {
                            found_shard = idx;
                            break;
                        }
                    }
                    found_shard
                };

                ValidationTask {
                    tx_entry: entry.clone(),
                    shard_id,
                }
            })
            .collect();

        // Use rayon for parallel validation
        let config = self.config.clone();
        let kv_store = self.kv_store.clone();

        let results = tasks
            .into_par_iter()
            .map(|task| {
                Self::validate_single_transaction(&task, &config, kv_store.as_ref())
            })
            .collect();

        Ok(results)
    }

    /// Validate single transaction
    fn validate_single_transaction(
        task: &ValidationTask,
        config: &ParallelValidatorConfig,
        kv_store: Option<&Arc<KvStore>>,
    ) -> ValidationResult {
        let tx_hash = bincode::serialize(&task.tx_entry.transaction.id)
            .unwrap_or_else(|_| vec![]);

        // Basic validation
        let mut error = None;

        // Check transaction structure
        let mut status = if task.tx_entry.transaction.inputs.is_empty() && !task.tx_entry.transaction.is_coinbase() {
            error = Some("No inputs and not coinbase".to_string());
            TransactionStatus::Invalid
        } else if task.tx_entry.transaction.outputs.is_empty() {
            error = Some("No outputs".to_string());
            TransactionStatus::Invalid
        } else if config.strict_validation {
            // Perform strict validation
            // This would include signature checking, etc.
            TransactionStatus::Validated
        } else {
            TransactionStatus::Validated
        };

        // UTXO validation (if enabled and kv_store available)
        if config.validate_utxos && kv_store.is_some() {
            // In real implementation, would check UTXO existence in kv_store
            // For now, assume valid
            if status == TransactionStatus::Validated {
                status = TransactionStatus::Validated;
            }
        }

        ValidationResult {
            tx_hash,
            shard_id: task.shard_id,
            status,
            error,
        }
    }

    /// Apply validation results to mempool
    pub fn apply_results(
        &self,
        mempool: &ParallelMempool,
        results: &[ValidationResult],
    ) -> Result<usize, String> {
        let mut updated = 0;

        for result in results {
            if let Err(e) = mempool.update_status(&result.tx_hash, result.status) {
                eprintln!("Failed to update status: {}", e);
            } else {
                updated += 1;
            }
        }

        Ok(updated)
    }

    /// Full validation pipeline
    pub fn validate_and_apply(
        &self,
        mempool: &ParallelMempool,
    ) -> Result<ValidationStats, String> {
        let start = std::time::Instant::now();

        let _pending_count = mempool.get_by_status(TransactionStatus::Pending).len();
        let results = self.validate_parallel(mempool)?;
        let applied = self.apply_results(mempool, &results)?;

        let duration = start.elapsed();

        let stats = ValidationStats {
            transactions_processed: results.len(),
            transactions_updated: applied,
            validated_count: results.iter().filter(|r| r.status == TransactionStatus::Validated).count(),
            invalid_count: results.iter().filter(|r| r.status == TransactionStatus::Invalid).count(),
            duration_ms: duration.as_millis() as u64,
        };

        Ok(stats)
    }
}

impl Default for ParallelValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for ParallelValidator {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            kv_store: self.kv_store.clone(),
        }
    }
}

/// Statistics from parallel validation
#[derive(Clone, Debug)]
pub struct ValidationStats {
    pub transactions_processed: usize,
    pub transactions_updated: usize,
    pub validated_count: usize,
    pub invalid_count: usize,
    pub duration_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use klomang_core::core::crypto::Hash;
    use klomang_core::core::state::transaction::{TxInput, TxOutput};

    fn create_test_entry() -> SubPoolEntry {
        SubPoolEntry {
            transaction: klomang_core::core::state::transaction::Transaction {
                id: Hash::new(&[1u8; 32]),
                inputs: vec![TxInput {
                    prev_tx: Hash::new(&[1u8; 32]),
                    index: 0,
                    signature: vec![],
                    pubkey: vec![],
                    sighash_type: klomang_core::core::state::transaction::SigHashType::All,
                }],
                outputs: vec![TxOutput {
                    value: 1000,
                    pubkey_hash: Hash::new(&[2u8; 32]),
                }],
                execution_payload: vec![],
                contract_address: None,
                gas_limit: 0,
                max_fee_per_gas: 0,
                chain_id: 1,
                locktime: 0,
            },
            status: TransactionStatus::Pending,
            arrival_time: 0,
            size_bytes: 200,
            outpoints: vec![],
        }
    }

    #[test]
    fn test_validator_creation() {
        let validator = ParallelValidator::new();
        assert_eq!(validator.config.num_workers, num_cpus::get().max(2));
    }

    #[test]
    fn test_single_validation() {
        let config = ParallelValidatorConfig::default();
        let task = ValidationTask {
            tx_entry: create_test_entry(),
            shard_id: 0,
        };

        let result = ParallelValidator::validate_single_transaction(&task, &config, None);
        assert_eq!(result.shard_id, 0);
    }
}
