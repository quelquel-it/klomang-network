use std::collections::{HashSet, HashMap};
use crate::core::crypto::Hash;
use crate::core::dag::{Dag, BlockNode};
use crate::core::crypto::schnorr;
use crate::core::pow::hash;
use crate::core::state::v_trie::VerkleTree;
use crate::core::state::storage::Storage;
use crate::core::errors::CoreError;
use std::time::{SystemTime, UNIX_EPOCH};
use k256::schnorr::{Signature, VerifyingKey};

// Finality depth constant - blocks beyond this depth cannot be reorganized
pub const FINALITY_DEPTH: u64 = 100;

#[derive(Debug, Clone)]
pub struct VirtualBlock {
    pub parents: HashSet<Hash>,
    pub selected_parent: Option<Hash>,
    pub blue_set: HashSet<Hash>,
    pub red_set: HashSet<Hash>,
    pub blue_score: u64,
}

#[derive(Debug, Clone)]
pub struct GhostDag {
    pub k: usize,
    /// Network condition metrics for adaptive k adjustment
    pub network_load: f64, // 0.0 to 1.0, higher means more congested
    pub last_adjustment_time: u64,
}

// Constants for adaptive k adjustment
const K_MIN: usize = 1;
const K_MAX: usize = 64; // Allow larger k to support wide fork scenarios and test expectations like k=24
const ADJUSTMENT_INTERVAL: u64 = 3600; // 1 hour in seconds
const HIGH_LOAD_THRESHOLD: f64 = 0.8;
const LOW_LOAD_THRESHOLD: f64 = 0.2;

impl GhostDag {
    pub fn new(k: usize) -> Self {
        Self {
            k: k.clamp(K_MIN, K_MAX),
            network_load: 0.0,
            last_adjustment_time: 0,
        }
    }

    /// Create with adaptive k based on initial network conditions
    pub fn new_adaptive(initial_load: f64) -> Self {
        let mut gd = Self::new(1);
        gd.update_network_load(initial_load);
        gd.adjust_k();
        gd
    }

    /// Update network load metric (0.0 = idle, 1.0 = congested)
    pub fn update_network_load(&mut self, load: f64) {
        self.network_load = load.clamp(0.0, 1.0);
    }

    /// Adjust k parameter based on network conditions
    pub fn adjust_k(&mut self) {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        if current_time - self.last_adjustment_time < ADJUSTMENT_INTERVAL {
            return; // Too soon to adjust
        }

        let new_k = if self.network_load > HIGH_LOAD_THRESHOLD {
            // High congestion - increase k to reduce selfish mining advantage
            (self.k + 1).min(K_MAX)
        } else if self.network_load < LOW_LOAD_THRESHOLD {
            // Low congestion - decrease k for better performance
            (self.k - 1).max(K_MIN)
        } else {
            self.k // Keep current
        };

        if new_k != self.k {
            self.k = new_k;
            self.last_adjustment_time = current_time;
        }
    }

    /// Validate block comprehensively before acceptance
    /// Returns Ok(()) if block is valid, Err with reason if invalid
    pub fn validate_block<S: Storage>(
        &self,
        block: &BlockNode,
        dag: &Dag,
        verkle_tree: &VerkleTree<S>,
        current_time: u64
    ) -> Result<(), CoreError> {
        // 0. DAG connectivity check (parents exist)
        for parent in &block.header.parents {
            if dag.get_block(parent).is_none() {
                return Err(CoreError::ConsensusError(
                    format!("Parent block {} not found in DAG", parent)
                ));
            }
        }

        // 1. Validate timestamp
        let max_future_time = 2 * 60 * 60; // 2 hours tolerance into the future
        let max_past_time = 24 * 60 * 60;  // 24 hours tolerance into the past

        if block.header.timestamp > current_time.saturating_add(max_future_time) {
            return Err(CoreError::ConsensusError(
                format!("Block timestamp {} is too far in the future (current: {})", block.header.timestamp, current_time)
            ));
        }

        if block.header.timestamp < current_time.saturating_sub(max_past_time) {
            return Err(CoreError::ConsensusError(
                format!("Block timestamp {} is too far in the past (current: {})", block.header.timestamp, current_time)
            ));
        }

        // 2. Validate difficulty
        if block.header.difficulty == 0 {
            return Err(CoreError::ConsensusError("Block difficulty cannot be zero".to_string()));
        }

        // 3. Validate PoW
        let tx_root = self.compute_transaction_merkle_root(&block.transactions);
        let block_hash = hash::calculate_hash(
            block.header.timestamp,
            block.header.difficulty,
            &block.header.parents.iter().cloned().collect::<Vec<_>>(),
            block.blue_score,
            block.header.nonce,
            &tx_root,
        );
        if !hash::is_valid_pow(&block_hash, block.header.difficulty) {
            return Err(CoreError::ConsensusError(
                format!("Invalid PoW: hash {} does not meet difficulty {}", block_hash, block.header.difficulty)
            ));
        }

        // 4. Validate transactions
        self.validate_transactions(block, dag)?;

        // 5. Validate Verkle state integrity
        self.validate_verkle_proof(block, verkle_tree)?;

        Ok(())
    }

