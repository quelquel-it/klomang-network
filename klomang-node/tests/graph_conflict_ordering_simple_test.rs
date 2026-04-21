//! Simple integration tests for Graph-Based Conflict & Ordering System
//! Testing the public API only

#[cfg(test)]
mod graph_conflict_tests {
    use klomang_node::mempool::graph_conflict_ordering::GraphConflictOrderingEngine;
    use klomang_core::core::crypto::Hash;
    use klomang_core::core::state::transaction::{Transaction, TxInput, TxOutput};

    fn create_test_transaction() -> Transaction {
        Transaction {
            id: Hash::new(&[0; 32]),
            inputs: vec![TxInput {
                prev_tx: Hash::new(&[1; 32]),
                index: 0,
                signature: vec![],
                pubkey: vec![1, 2, 3],
                sighash_type: klomang_core::core::state::transaction::SigHashType::All,
            }],
            outputs: vec![TxOutput {
                value: 1000,
                pubkey_hash: Hash::new(&[2; 32]),
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
    fn test_engine_basic() {
        let engine = GraphConflictOrderingEngine::new(None);
        assert_eq!(engine.transaction_count(), 0);
    }

    #[test]
    fn test_register_and_detect_conflict() {
        let engine = GraphConflictOrderingEngine::new(None);
        let tx = create_test_transaction();
        
        // Register first transaction
        let tx1_hash = vec![1];
        let result1 = engine.register_transaction(&tx, tx1_hash.clone(), 1000, 0);
        assert!(result1.is_ok());
        assert_eq!(engine.transaction_count(), 1);
        
        // Register second with same UTXO - should conflict
        let tx2_hash = vec![2];
        let result2 = engine.register_transaction(&tx, tx2_hash.clone(), 2000, 1000);
        assert!(result2.is_ok());
        
        let conflicts = result2.unwrap();
        assert!(conflicts.contains(&tx1_hash));
    }

    #[test]
    fn test_double_spend_detection() {
        let engine = GraphConflictOrderingEngine::new(None);
        let tx = create_test_transaction();
        
        let tx1_hash = vec![1];
        let tx2_hash = vec![2];
        
        engine.register_transaction(&tx, tx1_hash, 1000, 0).ok();
        engine.register_transaction(&tx, tx2_hash.clone(), 2000, 1000).ok();
        
        let has_double_spend = engine.detect_double_spend(&tx2_hash).unwrap();
        assert!(has_double_spend);
    }

    #[test]
    fn test_canonical_ordering() {
        let engine = GraphConflictOrderingEngine::new(None);
        let tx = create_test_transaction();
        
        engine.register_transaction(&tx, vec![1], 1000, 0).ok();
        engine.register_transaction(&tx, vec![2], 2000, 1000).ok();
        
        let result = engine.compute_canonical_order().unwrap();
        assert!(result.ordered_hashes.len() >= 1);
    }

    #[test]
    fn test_parallel_groups() {
        let engine = GraphConflictOrderingEngine::new(None);
        let tx = create_test_transaction();
        
        engine.register_transaction(&tx, vec![1], 1000, 0).ok();
        
        let groups = engine.get_parallel_execution_groups().unwrap();
        assert!(!groups.is_empty());
    }

    #[test]
    fn test_get_conflicts() {
        let engine = GraphConflictOrderingEngine::new(None);
        let tx = create_test_transaction();
        
        let tx1_hash = vec![1];
        let tx2_hash = vec![2];
        
        engine.register_transaction(&tx, tx1_hash.clone(), 1000, 0).ok();
        engine.register_transaction(&tx, tx2_hash.clone(), 2000, 1000).ok();
        
        let conflicts = engine.get_conflicts(&tx1_hash);
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts.contains(&tx2_hash));
    }

    #[test]
    fn test_dependency_management() {
        let engine = GraphConflictOrderingEngine::new(None);
        let tx = create_test_transaction();
        
        let parent = vec![1];
        let child = vec![2];
        
        engine.register_transaction(&tx, parent.clone(), 1000, 0).ok();
        engine.register_transaction(&tx, child.clone(), 2000, 1000).ok();
        
        let result = engine.add_dependency(parent, child);
        assert!(result.is_ok());
    }

    #[test]
    fn test_cascade_removal() {
        let engine = GraphConflictOrderingEngine::new(None);
        let tx = create_test_transaction();
        
        let parent = vec![1];
        let child = vec![2];
        
        engine.register_transaction(&tx, parent.clone(), 1000, 0).ok();
        engine.register_transaction(&tx, child.clone(), 2000, 1000).ok();
        engine.add_dependency(parent.clone(), child).ok();
        
        let removed = engine.remove_transaction_cascade(&parent).unwrap();
        assert!(removed.contains(&parent));
    }

    #[test]
    fn test_weight_priority() {
        let mut engine = GraphConflictOrderingEngine::new(None);
        engine.set_priority_weights(0.8, 0.2);
        
        let tx = create_test_transaction();
        engine.register_transaction(&tx, vec![1], 1000, 0).ok();
        
        let result = engine.compute_canonical_order();
        assert!(result.is_ok());
    }
}

#[cfg(test)]
mod integration_tests {
    use klomang_node::mempool::graph_conflict_ordering_integration::{
        ConflictOrderingIntegration,
        ConflictOrderingIntegrationConfig,
    };
    use klomang_core::core::crypto::Hash;
    use klomang_core::core::state::transaction::{Transaction, TxInput, TxOutput};

    fn create_test_transaction() -> Transaction {
        Transaction {
            id: Hash::new(&[0; 32]),
            inputs: vec![TxInput {
                prev_tx: Hash::new(&[1; 32]),
                index: 0,
                signature: vec![],
                pubkey: vec![1, 2, 3],
                sighash_type: klomang_core::core::state::transaction::SigHashType::All,
            }],
            outputs: vec![TxOutput {
                value: 1000,
                pubkey_hash: Hash::new(&[2; 32]),
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
    fn test_integration_creation() {
        let config = ConflictOrderingIntegrationConfig::default();
        let integration = ConflictOrderingIntegration::new(config, None);
        
        let stats = integration.get_stats();
        assert_eq!(stats.transaction_count, 0);
    }

    #[test]
    fn test_register_transaction() {
        let config = ConflictOrderingIntegrationConfig::default();
        let integration = ConflictOrderingIntegration::new(config, None);
        
        let tx = create_test_transaction();
        let tx_hash = vec![1];
        
        let result = integration.register_transaction(&tx, tx_hash, 1000, 0);
        assert!(result.is_ok());
        
        let detection = result.unwrap();
        assert!(!detection.has_double_spend);
        assert!(detection.is_valid);
    }

    #[test]
    fn test_block_building() {
        let config = ConflictOrderingIntegrationConfig::default();
        let integration = ConflictOrderingIntegration::new(config, None);
        
        let tx = create_test_transaction();
        integration.register_transaction(&tx, vec![1], 1000, 0).ok();
        
        let result = integration.build_block_canonical(1_000_000);
        assert!(result.is_ok());
    }

    #[test]
    fn test_utxo_validation() {
        let config = ConflictOrderingIntegrationConfig::default();
        let integration = ConflictOrderingIntegration::new(config, None);
        
        let tx = create_test_transaction();
        let result = integration.validate_utxo_state(&tx);
        assert!(result.is_ok());
    }

    #[test]
    fn test_clear() {
        let config = ConflictOrderingIntegrationConfig::default();
        let integration = ConflictOrderingIntegration::new(config, None);
        
        let tx = create_test_transaction();
        integration.register_transaction(&tx, vec![1], 1000, 0).ok();
        
        assert_eq!(integration.get_stats().transaction_count, 1);
        
        integration.clear();
        assert_eq!(integration.get_stats().transaction_count, 0);
    }
}
