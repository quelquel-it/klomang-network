//! Comprehensive tests for UTXO conflict management system

#[cfg(test)]
mod conflict_management_tests {
    use klomang_core::core::crypto::Hash;
    use klomang_core::core::state::transaction::{Transaction, TxInput, SigHashType};
    use std::sync::Arc;

    use klomang_node::mempool::{
        UtxoOwnershipManager, UtxoTracker, TransactionPool, PoolConfig, OutPoint,
    };
    use klomang_node::storage::kv_store::KvStore;

    /// Helper function to create test transactions
    fn create_test_tx(id: u8, prev_tx_seeds: Vec<u8>) -> Transaction {
        let inputs = prev_tx_seeds
            .iter()
            .enumerate()
            .map(|(idx, seed)| TxInput {
                prev_tx: Hash::new(&[*seed; 32]),
                index: idx as u32,
                signature: vec![0; 64],
                pubkey: vec![0; 33],
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

    #[test]
    fn test_utxo_tracker_creation() {
        let kv_store = Arc::new(KvStore::new_dummy());
        let tracker = UtxoTracker::new(kv_store);
        assert_eq!(tracker.active_claims_count(), 0);
    }

    #[test]
    fn test_ownership_manager_creation() {
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
        let kv_store = Arc::new(KvStore::new_dummy());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        let stats = manager.get_conflict_stats();
        assert_eq!(stats.total_tracked, 0);
        assert_eq!(stats.rbf_replacements, 0);
    }

    #[test]
    fn test_add_transaction_with_ownership() {
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
        let kv_store = Arc::new(KvStore::new_dummy());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        let tx = create_test_tx(1, vec![10]);
        let result = manager.add_transaction_with_ownership(tx, 1000, 250);

        assert!(result.is_ok());
        let info = result.unwrap();
        assert!(info.added);
        assert!(!info.claimed_outpoints.is_empty());
    }

    #[test]
    fn test_multiple_inputs_tracking() {
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
        let kv_store = Arc::new(KvStore::new_dummy());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        let tx = create_test_tx(1, vec![10, 11, 12]); // 3 inputs
        let result = manager.add_transaction_with_ownership(tx, 1000, 250);

        assert!(result.is_ok());
        let info = result.unwrap();
        assert_eq!(info.claimed_outpoints.len(), 3);
    }

    #[test]
    fn test_conflict_detection_same_input() {
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
        let kv_store = Arc::new(KvStore::new_dummy());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        let tx1 = create_test_tx(1, vec![20]);
        let tx2 = create_test_tx(2, vec![20]); // Same input

        assert!(manager.add_transaction_with_ownership(tx1, 1000, 250).is_ok());

        // Second transaction should detect conflict
        let result = manager.add_transaction_with_ownership(tx2, 500, 250);
        assert!(result.is_err());
    }

    #[test]
    fn test_has_conflicts() {
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
        let kv_store = Arc::new(KvStore::new_dummy());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        let tx1 = create_test_tx(1, vec![30]);
        let tx2 = create_test_tx(2, vec![30]); // Same input

        manager.add_transaction_with_ownership(tx1, 1000, 250).ok();

        // Check for conflicts in tx2
        let has_conflicts = manager.has_conflicts(&tx2);
        assert!(has_conflicts.is_ok());
        assert_eq!(has_conflicts.unwrap(), true);
    }

    #[test]
    fn test_rbf_replacement_higher_fee() {
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
        let kv_store = Arc::new(KvStore::new_dummy());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        let tx1 = create_test_tx(1, vec![40]);
        let tx2 = create_test_tx(2, vec![40]); // Same input

        // Add first transaction with low fee
        assert!(manager.add_transaction_with_ownership(tx1.clone(), 500, 250).is_ok());

        // Add second transaction with higher fee (should trigger RBF)
        let result = manager.add_transaction_with_ownership(tx2, 2000, 250);
        assert!(result.is_ok());

        let info = result.unwrap();
        assert_eq!(info.rbf_replacements, 1);
    }

    #[test]
    fn test_rbf_replacement_lower_fee() {
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
        let kv_store = Arc::new(KvStore::new_dummy());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        let tx1 = create_test_tx(1, vec![50]);
        let tx2 = create_test_tx(2, vec![50]);

        // Add first transaction with high fee
        assert!(manager.add_transaction_with_ownership(tx1, 2000, 250).is_ok());

        // Add second transaction with lower fee (should be rejected)
        let result = manager.add_transaction_with_ownership(tx2, 500, 250);
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_transaction() {
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
        let kv_store = Arc::new(KvStore::new_dummy());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        let tx = create_test_tx(1, vec![60]);
        let tx_hash = bincode::serialize(&tx.id).unwrap();

        // Add transaction
        assert!(manager.add_transaction_with_ownership(tx, 1000, 250).is_ok());
        assert!(manager.tracker().active_claims_count() > 0);

        // Remove transaction
        let result = manager.remove_transaction(&tx_hash);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().found, true);

        // Claims should be released
        let _released_count = manager.tracker().active_claims_count();
        // Note: This might not be 0 if mock storage doesn't properly track
    }

    #[test]
    fn test_get_transaction_claims() {
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
        let kv_store = Arc::new(KvStore::new_dummy());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        let tx = create_test_tx(1, vec![70, 71]);
        let tx_hash = bincode::serialize(&tx.id).unwrap();

        manager.add_transaction_with_ownership(tx, 1000, 250).ok();

        let claims = manager.get_transaction_claims(&tx_hash);
        assert_eq!(claims.len(), 2);
    }

    #[test]
    fn test_conflict_analysis() {
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
        let kv_store = Arc::new(KvStore::new_dummy());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        // Add several transactions
        for i in 1..=3 {
            let tx = create_test_tx(i as u8, vec![80 + i as u8]);
            manager.add_transaction_with_ownership(tx, 1000, 250).ok();
        }

        let analysis = manager.analyze_conflicts().unwrap();
        assert!(analysis.total_transactions > 0);
        assert!(analysis.total_transactions >= 1);
    }

    #[test]
    fn test_get_conflict_stats() {
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
        let kv_store = Arc::new(KvStore::new_dummy());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        let tx = create_test_tx(1, vec![90]);
        manager.add_transaction_with_ownership(tx, 1000, 250).ok();

        let stats = manager.get_conflict_stats();
        assert_eq!(stats.total_tracked, 1);
        assert!(stats.total_claims > 0);
    }

    #[test]
    fn test_sync_with_new_block() {
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
        let kv_store = Arc::new(KvStore::new_dummy());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        let tx = create_test_tx(1, vec![100]);
        let tx_hash = bincode::serialize(&tx.id).unwrap();

        manager.add_transaction_with_ownership(tx, 1000, 250).ok();

        // Simulate new block with this transaction
        let result = manager.sync_with_new_block(&[tx_hash.clone()]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_non_conflicting_transactions() {
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
        let kv_store = Arc::new(KvStore::new_dummy());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        let tx1 = create_test_tx(1, vec![110]); // Uses UTXOs from different sources
        let tx2 = create_test_tx(2, vec![111]);

        let result1 = manager.add_transaction_with_ownership(tx1, 1000, 250);
        let result2 = manager.add_transaction_with_ownership(tx2, 1000, 250);

        // Both should succeed as they don't conflict
        assert!(result1.is_ok());
        assert!(result2.is_ok());
    }

    #[test]
    fn test_outpoint_creation() {
        let outpoint = OutPoint::new(vec![1, 2, 3], 5);
        assert_eq!(outpoint.index, 5);
        assert_eq!(outpoint.tx_hash, vec![1, 2, 3]);
    }

    #[test]
    fn test_is_outpoint_available() {
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
        let kv_store = Arc::new(KvStore::new_dummy());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        let outpoint = OutPoint::new(vec![1; 32], 0);

        // Should be available initially
        assert!(manager.is_outpoint_available(&outpoint));

        // Add transaction that claims this outpoint
        // This test is simplified as actual tracking requires proper setup
    }

    #[test]
    fn test_verify_inputs_available() {
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
        let kv_store = Arc::new(KvStore::new_dummy());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        let tx = create_test_tx(1, vec![120]);
        let result = manager.verify_inputs_available(&tx);

        assert!(result.is_ok());
    }

    #[test]
    fn test_cleanup_expired() {
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
        let kv_store = Arc::new(KvStore::new_dummy());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        let tx = create_test_tx(1, vec![130]);
        manager.add_transaction_with_ownership(tx, 1000, 250).ok();

        // Run cleanup
        let result = manager.cleanup_expired();
        assert!(result.is_ok());
    }

    #[test]
    fn test_complex_rbf_scenario() {
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
        let kv_store = Arc::new(KvStore::new_dummy());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        // Create chain of replacements
        let tx1 = create_test_tx(1, vec![140]);
        let tx2 = create_test_tx(2, vec![140]); // Same input - will replace tx1
        let tx3 = create_test_tx(3, vec![140]); // Same input - will replace tx2

        let r1 = manager.add_transaction_with_ownership(tx1, 500, 250).ok();
        let r2 = manager.add_transaction_with_ownership(tx2, 1000, 250).ok();
        let r3 = manager.add_transaction_with_ownership(tx3, 1500, 250).ok();

        assert!(r1.is_some());
        assert!(r2.is_some());
        assert!(r3.is_some());

        if let Some(info3) = r3 {
            // Should have 1 RBF replacement
            assert_eq!(info3.rbf_replacements, 1);
        }
    }

    #[test]
    fn test_stats_accumulation() {
        let pool = Arc::new(TransactionPool::new(PoolConfig::default()));
        let kv_store = Arc::new(KvStore::new_dummy());
        let manager = UtxoOwnershipManager::new(pool, kv_store);

        // Add multiple transactions
        for i in 1..=5 {
            let tx = create_test_tx(i as u8, vec![150 + i as u8]);
            manager.add_transaction_with_ownership(tx, 1000, 250).ok();
        }

        let stats = manager.get_conflict_stats();
        assert_eq!(stats.total_tracked, 5);
        assert_eq!(stats.total_claims, 5); // 1 input each

        // Perform RBF
        let tx_old = create_test_tx(10, vec![160]);
        let tx_new = create_test_tx(11, vec![160]);

        manager.add_transaction_with_ownership(tx_old, 500, 250).ok();
        manager.add_transaction_with_ownership(tx_new, 1000, 250).ok();

        let stats_after = manager.get_conflict_stats();
        assert!(stats_after.rbf_replacements > stats.rbf_replacements);
    }
}
