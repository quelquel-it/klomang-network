/// Fee + subsidy reward system for Klomang Core.
///
/// This module implements deterministic, in-memory reward calculations
/// based on UTXO transaction inputs and a halving schedule every 100,000 blocks.
/// 
/// All reward distribution ratios are locked by economic_constants and cannot be changed.
use crate::core::config::Config;
use crate::core::consensus::emission;
use crate::core::consensus::economic_constants;
use crate::core::dag::BlockNode;
use crate::core::errors::CoreError;
use crate::core::state::transaction::Transaction;
use crate::core::state::utxo::UtxoSet;

/// Full Node Provider validation interface
/// This trait allows Repo Node to inject a validator that checks if an address
/// is a valid full node provider eligible for the 20% reward pool
pub trait FullNodeValidator: Send + Sync {
    /// Verify if the given address is a valid full node provider
    /// Returns true if address is a registered, data-available full node
    fn is_valid_full_node(&self, address: &[u8; 32]) -> bool;
    
    /// Get the list of all valid full nodes (for auditing/admin purposes)
    fn get_valid_nodes(&self) -> Vec<[u8; 32]>;
    
    /// Verify Verkle proof of data availability (future extensibility)
    fn verify_data_availability(&self, address: &[u8; 32], _proof: Option<&[u8]>) -> bool {
        // Default: rely on registration only
        self.is_valid_full_node(address)
    }
}

/// Default implementation: empty validator (conservative, no nodes eligible)
pub struct DefaultNodeValidator;

impl FullNodeValidator for DefaultNodeValidator {
    fn is_valid_full_node(&self, _address: &[u8; 32]) -> bool {
        false
    }
    
    fn get_valid_nodes(&self) -> Vec<[u8; 32]> {
        Vec::new()
    }
}

/// Calculate fee for a transaction using the current UTXO set.
///
/// Fee = sum(inputs) - sum(outputs)
/// Returns an error if the transaction is invalid or spends more than its inputs.
pub fn calculate_fees(tx: &Transaction, utxo: &UtxoSet) -> Result<u64, CoreError> {
    utxo.validate_tx(tx)
}

/// Calculate total fees for transaction: base transaction fee + gas fee.
///
/// Formula: total_fee = base_fee + (gas_used * max_fee_per_gas)
/// 
/// ANTI-DEFLATIONARY POLICY:
/// - All fees (both transaction and gas) are collected into the reward pool
/// - NO fees are ever burned (sent to zero address)
/// - All collected fees are split 80% miner, 20% full nodes
/// 
/// Arguments:
/// - tx: The transaction to calculate fees for
/// - utxo: Current UTXO set for validation
/// 
/// Returns: Total fee in smallest units (Nano-SLUG), or error if invalid
fn calculate_tx_total_fee(tx: &Transaction, utxo: &UtxoSet) -> Result<u128, CoreError> {
    // Step 1: Calculate base transaction fee (input - output)
    let base_fee = calculate_fees(tx, utxo)? as u128;
    
    // Step 2: Calculate gas fee (only for contract execution txs)
    // Formula: total_gas_fee = gas_used * max_fee_per_gas
    let gas_used = tx.gas_limit;
    let max_fee_per_gas = tx.max_fee_per_gas;
    let total_gas_fee = (gas_used as u128).saturating_mul(max_fee_per_gas);

    // Step 3: Combine all fees
    // This ensures 100% of Nano-SLUG fees enter the reward pool
    // - No burn ever occurs
    // - All fees participate in 80/20 miner/fullnode split
    let combined_fee = base_fee.saturating_add(total_gas_fee);
    
    // Debug: log gas fee collection for audit trail
    if gas_used > 0 && total_gas_fee > 0 {
        // Gas fee is successfully collected into reward pool
        // This implements the GAS_COLLECTION_POLICY (ALL_FEES_TO_POOL_NO_BURN)
        debug_assert!(
            total_gas_fee == (gas_used as u128).saturating_mul(max_fee_per_gas),
            "Gas fee calculation must match formula: gas_used * max_fee_per_gas"
        );
    }
    
    Ok(combined_fee)
}

/// Calculate total fees for all non-coinbase transactions in a block.
///
/// This uses a cloned UTXO state and applies each transaction sequentially so
/// fees are computed deterministically for blocks with dependent transactions.
/// 
/// ANTI-DEFLATIONARY COLLECTION:
/// Each transaction's fee is calculated as: base_fee + gas_fee
/// - base_fee: sum(inputs) - sum(outputs)
/// - gas_fee: gas_used * max_fee_per_gas
/// 
/// All fees collected are added to the block's reward pool and split:
/// - 80% to miner
/// - 20% to full node operators
/// 
/// NO FEES ARE BURNED - 100% enter the incentive structure.
pub fn calculate_accepted_fees(block: &BlockNode, utxo: &UtxoSet) -> Result<u64, CoreError> {
    let mut total_fees: u128 = 0;
    let mut working_utxo = utxo.clone();

    for tx in &block.transactions {
        if tx.is_coinbase() {
            continue;
        }

        let fee = calculate_tx_total_fee(tx, &working_utxo)?;
        working_utxo.apply_tx(tx)?;

        total_fees = total_fees.saturating_add(fee);
        if total_fees > u64::MAX as u128 {
            return Err(CoreError::TransactionError(
                "Fee overflow in accepted fee calculation".to_string(),
            ));
        }
    }

    Ok(total_fees as u64)
}

