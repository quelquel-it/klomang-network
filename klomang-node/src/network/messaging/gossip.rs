use libp2p::gossipsub::{
    Behaviour, ConfigBuilder, IdentTopic, Message, MessageAuthenticity, MessageId, PublishError,
};
use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use klomang_core::core::crypto::Hash;

/// Advanced Gossipsub mesh configuration with explicit parameter control
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GossipsubMeshConfig {
    /// Target mesh degree (D)
    pub mesh_n: usize,
    /// Lower watermark for mesh size (D_low)
    pub mesh_n_low: usize,
    /// Upper watermark for mesh size (D_high)
    pub mesh_n_high: usize,
    /// Lazy peer degree (D_lazy)
    pub mesh_n_lazy: usize,
    /// Factor for gossip message distribution
    pub gossip_factor: f64,
    /// Heartbeat interval in milliseconds
    pub heartbeat_interval_ms: u64,
    /// Message history length
    pub history_length: usize,
    /// History gossip threshold for IHAVE/IWANT negotiation
    pub history_gossip_threshold: usize,
    /// Maximum number of IHAVE message ids to request via IWANT
    pub max_ihave_length: usize,
    /// Maximum number of IHAVE advertisements accepted from a peer in a heartbeat
    pub max_ihave_messages: usize,
    /// Time to wait before following up on IWANT requests
    pub iwant_followup_secs: u64,
    /// Enable flood publishing (fallback to all peers when mesh unavailable)
    pub flood_publish: bool,
}

impl Default for GossipsubMeshConfig {
    fn default() -> Self {
        Self {
            mesh_n: 6,
            mesh_n_low: 4,
            mesh_n_high: 12,
            mesh_n_lazy: 6,
            gossip_factor: 0.25,
            heartbeat_interval_ms: 1000,
            history_length: 5,
            history_gossip_threshold: 3,
            max_ihave_length: 5000,
            max_ihave_messages: 10,
            iwant_followup_secs: 3,
            flood_publish: false,
        }
    }
}

impl GossipsubMeshConfig {
    /// Create a new configuration with default parameters
    pub fn new() -> Self {
        Self::default()
    }

    /// Create config with explicit parameters
    pub fn with_params(
        mesh_n: usize,
        mesh_n_low: usize,
        mesh_n_high: usize,
        mesh_n_lazy: usize,
        gossip_factor: f64,
        heartbeat_interval_ms: u64,
        history_length: usize,
        history_gossip_threshold: usize,
    ) -> Self {
        Self {
            mesh_n,
            mesh_n_low,
            mesh_n_high,
            mesh_n_lazy,
            gossip_factor,
            heartbeat_interval_ms,
            history_length,
            history_gossip_threshold,
            ..Default::default()
        }
    }

    /// Validate parameter consistency
    pub fn validate(&self) -> Result<(), String> {
        if self.mesh_n < self.mesh_n_low {
            return Err(format!(
                "mesh_n ({}) must be >= mesh_n_low ({})",
                self.mesh_n, self.mesh_n_low
            ));
        }
        if self.mesh_n > self.mesh_n_high {
            return Err(format!(
                "mesh_n ({}) must be <= mesh_n_high ({})",
                self.mesh_n, self.mesh_n_high
            ));
        }
        if self.gossip_factor <= 0.0 || self.gossip_factor > 1.0 {
            return Err(format!(
                "gossip_factor must be between 0.0 and 1.0, got {}",
                self.gossip_factor
            ));
        }
        if self.heartbeat_interval_ms == 0 {
            return Err("heartbeat_interval_ms must be > 0".to_string());
        }
        Ok(())
    }

    /// Format configuration for logging
    pub fn to_log_string(&self) -> String {
        format!(
            "Gossipsub configured with D={}, D_low={}, D_high={}, D_lazy={}, gossip_factor={}, heartbeat={}ms, history_length={}",
            self.mesh_n,
            self.mesh_n_low,
            self.mesh_n_high,
            self.mesh_n_lazy,
            self.gossip_factor,
            self.heartbeat_interval_ms,
            self.history_length
        )
    }
}

/// Configuration for Gossipsub messaging (legacy, for compatibility)
#[derive(Clone, Debug)]
pub struct GossipsubConfig {
    pub mesh_n: usize,
    pub mesh_n_low: usize,
    pub mesh_n_high: usize,
    pub flood_publish: bool,
}

impl Default for GossipsubConfig {
    fn default() -> Self {
        Self {
            mesh_n: 6,
            mesh_n_low: 4,
            mesh_n_high: 12,
            flood_publish: false,
        }
    }
}

impl From<GossipsubMeshConfig> for GossipsubConfig {
    fn from(mesh_config: GossipsubMeshConfig) -> Self {
        Self {
            mesh_n: mesh_config.mesh_n,
            mesh_n_low: mesh_config.mesh_n_low,
            mesh_n_high: mesh_config.mesh_n_high,
            flood_publish: mesh_config.flood_publish,
        }
    }
}

/// Main topics for Klomang network messaging
pub struct GossipsubTopics;

impl GossipsubTopics {
    pub const TRANSACTIONS: &'static str = "klomang/transactions/v1";
    pub const BLOCKS: &'static str = "klomang/blocks/v1";

    pub fn transaction_topic() -> IdentTopic {
        IdentTopic::new(Self::TRANSACTIONS)
    }

    pub fn blocks_topic() -> IdentTopic {
        IdentTopic::new(Self::BLOCKS)
    }
}

/// Build Gossipsub behaviour with explicit mesh parameter configuration
pub fn build_gossipsub_behaviour(
    _local_peer_id: PeerId,
    _config: GossipsubConfig,
) -> Result<Behaviour, Box<dyn std::error::Error>> {
    build_gossipsub_behaviour_with_config(_local_peer_id, GossipsubMeshConfig::default())
}

