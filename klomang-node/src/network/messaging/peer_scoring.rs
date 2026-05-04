//! Peer Scoring System Integration dengan Klomang-Core Validation
//!
//! Implementasi:
//! - GossipsubScoreParams dan GossipsubScoreThresholds
//! - Otomatis penalti berdasarkan validation dari klomang-core
//! - Graylisting dengan intelligent recovery
//! - Scoring hooks untuk di-apply setelah validation

use libp2p::PeerId;
use log::{debug, warn, info};
use std::sync::Arc;
use dashmap::DashMap;
use std::time::Instant;

use crate::network::messaging::spam_filter::SpamFilter;

/// Gossipsub score parameters (inspired dari libp2p-gossipsub score calculation)
#[derive(Clone, Debug)]
pub struct GossipsubScoreParams {
    /// Topic weight
    pub topic_weight: f64,
    /// Time in the mesh decay
    pub time_in_mesh_decay: f64,
    /// First message deliveries decay  
    pub first_message_deliveries_decay: f64,
    /// Mesh message deliveries decay
    pub mesh_message_deliveries_decay: f64,
    /// Invalid message deliveries weight
    pub invalid_message_deliveries_weight: f64,
    /// IP colocation factor weight
    pub ip_colocation_factor_weight: f64,
}

impl Default for GossipsubScoreParams {
    fn default() -> Self {
        Self {
            topic_weight: 0.5,
            time_in_mesh_decay: 0.95, // Decay per heartbeat
            first_message_deliveries_decay: 0.97,
            mesh_message_deliveries_decay: 0.93,
            invalid_message_deliveries_weight: -10.0,
            ip_colocation_factor_weight: -5.0,
        }
    }
}

/// Gossipsub score thresholds untuk actions
#[derive(Clone, Debug)]
pub struct GossipsubScoreThresholds {
    /// Gossip threshold - kurangi pesan gossip untuk peer di bawah ini
    pub gossip_threshold: f64,
    /// Publish threshold - tolak publish dari peer di bawah ini
    pub publish_threshold: f64,
    /// Greylist threshold - masukkan peer ke greylist di bawah ini
    pub greylist_threshold: f64,
    /// Accept PX threshold
    pub accept_px_threshold: f64,
    /// Opportunistic graft threshold
    pub opportunistic_graft_threshold: f64,
}

impl Default for GossipsubScoreThresholds {
    fn default() -> Self {
        Self {
            gossip_threshold: -500.0,
            publish_threshold: -1000.0,
            greylist_threshold: -500.0,
            accept_px_threshold: 1000.0,
            opportunistic_graft_threshold: 500.0,
        }
    }
}

/// Validation result yang akan di-apply ke scoring
#[derive(Clone, Debug)]
pub enum ValidationOutcome {
    /// Transaction/block valid - reward peer
    Valid,
    /// Invalid signature
    InvalidSignature(f64), // penalty amount
    /// Invalid transaction structure
    InvalidStructure(f64),
    /// Oversized message
    OversizedMessage(f64),
    /// Double-spend attempt
    DoubleSpend(f64),
    /// Invalid fee
    InvalidFee(f64),
    /// Other error
    OtherError(f64),
}

impl ValidationOutcome {
    /// Get penalty amount (negative for penalties, positive for rewards)
    pub fn penalty(&self) -> f64 {
        match self {
            ValidationOutcome::Valid => 0.5, // Small reward untuk valid
            ValidationOutcome::InvalidSignature(p) => -*p,
            ValidationOutcome::InvalidStructure(p) => -*p,
            ValidationOutcome::OversizedMessage(p) => -*p,
            ValidationOutcome::DoubleSpend(p) => -*p,
            ValidationOutcome::InvalidFee(p) => -*p,
            ValidationOutcome::OtherError(p) => -*p,
        }
    }

    /// Log message untuk debug
    pub fn log_message(&self) -> String {
        match self {
            ValidationOutcome::Valid => "Transaction/block validation passed".to_string(),
            ValidationOutcome::InvalidSignature(_) => {
                "Invalid signature detected in transaction".to_string()
            }
            ValidationOutcome::InvalidStructure(_) => "Invalid message structure detected".to_string(),
            ValidationOutcome::OversizedMessage(_) => "Oversized message detected".to_string(),
            ValidationOutcome::DoubleSpend(_) => "Double-spend attempt detected".to_string(),
            ValidationOutcome::InvalidFee(_) => "Invalid transaction fee detected".to_string(),
            ValidationOutcome::OtherError(_) => "Validation error occurred".to_string(),
        }
    }
}

/// Peer scoring manager dengan graylisting
pub struct PeerScoringManager {
    /// Spam filter untuk score updates
    spam_filter: Arc<SpamFilter>,
    /// Score parameters
    score_params: GossipsubScoreParams,
    /// Score thresholds
    score_thresholds: GossipsubScoreThresholds,
    /// Peer validation history untuk scoring
    peer_validation_history: Arc<DashMap<PeerId, PeerValidationRecord>>,
}

#[derive(Clone, Debug)]
struct PeerValidationRecord {
    /// Number of valid messages
    valid_count: u32,
    /// Number of invalid messages
    invalid_count: u32,
    /// Last validation time
    last_validation: Instant,
}

