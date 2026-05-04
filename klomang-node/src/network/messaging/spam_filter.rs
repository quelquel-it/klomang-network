//! Gossip Spam Protection System
//!
//! Implementasi comprehensive anti-spam protection untuk Gossipsub:
//! - Token Bucket Rate Limiting per-peer
//! - Message size filtering
//! - Duplicate detection dan early-drop
//! - Peer scoring dan graylisting
//! - Integration dengan klomang-core validation

use libp2p::gossipsub::MessageId;
use libp2p::PeerId;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use log::{warn, debug};

/// Konfigurasi untuk spam protection
#[derive(Clone, Debug)]
pub struct SpamProtectionConfig {
    /// Maximum messages per peer dalam satu window (default: 50)
    pub max_messages_per_window: usize,
    /// Time window untuk rate limiting (default: 1 second)
    pub rate_limit_window: Duration,
    /// Maximum message size untuk transaction (default: 1MB)
    pub max_transaction_size: usize,
    /// Maximum message size untuk block (default: 2MB)
    pub max_block_size: usize,
    /// Peer score threshold untuk graylisting (default: -500)
    pub greylist_threshold: f64,
    /// Duration untuk greylist peers (default: 5 minutes)
    pub greylist_duration: Duration,
    /// Initial peer score (default: 0)
    pub initial_peer_score: f64,
    /// Penalty untuk message rate limit violation
    pub rate_limit_violation_penalty: f64,
    /// Penalty untuk invalid message
    pub invalid_message_penalty: f64,
    /// Penalty untuk oversized message
    pub oversized_message_penalty: f64,
    /// Reward untuk valid message
    pub valid_message_reward: f64,
}

impl Default for SpamProtectionConfig {
    fn default() -> Self {
        Self {
            max_messages_per_window: 50,
            rate_limit_window: Duration::from_secs(1),
            max_transaction_size: 1024 * 1024, // 1MB
            max_block_size: 2 * 1024 * 1024, // 2MB
            greylist_threshold: -500.0,
            greylist_duration: Duration::from_secs(300), // 5 minutes
            initial_peer_score: 0.0,
            rate_limit_violation_penalty: -10.0,
            invalid_message_penalty: -20.0,
            oversized_message_penalty: -15.0,
            valid_message_reward: 0.5,
        }
    }
}

impl SpamProtectionConfig {
    /// Validate konfigurasi
    pub fn validate(&self) -> Result<(), String> {
        if self.max_messages_per_window == 0 {
            return Err("max_messages_per_window must be > 0".to_string());
        }
        if self.rate_limit_window.as_secs() == 0 {
            return Err("rate_limit_window must be > 0".to_string());
        }
        if self.max_transaction_size == 0 {
            return Err("max_transaction_size must be > 0".to_string());
        }
        if self.max_block_size == 0 {
            return Err("max_block_size must be > 0".to_string());
        }
        Ok(())
    }
}

/// Hasil dari spam filter check
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpamFilterResult {
    /// Message OK, propagate it
    Allow,
    /// Message rejected due to rate limit
    RateLimitExceeded,
    /// Message rejected due to size
    OversizedMessage,
    /// Message is a duplicate
    Duplicate,
    /// Peer is greylisted
    PeerGreylisted,
    /// Message rejected, peer score penalized
    InvalidMessage(String),
}

/// Token Bucket untuk rate limiting per peer
#[derive(Debug, Clone)]
struct TokenBucket {
    /// Current tokens available
    tokens: f64,
    /// Maximum tokens capacity
    capacity: f64,
    /// Last refill time
    last_refill: Instant,
    /// Refill rate (tokens per second)
    refill_rate: f64,
}

impl TokenBucket {
    /// Create new token bucket
    fn new(capacity: f64, refill_rate: f64) -> Self {
        Self {
            tokens: capacity,
            capacity,
            last_refill: Instant::now(),
            refill_rate,
        }
    }

    /// Refill tokens based on elapsed time
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + (elapsed * self.refill_rate)).min(self.capacity);
        self.last_refill = now;
    }

    /// Try to consume a token
    fn try_consume(&mut self, tokens: f64) -> bool {
        self.refill();
        if self.tokens >= tokens {
            self.tokens -= tokens;
            true
        } else {
            false
        }
    }
}

/// Peer scoring information
#[derive(Debug, Clone)]
pub(crate) struct PeerScore {
    /// Current peer score
    pub(crate) score: f64,
    /// Whether peer is greylisted
    pub(crate) is_greylisted: bool,
    /// When greylist expires
    pub(crate) greylist_expires: Option<Instant>,
}

impl PeerScore {
    fn new(config: &SpamProtectionConfig) -> Self {
        Self {
            score: config.initial_peer_score,
            is_greylisted: false,
            greylist_expires: None,
        }
    }

    /// Check if greylist has expired
    fn check_greylist_expiry(&mut self, _config: &SpamProtectionConfig) {
        if let Some(expiry) = self.greylist_expires {
            if Instant::now() >= expiry {
                self.is_greylisted = false;
                self.greylist_expires = None;
                debug!("Greylist expired for peer");
            }
        }
    }

