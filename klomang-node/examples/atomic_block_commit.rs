// Example: Atomic block commitment with Write Path + klomang-core integration
#![allow(dead_code)]

use std::sync::Arc;

use klomang_node::storage::{
    BlockTransactionBatch, SpentUtxoBatch, KvStore, StorageDb, StorageConfig, StorageCacheLayer,
};

use klomang_node::storage::schema::{
    BlockValue, HeaderValue, TransactionValue, TransactionInput, TransactionOutput,
    UtxoValue, UtxoSpentValue, DagNodeValue, DagTipsValue,
};

use klomang_node::storage::metrics::StorageMetrics;
use klomang_core::NoOpMetricsCollector;

/// Example: Demonstrate atomic block commitment with error handling
pub fn example_atomic_block_commitment() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize storage
    let config = StorageConfig::new("./atomic_block_data")
        .with_block_cache_size(2 * 1024 * 1024 * 1024);

    let db = StorageDb::open_with_config(&config, Arc::new(StorageMetrics::new(Box::new(NoOpMetricsCollector))))?;
    let cache_layer = Arc::new(StorageCacheLayer::new(db));
    let kv_store = KvStore::new(cache_layer);

    // === Block Setup ===
    let block_hash = b"block_001_hash_____________________________";
    let block_height = 1u32;

    // Create block value
    let block_value = BlockValue {
        hash: block_hash.to_vec(),
        header_bytes: b"block_header_data".to_vec(),
        transactions: vec![
            b"tx_hash_1".to_vec(),
            b"tx_hash_2".to_vec(),
        ],
        timestamp: 1704067200,
    };

    // Create header value
    let header_value = HeaderValue {
        block_hash: block_hash.to_vec(),
        parent_hashes: vec![b"genesis_hash____________________________".to_vec()],
        timestamp: 1704067200,
        difficulty: 1000000,
        nonce: 12345,
        verkle_root: b"verkle_root_hash_".to_vec(),
        height: block_height as u64,
    };

    // === Transaction 1: Spend one UTXO, create two new ones ===
    let tx1_hash = b"tx_hash_1_______________________________";

    let tx1_spent_utxo = SpentUtxoBatch {
        prev_tx_hash: b"previous_tx_hash_______________________".to_vec(),
        output_index: 0,
        spent_value: UtxoSpentValue::new(
            tx1_hash.to_vec(),
            0, // input_index in tx1
            block_height,
        ),
    };

    let tx1_value = TransactionValue {
        tx_hash: tx1_hash.to_vec(),
        inputs: vec![TransactionInput {
            previous_tx_hash: tx1_spent_utxo.prev_tx_hash.clone(),
            output_index: tx1_spent_utxo.output_index,
        }],
        outputs: vec![
            TransactionOutput {
                amount: 50_000_000,
                pubkey_hash: b"address_1".to_vec(),
            },
            TransactionOutput {
                amount: 49_900_000,
                pubkey_hash: b"address_2".to_vec(),
            },
        ],
        fee: 100_000,
    };

    let tx1_batch = BlockTransactionBatch {
        tx_hash: tx1_hash.to_vec(),
        tx_value: tx1_value,
        spent_utxos: vec![tx1_spent_utxo],
        new_utxos: vec![
            UtxoValue::new(50_000_000, b"script_1".to_vec(), b"owner_1".to_vec(), block_height),
            UtxoValue::new(49_900_000, b"script_2".to_vec(), b"owner_2".to_vec(), block_height),
        ],
    };

    // === Transaction 2: Spend two UTXOs, create one new one ===
    let tx2_hash = b"tx_hash_2_______________________________";

    let tx2_spent_utxos = vec![
        SpentUtxoBatch {
            prev_tx_hash: b"previous_tx_hash_______________________".to_vec(),
            output_index: 1,
            spent_value: UtxoSpentValue::new(
                tx2_hash.to_vec(),
                0, // input_index
                block_height,
            ),
        },
        SpentUtxoBatch {
            prev_tx_hash: b"another_tx_hash________________________".to_vec(),
            output_index: 0,
            spent_value: UtxoSpentValue::new(
                tx2_hash.to_vec(),
                1, // second input_index
                block_height,
            ),
        },
    ];

    let tx2_value = TransactionValue {
        tx_hash: tx2_hash.to_vec(),
        inputs: vec![
            TransactionInput {
                previous_tx_hash: b"previous_tx_hash_______________________".to_vec(),
                output_index: 1,
            },
            TransactionInput {
                previous_tx_hash: b"another_tx_hash________________________".to_vec(),
                output_index: 0,
            },
        ],
        outputs: vec![TransactionOutput {
            amount: 99_000_000,
            pubkey_hash: b"address_merged".to_vec(),
        }],
        fee: 900_000,
    };

    let tx2_batch = BlockTransactionBatch {
        tx_hash: tx2_hash.to_vec(),
        tx_value: tx2_value,
        spent_utxos: tx2_spent_utxos,
        new_utxos: vec![
            UtxoValue::new(99_000_000, b"script_merged".to_vec(), b"owner_merged".to_vec(), block_height),
        ],
    };

    // === DAG Information ===
    let dag_node = DagNodeValue::new(
        block_hash.to_vec(),
        vec![b"genesis_hash____________________________".to_vec()],
        vec![block_hash.to_vec()],
        vec![],
        1, // blue_score
    );

    let dag_tips = DagTipsValue::new(
        vec![block_hash.to_vec()],
        block_height,
    );

    // === Atomic Commit ===
    println!("Committing block {} atomically...", String::from_utf8_lossy(block_hash));

    kv_store.commit_block_atomic(
        block_hash,
        &block_value,
        &header_value,
        vec![tx1_batch, tx2_batch],
        &dag_node,
        &dag_tips,
    )?;

    println!("✓ Block committed successfully");
    println!("  - Block stored");
    println!("  - 2 transactions stored");
    println!("  - 1 spent UTXO (tx1 input) + 2 spent UTXOs (tx2 inputs) = 3 total marked as spent");
    println!("  - 3 new UTXOs created (2 from tx1, 1 from tx2)");
    println!("  - DAG structure updated");

    // === Verify Stored Data ===
    if let Some(stored_tx1) = kv_store.get_transaction(tx1_hash)? {
        println!("\n✓ Verified Transaction 1 stored:");
        println!("  - Inputs: {}", stored_tx1.inputs.len());
        println!("  - Outputs: {}", stored_tx1.outputs.len());
    }

    if let Some(stored_utxo) = kv_store.get_utxo(tx1_hash, 0)? {
        println!("\n✓ Verified UTXO (tx1, output 0) created:");
        println!("  - Amount: {}", stored_utxo.amount);
        println!("  - Block height: {}", stored_utxo.block_height);
    }

    println!("\n✓ Atomic block commitment completed successfully");
    Ok(())
}

