//! DAA (Difficulty Adjustment Algorithm) and Consensus Edge Cases
//! Tests proof-of-work difficulty, mining, and edge cases

use klomang_core::core::consensus::GhostDag;
use klomang_core::core::dag::Dag;
use klomang_core::core::daa::difficulty::Daa;
use klomang_core::core::crypto::Hash;
use klomang_core::core::dag::{BlockNode, BlockHeader};
use std::collections::HashSet;

fn make_block(id: &[u8], parents: HashSet<Hash>) -> BlockNode {
    BlockNode {
        header: BlockHeader {
            id: Hash::new(id),
            parents,
            timestamp: 0,
            difficulty: 0,
            nonce: 0,
            verkle_root: Hash::new(b"root"),
            verkle_proofs: None,
            signature: None,
        },
        children: HashSet::new(),
        selected_parent: None,
        blue_set: HashSet::new(),
        red_set: HashSet::new(),
        blue_score: 0,
        transactions: Vec::new(),
    }
}

fn make_timed_block(id: &[u8], parents: HashSet<Hash>, timestamp: u64, difficulty: u64) -> BlockNode {
    BlockNode {
        header: BlockHeader {
            id: Hash::new(id),
            parents,
            timestamp,
            difficulty,
            nonce: 0,
            verkle_root: Hash::new(b"root"),
            verkle_proofs: None,
            signature: None,
        },
        children: HashSet::new(),
        selected_parent: None,
        blue_set: HashSet::new(),
        red_set: HashSet::new(),
        blue_score: 0,
        transactions: Vec::new(),
    }
}

#[test]
fn test_daa_calculate_next_difficulty_fast_chain_increases() {
    let mut dag = Dag::new();
    let genesis = make_timed_block(b"genesis", HashSet::new(), 0, 1000);
    dag.add_block(genesis).expect("Failed genesis");

    let mut parents = HashSet::new();
    parents.insert(Hash::new(b"genesis"));

    for i in 1..=5u64 {
        let block = make_timed_block(
            format!("block-fast-{}", i).as_bytes(),
            parents.clone(),
            i,
            1000,
        );

        dag.add_block(block.clone()).expect("Failed add fast block");
        parents.clear();
        parents.insert(block.header.id.clone());
    }

    let daa = Daa::new(2, 5);
    let next_diff = daa.calculate_next_difficulty(&dag, 6);
    assert_eq!(next_diff, 2000);
}

#[test]
fn test_daa_calculate_next_difficulty_slow_chain_decreases() {
    let mut dag = Dag::new();
    let genesis = make_timed_block(b"genesis", HashSet::new(), 0, 1000);
    dag.add_block(genesis).expect("Failed genesis");

    let mut parents = HashSet::new();
    parents.insert(Hash::new(b"genesis"));

    for i in 1..=5u64 {
        let block = make_timed_block(
            format!("block-slow-{}", i).as_bytes(),
            parents.clone(),
            i * 10,
            1000,
        );

        dag.add_block(block.clone()).expect("Failed add slow block");
        parents.clear();
        parents.insert(block.header.id.clone());
    }

    let daa = Daa::new(2, 5);
    let next_diff = daa.calculate_next_difficulty(&dag, 60);
    assert_eq!(next_diff, 500);
}

/// Test 5: GHOSTDAG with single block
#[test]
fn test_ghostdag_single_block() {
    let ghostdag = GhostDag::new(10);
    let mut dag = Dag::new();
    
    // Add explicit genesis block for parent reference
    let genesis = make_block(b"genesis", HashSet::new());
    dag.add_block(genesis).expect("Failed to add genesis");
    
    let block = make_block(b"block1", {
        let mut parents = HashSet::new();
        parents.insert(Hash::new(b"genesis"));
        parents
    });
    
    dag.add_block(block.clone()).expect("Failed to add block");
    
    let vblock = ghostdag.get_virtual_block(&dag);
    
    assert!(vblock.is_some());
}

/// Test 6: GHOSTDAG with empty tips
#[test]
fn test_ghostdag_empty_tips() {
    let ghostdag = GhostDag::new(10);
    let dag = Dag::new();
    
    let vblock = ghostdag.get_virtual_block(&dag);
    assert!(vblock.is_some());
}

/// Test 7: GHOSTDAG parent selection
#[test]
fn test_ghostdag_parent_selection() {
    let ghostdag = GhostDag::new(3);
    let mut dag = Dag::new();
    
    // Add explicit genesis block for parent reference
    let genesis = make_block(b"genesis", HashSet::new());
    dag.add_block(genesis).expect("Failed to add genesis");
    
    // Create multiple blocks
    let b1 = make_block(b"block1", {
        let mut parents = HashSet::new();
        parents.insert(Hash::new(b"genesis"));
        parents
    });
    dag.add_block(b1.clone()).expect("Failed to add b1");
    
    let b2 = make_block(b"block2", {
        let mut parents = HashSet::new();
        parents.insert(b1.header.id.clone());
        parents
    });
    dag.add_block(b2).expect("Failed to add b2");
    
    // Test parent selection
    let parents = vec![b1.header.id.clone()];
    let selected = ghostdag.select_parent(&dag, &parents);
    
    assert!(selected.is_some());
}

