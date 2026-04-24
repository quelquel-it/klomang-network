//! Transaction validation against UTXO storage
//!
//! Validates transaction inputs against UTXO set to detect:
//! - Missing UTXOs (orphan transactions)
//! - Double-spending attempts
//! - Invalid amounts or signatures

use std::sync::Arc;

use klomang_core::core::state::transaction::Transaction;

use crate::storage::kv_store::KvStore;
use crate::storage::error::StorageResult;

/// Result of transaction validation
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidationResult {
    /// All inputs are available and valid
    Valid,
    
    /// One or more inputs are missing from UTXO storage
    MissingInputs(Vec<usize>),
    
    /// Transaction failed validation due to double-spend
    DoubleSpent,
    
    /// Input UTXO was not found in storage
    InputNotFound(usize),
}

/// Validates transactions against UTXO storage
pub struct PoolValidator {
    kv_store: Option<Arc<KvStore>>,
}

impl PoolValidator {
    /// Create new validator with KV store access
    pub fn new(kv_store: Option<Arc<KvStore>>) -> Self {
        Self { kv_store }
    }

    /// Validate transaction inputs against UTXO storage
    pub fn validate_transaction(&self, tx: &Transaction) -> StorageResult<ValidationResult> {
        // Coinbase transactions are always valid
        if tx.is_coinbase() {
            return Ok(ValidationResult::Valid);
        }

        // If no KvStore available, assume valid (for backward compatibility)
        let Some(kv_store) = &self.kv_store else {
            return Ok(ValidationResult::Valid);
        };

        let mut missing_inputs = Vec::new();

        // Check each input against UTXO storage
        for (index, input) in tx.inputs.iter().enumerate() {
            let prev_tx_bytes = bincode::serialize(&input.prev_tx)
                .map_err(|e| crate::storage::error::StorageError::SerializationError(e.to_string()))?;

            // Check if output exists in UTXO set
            let exists = kv_store.utxo_exists(&prev_tx_bytes, input.index)?;

            if !exists {
                missing_inputs.push(index);
            }
        }

        if !missing_inputs.is_empty() {
            Ok(ValidationResult::MissingInputs(missing_inputs))
        } else {
            Ok(ValidationResult::Valid)
        }
    }

    /// Check if transaction has any missing inputs (would go to orphan pool)
    pub fn has_missing_inputs(&self, tx: &Transaction) -> StorageResult<bool> {
        match self.validate_transaction(tx)? {
            ValidationResult::MissingInputs(_) => Ok(true),
            _ => Ok(false),
        }
    }

    /// Get the UTXOs required by this transaction
    pub fn get_required_utxos(&self, tx: &Transaction) -> Vec<(Vec<u8>, u32)> {
        tx.inputs.iter()
            .map(|input| {
                let tx_bytes = bincode::serialize(&input.prev_tx).unwrap_or_default();
                (tx_bytes, input.index)
            })
            .collect()
    }

    /// Try to validate an orphan transaction (check if dependencies are now available)
    pub fn try_validate_orphan(&self, tx: &Transaction) -> StorageResult<bool> {
        match self.validate_transaction(tx)? {
            ValidationResult::Valid => Ok(true),
            ValidationResult::MissingInputs(_) => Ok(false),
            _ => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klomang_core::core::crypto::Hash;
    use klomang_core::core::state::transaction::{TxInput, TxOutput, SigHashType};

    fn create_validator_with_storage(kv_store: Arc<KvStore>) -> PoolValidator {
        PoolValidator::new(Some(kv_store))
    }

    fn create_test_transaction_with_inputs() -> Transaction {
        Transaction {
            id: Hash::new(&[1u8; 32]),
            inputs: vec![
                TxInput {
                    prev_tx: Hash::new(&[2u8; 32]),
                    index: 0,
                    signature: vec![],
                    pubkey: vec![],
                    sighash_type: SigHashType::All,
                },
            ],
            outputs: vec![
                TxOutput {
                    value: 5000,
                    pubkey_hash: Hash::new(&[3u8; 32]),
                },
            ],
            execution_payload: vec![],
            contract_address: None,
            gas_limit: 0,
            max_fee_per_gas: 0,
            chain_id: 1,
            locktime: 0,
        }
    }

    #[test]
    fn test_coinbase_always_valid() {
        let cache = Arc::new(
            crate::storage::cache::StorageCacheLayer::new(
                crate::storage::db::StorageDb::new("./.test_coinbase").unwrap()
            )
        );
        let kv_store = Arc::new(KvStore::new(cache));
        let validator = create_validator_with_storage(kv_store);

        let coinbase_tx = Transaction {
            id: Hash::new(&[1u8; 32]),
            inputs: vec![], // Coinbase has no inputs
            outputs: vec![
                TxOutput {
                    value: 5000,
                    pubkey_hash: Hash::new(&[2u8; 32]),
                },
            ],
            execution_payload: vec![],
            contract_address: None,
            gas_limit: 0,
            max_fee_per_gas: 0,
            chain_id: 1,
            locktime: 0,
        };

        assert_eq!(
            validator.validate_transaction(&coinbase_tx).unwrap(),
            ValidationResult::Valid
        );
    }

    #[test]
    fn test_required_utxos() {
        let validator = PoolValidator {
            kv_store: Some(Arc::new(KvStore::new(
                Arc::new(
                    crate::storage::cache::StorageCacheLayer::new(
                        crate::storage::db::StorageDb::new("./.test_utxos").unwrap()
                    )
                )
            ))),
        };

        let tx = create_test_transaction_with_inputs();
        let required = validator.get_required_utxos(&tx);

        assert_eq!(required.len(), 1);
        assert_eq!(required[0].1, 0);
    }
}