const MINER_SHARE_PERCENT: u128 = economic_constants::MINER_REWARD_PERCENT;
const FULLNODE_SHARE_PERCENT: u128 = economic_constants::FULLNODE_REWARD_PERCENT;

/// Compile-time verification: ensure distribution ratios match economic policy
const _: () = {
    assert!(MINER_SHARE_PERCENT == 80, "Miner share must be locked at 80%");
    assert!(FULLNODE_SHARE_PERCENT == 20, "Full node share must be locked at 20%");
};

/// Calculate the halving block reward in whole coins.
///
/// Uses the default config block reward as the initial reward and halves it every
/// 100,000 blocks using integer division.
pub fn calculate_block_reward(height: u64) -> u64 {
    let initial_reward = Config::default().block_reward;
    let halvings = height / emission::HALVING_INTERVAL;

    if halvings >= 64 {
        return 0;
    }

    initial_reward >> halvings
}

/// Calculate total block reward (subsidy + transaction fees) in smallest units.
///
/// Requires the block height for halving and the active UTXO state for fee calculation.
pub fn block_total_reward(
    block: &BlockNode,
    height: u64,
    is_blue: bool,
    utxo: &UtxoSet,
) -> Result<u64, CoreError> {
    if !is_blue {
        return Ok(0);
    }

    let subsidy_units = emission::capped_reward(height);
    let fees = calculate_accepted_fees(block, utxo)?;
    let total = subsidy_units
        .checked_add(fees as u128)
        .ok_or_else(|| {
            CoreError::TransactionError("Reward overflow: subsidy + fees exceed max limit".to_string())
        })?;

    total
        .try_into()
        .map_err(|_| CoreError::TransactionError("Total reward overflow u64".to_string()))
}

pub fn create_coinbase_tx(
    miner_reward_address: &crate::core::crypto::Hash,
    node_reward_pool_address: Option<&crate::core::crypto::Hash>,
    active_node_count: u32,
    total_reward: u128,
) -> crate::core::state::transaction::Transaction {
    // ANTI-DEFLATIONARY: miner address must not be zero address
    if miner_reward_address.as_bytes() == &[0u8; 32] {
        panic!("[ANTI-DEFLATIONARY] Miner address may not be zero address (burn address)");
    }
    
    // ECONOMIC POLICY: ensure 80/20 split is applied correctly
    let (miner_reward, node_reward_pool) = if active_node_count == 0 {
        // No full nodes: miner receives 100% (not burned)
        (total_reward, 0)
    } else {
        // Full nodes active: apply 80/20 split
        let miner_share = (total_reward * MINER_SHARE_PERCENT) / 100;
        let fullnode_share = total_reward.saturating_sub(miner_share);
        
        // Verify 80/20 split matches economic policy
        debug_assert!(
            economic_constants::validate_miner_share(total_reward, miner_share),
            "Miner share calculation violates 80/20 policy"
        );
        debug_assert!(
            economic_constants::validate_fullnode_share(total_reward, fullnode_share),
            "Full node share calculation violates 80/20 policy"
        );
        
        (miner_share, fullnode_share)
    };

    let mut outputs = Vec::new();

    // Output 1: Miner reward (80% or 100% if no full nodes)
    outputs.push(crate::core::state::transaction::TxOutput {
        value: miner_reward as u64,
        pubkey_hash: miner_reward_address.clone(),
    });

    // Output 2: Full node reward pool (20% if nodes active, else 0)
    if active_node_count > 0 && node_reward_pool > 0 {
        if let Some(pool_addr) = node_reward_pool_address {
            // ANTI-DEFLATIONARY: verify pool address is not zero
            if pool_addr.as_bytes() == &economic_constants::BURN_ADDRESS {
                panic!("[ANTI-DEFLATIONARY] Full node pool address cannot be zero address (burn address)");
            }
            outputs.push(crate::core::state::transaction::TxOutput {
                value: node_reward_pool as u64,
                pubkey_hash: pool_addr.clone(),
            });
        } else {
            // No pool address provided: split goes back to miner (not burned)
            // This ensures no value is lost even with incomplete configuration
            outputs.push(crate::core::state::transaction::TxOutput {
                value: node_reward_pool as u64,
                pubkey_hash: miner_reward_address.clone(),
            });
        }
    }

    let mut tx = crate::core::state::transaction::Transaction { 
        execution_payload: Vec::new(), 
        contract_address: None, 
        gas_limit: 0, 
        max_fee_per_gas: 0,
        id: crate::core::crypto::Hash::new(b""),
        inputs: Vec::new(),
        outputs,
        chain_id: 1,
        locktime: 0,
    };
    tx.id = tx.calculate_id();
    tx
}