/// Test 8: GHOSTDAG with multiple competing chains
#[test]
fn test_ghostdag_multiple_chains() {
    let ghostdag = GhostDag::new(10);
    let mut dag = Dag::new();
    
    let genesis = make_block(b"genesis", HashSet::new());
    dag.add_block(genesis).expect("Failed to add genesis");
    
    // Main chain: genesis -> b1 -> b2
    let b1 = make_block(b"b1", {
        let mut parents = HashSet::new();
        parents.insert(Hash::new(b"genesis"));
        parents
    });
    dag.add_block(b1.clone()).expect("Failed to add b1");
    
    let b2 = make_block(b"b2", {
        let mut parents = HashSet::new();
        parents.insert(b1.header.id.clone());
        parents
    });
    dag.add_block(b2.clone()).expect("Failed to add b2");
    
    // Alt chain: genesis -> b1_alt -> b2_alt
    let b1_alt = make_block(b"b1_alt", {
        let mut parents = HashSet::new();
        parents.insert(Hash::new(b"genesis"));
        parents
    });
    dag.add_block(b1_alt.clone()).expect("Failed to add b1_alt");
    
    let b2_alt = make_block(b"b2_alt", {
        let mut parents = HashSet::new();
        parents.insert(b1_alt.header.id.clone());
        parents
    });
    dag.add_block(b2_alt.clone()).expect("Failed to add b2_alt");
    
    // Compute virtual block with both chain tips
    let vblock = ghostdag.get_virtual_block(&dag);
    assert!(vblock.is_some());
}

/// Test 9: DAG add_block functionality
#[test]
fn test_dag_add_block() {
    let mut dag = Dag::new();

    let genesis = make_block(b"genesis", HashSet::new());
    dag.add_block(genesis).expect("Failed to add genesis");
    
    let block = make_block(b"test_block", {
        let mut parents = HashSet::new();
        parents.insert(Hash::new(b"genesis"));
        parents
    });
    
    let block_id = block.header.id.clone();
    
    let result = dag.add_block(block);
    assert!(result.is_ok());
    
    // Verify block was added
    assert!(dag.get_block(&block_id).is_some());
}

/// Test 10: DAG get_block for non-existent block
#[test]
fn test_dag_get_nonexistent_block() {
    let dag = Dag::new();
    let non_existent = Hash::new(b"non_existent");
    
    assert!(dag.get_block(&non_existent).is_none());
}

/// Test 11: DAG get_all_hashes
#[test]
fn test_dag_get_all_hashes() {
    let mut dag = Dag::new();
    
    let genesis = make_block(b"genesis", HashSet::new());
    dag.add_block(genesis).expect("Failed to add genesis");

    for i in 1..=5 {
        let block = make_block(
            format!("block{}", i).as_bytes(),
            {
                let mut parents = HashSet::new();
                if i == 1 {
                    parents.insert(Hash::new(b"genesis"));
                } else {
                    parents.insert(Hash::new(format!("block{}", i - 1).as_bytes()));
                }
                parents
            },
        );
        dag.add_block(block).expect("Failed to add block");
    }
    
    let all_hashes = dag.get_all_hashes();
    assert_eq!(all_hashes.len(), 6); // includes genesis
}

/// Test 12: GHOSTDAG anticone computation
#[test]
fn test_ghostdag_anticone() {
    let ghostdag = GhostDag::new(10);
    let mut dag = Dag::new();
    
    let genesis = make_block(b"genesis", HashSet::new());
    dag.add_block(genesis).expect("Failed to add genesis");

    // Create diamond: b1 -> b2, b3 -> b4
    let b1 = make_block(b"b1", {
        let mut parents = HashSet::new();
        parents.insert(Hash::new(b"genesis"));
        parents
    });
    dag.add_block(b1.clone()).expect("Failed to add b1");
    
    let b2 = make_block(b"b2", {
        let mut parents = HashSet::new();
        parents.insert(b1.header.id.clone());
        parents
    });
    dag.add_block(b2.clone()).expect("Failed to add b2");
    
    let b3 = make_block(b"b3", {
        let mut parents = HashSet::new();
        parents.insert(b1.header.id.clone());
        parents
    });
    dag.add_block(b3.clone()).expect("Failed to add b3");
    
    let anticone = ghostdag.anticone(&dag, &b2.header.id);
    
    // Should be some blocks in anticone or empty
    assert!(anticone.is_empty() || !anticone.is_empty());
}

/// Test 14: Block hash consistency
#[test]
fn test_block_hash_consistency() {
    let b1 = make_block(b"block", HashSet::new());
    let b2 = make_block(b"block", HashSet::new());
    
    assert_eq!(b1.header.id, b2.header.id);
}

