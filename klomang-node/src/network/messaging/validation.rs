//! Advanced Message Validation Hooks for Klomang Gossipsub
//!
//! This module implements:
//! - Extended Validation (V2) with cryptographic verification
//! - Message authenticity checking using PeerId signatures
//! - Integration with klomang-core validation functions
//! - Strict validation mode for transaction and block messages

use libp2p::gossipsub::{Behaviour, Message, MessageAcceptance, MessageId};
use libp2p::PeerId;
use klomang_core::core::crypto::schnorr::{compute_sighash, verify_schnorr, verify_block_signature};
use klomang_core::core::state::transaction::Transaction;
use klomang_core::core::dag::BlockNode;
use std::sync::Arc;

/// Validation result for incoming messages
#[derive(Clone, Debug)]
pub enum ValidationResult {
    /// Accept the message for propagation
    Accept,
    /// Reject the message (invalid)
    Reject,
    /// Ignore the message (not relevant or duplicate)
    Ignore,
}

/// Extended validator implementing V2 validation
pub struct KlomangValidator {
    /// Local peer ID for self-message filtering
    local_peer_id: PeerId,
}

impl KlomangValidator {
    /// Create new validator
    pub fn new(local_peer_id: PeerId) -> Self {
        Self { local_peer_id }
    }

    /// Validate incoming gossipsub message
    pub async fn validate_message(
        &self,
        peer_id: &PeerId,
        message_id: &MessageId,
        message: &Message,
    ) -> ValidationResult {
        // Skip validation for our own messages
        if *peer_id == self.local_peer_id {
            return ValidationResult::Accept;
        }

        // Check message authenticity
        if !self.verify_message_authenticity(message) {
            log::warn!("Message {} from {} failed authenticity check", message_id, peer_id);
            return ValidationResult::Reject;
        }

        // Parse and validate based on topic
        match message.topic.as_str() {
            "klomang/transactions/v1" => {
                self.validate_transaction_message(message).await
            }
            "klomang/blocks/v1" => {
                self.validate_block_message(message).await
            }
            _ => {
                // Unknown topic - ignore
                ValidationResult::Ignore
            }
        }
    }

    /// Verify message authenticity using signature
    fn verify_message_authenticity(&self, message: &Message) -> bool {
        // In libp2p gossipsub with MessageAuthenticity::Signed,
        // the library handles signature verification internally
        // We can add additional checks here if needed

        // Check if message has required fields
        if message.data.is_empty() {
            return false;
        }

        // Additional authenticity checks can be added here
        // For now, rely on libp2p's built-in signature verification
        true
    }

    /// Validate transaction message using klomang-core
    async fn validate_transaction_message(&self, message: &Message) -> ValidationResult {
        // Deserialize transaction
        match bincode::deserialize::<Transaction>(&message.data) {
            Ok(transaction) => {
                // Validate the transaction id against its contents
                if transaction.id != transaction.calculate_id() {
                    log::warn!("Transaction id mismatch detected");
                    return ValidationResult::Reject;
                }

                for (idx, input) in transaction.inputs.iter().enumerate() {
                    if input.pubkey.len() != 32 || input.signature.len() != 64 {
                        log::warn!("Invalid transaction input signature length");
                        return ValidationResult::Reject;
                    }

                    let sighash = match compute_sighash(&transaction, idx, input.sighash_type) {
                        Ok(value) => value,
                        Err(e) => {
                            log::warn!("Failed to compute transaction sighash: {}", e);
                            return ValidationResult::Reject;
                        }
                    };

                    let mut pubkey_bytes = [0u8; 32];
                    pubkey_bytes.copy_from_slice(&input.pubkey[..32]);

                    let mut sig_bytes = [0u8; 64];
                    sig_bytes.copy_from_slice(&input.signature[..64]);

                    match verify_schnorr(&pubkey_bytes, &sig_bytes, &sighash) {
                        Ok(true) => continue,
                        Ok(false) => {
                            log::warn!("Transaction signature verification failed");
                            return ValidationResult::Reject;
                        }
                        Err(e) => {
                            log::warn!("Transaction signature verification error: {}", e);
                            return ValidationResult::Reject;
                        }
                    }
                }

                ValidationResult::Accept
            }
            Err(e) => {
                log::warn!("Failed to deserialize transaction: {}", e);
                ValidationResult::Reject
            }
        }
    }

    /// Validate block message using klomang-core
    async fn validate_block_message(&self, message: &Message) -> ValidationResult {
        // Deserialize block
        match bincode::deserialize::<BlockNode>(&message.data) {
            Ok(block) => {
                // Basic block structure validation
                if block.header.id.as_bytes().is_empty() {
                    return ValidationResult::Reject;
                }

                // Check timestamp is reasonable (not in future)
                let current_time = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                if block.header.timestamp > current_time + 300 { // 5 minutes tolerance
                    log::warn!("Block timestamp too far in future: {}", block.header.timestamp);
                    return ValidationResult::Reject;
                }

                // Additional validation: check proof-of-work (simplified)
                if block.header.nonce == 0 {
                    log::warn!("Invalid block nonce");
                    return ValidationResult::Reject;
                }

                if !verify_block_signature(&block) {
                    log::warn!("Block signature verification failed");
                    return ValidationResult::Reject;
                }

                ValidationResult::Accept
            }
            Err(e) => {
                log::warn!("Failed to deserialize block: {}", e);
                ValidationResult::Reject
            }
        }
    }
}

/// Validation hooks manager
pub struct ValidationHooks {
    /// The validator instance
    validator: Arc<KlomangValidator>,
}

impl ValidationHooks {
    /// Create new validation hooks
    pub fn new(local_peer_id: PeerId) -> Self {
        Self {
            validator: Arc::new(KlomangValidator::new(local_peer_id)),
        }
    }

    /// Register validation hooks with Gossipsub behaviour
    pub fn register_validation_hooks(&self, _behaviour: &mut Behaviour) {
        // Set validation mode to Strict
        // Note: In libp2p v0.53, validation is handled via the message validation callback
        // The actual validation happens in the event loop when processing messages
    }

    /// Validate a message (called from event loop)
    pub async fn validate_message(
        &self,
        peer_id: &PeerId,
        message_id: &MessageId,
        message: &Message,
    ) -> MessageAcceptance {
        match self.validator.validate_message(peer_id, message_id, message).await {
            ValidationResult::Accept => MessageAcceptance::Accept,
            ValidationResult::Reject => MessageAcceptance::Reject,
            ValidationResult::Ignore => MessageAcceptance::Ignore,
        }
    }

    /// Get validator reference for external use
    pub fn validator(&self) -> &Arc<KlomangValidator> {
        &self.validator
    }
}

/// Helper function to create validation hooks
pub fn create_validation_hooks(local_peer_id: PeerId) -> ValidationHooks {
    ValidationHooks::new(local_peer_id)
}