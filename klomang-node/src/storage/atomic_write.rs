//! Atomic write operations for blockchain data with strong consistency guarantees.
//!
//! This module implements the Write Path with Atomicity using RocksDB WriteBatch.
//! All operations are grouped into a single batch that is committed atomically,
//! ensuring that either all operations succeed or none of them do.

use crate::storage::batch::WriteBatch;
use crate::storage::cf::ColumnFamilyName;
use crate::storage::db::StorageDb;
use crate::storage::error::{StorageError, StorageResult};
use crate::storage::schema::*;

/// Atomic block insertion handler
pub struct AtomicBlockWriter;

impl AtomicBlockWriter {
    /// Commit a block to storage atomically with all its transactions and state changes.
    ///
    /// This function ensures that the following operations are all-or-nothing:
    /// 1. Store the block and its header
    /// 2. Store all transactions in the block
    /// 3. Update UTXO state (mark spent UTXOs, create new UTXOs)
    /// 4. Update DAG structure (parent-child relationships, tips)
    /// 5. Update Verkle state if provided
    ///
    /// # Arguments
    ///
    /// * `db` - Database instance
    /// * `block_hash` - The hash of the block being inserted
    /// * `block_value` - The block data to store
    /// * `header_value` - The header data to store
    /// * `transactions` - List of transactions with their data and UTXOs
    /// * `dag_node` - DAG node information for the block
    /// * `dag_tips` - Updated DAG tips information
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` if the block was committed successfully, or `StorageError` if any
    /// step in the process failed. In case of error, no data is written to the database.
    pub fn commit_block_to_storage(
        db: &StorageDb,
        block_hash: &[u8],
        block_value: &BlockValue,
        header_value: &HeaderValue,
        transactions: Vec<BlockTransactionBatch>,
        dag_node: &DagNodeValue,
        dag_tips: &DagTipsValue,
    ) -> StorageResult<()> {
        let mut batch = WriteBatch::new();

        // Step 1: Prepare block and header data (with error handling during preparation)
        let block_bytes = block_value.to_bytes()?;
        let header_bytes = header_value.to_bytes()?;

        // Step 2: Prepare transaction data and UTXO updates
        let mut tx_operations = Vec::new();
        for tx_batch in transactions {
            // Serialize transaction first to ensure it's valid
            let tx_bytes = tx_batch.tx_value.to_bytes()?;
            tx_operations.push((tx_batch, tx_bytes));
        }

        // Step 3: Prepare DAG update data
        let dag_node_bytes = dag_node.to_bytes()?;
        let dag_tips_bytes = dag_tips.to_bytes()?;

        // All data is prepared and validated. Now commit to batch (this won't fail)
        // Add block and header to batch
        batch.put_cf_typed(ColumnFamilyName::Blocks, block_hash, &block_bytes);
        batch.put_cf_typed(ColumnFamilyName::Headers, block_hash, &header_bytes);

        // Add transactions and manage UTXO state
        for (tx_batch, tx_bytes) in tx_operations {
            let tx_hash = &tx_batch.tx_hash;

            // Store transaction
            batch.put_cf_typed(ColumnFamilyName::Transactions, tx_hash, &tx_bytes);

            // Process transaction inputs - mark UTXOs as spent
            for spent_utxo in tx_batch.spent_utxos {
                let utxo_key = make_utxo_key(&spent_utxo.prev_tx_hash, spent_utxo.output_index);

                // Delete from UTXO set
                batch.delete_cf_typed(ColumnFamilyName::Utxo, &utxo_key);

                // Record in spent index
                let spent_value_bytes = spent_utxo.spent_value.to_bytes()?;
                batch.put_cf_typed(ColumnFamilyName::UtxoSpent, &utxo_key, &spent_value_bytes);
            }

            // Process transaction outputs - create new UTXOs
            for (output_index, new_utxo) in tx_batch.new_utxos.iter().enumerate() {
                let utxo_key = make_utxo_key(tx_hash, output_index as u32);
                let utxo_bytes = new_utxo.to_bytes()?;

                batch.put_cf_typed(ColumnFamilyName::Utxo, &utxo_key, &utxo_bytes);
            }
        }

        // Update DAG structure
        batch.put_cf_typed(ColumnFamilyName::Dag, block_hash, &dag_node_bytes);
        batch.put_cf_typed(ColumnFamilyName::DagTips, b"current_tips", &dag_tips_bytes);

        // Step 4: Commit batch atomically
        db.write_batch(batch)
            .map_err(|e| StorageError::DbError(format!("failed to commit block batch: {}", e)))?;

        Ok(())
    }

    /// Commit a block without WAL for non-critical operations (snapshot sync, etc.)
    ///
    /// # Warning
    ///
    /// This bypasses the write-ahead log. Only use for non-critical data that can be
    /// recomputed (e.g., snapshots during bulk sync).
    pub fn commit_block_to_storage_no_wal(
        db: &StorageDb,
        block_hash: &[u8],
        block_value: &BlockValue,
        header_value: &HeaderValue,
        transactions: Vec<BlockTransactionBatch>,
        dag_node: &DagNodeValue,
        dag_tips: &DagTipsValue,
    ) -> StorageResult<()> {
        let mut batch = WriteBatch::new();

        // Prepare all data first (same as with WAL)
        let block_bytes = block_value.to_bytes()?;
        let header_bytes = header_value.to_bytes()?;

        let mut tx_operations = Vec::new();
        for tx_batch in transactions {
            let tx_bytes = tx_batch.tx_value.to_bytes()?;
            tx_operations.push((tx_batch, tx_bytes));
        }

        let dag_node_bytes = dag_node.to_bytes()?;
        let dag_tips_bytes = dag_tips.to_bytes()?;

        // Build batch
        batch.put_cf_typed(ColumnFamilyName::Blocks, block_hash, &block_bytes);
        batch.put_cf_typed(ColumnFamilyName::Headers, block_hash, &header_bytes);

        for (tx_batch, tx_bytes) in tx_operations {
            let tx_hash = &tx_batch.tx_hash;

            batch.put_cf_typed(ColumnFamilyName::Transactions, tx_hash, &tx_bytes);

            for spent_utxo in tx_batch.spent_utxos {
                let utxo_key = make_utxo_key(&spent_utxo.prev_tx_hash, spent_utxo.output_index);
                batch.delete_cf_typed(ColumnFamilyName::Utxo, &utxo_key);

                let spent_value_bytes = spent_utxo.spent_value.to_bytes()?;
                batch.put_cf_typed(ColumnFamilyName::UtxoSpent, &utxo_key, &spent_value_bytes);
            }

            for (output_index, new_utxo) in tx_batch.new_utxos.iter().enumerate() {
                let utxo_key = make_utxo_key(tx_hash, output_index as u32);
                let utxo_bytes = new_utxo.to_bytes()?;
                batch.put_cf_typed(ColumnFamilyName::Utxo, &utxo_key, &utxo_bytes);
            }
        }

        batch.put_cf_typed(ColumnFamilyName::Dag, block_hash, &dag_node_bytes);
        batch.put_cf_typed(ColumnFamilyName::DagTips, b"current_tips", &dag_tips_bytes);

        // Commit without WAL
        db.write_batch_no_wal(batch)
            .map_err(|e| StorageError::DbError(format!("failed to commit block batch (no-wal): {}", e)))?;

        Ok(())
    }
}

/// Represents a transaction's data and its UTXO changes within a block
#[derive(Debug, Clone)]
pub struct BlockTransactionBatch {
    /// Transaction hash
    pub tx_hash: Vec<u8>,
    /// Transaction data
    pub tx_value: TransactionValue,
    /// UTXOs spent by this transaction
    pub spent_utxos: Vec<SpentUtxoBatch>,
    /// New UTXOs created by this transaction
    pub new_utxos: Vec<UtxoValue>,
}

/// Represents a UTXO that was spent in a transaction
#[derive(Debug, Clone)]
pub struct SpentUtxoBatch {
    /// Hash of the transaction that created this UTXO
    pub prev_tx_hash: Vec<u8>,
    /// Output index in the previous transaction
    pub output_index: u32,
    /// Information about where this UTXO was spent
    pub spent_value: UtxoSpentValue,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_transaction_batch_creation() {
        let tx_hash = b"test_tx_hash".to_vec();
        let tx_value = TransactionValue {
            tx_hash: tx_hash.clone(),
            inputs: vec![],
            outputs: vec![],
            fee: 0,
        };

        let batch = BlockTransactionBatch {
            tx_hash,
            tx_value,
            spent_utxos: vec![],
            new_utxos: vec![],
        };

        assert_eq!(batch.spent_utxos.len(), 0);
        assert_eq!(batch.new_utxos.len(), 0);
    }
}
