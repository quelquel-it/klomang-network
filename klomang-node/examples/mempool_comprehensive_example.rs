//! Complete Mempool System Usage Example
//!
//! Demonstrates the full workflow of transaction pool management including:
//! - Adding transactions to the pool
//! - Validating transactions
//! - Selecting transactions for block building
//! - Handling new blocks with revalidation
//! - Managing memory with eviction

use klomang_core::core::crypto::Hash;
use klomang_core::core::state::transaction::{Transaction, TxInput, TxOutput};
use std::sync::Arc;

use klomang_node::mempool::{
    TransactionPool, PoolConfig, DeterministicSelector, SelectionStrategy,
    EvictionEngine, EvictionPolicy, MempoolPressure, TransactionStatus,
};

/// Example: Basic transaction pool operations
pub fn example_basic_pool_operations() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Basic Transaction Pool Operations ===\n");

    // Create pool with custom configuration
    let config = PoolConfig {
        max_pool_size: 100,
        orphan_ttl_seconds: 600,
        rejected_ttl_seconds: 3600,
    };
    let pool = Arc::new(TransactionPool::new(config));

    // Create and add a transaction
    let tx = Transaction {
        id: Hash::new(&[1u8; 32]),
        inputs: vec![TxInput {
            prev_tx: Hash::new(&[0u8; 32]),
            index: 0,
        }],
        outputs: vec![TxOutput {
            amount: 100,
            script: vec![0x51],
        }],
        execution_payload: vec![],
        contract_address: None,
        gas_limit: 0,
        max_fee_per_gas: 0,
        chain_id: 1,
        locktime: 0,
    };

    // Add transaction with fee and size metadata
    pool.add_transaction(tx.clone(), 1000, 250)?;

    // Get pool statistics
    let stats = pool.get_stats();
    println!("Pool stats after adding 1 transaction:");
    println!("  - Total transactions: {}", stats.total_count);
    println!("  - Pending: {}", stats.pending_count);
    println!("  - Total fees: {}", stats.total_fees);
    println!("  - Total size: {} bytes\n", stats.total_size_bytes);

    Ok(())
}

/// Example: Deterministic transaction selection
pub fn example_deterministic_selection() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Deterministic Transaction Selection ===\n");

    let pool = Arc::new(TransactionPool::new(PoolConfig::default()));

    // Add multiple transactions with different fees
    for i in 1..=5 {
        let tx = Transaction {
            id: Hash::new(&[i as u8; 32]),
            inputs: vec![],
            outputs: vec![],
            execution_payload: vec![],
            contract_address: None,
            gas_limit: 0,
            max_fee_per_gas: 0,
            chain_id: 1,
            locktime: 0,
        };
        let fee = i as u64 * 1000;
        pool.add_transaction(tx, fee, 250)?;
    }

    // Select using HighestFee strategy (default)
    let selector_fee = DeterministicSelector::new(SelectionStrategy::HighestFee);
    let selected_fee = selector_fee.select_transactions(&pool, 3, None)?;
    println!("HighestFee selection (selecting 3 of 5):");
    for (idx, entry) in selected_fee.iter().enumerate() {
        println!("  {} - Fee: {}, Size: {} bytes", idx + 1, entry.total_fee, entry.size_bytes);
    }
    println!();

    // Select using FIFO strategy
    let selector_fifo = DeterministicSelector::new(SelectionStrategy::FIFO);
    let selected_fifo = selector_fifo.select_transactions(&pool, 3, None)?;
    println!("FIFO selection (selecting 3 of 5):");
    for (idx, entry) in selected_fifo.iter().enumerate() {
        println!("  {} - Fee: {}, Size: {} bytes", idx + 1, entry.total_fee, entry.size_bytes);
    }
    println!();

    Ok(())
}

