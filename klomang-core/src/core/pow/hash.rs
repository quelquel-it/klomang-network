use crate::core::crypto::Hash;

/// Calculate hash by combining required block header fields in deterministic order.
/// Fields: timestamp, difficulty, parent hashes (sorted), merit, nonce, transaction root.
pub fn calculate_hash(
    timestamp: u64,
    difficulty: u64,
    parent_hashes: &[Hash],
    merit: u64,
    nonce: u64,
    tx_merkle_root: &Hash,
) -> Hash {
    let mut data = Vec::new();
    data.extend_from_slice(&timestamp.to_le_bytes());
    data.extend_from_slice(&difficulty.to_le_bytes());

    let mut parents: Vec<_> = parent_hashes.iter().collect();
    parents.sort();
    for parent in parents {
        data.extend_from_slice(parent.as_bytes());
    }

    data.extend_from_slice(&merit.to_le_bytes());
    data.extend_from_slice(&nonce.to_le_bytes());
    data.extend_from_slice(tx_merkle_root.as_bytes());

    Hash::new(&data)
}

/// Legacy helper for raw header bytes; may be used for deterministic hashing of structured bytes.
pub fn calculate_hash_raw(header: &[u8]) -> Hash {
    Hash::new(header)
}

/// Check if hash meets the target difficulty
pub fn is_valid_pow(hash: &Hash, target: u64) -> bool {
    // Convert first 8 bytes of hash to u64 (little endian)
    let hash_bytes = hash.as_bytes();
    if hash_bytes.len() < 8 {
        return false;
    }
    let hash_val = u64::from_le_bytes(hash_bytes[0..8].try_into().unwrap_or([0u8; 8]));
    hash_val < target
}

/// Parameters for block mining
#[derive(Clone)]
pub struct BlockMiningParams<'a> {
    pub header: &'a [u8],
    pub target: u64,
    pub miner_address: &'a [u8],
    pub node_reward_address: &'a [u8],
    pub timestamp: u64,
    pub difficulty: u64,
    pub parent_hashes: &'a [crate::core::crypto::Hash],
    pub verkle_root: &'a [u8; 32],
}

/// Mine a block by finding a valid nonce with miner and node reward addresses
/// Includes address fields in hashed input to ensure explicit minting destination.
/// This guards coinbase issuance by making reward addresses part of PoW input.
/// Now includes all critical header fields for comprehensive pre-image attack protection.
pub fn mine_block(params: &BlockMiningParams) -> Option<u64> {
    if params.miner_address.is_empty() || params.node_reward_address.is_empty() {
        return None;
    }

    let tx_merkle_root = Hash::new(params.header); // deterministic representation of payload header for PoW

    for nonce in 0..=u64::MAX {
        // Using the structured hash function that covers all header fields deterministically
        let hash = calculate_hash(
            params.timestamp,
            params.difficulty,
            params.parent_hashes,
            0, // merit (blue_score) unknown for mining, typically set to 0 during candidate mining
            nonce,
            &tx_merkle_root,
        );

        // Include reward addresses as additional nonce-like entropy via parent hash derivation path
        // (to preserve previous behavior and protect candidate pool uniqueness)
        let mut with_rewards = Vec::new();
        with_rewards.extend_from_slice(hash.as_bytes());
        with_rewards.extend_from_slice(params.verkle_root);
        with_rewards.extend_from_slice(params.miner_address);
        with_rewards.extend_from_slice(params.node_reward_address);

        let final_hash = Hash::new(&with_rewards);

        if is_valid_pow(&final_hash, params.target) {
            return Some(nonce);
        }
    }
    None
}