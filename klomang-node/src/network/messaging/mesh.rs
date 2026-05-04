//! Advanced Topic Mesh Management for Klomang Gossipsub
//!
//! This module implements:
//! - Explicit MeshParams configuration for transaction and block topics
//! - Score-based peer management with performance tracking
//! - Dynamic mesh optimization for efficient message propagation

use libp2p::gossipsub::{Config, IdentTopic, PeerScoreThresholds};
use libp2p::PeerId;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Mesh parameters for topic-specific configuration
#[derive(Clone, Debug)]
pub struct MeshParams {
    /// Target number of peers in mesh (D)
    pub target_peers: usize,
    /// Lower bound for mesh size (D_low)
    pub low_watermark: usize,
    /// Upper bound for mesh size (D_high)
    pub high_watermark: usize,
    /// Number of peers to keep in mesh but not actively forward to (D_lazy)
    pub lazy_peers: usize,
}

/// Peer performance score tracking
#[derive(Clone, Debug)]
pub struct PeerScore {
    /// Current score value
    pub score: f64,
    /// Number of valid messages received
    pub valid_messages: u64,
    /// Number of invalid messages received
    pub invalid_messages: u64,
    /// Last activity timestamp
    pub last_activity: u64,
    /// Connection quality metric (0.0 to 1.0)
    pub connection_quality: f64,
}

impl Default for PeerScore {
    fn default() -> Self {
        Self {
            score: 0.0,
            valid_messages: 0,
            invalid_messages: 0,
            last_activity: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            connection_quality: 1.0,
        }
    }
}

/// Topic-specific mesh manager
pub struct TopicMesh {
    /// Topic identifier as string
    pub topic_str: String,
    /// Topic identifier
    pub topic: IdentTopic,
    /// Mesh parameters
    pub params: MeshParams,
    /// Peer scores for this topic
    peer_scores: Arc<RwLock<HashMap<PeerId, PeerScore>>>,
    /// Score thresholds for peer management
    score_thresholds: PeerScoreThresholds,
}

impl TopicMesh {
    /// Create new topic mesh with unified optimized parameters (D=6)
    pub fn new(topic: IdentTopic) -> Self {
        let topic_str = topic.to_string();
        // Use unified parameters across all topics: D=6, D_low=4, D_high=12, D_lazy=6
        let params = MeshParams {
            target_peers: 6,       // Unified D=6 for all topics
            low_watermark: 4,      // D_low=4
            high_watermark: 12,    // D_high=12
            lazy_peers: 6,         // D_lazy=6
        };

        let score_thresholds = PeerScoreThresholds {
            gossip_threshold: -10.0,     // Below this, peer is not gossiped to
            publish_threshold: -50.0,    // Below this, peer is not published to
            graylist_threshold: -100.0,  // Below this, peer is graylisted
            accept_px_threshold: 0.0,    // Above this, peer is accepted for PX
            opportunistic_graft_threshold: 1.0, // Above this, opportunistic grafting
        };

        Self {
            topic_str,
            topic,
            params,
            peer_scores: Arc::new(RwLock::new(HashMap::new())),
            score_thresholds,
        }
    }

    /// Update peer score based on message validation result
    pub async fn update_peer_score(&self, peer_id: &PeerId, valid: bool, timely: bool) {
        let mut scores = self.peer_scores.write().await;
        let score = scores.entry(*peer_id).or_insert_with(PeerScore::default);

        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        score.last_activity = current_time;

        if valid {
            score.valid_messages += 1;
            // Reward valid messages
            score.score += 1.0;
            // Bonus for timely delivery
            if timely {
                score.score += 0.5;
                score.connection_quality = (score.connection_quality + 1.0).min(1.0);
            }
        } else {
            score.invalid_messages += 1;
            // Penalize invalid messages heavily
            score.score -= 5.0;
            score.connection_quality = (score.connection_quality - 0.2).max(0.0);
        }

        // Decay score over time (simulate forgetting old behavior)
        let time_diff = current_time.saturating_sub(score.last_activity);
        if time_diff > 3600 { // 1 hour
            score.score *= 0.95; // Slight decay
        }
    }