    /// Apply greylist
    fn apply_greylist(&mut self, config: &SpamProtectionConfig) {
        self.is_greylisted = true;
        self.greylist_expires = Some(Instant::now() + config.greylist_duration);
    }

    /// Update score and check for greylist threshold
    pub(crate) fn update_score(&mut self, delta: f64, config: &SpamProtectionConfig) {
        self.score += delta;
        if self.score <= config.greylist_threshold && !self.is_greylisted {
            self.apply_greylist(config);
        }
    }
}

/// Core spam filter engine
pub struct SpamFilter {
    /// Configuration
    pub(crate) config: SpamProtectionConfig,
    /// Token buckets per peer for rate limiting
    token_buckets: Arc<DashMap<PeerId, TokenBucket>>,
    /// Peer scores dan status
    pub(crate) peer_scores: Arc<DashMap<PeerId, PeerScore>>,
    /// Recent message IDs untuk duplicate detection
    message_cache: Arc<DashMap<MessageId, Instant>>,
    /// Cache expiry duration
    cache_ttl: Duration,
}

impl SpamFilter {
    /// Create new spam filter
    pub fn new(config: SpamProtectionConfig) -> Result<Self, String> {
        config.validate()?;

        Ok(Self {
            config,
            token_buckets: Arc::new(DashMap::new()),
            peer_scores: Arc::new(DashMap::new()),
            message_cache: Arc::new(DashMap::new()),
            cache_ttl: Duration::from_secs(300), // 5 minute cache
        })
    }

    /// Create dengan default configuration
    pub fn default() -> Result<Self, String> {
        Self::new(SpamProtectionConfig::default())
    }

    /// Check message dari peer
    pub fn check_message(
        &self,
        peer_id: &PeerId,
        message_id: &MessageId,
        message_data: &[u8],
        message_type: MessageType,
    ) -> SpamFilterResult {
        // Check duplicate
        if self.is_duplicate(message_id) {
            return SpamFilterResult::Duplicate;
        }

        // Get or create peer score entry and work on it directly
        let mut peer_score = self
            .peer_scores
            .entry(*peer_id)
            .or_insert_with(|| PeerScore::new(&self.config));

        // Check greylist status
        peer_score.check_greylist_expiry(&self.config);
        if peer_score.is_greylisted {
            warn!(
                "Peer {} is currently greylisted, dropping incoming message",
                peer_id
            );
            return SpamFilterResult::PeerGreylisted;
        }

        // Check message size
        if !self.check_message_size(message_data, message_type) {
            warn!(
                "Peer {} sent oversized message: {} bytes",
                peer_id,
                message_data.len()
            );
            self.penalize_peer(peer_id, self.config.oversized_message_penalty);
            self.update_peer_score(peer_id);
            return SpamFilterResult::OversizedMessage;
        }

        // Check rate limit
        if !self.check_rate_limit(peer_id) {
            warn!(
                "Peer {} exceeded rate limit: {} messages per {:?}",
                peer_id,
                self.config.max_messages_per_window,
                self.config.rate_limit_window
            );
            self.penalize_peer(peer_id, self.config.rate_limit_violation_penalty);
            self.update_peer_score(peer_id);
            return SpamFilterResult::RateLimitExceeded;
        }

        // Message OK - add to cache dan reward peer
        self.add_to_cache(message_id);
        self.reward_peer(peer_id, self.config.valid_message_reward);
        self.update_peer_score(peer_id);

        SpamFilterResult::Allow
    }

    /// Check if message is duplicate
    fn is_duplicate(&self, message_id: &MessageId) -> bool {
        self.cleanup_cache();
        self.message_cache.contains_key(message_id)
    }

    /// Add message to cache
    fn add_to_cache(&self, message_id: &MessageId) {
        self.message_cache.insert(message_id.clone(), Instant::now());
    }

    /// Cleanup expired cache entries
    fn cleanup_cache(&self) {
        let now = Instant::now();
        self.message_cache.retain(|_, inserted_at| {
            now.duration_since(*inserted_at) < self.cache_ttl
        });
    }

    /// Check message size
    fn check_message_size(&self, data: &[u8], message_type: MessageType) -> bool {
        let max_size = match message_type {
            MessageType::Transaction => self.config.max_transaction_size,
            MessageType::Block => self.config.max_block_size,
        };

        data.len() <= max_size
    }

    /// Check rate limit untuk peer
    fn check_rate_limit(&self, peer_id: &PeerId) -> bool {
        let mut bucket = self
            .token_buckets
            .entry(*peer_id)
            .or_insert_with(|| {
                let capacity = self.config.max_messages_per_window as f64;
                let refill_rate = capacity / self.config.rate_limit_window.as_secs_f64();
                TokenBucket::new(capacity, refill_rate)
            });

        bucket.try_consume(1.0)
    }

