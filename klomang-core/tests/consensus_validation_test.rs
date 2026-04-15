use klomang_core::core::consensus::ghostdag::GhostDag;
use klomang_core::core::dag::{Dag, BlockNode, BlockHeader};
use klomang_core::core::state::storage::MemoryStorage;
use klomang_core::core::state::v_trie::VerkleTree;
use klomang_core::core::state::transaction::{Transaction, TxOutput};
use klomang_core::core::crypto::Hash;
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

fn make_coinbase_transaction(value: u64, pubkey_hash: Hash) -> Transaction {
    Transaction::new(vec![], vec![TxOutput { value, pubkey_hash }])
}

fn make_test_block(transactions: Vec<Transaction>, timestamp: u64, difficulty: u64, nonce: u64) -> BlockNode {
    BlockNode {
        header: BlockHeader {
            id: Hash::new(b"block"),
            parents: HashSet::new(),
            timestamp,
            difficulty,
            nonce,
            verkle_root: Hash::new(b"root"),
            verkle_proofs: None,
            signature: None,
        },
        children: HashSet::new(),
        selected_parent: None,
        blue_set: HashSet::new(),
        red_set: HashSet::new(),
        blue_score: 1,
        transactions,
    }
}

#[test]
fn ghostdag_validate_block_success() {
    let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let dag = Dag::new();

    let tree = VerkleTree::new(MemoryStorage::new()).expect("Verkle tree create");
    let consensus = GhostDag::new(1);

    let tx = make_coinbase_transaction(1_000, Hash::new(b"pubkey"));
    let block = make_test_block(vec![tx], current_time, u64::MAX, 0);

    let res = consensus.validate_block(&block, &dag, &tree, current_time);
    assert!(res.is_ok(), "Block should validate successfully, got: {:?}", res);
}

#[test]
fn ghostdag_validate_block_future_timestamp_fails() {
    let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let dag = Dag::new();
    let tree = VerkleTree::new(MemoryStorage::new()).expect("Verkle tree create");
    let consensus = GhostDag::new(1);

    let tx = make_coinbase_transaction(1_000, Hash::new(b"pubkey"));
    let block = make_test_block(vec![tx], current_time + 2 * 60 * 60 + 1, u64::MAX, 0);

    let res = consensus.validate_block(&block, &dag, &tree, current_time);
    assert!(res.is_err());
    assert!(format!("{}", res.unwrap_err()).contains("too far in the future"));
}

#[test]
fn ghostdag_validate_block_invalid_pow_fails() {
    let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let dag = Dag::new();
    let tree = VerkleTree::new(MemoryStorage::new()).expect("Verkle tree create");
    let consensus = GhostDag::new(1);

    let tx = make_coinbase_transaction(1_000, Hash::new(b"pubkey"));
    let block = make_test_block(vec![tx], current_time, 1, 0);

    let res = consensus.validate_block(&block, &dag, &tree, current_time);
    assert!(res.is_err());
    assert!(format!("{}", res.unwrap_err()).contains("Invalid PoW"));
}

#[test]
fn ghostdag_validate_block_verkle_output_collision_fails() {
    let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let dag = Dag::new();
    let mut tree = VerkleTree::new(MemoryStorage::new()).expect("Verkle tree create");
    let consensus = GhostDag::new(1);

    let tx = make_coinbase_transaction(1_000, Hash::new(b"pubkey"));
    let output_key = tx.hash_with_index(0);

    // Insert collision key in Verkle tree to trigger collision rejection
    tree.insert(output_key, tx.outputs[0].serialize());

    let block = make_test_block(vec![tx], current_time, u64::MAX, 0);

    let res = consensus.validate_block(&block, &dag, &tree, current_time);
    assert!(res.is_err());
    assert!(format!("{}", res.unwrap_err()).contains("Output key collision"));
}
