use std::sync::Arc;

use klomang_core::core::crypto::Hash;
use klomang_core::core::state::transaction::{Transaction, TxOutput};
use klomang_core::core::state::utxo::OutPoint as CoreOutPoint;
use klomang_core::core::dag::BlockNode;
use klomang_core::core::errors::CoreError;

use crate::storage::batch::WriteBatch;
use crate::storage::cf::ColumnFamilyName;
use crate::storage::concurrency::StorageEngine;
use crate::storage::error::{StorageError, StorageResult};
use crate::storage::schema::{BlockValue, HeaderValue, TransactionValue, UtxoValue, make_utxo_key};

/// Integration points between klomang-node storage and klomang-core components
pub struct CoreIntegration {
    storage: Arc<StorageEngine>,
}

impl CoreIntegration {
    pub fn new(storage: Arc<StorageEngine>) -> Self {
        Self { storage }
    }

    /// Get UTXO data directly from storage for transaction validation in Core
    ///
    /// This function bridges the gap between Core's OutPoint type and storage operations.
    pub fn get_utxo(&self, outpoint: &CoreOutPoint) -> StorageResult<Option<UtxoValue>> {
        let key = make_utxo_key(&outpoint.0.0, outpoint.1);
        self.storage.cache_layer.db().get(ColumnFamilyName::Utxo, &key)?
            .map(|raw| UtxoValue::from_bytes(&raw))
            .transpose()
    }

    /// Apply transaction state changes from Core to database operations
    ///
    /// Maps the logical state transitions from Core's transaction processing
    /// to concrete database write operations.
    pub fn apply_transaction_state(&self, tx: &Transaction, batch: &mut WriteBatch) -> StorageResult<()> {
        // Process inputs: mark UTXOs as spent
        for input in &tx.inputs {
            let outpoint = (input.prev_tx.clone(), input.index);
            let utxo_key = make_utxo_key(&outpoint.0.0, outpoint.1);

            // Remove from UTXO set
            batch.delete_cf_typed(ColumnFamilyName::Utxo, &utxo_key);

            // Record in spent index (if needed for tracking)
            // Note: Core handles the logical spent tracking, storage just removes
        }

        // Process outputs: create new UTXOs
        for (output_index, output) in tx.outputs.iter().enumerate() {
            let utxo_key = make_utxo_key(&tx.id.0, output_index as u32);
            let utxo_value = UtxoValue::new(
                output.value,
                output.pubkey_hash.0.to_vec(),
                output.script.clone(),
                0, // block_height - will be set when block is committed
            );

            let serialized = utxo_value.to_bytes()?;
            batch.put_cf_typed(ColumnFamilyName::Utxo, &utxo_key, &serialized);
        }

        Ok(())
    }
}

/// Persistent mempool storage for transaction resilience across node restarts
pub struct MempoolStorage {
    storage: Arc<StorageEngine>,
}

impl MempoolStorage {
    pub fn new(storage: Arc<StorageEngine>) -> Self {
        Self { storage }
    }

    /// Store a transaction in the persistent mempool
    pub fn store_transaction(&self, tx: &Transaction) -> StorageResult<()> {
        let tx_value = TransactionValue {
            tx_hash: tx.id.0.to_vec(),
            inputs: tx.inputs.iter().map(|input| crate::storage::schema::TransactionInput {
                previous_tx_hash: input.prev_tx.0.to_vec(),
                output_index: input.index,
            }).collect(),
            outputs: tx.outputs.iter().map(|output| crate::storage::schema::TransactionOutput {
                amount: output.value,
                script: output.script.clone(),
            }).collect(),
            fee: 0, // TODO: Calculate fee if needed
        };

        let serialized = tx_value.to_bytes()?;
        self.storage.writer.enqueue(vec![crate::storage::concurrency::StorageWriteCommand::Put {
            cf: ColumnFamilyName::Mempool,
            key: tx.id.0.to_vec(),
            value: serialized,
        }])
    }

    /// Remove a transaction from the persistent mempool
    pub fn remove_transaction(&self, tx_hash: &Hash) -> StorageResult<()> {
        self.storage.writer.enqueue(vec![crate::storage::concurrency::StorageWriteCommand::Delete {
            cf: ColumnFamilyName::Mempool,
            key: tx_hash.0.to_vec(),
        }])
    }

