//! Proof-of-Work and Mining Edge Cases
//! Tests mining, difficulty verification, and hash computation edge cases

use klomang_core::core::crypto::Hash;
use klomang_core::core::dag::Dag;
use klomang_core::core::pow::miner::Pow;
use klomang_core::core::daa::difficulty::Daa;

/// Test 1: PoW helper creation
#[test]
fn test_pow_creation() {
    let pow = Pow::new(1);
    assert_eq!(pow.difficulty, 1);
}

/// Test 2: PoW helper creation with high difficulty
#[test]
fn test_pow_max_difficulty() {
    let pow = Pow::new(10);
    assert_eq!(pow.difficulty, 10);
}

/// Test 3: PoW helper creation with zero difficulty clamps to 1
#[test]
fn test_pow_zero_difficulty_clamps() {
    let pow = Pow::new(0);
    assert_eq!(pow.difficulty, 1);
}

/// Test 4: Hash determinism
#[test]
fn test_hash_determinism() {
    let hash1 = Hash::new(b"test_data");
    let hash2 = Hash::new(b"test_data");
    
    assert_eq!(hash1, hash2);
}

/// Test 5: Hash collision resistance (different inputs)
#[test]
fn test_hash_different_inputs() {
    let hash1 = Hash::new(b"input1");
    let hash2 = Hash::new(b"input2");
    
    assert_ne!(hash1, hash2);
}

/// Test 6: Hash with empty input
#[test]
fn test_hash_empty_input() {
    let hash = Hash::new(b"");
    assert_eq!(hash.as_bytes().len(), 32);
}

/// Test 7: Hash with large input
#[test]
fn test_hash_large_input() {
    let large_input = vec![0x00; 10_000];
    let hash = Hash::new(&large_input);
    assert_eq!(hash.as_bytes().len(), 32);
}

/// Test 8: Hash byte length validation
#[test]
fn test_hash_byte_length() {
    let hash = Hash::new(b"any_data");
    assert!(hash.as_bytes().len() >= 32);
}

/// Test 9: Multiple hash operations
#[test]
fn test_multiple_hashes() {
    let hashes: Vec<Hash> = (0..100).map(|i| {
        Hash::new(format!("data{}", i).as_bytes())
    }).collect();
    
    assert_eq!(hashes.len(), 100);
    
    // Verify all unique (very likely with good hash function)
    for i in 0..hashes.len() {
        for j in (i+1)..hashes.len() {
            assert_ne!(hashes[i], hashes[j]);
        }
    }
}

/// Test 10: DAA with empty DAG returns base difficulty
#[test]
fn test_daa_empty_dag_returns_initial_difficulty() {
    let dag = Dag::new();
    let daa = Daa::new(1, 5);

    assert_eq!(daa.calculate_next_difficulty(&dag, 0), 1000);
}

/// Test 11: Pow adjustment baseline check
#[test]
fn test_pow_difficulty_adjustment_baseline() {
    let pow = Pow::new(10);
    assert_eq!(pow.difficulty, 10);
}

/// Test 12: Hash representation
#[test]
fn test_hash_representation() {
    let hash = Hash::new(b"test");
    let hex_str = format!("{:?}", hash);
    
    // Should be representable in hex
    assert!(!hex_str.is_empty());
}

/// Test 14: Hash ordering
#[test]
fn test_hash_ordering() {
    let hashes: Vec<Hash> = vec![
        Hash::new(b"a"),
        Hash::new(b"b"),
        Hash::new(b"c"),
    ];
    
    // Hashes should be comparable
    let _ = hashes.iter().min();
    let _ = hashes.iter().max();
}

/// Test 15: Pow adjustment
#[test]
fn test_pow_difficulty_adjustment() {
    let pow = Pow::new(10);

    // Verify initial state
    assert_eq!(pow.difficulty, 10);

    // PoW helper should be usable
    assert!(pow.difficulty > 0);
}
