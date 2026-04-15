use crate::core::crypto::Hash;
use crate::core::dag::BlockNode;
use crate::core::state::storage::Storage;
use crate::core::state::transaction::{Transaction, TxOutput};
use crate::core::state::utxo::{OutPoint, UtxoSet};
use crate::core::state::v_trie::VerkleTree;
use crate::core::state::PruneMarker;
use crate::core::vm::VMExecutor;
use crate::core::consensus::{economic_constants, ghostdag::GhostDag};
use crate::core::dag::Dag;
use crate::core::errors::CoreError;
use crate::Mempool;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Minimal state container exposing the current Verkle root.
#[derive(Debug, Clone)]
pub struct State {
    pub root: [u8; 32],
}

/// Snapshot of the chain state at a specific block height.
#[derive(Debug, Clone)]
pub struct StateSnapshot {
    pub height: u64,
    pub root: [u8; 32],
    pub total_supply: u128, // Track total supply for validation
    pub gas_fees: Vec<GasFeeWitness>, // Gas fee witnesses for the block
    pub last_block_hash: Option<Hash>,
}

/// Gas fee distribution witness for 80/20 validation
#[derive(Debug, Clone)]
pub struct GasFeeWitness {
    pub total_gas_fee: u128,
    pub miner_share: u128,
    pub fullnode_share: u128,
}

/// Undo data untuk rollback block application
#[derive(Debug, Clone)]
pub struct BlockUndo {
    pub spent_utxos: Vec<(OutPoint, TxOutput)>,
    pub created_utxos: Vec<OutPoint>,
    pub verkle_updates: Vec<([u8; 32], Option<Vec<u8>>)>, // key -> old_value (None if new)
    pub total_supply_delta: i128, // positive for increase, negative for decrease
    pub gas_fees_added: Vec<GasFeeWitness>,
}

/// Error types untuk StateManager operations
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateManagerError {
    InvalidRollback(String),
    SnapshotNotFound(u64),
    ApplyBlockFailed(String),
    RestoreFailed(String),
    CryptographicError(String),
    SerializationError(String),
    SupplyCapExceeded(String),
    BurnAddressViolation(String),
}

/// Execution witness containing all key/value state reads required for a block or contract execution.
#[derive(Debug, Clone)]
pub struct ExecutionWitnessEntry {
    pub key: [u8; 32],
    pub value: Option<Vec<u8>>,
    pub proof: crate::core::state::v_trie::VerkleProof,
}

#[derive(Debug, Clone)]
pub struct ExecutionWitness {
    pub root: [u8; 32],
    pub entries: Vec<ExecutionWitnessEntry>,
}

impl ExecutionWitness {
    pub fn is_valid(&self) -> bool {
        self.entries.iter().all(|entry| {
            entry.proof.root == self.root && entry.proof.path == entry.key.to_vec()
        })
    }

    pub fn serialize_compact(&self) -> Vec<u8> {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&self.root);
        buffer.extend_from_slice(&(self.entries.len() as u32).to_be_bytes());

        for entry in &self.entries {
            buffer.extend_from_slice(&entry.key);
            buffer.push(entry.proof.proof_type.clone() as u8);
            buffer.push(if entry.value.is_some() { 1 } else { 0 });
            if let Some(value) = &entry.value {
                buffer.extend_from_slice(&(value.len() as u32).to_be_bytes());
                buffer.extend_from_slice(value);
            }
            buffer.extend_from_slice(&(entry.proof.path.len() as u32).to_be_bytes());
            buffer.extend_from_slice(&entry.proof.path);
            buffer.extend_from_slice(&(entry.proof.siblings.len() as u32).to_be_bytes());
            for sibling in &entry.proof.siblings {
                buffer.extend_from_slice(sibling);
            }
            buffer.extend_from_slice(&entry.proof.root);
            buffer.push(if entry.proof.leaf_value.is_some() { 1 } else { 0 });
            if let Some(leaf_value) = &entry.proof.leaf_value {
                buffer.extend_from_slice(&(leaf_value.len() as u32).to_be_bytes());
                buffer.extend_from_slice(leaf_value);
            }
            buffer.push(if entry.proof.gas_fee_distribution.is_some() { 1 } else { 0 });
            if let Some(witness) = &entry.proof.gas_fee_distribution {
                buffer.extend_from_slice(&witness.total_gas_fee.to_be_bytes());
                buffer.extend_from_slice(&witness.miner_share.to_be_bytes());
                buffer.extend_from_slice(&witness.fullnode_share.to_be_bytes());
            }
        }

        buffer
    }

    pub fn deserialize_compact(bytes: &[u8]) -> Result<Self, String> {
        let mut cursor = 0;

        if bytes.len() < 36 {
            return Err("Invalid witness serialization".to_string());
        }

        let mut root = [0u8; 32];
        root.copy_from_slice(&bytes[cursor..cursor + 32]);
        cursor += 32;

        let entry_count = u32::from_be_bytes(
            bytes[cursor..cursor + 4]
                .try_into()
                .map_err(|_| "Invalid entry count" )?,
        ) as usize;
        cursor += 4;

        let mut entries = Vec::with_capacity(entry_count);

        for _ in 0..entry_count {
            if cursor + 32 + 1 + 1 > bytes.len() {
                return Err("Invalid execution witness data".to_string());
            }

            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes[cursor..cursor + 32]);
            cursor += 32;

            let proof_type = match bytes[cursor] {
                0 => crate::core::state::v_trie::ProofType::NonMembership,
                1 => crate::core::state::v_trie::ProofType::Membership,
                _ => return Err("Invalid proof type".to_string()),
            };
            cursor += 1;

            let has_value = bytes[cursor] == 1;
            cursor += 1;
            let value = if has_value {
                if cursor + 4 > bytes.len() {
                    return Err("Invalid execution witness data".to_string());
                }
                let len = u32::from_be_bytes(
                    bytes[cursor..cursor + 4]
                        .try_into()
                        .map_err(|_| "Invalid value length" )?,
                ) as usize;
                cursor += 4;
                if cursor + len > bytes.len() {
                    return Err("Invalid execution witness data".to_string());
                }
                let value_bytes = bytes[cursor..cursor + len].to_vec();
                cursor += len;
                Some(value_bytes)
            } else {
                None
            };

            if cursor + 4 > bytes.len() {
                return Err("Invalid execution witness data".to_string());
            }
            let path_len = u32::from_be_bytes(
                bytes[cursor..cursor + 4]
                    .try_into()
                    .map_err(|_| "Invalid path length" )?,
            ) as usize;
            cursor += 4;
            if cursor + path_len > bytes.len() {
                return Err("Invalid execution witness data".to_string());
            }
            let path = bytes[cursor..cursor + path_len].to_vec();
            cursor += path_len;

            if cursor + 4 > bytes.len() {
                return Err("Invalid execution witness data".to_string());
            }
            let sibling_count = u32::from_be_bytes(
                bytes[cursor..cursor + 4]
                    .try_into()
                    .map_err(|_| "Invalid sibling count" )?,
            ) as usize;
            cursor += 4;
            let mut siblings = Vec::with_capacity(sibling_count);
            for _ in 0..sibling_count {
                if cursor + 32 > bytes.len() {
                    return Err("Invalid execution witness data".to_string());
                }
                let mut sibling = [0u8; 32];
                sibling.copy_from_slice(&bytes[cursor..cursor + 32]);
                cursor += 32;
                siblings.push(sibling);
            }

            if cursor + 32 > bytes.len() {
                return Err("Invalid execution witness data".to_string());
            }
            let mut proof_root = [0u8; 32];
            proof_root.copy_from_slice(&bytes[cursor..cursor + 32]);
            cursor += 32;

            if cursor + 1 > bytes.len() {
                return Err("Invalid execution witness data".to_string());
            }
            let leaf_present = bytes[cursor] == 1;
            cursor += 1;
            let leaf_value = if leaf_present {
                if cursor + 4 > bytes.len() {
                    return Err("Invalid execution witness data".to_string());
                }
                let len = u32::from_be_bytes(
                    bytes[cursor..cursor + 4]
                        .try_into()
                        .map_err(|_| "Invalid leaf value length" )?,
                ) as usize;
                cursor += 4;
                if cursor + len > bytes.len() {
                    return Err("Invalid execution witness data".to_string());
                }
                let leaf_bytes = bytes[cursor..cursor + len].to_vec();
                cursor += len;
                Some(leaf_bytes)
            } else {
                None
            };

            if cursor + 1 > bytes.len() {
                return Err("Invalid execution witness data".to_string());
            }
            let has_witness = bytes[cursor] == 1;
            cursor += 1;
            let gas_fee_distribution = if has_witness {
                if cursor + 48 > bytes.len() {
                    return Err("Invalid execution witness data".to_string());
                }
                let total_gas_fee = u128::from_be_bytes(
                    bytes[cursor..cursor + 16]
                        .try_into()
                        .map_err(|_| "Invalid gas fee bytes" )?,
                );
                cursor += 16;
                let miner_share = u128::from_be_bytes(
                    bytes[cursor..cursor + 16]
                        .try_into()
                        .map_err(|_| "Invalid gas fee bytes" )?,
                );
                cursor += 16;
                let fullnode_share = u128::from_be_bytes(
                    bytes[cursor..cursor + 16]
                        .try_into()
                        .map_err(|_| "Invalid gas fee bytes" )?,
                );
                cursor += 16;
                Some(GasFeeWitness {
                    total_gas_fee,
                    miner_share,
                    fullnode_share,
                })
            } else {
                None
            };

            entries.push(ExecutionWitnessEntry {
                key,
                value,
                proof: crate::core::state::v_trie::VerkleProof {
                    proof_type,
                    path,
                    siblings,
                    leaf_value: leaf_value.clone(),
                    root: proof_root,
                    opening_proofs: Vec::new(),
                    gas_fee_distribution: gas_fee_distribution.map(|w| crate::core::state::v_trie::GasFeeWitness {
                        total_gas_fee: w.total_gas_fee,
                        miner_share: w.miner_share,
                        fullnode_share: w.fullnode_share,
                    }),
                },
            });
        }

        Ok(Self { root, entries })
    }
}

