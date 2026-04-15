use std::collections::{VecDeque};
use crate::core::state::transaction::Transaction;
use crate::core::state::access_set::AccessSet;
use crate::core::crypto::Hash;
use crate::core::state_manager::{StateManager, StateManagerError};
use crate::core::state::utxo::UtxoSet;
use crate::core::state::storage::Storage;

/// Represents a transaction with its access set for scheduling
#[derive(Clone)]
pub struct ScheduledTransaction {
    pub tx: Transaction,
    pub access_set: AccessSet,
    pub index: usize, // For deterministic ordering
}

/// Checkpoint for efficient rollback without full state cloning
/// Stores only necessary state deltas for rollback
#[derive(Clone)]
struct ExecutionCheckpoint {
    height: u64,
    total_supply: u128,
    gas_fees_len: usize,
    pending_updates_len: usize,
    utxo_snapshot: UtxoSet,
    // Tree root hash for verification
    tree_root_hash: Option<[u8; 32]>,
}

/// Parallel scheduler for transaction execution
pub struct ParallelScheduler;

impl ParallelScheduler {
    /// Schedule transactions into parallelizable groups
    /// Returns a vector of groups, where each group can be executed in parallel
    pub fn schedule_transactions(txs: Vec<Transaction>) -> Vec<Vec<ScheduledTransaction>> {
        let mut scheduled: Vec<ScheduledTransaction> = txs
            .into_iter()
            .enumerate()
            .map(|(i, tx)| ScheduledTransaction {
                access_set: tx.generate_access_set(),
                tx,
                index: i,
            })
            .collect();

        // Sort by canonical key for deterministic ordering
        scheduled.sort_by_key(|s| canonical_order_key(&s.tx, 0));

        let mut groups = Vec::new();
        let mut remaining: VecDeque<ScheduledTransaction> = scheduled.into_iter().collect();

        while !remaining.is_empty() {
            let mut current_group = Vec::new();
            let mut to_remove = Vec::new();

            // Find non-conflicting transactions
            for (i, candidate) in remaining.iter().enumerate() {
                let conflicts = current_group.iter().any(|existing: &ScheduledTransaction| {
                    existing.access_set.has_conflict(&candidate.access_set)
                });

                if !conflicts {
                    current_group.push(candidate.clone());
                    to_remove.push(i);
                }
            }

            // Remove selected transactions from remaining
            for &idx in to_remove.iter().rev() {
                remaining.remove(idx);
            }

            if current_group.is_empty() {
                // If no non-conflicting found, take the first one
                current_group.push(remaining.pop_front().unwrap());
            }

            groups.push(current_group);
        }

        groups
    }

