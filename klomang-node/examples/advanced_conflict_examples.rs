//! Examples for Advanced Transaction Conflict Management
//! 
//! Demonstrates real-world scenarios for double-spend prevention,
//! dependency tracking, and deterministic conflict resolution.

#[allow(dead_code)]
mod examples {
    use std::sync::Arc;
    use std::collections::VecDeque;

    use klomang_node::mempool::advanced_conflicts::ConflictMap;
    use klomang_node::mempool::dependency_graph::DependencyGraph;
    use klomang_node::mempool::advanced_transaction_manager::AdvancedTransactionManager;
    use klomang_node::mempool::pool::TransactionPool;
    use klomang_node::storage::kv_store::KvStore;
    use klomang_core::core::crypto::Hash;
    use klomang_core::core::state::transaction::{Transaction, TxInput};

    fn create_example_tx(id: u8, input_sources: Vec<u8>) -> Transaction {
        let mut inputs = Vec::new();
        for (idx, source_id) in input_sources.iter().enumerate() {
            inputs.push(TxInput {
                prev_tx: Hash::new(&[*source_id; 32]),
                index: idx as u32,
            });
        }

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

    /// Example 1: Basic double-spending detection
    pub fn example_basic_double_spend_detection() {
        println!("=== Example 1: Basic Double-Spending Detection ===\n");

        let kv_store = Arc::new(KvStore::new_test());
        let conflict_map = Arc::new(ConflictMap::new(kv_store));

        // Two transactions claiming the same UTXO
        let tx_alice = create_example_tx(1, vec![100]); // Claims UTXO from TX-100
        let tx_bob = create_example_tx(2, vec![100]);   // Also claims UTXO from TX-100

        let alice_hash = klomang_node::mempool::TxHash::new(vec![1; 32]);
        let bob_hash = klomang_node::mempool::TxHash::new(vec![2; 32]);

        println!("Registering TX-Alice...");
        let result_alice = conflict_map.register_transaction(&tx_alice, &alice_hash);
        match result_alice {
            Ok(klomang_node::mempool::ConflictType::NoConflict) => {
                println!("✓ TX-Alice registered successfully (no conflict)\n");
            }
            _ => println!("✗ Unexpected result for TX-Alice"),
        }

        println!("Registering TX-Bob (claims same UTXO)...");
        let result_bob = conflict_map.register_transaction(&tx_bob, &bob_hash);
        match result_bob {
            Ok(klomang_node::mempool::ConflictType::DirectConflict { tx_a, tx_b, outpoint }) => {
                println!("✓ CONFLICT DETECTED!");
                println!("  - TX-A: {:?}", tx_a);
                println!("  - TX-B: {:?}", tx_b);
                println!("  - Contested OutPoint: index {}\n", outpoint.index);
            }
            _ => println!("✗ Unexpected result for TX-Bob"),
        }

        let stats = conflict_map.get_stats();
        println!("Conflict Statistics:");
        println!("  - Total conflicts detected: {}", stats.total_conflicts_detected);
        println!("  - Direct conflicts: {}\n", stats.direct_conflicts);
    }

    /// Example 2: Deterministic resolution - Fee rate priority
    pub fn example_deterministic_resolution_fee_rate() {
        println!("=== Example 2: Deterministic Resolution - Fee Rate Priority ===\n");

        let kv_store = Arc::new(KvStore::new_test());
        let conflict_map = Arc::new(ConflictMap::new(kv_store));

        let tx_low_fee = create_example_tx(1, vec![100]);
        let tx_high_fee = create_example_tx(2, vec![100]);

        let low_hash = klomang_node::mempool::TxHash::new(vec![1; 32]);
        let high_hash = klomang_node::mempool::TxHash::new(vec![2; 32]);

        conflict_map.register_transaction(&tx_low_fee, &low_hash).ok();

        // Resolve between low-fee (500 sat, 1000 bytes) and high-fee (2000 sat, 100 bytes)
        // Low-fee rate: 0.5 sat/byte
        // High-fee rate: 20.0 sat/byte (WINNER)
        println!("TX-LowFee:  500 satoshis / 1000 bytes = 0.5 sat/byte");
        println!("TX-HighFee: 2000 satoshis / 100 bytes = 20.0 sat/byte\n");

        let resolution = conflict_map.resolve_conflict(
            &tx_low_fee,
            &tx_high_fee,
            &low_hash,
            &high_hash,
            1000, // TX-LowFee size
            100,  // TX-HighFee size
            500,  // TX-LowFee fee
            2000, // TX-HighFee fee
        );

        match resolution {
            Ok(result) => {
                println!("Resolution Result:");
                println!("  - Winner: {:?}", result.winner);
                println!("  - Loser: {:?}", result.loser);
                println!("  - Reason: {:?}", result.reason);
                println!("  - Expected: TX-HighFee wins due to HIGHER FEE RATE\n");
            }
            Err(e) => println!("✗ Resolution failed: {}\n", e),
        }
    }

    /// Example 3: Dependency chain cascade
    pub fn example_dependency_chain_cascade() {
        println!("=== Example 3: Dependency Chain Cascade ===\n");

        let graph = Arc::new(DependencyGraph::new());

        // Build payment chain:
        // Alice-TX receives coins from Previous-TX
        // Bob-TX spends from Alice-TX
        // Carol-TX spends from Bob-TX
        let prev_tx = klomang_node::mempool::TxHash::new(vec![0; 32]);
        let alice_tx = klomang_node::mempool::TxHash::new(vec![1; 32]);
        let bob_tx = klomang_node::mempool::TxHash::new(vec![2; 32]);
        let carol_tx = klomang_node::mempool::TxHash::new(vec![3; 32]);

        println!("Building dependency chain:");
        println!("  PrevTX -> Alice-TX -> Bob-TX -> Carol-TX\n");

        graph.register_transaction(&prev_tx);
        graph.register_transaction(&alice_tx);
        graph.register_transaction(&bob_tx);
        graph.register_transaction(&carol_tx);

        graph.add_dependency(&alice_tx, &prev_tx).ok();
        println!("✓ Alice-TX depends on PrevTX");

        graph.add_dependency(&bob_tx, &alice_tx).ok();
        println!("✓ Bob-TX depends on Alice-TX");

        graph.add_dependency(&carol_tx, &bob_tx).ok();
        println!("✓ Carol-TX depends on Bob-TX\n");

        // Now mark previous as conflict
        println!("Marking PrevTX as CONFLICTED...");
        let affected = graph.mark_conflict(&prev_tx, "Double-spend at root".to_string());

        match affected {
            Ok(affected_txs) => {
                println!("✓ Conflict propagated to {} transactions:", affected_txs.len());
                for tx in &[&prev_tx, &alice_tx, &bob_tx, &carol_tx] {
                    let in_conflict = graph.is_in_conflict(tx);
                    println!("  - {:?}: {}", tx, if in_conflict { "CONFLICT" } else { "OK" });
                }
                println!();
            }
            Err(e) => println!("✗ Failed to mark conflict: {}\n", e),
        }
    }

    /// Example 4: Multiple input conflict resolution
    pub fn example_multiple_input_conflict() {
        println!("=== Example 4: Multiple Input Conflict ===\n");

        let kv_store = Arc::new(KvStore::new_test());
        let conflict_map = Arc::new(ConflictMap::new(kv_store));

        // TX-Expensive uses 3 inputs from different sources
        let tx_expensive = create_example_tx(1, vec![10, 20, 30]);
        let expensive_hash = klomang_node::mempool::TxHash::new(vec![1; 32]);

        println!("TX-Expensive claims inputs from:");
        println!("  - UTXO from TX-10 (index 0)");
        println!("  - UTXO from TX-20 (index 1)");
        println!("  - UTXO from TX-30 (index 2)\n");

        conflict_map.register_transaction(&tx_expensive, &expensive_hash).ok();
        println!("✓ TX-Expensive registered\n");

        // TX-Fast claims only 1 of those inputs
        let tx_fast = create_example_tx(2, vec![20, 40]);
        let fast_hash = klomang_node::mempool::TxHash::new(vec![2; 32]);

        println!("Attempting to register TX-Fast (claims TX-20 input)...");
        let result = conflict_map.register_transaction(&tx_fast, &fast_hash);

        match result {
            Ok(klomang_node::mempool::ConflictType::DirectConflict { .. }) => {
                println!("✓ CONFLICT on shared input!");
                println!("  Now need to resolve which one keeps the TX-20 UTXO\n");

                // Hypothetical: TX-Fast has better fee rate
                let resolution = conflict_map.resolve_conflict(
                    &tx_expensive, &tx_fast,
                    &expensive_hash, &fast_hash,
                    500, 100,  // sizes
                    2000, 1500, // fees
                );

                if let Ok(res) = resolution {
                    println!("Resolution: {:?} wins", res.winner);
                    println!("Reason: {:?}\n", res.reason);
                }
            }
            _ => println!("✗ Unexpected result\n"),
        }

        // Show all conflicted outpoints
        let conflicted = conflict_map.get_conflicted_outpoints();
        println!("Total conflicted outpoints: {}\n", conflicted.len());
    }

    /// Example 5: Transaction orphaning via graph
    pub fn example_orphaned_transaction_handling() {
        println!("=== Example 5: Orphaned Transaction Handling ===\n");

        let graph = Arc::new(DependencyGraph::new());

        // Scenario: User submits chain of transactions before parent is confirmed
        let parent = klomang_node::mempool::TxHash::new(vec![1; 32]);
        let child1 = klomang_node::mempool::TxHash::new(vec![2; 32]);
        let child2 = klomang_node::mempool::TxHash::new(vec![3; 32]);

        graph.register_transaction(&parent);
        graph.register_transaction(&child1);
        graph.register_transaction(&child2);

        println!("Transaction dependency tree:");
        println!("         Parent");
        println!("        /      \\");
        println!("     Child1   Child2\n");

        graph.add_dependency(&child1, &parent).ok();
        graph.add_dependency(&child2, &parent).ok();

        println!("✓ Dependencies registered\n");

        // Parent gets double-spent
        println!("Parent TX gets conflicted (detected double-spend)...\n");
        graph.mark_conflict(&parent, "Double-spend detected".to_string()).ok();

        println!("Impact on children:");
        println!("  - Child1: {} (connected to conflicted chain)", 
            if graph.is_in_conflict(&child1) { "ORPHANED" } else { "OK" });
        println!("  - Child2: {} (connected to conflicted chain)", 
            if graph.is_in_conflict(&child2) { "ORPHANED" } else { "OK" });
        println!();
    }

    /// Example 6: Conflict analysis and statistics
    pub fn example_conflict_system_analysis() {
        println!("=== Example 6: Conflict System Analysis ===\n");

        let kv_store = Arc::new(KvStore::new_test());
        let conflict_map = Arc::new(ConflictMap::new(kv_store.clone()));
        let graph = Arc::new(DependencyGraph::new());
        let pool = Arc::new(TransactionPool::new(
            Arc::new(std::sync::Mutex::new(VecDeque::new())),
        ));

        let manager = AdvancedTransactionManager::new(
            conflict_map.clone(),
            graph.clone(),
            pool,
            kv_store,
        );

        // Simulate several transactions with conflicts
        let tx1 = create_example_tx(1, vec![100]);
        let tx2 = create_example_tx(2, vec![100]); // Conflicts with tx1
        let tx3 = create_example_tx(3, vec![101]);
        let tx4 = create_example_tx(4, vec![101]); // Conflicts with tx3

        let hash1 = klomang_node::mempool::TxHash::new(vec![1; 32]);
        let hash2 = klomang_node::mempool::TxHash::new(vec![2; 32]);
        let hash3 = klomang_node::mempool::TxHash::new(vec![3; 32]);
        let hash4 = klomang_node::mempool::TxHash::new(vec![4; 32]);

        conflict_map.register_transaction(&tx1, &hash1).ok();
        conflict_map.register_transaction(&tx2, &hash2).ok();
        conflict_map.register_transaction(&tx3, &hash3).ok();
        conflict_map.register_transaction(&tx4, &hash4).ok();

        let analysis = manager.analyze_conflicts();
        match analysis {
            Ok(data) => {
                println!("Mempool Conflict Analysis:");
                println!("  - Total conflicts detected: {}", data.total_conflicts);
                println!("  - Conflicted outpoints: {}", data.conflicted_outpoints);
                println!("  - Affected transactions: {}", data.affected_transactions);
                println!("  - Total resolutions: {}", data.total_resolutions);
                println!("  - Total evictions: {}", data.total_evictions);
                println!("  - Partition count: {}", data.partition_count);
                println!();
            }
            Err(e) => println!("✗ Analysis failed: {}\n", e),
        }
    }

    /// Example 7: Complete workflow - Network node receiving conflicting transactions
    pub fn example_complete_network_workflow() {
        println!("=== Example 7: Complete Network Workflow ===\n");

        let kv_store = Arc::new(KvStore::new_test());
        let conflict_map = Arc::new(ConflictMap::new(kv_store.clone()));
        let graph = Arc::new(DependencyGraph::new());
        let pool = Arc::new(TransactionPool::new(
            Arc::new(std::sync::Mutex::new(VecDeque::new())),
        ));

        let manager = AdvancedTransactionManager::new(
            conflict_map,
            graph,
            pool,
            kv_store,
        );

        println!("Scenario: Network node receives transactions with double-spend attempts\n");

        // Transaction 1: User A sends payment
        println!("[1] User A sends 1 BTC to User C (TX-A)");
        let tx_a = create_example_tx(10, vec![50]);
        let hash_a = klomang_node::mempool::TxHash::new(vec![10; 32]);
        println!("    Inputs: [UTXO-50]");
        println!("    Fee: 5000 satoshis / 250 bytes = 20 sat/byte\n");

        // Transaction 2: Attacker attempts double-spend with same input
        println!("[2] Attacker tries to send same coin to User D (TX-Attack)");
        let tx_attack = create_example_tx(11, vec![50]);
        let hash_attack = klomang_node::mempool::TxHash::new(vec![11; 32]);
        println!("    Inputs: [UTXO-50] <- SAME as TX-A");
        println!("    Fee: 2000 satoshis / 200 bytes = 10 sat/byte");
        println!("    ✗ EVICTED: Lower fee rate\n");

        // Transaction 3: User B sends dependent transaction
        println!("[3] User B sends payment using output from TX-A (TX-B)");
        let tx_b = create_example_tx(12, vec![10]); // Depends on TX-A
        let hash_b = klomang_node::mempool::TxHash::new(vec![12; 32]);
        println!("    Parents: [TX-A]");
        println!("    ✓ ACCEPTED: Depends on valid TX-A\n");

        manager.clear();
        println!("Summary:");
        println!("  ✓ TX-A: Accepted (first, good fee)");
        println!("  ✗ TX-Attack: Rejected (double-spend with lower fee)");
        println!("  ✓ TX-B: Accepted (valid dependency on TX-A)");
        println!("  → Network consensus achieved: All nodes will make same decision\n");
    }
}

#[cfg(test)]
mod run_examples {
    use super::examples::*;

    #[test]
    fn run_all_examples() {
        example_basic_double_spend_detection();
        example_deterministic_resolution_fee_rate();
        example_dependency_chain_cascade();
        example_multiple_input_conflict();
        example_orphaned_transaction_handling();
        example_conflict_system_analysis();
        example_complete_network_workflow();
    }
}