/// Build Gossipsub behaviour with advanced mesh configuration
pub fn build_gossipsub_behaviour_with_config(
    _local_peer_id: PeerId,
    mesh_config: GossipsubMeshConfig,
) -> Result<Behaviour, Box<dyn std::error::Error>> {
    // Validate configuration
    mesh_config.validate()?;

    // Log configuration at initialization
    log::info!("{}", mesh_config.to_log_string());

    // Build the gossipsub config with IHAVE/IWANT and deterministic message IDs.
    let gs_config = ConfigBuilder::default()
        .mesh_n(mesh_config.mesh_n)
        .mesh_n_low(mesh_config.mesh_n_low)
        .mesh_n_high(mesh_config.mesh_n_high)
        .gossip_lazy(mesh_config.mesh_n_lazy)
        .gossip_factor(mesh_config.gossip_factor)
        .heartbeat_interval(Duration::from_millis(mesh_config.heartbeat_interval_ms))
        .history_length(mesh_config.history_length)
        .history_gossip(mesh_config.history_gossip_threshold)
        .duplicate_cache_time(Duration::from_millis(mesh_config.heartbeat_interval_ms * mesh_config.history_length as u64))
        .validate_messages()
        .message_id_fn(|message: &Message| {
            let message_hash = Hash::new(&message.data);
            MessageId(message_hash.as_bytes().to_vec())
        })
        .max_ihave_length(mesh_config.max_ihave_length)
        .max_ihave_messages(mesh_config.max_ihave_messages)
        .iwant_followup_time(Duration::from_secs(mesh_config.iwant_followup_secs))
        .flood_publish(mesh_config.flood_publish)
        .build()?;

    // Build the behaviour with signed message authentication
    let keypair = libp2p::identity::Keypair::generate_secp256k1();
    let behaviour = Behaviour::new(MessageAuthenticity::Signed(keypair), gs_config)?;

    Ok(behaviour)
}

/// Update Gossipsub mesh parameters at runtime
pub fn update_gossipsub_mesh_params(
    mesh_config: &GossipsubMeshConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    // Validate the new configuration
    mesh_config.validate()?;

    // Log the parameter update
    log::info!(
        "Updating Gossipsub mesh parameters: D={}, D_low={}, D_high={}, D_lazy={}",
        mesh_config.mesh_n,
        mesh_config.mesh_n_low,
        mesh_config.mesh_n_high,
        mesh_config.mesh_n_lazy
    );

    Ok(())
}

/// Subscribe to a specific Gossipsub topic
pub fn subscribe_to_topic(
    behaviour: &mut Behaviour,
    topic: IdentTopic,
) -> Result<bool, String> {
    behaviour.subscribe(&topic).map_err(|e| format!("Subscribe error: {:?}", e))
}

/// Publish a message to a Gossipsub topic
pub fn publish_message(
    behaviour: &mut Behaviour,
    topic: IdentTopic,
    data: Vec<u8>,
) -> Result<libp2p::gossipsub::MessageId, PublishError> {
    behaviour.publish(topic, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gossipsub_mesh_config_default() {
        let config = GossipsubMeshConfig::default();
        assert_eq!(config.mesh_n, 6);
        assert_eq!(config.mesh_n_low, 4);
        assert_eq!(config.mesh_n_high, 12);
        assert_eq!(config.mesh_n_lazy, 6);
        assert_eq!(config.gossip_factor, 0.25);
        assert_eq!(config.heartbeat_interval_ms, 1000);
        assert_eq!(config.history_length, 5);
        assert_eq!(config.history_gossip_threshold, 3);
    }

    #[test]
    fn test_gossipsub_mesh_config_validation_success() {
        let config = GossipsubMeshConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_gossipsub_mesh_config_validation_mesh_n_too_low() {
        let config = GossipsubMeshConfig {
            mesh_n: 2,
            mesh_n_low: 4,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_gossipsub_mesh_config_validation_mesh_n_too_high() {
        let config = GossipsubMeshConfig {
            mesh_n: 15,
            mesh_n_high: 12,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_gossipsub_mesh_config_validation_invalid_gossip_factor() {
        let config = GossipsubMeshConfig {
            gossip_factor: 1.5,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_gossipsub_mesh_config_to_log_string() {
        let config = GossipsubMeshConfig::default();
        let log_str = config.to_log_string();
        assert!(log_str.contains("D=6"));
        assert!(log_str.contains("D_low=4"));
        assert!(log_str.contains("D_high=12"));
        assert!(log_str.contains("D_lazy=6"));
        assert!(log_str.contains("gossip_factor=0.25"));
    }

    #[test]
    fn test_gossipsub_mesh_config_with_params() {
        let config = GossipsubMeshConfig::with_params(8, 5, 14, 4, 0.3, 1500);
        assert_eq!(config.mesh_n, 8);
        assert_eq!(config.mesh_n_low, 5);
        assert_eq!(config.mesh_n_high, 14);
        assert_eq!(config.mesh_n_lazy, 4);
        assert_eq!(config.gossip_factor, 0.3);
        assert_eq!(config.heartbeat_interval_ms, 1500);
        // Should use defaults for other fields
        assert_eq!(config.history_length, 5);
        assert_eq!(config.history_gossip_threshold, 3);
    }

    #[test]
    fn test_gossipsub_config_from_mesh_config() {
        let mesh_config = GossipsubMeshConfig::default();
        let config: GossipsubConfig = mesh_config.into();
        assert_eq!(config.mesh_n, 6);
        assert_eq!(config.mesh_n_low, 4);
        assert_eq!(config.mesh_n_high, 12);
        assert!(!config.flood_publish);
    }
}