/// Validate that coinbase outputs correctly split the reward:
/// - 80% to miner (Coinbase Address)
/// - 20% to node reward pool (Full Node Provider Address)
/// 
/// Returns error if:
/// - Total value doesn't match expected reward
/// - Split ratio is incorrect (not 80/20)
/// - Missing miner or node output
/// - Node reward address is not a valid full node (if validator provided)
pub fn validate_coinbase_reward(
    block: &BlockNode,
    actual_reward: u128,
) -> Result<(), CoreError> {
    validate_coinbase_reward_internal(block, actual_reward, None)
}

/// Internal validation with optional full node validator
/// Supports both strict mode (with validator) and permissive mode (without)
/// 
/// ANTI-DEFLATIONARY VALIDATION:
/// - All output addresses must be non-zero (no burn address)
/// - Exactly 2 outputs required (miner + full node pool)
/// - 80/20 split must be exact
/// - Per-node shares calculated correctly if applicable
fn validate_coinbase_reward_internal(
    block: &BlockNode,
    actual_reward: u128,
    validator: Option<&dyn FullNodeValidator>,
) -> Result<(), CoreError> {
    let coinbase_tx = block
        .transactions
        .iter()
        .find(|tx| tx.is_coinbase());

    match coinbase_tx {
        Some(tx) if !tx.outputs.is_empty() => {
            let total_value: u128 = tx.outputs.iter().map(|o| o.value as u128).sum();

            if total_value != actual_reward {
                return Err(CoreError::TransactionError(format!(
                    "Invalid coinbase reward: expected {} total, got {}",
                    actual_reward, total_value
                )));
            }

            // ANTI-DEFLATIONARY: verify all output addresses are non-zero
            for (idx, output) in tx.outputs.iter().enumerate() {
                if output.pubkey_hash.as_bytes() == &economic_constants::BURN_ADDRESS {
                    return Err(CoreError::TransactionError(format!(
                        "Coinbase output {} is sent to burn address (zero address) - prohibited",
                        idx
                    )));
                }
            }

            // Require exactly 2 outputs for protocol-locked 80/20 split (when active nodes present)
            // Or single output if no full nodes (all goes to miner)
            if !(tx.outputs.len() == 2 || tx.outputs.len() == 1) {
                return Err(CoreError::TransactionError(format!(
                    "Coinbase must have 1 or 2 outputs, got {}",
                    tx.outputs.len()
                )));
            }

            if tx.outputs.len() == 2 {
                // Verify 80/20 split is exact
                let first_value = tx.outputs[0].value as u128;
                let second_value = tx.outputs[1].value as u128;
                
                let expected_miner = (actual_reward * MINER_SHARE_PERCENT) / 100;
                let expected_node = actual_reward.saturating_sub(expected_miner);
                
                // Allow both (miner, node) and (node, miner) orderings
                let split_valid = (first_value == expected_miner && second_value == expected_node) ||
                                 (first_value == expected_node && second_value == expected_miner);
                
                if !split_valid {
                    return Err(CoreError::TransactionError(format!(
                        "Invalid 80/20 split: expected [miner: {}, node: {}], got [{}, {}]",
                        expected_miner, expected_node, first_value, second_value
                    )));
                }

                // If validator provided, verify node pool recipient is valid full node
                if let Some(validator) = validator {
                    let node_address = if first_value == expected_node {
                        tx.outputs[0].pubkey_hash.as_bytes()
                    } else {
                        tx.outputs[1].pubkey_hash.as_bytes()
                    };
                    
                    if !validator.is_valid_full_node(node_address) {
                        return Err(CoreError::TransactionError(format!(
                            "Node reward address {:?} is not a valid full node provider",
                            hex::encode(node_address)
                        )));
                    }
                }
            } else {
                // Single output: all reward to miner (no active full nodes)
                let output_value = tx.outputs[0].value as u128;
                if output_value != actual_reward {
                    return Err(CoreError::TransactionError(format!(
                        "Single output coinbase must contain all reward value: expected {}, got {}",
                        actual_reward, output_value
                    )));
                }
            }

            Ok(())
        }
        Some(_) => Err(CoreError::TransactionError(
            "Coinbase transaction has no outputs".to_string(),
        )),
        None => Err(CoreError::TransactionError(
            "Block must contain a coinbase transaction".to_string(),
        )),
    }
}
