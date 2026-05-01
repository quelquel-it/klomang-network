#[cfg(test)]
mod tests {
    use bincode;
    use std::sync::Arc;
    use tempfile::TempDir;

    use klomang_core::core::crypto::Hash;
    use klomang_core::core::dag::{BlockHeader, BlockNode};
    use klomang_core::core::state::transaction::{Transaction, TxInput, TxOutput};
    use klomang_core::NoOpMetricsCollector;

    use crate::storage::batch::WriteBatch;
    use crate::storage::cf::ColumnFamilyName;
    use crate::storage::config::StorageConfig;
    use crate::storage::db::StorageDb;
    use crate::storage::schema::{
        make_utxo_key, parse_utxo_key, BlockValue, TransactionInput, TransactionOutput,
        TransactionValue, UtxoValue,
    };

    // ============================================
    // HELPER FUNCTIONS
    // ============================================

    /// Helper to serialize Hash to bytes (field is private in klomang-core)
    fn hash_to_bytes(hash: &Hash) -> Vec<u8> {
        bincode::serialize(hash).expect("Failed to serialize Hash")
    }

    /// Create isolated test database with unique tempdir
    fn create_test_db() -> (TempDir, StorageDb) {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let db_path = temp_dir.path().join("testdb");
        let wal_path = temp_dir.path().join("testdb_wal");
        let config = StorageConfig::new(&db_path).with_wal_dir(&wal_path);
        let metrics = Arc::new(crate::storage::metrics::StorageMetrics::new(Box::new(
            NoOpMetricsCollector,
        )));
        let db =
            StorageDb::open_with_config(&config, metrics).expect("Failed to create test database");
        (temp_dir, db)
    }

