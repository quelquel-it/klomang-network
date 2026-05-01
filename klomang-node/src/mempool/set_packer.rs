//! Transaction Set Packing Optimizer
//!
//! Implements greedy approximation for transaction selection optimization
//! with sovereign set logic for dependent transactions.

use klomang_core::core::state::transaction::Transaction;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Represents a sovereign set of dependent transactions
#[derive(Clone, Debug)]
pub struct SovereignSet {
    /// Transactions in this set (inseparable unit)
    pub transactions: Vec<Arc<Transaction>>,
    /// Total weight of the set
    pub total_weight: usize,
    /// Total fee of the set
    pub total_fee: u64,
    /// Fee per weight ratio for greedy selection
    pub fee_density: f64,
}

impl SovereignSet {
    /// Create new sovereign set from transactions
    pub fn new(transactions: Vec<Arc<Transaction>>) -> Result<Self, String> {
        let mut total_weight = 0;
        let mut total_fee = 0;

        for tx in &transactions {
            let weight = Self::calculate_weight(tx)?;
            total_weight += weight;
            total_fee += Self::extract_fee(tx);
        }

        let fee_density = if total_weight > 0 {
            total_fee as f64 / total_weight as f64
        } else {
            0.0
        };

        Ok(Self {
            transactions,
            total_weight,
            total_fee,
            fee_density,
        })
    }

    /// Calculate transaction weight using core transaction data
    fn calculate_weight(tx: &Transaction) -> Result<usize, String> {
        // Base weight + inputs/outputs weight
        let base_weight = 100;
        let input_weight = tx.inputs.len() * 50;
        let output_weight = tx.outputs.len() * 30;

        Ok(base_weight + input_weight + output_weight)
    }

    /// Extract fee from transaction (simplified - assume fee is in outputs or metadata)
    fn extract_fee(tx: &Transaction) -> u64 {
        // Placeholder: assume fee is stored in transaction metadata or calculated
        // In real implementation, fee would be part of transaction structure
        // For now, use a default or calculate from size
        (tx.inputs.len() + tx.outputs.len()) as u64 * 10
    }
}

/// Transaction Set Packing Optimizer
///
/// Uses greedy approximation algorithm to select optimal transaction combinations
/// within weight constraints, treating dependent transactions as sovereign units.
pub struct SetPacker {
    /// Cache of computed sovereign sets
    set_cache: RwLock<HashMap<Vec<u8>, SovereignSet>>,
}

impl SetPacker {
    /// Create new set packer
    pub fn new() -> Self {
        Self {
            set_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Pack optimal transaction sets within weight limit
    ///
    /// Uses greedy algorithm to maximize total fee while respecting max_weight.
    /// Dependent transactions are grouped into sovereign sets that cannot be split.
    pub fn pack_sets(
        &self,
        transactions: &[Arc<Transaction>],
        max_weight: usize,
    ) -> Result<Vec<SovereignSet>, String> {
        if transactions.is_empty() {
            return Ok(Vec::new());
        }

        // Group transactions into sovereign sets based on dependencies
        let sovereign_sets = self.build_sovereign_sets(transactions)?;

        // Sort sets by fee density (greedy selection)
        let mut sorted_sets = sovereign_sets;
        sorted_sets.sort_by(|a, b| {
            b.fee_density
                .partial_cmp(&a.fee_density)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Greedy selection within weight limit
        let mut selected_sets = Vec::new();
        let mut current_weight = 0;

        for set in sorted_sets {
            if current_weight + set.total_weight <= max_weight {
                selected_sets.push(set.clone());
                current_weight += set.total_weight;
            }
        }

        Ok(selected_sets)
    }

    /// Build sovereign sets from transactions
    ///
    /// Groups dependent transactions into inseparable units.
    /// For simplicity, treats each transaction as its own set initially.
    /// In full implementation, this would analyze dependency graph.
    fn build_sovereign_sets(
        &self,
        transactions: &[Arc<Transaction>],
    ) -> Result<Vec<SovereignSet>, String> {
        let mut sets = Vec::new();

        for tx in transactions {
            let tx_hash =
                bincode::serialize(&tx.id).map_err(|e| format!("Serialization error: {}", e))?;

            // Check cache first
            if let Some(cached_set) = self.set_cache.read().get(&tx_hash) {
                sets.push(cached_set.clone());
                continue;
            }

            // Create new set (single transaction for now)
            let set = SovereignSet::new(vec![Arc::clone(tx)])?;

            // Cache the set
            self.set_cache.write().insert(tx_hash, set.clone());

            sets.push(set);
        }

        Ok(sets)
    }

    /// Clear cache (for memory management)
    pub fn clear_cache(&self) {
        self.set_cache.write().clear();
    }
}
