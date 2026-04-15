use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const STORAGE_SCHEMA_VERSION: u32 = 1;

pub fn schema_description() -> &'static str {
    "Klomang node storage schema version 1 with full blockchain state"
}

// ============================================
// KEY TYPES AND SERIALIZATION
// ============================================

/// Serialize value with bincode
pub fn serialize_value<T: Serialize>(value: &T) -> Result<Vec<u8>, bincode::Error> {
    bincode::serialize(value)
}

/// Deserialize value with bincode
pub fn deserialize_value<'a, T: Deserialize<'a>>(data: &'a [u8]) -> Result<T, bincode::Error> {
    bincode::deserialize(data)
}

// ============================================
// BLOCK STORAGE
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockValue {
    pub hash: Vec<u8>,
    pub header_bytes: Vec<u8>,
    pub transactions: Vec<Vec<u8>>,
    pub timestamp: u64,
}

impl BlockValue {
    pub fn from_parts(hash: Vec<u8>, header_bytes: Vec<u8>, transactions: Vec<Vec<u8>>, timestamp: u64) -> Self {
        Self {
            hash,
            header_bytes,
            transactions,
            timestamp,
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, bincode::Error> {
        serialize_value(self)
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, bincode::Error> {
        deserialize_value(data)
    }
}

// ============================================
// HEADER STORAGE
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderValue {
    pub block_hash: Vec<u8>,
    pub parent_hashes: Vec<Vec<u8>>,
    pub timestamp: u64,
    pub difficulty: u64,
    pub nonce: u64,
    pub verkle_root: Vec<u8>,
    pub height: u64, // Block height for pruning purposes
}

impl HeaderValue {
    pub fn to_bytes(&self) -> Result<Vec<u8>, bincode::Error> {
        serialize_value(self)
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, bincode::Error> {
        deserialize_value(data)
    }
}

// ============================================
// TRANSACTION STORAGE
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionValue {
    pub tx_hash: Vec<u8>,
    pub inputs: Vec<TransactionInput>,
    pub outputs: Vec<TransactionOutput>,
    pub fee: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionInput {
    pub previous_tx_hash: Vec<u8>,
    pub output_index: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionOutput {
    pub amount: u64,
    pub script: Vec<u8>,
    pub owner: Vec<u8>,
}

impl TransactionValue {
    pub fn to_bytes(&self) -> Result<Vec<u8>, bincode::Error> {
        serialize_value(self)
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, bincode::Error> {
        deserialize_value(data)
    }
}

// ============================================
// UTXO STORAGE
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UtxoValue {
    pub amount: u64,
    pub script: Vec<u8>,
    pub owner: Vec<u8>,
    pub block_height: u32,
}

impl UtxoValue {
    pub fn new(amount: u64, script: Vec<u8>, owner: Vec<u8>, block_height: u32) -> Self {
        Self {
            amount,
            script,
            owner,
            block_height,
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, bincode::Error> {
        serialize_value(self)
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, bincode::Error> {
        deserialize_value(data)
    }
}

/// Composite key for UTXO: tx_hash (32 bytes) + output_index (4 bytes)
pub fn make_utxo_key(tx_hash: &[u8], output_index: u32) -> Vec<u8> {
    let mut key = Vec::with_capacity(36);
    key.extend_from_slice(tx_hash);
    key.extend_from_slice(&output_index.to_le_bytes());
    key
}

/// Parse UTXO key back to components
pub fn parse_utxo_key(key: &[u8]) -> Option<(&[u8], u32)> {
    if key.len() != 36 {
        return None;
    }
    let (tx_hash, index_bytes) = key.split_at(32);
    let mut index_array = [0u8; 4];
    index_array.copy_from_slice(index_bytes);
    let output_index = u32::from_le_bytes(index_array);
    Some((tx_hash, output_index))
}

// ============================================
// UTXO SPENT INDEX STORAGE
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UtxoSpentValue {
    pub spent_by_tx_hash: Vec<u8>,
    pub input_index: u32,
    pub spent_at_block_height: u32,
}

impl UtxoSpentValue {
    pub fn new(spent_by_tx_hash: Vec<u8>, input_index: u32, spent_at_block_height: u32) -> Self {
        Self {
            spent_by_tx_hash,
            input_index,
            spent_at_block_height,
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, bincode::Error> {
        serialize_value(self)
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, bincode::Error> {
        deserialize_value(data)
    }
}

// ============================================
// VERKLE STATE STORAGE
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerkleStateValue {
    pub commitment_path: Vec<u8>,
    pub node_value: Vec<u8>,
    pub is_leaf: bool,
}

impl VerkleStateValue {
    pub fn new(commitment_path: Vec<u8>, node_value: Vec<u8>, is_leaf: bool) -> Self {
        Self {
            commitment_path,
            node_value,
            is_leaf,
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, bincode::Error> {
        serialize_value(self)
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, bincode::Error> {
        deserialize_value(data)
    }
}

// ============================================
// DAG STORAGE
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagNodeValue {
    pub block_hash: Vec<u8>,
    pub parent_hashes: Vec<Vec<u8>>,
    pub blue_set: Vec<Vec<u8>>,
    pub red_set: Vec<Vec<u8>>,
    pub blue_score: u64,
}

impl DagNodeValue {
    pub fn new(
        block_hash: Vec<u8>,
        parent_hashes: Vec<Vec<u8>>,
        blue_set: Vec<Vec<u8>>,
        red_set: Vec<Vec<u8>>,
        blue_score: u64,
    ) -> Self {
        Self {
            block_hash,
            parent_hashes,
            blue_set,
            red_set,
            blue_score,
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, bincode::Error> {
        serialize_value(self)
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, bincode::Error> {
        deserialize_value(data)
    }
}

// ============================================
// DAG TIPS STORAGE
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagTipsValue {
    pub tip_blocks: Vec<Vec<u8>>,
    pub last_updated_height: u32,
}

impl DagTipsValue {
    pub fn new(tip_blocks: Vec<Vec<u8>>, last_updated_height: u32) -> Self {
        Self {
            tip_blocks,
            last_updated_height,
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, bincode::Error> {
        serialize_value(self)
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, bincode::Error> {
        deserialize_value(data)
    }
}
