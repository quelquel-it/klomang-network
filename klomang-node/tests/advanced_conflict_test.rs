//! Comprehensive tests for advanced transaction conflict management system

#[cfg(test)]
mod advanced_conflict_tests {
    use std::sync::Arc;
    use std::collections::VecDeque;

    use crate::mempool::advanced_conflicts::{ConflictMap, TxHash, ConflictType, ResolutionReason};
    use crate::mempool::dependency_graph::DependencyGraph;
    use crate::mempool::advanced_transaction_manager::{AdvancedTransactionManager, ManagerError};
    use crate::mempool::pool::TransactionPool;
    use crate::storage::kv_store::KvStore;
    use klomang_core::core::crypto::Hash;
    use klomang_core::core::state::transaction::{Transaction, TxInput};

    fn create_test_tx(id: u8, prev_txs: Vec<u8>) -> Transaction {
        let mut inputs = Vec::new();
        for (idx, prev_tx_id) in prev_txs.iter().enumerate() {
            inputs.push(TxInput {
                prev_tx: Hash::new(&[*prev_tx_id; 32]),
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

    fn tx_hash(id: u8) -> TxHash {
        TxHash::new(vec![id; 32])
    }

    #[test]
    fn test_triple_conflict_detection_and_resolution() {
        // Scenario: Three transactions all claiming same UTXO
        let kv_store = Arc::new(KvStore::new_test());
        let conflict_map = Arc::new(ConflictMap::new(kv_store.clone()));

        // TX-A: 1000 fee / 100 bytes = 10 sat/byte
        let tx_a = create_test_tx(1, vec![100]);
        let hash_a = tx_hash(1);
        conflict_map.register_transaction(&tx_a, &hash_a).ok();

        // TX-B: 2000 fee / 100 bytes = 20 sat/byte (should win)
        let tx_b = create_test_tx(2, vec![100]);
        let hash_b = tx_hash(2);
        let result_b = conflict_map.register_transaction(&tx_b, &hash_b);
        assert!(matches!(result_b, Ok(ConflictType::DirectConflict { .. })));

        // TX-C: 500 fee / 100 bytes = 5 sat/byte (should lose)
        let tx_c = create_test_tx(3, vec![100]);
        let hash_c = tx_hash(3);
        let result_c = conflict_map.register_transaction(&tx_c, &hash_c);
        assert!(matches!(result_c, Ok(ConflictType::DirectConflict { .. })));

        // Resolve all three
        let resolution_ab = conflict_map.resolve_conflict(
            &tx_a, &tx_b, &hash_a, &hash_b, 100, 100, 1000, 2000,
        );
        assert!(resolution_ab.is_ok());
        let res_ab = resolution_ab.unwrap();
        assert_eq!(res_ab.winner, hash_b); // B has higher fee rate
        assert_eq!(res_ab.reason, ResolutionReason::HigherFeeRate);

        let resolution_bc = conflict_map.resolve_conflict(
            &tx_b, &tx_c, &hash_b, &hash_c, 100, 100, 2000, 500,
        );
        assert!(resolution_bc.is_ok());
        let res_bc = resolution_bc.unwrap();
        assert_eq!(res_bc.winner, hash_b); // B still higher
        assert_eq!(res_bc.reason, ResolutionReason::HigherFeeRate);
    }

    #[test]
    fn test_timestamp_based_resolution() {
        let kv_store = Arc::new(KvStore::new_test());
        let conflict_map = Arc::new(ConflictMap::new(kv_store));

        // Two transactions with identical fee rates
        let tx_a = create_test_tx(1, vec![100]);
        let hash_a = tx_hash(1);
        conflict_map.register_transaction(&tx_a, &hash_a).ok();

        // Wait to ensure different timestamps (in real scenario)
        std::thread::sleep(std::time::Duration::from_millis(10));

        let tx_b = create_test_tx(2, vec![100]);
        let hash_b = tx_hash(2);

        let result = conflict_map.resolve_conflict(
            &tx_a, &tx_b, &hash_a, &hash_b, 100, 100, 1000, 1000,
        );
        assert!(result.is_ok());
        let res = result.unwrap();
        assert_eq!(res.reason, ResolutionReason::EarlierArrival);
        assert_eq!(res.winner, hash_a); // A arrived first
    }

    #[test]
    fn test_lexicographical_hash_resolution() {
        let kv_store = Arc::new(KvStore::new_test());
        let conflict_map = Arc::new(ConflictMap::new(kv_store));

        // Create hashes where one is lexicographically smaller
        let hash_a = TxHash::new(vec![0x01; 32]);
        let hash_b = TxHash::new(vec![0xFF; 32]);

        let tx_a = create_test_tx(1, vec![100]);
        let tx_b = create_test_tx(2, vec![100]);

        conflict_map.register_transaction(&tx_a, &hash_a).ok();

        let result = conflict_map.resolve_conflict(
            &tx_a, &tx_b, &hash_a, &hash_b, 100, 100, 1000, 1000,
        );
        assert!(result.is_ok());
        let res = result.unwrap();
        assert_eq!(res.reason, ResolutionReason::LexicographicalHash);
        assert_eq!(res.winner, hash_a);
    }

    #[test]
    fn test_dependency_graph_conflict_propagation() {
        let graph = Arc::new(DependencyGraph::new());

        // Create chain: TX-A -> TX-B -> TX-C (each depends on previous)
        let tx_a = tx_hash(1);
        let tx_b = tx_hash(2);
        let tx_c = tx_hash(3);

        graph.register_transaction(&tx_a);
        graph.register_transaction(&tx_b);
        graph.register_transaction(&tx_c);

        graph.add_dependency(&tx_b, &tx_a).ok();
        graph.add_dependency(&tx_c, &tx_b).ok();

        // Mark TX-A as conflict
        let affected = graph.mark_conflict(&tx_a, "Conflict at root".to_string());
        assert!(affected.is_ok());
        let affected_list = affected.unwrap();

        // Should affect all three
        assert_eq!(affected_list.len(), 3);
        assert!(graph.is_in_conflict(&tx_a));
        assert!(graph.is_in_conflict(&tx_b));
        assert!(graph.is_in_conflict(&tx_c));
    }

    #[test]
    fn test_dependency_graph_multiple_branches() {
        let graph = Arc::new(DependencyGraph::new());

        // Tree structure:
        //     TX-A
        //    /    \
        //  TX-B  TX-C
        //   |
        //  TX-D

        let tx_a = tx_hash(1);
        let tx_b = tx_hash(2);
        let tx_c = tx_hash(3);
        let tx_d = tx_hash(4);

        graph.register_transaction(&tx_a);
        graph.register_transaction(&tx_b);
        graph.register_transaction(&tx_c);
        graph.register_transaction(&tx_d);

        graph.add_dependency(&tx_b, &tx_a).ok();
        graph.add_dependency(&tx_c, &tx_a).ok();
        graph.add_dependency(&tx_d, &tx_b).ok();

        // Mark TX-A as conflict
        let affected = graph.mark_conflict(&tx_a, "Root conflict".to_string());
        assert!(affected.is_ok());
        let affected_list = affected.unwrap();

        // All should be in same partition
        let partition_a = graph.get_partition(&tx_a);
        let partition_b = graph.get_partition(&tx_b);
        assert_eq!(partition_a.as_ref().map(|p| p.id), partition_b.as_ref().map(|p| p.id));

        // All should be marked as conflict
        for tx in &[tx_a, tx_b, tx_c, tx_d] {
            assert!(graph.is_in_conflict(tx));
        }
    }

    #[test]
    fn test_complex_multi_input_conflict_scenario() {
        let kv_store = Arc::new(KvStore::new_test());
        let conflict_map = Arc::new(ConflictMap::new(kv_store));

        // TX-A uses inputs from UTXO-1, UTXO-2, UTXO-3
        let tx_a = create_test_tx(1, vec![10, 11, 12]);
        let hash_a = tx_hash(1);
        conflict_map.register_transaction(&tx_a, &hash_a).ok();

        // TX-B uses input from UTXO-2 (conflicts with A)
        let tx_b = create_test_tx(2, vec![11, 20]);
        let hash_b = tx_hash(2);
        let result_b = conflict_map.register_transaction(&tx_b, &hash_b);
        assert!(matches!(result_b, Ok(ConflictType::DirectConflict { .. })));

        // TX-C uses input from UTXO-3 (also conflicts with A)
        let tx_c = create_test_tx(3, vec![12, 30]);
        let hash_c = tx_hash(3);
        let result_c = conflict_map.register_transaction(&tx_c, &hash_c);
        assert!(matches!(result_c, Ok(ConflictType::DirectConflict { .. })));

        // Stats should show two conflicts
        let stats = conflict_map.get_stats();
        assert_eq!(stats.direct_conflicts, 2);
    }

    #[test]
    fn test_conflict_map_remove_and_reuse() {
        let kv_store = Arc::new(KvStore::new_test());
        let conflict_map = Arc::new(ConflictMap::new(kv_store));

        let tx_a = create_test_tx(1, vec![100]);
        let hash_a = tx_hash(1);
        conflict_map.register_transaction(&tx_a, &hash_a).ok();

        let conflicted = conflict_map.get_conflicted_outpoints();
        assert_eq!(conflicted.len(), 1);

        // Remove TX-A
        conflict_map.remove_transaction(&hash_a).ok();

        // Outpoint should be free now
        let conflicted_after = conflict_map.get_conflicted_outpoints();
        assert_eq!(conflicted_after.len(), 0);

        // TX-B should be able to use same input without conflict
        let tx_b = create_test_tx(2, vec![100]);
        let hash_b = tx_hash(2);
        let result = conflict_map.register_transaction(&tx_b, &hash_b);
        assert_eq!(result.unwrap(), ConflictType::NoConflict);
    }

    #[test]
    fn test_dependency_graph_partition_merging() {
        let graph = Arc::new(DependencyGraph::new());

        // Create two separate partitions
        let tx_a = tx_hash(1);
        let tx_b = tx_hash(2);
        graph.register_transaction(&tx_a);
        graph.register_transaction(&tx_b);

        let partition_a = graph.get_partition(&tx_a).unwrap().id;
        let partition_b = graph.get_partition(&tx_b).unwrap().id;
        assert_ne!(partition_a, partition_b);

        // Add dependency to merge partitions
        graph.add_dependency(&tx_b, &tx_a).ok();

        let new_partition_b = graph.get_partition(&tx_b).unwrap().id;
        assert_eq!(partition_a, new_partition_b);
    }

    #[test]
    fn test_find_affected_downstream() {
        let graph = Arc::new(DependencyGraph::new());

        // Linear chain: A -> B -> C -> D
        let tx_a = tx_hash(1);
        let tx_b = tx_hash(2);
        let tx_c = tx_hash(3);
        let tx_d = tx_hash(4);

        graph.register_transaction(&tx_a);
        graph.register_transaction(&tx_b);
        graph.register_transaction(&tx_c);
        graph.register_transaction(&tx_d);

        graph.add_dependency(&tx_b, &tx_a).ok();
        graph.add_dependency(&tx_c, &tx_b).ok();
        graph.add_dependency(&tx_d, &tx_c).ok();

        let affected = graph.find_affected_downstream(&tx_a);
        assert_eq!(affected.len(), 4);
        assert!(affected.contains(&tx_a));
        assert!(affected.contains(&tx_b));
        assert!(affected.contains(&tx_c));
        assert!(affected.contains(&tx_d));
    }

    #[test]
    fn test_advanced_manager_conflict_analysis() {
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

        // Register some conflicting transactions
        let tx_a = create_test_tx(1, vec![100]);
        let hash_a = tx_hash(1);
        conflict_map.register_transaction(&tx_a, &hash_a).ok();

        let tx_b = create_test_tx(2, vec![100]);
        let hash_b = tx_hash(2);
        conflict_map.register_transaction(&tx_b, &hash_b).ok();

        let analysis = manager.analyze_conflicts();
        assert!(analysis.is_ok());
        let analysis_data = analysis.unwrap();
        assert_eq!(analysis_data.conflicted_outpoints, 1);
        assert_eq!(analysis_data.affected_transactions, 2);
    }

    #[test]
    fn test_conflict_status_tracking() {
        let kv_store = Arc::new(KvStore::new_test());
        let conflict_map = Arc::new(ConflictMap::new(kv_store.clone()));
        let graph = Arc::new(DependencyGraph::new());
        let pool = Arc::new(TransactionPool::new(
            Arc::new(std::sync::Mutex::new(VecDeque::new())),
        ));

        let manager = AdvancedTransactionManager::new(
            conflict_map,
            graph.clone(),
            pool,
            kv_store,
        );

        let tx = tx_hash(1);
        graph.register_transaction(&tx);

        // Status before conflict
        let status_before = manager.get_conflict_status(&tx);
        assert!(!status_before.in_conflict);
        assert_eq!(status_before.reason, None);

        // Mark as conflict
        graph.mark_conflict(&tx, "Test conflict".to_string()).ok();

        // Status after conflict
        let status_after = manager.get_conflict_status(&tx);
        assert!(status_after.in_conflict);
        assert_eq!(status_after.reason, Some("Test conflict".to_string()));
    }

    #[test]
    fn test_orphaned_transaction_detection() {
        // Scenario: Parent transaction conflicts and is evicted
        // Child transaction should be marked as orphaned
        let graph = Arc::new(DependencyGraph::new());

        let parent = tx_hash(1);
        let child = tx_hash(2);

        graph.register_transaction(&parent);
        graph.register_transaction(&child);
        graph.add_dependency(&child, &parent).ok();

        // Mark parent as conflict (simulating eviction)
        let affected = graph.mark_conflict(&parent, "Parent conflicted".to_string());
        assert!(affected.is_ok());

        // Child should now be in conflict
        assert!(graph.is_in_conflict(&child));

        // Remove parent
        graph.remove_transaction(&parent).ok();

        // Child should have no remaining parents but still be marked conflict
        let remaining_parents = graph.get_parents(&child);
        assert_eq!(remaining_parents.len(), 0);
        assert!(graph.is_in_conflict(&child));
    }
}
