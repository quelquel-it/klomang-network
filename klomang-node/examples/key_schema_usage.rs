// Example usage of KvStore with Key Schema Design and Performance Optimization
#![allow(dead_code)]

use klomang_node::storage::{
    StorageDb, StorageConfig, KvStore,
    BlockValue, HeaderValue, TransactionValue, TransactionInput, TransactionOutput,
    UtxoValue, UtxoSpentValue, VerkleStateValue, DagNodeValue, DagTipsValue,
};
use std::path::PathBuf;

/// Example: Configure RocksDB for High TPS Performance
pub fn example_performance_config() -> Result<(), Box<dyn std::error::Error>> {
    // Configure for high TPS with 2GB cache and optimized settings
    let config = StorageConfig::new("./high_tps_data")
        .with_block_cache_size(2 * 1024 * 1024 * 1024)  // 2GB LRU cache
        .with_block_size(32 * 1024)                     // 32KB blocks
        .with_bloom_bits_per_key(10)                    // Bloom filter precision
        .with_max_background_jobs(8)                    // More compaction threads
        .with_write_buffer_size(128 * 1024 * 1024);    // 128MB write buffer

    // Open database with performance optimizations
    let db = StorageDb::open_with_config(&config)?;
    let kv_store = KvStore::new(db);

    println!("Database opened with high TPS optimizations:");
    println!("- Block cache: {} GB", config.block_cache_size / (1024 * 1024 * 1024));
    println!("- Block size: {} KB", config.block_size / 1024);
    println!("- Bloom filter bits per key: {}", config.bloom_bits_per_key);

    Ok(())
}

/// Example: Store and retrieve a block
pub fn example_block_operations() -> Result<(), Box<dyn std::error::Error>> {
    let config = StorageConfig::new("./blockchain_data");
    let db = StorageDb::open_with_config(&config)?;
    let kv_store = KvStore::new(db);

    // Create a sample block
    let block_hash = b"block123456789abcdef0123456789ab";
    let block = BlockValue {
        hash: block_hash.to_vec(),
        header_bytes: b"header_data".to_vec(),
        transactions: vec![
            b"tx1".to_vec(),
            b"tx2".to_vec(),
        ],
        timestamp: 1704067200,
    };

    // Store the block
    kv_store.put_block(block_hash, &block)?;

    // Retrieve the block
    if let Some(retrieved_block) = kv_store.get_block(block_hash)? {
        println!("Retrieved block with {} transactions", retrieved_block.transactions.len());
    }

    // Delete the block
    kv_store.delete_block(block_hash)?;

    Ok(())
}

/// Example: UTXO management
pub fn example_utxo_operations() -> Result<(), Box<dyn std::error::Error>> {
    let config = StorageConfig::new("./blockchain_data");
    let db = StorageDb::open_with_config(&config)?;
    let kv_store = KvStore::new(db);

    let tx_hash = b"transaction_hash_________________________".to_vec();
    let output_index = 0u32;

    // Create and store a UTXO
    let utxo = UtxoValue::new(
        50_000_000,  // 50M satoshis
        b"script_code".to_vec(),
        b"owner_address".to_vec(),
        100,  // block height
    );

    kv_store.put_utxo(&tx_hash, output_index, &utxo)?;

    // Check if UTXO exists
    if kv_store.utxo_exists(&tx_hash, output_index)? {
        println!("UTXO exists!");

        // Retrieve UTXO
        if let Some(retrieved_utxo) = kv_store.get_utxo(&tx_hash, output_index)? {
            println!("UTXO amount: {}", retrieved_utxo.amount);

            // Mark UTXO as spent
            let spent = UtxoSpentValue::new(
                b"spending_tx_hash".to_vec(),
                0,  // input index
                101,  // spent at block height
            );
            kv_store.put_utxo_spent(&tx_hash, output_index, &spent)?;
        }
    }

    Ok(())
}

/// Example: DAG structure storage
pub fn example_dag_operations() -> Result<(), Box<dyn std::error::Error>> {
    let config = StorageConfig::new("./blockchain_data");
    let db = StorageDb::open_with_config(&config)?;
    let kv_store = KvStore::new(db);

    let block_hash = b"dag_block_hash__________________________".to_vec();

    // Create a DAG node
    let dag_node = DagNodeValue::new(
        block_hash.clone(),
        vec![b"parent1".to_vec(), b"parent2".to_vec()],  // parents
        vec![b"blue_child1".to_vec()],  // blue set
        vec![b"red_child1".to_vec()],   // red set
        42,  // blue score
    );

    // Store DAG node
    kv_store.put_dag_node(&block_hash, &dag_node)?;

    // Update DAG tips
    let tips = DagTipsValue::new(
        vec![block_hash.clone()],
        100,  // last updated height
    );
    kv_store.put_current_dag_tips(&tips)?;

    // Retrieve tips
    if let Some(current_tips) = kv_store.get_current_dag_tips()? {
        println!("Current DAG has {} tip blocks", current_tips.tip_blocks.len());
    }

    Ok(())
}

/// Example: Verkle state storage
pub fn example_verkle_operations() -> Result<(), Box<dyn std::error::Error>> {
    let config = StorageConfig::new("./blockchain_data");
    let db = StorageDb::open_with_config(&config)?;
    let kv_store = KvStore::new(db);

    // Create and store Verkle tree nodes
    let path = b"verkle_path_123456789abcdef0123456789";
    let state = VerkleStateValue::new(
        path.to_vec(),
        b"node_commitment_value".to_vec(),
        true,  // is_leaf
    );

    kv_store.put_verkle_state(path, &state)?;

    // Retrieve Verkle state
    if let Some(retrieved_state) = kv_store.get_verkle_state(path)? {
        println!("Verkle state is_leaf: {}", retrieved_state.is_leaf);
    }

    Ok(())
}

/// Example: Transaction storage
pub fn example_transaction_operations() -> Result<(), Box<dyn std::error::Error>> {
    let config = StorageConfig::new("./blockchain_data");
    let db = StorageDb::open_with_config(&config)?;
    let kv_store = KvStore::new(db);

    let tx_hash = b"tx_hash_32bytes__________________________";

    // Create a transaction
    let tx = TransactionValue {
        tx_hash: tx_hash.to_vec(),
        inputs: vec![
            TransactionInput {
                previous_tx_hash: b"prev_tx1_32bytes_____________________".to_vec(),
                output_index: 0,
            },
        ],
        outputs: vec![
            TransactionOutput {
                amount: 25_000_000,
                script: b"output_script_1".to_vec(),
                owner: b"address_1".to_vec(),
            },
            TransactionOutput {
                amount: 24_990_000,
                script: b"output_script_2".to_vec(),
                owner: b"address_2".to_vec(),
            },
        ],
        fee: 10_000,
    };

    // Store transaction
    kv_store.put_transaction(tx_hash, &tx)?;

    // Retrieve transaction
    if let Some(retrieved_tx) = kv_store.get_transaction(tx_hash)? {
        println!("Transaction has {} inputs and {} outputs",
                 retrieved_tx.inputs.len(),
                 retrieved_tx.outputs.len());
    }

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Klomang Key Schema Design and Performance Optimization Example!");

    example_performance_config()?;
    example_block_operations()?;
    example_utxo_operations()?;
    example_dag_operations()?;
    example_verkle_operations()?;
    example_transaction_operations()?;

    println!("All examples completed successfully!");
    Ok(())
}