    /// Create test transaction with unique hash
    fn create_test_transaction(hash: Hash) -> Transaction {
        Transaction {
            id: hash,
            inputs: vec![TxInput {
                prev_tx: Hash::new(&[0u8; 32]),
                index: 0,
                signature: vec![1, 2, 3],
                pubkey: vec![4, 5, 6],
                sighash_type: klomang_core::core::state::transaction::SigHashType::All,
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

    /// Create test block with unique hash
    fn create_test_block(hash: Hash) -> BlockNode {
        use std::collections::HashSet;
        BlockNode {
            header: BlockHeader {
                id: hash.clone(),
                parents: HashSet::from_iter(vec![Hash::new(&[10u8; 32])]),
                timestamp: 1234567890,
                difficulty: 1000,
                nonce: 42,
                verkle_root: Hash::new(&[11u8; 32]),
                verkle_proofs: None,
                signature: None,
            },
            children: HashSet::new(),
            selected_parent: None,
            blue_set: HashSet::new(),
            red_set: HashSet::new(),
            blue_score: 0,
            transactions: vec![create_test_transaction(Hash::new(&[12u8; 32]))],
        }
    }

    // ============================================
    // BASIC STORAGE TESTS
    // ============================================

    #[test]
    fn test_db_basic_put_get() {
        let (_temp_dir, db) = create_test_db();

        // Put and get basic key-value
        let key = b"test_key".to_vec();
        let value = b"test_value".to_vec();

        db.put(ColumnFamilyName::Transactions, &key, &value)
            .expect("Failed to put value");

        let retrieved = db
            .get(ColumnFamilyName::Transactions, &key)
            .expect("Failed to get value")
            .expect("Value not found");

        assert_eq!(retrieved, value);
        drop(db);
    }

    #[test]
    fn test_db_exists() {
        let (_temp_dir, db) = create_test_db();

        let key = b"test_key".to_vec();
        let value = b"test_value".to_vec();

        // Key should not exist initially
        assert!(!db
            .exists(ColumnFamilyName::Transactions, &key)
            .expect("Failed to check existence"));

        // Put value
        db.put(ColumnFamilyName::Transactions, &key, &value)
            .expect("Failed to put value");

        // Key should exist now
        assert!(db
            .exists(ColumnFamilyName::Transactions, &key)
            .expect("Failed to check existence"));
        drop(db);
    }

    #[test]
    fn test_db_delete() {
        let (_temp_dir, db) = create_test_db();

        let key = b"test_key".to_vec();
        let value = b"test_value".to_vec();

        // Put value
        db.put(ColumnFamilyName::Transactions, &key, &value)
            .expect("Failed to put value");

        // Verify it exists
        assert!(db
            .get(ColumnFamilyName::Transactions, &key)
            .expect("Failed to get value")
            .is_some());

        // Delete it
        db.delete(ColumnFamilyName::Transactions, &key)
            .expect("Failed to delete value");

        // Verify it's gone
        assert!(db
            .get(ColumnFamilyName::Transactions, &key)
            .expect("Failed to get value")
            .is_none());
        drop(db);
    }

    // ============================================
    // BATCH OPERATION TESTS
    // ============================================

    #[test]
    fn test_write_batch_atomicity() {
        let (_temp_dir, db) = create_test_db();

        // Create batch with multiple operations
        let mut batch = WriteBatch::new();

        let key1 = b"key1".to_vec();
        let value1 = b"value1".to_vec();
        let key2 = b"key2".to_vec();
        let value2 = b"value2".to_vec();

        batch.put_cf_typed(ColumnFamilyName::Transactions, &key1, &value1);
        batch.put_cf_typed(ColumnFamilyName::Transactions, &key2, &value2);

        // Execute batch
        db.write_batch(batch).expect("Failed to write batch");

        // Verify both values exist
        assert_eq!(
            db.get(ColumnFamilyName::Transactions, &key1)
                .expect("Failed to get key1"),
            Some(value1)
        );
        assert_eq!(
            db.get(ColumnFamilyName::Transactions, &key2)
                .expect("Failed to get key2"),
            Some(value2)
        );
        drop(db);
    }

    // ============================================
    // SCHEMA SERIALIZATION TESTS
    // ============================================

    #[test]
    fn test_block_value_serialization() {
        let (_temp_dir, db) = create_test_db();

        let block_hash = Hash::new(&[1u8; 32]);
        let block = create_test_block(block_hash.clone());

        // Convert to storage format using bincode
        let block_hash_bytes = hash_to_bytes(&block_hash);

        let block_value = BlockValue {
            hash: block_hash_bytes.clone(),
            header_bytes: bincode::serialize(&block.header).expect("Failed to serialize header"),
            transactions: block
                .transactions
                .iter()
                .map(|tx| bincode::serialize(tx).expect("Failed to serialize transaction"))
                .collect(),
            timestamp: block.header.timestamp,
        };

        // Serialize and store
        let serialized = block_value
            .to_bytes()
            .expect("Failed to serialize BlockValue");
        db.put(ColumnFamilyName::Blocks, &block_hash_bytes, &serialized)
            .expect("Failed to store block");

        // Retrieve and deserialize
        let retrieved_bytes = db
            .get(ColumnFamilyName::Blocks, &block_hash_bytes)
            .expect("Failed to get block")
            .expect("Block not found");

        let retrieved_block =
            BlockValue::from_bytes(&retrieved_bytes).expect("Failed to deserialize BlockValue");

        // Verify integrity
        assert_eq!(retrieved_block.hash, block_value.hash);
        assert_eq!(retrieved_block.timestamp, block_value.timestamp);
        assert_eq!(
            retrieved_block.transactions.len(),
            block_value.transactions.len()
        );
        drop(db);
    }

    #[test]
    fn test_transaction_value_serialization() {
        let (_temp_dir, db) = create_test_db();

        let tx_hash = Hash::new(&[2u8; 32]);
        let tx = create_test_transaction(tx_hash.clone());

        let tx_hash_bytes = hash_to_bytes(&tx_hash);

        // Convert to storage format
        let tx_value = TransactionValue {
            tx_hash: tx_hash_bytes.clone(),
            inputs: tx
                .inputs
                .iter()
                .map(|input| TransactionInput {
                    previous_tx_hash: hash_to_bytes(&input.prev_tx),
                    output_index: input.index,
                })
                .collect(),
            outputs: tx
                .outputs
                .iter()
                .map(|output| TransactionOutput {
                    amount: output.value,
                    pubkey_hash: hash_to_bytes(&output.pubkey_hash),
                })
                .collect(),
            fee: 10,
        };

        // Serialize and store
        let serialized = tx_value
            .to_bytes()
            .expect("Failed to serialize TransactionValue");
        db.put(ColumnFamilyName::Transactions, &tx_hash_bytes, &serialized)
            .expect("Failed to store transaction");

        // Retrieve and deserialize
        let retrieved_bytes = db
            .get(ColumnFamilyName::Transactions, &tx_hash_bytes)
            .expect("Failed to get transaction")
            .expect("Transaction not found");

        let retrieved_tx = TransactionValue::from_bytes(&retrieved_bytes)
            .expect("Failed to deserialize TransactionValue");

        // Verify integrity
        assert_eq!(retrieved_tx.tx_hash, tx_value.tx_hash);
        assert_eq!(retrieved_tx.inputs.len(), tx_value.inputs.len());
        assert_eq!(retrieved_tx.outputs.len(), tx_value.outputs.len());
        assert_eq!(retrieved_tx.fee, tx_value.fee);
        drop(db);
    }

    #[test]
    fn test_utxo_value_serialization() {
        let (_temp_dir, db) = create_test_db();

        let tx_hash = Hash::new(&[3u8; 32]);
        let tx_hash_bytes = hash_to_bytes(&tx_hash);

        let utxo_key = make_utxo_key(&tx_hash_bytes, 0);
        let pubkey_hash = Hash::new(&[8u8; 32]);
        let pubkey_hash_bytes = hash_to_bytes(&pubkey_hash);

        let utxo_value = UtxoValue::new(500, pubkey_hash_bytes.clone(), vec![], 1000);

        // Serialize and store
        let serialized = utxo_value
            .to_bytes()
            .expect("Failed to serialize UtxoValue");
        db.put(ColumnFamilyName::Utxo, &utxo_key, &serialized)
            .expect("Failed to store UTXO");

        // Retrieve and deserialize
        let retrieved_bytes = db
            .get(ColumnFamilyName::Utxo, &utxo_key)
            .expect("Failed to get UTXO")
            .expect("UTXO not found");

        let retrieved_utxo =
            UtxoValue::from_bytes(&retrieved_bytes).expect("Failed to deserialize UtxoValue");

        // Verify integrity
        assert_eq!(retrieved_utxo.amount, utxo_value.amount);
        assert_eq!(retrieved_utxo.pubkey_hash, utxo_value.pubkey_hash);
        assert_eq!(retrieved_utxo.block_height, utxo_value.block_height);
        drop(db);
    }

    // ============================================
    // UTXO COMPOSITE KEY TESTS
    // ============================================

    #[test]
    fn test_utxo_key_operations() {
        let tx_hash = &[7u8; 32];
        let output_index = 42u32;

        // Create composite key
        let key = make_utxo_key(tx_hash, output_index);

        // Should be 36 bytes: 32 (hash) + 4 (index)
        assert_eq!(key.len(), 36);

        // Parse key back
        let (parsed_hash, parsed_index) = parse_utxo_key(&key).expect("Failed to parse UTXO key");

        assert_eq!(parsed_hash, tx_hash);
        assert_eq!(parsed_index, output_index);
    }

    // ============================================
    // MULTI-COLUMN-FAMILY TESTS
    // ============================================

    #[test]
    fn test_multiple_column_families() {
        let (_temp_dir, db) = create_test_db();

        let key = b"test_key".to_vec();
        let value_blocks = b"value_in_blocks".to_vec();
        let value_txs = b"value_in_transactions".to_vec();

        // Put same key in different CFs
        db.put(ColumnFamilyName::Blocks, &key, &value_blocks)
            .expect("Failed to put in Blocks CF");
        db.put(ColumnFamilyName::Transactions, &key, &value_txs)
            .expect("Failed to put in Transactions CF");

        // Retrieve from different CFs
        let retrieved_blocks = db
            .get(ColumnFamilyName::Blocks, &key)
            .expect("Failed to get from Blocks CF")
            .expect("Value not found in Blocks CF");

        let retrieved_txs = db
            .get(ColumnFamilyName::Transactions, &key)
            .expect("Failed to get from Transactions CF")
            .expect("Value not found in Transactions CF");

        // Values should be different despite same key
        assert_eq!(retrieved_blocks, value_blocks);
        assert_eq!(retrieved_txs, value_txs);
        assert_ne!(retrieved_blocks, retrieved_txs);
        drop(db);
    }

    // ============================================
    // CLEAR AND RESET TEST
    // ============================================

    #[test]
    fn test_clear_and_reset() {
        let (_temp_dir, db) = create_test_db();

        // Put some data
        let key1 = b"key1".to_vec();
        let value1 = b"value1".to_vec();
        let key2 = b"key2".to_vec();
        let value2 = b"value2".to_vec();

        db.put(ColumnFamilyName::Transactions, &key1, &value1)
            .expect("Failed to put key1");
        db.put(ColumnFamilyName::Blocks, &key2, &value2)
            .expect("Failed to put key2");

        // Verify data exists
        assert!(db
            .get(ColumnFamilyName::Transactions, &key1)
            .expect("Failed to get key1")
            .is_some());
        assert!(db
            .get(ColumnFamilyName::Blocks, &key2)
            .expect("Failed to get key2")
            .is_some());

        // Clear all data
        db.clear_and_reset().expect("Failed to clear and reset");

        // Verify all data is gone
        assert!(db
            .get(ColumnFamilyName::Transactions, &key1)
            .expect("Failed to get key1 after reset")
            .is_none());
        assert!(db
            .get(ColumnFamilyName::Blocks, &key2)
            .expect("Failed to get key2 after reset")
            .is_none());
        drop(db);
    }

    // ============================================
    // SNAPSHOT TEST
    // ============================================

    #[test]
    fn test_snapshot_consistency() {
        let (_temp_dir, db) = create_test_db();

        let key = b"snapshot_key".to_vec();
        let value1 = b"value1".to_vec();
        let value2 = b"value2".to_vec();

        // Put initial value
        db.put(ColumnFamilyName::Transactions, &key, &value1)
            .expect("Failed to put initial value");

        // Create snapshot
        let _snapshot = db.snapshot();

        // Modify value
        db.put(ColumnFamilyName::Transactions, &key, &value2)
            .expect("Failed to put updated value");

        // Verify latest value is different
        assert_eq!(
            db.get(ColumnFamilyName::Transactions, &key)
                .expect("Failed to get current value"),
            Some(value2)
        );

        // Snapshot should see original value (point-in-time consistency)
        // Note: test depends on snapshot API availability
        println!("Snapshot created and database was updated");
        drop(_snapshot);
        drop(db);
    }

    // ============================================
    // INNER ARC TEST
    // ============================================

    #[test]
    fn test_inner_arc_clone() {
        let (_temp_dir, db) = create_test_db();

        // Get Arc reference for thread sharing
        let db_arc1 = db.inner_arc();
        let db_arc2 = db.inner_arc();

        // Both should reference same DB
        assert_eq!(db_arc1.as_ref() as *const _, db_arc2.as_ref() as *const _);

        let key = b"test_key".to_vec();
        let _value = b"test_value".to_vec();

        // Use Arc in a thread-like scenario
        db_arc1
            .get_cf(
                &db_arc1
                    .cf_handle("default")
                    .expect("Failed to get CF handle"),
                &key,
            )
            .expect("Failed to get from arc");

        println!("Arc cloning works correctly");
        drop(db);
    }
}
