//! Advanced Message Filtering untuk Gossipsub
//!
//! Mengintegrasikan:
//! - Duplicate detection early-drop
//! - Size filtering
//! - Spam filter checks
//! - Integration dengan GossipsubCache

use libp2p::gossipsub::{Message, MessageId};
use libp2p::PeerId;
use log::{debug, warn};
use std::sync::Arc;

use crate::network::messaging::cache::GossipsubCache;
use crate::network::messaging::spam_filter::{SpamFilter, SpamFilterResult, MessageType};

/// Result dari message filtering
#[derive(Clone, Debug, PartialEq)]
pub enum FilterResult {
    /// Message OK, propagate
    Accept,
    /// Early drop - duplicate
    DuplicateDrop,
    /// Early drop - oversized
    OversizedDrop,
    /// Early drop - rate limit exceeded
    RateLimitDrop,
    /// Early drop - peer greylisted
    GreylistDrop,
    /// Reject - invalid message
    Reject(String),
}

/// Advanced message filter dengan integrated duplicate dan size check
pub struct AdvancedMessageFilter {
    /// Spam protection engine
    spam_filter: Arc<SpamFilter>,
    /// Message cache untuk duplicate detection
    message_cache: Arc<parking_lot::Mutex<GossipsubCache>>,
}

impl AdvancedMessageFilter {
    /// Create new message filter
    pub fn new(
        spam_filter: Arc<SpamFilter>,
        message_cache: Arc<parking_lot::Mutex<GossipsubCache>>,
    ) -> Self {
        Self {
            spam_filter,
            message_cache,
        }
    }

    /// Filter incoming message dari peer
    pub fn filter_message(
        &self,
        peer_id: &PeerId,
        message: &Message,
    ) -> FilterResult {
        // Compute message_id dari data (SHA256 hash dari data)
        let message_id = MessageId::from(message.data.clone());
        let message_data = &message.data;

        // Step 1: Early duplicate check dari cache
        {
            let mut cache = self.message_cache.lock();
            if cache.contains(&message_id) {
                debug!(
                    "Duplicate message detected (early-drop): from {}",
                    peer_id
                );
                return FilterResult::DuplicateDrop;
            }
        }

        // Step 2: Determine message type untuk size check
        let message_type = match message.topic.as_str() {
            "klomang/transactions/v1" => MessageType::Transaction,
            "klomang/blocks/v1" => MessageType::Block,
            _ => {
                debug!("Unknown topic type: {}", message.topic);
                return FilterResult::Reject("Unknown topic".to_string());
            }
        };

        // Step 3: Full spam filter check (size, rate limit, greylist)
        let spam_check = self.spam_filter.check_message(
            peer_id,
            &message_id,
            message_data,
            message_type,
        );

        match spam_check {
            SpamFilterResult::Allow => {
                // Add to cache after all checks pass
                {
                    let mut cache = self.message_cache.lock();
                    cache.insert(message_id.clone());
                }
                debug!(
                    "Message accepted from {} (size: {} bytes)",
                    peer_id,
                    message_data.len()
                );
                FilterResult::Accept
            }
            SpamFilterResult::Duplicate => FilterResult::DuplicateDrop,
            SpamFilterResult::OversizedMessage => {
                warn!(
                    "Oversized message rejected from {}: {} bytes",
                    peer_id,
                    message_data.len()
                );
                FilterResult::OversizedDrop
            }
            SpamFilterResult::RateLimitExceeded => {
                warn!("Rate limit exceeded for peer {}", peer_id);
                FilterResult::RateLimitDrop
            }
            SpamFilterResult::PeerGreylisted => {
                debug!("Message from greylisted peer {}", peer_id);
                FilterResult::GreylistDrop
            }
            SpamFilterResult::InvalidMessage(reason) => {
                warn!("Invalid message from {}: {}", peer_id, reason);
                FilterResult::Reject(reason)
            }
        }
    }

    /// Get spam filter reference untuk administrative operations
    pub fn spam_filter(&self) -> &SpamFilter {
        &self.spam_filter
    }

    /// Get message cache reference
    pub fn message_cache(&self) -> Arc<parking_lot::Mutex<GossipsubCache>> {
        Arc::clone(&self.message_cache)
    }

    /// Get filtering statistics
    pub fn get_stats(&self) -> FilterStats {
        let spam_metrics = self.spam_filter.get_metrics();
        FilterStats {
            total_peers_tracked: spam_metrics.total_peers,
            greylisted_peers: spam_metrics.greylisted_peers,
            cached_messages: spam_metrics.cached_messages,
        }
    }
}

/// Filtering statistics
#[derive(Clone, Debug)]
pub struct FilterStats {
    pub total_peers_tracked: usize,
    pub greylisted_peers: usize,
    pub cached_messages: usize,
}

impl std::fmt::Display for FilterStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "FilterStats: {} peers tracked, {} greylisted, {} cached messages",
            self.total_peers_tracked, self.greylisted_peers, self.cached_messages
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_stats_display() {
        let stats = FilterStats {
            total_peers_tracked: 10,
            greylisted_peers: 2,
            cached_messages: 100,
        };
        let display_str = stats.to_string();
        assert!(display_str.contains("10 peers"));
        assert!(display_str.contains("2 greylisted"));
        assert!(display_str.contains("100 cached"));
    }
}