/// Example: Error handling - serialization failure prevents commit
pub fn example_error_handling() -> Result<(), Box<dyn std::error::Error>> {
    let config = StorageConfig::new("./error_handling_data");
    let db = StorageDb::open_with_config(&config, Arc::new(StorageMetrics::new(Box::new(NoOpMetricsCollector))))?;
    let cache_layer = Arc::new(StorageCacheLayer::new(db));
    let kv_store = KvStore::new(cache_layer);

    let block_hash = b"error_test_block________________________";

    // Create valid block data
    let block_value = BlockValue {
        hash: block_hash.to_vec(),
        header_bytes: b"header".to_vec(),
        transactions: vec![],
        timestamp: 1704067200,
    };

    let header_value = HeaderValue {
        block_hash: block_hash.to_vec(),
        parent_hashes: vec![],
        timestamp: 1704067200,
        difficulty: 1000000,
        nonce: 12345,
        verkle_root: b"root".to_vec(),
        height: 1,
    };

    let dag_node = DagNodeValue::new(block_hash.to_vec(), vec![], vec![], vec![], 0);
    let dag_tips = DagTipsValue::new(vec![block_hash.to_vec()], 1);

    // Note: If serialization failed for any data structure in the batch,
    // the error would be caught before any writes, and the entire batch
    // would be rejected atomically.

    match kv_store.commit_block_atomic(
        block_hash,
        &block_value,
        &header_value,
        vec![], // empty transactions
        &dag_node,
        &dag_tips,
    ) {
        Ok(_) => println!("✓ Block committed"),
        Err(e) => println!("✗ Error: {}", e),
    }

    Ok(())
}