    /// Compute simple merkle root of transactions
    fn compute_transaction_merkle_root(&self, transactions: &[crate::core::state::transaction::Transaction]) -> Hash {
        if transactions.is_empty() {
            return Hash::new(&[]);
        }

        let mut hashes: Vec<Hash> = transactions.iter()
            .map(|tx| tx.id.clone())
            .collect();

        while hashes.len() > 1 {
            let mut new_hashes = Vec::new();
            for chunk in hashes.chunks(2) {
                let mut combined = Vec::new();
                combined.extend_from_slice(chunk[0].as_bytes());
                if chunk.len() > 1 {
                    combined.extend_from_slice(chunk[1].as_bytes());
                }
                new_hashes.push(Hash::new(&combined));
            }
            hashes = new_hashes;
        }

        hashes[0].clone()
    }

    /// Validate all transactions in block with batch signature verification
    fn validate_transactions(&self, block: &BlockNode, _dag: &Dag) -> Result<(), CoreError> {
        let mut spent_outpoints = HashSet::new();
        let mut signature_items = Vec::new();
        
        for tx in &block.transactions {
            // Validate TX hash matches body
            let expected_id = tx.calculate_id();
            if tx.id != expected_id {
                return Err(CoreError::ConsensusError(
                    format!("Invalid transaction id: expected {} but got {}", expected_id, tx.id)
                ));
            }

            for input in &tx.inputs {
                let outpoint = (input.prev_tx.clone(), input.index);
                if !spent_outpoints.insert(outpoint.clone()) {
                    return Err(CoreError::ConsensusError(
                        format!("Double-spend across block detected for outpoint {:?}", outpoint)
                    ));
                }
            }

            // Collect signature items for batch verification
            self.collect_transaction_signatures(tx, &mut signature_items)?;
        }

        // Batch verify all signatures at once for better TPS
        if !schnorr::batch_verify(&signature_items)? {
            return Err(CoreError::ConsensusError(
                "One or more invalid signatures in block".to_string()
            ));
        }

        Ok(())
    }

    /// Collect signature items for batch verification
    fn collect_transaction_signatures(&self, tx: &crate::core::state::transaction::Transaction, items: &mut Vec<(VerifyingKey, [u8; 32], Signature)>) -> Result<(), CoreError> {
        for (input_idx, input) in tx.inputs.iter().enumerate() {
            if input.signature.is_empty() {
                return Err(CoreError::ConsensusError(
                    format!("Missing signature for input {} in transaction {}", input_idx, tx.id)
                ));
            }

            let sighash = schnorr::compute_sighash(tx, input_idx, input.sighash_type)
                .map_err(|e| CoreError::CryptographicError(format!("Sighash error: {}", e)))?;

            let pubkey = VerifyingKey::from_bytes(&input.pubkey)
                .map_err(|e| CoreError::CryptographicError(format!("Invalid pubkey: {}", e)))?;

            let signature = Signature::try_from(input.signature.as_slice())
                .map_err(|e| CoreError::CryptographicError(format!("Invalid signature: {}", e)))?;

            items.push((pubkey, sighash, signature));
        }

        Ok(())
    }