impl std::fmt::Display for StateManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateManagerError::InvalidRollback(msg) => write!(f, "Invalid rollback: {}", msg),
            StateManagerError::SnapshotNotFound(height) => write!(f, "Snapshot not found at height {}", height),
            StateManagerError::ApplyBlockFailed(msg) => write!(f, "Apply block failed: {}", msg),
            StateManagerError::RestoreFailed(msg) => write!(f, "Restore failed: {}", msg),
            StateManagerError::CryptographicError(msg) => write!(f, "Cryptographic error: {}", msg),
            StateManagerError::SerializationError(msg) => write!(f, "Serialization error: {}", msg),
            StateManagerError::SupplyCapExceeded(msg) => write!(f, "Supply cap exceeded: {}", msg),
            StateManagerError::BurnAddressViolation(msg) => write!(f, "Burn address violation: {}", msg),
        }
    }
}

/// Basic state manager for applying blocks, tracking snapshots, and rolling back.
#[derive(Debug)]
pub struct StateManager<S: Storage + Clone> {
    pub tree: VerkleTree<S>,
    pub current_height: u64,
    pub snapshots: Vec<StateSnapshot>,
    pub snapshot_storages: Vec<S>,
    pub prune_markers: HashMap<OutPoint, PruneMarker>,
    pub outpoint_to_key: HashMap<OutPoint, [u8; 32]>,
    pub current_total_supply: u128, // Running total supply tracker
    pub block_gas_fees: Vec<GasFeeWitness>, // Gas fee witnesses per block
    pub pending_updates: Vec<([u8; 32], Vec<u8>)>, // Pending state updates for atomic application
    /// Atomic operation flag to prevent concurrent state modifications
    pub applying_block: std::sync::atomic::AtomicBool,
    /// Guard untuk prevent race condition saat apply_block dijalankan bersamaan
    pub apply_lock: std::sync::Mutex<()>,
    /// Undo data untuk setiap block yang diaplikasikan, indexed by block hash
    pub block_undo_data: HashMap<Hash, BlockUndo>,
    pub current_block_hash: Option<Hash>,
}

