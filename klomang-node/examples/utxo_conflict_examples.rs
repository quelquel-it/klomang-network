//! UTXO Conflict Management Usage Examples
//!
//! Demonstrates practical scenarios for UTXO ownership management and conflict resolution

use klomang_core::core::crypto::Hash;
use klomang_core::core::state::transaction::{SigHashType, Transaction, TxInput};
use std::sync::Arc;

use klomang_node::mempool::{PoolConfig, TransactionPool, UtxoOwnershipManager};
use klomang_node::storage::kv_store::KvStore;

/// Create a test transaction with specified inputs
fn create_test_tx(id: u8, prev_tx_seeds: Vec<u8>) -> Transaction {
    let inputs = prev_tx_seeds
        .iter()
        .enumerate()
        .map(|(idx, seed)| TxInput {
            prev_tx: Hash::new(&[*seed; 32]),
            index: idx as u32,
            signature: vec![],
            pubkey: vec![],
            sighash_type: SigHashType::All,
        })
        .collect();

    Transaction {
        id: Hash::new(&[id; 32]),
        inputs,
        outputs: vec![],
        execution_payload: vec![],
        contract_address: None,
        gas_limit: 0,
        max_fee_per_gas: 0,
        chain_id: 1,
        locktime: 0,
    }
}

/// Example 1: Basic transaction addition with ownership tracking
pub fn example_basic_ownership_tracking() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Example 1: Basic Ownership Tracking ===\n");

    let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
    let kv_store = Arc::new(KvStore::new_dummy());
    let manager = UtxoOwnershipManager::new(pool, kv_store);

    // Create transaction that spends two UTXOs
    let tx = create_test_tx(1, vec![10, 11]);
    let fee = 1000;
    let size = 250;

    // Add transaction with ownership tracking
    let result = manager.add_transaction_with_ownership(tx.clone(), fee, size)?;

    println!("Transaction added successfully!");
    println!("  - Added: {}", result.added);
    println!("  - Claimed outpoints: {}", result.claimed_outpoints.len());
    for (idx, outpoint) in result.claimed_outpoints.iter().enumerate() {
        println!("    {} - {}", idx + 1, outpoint);
    }
    println!();

    // Check stats
    let stats = manager.get_conflict_stats();
    println!("Conflict tracker stats:");
    println!("  - Total tracked: {}", stats.total_tracked);
    println!("  - Total claims: {}", stats.total_claims);
    println!("  - RBF replacements: {}", stats.rbf_replacements);
    println!();

    Ok(())
}

/// Example 2: Conflict detection and rejection
pub fn example_conflict_detection() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Example 2: Conflict Detection & Rejection ===\n");

    let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
    let kv_store = Arc::new(KvStore::new_dummy());
    let manager = UtxoOwnershipManager::new(pool, kv_store);

    // Create first transaction spending UTXO from tx:10
    let tx1 = create_test_tx(1, vec![10]);
    let fee1 = 1000;

    // Add first transaction
    let result1 = manager.add_transaction_with_ownership(tx1.clone(), fee1, 250)?;
    println!("TX-1 added successfully");
    println!("  - Claimed outpoints: {:#?}\n", result1.claimed_outpoints);

    // Create second transaction trying to spend the SAME UTXO
    let tx2 = create_test_tx(2, vec![10]); // Same prev_tx as tx1
    let fee2 = 500; // LOWER fee

    // Try to add second transaction
    match manager.add_transaction_with_ownership(tx2.clone(), fee2, 250) {
        Ok(_) => println!("❌ ERROR: TX-2 should have been rejected!"),
        Err(e) => {
            println!("✅ TX-2 correctly rejected!");
            println!("  Reason: {:?}\n", e);
        }
    }

    // Check tracker state
    let claims = manager.get_transaction_claims(&bincode::serialize(&tx1.id).unwrap());
    println!(
        "TX-1 claims are still active: {} outpoints claimed",
        claims.len()
    );
    println!();

    Ok(())
}

