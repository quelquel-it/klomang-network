use crate::core::crypto::Hash;
use crate::core::state::transaction::Transaction;
use std::collections::HashSet;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct BlockHeader {
    pub id: Hash,
    pub parents: HashSet<Hash>,
    pub timestamp: u64,
    pub difficulty: u64,
    pub nonce: u64,
    pub verkle_root: Hash,
    pub verkle_proofs: Option<Vec<u8>>,
    pub signature: Option<Vec<u8>>, // Schnorr signature
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct BlockNode {
    pub header: BlockHeader,
    pub children: HashSet<Hash>,
    pub selected_parent: Option<Hash>,
    pub blue_set: HashSet<Hash>,
    pub red_set: HashSet<Hash>,
    pub blue_score: u64,
    pub transactions: Vec<Transaction>,
}