impl<S: Storage + Clone + Send + Sync + 'static> StateManager<S> {
    /// Initialize a new StateManager with a Verkle tree snapshot.
    ///
    /// This sets the origin state as height zero and stores first snapshot for rollback.
    pub fn new(mut tree: VerkleTree<S>) -> Result<Self, StateManagerError> {
        let root = tree.get_root()
            .map_err(|e| StateManagerError::CryptographicError(format!("Failed to get root: {}", e)))?;
        let storage_snapshot = tree.storage_clone();

        Ok(Self {
            tree,
            current_height: 0,
            snapshots: vec![StateSnapshot { height: 0, root, total_supply: 0, gas_fees: Vec::new(), last_block_hash: None }],
            snapshot_storages: vec![storage_snapshot],
            prune_markers: HashMap::new(),
            outpoint_to_key: HashMap::new(),
            current_total_supply: 0,
            block_gas_fees: Vec::new(),
            pending_updates: Vec::new(),
            block_undo_data: HashMap::new(),
            current_block_hash: None,
            applying_block: std::sync::atomic::AtomicBool::new(false),
            apply_lock: std::sync::Mutex::new(()),
        })
    }

    /// Apply a block atomically with atomic reversion on failure.
    ///
    /// Steps:
    /// 1. Acquire lock and set atomic flag.
    /// 2. Create in-memory snapshot for rollback.
    /// 3. Apply transactions and verify state transition.
    /// 4. Commit root hash and push snapshot.
    ///
    /// Prevents double-commit and enforces state machine consistency.
    pub fn apply_block(&mut self, block: &BlockNode, utxo: &mut UtxoSet) -> Result<(), StateManagerError> {
        // Check if another block application is already in progress and set flag atomically.
        if self.applying_block.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err(StateManagerError::ApplyBlockFailed(
                "Concurrent block application not allowed".to_string()
            ));
        }

        let applying_block_ptr: *const std::sync::atomic::AtomicBool = &self.applying_block;
        struct AtomicFlagGuard(*const std::sync::atomic::AtomicBool);
        impl Drop for AtomicFlagGuard {
            fn drop(&mut self) {
                unsafe {
                    (*self.0).store(false, std::sync::atomic::Ordering::SeqCst);
                }
            }
        }
        let _guard = AtomicFlagGuard(applying_block_ptr);

        // CRITICAL: Create snapshot at start of block for atomic rollback on any failure
        let mut snapshot_tree = self.tree.clone();
        let snapshot_height = self.current_height;
        let snapshot_supply = self.current_total_supply;
        let snapshot_gas_fees = self.block_gas_fees.clone();
        let snapshot_pending_updates = self.pending_updates.clone();
        let snapshot_prune_markers = self.prune_markers.clone();
        let snapshot_outpoint_to_key = self.outpoint_to_key.clone();
        let snapshot_utxo = utxo.clone();
        let snapshot_block_hash = self.current_block_hash.clone();

        // Initialize undo data collection
        let mut undo_data = BlockUndo {
            spent_utxos: Vec::new(),
            created_utxos: Vec::new(),
            verkle_updates: Vec::new(),
            total_supply_delta: 0,
            gas_fees_added: Vec::new(),
        };

        // 
        // Clear pending updates for new block
        self.pending_updates.clear();

        // Reset gas fees untuk block baru
        self.block_gas_fees.clear();

        // Process transactions untuk state update
        for tx in &block.transactions {
            if let Err(e) = self.apply_transaction(tx, utxo, &mut undo_data) {
                // ROLLBACK: Restore snapshot on transaction processing error
                eprintln!("[ERROR] apply_transaction failed: {}. Rolling back to height {}.", e, snapshot_height);
                std::mem::swap(&mut self.tree, &mut snapshot_tree);
                self.current_height = snapshot_height;
                self.current_total_supply = snapshot_supply;
                self.block_gas_fees = snapshot_gas_fees;
                self.pending_updates = snapshot_pending_updates;
                self.prune_markers = snapshot_prune_markers.clone();
                self.outpoint_to_key = snapshot_outpoint_to_key.clone();
                *utxo = snapshot_utxo;
                self.current_block_hash = snapshot_block_hash.clone();
                return Err(e);
            }
        }

        // Apply all pending updates atomically with anti-burn and supply cap checks
        if let Err(e) = self.tree.apply_state_transition(self.pending_updates.clone(), self.current_total_supply) {
            // ROLLBACK: Restore snapshot if state transition fails (supply cap, anti-burn, etc)
            eprintln!("[CRITICAL] apply_state_transition failed: {}. Rolling back block to height {}.", e, snapshot_height);
            std::mem::swap(&mut self.tree, &mut snapshot_tree);
            self.current_height = snapshot_height;
            self.current_total_supply = snapshot_supply;
            self.block_gas_fees = snapshot_gas_fees;
            self.pending_updates = snapshot_pending_updates;
            self.prune_markers = snapshot_prune_markers.clone();
            self.outpoint_to_key = snapshot_outpoint_to_key.clone();
            *utxo = snapshot_utxo;
            self.current_block_hash = snapshot_block_hash.clone();
            return Err(StateManagerError::ApplyBlockFailed(format!("State transition failed: {}", e)));
        }

        self.current_height += 1;
        self.current_block_hash = Some(block.header.id.clone());

        // CRITICAL: Cross-check Verkle root consistency after all updates applied
        let new_root = self.tree.get_root().unwrap_or([0u8; 32]);
        
        // Verify root changed if transactions were applied (basic sanity check)
        let old_root = self.snapshots[snapshot_height as usize].root;
        if !block.transactions.is_empty() && new_root == old_root {
            eprintln!("[WARNING] Block root unchanged after applying {} transactions. Potential state corruption.", block.transactions.len());
        }

        // Create snapshot setelah semua transactions applied
        self.snapshots.push(StateSnapshot {
            height: self.current_height,
            root: new_root,
            total_supply: self.current_total_supply,
            gas_fees: self.block_gas_fees.clone(),
            last_block_hash: self.current_block_hash.clone(),
        });
        self.snapshot_storages.push(self.tree.storage_clone());

        // Store undo data for potential reorganization
        self.block_undo_data.insert(block.header.id.clone(), undo_data);

        Ok(())
    }

    /// Validate block with consensus rules and apply atomically
    /// This ensures consensus validation happens before any state changes
    pub fn validate_and_apply_block(
        &mut self,
        block: &BlockNode,
        utxo: &mut UtxoSet,
        dag: &Dag,
        consensus: &GhostDag
    ) -> Result<(), StateManagerError> {
        // Get current time for validation
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| StateManagerError::CryptographicError(format!("Time error: {}", e)))?
            .as_secs();

        // First, validate block with consensus rules
        consensus.validate_block(block, dag, &self.tree, current_time)?;

        // Check finality constraints for reorganization
        if !consensus.can_reorganize(dag, &block.header.id)? {
            return Err(StateManagerError::ApplyBlockFailed(
                "Block reorganization would violate finality constraints".to_string()
            ));
        }

        // Apply the block
        self.apply_block(block, utxo)
    }

    /// Apply transaction dengan error handling untuk validation
    pub fn apply_transaction(&mut self, tx: &Transaction, utxo: &mut UtxoSet, undo_data: &mut BlockUndo) -> Result<(), StateManagerError> {
        if tx.execution_payload.is_empty() && tx.contract_address.is_none() {
            self.apply_utxo_transaction(tx, utxo, undo_data)
        } else {
            self.apply_contract_transition(tx, utxo, undo_data)
        }
    }

    /// Internal helper: apply standar UTXO-only transaction
    fn apply_utxo_transaction(&mut self, tx: &Transaction, utxo: &mut UtxoSet, undo_data: &mut BlockUndo) -> Result<(), StateManagerError> {
        // Process inputs (remove from UTXO set and subtract from total supply)
        for input in &tx.inputs {
            let outpoint = (input.prev_tx.clone(), input.index);
            let output = match utxo.utxos.get(&outpoint) {
                Some(output) => output.clone(),
                None => {
                    return Err(StateManagerError::ApplyBlockFailed(format!(
                        "Missing UTXO input in transaction {} at outpoint {:?}",
                        tx.id, outpoint
                    )));
                }
            };
            // Record for undo
            undo_data.spent_utxos.push((outpoint.clone(), output.clone()));
            undo_data.total_supply_delta -= output.value as i128;

            // Subtract spent amount from total supply
            self.current_total_supply = self.current_total_supply.saturating_sub(output.value as u128);
            utxo.utxos.remove(&outpoint);
            self.outpoint_to_key.remove(&outpoint);
            self.prune_markers.remove(&outpoint);
        }

        // Process outputs (add to UTXO set dan tree)
        for (i, output) in tx.outputs.iter().enumerate() {
            let key = tx.hash_with_index(i as u32);
            let outpoint = (tx.id.clone(), i as u32);
            
            // Record for undo - check if key existed before
            let old_value = self.tree.get(key).ok().flatten();
            undo_data.verkle_updates.push((key, old_value));
            undo_data.created_utxos.push(outpoint.clone());
            undo_data.total_supply_delta += output.value as i128;

            utxo.utxos.insert(outpoint.clone(), output.clone());
            self.pending_updates.push((key, output.serialize()));
            self.outpoint_to_key.insert(outpoint.clone(), key);

            // Add new output amount to total supply
            self.current_total_supply = self.current_total_supply.saturating_add(output.value as u128);
        }

        Ok(())
    }

    /// Internal helper: apply contract execution transaction
    fn apply_contract_transition(&mut self, tx: &Transaction, utxo: &mut UtxoSet, undo_data: &mut BlockUndo) -> Result<(), StateManagerError> {
        // Gas validation based on intrinsic + calldata scoring.
        let payload_data_cost: u64 = tx.execution_payload.iter().fold(0, |acc, byte| {
            acc + if *byte == 0 { 4 } else { 16 }
        });
        let intrinsic_cost: u64 = 21_000;
        let required_gas = intrinsic_cost.saturating_add(payload_data_cost);

        if tx.gas_limit < required_gas {
            return Err(StateManagerError::ApplyBlockFailed(format!(
                "Insufficient gas limit ({}), required {} for intrinsic+payload cost {} bytes",
                tx.gas_limit,
                required_gas,
                tx.execution_payload.len()
            )));
        }

        // Snapshot state tree for rollback
        let tree_snapshot = self.tree.clone();

        // Execute contract payload using VMExecutor
        let exec_res = VMExecutor::execute(&tx.execution_payload, self, [0u8; 32], tx.gas_limit);

        match exec_res {
            Ok(gas_used) => {
                let total_gas_fee = (gas_used as u128).saturating_mul(tx.max_fee_per_gas);

                // Create gas fee distribution witness untuk 80/20 validation
                let gas_witness = self.create_gas_fee_witness(total_gas_fee);
                self.block_gas_fees.push(gas_witness.clone());
                undo_data.gas_fees_added.push(gas_witness);

                // Gas fee is accounted for and pooled, no burn.
                // This will be included in reward calculations in consensus/reward.rs.
                let _ = (gas_used, tx.max_fee_per_gas, total_gas_fee); // keep info for debugging, avoid warning

                // Keep the state updates done by host functions from VM
                // For compatibility, still apply UTXO outputs on top of contract run
                self.apply_utxo_transaction(tx, utxo, undo_data)
            }
            Err(err) => {
                // Roll back Verkle tree state
                self.tree = tree_snapshot;
                Err(StateManagerError::ApplyBlockFailed(format!("VM execution failed: {}", err)))
            }
        }
    }

    /// Verify gas fee distribution witnesses untuk 80/20 compliance
    pub fn verify_gas_fee_distribution(&self, witness: &GasFeeWitness) -> bool {
        // Verify 80/20 split calculation
        let expected_miner = (witness.total_gas_fee * economic_constants::MINER_REWARD_PERCENT) / 100;
        let expected_fullnode = witness.total_gas_fee.saturating_sub(expected_miner);
        
        witness.miner_share == expected_miner && 
        witness.fullnode_share == expected_fullnode &&
        witness.miner_share + witness.fullnode_share == witness.total_gas_fee
    }

    /// Verify global supply cap is not exceeded
    pub fn verify_global_supply(&self) -> Result<(), StateManagerError> {
        if self.current_total_supply > economic_constants::MAX_GLOBAL_SUPPLY_NANO_SLUG {
            return Err(StateManagerError::SupplyCapExceeded(
                format!("Total supply {} exceeds maximum allowed {}", 
                        self.current_total_supply, economic_constants::MAX_GLOBAL_SUPPLY_NANO_SLUG)
            ));
        }
        Ok(())
    }

    /// Stateful validation of block against current UTXO set and Verkle state
    pub fn validate_block_stateful(&mut self, block: &BlockNode, current_utxo_set: &UtxoSet) -> Result<(), CoreError> {
        // Double Spend Check: Ensure all inputs are unspent in current UTXO set
        for tx in &block.transactions {
            if tx.is_coinbase() {
                continue; // Coinbase has no inputs
            }
            for input in &tx.inputs {
                let outpoint = (input.prev_tx.clone(), input.index);
                if !current_utxo_set.utxos.contains_key(&outpoint) {
                    return Err(CoreError::ValidationError(format!(
                        "Double spend detected: input {:?} in transaction {} is already spent or doesn't exist",
                        outpoint, tx.id
                    )));
                }
            }
        }

        // Verkle Proof Verification: Verify state transitions match block's Verkle root
        if let Some(_proof) = &block.header.verkle_proofs {
            // For each transaction, verify the state changes are correctly committed
            for _tx in &block.transactions {
                // Simulate state changes and verify against proof
                // This is a simplified check - in production, we'd verify the full proof
                let expected_root = block.header.verkle_root.clone();
                if let Ok(current_root) = self.tree.get_root() {
                    let current_root_hash = Hash::from_bytes(&current_root);
                    // For now, just check that the block declares a valid root
                    // Full proof verification would require reconstructing the state changes
                    if expected_root != current_root_hash && !block.transactions.is_empty() {
                        // Allow genesis or blocks that don't change state
                        return Err(CoreError::ValidationError(format!(
                            "Verkle root mismatch: block declares {:?}, current state is {:?}",
                            expected_root, current_root
                        )));
                    }
                }
            }
        }

        // Contextual Rules: Inflation check (except for coinbase)
        for tx in &block.transactions {
            if tx.is_coinbase() {
                continue; // Coinbase can create new coins
            }

            let mut total_input = 0u64;
            for input in &tx.inputs {
                let outpoint = (input.prev_tx.clone(), input.index);
                if let Some(output) = current_utxo_set.utxos.get(&outpoint) {
                    total_input = total_input.saturating_add(output.value);
                }
            }

            let mut total_output = 0u64;
            for output in &tx.outputs {
                total_output = total_output.saturating_add(output.value);
            }

            // Include gas fee in output calculation
            let gas_fee = (tx.gas_limit as u128).saturating_mul(tx.max_fee_per_gas);
            let total_output_with_fee = total_output.saturating_add(gas_fee as u64);

            if total_input < total_output_with_fee {
                return Err(CoreError::ValidationError(format!(
                    "Inflation detected in transaction {}: input {} < output {} + fee {}",
                    tx.id, total_input, total_output, gas_fee
                )));
            }
        }

        Ok(())
    }

    /// Disconnect a block from the current chain (for reorganization)
    pub fn disconnect_block(&mut self, block: &BlockNode, utxo: &mut UtxoSet) -> Result<(), StateManagerError> {
        let block_hash = &block.header.id;
        let undo_data = self.block_undo_data.get(block_hash)
            .ok_or_else(|| StateManagerError::ApplyBlockFailed(format!("No undo data for block {}", block_hash)))?
            .clone();

        // Reverse the operations in undo_data

        // 1. Remove created UTXOs
        for outpoint in &undo_data.created_utxos {
            utxo.utxos.remove(outpoint);
            self.outpoint_to_key.remove(outpoint);
        }

        // 2. Restore spent UTXOs
        for (outpoint, output) in &undo_data.spent_utxos {
            utxo.utxos.insert(outpoint.clone(), output.clone());
            // Recreate the key as done in apply_utxo_transaction
            let mut data = outpoint.0.as_bytes().to_vec();
            data.extend_from_slice(&outpoint.1.to_le_bytes());
            let hash = Hash::new(&data);
            let key = hash.as_bytes();
            self.outpoint_to_key.insert(outpoint.clone(), *key);
        }

        // 3. Reverse Verkle tree updates
        for (key, old_value) in undo_data.verkle_updates.into_iter().rev() {
            if let Some(old_val) = old_value {
                self.tree.insert(key, old_val);
            } else {
                // If it was newly created, insert empty value to "remove" it
                self.tree.insert(key, Vec::new());
            }
        }

        // 4. Reverse total supply changes
        if undo_data.total_supply_delta > 0 {
            self.current_total_supply = self.current_total_supply.saturating_sub(undo_data.total_supply_delta as u128);
        } else {
            self.current_total_supply = self.current_total_supply.saturating_add((-undo_data.total_supply_delta) as u128);
        }

        // 5. Remove gas fees added by this block
        for _ in &undo_data.gas_fees_added {
            self.block_gas_fees.pop();
        }

        // 6. Decrement height
        self.current_height = self.current_height.saturating_sub(1);

        // 7. Remove snapshot
        self.snapshots.pop();
        self.snapshot_storages.pop();

        // 8. Remove undo data
        self.block_undo_data.remove(block_hash);

        Ok(())
    }

    /// Connect a block to the current chain (for reorganization)
    pub fn connect_block(&mut self, block: &BlockNode, utxo: &mut UtxoSet) -> Result<(), StateManagerError> {
        // Validate statefully before connecting
        self.validate_block_stateful(block, utxo)
            .map_err(|e| StateManagerError::ApplyBlockFailed(format!("Stateful validation failed: {:?}", e)))?;

        // Apply the block normally
        self.apply_block(block, utxo)
    }

    /// Perform a full reorganization from old tip to new tip
    pub fn reorganize_chain(
        &mut self,
        dag: &mut Dag,
        ghostdag: &GhostDag,
        utxo: &mut UtxoSet,
        new_tip: &Hash
    ) -> Result<(), StateManagerError> {
        // Check if reorganization is allowed
        if !ghostdag.can_reorganize(dag, new_tip)
            .map_err(|e| StateManagerError::ApplyBlockFailed(format!("Cannot reorganize: {:?}", e)))? {
            return Err(StateManagerError::ApplyBlockFailed("Reorganization would violate finality".to_string()));
        }

        // Get the blocks to reorganize
        let blocks_to_change = ghostdag.reorganize_to_tip(dag, new_tip)
            .map_err(|e| StateManagerError::ApplyBlockFailed(format!("Failed to plan reorganization: {:?}", e)))?;

        // Create a snapshot for rollback in case of failure
        let snapshot = StateSnapshot {
            height: self.current_height,
            root: self.tree.get_root().unwrap_or([0u8; 32]),
            total_supply: self.current_total_supply,
            gas_fees: self.block_gas_fees.clone(),
            last_block_hash: self.current_block_hash.clone(),
        };
        let utxo_snapshot = utxo.clone();

        // Perform the reorganization atomically
        let result = self.perform_reorganization(dag, utxo, &blocks_to_change);

        match result {
            Ok(_) => {
                // Update the virtual selected parent in DAG
                let _virtual_block = ghostdag.build_virtual_block(dag);
                // The reorganization should have updated the selected parents appropriately
                Ok(())
            }
            Err(e) => {
                // Rollback on failure
                self.current_height = snapshot.height;
                self.current_total_supply = snapshot.total_supply;
                self.block_gas_fees = snapshot.gas_fees;
                self.current_block_hash = snapshot.last_block_hash.clone();
                // Note: tree rollback would require more complex logic, for now we assume tree is consistent
                *utxo = utxo_snapshot;
                Err(e)
            }
        }
    }

    /// Internal function to perform the actual reorganization
    fn perform_reorganization(
        &mut self,
        dag: &mut Dag,
        utxo: &mut UtxoSet,
        blocks: &[Hash]
    ) -> Result<(), StateManagerError> {
        // Find the common ancestor by finding where the paths diverge
        let mut disconnect_blocks = Vec::new();
        let mut connect_blocks = Vec::new();
        let mut found_divergence = false;

        for block_hash in blocks {
            if let Some(_block) = dag.get_block(block_hash) {
                if !found_divergence {
                    // Check if this block is in current chain
                    if self.block_undo_data.contains_key(block_hash) {
                        disconnect_blocks.push(block_hash.clone());
                    } else {
                        found_divergence = true;
                        connect_blocks.push(block_hash.clone());
                    }
                } else {
                    connect_blocks.push(block_hash.clone());
                }
            }
        }

        // Disconnect blocks in reverse order (from tip to ancestor)
        for block_hash in disconnect_blocks.into_iter().rev() {
            if let Some(block) = dag.get_block(&block_hash) {
                self.disconnect_block(block, utxo)?;
            }
        }

        // Connect blocks in forward order (from ancestor to tip)
        for block_hash in connect_blocks {
            if let Some(block) = dag.get_block(&block_hash) {
                self.connect_block(block, utxo)?;
            }
        }

        Ok(())
    }

    /// Notify mempool to revalidate pending transactions after state update
    /// This should be called after applying a new block to ensure mempool transactions are still valid
    pub fn notify_mempool_update(&mut self, mempool: &Mempool, utxo_set: &UtxoSet) -> usize {
        mempool.revalidate_pending_transactions(self, utxo_set)
    }

    /// Get root hash dari current state
    pub fn create_gas_fee_witness(&self, total_gas_fee: u128) -> GasFeeWitness {
        let miner_share = (total_gas_fee * economic_constants::MINER_REWARD_PERCENT) / 100;
        let fullnode_share = total_gas_fee.saturating_sub(miner_share);
        
        GasFeeWitness {
            total_gas_fee,
            miner_share,
            fullnode_share,
        }
    }

    /// Get state snapshot pada specific height
    pub fn get_state_at(&self, height: u64) -> Option<&StateSnapshot> {
        self.snapshots.iter().find(|s| s.height == height)
    }

    /// Rollback state ke target height dengan error handling
    pub fn rollback_state(&mut self, target_height: u64) -> Result<(), StateManagerError> {
        // Validation
        if target_height > self.current_height {
            return Err(StateManagerError::InvalidRollback(
                format!("Cannot rollback to height {} when current height is {}", 
                        target_height, self.current_height)
            ));
        }

        // Check snapshot exists
        if self.get_state_at(target_height).is_none() {
            return Err(StateManagerError::SnapshotNotFound(target_height));
        }

        // Truncate snapshots dan storages
        self.snapshots.truncate(target_height as usize + 1);
        self.snapshot_storages.truncate(target_height as usize + 1);
        self.current_height = target_height;

        // Restore tree dari snapshot storage
        let snapshot_storage = self
            .snapshot_storages
            .get(target_height as usize)
            .ok_or_else(|| StateManagerError::RestoreFailed(
                "Snapshot storage missing after truncation".to_string()
            ))?;

        self.tree = VerkleTree::new(snapshot_storage.clone())
            .map_err(|e| StateManagerError::RestoreFailed(format!("Failed to restore tree: {}", e)))?;
        
        // Restore total supply from snapshot
        self.current_total_supply = self.snapshots[target_height as usize].total_supply;
        
        // Restore gas fees from snapshot
        self.block_gas_fees = self.snapshots[target_height as usize].gas_fees.clone();
        
        // Verify restoration
        let restored_root = self.tree.get_root().unwrap_or([0u8; 32]);
        let snapshot_root = self.snapshots[target_height as usize].root;
        
        if restored_root != snapshot_root {
            return Err(StateManagerError::RestoreFailed(
                format!("Root mismatch after rollback: expected {:?}, got {:?}", 
                        snapshot_root, restored_root)
            ));
        }

        Ok(())
    }

    /// Legacy rollback method - should use rollback_state() instead
    pub fn rollback(&mut self, target_height: u64) -> Result<(), StateManagerError> {
        if target_height > self.current_height {
            return Err(StateManagerError::InvalidRollback(format!(
                "Requested rollback to {} is beyond current height {}",
                target_height, self.current_height
            )));
        }

        // Keep safe backup in case restore fails.
        let backup_tree = self.tree.clone();
        let backup_height = self.current_height;
        let backup_snapshots = self.snapshots.clone();
        let backup_snapshot_storages = self.snapshot_storages.clone();
        let backup_total_supply = self.current_total_supply;
        let backup_gas_fees = self.block_gas_fees.clone();

        match self.rollback_state(target_height) {
            Ok(()) => {
                // Additional verification: ensure root hash matches snapshot after successful rollback
                // This catches any silent corruption that might have occurred during restoration
                if let Ok(current_root) = self.tree.get_root() {
                    let expected_root = self.snapshots[target_height as usize].root;
                    if current_root != expected_root {
                        eprintln!(
                            "[CRITICAL] Silent corruption detected after rollback: root mismatch at height {}. Expected {:?}, got {:?}. Attempting emergency restore.",
                            target_height, expected_root, current_root
                        );
                        
                        // Emergency restore from backup
                        self.tree = backup_tree;
                        self.current_height = backup_height;
                        self.snapshots = backup_snapshots;
                        self.snapshot_storages = backup_snapshot_storages;
                        self.current_total_supply = backup_total_supply;
                        self.block_gas_fees = backup_gas_fees;
                        
                        return Err(StateManagerError::RestoreFailed(
                            format!("Silent corruption detected: root hash mismatch after rollback to height {}", target_height)
                        ));
                    }
                } else {
                    eprintln!(
                        "[WARNING] Could not verify root hash after rollback to height {}: get_root_hash failed. Proceeding with caution.",
                        target_height
                    );
                }
                Ok(())
            }
            Err(err) => {
                eprintln!(
                    "[ERROR] rollback to height {} failed: {:?}. Restoring last known safe state at height {}.",
                    target_height, err, backup_height
                );

                // Restore safe state from backup
                self.tree = backup_tree;
                self.current_height = backup_height;
                self.snapshots = backup_snapshots;
                self.snapshot_storages = backup_snapshot_storages;
                self.current_total_supply = backup_total_supply;
                self.block_gas_fees = backup_gas_fees;

                Err(err)
            }
        }
    }

    /// Restore entire state dari specific snapshot
    pub fn restore_from_snapshot(&mut self, snapshot_root: [u8; 32], height: u64) -> Result<(), StateManagerError> {
        let snapshot_idx = self.snapshots.iter()
            .position(|s| s.height == height && s.root == snapshot_root)
            .ok_or(StateManagerError::SnapshotNotFound(height))?;

        let storage = self.snapshot_storages
            .get(snapshot_idx)
            .ok_or_else(|| StateManagerError::RestoreFailed("Storage missing".to_string()))?;

        self.tree = VerkleTree::new(storage.clone())
            .map_err(|e| StateManagerError::RestoreFailed(format!("Failed to restore tree: {}", e)))?;
        self.current_height = height;
        
        // Truncate snapshots ke restore point
        self.snapshots.truncate(snapshot_idx + 1);
        self.snapshot_storages.truncate(snapshot_idx + 1);
        self.current_block_hash = self.snapshots.last().and_then(|s| s.last_block_hash.clone());

        Ok(())
    }

    /// Get current state snapshot
    pub fn get_current_state(&mut self) -> Result<StateSnapshot, StateManagerError> {
        let root = self.tree.get_root()
            .map_err(|e| StateManagerError::CryptographicError(format!("Failed to get root: {}", e)))?;
        Ok(StateSnapshot {
            height: self.current_height,
            root,
            total_supply: self.current_total_supply,
            gas_fees: self.block_gas_fees.clone(),
            last_block_hash: self.current_block_hash.clone(),
        })
    }

    /// Get current Verkle root hash
    pub fn get_root_hash(&mut self) -> Result<[u8; 32], StateManagerError> {
        self.tree.get_root()
            .map_err(|e| StateManagerError::CryptographicError(format!("Failed to get root: {}", e)))
    }

    /// Validate the current state root against a known backup root.
    pub fn verify_state_root(&mut self, expected_root: Hash) -> Result<(), StateManagerError> {
        let current_root = self.get_root_hash()?;
        let current_hash = Hash::from_bytes(&current_root);
        if current_hash != expected_root {
            return Err(StateManagerError::RestoreFailed(format!(
                "State root mismatch after restore: expected {}, got {}",
                expected_root, current_hash
            )));
        }
        Ok(())
    }

    /// VM host read state from Verkle tree
    pub fn state_read(&self, key: [u8; 32]) -> Result<Option<Vec<u8>>, String> {
        match self.tree.get(key) {
            Ok(val) => Ok(val),
            Err(e) => Err(e.to_string()),
        }
    }

    /// VM host write state to Verkle tree
    pub fn state_write(&mut self, key: [u8; 32], value: Vec<u8>) -> Result<(), String> {
        // For simplicity, state writes use insert (overwrite leaf)
        self.tree.insert(key, value);
        Ok(())
    }

    /// Tandai UTXO/outpoint untuk pruning pada epoch/timestamp tertentu.
    pub fn mark_outpoint_for_pruning(&mut self, outpoint: OutPoint, epoch: u64, timestamp: u64) {
        self.prune_markers.insert(outpoint, PruneMarker { epoch, timestamp });
    }

    /// Jalankan pruning cycle: prune semua outpoint yang melewati epoch threshold.
    pub fn execute_pruning_cycle(&mut self, epoch_threshold: u64, utxo: &mut UtxoSet) -> Result<Vec<OutPoint>, StateManagerError> {
        let keys_to_prune: Vec<OutPoint> = self
            .prune_markers
            .iter()
            .filter(|(_, marker)| marker.epoch <= epoch_threshold)
            .map(|(outpoint, _)| outpoint.clone())
            .collect();

        let mut pruned = Vec::new();
        for outpoint in keys_to_prune {
            if let Some(key) = self.outpoint_to_key.get(&outpoint).cloned() {
                self.tree
                    .prune_key(key)
                    .map_err(|e| StateManagerError::CryptographicError(format!("Prune failed: {}", e)))?;
                utxo.utxos.remove(&outpoint);
                self.outpoint_to_key.remove(&outpoint);
                self.prune_markers.remove(&outpoint);
                pruned.push(outpoint);
            }
        }

        Ok(pruned)
    }

    /// Validate snapshot consistency
    pub fn validate_snapshots(&self) -> Result<(), StateManagerError> {
        for (i, snapshot) in self.snapshots.iter().enumerate() {
            if snapshot.height != i as u64 {
                return Err(StateManagerError::ApplyBlockFailed(
                    format!("Snapshot height mismatch at index {}", i)
                ));
            }
        }

        if self.snapshots.len() != self.snapshot_storages.len() {
            return Err(StateManagerError::ApplyBlockFailed(
                "Snapshot and storage length mismatch".to_string()
            ));
        }

        Ok(())
    }

    /// CRITICAL: Cross-check total supply consistency between UTXO set and Verkle state
    /// Ensures Verkle root hash reflects accurate supply count
    pub fn cross_check_supply_consistency(&mut self, utxo: &UtxoSet) -> Result<(), StateManagerError> {
        // Calculate total supply by summing all UTXO values
        let mut calculated_supply: u128 = 0;
        for output in utxo.utxos.values() {
            calculated_supply = calculated_supply.checked_add(output.value as u128)
                .ok_or_else(|| StateManagerError::SupplyCapExceeded(
                    "UTXO sum overflow during supply check".to_string()
                ))?;
        }

        // Compare with StateManager tracked supply
        if calculated_supply != self.current_total_supply {
            return Err(StateManagerError::ApplyBlockFailed(
                format!(
                    "Supply mismatch: UTXO sum {} != StateManager total supply {}. State corruption detected!",
                    calculated_supply, self.current_total_supply
                )
            ));
        }

        // Get current Verkle root (which includes supply leaf)
        let root = self.tree.get_root().unwrap_or([0u8; 32]);
        
        // Verify snapshot root matches current root (basic consistency)
        if let Some(snapshot) = self.snapshots.last() {
            if snapshot.root != root {
                return Err(StateManagerError::ApplyBlockFailed(
                    "Root hash mismatch between snapshot and current tree".to_string()
                ));
            }
        }

        Ok(())
    }

    /// Rebuild Verkle state root and prune old proofs to optimize storage
    ///
    /// This consolidates the current state and removes outdated proof data
    /// that is no longer needed for current state verification.
    ///
    /// # Returns
    /// Number of proof entries pruned
    pub fn rebuild_state_root_and_prune_proofs(&mut self) -> Result<usize, StateManagerError> {
        // For now, implement basic pruning of old prune markers
        // In a full implementation, this would rebuild the Verkle tree and prune old proofs
        let pruned_count = self.prune_markers.len().saturating_sub(1000); // Keep last 1000 markers
        if pruned_count > 0 {
            // Remove oldest markers (simple FIFO)
            let mut markers: Vec<_> = self.prune_markers.drain().collect();
            markers.sort_by_key(|(_, marker)| marker.timestamp);
            markers.truncate(1000);
            self.prune_markers = markers.into_iter().collect();
        }
        
        // TODO: Integrate with Verkle tree pruning in verkle_tree.rs
        Ok(pruned_count)
    }

    /// Get the current state root hash for backup validation
    pub fn get_current_state_root(&mut self) -> Hash {
        let root_bytes = self.tree.get_root().unwrap_or([0u8; 32]);
        Hash::from_bytes(&root_bytes)
    }

    /// Get the last applied block hash for backup validation
    pub fn get_last_block_hash(&self) -> Hash {
        self.current_block_hash.clone().unwrap_or_else(|| Hash::from_bytes(&[0u8; 32]))
    }
}

