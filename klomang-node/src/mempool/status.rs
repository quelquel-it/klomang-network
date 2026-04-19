//! Transaction lifecycle state machine
//!
//! Transaction state transitions:
//! Pending -> Validated -> [InBlock -> (removed)] or [OrphanPool -> Pending]
//! Anystate -> Rejected

use serde::{Deserialize, Serialize};

/// Transaction lifecycle status
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TransactionStatus {
    /// Transaction received but not yet validated
    Pending,
    
    /// Transaction inputs fully verified against UTXO storage
    Validated,
    
    /// Transaction inputs reference unknown UTXOs (waiting for dependencies)
    /// When dependencies arrive, transitions to Validated
    InOrphanPool,
    
    /// Transaction included in a block and committed to storage
    InBlock,
    
    /// Transaction failed validation and rejected permanently
    Rejected,
}

/// Errors during status transition
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransactionStatusError {
    /// Cannot transition from current state to target state
    InvalidTransition {
        current: TransactionStatus,
        target: TransactionStatus,
    },
    
    /// Operation not allowed in current state
    OperationNotAllowed {
        state: TransactionStatus,
        operation: &'static str,
    },
}

impl TransactionStatus {
    /// Check if this state allows the given transition
    pub fn can_transition_to(&self, target: TransactionStatus) -> bool {
        match (*self, target) {
            // From Pending, can go to Validated, InOrphanPool, or Rejected
            (TransactionStatus::Pending, TransactionStatus::Validated) => true,
            (TransactionStatus::Pending, TransactionStatus::InOrphanPool) => true,
            (TransactionStatus::Pending, TransactionStatus::Rejected) => true,
            
            // From InOrphanPool, can go to Validated or Rejected (when deps never arrive)
            (TransactionStatus::InOrphanPool, TransactionStatus::Validated) => true,
            (TransactionStatus::InOrphanPool, TransactionStatus::Rejected) => true,
            
            // From Validated, can go to InBlock or Rejected (if double-spent)
            (TransactionStatus::Validated, TransactionStatus::InBlock) => true,
            (TransactionStatus::Validated, TransactionStatus::Rejected) => true,
            
            // InBlock and Rejected are terminal states
            (TransactionStatus::InBlock, _) => false,
            (TransactionStatus::Rejected, _) => false,
            
            // Identity transition always allowed for polling/refresh
            (s1, s2) if s1 == s2 => true,
            
            // All other transitions are invalid
            _ => false,
        }
    }

    /// Attempt safe transition with validation
    pub fn transition_to(&mut self, target: TransactionStatus) -> Result<(), TransactionStatusError> {
        if !self.can_transition_to(target) {
            return Err(TransactionStatusError::InvalidTransition {
                current: *self,
                target,
            });
        }
        *self = target;
        Ok(())
    }

    /// Check if transaction is still in pool (not finalized)
    pub fn is_in_pool(&self) -> bool {
        matches!(
            self,
            TransactionStatus::Pending
                | TransactionStatus::Validated
                | TransactionStatus::InOrphanPool
        )
    }

    /// Check if transaction is ready for block inclusion
    pub fn is_ready_for_block(&self) -> bool {
        matches!(self, TransactionStatus::Validated)
    }

    /// Check if transaction has reached final state
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TransactionStatus::InBlock | TransactionStatus::Rejected
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        let mut status = TransactionStatus::Pending;
        assert!(status.transition_to(TransactionStatus::Validated).is_ok());
        assert_eq!(status, TransactionStatus::Validated);

        assert!(status.transition_to(TransactionStatus::InBlock).is_ok());
        assert_eq!(status, TransactionStatus::InBlock);
        assert!(status.is_terminal());
    }

    #[test]
    fn test_orphan_to_validated() {
        let mut status = TransactionStatus::InOrphanPool;
        assert!(status.transition_to(TransactionStatus::Validated).is_ok());
        assert_eq!(status, TransactionStatus::Validated);
    }

    #[test]
    fn test_invalid_transition() {
        let status = TransactionStatus::InBlock;
        assert!(!status.can_transition_to(TransactionStatus::Pending));
        assert!(!status.can_transition_to(TransactionStatus::Rejected));
    }

    #[test]
    fn test_identity_transition() {
        let mut status = TransactionStatus::Pending;
        assert!(status.transition_to(TransactionStatus::Pending).is_ok());
        assert_eq!(status, TransactionStatus::Pending);
    }
}