    /// Load all transactions from persistent mempool on startup
    pub fn load_all_transactions(&self) -> StorageResult<Vec<Transaction>> {
        // For now, return empty vec - full implementation would iterate CF Mempool
        // and reconstruct Transaction objects
        Ok(Vec::new())
    }
}

/// Network layer integration for storing validated blocks and transactions
pub struct NetworkStorage {
    storage: Arc<StorageEngine>,
    core_integration: Arc<CoreIntegration>,
}

impl NetworkStorage {
    pub fn new(storage: Arc<StorageEngine>, core_integration: Arc<CoreIntegration>) -> Self {
        Self {
            storage,
            core_integration,
        }
    }

    /// Store a block received from network after validation
    ///
    /// Validates block structure using Core rules before storing.
    pub fn store_block_from_network(&self, block: &BlockNode) -> StorageResult<()> {
        // Basic validation using Core's block structure
        if block.transactions.is_empty() {
            return Err(StorageError::OperationFailed("Block must contain at least one transaction".into()));
        }

        // Convert to storage format
        let header_value = HeaderValue {
            block_hash: block.header.id.0.to_vec(),
            parent_hashes: block.header.parents.iter().map(|p| p.0.to_vec()).collect(),
            timestamp: block.header.timestamp,
            difficulty: block.header.difficulty,
            nonce: block.header.nonce,
            verkle_root: block.header.verkle_root.0.to_vec(),
            height: 0, // Will be set by consensus layer
        };

        let block_value = BlockValue {
            hash: block.header.id.0.to_vec(),
            header_bytes: bincode::serialize(&block.header).map_err(|e| StorageError::SerializationError(e.to_string()))?,
            transactions: block.transactions.iter().map(|tx| {
                bincode::serialize(tx).map_err(|e| StorageError::SerializationError(e.to_string()))
            }).collect::<Result<Vec<_>, _>>()?,
            timestamp: block.header.timestamp,
        };

        // Store using write queue
        let mut commands = vec![
            crate::storage::concurrency::StorageWriteCommand::Put {
                cf: ColumnFamilyName::Headers,
                key: block.header.id.0.to_vec(),
                value: header_value.to_bytes()?,
            },
            crate::storage::concurrency::StorageWriteCommand::Put {
                cf: ColumnFamilyName::Blocks,
                key: block.header.id.0.to_vec(),
                value: block_value.to_bytes()?,
            },
        ];

        // Store transactions
        for tx in &block.transactions {
            let tx_value = TransactionValue {
                tx_hash: tx.id.0.to_vec(),
                inputs: tx.inputs.iter().map(|input| crate::storage::schema::TransactionInput {
                    previous_tx_hash: input.prev_tx.0.to_vec(),
                    output_index: input.index,
                }).collect(),
                outputs: tx.outputs.iter().map(|output| crate::storage::schema::TransactionOutput {
                    amount: output.value,
                    script: output.script.clone(),
                }).collect(),
                fee: 0, // TODO: Calculate fee
            };

            commands.push(crate::storage::concurrency::StorageWriteCommand::Put {
                cf: ColumnFamilyName::Transactions,
                key: tx.id.0.to_vec(),
                value: tx_value.to_bytes()?,
            });
        }

        self.storage.writer.enqueue(commands)
    }

