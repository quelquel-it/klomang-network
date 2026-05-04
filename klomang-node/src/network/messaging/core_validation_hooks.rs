//! Klomang-Core Validation Hooks untuk Gossip Spam Protection
//!
//! Integration layer yang menggunakan klomang-core validation functions
//! untuk:
//! - Detect spam transactions (invalid fee, structure, signature)
//! - Validate transactions sebelum mempropagate
//! - Apply peer scoring berdasarkan validation results
//! - Hook ke verify_spam_potential() sebelum message processing

use libp2p::gossipsub::Message;
use libp2p::PeerId;
use log::{warn, debug};
use std::sync::Arc;

use klomang_core::core::state::transaction::Transaction;
use klomang_core::core::dag::BlockNode;

use crate::network::messaging::peer_scoring::{PeerScoringManager, ValidationOutcome};

/// Validation hook result
#[derive(Clone, Debug)]
pub enum ValidationHookResult {
    /// Message passed validation
    Valid,
    /// Message failed validation - peer should be penalized
    Invalid(ValidationOutcome),
}

impl ValidationHookResult {
    /// Check if valid
    pub fn is_valid(&self) -> bool {
        matches!(self, ValidationHookResult::Valid)
    }
}

/// Klomang-core validation hooks
pub struct CoreValidationHooks {
    /// Peer scoring manager untuk apply penalties
    scoring_manager: Arc<PeerScoringManager>,
}

impl CoreValidationHooks {
    /// Create new validation hooks
    pub fn new(scoring_manager: Arc<PeerScoringManager>) -> Self {
        Self { scoring_manager }
    }

    /// Verify spam potential sebelum message processing
    ///
    /// This is the main hook yang dipanggil setelah message lolos filter dasar
    pub async fn verify_spam_potential(
        &self,
        peer_id: &PeerId,
        message: &Message,
    ) -> ValidationHookResult {
        // Determine message type dan validate accordingly
        match message.topic.as_str() {
            "klomang/transactions/v1" => {
                self.validate_transaction_payload(peer_id, message)
            }
            "klomang/blocks/v1" => {
                self.validate_block_payload(peer_id, message)
            }
            _ => {
                debug!("Unknown topic: {}", message.topic);
                ValidationHookResult::Valid // Let other layers handle unknown topics
            }
        }
    }

    /// Validate transaction payload dari gossipsub message
    fn validate_transaction_payload(
        &self,
        peer_id: &PeerId,
        message: &Message,
    ) -> ValidationHookResult {
        debug!("Validating transaction message from {}", peer_id);

        // Deserialize transaction
        let tx = match bincode::deserialize::<Transaction>(&message.data) {
            Ok(t) => t,
            Err(e) => {
                warn!("Failed to deserialize transaction from {}: {}", peer_id, e);
                let outcome = ValidationOutcome::InvalidStructure(20.0);
                self.scoring_manager
                    .apply_validation_outcome(peer_id, &outcome);
                return ValidationHookResult::Invalid(outcome);
            }
        };

        // Check 1: Validate transaction structure
        if !self.check_transaction_structure(&tx) {
            warn!("Invalid transaction structure from {}", peer_id);
            let outcome = ValidationOutcome::InvalidStructure(15.0);
            self.scoring_manager
                .apply_validation_outcome(peer_id, &outcome);
            return ValidationHookResult::Invalid(outcome);
        }

        // Check 2: Validate transaction ID
        if tx.id != tx.calculate_id() {
            warn!("Transaction ID mismatch from {}", peer_id);
            let outcome = ValidationOutcome::InvalidStructure(15.0);
            self.scoring_manager
                .apply_validation_outcome(peer_id, &outcome);
            return ValidationHookResult::Invalid(outcome);
        }

        // Check 3: Validate signatures
        if let Some(invalid_count) = self.validate_transaction_signatures(&tx) {
            warn!(
                "Invalid signatures in transaction from {}: {} inputs failed",
                peer_id, invalid_count
            );
            let outcome = ValidationOutcome::InvalidSignature(25.0);
            self.scoring_manager
                .apply_validation_outcome(peer_id, &outcome);
            return ValidationHookResult::Invalid(outcome);
        }

        // Check 4: Detect spam patterns (fee analysis, structure patterns)
        if self.detect_spam_patterns(&tx) {
            warn!("Spam pattern detected in transaction from {}", peer_id);
            let outcome = ValidationOutcome::InvalidStructure(20.0);
            self.scoring_manager
                .apply_validation_outcome(peer_id, &outcome);
            return ValidationHookResult::Invalid(outcome);
        }

        // All checks passed
        debug!("Transaction from {} passed all validation checks", peer_id);
        let outcome = ValidationOutcome::Valid;
        self.scoring_manager
            .apply_validation_outcome(peer_id, &outcome);
        ValidationHookResult::Valid
    }

