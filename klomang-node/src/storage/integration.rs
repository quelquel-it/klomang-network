use std::sync::Arc;

use klomang_core::core::dag::BlockNode;
use klomang_core::core::state::transaction::Transaction;
use klomang_core::core::state::utxo::OutPoint as CoreOutPoint;

use crate::storage::cf::ColumnFamilyName;
use crate::storage::concurrency::StorageEngine;
use crate::storage::error::{StorageError, StorageResult};
use crate::storage::schema::{
    from_bytes, make_utxo_key, to_bytes, BlockValue, HeaderValue, TransactionInput,
    TransactionOutput, TransactionValue, UtxoValue,
};

/// Integration points between klomang-node storage and klomang-core components
pub struct CoreIntegration {
    storage: Arc<StorageEngine>,
}

impl CoreIntegration {
    pub fn new(storage: Arc<StorageEngine>) -> Self {
        Self { storage }
    }

    /// Get UTXO data directly from storage for transaction validation
    pub fn get_utxo(&self, outpoint: &CoreOutPoint) -> StorageResult<Option<UtxoValue>> {
        let tx_hash_bytes =
            to_bytes(&outpoint.0).map_err(|e| StorageError::SerializationError(e.to_string()))?;
        let key = make_utxo_key(&tx_hash_bytes, outpoint.1);

        self.storage
            .cache_layer
            .db()
            .get(ColumnFamilyName::Utxo, &key)
            .map_err(|e| StorageError::DbError(e.to_string()))?
            .map(|raw| from_bytes::<UtxoValue>(&raw))
            .transpose()
            .map_err(|e| StorageError::SerializationError(e.to_string()))
    }

    /// Apply and commit transaction state changes atomically
    pub fn apply_tx(&self, tx: &Transaction) -> StorageResult<()> {
        // Create internal batch
        let mut batch = crate::storage::batch::WriteBatch::new();

        // Process inputs: remove spent UTXOs
        for input in &tx.inputs {
            let tx_hash_bytes = to_bytes(&input.prev_tx)
                .map_err(|e| StorageError::SerializationError(e.to_string()))?;
            let utxo_key = make_utxo_key(&tx_hash_bytes, input.index);
            batch.delete_cf_typed(ColumnFamilyName::Utxo, &utxo_key);
        }

        // Process outputs: create new UTXOs
        let tx_hash_bytes =
            to_bytes(&tx.id).map_err(|e| StorageError::SerializationError(e.to_string()))?;

        for (output_index, output) in tx.outputs.iter().enumerate() {
            let utxo_key = make_utxo_key(&tx_hash_bytes, output_index as u32);
            let pubkey_hash_bytes = to_bytes(&output.pubkey_hash)
                .map_err(|e| StorageError::SerializationError(e.to_string()))?;

            let utxo_value = UtxoValue::new(output.value, pubkey_hash_bytes, vec![], 0);
            let serialized = to_bytes(&utxo_value)
                .map_err(|e| StorageError::SerializationError(e.to_string()))?;
            batch.put_cf_typed(ColumnFamilyName::Utxo, &utxo_key, &serialized);
        }

        // Commit batch immediately
        self.storage
            .cache_layer
            .db()
            .write_batch(batch)
            .map_err(|e| StorageError::DbError(e.to_string()))
        // Batch drops, no lock held
    }

    /// Store block with all transaction state changes
    pub fn store_block(&self, block: &BlockNode) -> StorageResult<()> {
        if block.transactions.is_empty() {
            return Err(StorageError::OperationFailed(
                "Block must have transactions".into(),
            ));
        }

        // Create header and block values
        let block_hash_bytes = to_bytes(&block.header.id)
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;

        let parent_hashes: Result<Vec<_>, StorageError> = block
            .header
            .parents
            .iter()
            .map(|p| to_bytes(p).map_err(|e| StorageError::SerializationError(e.to_string())))
            .collect();
        let parent_hashes = parent_hashes?;

        let verkle_root_bytes = to_bytes(&block.header.verkle_root)
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;

        let header_value = HeaderValue {
            block_hash: block_hash_bytes.clone(),
            parent_hashes,
            timestamp: block.header.timestamp,
            difficulty: block.header.difficulty,
            nonce: block.header.nonce,
            verkle_root: verkle_root_bytes,
            height: 0,
        };

        let transactions_bytes: Result<Vec<_>, StorageError> = block
            .transactions
            .iter()
            .map(|tx| to_bytes(tx).map_err(|e| StorageError::SerializationError(e.to_string())))
            .collect();
        let transactions = transactions_bytes?;

        let block_value = BlockValue {
            hash: block_hash_bytes.clone(),
            header_bytes: to_bytes(&block.header)
                .map_err(|e| StorageError::SerializationError(e.to_string()))?,
            transactions,
            timestamp: block.header.timestamp,
        };

        // Store header and block
        let header_bytes =
            to_bytes(&header_value).map_err(|e| StorageError::SerializationError(e.to_string()))?;
        let block_bytes =
            to_bytes(&block_value).map_err(|e| StorageError::SerializationError(e.to_string()))?;

        self.storage
            .cache_layer
            .db()
            .put(ColumnFamilyName::Headers, &block_hash_bytes, &header_bytes)
            .map_err(|e| StorageError::DbError(e.to_string()))?;
        self.storage
            .cache_layer
            .db()
            .put(ColumnFamilyName::Blocks, &block_hash_bytes, &block_bytes)
            .map_err(|e| StorageError::DbError(e.to_string()))?;

        // Store each transaction
        for tx in &block.transactions {
            let tx_id_bytes =
                to_bytes(&tx.id).map_err(|e| StorageError::SerializationError(e.to_string()))?;

            let tx_value = TransactionValue {
                tx_hash: tx_id_bytes.clone(),
                inputs: tx
                    .inputs
                    .iter()
                    .map(|input| {
                        let prev_tx_hash = to_bytes(&input.prev_tx).unwrap_or_default();
                        TransactionInput {
                            previous_tx_hash: prev_tx_hash,
                            output_index: input.index,
                        }
                    })
                    .collect(),
                outputs: tx
                    .outputs
                    .iter()
                    .map(|output| {
                        let pubkey_hash = to_bytes(&output.pubkey_hash).unwrap_or_default();
                        TransactionOutput {
                            amount: output.value,
                            pubkey_hash,
                        }
                    })
                    .collect(),
                fee: 0,
            };

            let tx_bytes =
                to_bytes(&tx_value).map_err(|e| StorageError::SerializationError(e.to_string()))?;
            self.storage
                .cache_layer
                .db()
                .put(ColumnFamilyName::Transactions, &tx_id_bytes, &tx_bytes)
                .map_err(|e| StorageError::DbError(e.to_string()))?;
        }

        Ok(())
    }
}
