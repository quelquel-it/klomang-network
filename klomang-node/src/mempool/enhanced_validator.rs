//! Enhanced Transaction Validator with Dependency Tracking Integration
//!
//! Extends the base validator with hooks to the dependency tracking system.
//! When a transaction is validated, its parent-child relationships are tracked
//! for cascade validation when parents are confirmed.

use std::sync::Arc;

use klomang_core::core::state::transaction::Transaction;

use crate::storage::error::StorageResult;

use super::validation::{PoolValidator, ValidationResult};
use super::advanced_dependency_manager::TxDependencyManager;

/// Validation result with dependency tracking information
#[derive(Clone, Debug)]
pub struct EnhancedValidationResult {
    /// Base validation result
    pub validation_result: ValidationResult,
    /// Number of dependency ancestors
    pub ancestor_count: usize,
    /// Execution depth in dependency chain (0 = all inputs on-chain)
    pub execution_depth: u32,
    /// Whether transaction is immediately executable
    pub is_immediately_executable: bool,
}

/// Enhanced validator that integrates with dependency tracking
pub struct EnhancedPoolValidator {
    /// Base validator for UTXO checking
    base_validator: Arc<PoolValidator>,
    /// Dependency manager for tracking relationships
    dependency_manager: Arc<TxDependencyManager>,
}

impl EnhancedPoolValidator {
    /// Create new enhanced validator
    pub fn new(
        base_validator: Arc<PoolValidator>,
        dependency_manager: Arc<TxDependencyManager>,
    ) -> Self {
        Self {
            base_validator,
            dependency_manager,
        }
    }

    /// Validate transaction and track its dependencies
    pub fn validate_and_track(&self, tx: &Transaction) -> StorageResult<EnhancedValidationResult> {
        // First, run base validation
        let validation_result = self.base_validator.validate_transaction(tx)?;

        // Then, register with dependency manager if valid
        let (ancestor_count, execution_depth, is_immediately_executable) = match &validation_result {
            ValidationResult::Valid => {
                // Register transaction with dependencies
                let dep_chain = self.dependency_manager.register_transaction(tx)?;

                let ancestors = dep_chain.all_ancestors.len();
                let depth = dep_chain.execution_depth;
                let immediately_exec = depth == 0; // Immediately executable if at depth 0

                (ancestors, depth, immediately_exec)
            }
            ValidationResult::MissingInputs(_) => {
                // Even for missing inputs, we try to register to track orphan relationships
                if let Ok(dep_chain) = self.dependency_manager.register_transaction(tx) {
                    let ancestors = dep_chain.all_ancestors.len();
                    let depth = dep_chain.execution_depth;
                    (ancestors, depth, false)
                } else {
                    (0, 0, false)
                }
            }
            _ => (0, 0, false),
        };

        Ok(EnhancedValidationResult {
            validation_result,
            ancestor_count,
            execution_depth,
            is_immediately_executable,
        })
    }

    /// Get dependency chain for validated transaction
    pub fn get_dependency_info(&self, tx_hash: &[u8]) -> Option<super::advanced_dependency_manager::DependencyChain> {
        self.dependency_manager.get_dependency_chain(&tx_hash.to_vec())
    }

    /// Check if transaction has circular dependencies
    pub fn has_circular_dependency(&self, tx: &Transaction) -> StorageResult<bool> {
        // Try to register - if it fails with cycle detection, return true
        match self.dependency_manager.register_transaction(tx) {
            Ok(_) => Ok(false),
            Err(_) => Ok(true), // Assume cycle if registration fails
        }
    }

    /// Get base validator
    pub fn base_validator(&self) -> Arc<PoolValidator> {
        self.base_validator.clone()
    }

    /// Get dependency manager
    pub fn dependency_manager(&self) -> Arc<TxDependencyManager> {
        self.dependency_manager.clone()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_enhanced_validator_creation() {
        // Placeholder test - integration tests will verify functionality
    }
}