    /// Validate block payload dari gossipsub message
    fn validate_block_payload(
        &self,
        peer_id: &PeerId,
        message: &Message,
    ) -> ValidationHookResult {
        debug!("Validating block message from {}", peer_id);

        // Deserialize block
        let block = match bincode::deserialize::<BlockNode>(&message.data) {
            Ok(b) => b,
            Err(e) => {
                warn!("Failed to deserialize block from {}: {}", peer_id, e);
                let outcome = ValidationOutcome::InvalidStructure(25.0);
                self.scoring_manager
                    .apply_validation_outcome(peer_id, &outcome);
                return ValidationHookResult::Invalid(outcome);
            }
        };

        // Check 1: Block structure (non-empty ID, valid timestamp)
        if block.header.id.as_bytes().is_empty() {
            warn!("Block with empty ID from {}", peer_id);
            let outcome = ValidationOutcome::InvalidStructure(20.0);
            self.scoring_manager
                .apply_validation_outcome(peer_id, &outcome);
            return ValidationHookResult::Invalid(outcome);
        }

        // Check 2: Timestamp validation
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        
        if block.header.timestamp > current_time + 600 {
            // 10 minutes tolerance
            warn!(
                "Block timestamp too far in future from {}: {}",
                peer_id, block.header.timestamp
            );
            let outcome = ValidationOutcome::InvalidStructure(15.0);
            self.scoring_manager
                .apply_validation_outcome(peer_id, &outcome);
            return ValidationHookResult::Invalid(outcome);
        }

        // Check 3: Block verification (cek struktur transaksi dalam block)
        if !self.validate_block_transactions(&block) {
            warn!("Invalid transactions in block from {}", peer_id);
            let outcome = ValidationOutcome::InvalidStructure(20.0);
            self.scoring_manager
                .apply_validation_outcome(peer_id, &outcome);
            return ValidationHookResult::Invalid(outcome);
        }

        debug!("Block from {} passed all validation checks", peer_id);
        let outcome = ValidationOutcome::Valid;
        self.scoring_manager
            .apply_validation_outcome(peer_id, &outcome);
        ValidationHookResult::Valid
    }

    /// Check basic transaction structure
    fn check_transaction_structure(&self, tx: &Transaction) -> bool {
        // Transaction must have inputs dan outputs
        if tx.inputs.is_empty() && !tx.is_coinbase() {
            return false;
        }

        if tx.outputs.is_empty() {
            return false;
        }

        // Each input must have signature dan pubkey
        for input in &tx.inputs {
            if input.signature.is_empty() {
                return false;
            }
            // Pubkey dapat empty untuk coinbase
            if input.pubkey.is_empty() && !tx.is_coinbase() {
                return false;
            }
        }

        true
    }

    /// Validate transaction signatures
    fn validate_transaction_signatures(&self, tx: &Transaction) -> Option<usize> {
        let mut invalid_count = 0;

        for (idx, input) in tx.inputs.iter().enumerate() {
            // Skip validation untuk coinbase
            if tx.is_coinbase() {
                continue;
            }

            // Check signature length
            if input.signature.len() != 64 {
                invalid_count += 1;
                continue;
            }

            // Check pubkey length (32 or 33 bytes)
            if input.pubkey.len() != 32 && input.pubkey.len() != 33 {
                invalid_count += 1;
                continue;
            }

            // Compute sighash dan verify signature
            match klomang_core::core::crypto::schnorr::compute_sighash(tx, idx, input.sighash_type) {
                Ok(sighash) => {
                    let mut pubkey_bytes = [0u8; 32];
                    if input.pubkey.len() >= 32 {
                        pubkey_bytes.copy_from_slice(&input.pubkey[..32]);
                    } else {
                        continue; // Invalid pubkey length
                    }

                    let mut sig_bytes = [0u8; 64];
                    sig_bytes.copy_from_slice(&input.signature[..64]);

                    match klomang_core::core::crypto::schnorr::verify_schnorr(
                        &pubkey_bytes,
                        &sig_bytes,
                        &sighash,
                    ) {
                        Ok(true) => {
                            // Signature valid
                        }
                        Ok(false) => {
                            debug!("Signature verification failed for input {}", idx);
                            invalid_count += 1;
                        }
                        Err(e) => {
                            debug!("Signature verification error for input {}: {}", idx, e);
                            invalid_count += 1;
                        }
                    }
                }
                Err(e) => {
                    debug!("Failed to compute sighash for input {}: {}", idx, e);
                    invalid_count += 1;
                }
            }
        }

        if invalid_count > 0 {
            Some(invalid_count)
        } else {
            None
        }
    }

    /// Detect spam patterns dalam transaction
    fn detect_spam_patterns(&self, tx: &Transaction) -> bool {
        // Pattern 1: Zero fee (highly suspicious unless intentional)
        if tx.gas_limit == 0 && tx.max_fee_per_gas == 0 {
            debug!("Zero fee transaction detected - potential spam");
            return true;
        }

        // Pattern 2: Excessive inputs/outputs (potential memory bomb)
        if tx.inputs.len() > 1000 || tx.outputs.len() > 1000 {
            warn!(
                "Excessive inputs/outputs detected: {} inputs, {} outputs",
                tx.inputs.len(),
                tx.outputs.len()
            );
            return true;
        }

        // Pattern 3: Invalid chain_id
        if tx.chain_id == 0 {
            debug!("Invalid chain_id (0) detected");
            return true;
        }

        false
    }

    /// Validate transactions dalam block
    fn validate_block_transactions(&self, block: &BlockNode) -> bool {
        // Basic check: block harus punya transactions
        // Jangan validate isi transaction, hanya structure
        if block.transactions.is_empty() {
            // Some blocks might be empty, yang penting struktur valid
        }

        // Check each transaction structure
        for tx in &block.transactions {
            if !self.check_transaction_structure(tx) {
                return false;
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validation_hook_result_is_valid() {
        let valid_result = ValidationHookResult::Valid;
        assert!(valid_result.is_valid());

        let invalid_result =
            ValidationHookResult::Invalid(ValidationOutcome::InvalidStructure(15.0));
        assert!(!invalid_result.is_valid());
    }
}