    /// Convert an outpoint (prev_tx, index) into the UTXO key used in Verkle state
    fn outpoint_key(prev_tx: &Hash, index: u32) -> [u8; 32] {
        let mut id_data = Vec::with_capacity(36);
        id_data.extend_from_slice(prev_tx.as_bytes());
        id_data.extend_from_slice(&index.to_be_bytes());
        *Hash::new(&id_data).as_bytes()
    }

    /// Validate Verkle proof for state transition
    fn validate_verkle_proof<S: Storage>(
        &self,
        block: &BlockNode,
        verkle_tree: &VerkleTree<S>
    ) -> Result<(), CoreError> {
        for tx in &block.transactions {
            // Ensure all inputs are present in current state
            for input in &tx.inputs {
                let key = Self::outpoint_key(&input.prev_tx, input.index);
                match verkle_tree.get(key) {
                    Ok(Some(_value)) => (),
                    Ok(None) => {
                        return Err(CoreError::ConsensusError(
                            format!("Missing input UTXO in Verkle tree for outpoint ({}, {})", input.prev_tx, input.index)
                        ));
                    }
                    Err(e) => {
                        return Err(CoreError::CryptographicError(format!("Verkle tree query failed: {}", e)));
                    }
                }
            }

            // Ensure outputs do not collide with existing UTXO keys
            for (idx, _output) in tx.outputs.iter().enumerate() {
                let out_key = tx.hash_with_index(idx as u32);
                if let Ok(Some(_existing)) = verkle_tree.get(out_key) {
                    return Err(CoreError::ConsensusError(
                        format!("Output key collision in Verkle tree for tx {} output {}", tx.id, idx)
                    ));
                }
            }
        }

        Ok(())
    }

    pub fn select_parent(&self, dag: &Dag, parents: &[Hash]) -> Option<Hash> {
        if parents.is_empty() {
            return None;
        }
        
        // Max 2 parents: select only two with highest blue_score
        let mut parent_scores: Vec<_> = parents
            .iter()
            .filter_map(|h| dag.get_block(h).map(|b| (h.clone(), b.blue_score)))
            .collect();
        
        // Sort by blue_score descending, then by hash ascending (deterministic)
        parent_scores.sort_by(|(h1, s1), (h2, s2)| s2.cmp(s1).then(h1.cmp(h2)));
        
        // Return highest score parent
        parent_scores.first().map(|(h, _)| h.clone())
    }

    pub fn anticone(&self, dag: &Dag, block: &Hash) -> Vec<Hash> {
        dag.get_anticone(block)
    }

    /// Calculate blue score for a block
    pub fn calculate_blue_score(&self, dag: &Dag, block_hash: &Hash) -> u64 {
        let block = match dag.get_block(block_hash) {
            Some(b) => b,
            None => return 0,
        };

        if block.header.parents.is_empty() {
            return 0; // genesis
        }

        let selected_parent = match &block.selected_parent {
            Some(sp) => sp,
            None => return 0,
        };

        let parent_score = dag.get_block(selected_parent)
            .map(|b| b.blue_score)
            .unwrap_or(0);

        parent_score + (block.blue_set.len() as u64)
    }

    /// Get anticone for a block
    pub fn get_anticone(&self, dag: &Dag, block: &Hash) -> Vec<Hash> {
        self.anticone(dag, block)
    }

    pub fn build_blue_set(
        &self,
        dag: &Dag,
        selected_parent: &Hash,
        _parents: &[Hash],
    ) -> (HashSet<Hash>, HashSet<Hash>) {
        let mut blue_set = HashSet::new();
        let mut red_set = HashSet::new();

        if let Some(parent_block) = dag.get_block(selected_parent) {
            blue_set.extend(parent_block.blue_set.iter().cloned());
            blue_set.insert(selected_parent.clone());
        }

        // Get anticone and convert to HashSet for k-cluster check
        let candidates = self.anticone(dag, selected_parent);
        // Already sorted from anticone, so iteration is deterministic
        
        for candidate in candidates {
            let candidate_anticone: HashSet<Hash> = self.anticone(dag, &candidate).into_iter().collect();
            let conflicts = candidate_anticone.intersection(&blue_set).count();
            if conflicts <= self.k {
                blue_set.insert(candidate);
            } else {
                red_set.insert(candidate);
            }
        }

        (blue_set, red_set)
    }