    /// Penalite peer dengan score reduction
    fn penalize_peer(&self, peer_id: &PeerId, penalty: f64) {
        if let Some(mut peer) = self.peer_scores.get_mut(peer_id) {
            peer.update_score(penalty, &self.config);
        }
    }

    /// Reward peer dengan score increase
    fn reward_peer(&self, peer_id: &PeerId, reward: f64) {
        if let Some(mut peer) = self.peer_scores.get_mut(peer_id) {
            peer.score += reward;
        }
    }

    /// Update peer score dalam dashboard
    fn update_peer_score(&self, peer_id: &PeerId) {
        if let Some(peer) = self.peer_scores.get(peer_id) {
            if peer.score <= self.config.greylist_threshold && !peer.is_greylisted {
                debug!(
                    "Peer {} marked for greylist due to low score: {}",
                    peer_id, peer.score
                );
            }
        }
    }

    /// Get peer score
    pub fn get_peer_score(&self, peer_id: &PeerId) -> Option<f64> {
        self.peer_scores.get(peer_id).map(|p| p.score)
    }

    /// Get peer status
    pub fn get_peer_status(&self, peer_id: &PeerId) -> Option<PeerStatus> {
        self.peer_scores.get(peer_id).map(|p| PeerStatus {
            score: p.score,
            is_greylisted: p.is_greylisted,
            greylist_expires: p.greylist_expires,
        })
    }

    /// Get metrics
    pub fn get_metrics(&self) -> SpamFilterMetrics {
        SpamFilterMetrics {
            total_peers: self.peer_scores.len(),
            greylisted_peers: self.peer_scores.iter().filter(|p| p.value().is_greylisted).count(),
            cached_messages: self.message_cache.len(),
        }
    }

    /// Clear greylist untuk peer (administrative)
    pub fn clear_greylist(&self, peer_id: &PeerId) {
        if let Some(mut peer) = self.peer_scores.get_mut(peer_id) {
            peer.is_greylisted = false;
            peer.greylist_expires = None;
            warn!("Greylist cleared for peer {}", peer_id);
        }
    }

    /// Reset peer score (administrative)
    pub fn reset_peer_score(&self, peer_id: &PeerId) {
        if let Some(mut peer) = self.peer_scores.get_mut(peer_id) {
            peer.score = self.config.initial_peer_score;
            warn!("Peer score reset for {}", peer_id);
        }
    }
}

/// Message type untuk size validation
#[derive(Clone, Copy, Debug)]
pub enum MessageType {
    Transaction,
    Block,
}

/// Peer status snapshot
#[derive(Clone, Debug)]
pub struct PeerStatus {
    pub score: f64,
    pub is_greylisted: bool,
    pub greylist_expires: Option<Instant>,
}

/// Spam filter metrics
#[derive(Clone, Debug)]
pub struct SpamFilterMetrics {
    pub total_peers: usize,
    pub greylisted_peers: usize,
    pub cached_messages: usize,
}

impl std::fmt::Display for SpamFilterMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SpamFilter: {} total peers, {} greylisted, {} cached messages",
            self.total_peers, self.greylisted_peers, self.cached_messages
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_bucket_refill() {
        let mut bucket = TokenBucket::new(10.0, 10.0); // 10 tokens per second
        assert_eq!(bucket.tokens, 10.0);

        // Consume all tokens
        assert!(bucket.try_consume(10.0));
        assert_eq!(bucket.tokens, 0.0);

        // Try to consume more - should fail
        assert!(!bucket.try_consume(1.0));
    }

    #[test]
    fn test_spam_filter_creation() {
        let filter = SpamFilter::default().expect("Failed to create spam filter");
        let metrics = filter.get_metrics();
        assert_eq!(metrics.total_peers, 0);
        assert_eq!(metrics.greylisted_peers, 0);
    }

    #[test]
    fn test_duplicate_detection() {
        let filter = SpamFilter::default().expect("Failed to create spam filter");
        let msg_id = MessageId::from(vec![1, 2, 3]);

        assert_eq!(
            filter.check_message(
                &PeerId::random(),
                &msg_id,
                b"test data",
                MessageType::Transaction
            ),
            SpamFilterResult::Allow
        );

        assert_eq!(
            filter.check_message(
                &PeerId::random(),
                &msg_id,
                b"test data",
                MessageType::Transaction
            ),
            SpamFilterResult::Duplicate
        );
    }

    #[test]
    fn test_size_filtering() {
        let mut config = SpamProtectionConfig::default();
        config.max_transaction_size = 100;

        let filter = SpamFilter::new(config).expect("Failed to create spam filter");
        let peer_id = PeerId::random();
        let msg_id = MessageId::from(vec![1, 2, 3]);

        // Message too large
        let large_data = vec![0u8; 200];
        assert_eq!(
            filter.check_message(&peer_id, &msg_id, &large_data, MessageType::Transaction),
            SpamFilterResult::OversizedMessage
        );
    }
}
