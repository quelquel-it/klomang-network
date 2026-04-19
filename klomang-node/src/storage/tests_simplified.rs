#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;
    use bincode;

    use klomang_core::core::crypto::Hash;
    use klomang_core::core::state::transaction::{Transaction, TxInput, TxOutput, SigHashType};
    use klomang_core::core::dag::{BlockHeader, BlockNode};

    use crate::storage::batch::WriteBatch;
    use crate::storage::cf::ColumnFamilyName;
    use crate::storage::concurrency::StorageEngine;
    use crate::storage::db::StorageDb;
    use crate::storage::schema::{UtxoValue, make_utxo_key};

    // Helper: Serialize Hash to bytes (since field is private in klomang_core)
    fn hash_to_bytes(hash: &Hash) -> Vec<u8> {
        bincode::serialize(hash).expect("Failed to serialize Hash")
    }

    fn create_test_storage() -> (TempDir, Arc<StorageEngine>) {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let db_path = temp_dir.path().join("test.db");
        let db = StorageDb::new(&db_path).expect("Failed to create test database");
        let storage = Arc::new(StorageEngine::new(db).expect("Failed to create storage engine"));
        (temp_dir, storage)
    }

    fn create_test_transaction(id: Hash) -> Transaction {
        Transaction {
            id,
            inputs: vec![TxInput {
                prev_tx: Hash::new(&[0u8; 32]),
                index: 0,
                signature: vec![1, 2, 3],
                pubkey: vec![4, 5, 6],
                sighash_type: SigHashType::All,
            }],
            outputs: vec![TxOutput {
                value: 1000,
                pubkey_hash: Hash::new(&[7u8; 32]),
            }],
            execution_payload: vec![],
            contract_address: None,
            gas_limit: 0,
            max_fee_per_gas: 0,
            chain_id: 1,
            locktime: 0,
        }
    }

    #[test]
    fn test_storage_initialization() {
        let (_temp_dir, storage) = create_test_storage();
        
        // Verify storage was initialized successfully
        assert!(storage.cache_layer.db() is not null);
    }

    #[test]
    fn test_utxo_serialization() {
        // Test that UtxoValue can be serialized and deserialized correctly
        let utxo = UtxoValue::new(
            5000,
            vec![1, 2, 3, 4, 5],
            vec![],
            42,
        );

        let serialized = utxo.to_bytes().expect("Failed to serialize");
        let deserialized = UtxoValue::from_bytes(&serialized).expect("Failed to deserialize");

        assert_eq!(deserialized.amount, utxo.amount);
        assert_eq!(deserialized.pubkey_hash, utxo.pubkey_hash);
        assert_eq!(deserialized.block_height, utxo.block_height);
    }

    #[test]
    fn test_transaction_creation() {
        let tx_id = Hash::new(&[100u8; 32]);
        let tx = create_test_transaction(tx_id.clone());

        assert_eq!(tx.inputs.len(), 1);
        assert_eq!(tx.outputs.len(), 1);
        assert_eq!(tx.chain_id, 1);
    }

    #[test]
    fn test_utxo_key_generation() {
        let tx_hash = vec![1u8; 32];
        let output_index = 42u32;

        let key = make_utxo_key(&tx_hash, output_index);

        // Verify key format
        assert_eq!(key.len(), 36); // 32 bytes for hash + 4 bytes for index
    }

    #[test]
    fn test_write_batch_operations() {
        let (_temp_dir, db) = {
            let temp_dir = TempDir::new().expect("Failed to create temp dir");
            let db_path = temp_dir.path().join("test_batch.db");
            let db = StorageDb::new(&db_path).expect("Failed to create test database");
            (db, temp_dir)
        };

        let mut batch = WriteBatch::new();

        // Add key-value to batch
        let key = vec![1, 2, 3, 4, 5];
        let value = vec![10, 20, 30];
        batch.put_cf_typed(ColumnFamilyName::Utxo, &key, &value);

        // Execute batch
        db.write(batch).expect("Failed to write batch");

        // Verify data was written
        let retrieved = db.get(ColumnFamilyName::Utxo, &key)
            .expect("Failed to retrieve")
            .expect("Key should exist");
        assert_eq!(retrieved, value);
    }
}