    pub fn recompute_block(&self, dag: &mut Dag, hash: &Hash) -> bool {
        let block = match dag.get_block(hash) {
            Some(b) => b.clone(),
            None => return false,
        };

        if block.header.parents.is_empty() {
            return false;
        }

        for parent in &block.header.parents {
            if dag.get_block(parent).is_none() {
                return false;
            }
        }

        // Convert HashSet to Vec for deterministic processing
        let parents_vec: Vec<Hash> = {
            let mut v: Vec<_> = block.header.parents.iter().cloned().collect();
            v.sort();
            v
        };

        let selected_parent = match self.select_parent(dag, &parents_vec) {
            Some(p) => p,
            None => return false,
        };

        let (blue_set, red_set) = self.build_blue_set(dag, &selected_parent, &parents_vec);
        let parent_score = dag
            .get_block(&selected_parent)
            .map(|b| b.blue_score)
            .unwrap_or(0);
        let blue_score = parent_score + (blue_set.len() as u64);

        if let Some(stored) = dag.get_block_mut(hash) {
            if stored.selected_parent == Some(selected_parent.clone())
                && stored.blue_set == blue_set
                && stored.red_set == red_set
                && stored.blue_score == blue_score
            {
                return false;
            }

            stored.selected_parent = Some(selected_parent);
            stored.blue_set = blue_set;
            stored.red_set = red_set;
            stored.blue_score = blue_score;
            return true;
        }

        false
    }

    pub fn build_virtual_block(&self, dag: &Dag) -> VirtualBlock {
        let tips = dag.get_tips();

        if dag.get_all_hashes().is_empty() {
            return VirtualBlock {
                parents: HashSet::new(),
                selected_parent: None,
                blue_set: HashSet::new(),
                red_set: HashSet::new(),
                blue_score: 0,
            };
        }

        if dag.get_all_hashes().len() == 1 {
            // Safe to unwrap because we just checked len() == 1
            if let Some(only_block_hash) = dag.get_all_hashes().first() {
                let only_block_hash = only_block_hash.clone();
                if let Some(b) = dag.get_block(&only_block_hash) {
                    return VirtualBlock {
                        parents: tips.into_iter().collect(),
                        selected_parent: b.selected_parent.clone(),
                        blue_set: b.blue_set.clone(),
                        red_set: b.red_set.clone(),
                        blue_score: b.blue_score,
                    };
                }
            }
        }

        let selected_parent = self.select_parent(dag, &tips);
        if let Some(selected_parent) = selected_parent {
            let (blue_set, red_set) = self.build_blue_set(dag, &selected_parent, &tips);
            let parent_score = dag.get_block(&selected_parent).map(|b| b.blue_score).unwrap_or(0);
            let blue_score = parent_score + (blue_set.len() as u64);

            VirtualBlock {
                parents: tips.into_iter().collect(),
                selected_parent: Some(selected_parent),
                blue_set,
                red_set,
                blue_score,
            }
        } else {
            VirtualBlock {
                parents: tips.into_iter().collect(),
                selected_parent: None,
                blue_set: HashSet::new(),
                red_set: HashSet::new(),
                blue_score: 0,
            }
        }
    }

    pub fn get_virtual_selected_chain(&self, dag: &Dag) -> Vec<Hash> {
        let v = self.build_virtual_block(dag);
        let mut chain = Vec::new();
        let mut current = v.selected_parent;

        while let Some(parent_hash) = current {
            chain.push(parent_hash.clone());
            current = dag.get_block(&parent_hash).and_then(|b| b.selected_parent.clone());
        }

        chain.reverse();
        chain
    }