/// Example 3: Replace-By-Fee (RBF) replacement
pub fn example_rbf_replacement() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Example 3: Replace-By-Fee Replacement ===\n");

    let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
    let kv_store = Arc::new(KvStore::new_dummy());
    let manager = UtxoOwnershipManager::new(pool, kv_store);

    // Create first transaction with low fee
    let tx1 = create_test_tx(1, vec![20]);
    let fee1 = 500; // Low fee

    let result1 = manager.add_transaction_with_ownership(tx1.clone(), fee1, 250)?;
    println!("Step 1: TX-1 (low fee) added");
    println!("  - Fee: {} sat/byte", fee1 / 250);
    println!(
        "  - Claimed: {} outpoints\n",
        result1.claimed_outpoints.len()
    );

    // Create second transaction trying to spend the SAME UTXO with HIGHER FEE
    let tx2 = create_test_tx(2, vec![20]); // Same prev_tx as tx1
    let fee2 = 2000; // HIGHER fee

    // Try to add second transaction (should trigger RBF)
    match manager.add_transaction_with_ownership(tx2.clone(), fee2, 250) {
        Ok(result2) => {
            println!("✅ TX-2 (higher fee) accepted!");
            println!("  - Fee: {} sat/byte", fee2 / 250);
            println!(
                "  - RBF replacements: {} (TX-1 removed)",
                result2.rbf_replacements
            );
            println!(
                "  - Claimed: {} outpoints\n",
                result2.claimed_outpoints.len()
            );
        }
        Err(e) => {
            println!("❌ ERROR: TX-2 with higher fee should have replaced TX-1!");
            println!("  Error: {:?}\n", e);
        }
    }

    // Check tracker stats
    let stats = manager.get_conflict_stats();
    println!("Final stats:");
    println!("  - RBF replacements: {}", stats.rbf_replacements);
    println!();

    Ok(())
}

/// Example 4: Transaction removal and claim cleanup
pub fn example_transaction_removal() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Example 4: Transaction Removal & Cleanup ===\n");

    let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
    let kv_store = Arc::new(KvStore::new_dummy());
    let manager = UtxoOwnershipManager::new(pool, kv_store);

    // Create and add transaction
    let tx = create_test_tx(1, vec![30, 31, 32]);
    let tx_hash = bincode::serialize(&tx.id)?;

    let result = manager.add_transaction_with_ownership(tx.clone(), 1000, 250)?;
    println!(
        "Transaction added with {} outpoint claims",
        result.claimed_outpoints.len()
    );

    // Get transaction info before removal
    let claims_before = manager.get_transaction_claims(&tx_hash);
    println!("Before removal: {} claims held", claims_before.len());
    println!("  Claims: {:#?}\n", claims_before);

    // Remove transaction
    let removal_info = manager.remove_transaction(&tx_hash)?;
    println!("Transaction removed!");
    println!("  - Found: {}", removal_info.found);
    println!(
        "  - Released outpoints: {}",
        removal_info.released_outpoints
    );
    println!();

    // Try to add a new transaction using the same UTXO (should now succeed)
    let tx2 = create_test_tx(2, vec![30]);
    match manager.add_transaction_with_ownership(tx2, 1000, 250) {
        Ok(_) => println!("✅ New TX-2 can now claim the released UTXO\n"),
        Err(e) => println!("❌ ERROR: TX-2 should be able to claim: {:?}\n", e),
    }

    Ok(())
}

/// Example 5: Conflict analysis and diagnostics
pub fn example_conflict_analysis() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Example 5: Conflict Analysis & Diagnostics ===\n");

    let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
    let kv_store = Arc::new(KvStore::new_dummy());
    let manager = UtxoOwnershipManager::new(pool, kv_store);

    // Add several transactions
    println!("Adding 5 transactions...");
    for i in 1..=5 {
        let tx = create_test_tx(i as u8, vec![i as u8 + 40]);
        manager
            .add_transaction_with_ownership(tx, 1000 + (i as u64 * 100), 250)
            .ok();
    }
    println!();

    // Analyze conflicts
    let analysis = manager.analyze_conflicts()?;
    println!("Conflict Analysis Results:");
    println!(
        "  - Total transactions in pool: {}",
        analysis.total_transactions
    );
    println!(
        "  - Transactions with UTXO claims: {}",
        analysis.transactions_with_claims
    );
    println!(
        "  - Unique outpoints claimed: {}",
        analysis.unique_outpoints_claimed
    );
    println!(
        "  - RBF replacements (lifetime): {}",
        analysis.rbf_replacements_lifetime
    );
    println!(
        "  - Conflicts detected (lifetime): {}",
        analysis.total_conflicts_detected
    );
    println!();

    // Get tracker stats
    let stats = manager.get_conflict_stats();
    println!("Tracker Statistics:");
    println!("  - Total tracked: {}", stats.total_tracked);
    println!("  - Total claims registered: {}", stats.total_claims);
    println!("  - Claims released: {}", stats.claims_released);
    println!();

    Ok(())
}