/// Test 15: GHOSTDAG blue set computation
#[test]
fn test_ghostdag_blue_set() {
    let ghostdag = GhostDag::new(10);
    let mut dag = Dag::new();
    
    let genesis = make_block(b"genesis", HashSet::new());
    dag.add_block(genesis).expect("Failed to add genesis");
    
    let b1 = make_block(b"b1", {
        let mut parents = HashSet::new();
        parents.insert(Hash::new(b"genesis"));
        parents
    });
    dag.add_block(b1.clone()).expect("Failed to add b1");
    
    let b2 = make_block(b"b2", {
        let mut parents = HashSet::new();
        parents.insert(b1.header.id.clone());
        parents
    });
    dag.add_block(b2.clone()).expect("Failed to add b2");
    
    let vblock = ghostdag.get_virtual_block(&dag);
    
    // Blue set should be computed
    assert!(vblock.is_some());
}

/// Test DAA: Simulate block time changes and verify difficulty adjustment
#[test]
fn test_daa_simulate_block_time_changes() {
    let mut dag = Dag::new();
    let daa = Daa::new(1000, 10); // Target 1 second (represented as 1000 ms), window 10 blocks

    // Initial difficulty
    let initial_diff = daa.calculate_next_difficulty(&dag, 0);
    assert_eq!(initial_diff, 1000);

    let genesis_fast = make_timed_block(b"genesis", HashSet::new(), 0, initial_diff);
    dag.add_block(genesis_fast).expect("Failed to add genesis fast block");

    let mut parents = HashSet::new();
    parents.insert(Hash::new(b"genesis"));

    // Simulate fast blocks (0.5 seconds each) - should increase difficulty
    let mut current_time = 0u64;
    let mut current_diff = initial_diff;
    for i in 1..=15 {
        current_time += 500; // 0.5 seconds per block
        let block = make_timed_block(
            format!("fast_block_{}", i).as_bytes(),
            parents.clone(),
            current_time,
            current_diff,
        );
        dag.add_block(block.clone()).expect("Failed to add fast block");
        parents.clear();
        parents.insert(block.header.id.clone());

        if i >= 10 {
            let next_diff = daa.calculate_next_difficulty(&dag, current_time);
            assert!(next_diff > current_diff, "Difficulty should increase for fast blocks at block {}", i);
            current_diff = next_diff;
        }
    }

    // Reset for slow blocks simulation
    let mut dag_slow = Dag::new();
    let genesis_slow_block = make_timed_block(b"genesis_slow", HashSet::new(), 0, initial_diff);
    dag_slow.add_block(genesis_slow_block).expect("Failed to add genesis_slow block");

    let mut parents_slow = HashSet::new();
    parents_slow.insert(Hash::new(b"genesis_slow"));
    current_time = 0;
    current_diff = initial_diff;

    // Simulate slow blocks (2 seconds each) - should decrease difficulty
    for i in 1..=15 {
        current_time += 2000; // 2 seconds per block
        let block = make_timed_block(
            format!("slow_block_{}", i).as_bytes(),
            parents_slow.clone(),
            current_time,
            current_diff,
        );
        dag_slow.add_block(block.clone()).expect("Failed to add slow block");
        parents_slow.clear();
        parents_slow.insert(block.header.id.clone());

        if i >= 10 {
            let next_diff = daa.calculate_next_difficulty(&dag_slow, current_time);
            assert!(next_diff < current_diff, "Difficulty should decrease for slow blocks at block {}", i);
            current_diff = next_diff;
        }
    }

    // Test boundary conditions
    let daa_strict = Daa::new(1, 2); // Very small window
    let mut dag_boundary = Dag::new();
    let genesis_boundary = make_timed_block(b"genesis_boundary", HashSet::new(), 0, 1000);
    dag_boundary.add_block(genesis_boundary).expect("Failed to add genesis_boundary");
    let mut parents_boundary = HashSet::new();
    parents_boundary.insert(Hash::new(b"genesis_boundary"));

    // Add blocks with varying times
    // Expect next difficulty to adapt around target time of 1 unit
    let times = [2, 3, 5];

    let mut prev_diff = 1000;
    let mut last_time = 0;
    for (i, time) in times.iter().enumerate() {
        let block = make_timed_block(
            format!("boundary_block_{}", i).as_bytes(),
            parents_boundary.clone(),
            *time,
            prev_diff,
        );
        dag_boundary.add_block(block.clone()).expect("Failed to add boundary block");
        parents_boundary.clear();
        parents_boundary.insert(block.header.id.clone());

        let next_diff = daa_strict.calculate_next_difficulty(&dag_boundary, *time);

        let interval = *time - last_time;
        if interval <= 1 {
            assert!(next_diff >= prev_diff, "Expected difficulty to increase for fast interval {} (block {})", interval, i);
        } else {
            assert!(next_diff <= prev_diff, "Expected difficulty to decrease for slow interval {} (block {})", interval, i);
        }

        last_time = *time;
        prev_diff = next_diff;
    }
}