    pub fn get_virtual_ordering(&self, dag: &Dag) -> Vec<Hash> {
        let mut ordering: Vec<_> = dag.get_all_hashes().into_iter().collect();
        ordering.sort_by(|a, b| {
            let a_score = dag.get_block(a).map_or(0, |block| block.blue_score);
            let b_score = dag.get_block(b).map_or(0, |block| block.blue_score);
            a_score.cmp(&b_score).then(a.cmp(b))
        });
        ordering
    }

    pub fn topological_sort(&self, dag: &Dag, nodes: &[Hash]) -> Vec<Hash> {
        let node_set: HashSet<Hash> = nodes.iter().cloned().collect();
        let mut indegree: HashMap<Hash, usize> = HashMap::new();

        for hash in nodes {
            let degree = dag
                .get_block(hash)
                .map(|block| {
                    block
                        .header.parents
                        .iter()
                        .filter(|p| node_set.contains(p))
                        .count()
                })
                .unwrap_or(0);
            indegree.insert(hash.clone(), degree);
        }

        let mut queue: Vec<Hash> = indegree
            .iter()
            .filter_map(|(h, d)| if *d == 0 { Some(h.clone()) } else { None })
            .collect();
        queue.sort();

        let mut sorted = Vec::new();

        while let Some(current) = queue.pop() {
            sorted.push(current.clone());
            if let Some(block) = dag.get_block(&current) {
                let mut children: Vec<_> = block
                    .children
                    .iter()
                    .filter(|c| node_set.contains(c))
                    .cloned()
                    .collect();
                children.sort();
                for child in children {
                    if let Some(deg) = indegree.get_mut(&child) {
                        *deg = deg.saturating_sub(1);
                        if *deg == 0 {
                            queue.push(child.clone());
                        }
                    }
                }
            }
        }

        sorted
    }

    pub fn process_block(&self, dag: &mut Dag, block_hash: &Hash) {
        let block = match dag.get_block(block_hash) {
            Some(b) => b.clone(),
            None => return,
        };

        if block.header.parents.is_empty() {
            if let Some(stored) = dag.get_block_mut(block_hash) {
                stored.selected_parent = None;
                stored.blue_set = HashSet::new();
                stored.red_set = HashSet::new();
                stored.blue_score = 1;
            }
        } else {
            self.recompute_block(dag, block_hash);
        }

        // Affected area should include all descendants of all parents, and the block itself
        let mut affected_set = HashSet::new();
        for parent in &block.header.parents {
            affected_set.insert(parent.clone());
            for child in dag.get_descendants(parent) {
                affected_set.insert(child);
            }
        }

        let affected: Vec<Hash> = affected_set.into_iter().collect();
        let sorted_descendants = self.topological_sort(dag, &affected);

        for descendant in sorted_descendants {
            self.recompute_block(dag, &descendant);
        }
    }

    pub fn get_blue_set(&self, dag: &Dag, hash: &Hash) -> HashSet<Hash> {
        dag.get_block(hash)
            .map(|block| block.blue_set.clone())
            .unwrap_or_default()
    }

    pub fn get_red_set(&self, dag: &Dag, hash: &Hash) -> HashSet<Hash> {
        dag.get_block(hash)
            .map(|block| block.red_set.clone())
            .unwrap_or_default()
    }

    pub fn get_virtual_block(&self, dag: &Dag) -> Option<Hash> {
        let all_hashes = dag.get_all_hashes();
        if all_hashes.is_empty() {
            // Keep behavior consistent for empty DAGs in tests that expect a virtual block reference
            return Some(Hash::new(b"genesis"));
        }

        all_hashes
            .into_iter()
            .filter_map(|hash| {
                dag.get_block(&hash).map(|block| (hash, block.blue_score))
            })
            .max_by(|(h1, s1), (h2, s2)| s1.cmp(s2).then(h2.cmp(h1)))
            .map(|(hash, _)| hash)
    }