/// Example 6: Multiple conflict scenarios
pub fn example_multiple_conflicts() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Example 6: Multiple Conflict Scenarios ===\n");

    let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
    let kv_store = Arc::new(KvStore::new_dummy());
    let manager = UtxoOwnershipManager::new(pool, kv_store);

    // Scenario A: Non-conflicting transactions
    println!("Scenario A: Non-conflicting transactions");
    let tx_a1 = create_test_tx(1, vec![50]); // Uses prev_tx:50
    let tx_a2 = create_test_tx(2, vec![51]); // Uses prev_tx:51

    manager.add_transaction_with_ownership(tx_a1.clone(), 1000, 250)?;
    match manager.add_transaction_with_ownership(tx_a2, 1000, 250) {
        Ok(_) => println!("✅ Both transactions accepted (no conflict)\n"),
        Err(e) => println!("❌ ERROR: {:?} \n", e),
    }

    // Scenario B: Conflict with lower fee rejection
    println!("Scenario B: Conflict with lower fee (rejection)");
    let tx_b1 = create_test_tx(3, vec![52]);
    let tx_b2 = create_test_tx(4, vec![52]); // Same input

    manager.add_transaction_with_ownership(tx_b1.clone(), 2000, 250)?;
    println!("  TX-1 added (fee: 2000)");

    match manager.add_transaction_with_ownership(tx_b2, 1000, 250) {
        Ok(_) => println!("❌ ERROR: Should have rejected\n"),
        Err(_) => println!("✅ TX-2 rejected (fee: 1000 < 2000)\n"),
    }

    // Scenario C: Conflict with higher fee replacement
    println!("Scenario C: Conflict with higher fee (replacement)");
    let tx_c1 = create_test_tx(5, vec![53]);
    let tx_c2 = create_test_tx(6, vec![53]); // Same input

    manager.add_transaction_with_ownership(tx_c1.clone(), 1000, 250)?;
    println!("  TX-1 added (fee: 1000)");

    match manager.add_transaction_with_ownership(tx_c2, 3000, 250) {
        Ok(info) => {
            println!("✅ TX-2 accepted (fee: 3000 > 1000)");
            println!("  RBF replacements: {}\n", info.rbf_replacements);
        }
        Err(e) => println!("❌ ERROR: {}\n", e),
    }

    Ok(())
}

/// Example 7: Blockchain synchronization
pub fn example_blockchain_sync() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Example 7: Blockchain Synchronization ===\n");

    let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
    let kv_store = Arc::new(KvStore::new_dummy());
    let manager = UtxoOwnershipManager::new(pool, kv_store);

    // Add transactions to mempool
    println!("Step 1: Add transactions to mempool");
    let tx1 = create_test_tx(1, vec![60]);
    let tx2 = create_test_tx(2, vec![61]);
    let tx3 = create_test_tx(3, vec![62]);

    let hash1 = bincode::serialize(&tx1.id)?;
    let hash2 = bincode::serialize(&tx2.id)?;
    let hash3 = bincode::serialize(&tx3.id)?;

    manager.add_transaction_with_ownership(tx1, 1000, 250).ok();
    manager.add_transaction_with_ownership(tx2, 1000, 250).ok();
    manager.add_transaction_with_ownership(tx3, 1000, 250).ok();

    println!("  3 transactions added to mempool\n");

    // Simulate new block with some of these transactions
    println!("Step 2: Simulate new block arrival");
    let block_txs = vec![hash1.clone(), hash3.clone()];
    println!("  Block contains: 2 transactions\n");

    // Synchronize with new block
    println!("Step 3: Synchronize mempool with block");
    let released = manager.sync_with_new_block(&block_txs)?;
    println!("  Released {} transaction claims", released);
    println!("  TX-1 and TX-3 are now on-chain");
    println!("  TX-2 remains in mempool\n");

    // Check remaining claims
    println!("Step 4: Verify remaining transactions");
    let remaining_claims = manager.get_transaction_claims(&hash2);
    println!("  TX-2 still claims {} outpoints", remaining_claims.len());
    println!();

    Ok(())
}

/// Main execution
#[allow(dead_code)]
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║    UTXO Conflict Management - Practical Examples              ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    example_basic_ownership_tracking()?;
    println!("{}", "─".repeat(70));
    println!();

    example_conflict_detection()?;
    println!("{}", "─".repeat(70));
    println!();

    example_rbf_replacement()?;
    println!("{}", "─".repeat(70));
    println!();

    example_transaction_removal()?;
    println!("{}", "─".repeat(70));
    println!();

    example_conflict_analysis()?;
    println!("{}", "─".repeat(70));
    println!();

    example_multiple_conflicts()?;
    println!("{}", "─".repeat(70));
    println!();

    example_blockchain_sync()?;

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║        All examples completed successfully!                   ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    Ok(())
}