impl PeerScoringManager {
    /// Create new peer scoring manager
    pub fn new(
        spam_filter: Arc<SpamFilter>,
        score_params: GossipsubScoreParams,
        score_thresholds: GossipsubScoreThresholds,
    ) -> Self {
        Self {
            spam_filter,
            score_params,
            score_thresholds,
            peer_validation_history: Arc::new(DashMap::new()),
        }
    }

    /// Create dengan default parameters
    pub fn default_with_spam_filter(spam_filter: Arc<SpamFilter>) -> Self {
        Self::new(
            spam_filter,
            GossipsubScoreParams::default(),
            GossipsubScoreThresholds::default(),
        )
    }

    /// Apply validation outcome ke peer score
    pub fn apply_validation_outcome(
        &self,
        peer_id: &PeerId,
        outcome: &ValidationOutcome,
    ) {
        let penalty = outcome.penalty();
        let log_msg = outcome.log_message();

        // Update spam filter score
        if penalty < 0.0 {
            warn!(
                "Spam detected from {}: {} (penalty: {})",
                peer_id, log_msg, penalty
            );
            // Apply penalty trough spam filter
            if let Some(mut score_entry) = self.spam_filter.peer_scores.get_mut(peer_id) {
                score_entry.update_score(penalty, &self.spam_filter.config);
            }
        } else if penalty > 0.0 {
            debug!(
                "Valid message from {}: {} (reward: +{})",
                peer_id, log_msg, penalty
            );
            if let Some(mut score_entry) = self.spam_filter.peer_scores.get_mut(peer_id) {
                score_entry.score += penalty;
            }
        }

        // Update validation history
        self.update_validation_record(peer_id, matches!(outcome, ValidationOutcome::Valid));

        // Check if should be greylisted
        if let Some(score) = self.spam_filter.get_peer_score(peer_id) {
            if score <= self.score_thresholds.greylist_threshold {
                warn!(
                    "Peer {} greylisted due to spam (score: {})",
                    peer_id, score
                );
            }
        }
    }

    /// Update validation record untuk peer
    fn update_validation_record(&self, peer_id: &PeerId, is_valid: bool) {
        let mut record = self
            .peer_validation_history
            .entry(*peer_id)
            .or_insert_with(|| PeerValidationRecord {
                valid_count: 0,
                invalid_count: 0,
                last_validation: Instant::now(),
            });

        if is_valid {
            record.valid_count += 1;
        } else {
            record.invalid_count += 1;
        }
        record.last_validation = Instant::now();
    }

    /// Check if peer should be greylisted based on validation history
    pub fn should_greylist(&self, peer_id: &PeerId) -> bool {
        if let Some(current_score) = self.spam_filter.get_peer_score(peer_id) {
            return current_score <= self.score_thresholds.greylist_threshold;
        }
        false
    }

    /// Clear greylist untuk peer (after reputation recovery)
    pub fn clear_greylist_if_recovered(&self, peer_id: &PeerId) -> bool {
        if let Some(status) = self.spam_filter.get_peer_status(peer_id) {
            // Only clear if score has improved significantly
            if status.score > self.score_thresholds.greylist_threshold + 100.0 {
                self.spam_filter.clear_greylist(peer_id);
                info!("Greylist cleared for peer {} (score recovered to {})", peer_id, status.score);
                return true;
            }
        }
        false
    }

    /// Get peer score
    pub fn get_peer_score(&self, peer_id: &PeerId) -> Option<f64> {
        self.spam_filter.get_peer_score(peer_id)
    }

    /// Get peer status
    pub fn get_peer_status(&self, peer_id: &PeerId) -> Option<String> {
        if let Some(status) = self.spam_filter.get_peer_status(peer_id) {
            if let Some(record) = self.peer_validation_history.get(peer_id) {
                let total = record.valid_count + record.invalid_count;
                let validity_rate = if total > 0 {
                    (record.valid_count as f64 / total as f64) * 100.0
                } else {
                    0.0
                };

                let greylist_status = if status.is_greylisted {
                    "GREYLISTED"
                } else {
                    "ACTIVE"
                };

                return Some(format!(
                    "Peer {}: score={:.2}, status={}, valid_msgs={}, invalid_msgs={}, validity_rate={:.1}%",
                    peer_id, status.score, greylist_status, record.valid_count, record.invalid_count, validity_rate
                ));
            }
        }
        None
    }

    /// Get scoring configuration untuk reference
    pub fn score_params(&self) -> &GossipsubScoreParams {
        &self.score_params
    }

    /// Get scoring thresholds
    pub fn score_thresholds(&self) -> &GossipsubScoreThresholds {
        &self.score_thresholds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validation_outcome_penalty() {
        assert_eq!(ValidationOutcome::Valid.penalty(), 0.5);
        assert_eq!(ValidationOutcome::InvalidSignature(10.0).penalty(), -10.0);
    }

    #[test]
    fn test_score_thresholds_default() {
        let thresholds = GossipsubScoreThresholds::default();
        assert!(thresholds.gossip_threshold < 0.0);
        assert!(thresholds.greylist_threshold < 0.0);
    }
}