impl From<CoreError> for StateManagerError {
    fn from(err: CoreError) -> Self {
        StateManagerError::CryptographicError(format!("CoreError: {}", err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::crypto::Hash;
    use crate::core::dag::BlockNode;
    use crate::core::state::storage::MemoryStorage;
    use crate::core::state::transaction::{TxOutput, Transaction, TxInput, SigHashType};
    use std::collections::HashSet;

    fn make_coinbase_transaction(value: u64, pubkey_hash: Hash) -> Transaction {
        Transaction::new(
            Vec::new(),
            vec![TxOutput {
                value,
                pubkey_hash,
            }],
        )
    }

    fn make_block(id_bytes: &[u8], transactions: Vec<Transaction>) -> BlockNode {
        BlockNode {
            header: crate::core::dag::BlockHeader {
                id: Hash::new(id_bytes),
                parents: HashSet::new(),
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
            transactions,
        }
    }

    #[test]
    fn test_state_manager_apply_block() {
        let storage = MemoryStorage::new();
        let tree = VerkleTree::new(storage).expect("failed to create VerkleTree");
        let mut manager = StateManager::new(tree).expect("failed to create StateManager");
        let mut utxo = UtxoSet::new();

        let tx = make_coinbase_transaction(42, Hash::new(b"alice"));
        let block = make_block(b"block-1", vec![tx.clone()]);

        let root_before = manager.tree.get_root().expect("failed to get root");
        manager.apply_block(&block, &mut utxo).expect("apply block failed");
        let root_after = manager.tree.get_root().expect("failed to get root");

        assert_ne!(root_before, root_after);
        assert_eq!(manager.current_height, 1);
        assert_eq!(manager.snapshots.len(), 2);
        assert_eq!(utxo.utxos.len(), 1);
        assert_eq!(manager.get_state_at(1).unwrap().root, root_after);
    }

    #[test]
    fn test_state_manager_snapshot() {
        let storage = MemoryStorage::new();
        let tree = VerkleTree::new(storage).expect("failed to create VerkleTree");
        let mut manager = StateManager::new(tree).expect("failed to create StateManager");
        let mut utxo = UtxoSet::new();

        let block1 = make_block(b"block-1", vec![make_coinbase_transaction(10, Hash::new(b"alice"))]);
        manager.apply_block(&block1, &mut utxo).expect("apply block failed");
        let snapshot1 = manager.get_state_at(1).expect("snapshot at height 1");
        let snapshot1_root = snapshot1.root;
        let snapshot1_height = snapshot1.height;

        let block2 = make_block(b"block-2", vec![make_coinbase_transaction(20, Hash::new(b"bob"))]);
        manager.apply_block(&block2, &mut utxo).expect("apply block failed");
        let snapshot2 = manager.get_state_at(2).expect("snapshot at height 2");

        assert_ne!(snapshot1_root, snapshot2.root);
        assert_eq!(snapshot1_height, 1);
        assert_eq!(snapshot2.height, 2);
    }

    #[test]
    fn test_state_manager_rollback() {
        let storage = MemoryStorage::new();
        let tree = VerkleTree::new(storage).expect("failed to create VerkleTree");
        let mut manager = StateManager::new(tree).expect("failed to create StateManager");
        let mut utxo = UtxoSet::new();

        let block1 = make_block(b"block-1", vec![make_coinbase_transaction(10, Hash::new(b"alice"))]);
        manager.apply_block(&block1, &mut utxo).expect("apply block failed");
        let root1 = manager.tree.get_root().expect("failed to get root");

        let block2 = make_block(b"block-2", vec![make_coinbase_transaction(20, Hash::new(b"bob"))]);
        manager.apply_block(&block2, &mut utxo).expect("apply block failed");
        let root2 = manager.tree.get_root().expect("failed to get root");

        assert_ne!(root1, root2);
        assert_eq!(manager.current_height, 2);

        manager.rollback(1).expect("rollback failed");

        assert_eq!(manager.current_height, 1);
        assert_eq!(manager.snapshots.len(), 2);
        assert_eq!(manager.get_state_at(1).unwrap().root, root1);
        assert_eq!(manager.tree.get_root().expect("failed to get root"), root1);
    }

    #[test]
    fn test_state_manager_rollback_state_result() {
        let storage = MemoryStorage::new();
        let tree = VerkleTree::new(storage).expect("failed to create VerkleTree");
        let mut manager = StateManager::new(tree).expect("failed to create StateManager");
        let mut utxo = UtxoSet::new();

        let block1 = make_block(b"block-1", vec![make_coinbase_transaction(10, Hash::new(b"alice"))]);
        manager.apply_block(&block1, &mut utxo).expect("apply block failed");

        let block2 = make_block(b"block-2", vec![make_coinbase_transaction(20, Hash::new(b"bob"))]);
        manager.apply_block(&block2, &mut utxo).expect("apply block failed");

        // Successful rollback
        let result = manager.rollback_state(1);
        assert!(result.is_ok());
        assert_eq!(manager.current_height, 1);

        // Valid rollback to same height
        let result = manager.rollback_state(1);
        assert!(result.is_ok());

        // Invalid rollback to future height
        let result = manager.rollback_state(5);
        assert!(result.is_err());
        match result {
            Err(StateManagerError::InvalidRollback(_)) => {},
            _ => panic!("Expected InvalidRollback error"),
        }
    }

    #[test]
    fn test_state_manager_get_root_hash() {
        let storage = MemoryStorage::new();
        let tree = VerkleTree::new(storage).expect("failed to create VerkleTree");
        let mut manager = StateManager::new(tree).expect("failed to create StateManager");

        let root = manager.tree.get_root().unwrap_or([0u8; 32]);
        assert_eq!(root.len(), 32);
        assert_eq!(root, manager.snapshots[0].root);
    }

    #[test]
    fn test_state_manager_restore_from_snapshot() {
        let storage = MemoryStorage::new();
        let tree = VerkleTree::new(storage).expect("failed to create VerkleTree");
        let mut manager = StateManager::new(tree).expect("failed to create StateManager");
        let mut utxo = UtxoSet::new();

        let block1 = make_block(b"block-1", vec![make_coinbase_transaction(10, Hash::new(b"alice"))]);
        manager.apply_block(&block1, &mut utxo).expect("apply block failed");
        let snapshot1_root = manager.get_state_at(1).unwrap().root;

        let block2 = make_block(b"block-2", vec![make_coinbase_transaction(20, Hash::new(b"bob"))]);
        manager.apply_block(&block2, &mut utxo).expect("apply block failed");

        // Restore to height 1
        let result = manager.restore_from_snapshot(snapshot1_root, 1);
        assert!(result.is_ok());
        assert_eq!(manager.current_height, 1);
        assert_eq!(manager.tree.get_root().unwrap_or([0u8; 32]), snapshot1_root);
    }

    #[test]
    fn test_state_manager_validate_snapshots() {
        let storage = MemoryStorage::new();
        let tree = VerkleTree::new(storage).expect("failed to create VerkleTree");
        let mut manager = StateManager::new(tree).expect("failed to create StateManager");
        let mut utxo = UtxoSet::new();

        // Valid snapshots
        assert!(manager.validate_snapshots().is_ok());

        let block1 = make_block(b"block-1", vec![make_coinbase_transaction(10, Hash::new(b"alice"))]);
        manager.apply_block(&block1, &mut utxo).expect("apply block failed");
        assert!(manager.validate_snapshots().is_ok());

        let block2 = make_block(b"block-2", vec![make_coinbase_transaction(20, Hash::new(b"bob"))]);
        manager.apply_block(&block2, &mut utxo).expect("apply block failed");
        assert!(manager.validate_snapshots().is_ok());
    }

    #[test]
    fn test_state_manager_dag_reorganization() {
        let storage = MemoryStorage::new();
        let tree = VerkleTree::new(storage).expect("failed to create VerkleTree");
        let mut manager = StateManager::new(tree).expect("failed to create StateManager");
        let mut utxo = UtxoSet::new();

        // Apply block on main chain
        let block1_main = make_block(b"block-1-main", vec![make_coinbase_transaction(10, Hash::new(b"alice"))]);
        manager.apply_block(&block1_main, &mut utxo).expect("apply block failed");
        let root_1_main = manager.tree.get_root().unwrap_or([0u8; 32]);

        let block2_main = make_block(b"block-2-main", vec![make_coinbase_transaction(20, Hash::new(b"bob"))]);
        manager.apply_block(&block2_main, &mut utxo).expect("apply block failed");

        // DAG reorganization - rollback to height 1 and apply different chain
        manager.rollback_state(1).expect("rollback failed");

        let block2_alt = make_block(b"block-2-alt", vec![make_coinbase_transaction(15, Hash::new(b"charlie"))]);
        manager.apply_block(&block2_alt, &mut utxo).expect("apply block failed");
        let root_2_alt = manager.tree.get_root().unwrap_or([0u8; 32]);

        // Verify different root after reorg
        assert_ne!(root_1_main, root_2_alt);
        assert_eq!(manager.current_height, 2);
    }

    #[test]
    fn test_state_manager_multiple_snapshots() {
        let storage = MemoryStorage::new();
        let tree = VerkleTree::new(storage).expect("failed to create VerkleTree");
        let mut manager = StateManager::new(tree).expect("failed to create StateManager");
        let mut utxo = UtxoSet::new();

        // Create multiple blocks and snapshots
        for i in 1..=5 {
            let block = make_block(
                format!("block-{}", i).as_bytes(),
                vec![make_coinbase_transaction(10 * i as u64, Hash::new(format!("user-{}", i).as_bytes()))],
            );
            manager.apply_block(&block, &mut utxo).expect("apply block failed");
        }

        assert_eq!(manager.snapshots.len(), 6); // genesis + 5 blocks
        assert_eq!(manager.current_height, 5);

        // Verify snapshot progression
        for i in 0..=5 {
            let snapshot = manager.get_state_at(i as u64).expect("snapshot missing");
            assert_eq!(snapshot.height, i as u64);
        }

        // Rollback to middle
        manager.rollback_state(3).expect("rollback failed");
        assert_eq!(manager.snapshots.len(), 4); // genesis + 3 blocks
        assert_eq!(manager.current_height, 3);
    }

    #[test]
    fn test_state_manager_atomic_rollback_on_error() {
        // CRITICAL: Test automatic rollback when apply_block encounters error
        let storage = MemoryStorage::new();
        let tree = VerkleTree::new(storage).expect("failed to create VerkleTree");
        let mut manager = StateManager::new(tree).expect("failed to create StateManager");
        let mut utxo = UtxoSet::new();

        // Apply first block successfully
        let block1 = make_block(b"block-1", vec![make_coinbase_transaction(10, Hash::new(b"alice"))]);
        manager.apply_block(&block1, &mut utxo).expect("apply block 1 failed");
        let root_after_block1 = manager.tree.get_root().unwrap_or([0u8; 32]);
        let height_after_block1 = manager.current_height;
        let supply_after_block1 = manager.current_total_supply;

        // Prepare invalid transaction (non-existent input)
        let invalid_tx = Transaction { 
            execution_payload: Vec::new(), 
            contract_address: None, 
            gas_limit: 0, 
            max_fee_per_gas: 0,
            id: Hash::new(b"invalid_tx"),
            inputs: vec![TxInput {
                prev_tx: Hash::new(b"nonexistent"),
                index: 0,
                signature: vec![],
                pubkey: vec![],
                sighash_type: SigHashType::All,
            }],
            outputs: vec![TxOutput {
                value: 50,
                pubkey_hash: Hash::new(b"recipient"),
            }],
            chain_id: 1,
            locktime: 0,
        };

        let block2_invalid = make_block(b"block-2-invalid", vec![invalid_tx]);

        // Apply block with invalid transaction - should rollback
        let result = manager.apply_block(&block2_invalid, &mut utxo);
        assert!(result.is_err(), "Expected apply_block to fail");

        // Verify rollback occurred: height, supply, root should be restored
        assert_eq!(manager.current_height, height_after_block1, "Height was not rolled back");
        assert_eq!(manager.current_total_supply, supply_after_block1, "Supply was not rolled back");
        assert_eq!(
            manager.tree.get_root().unwrap_or([0u8; 32]),
            root_after_block1,
            "Root was not rolled back"
        );

        // Verify UTXO state also rolled back
        assert_eq!(utxo.utxos.len(), 1, "UTXO was not rolled back");
        assert_eq!(manager.outpoint_to_key.len(), 1, "outpoint_to_key was not rolled back");
        assert_eq!(manager.prune_markers.len(), 0, "prune_markers was not rolled back");
    }

    #[test]
    fn test_state_manager_supply_consistency_check() {
        // CRITICAL: Test cross-check between UTXO supply and StateManager tracked supply
        let storage = MemoryStorage::new();
        let tree = VerkleTree::new(storage).expect("failed to create VerkleTree");
        let mut manager = StateManager::new(tree).expect("failed to create StateManager");
        let mut utxo = UtxoSet::new();

        // Apply block with coinbase
        let block1 = make_block(b"block-1", vec![make_coinbase_transaction(100, Hash::new(b"alice"))]);
        manager.apply_block(&block1, &mut utxo).expect("apply block 1 failed");

        // Cross-check should pass
        let result = manager.cross_check_supply_consistency(&utxo);
        assert!(result.is_ok(), "Supply consistency check failed: {:?}", result);

        // Manually corrupt UTXO set (add extra value)
        let extra_tx = Hash::new(b"fake_tx");
        utxo.utxos.insert(
            (extra_tx, 0),
            TxOutput {
                value: 50,
                pubkey_hash: Hash::new(b"fake_owner"),
            },
        );

        // Cross-check should now fail
        let result = manager.cross_check_supply_consistency(&utxo);
        assert!(result.is_err(), "Supply consistency check should have detected mismatch");
    }
}