/// Example: Memory management and eviction
pub fn example_memory_eviction() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Memory Management and Eviction ===\n");

    let pool = Arc::new(TransactionPool::new(PoolConfig::default()));

    // Fill pool with transactions
    println!("Adding 50 transactions to pool...");
    for i in 1..=50 {
        let tx = Transaction {
            id: Hash::new(&[(i as u8) % 256; 32]),
            inputs: vec![],
            outputs: vec![],
            execution_payload: vec![],
            contract_address: None,
            gas_limit: 0,
            max_fee_per_gas: 0,
            chain_id: 1,
            locktime: 0,
        };
        let fee = ((i % 10) + 1) as u64 * 100;
        pool.add_transaction(tx, fee, 250)?;
    }

    // Create eviction policy
    let policy = EvictionPolicy {
        max_transaction_count: 100,
        max_memory_bytes: 100 * 1024 * 1024,
        batch_size: 10,
    };

    // Check mempool pressure
    let pressure = MempoolPressure::calculate(&pool, &policy);
    println!("Mempool Pressure:");
    println!("  - Transaction pressure: {:.1}%", pressure.transaction_pressure * 100.0);
    println!("  - Memory pressure: {:.1}%", pressure.memory_pressure * 100.0);
    println!("  - Total pressure: {:.1}%\n", pressure.total_pressure * 100.0);

    // Initialize eviction engine
    let engine = EvictionEngine::new(pool.clone(), policy);

    // Check if eviction is needed
    println!("Eviction needed: {}\n", engine.need_eviction());

    // Analyze eviction order (which transactions would be evicted first)
    let eviction_order = engine.analyze_eviction_order();
    println!("Eviction order analysis (lower scores evicted first):");
    for (idx, (_hash, score)) in eviction_order.iter().enumerate().take(5) {
        println!("  {} - Priority score: {}", idx + 1, score);
    }
    println!();

    Ok(())
}

/// Example: Status management and lifecycle
pub fn example_transaction_lifecycle() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Transaction Lifecycle ===\n");

    let pool = Arc::new(TransactionPool::new(PoolConfig::default()));

    let tx = Transaction {
        id: Hash::new(&[42u8; 32]),
        inputs: vec![],
        outputs: vec![],
        execution_payload: vec![],
        contract_address: None,
        gas_limit: 0,
        max_fee_per_gas: 0,
        chain_id: 1,
        locktime: 0,
    };

    let tx_hash = bincode::serialize(&tx.id)?;

    // Add transaction
    pool.add_transaction(tx, 500, 250)?;
    let entry = pool.get_by_hash(&tx_hash).unwrap();
    println!("1. Transaction created - Status: {:?}", entry.status);

    // Transition to Validated
    pool.set_status(&tx_hash, TransactionStatus::Validated)?;
    let entry = pool.get_by_hash(&tx_hash).unwrap();
    println!("2. After validation - Status: {:?}", entry.status);

    // Transition to InBlock
    pool.set_status(&tx_hash, TransactionStatus::InBlock)?;
    let entry = pool.get_by_hash(&tx_hash).unwrap();
    println!("3. After block inclusion - Status: {:?}\n", entry.status);

    Ok(())
}

/// Example: Adaptive eviction under different loads
pub fn example_adaptive_eviction() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Adaptive Eviction Example ===\n");

    let pool = Arc::new(TransactionPool::new(PoolConfig::default()));

    // Add transactions
    for i in 1..=30 {
        let tx = Transaction {
            id: Hash::new(&[(i as u8); 32]),
            inputs: vec![],
            outputs: vec![],
            execution_payload: vec![],
            contract_address: None,
            gas_limit: 0,
            max_fee_per_gas: 0,
            chain_id: 1,
            locktime: 0,
        };
        pool.add_transaction(tx, 100, 250)?;
    }

    let policy = EvictionPolicy {
        max_transaction_count: 100,
        max_memory_bytes: 100 * 1024 * 1024,
        batch_size: 10,
    };

    let engine = EvictionEngine::new(pool, policy);

    // Simulate low pressure (evicts normal amount)
    println!("Low pressure (20% full) - normal eviction");
    let result_low = engine.adaptive_eviction(0.2)?;
    println!("  - Evicted: {} transactions", result_low.evicted_count);
    println!("  - Freed: {} bytes", result_low.evicted_bytes);
    println!("  - Fees from evicted: {}\n", result_low.total_evicted_fees);

    Ok(())
}

/// Main execution
#[allow(dead_code)]
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n╔════════════════════════════════════════════════════════════════╗");
    println!("║          Klomang Mempool System - Complete Examples            ║");
    println!("╚════════════════════════════════════════════════════════════════╝\n");

    example_basic_pool_operations()?;
    println!("─────────────────────────────────────────────────────────────\n");

    example_deterministic_selection()?;
    println!("─────────────────────────────────────────────────────────────\n");

    example_transaction_lifecycle()?;
    println!("─────────────────────────────────────────────────────────────\n");

    example_memory_eviction()?;
    println!("─────────────────────────────────────────────────────────────\n");

    example_adaptive_eviction()?;

    println!("═════════════════════════════════════════════════════════════════");
    println!("All examples completed successfully!");
    println!("═════════════════════════════════════════════════════════════════\n");

    Ok(())
}