/// Example: Multi-transaction block with complex UTXO scenario
pub fn example_complex_block_scenario() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== Complex Block Scenario ===\n");

    let config = StorageConfig::new("./complex_scenario_data");
    let db = StorageDb::open_with_config(&config, Arc::new(StorageMetrics::new(Box::new(NoOpMetricsCollector))))?;
    let cache_layer = Arc::new(StorageCacheLayer::new(db));
    let kv_store = KvStore::new(cache_layer);

    let block_hash = b"complex_block_hash_______________________";
    let block_height = 100u32;

    // Block with multiple transactions
    let mut transactions = Vec::new();

    // Create several transactions with different UTXO patterns
    for tx_idx in 0..5 {
        let tx_hash = format!("tx_{:03}_hash____________________________", tx_idx)
            .as_bytes()
            .to_vec();
        let mut tx_hash_arr = [0u8; 32];
        tx_hash_arr[..tx_hash.len().min(32)].copy_from_slice(&tx_hash[..tx_hash.len().min(32)]);

        // Each transaction spends 1-3 UTXOs and creates 2-4 new ones
        let spent_count = 1 + (tx_idx % 3);
        let new_count = 2 + (tx_idx % 3);

        let mut spent = Vec::new();
        for i in 0..spent_count {
            spent.push(SpentUtxoBatch {
                prev_tx_hash: format!("prev_{}_tx__________________________", i)
                    .as_bytes()
                    .to_vec(),
                output_index: i as u32,
                spent_value: UtxoSpentValue::new(
                    tx_hash_arr.to_vec(),
                    i as u32,
                    block_height,
                ),
            });
        }

        let mut new = Vec::new();
        let base_amount = 1_000_000 * (tx_idx as u64 + 1);
        for i in 0..new_count {
            new.push(UtxoValue::new(
                base_amount + (i as u64 * 100_000),
                b"script".to_vec(),
                b"owner".to_vec(),
                block_height,
            ));
        }

        let tx_value = TransactionValue {
            tx_hash: tx_hash_arr.to_vec(),
            inputs: (0..spent_count).map(|i| TransactionInput {
                previous_tx_hash: format!("prev_{}_tx__________________________", i)
                    .as_bytes()
                    .to_vec(),
                output_index: i as u32,
            }).collect(),
            outputs: (0..new_count).map(|i| TransactionOutput {
                amount: base_amount + (i as u64 * 100_000),
                pubkey_hash: format!("owner_{}", i).as_bytes().to_vec(),
            }).collect(),
            fee: 100_000,
        };

        transactions.push(BlockTransactionBatch {
            tx_hash: tx_hash_arr.to_vec(),
            tx_value,
            spent_utxos: spent,
            new_utxos: new,
        });
    }

    // Prepare block and DAG data
    let block_value = BlockValue {
        hash: block_hash.to_vec(),
        header_bytes: b"header".to_vec(),
        transactions: transactions.iter().map(|t| t.tx_hash.clone()).collect(),
        timestamp: 1704067200,
    };

    let header_value = HeaderValue {
        block_hash: block_hash.to_vec(),
        parent_hashes: vec![],
        timestamp: 1704067200,
        difficulty: 1000000,
        nonce: 12345,
        verkle_root: b"root".to_vec(),
        height: block_height as u64,
    };

    let dag_node = DagNodeValue::new(
        block_hash.to_vec(),
        vec![],
        vec![block_hash.to_vec()],
        vec![],
        100,
    );

    let dag_tips = DagTipsValue::new(vec![block_hash.to_vec()], block_height);

    // Commit atomically
    let transaction_count = transactions.len();
    let spent_count: usize = transactions.iter().map(|t| t.spent_utxos.len()).sum();
    let new_count: usize = transactions.iter().map(|t| t.new_utxos.len()).sum();

    kv_store.commit_block_atomic(
        block_hash,
        &block_value,
        &header_value,
        transactions,
        &dag_node,
        &dag_tips,
    )?;

    println!("✓ Complex block committed atomically");
    println!("  - {} transactions stored", transaction_count);
    println!("  - {} UTXOs marked as spent", spent_count);
    println!("  - {} new UTXOs created", new_count);
    println!("  - DAG structure updated");

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Klomang Atomic Write Path Examples ===\n");

    example_atomic_block_commitment()?;
    example_error_handling()?;
    example_complex_block_scenario()?;

    println!("\n=== All examples completed successfully ===");
    Ok(())
}