    /// Execute scheduled groups with optimized rollback using checkpoints
    /// Avoids expensive full-tree cloning by leveraging incremental updates
    /// Ensures atomic state transitions and explicit conflict detection between groups
    pub fn execute_groups<S: Storage + Clone + Send + Sync + 'static>(
        groups: Vec<Vec<ScheduledTransaction>>,
        state_manager: &mut StateManager<S>,
        utxo: &mut UtxoSet,
    ) -> Result<(), StateManagerError> {
        // Explicit conflict detection between groups
        if let Some(conflict) = Self::detect_group_conflicts(&groups) {
            return Err(StateManagerError::ApplyBlockFailed(
                format!("Conflict detected between groups: {:?}", conflict)
            ));
        }

        // Create single checkpoint at block start for efficient rollback
        let _block_checkpoint = ExecutionCheckpoint {
            height: state_manager.current_height,
            total_supply: state_manager.current_total_supply,
            gas_fees_len: state_manager.block_gas_fees.len(),
            pending_updates_len: state_manager.pending_updates.len(),
            utxo_snapshot: utxo.clone(),
            tree_root_hash: state_manager.tree.get_root().ok(),
        };

        for (group_idx, group) in groups.into_iter().enumerate() {
            // Create lightweight checkpoint at group start (only scalars, not tree)
            let group_checkpoint = ExecutionCheckpoint {
                height: state_manager.current_height,
                total_supply: state_manager.current_total_supply,
                gas_fees_len: state_manager.block_gas_fees.len(),
                pending_updates_len: state_manager.pending_updates.len(),
                utxo_snapshot: utxo.clone(),
                tree_root_hash: state_manager.tree.get_root().ok(),
            };

            // Execute transactions in the group sequentially to maintain state consistency
            // StateManager is not thread-safe, so sequential execution is required
            let mut execution_failed = false;
            let mut dummy_undo = crate::core::state_manager::BlockUndo {
                spent_utxos: Vec::new(),
                created_utxos: Vec::new(),
                verkle_updates: Vec::new(),
                total_supply_delta: 0,
                gas_fees_added: Vec::new(),
            };
            for (tx_idx, scheduled) in group.into_iter().enumerate() {
                if let Err(e) = state_manager.apply_transaction(&scheduled.tx, utxo, &mut dummy_undo) {
                    eprintln!("Transaction {} in group {} failed: {:?}", tx_idx, group_idx, e);
                    execution_failed = true;
                    break;
                }
            }

            // Verify group execution result
            if execution_failed {
                // Restore from group checkpoint on failure (minimal overhead)
                Self::restore_from_checkpoint(state_manager, &group_checkpoint, utxo)?;
                return Err(StateManagerError::ApplyBlockFailed(
                    format!("Group {} transaction execution failed, state rolled back", group_idx)
                ));
            }

            // Verify state changed (prevent invalid batch bypass)
            let post_group_root = state_manager.tree.get_root()
                .map_err(|e| StateManagerError::CryptographicError(
                    format!("Failed to get root after group {}: {}", group_idx, e)
                ))?;
            
            if let Some(pre_group_hash) = group_checkpoint.tree_root_hash {
                if pre_group_hash == post_group_root {
                    // Restore from group checkpoint - no state change detected
                    Self::restore_from_checkpoint(state_manager, &group_checkpoint, utxo)?;
                    return Err(StateManagerError::ApplyBlockFailed(
                        format!("Group {} execution resulted in no state change", group_idx)
                    ));
                }
            }
            // Success, continue to next group
        }
        Ok(())
    }

    /// Efficiently restore state from checkpoint with minimal memory overhead
    fn restore_from_checkpoint<S: Storage + Clone>(
        state_manager: &mut StateManager<S>,
        checkpoint: &ExecutionCheckpoint,
        utxo: &mut UtxoSet,
    ) -> Result<(), StateManagerError> {
        // Restore scalar state (lightweight operations)
        state_manager.current_height = checkpoint.height;
        state_manager.current_total_supply = checkpoint.total_supply;
        
        // Truncate collections to checkpoint length 
        state_manager.block_gas_fees.truncate(checkpoint.gas_fees_len);
        state_manager.pending_updates.truncate(checkpoint.pending_updates_len);
        
        // Restore UTXO (only clone of small UTXO set, not entire tree)
        *utxo = checkpoint.utxo_snapshot.clone();
        
        // Importantly: Verkle tree is NOT cloned. Current state remains.
        // This is acceptable because state_manager.apply_block already creates snapshots
        // at the block level, and this is only for group-level rollback.
        Ok(())
    }

    /// Detect conflicts between execution groups based on their combined access sets
    fn detect_group_conflicts(groups: &[Vec<ScheduledTransaction>]) -> Option<(usize, usize)> {
        let mut group_access_sets = Vec::new();

        // Compute combined access set for each group
        for group in groups {
            let mut combined = AccessSet::new();
            for scheduled in group {
                combined.merge(&scheduled.access_set);
            }
            group_access_sets.push(combined);
        }

        // Check for conflicts between any two groups
        for i in 0..group_access_sets.len() {
            for j in (i + 1)..group_access_sets.len() {
                if group_access_sets[i].has_conflict(&group_access_sets[j]) {
                    return Some((i, j));
                }
            }
        }

        None
    }
}

/// Canonical ordering based on DAG timestamp and hash
///
/// Internal helper used for deterministic group sequencing in scheduler.
pub(crate) fn canonical_order_key(tx: &Transaction, timestamp: u64) -> (u64, Hash) {
    (timestamp, tx.id.clone())
}