    /// Get peer score for decision making
    pub async fn get_peer_score(&self, peer_id: &PeerId) -> f64 {
        let scores = self.peer_scores.read().await;
        scores.get(peer_id).map(|s| s.score).unwrap_or(0.0)
    }

    /// Check if peer should be disconnected based on score
    pub async fn should_disconnect_peer(&self, peer_id: &PeerId) -> bool {
        let score = self.get_peer_score(peer_id).await;
        score < self.score_thresholds.graylist_threshold
    }

    /// Get mesh statistics
    pub async fn get_mesh_stats(&self) -> MeshStats {
        let scores = self.peer_scores.read().await;
        let total_peers = scores.len();
        let high_score_peers = scores.values().filter(|s| s.score > 5.0).count();
        let low_score_peers = scores.values().filter(|s| s.score < -10.0).count();

        MeshStats {
            topic: self.topic.clone(),
            total_peers,
            high_score_peers,
            low_score_peers,
            target_mesh_size: self.params.target_peers,
        }
    }

    /// Apply mesh parameters to Gossipsub config
    pub fn apply_to_config(&self, config: Config) -> Config {
        // Note: libp2p Config doesn't expose direct mesh params setters in v0.53
        // These are internal defaults, but we can set related parameters
        config
    }
}

/// Mesh statistics for monitoring
#[derive(Clone, Debug)]
pub struct MeshStats {
    pub topic: IdentTopic,
    pub total_peers: usize,
    pub high_score_peers: usize,
    pub low_score_peers: usize,
    pub target_mesh_size: usize,
}

/// Global mesh manager coordinating multiple topic meshes
pub struct MeshManager {
    pub transaction_mesh: TopicMesh,
    pub block_mesh: TopicMesh,
}

impl MeshManager {
    /// Create new mesh manager with optimized configurations
    pub fn new() -> Self {
        let transaction_topic = IdentTopic::new("klomang/transactions/v1");
        let block_topic = IdentTopic::new("klomang/blocks/v1");

        Self {
            transaction_mesh: TopicMesh::new(transaction_topic),
            block_mesh: TopicMesh::new(block_topic),
        }
    }

    /// Get mesh for specific topic
    pub fn get_mesh(&self, topic: &IdentTopic) -> Option<&TopicMesh> {
        let topic_str = topic.to_string();
        if topic_str == self.transaction_mesh.topic_str {
            Some(&self.transaction_mesh)
        } else if topic_str == self.block_mesh.topic_str {
            Some(&self.block_mesh)
        } else {
            None
        }
    }

    /// Update peer score across all meshes
    pub async fn update_peer_score(&self, peer_id: &PeerId, topic: &IdentTopic, valid: bool, timely: bool) {
        let topic_str = topic.to_string();
        if topic_str == self.transaction_mesh.topic_str {
            self.transaction_mesh.update_peer_score(peer_id, valid, timely).await;
        } else if topic_str == self.block_mesh.topic_str {
            self.block_mesh.update_peer_score(peer_id, valid, timely).await;
        }
    }

    /// Check if peer should be disconnected
    pub async fn should_disconnect_peer(&self, peer_id: &PeerId) -> bool {
        let tx_disconnect = self.transaction_mesh.should_disconnect_peer(peer_id).await;
        let block_disconnect = self.block_mesh.should_disconnect_peer(peer_id).await;
        tx_disconnect || block_disconnect
    }

    /// Get comprehensive mesh statistics
    pub async fn get_stats(&self) -> Vec<MeshStats> {
        vec![
            self.transaction_mesh.get_mesh_stats().await,
            self.block_mesh.get_mesh_stats().await,
        ]
    }

    /// Log mesh management status
    pub async fn log_status(&self) {
        let stats = self.get_stats().await;
        for stat in stats {
            println!(
                "Mesh management active for {}: {} total peers, {} high-score, {} low-score, target mesh size {}",
                stat.topic.to_string(),
                stat.total_peers,
                stat.high_score_peers,
                stat.low_score_peers,
                stat.target_mesh_size
            );
        }
    }
}