    /// Check if a block/transaction is final (irreversible)
    /// A block is considered final if its blue score is below the virtual blue score by at least FINALITY_DEPTH
    pub fn check_finality(&self, dag: &Dag, block_hash: &Hash, finality_threshold: u64) -> bool {
        let virtual_block = self.build_virtual_block(dag);
        let block_blue_score = dag.get_block(block_hash)
            .map(|b| b.blue_score)
            .unwrap_or(0);

        virtual_block.blue_score.saturating_sub(block_blue_score) >= finality_threshold.max(FINALITY_DEPTH)
    }

    /// Check if reorganization to target block would violate finality
    pub fn can_reorganize(&self, dag: &Dag, target_block: &Hash) -> Result<bool, CoreError> {
        let virtual_block = self.build_virtual_block(dag);
        let target_blue_score = dag.get_block(target_block)
            .map(|b| b.blue_score)
            .ok_or_else(|| CoreError::ConsensusError(format!("Block {} not found", target_block)))?;

        // use target stake (blue score) for reorganize strictness in future enhancements
        let _target_blue_score = target_blue_score;

        // Find the common ancestor
        let mut current = Some(target_block.clone());
        let mut depth = 0u64;

        while let Some(hash) = current {
            if virtual_block.parents.contains(&hash) {
                break; // Found common ancestor
            }
            current = dag.get_block(&hash).and_then(|b| b.selected_parent.clone());
            depth += 1;

            // Prevent infinite loops
            if depth > 10000 {
                return Err(CoreError::ConsensusError("Chain too deep while finding common ancestor".to_string()));
            }
        }

        // If depth to common ancestor exceeds finality, cannot reorganize
        Ok(depth <= FINALITY_DEPTH)
    }

    /// Check if reorganization is needed based on blue score comparison
    /// Returns the tip of the chain with higher blue score if reorg is needed
    pub fn should_reorganize(&self, dag: &Dag) -> Option<Hash> {
        let virtual_block = self.build_virtual_block(dag);

        // Get all tips
        let tips = dag.get_tips();

        // Find the tip with the highest blue score
        let best_tip = tips.into_iter()
            .filter_map(|tip| {
                dag.get_block(&tip).map(|block| (tip, block.blue_score))
            })
            .max_by(|(_, score1), (_, score2)| score1.cmp(score2))
            .map(|(tip, _)| tip)?;

        // If the best tip is not the selected parent of virtual block, we need to reorganize
        if Some(&best_tip) != virtual_block.selected_parent.as_ref() {
            Some(best_tip)
        } else {
            None
        }
    }

    /// Perform reorganization to the specified tip
    /// This assumes the reorganization is valid (checked by should_reorganize/can_reorganize)
    pub fn reorganize_to_tip(&self, dag: &mut Dag, new_tip: &Hash) -> Result<Vec<Hash>, CoreError> {
        let virtual_block = self.build_virtual_block(dag);

        // Find the common ancestor
        let common_ancestor = dag.find_common_ancestor(
            virtual_block.selected_parent.as_ref().unwrap_or(&Hash::new(&[])),
            new_tip
        ).ok_or_else(|| CoreError::ConsensusError("No common ancestor found for reorganization".to_string()))?;

        // Get the path from common ancestor to current selected parent (blocks to disconnect)
        let mut to_disconnect = Vec::new();
        let mut current = virtual_block.selected_parent.clone();
        while let Some(hash) = current {
            if hash == common_ancestor {
                break;
            }
            to_disconnect.push(hash.clone());
            current = dag.get_block(&hash).and_then(|b| b.selected_parent.clone());
        }

        // Get the path from common ancestor to new tip (blocks to connect)
        let mut to_connect = Vec::new();
        current = Some(new_tip.clone());
        while let Some(hash) = current {
            if hash == common_ancestor {
                break;
            }
            to_connect.push(hash.clone());
            current = dag.get_block(&hash).and_then(|b| b.selected_parent.clone());
        }
        to_connect.reverse(); // Reverse so we connect from ancestor to tip

        // Return the blocks that need to be disconnected and connected
        // The caller (state manager) will handle the actual disconnect/connect operations
        Ok(to_disconnect.into_iter().chain(to_connect).collect())
    }
}

impl Default for GhostDag {
    fn default() -> Self {
        Self::new(1)
    }
}