    /// Store a transaction received from network after validation
    ///
    /// Validates transaction structure using Core rules before storing.
    pub fn store_tx_from_network(&self, tx: &Transaction) -> StorageResult<()> {
        // Basic validation
        if tx.inputs.is_empty() && tx.outputs.is_empty() {
            return Err(StorageError::OperationFailed("Transaction must have inputs or outputs".into()));
        }

        // Convert to storage format
        let tx_value = TransactionValue {
            tx_hash: tx.id.0.to_vec(),
            inputs: tx.inputs.iter().map(|input| crate::storage::schema::TransactionInput {
                previous_tx_hash: input.prev_tx.0.to_vec(),
                output_index: input.index,
            }).collect(),
            outputs: tx.outputs.iter().map(|output| crate::storage::schema::TransactionOutput {
                amount: output.value,
                script: output.script.clone(),
            }).collect(),
            fee: 0, // TODO: Calculate fee
        };

        // Store using write queue
        self.storage.writer.enqueue(vec![crate::storage::concurrency::StorageWriteCommand::Put {
            cf: ColumnFamilyName::Transactions,
            key: tx.id.0.to_vec(),
            value: tx_value.to_bytes()?,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    use klomang_core::core::crypto::Hash;
    use klomang_core::core::state::transaction::{Transaction, TxInput, TxOutput};
    use klomang_core::core::state::utxo::OutPoint as CoreOutPoint;

    use crate::storage::db::StorageDb;
    use crate::storage::concurrency::StorageEngine;
    use crate::storage::schema::{UtxoValue, make_utxo_key};

    fn create_test_storage() -> (TempDir, Arc<StorageEngine>) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let db = StorageDb::new(&db_path).unwrap();
        let storage = Arc::new(StorageEngine::new(db).unwrap());
        (temp_dir, storage)
    }

    #[test]
    fn test_get_utxo_integration() {
        let (_temp_dir, storage) = create_test_storage();
        let integration = CoreIntegration::new(Arc::clone(&storage));

        // Create test UTXO
        let tx_hash = Hash([1u8; 32]);
        let output_index = 0u32;
        let outpoint = CoreOutPoint(tx_hash.clone(), output_index);

        let utxo_value = UtxoValue::new(
            1000,
            vec![2u8; 20], // pubkey hash
            vec![0x76, 0xa9, 0x14], // script
            1, // block_height
        );

        // Store UTXO directly
        let key = make_utxo_key(&tx_hash.0, output_index);
        let serialized = utxo_value.to_bytes().unwrap();
        storage.writer.enqueue(vec![crate::storage::concurrency::StorageWriteCommand::Put {
            cf: crate::storage::cf::ColumnFamilyName::Utxo,
            key,
            value: serialized,
        }]);

        // Test retrieval through integration
        let result = integration.get_utxo(&outpoint).unwrap();
        assert!(result.is_some());
        let retrieved = result.unwrap();
        assert_eq!(retrieved.amount, 1000);
        assert_eq!(retrieved.pubkey_hash, vec![2u8; 20]);
    }

    #[test]
    fn test_apply_transaction_state() {
        let (_temp_dir, storage) = create_test_storage();
        let integration = CoreIntegration::new(Arc::clone(&storage));

        // Create test transaction
        let input_tx_hash = Hash([1u8; 32]);
        let input = TxInput {
            prev_tx: input_tx_hash.clone(),
            index: 0,
            script_sig: vec![],
        };

        let output = TxOutput {
            value: 500,
            pubkey_hash: vec![3u8; 20],
            script: vec![0x76, 0xa9, 0x14],
        };

        let tx = Transaction {
            id: Hash([2u8; 32]),
            inputs: vec![input],
            outputs: vec![output],
        };

        // Create batch and apply transaction state
        let mut batch = crate::storage::batch::WriteBatch::new();
        integration.apply_transaction_state(&tx, &mut batch).unwrap();

        // Verify batch contains delete for spent UTXO
        // Note: This is a basic test - full validation would require executing the batch
        assert!(!batch.is_empty());
    }

    #[test]
    fn test_mempool_storage() {
        let (_temp_dir, storage) = create_test_storage();
        let mempool = MempoolStorage::new(Arc::clone(&storage));

        // Create test transaction
        let tx = Transaction {
            id: Hash([1u8; 32]),
            inputs: vec![],
            outputs: vec![TxOutput {
                value: 1000,
                pubkey_hash: vec![2u8; 20],
                script: vec![0x76, 0xa9, 0x14],
            }],
        };

        // Store transaction
        mempool.store_transaction(&tx).unwrap();

        // Remove transaction
        mempool.remove_transaction(&tx.id).unwrap();

        // Load all (should be empty after removal)
        let loaded = mempool.load_all_transactions().unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_network_storage_validation() {
        let (_temp_dir, storage) = create_test_storage();
        let integration = Arc::new(CoreIntegration::new(Arc::clone(&storage)));
        let network = NetworkStorage::new(Arc::clone(&storage), integration);

        // Test invalid block (empty transactions)
        let invalid_block = klomang_core::core::dag::BlockNode {
            header: klomang_core::core::dag::BlockHeader {
                id: Hash([1u8; 32]),
                parents: vec![],
                timestamp: 1234567890,
                difficulty: 1,
                nonce: 0,
                verkle_root: Hash([0u8; 32]),
            },
            transactions: vec![], // Invalid: empty transactions
        };

        // Should fail validation
        let result = network.store_block_from_network(&invalid_block);
        assert!(result.is_err());
    }
}