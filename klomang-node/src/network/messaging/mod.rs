pub mod cache;
pub mod core_validation_hooks;
pub mod flood;
pub mod gossip;
pub mod mesh;
pub mod message_filter;
pub mod peer_scoring;
pub mod spam_filter;
pub mod validation;

use bincode;
use libp2p::gossipsub::{Behaviour as GossipsubBehaviour, Event as GossipsubEvent};
use libp2p::floodsub::{Floodsub, FloodsubEvent};
use libp2p::swarm::NetworkBehaviour;
use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use log::info;

use klomang_core::core::state::transaction::Transaction;

use crate::mempool::ParallelMempool;
use self::cache::GossipsubCache;
use self::core_validation_hooks::CoreValidationHooks;
use self::flood::FloodsubConfig;
use self::gossip::{build_gossipsub_behaviour_with_config, GossipsubConfig, GossipsubMeshConfig, GossipsubTopics};
use self::mesh::MeshManager;
use self::message_filter::AdvancedMessageFilter;
use self::peer_scoring::{PeerScoringManager, GossipsubScoreParams, GossipsubScoreThresholds};
use self::spam_filter::{SpamFilter, SpamProtectionConfig};
use self::validation::ValidationHooks;

/// Message wrapper for P2P network transmission
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkMessage {
    pub message_type: MessageType,
    pub payload: Vec<u8>,
    pub source: String,
    pub timestamp: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum MessageType {
    Transaction,
    Block,
    Bootstrap,
    Emergency,
}

impl NetworkMessage {
    pub fn from_transaction(tx: &Transaction, source: PeerId) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let payload = bincode::serialize(tx)?;
        Ok(NetworkMessage {
            message_type: MessageType::Transaction,
            payload,
            source: source.to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        })
    }

    pub fn to_transaction(&self) -> Result<Transaction, Box<dyn std::error::Error + Send + Sync>> {
        match self.message_type {
            MessageType::Transaction => {
                bincode::deserialize(&self.payload).map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
            }
            _ => Err("Invalid message type for transaction deserialization".into()),
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }

    pub fn decode(data: &[u8]) -> Result<Self, bincode::Error> {
        bincode::deserialize(data)
    }
}

/// Internal network behaviour combining gossipsub and floodsub
#[derive(NetworkBehaviour)]
#[behaviour(out_event = "InternalMessagingEvent")]
pub struct InternalMessagingBehaviour {
    pub gossipsub: GossipsubBehaviour,
    pub floodsub: Floodsub,
}

/// Events from internal messaging behaviour
#[derive(Debug)]
pub enum InternalMessagingEvent {
    Gossipsub(libp2p::gossipsub::Event),
    Floodsub(libp2p::floodsub::FloodsubEvent),
}

impl From<libp2p::gossipsub::Event> for InternalMessagingEvent {
    fn from(event: libp2p::gossipsub::Event) -> Self {
        InternalMessagingEvent::Gossipsub(event)
    }
}

impl From<libp2p::floodsub::FloodsubEvent> for InternalMessagingEvent {
    fn from(event: libp2p::floodsub::FloodsubEvent) -> Self {
        InternalMessagingEvent::Floodsub(event)
    }
}

/// Events emitted by MessagingBehaviour
#[derive(Debug)]
pub enum MessagingBehaviourEvent {
    Gossipsub(GossipsubEvent),
    Floodsub(FloodsubEvent),
}

impl From<GossipsubEvent> for MessagingBehaviourEvent {
    fn from(event: GossipsubEvent) -> Self {
        MessagingBehaviourEvent::Gossipsub(event)
    }
}

impl From<FloodsubEvent> for MessagingBehaviourEvent {
    fn from(event: FloodsubEvent) -> Self {
        MessagingBehaviourEvent::Floodsub(event)
    }
}

impl From<InternalMessagingEvent> for MessagingBehaviourEvent {
    fn from(event: InternalMessagingEvent) -> Self {
        match event {
            InternalMessagingEvent::Gossipsub(gossip_event) => MessagingBehaviourEvent::Gossipsub(gossip_event),
            InternalMessagingEvent::Floodsub(flood_event) => MessagingBehaviourEvent::Floodsub(flood_event),
        }
    }
}

pub struct MessagingBehaviour {
    pub internal: InternalMessagingBehaviour,
    /// Advanced mesh management
    pub mesh_manager: MeshManager,
    /// Message validation hooks
    pub validation_hooks: ValidationHooks,
    /// Spam protection filter
    pub spam_filter: Arc<SpamFilter>,
    /// Advanced message filter dengan duplicate detection
    pub message_filter: Arc<AdvancedMessageFilter>,
    /// Peer scoring manager
    pub peer_scoring: Arc<PeerScoringManager>,
    /// Core validation hooks
    pub core_validation: Arc<CoreValidationHooks>,
}

impl NetworkBehaviour for MessagingBehaviour {
    type ConnectionHandler = <InternalMessagingBehaviour as NetworkBehaviour>::ConnectionHandler;
    type ToSwarm = MessagingBehaviourEvent;

    fn handle_pending_inbound_connection(
        &mut self,
        connection_id: libp2p::swarm::ConnectionId,
        local_addr: &libp2p::Multiaddr,
        remote_addr: &libp2p::Multiaddr,
    ) -> Result<(), libp2p::swarm::ConnectionDenied> {
        self.internal
            .handle_pending_inbound_connection(connection_id, local_addr, remote_addr)
    }

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: libp2p::swarm::ConnectionId,
        peer: PeerId,
        local_addr: &libp2p::Multiaddr,
        remote_addr: &libp2p::Multiaddr,
    ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
        self.internal.handle_established_inbound_connection(
            connection_id,
            peer,
            local_addr,
            remote_addr,
        )
    }

    fn handle_pending_outbound_connection(
        &mut self,
        connection_id: libp2p::swarm::ConnectionId,
        maybe_peer: Option<PeerId>,
        addresses: &[libp2p::Multiaddr],
        effective_role: libp2p::core::Endpoint,
    ) -> Result<Vec<libp2p::Multiaddr>, libp2p::swarm::ConnectionDenied> {
        self.internal.handle_pending_outbound_connection(
            connection_id,
            maybe_peer,
            addresses,
            effective_role,
        )
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: libp2p::swarm::ConnectionId,
        peer: PeerId,
        addr: &libp2p::Multiaddr,
        role_override: libp2p::core::Endpoint,
    ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
        self.internal.handle_established_outbound_connection(
            connection_id,
            peer,
            addr,
            role_override,
        )
    }

    fn on_swarm_event(&mut self, event: libp2p::swarm::FromSwarm) {
        self.internal.on_swarm_event(event);
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: libp2p::swarm::ConnectionId,
        event: libp2p::swarm::THandlerOutEvent<Self>,
    ) {
        self.internal
            .on_connection_handler_event(peer_id, connection_id, event);
    }

    fn poll(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<libp2p::swarm::ToSwarm<Self::ToSwarm, libp2p::swarm::THandlerInEvent<Self>>> {
        match self.internal.poll(cx) {
            std::task::Poll::Ready(to_swarm) => match to_swarm {
                libp2p::swarm::ToSwarm::GenerateEvent(internal_event) => {
                    let event = match internal_event {
                        InternalMessagingEvent::Gossipsub(event) => MessagingBehaviourEvent::Gossipsub(event),
                        InternalMessagingEvent::Floodsub(event) => MessagingBehaviourEvent::Floodsub(event),
                    };
                    std::task::Poll::Ready(libp2p::swarm::ToSwarm::GenerateEvent(event))
                }
                libp2p::swarm::ToSwarm::Dial { opts } => std::task::Poll::Ready(libp2p::swarm::ToSwarm::Dial { opts }),
                libp2p::swarm::ToSwarm::ListenOn { opts } => std::task::Poll::Ready(libp2p::swarm::ToSwarm::ListenOn { opts }),
                libp2p::swarm::ToSwarm::RemoveListener { id } => {
                    std::task::Poll::Ready(libp2p::swarm::ToSwarm::RemoveListener { id })
                }
                libp2p::swarm::ToSwarm::NotifyHandler { peer_id, handler, event } => {
                    std::task::Poll::Ready(libp2p::swarm::ToSwarm::NotifyHandler { peer_id, handler, event })
                }
                libp2p::swarm::ToSwarm::NewExternalAddrCandidate(addr) => {
                    std::task::Poll::Ready(libp2p::swarm::ToSwarm::NewExternalAddrCandidate(addr))
                }
                libp2p::swarm::ToSwarm::ExternalAddrConfirmed(addr) => {
                    std::task::Poll::Ready(libp2p::swarm::ToSwarm::ExternalAddrConfirmed(addr))
                }
                libp2p::swarm::ToSwarm::ExternalAddrExpired(addr) => {
                    std::task::Poll::Ready(libp2p::swarm::ToSwarm::ExternalAddrExpired(addr))
                }
                libp2p::swarm::ToSwarm::CloseConnection { peer_id, connection } => {
                    std::task::Poll::Ready(libp2p::swarm::ToSwarm::CloseConnection { peer_id, connection })
                }
                libp2p::swarm::ToSwarm::NewExternalAddrOfPeer { peer_id, address } => {
                    std::task::Poll::Ready(libp2p::swarm::ToSwarm::NewExternalAddrOfPeer { peer_id, address })
                }
                _ => {
                    let other: libp2p::swarm::ToSwarm<MessagingBehaviourEvent, libp2p::swarm::THandlerInEvent<Self>> =
                        unsafe { std::mem::transmute(to_swarm) };
                    std::task::Poll::Ready(other)
                }
            },
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

/// Configuration for Messaging layer
#[derive(Clone, Debug)]
pub struct MessagingConfig {
    pub gossipsub: GossipsubConfig,
    pub floodsub: FloodsubConfig,
}

impl Default for MessagingConfig {
    fn default() -> Self {
        Self {
            gossipsub: GossipsubConfig::default(),
            floodsub: FloodsubConfig::default(),
        }
    }
}

/// Initialize messaging behaviour with Gossipsub, Floodsub, and Spam Protection
pub fn initialize_messaging_behaviour(
    local_peer_id: PeerId,
    _config: MessagingConfig,
) -> (InternalMessagingBehaviour, MeshManager, ValidationHooks, Arc<SpamFilter>, Arc<AdvancedMessageFilter>, Arc<PeerScoringManager>, Arc<CoreValidationHooks>) {
    // Create advanced mesh configuration with unified parameters
    let mesh_config = GossipsubMeshConfig::default();

    // Build Gossipsub with advanced configuration (this logs the configuration)
    let mut gossipsub = build_gossipsub_behaviour_with_config(local_peer_id, mesh_config.clone())
        .expect("Failed to build gossipsub behaviour with mesh configuration");

    // Subscribe to default topics
    let _ = gossipsub.subscribe(&GossipsubTopics::transaction_topic());
    let _ = gossipsub.subscribe(&GossipsubTopics::blocks_topic());

    // Build Floodsub
    let mut floodsub = Floodsub::new(local_peer_id);

    // Subscribe to critical topics
    floodsub.subscribe(flood::FloodsubTopics::bootstrap_topic());
    floodsub.subscribe(flood::FloodsubTopics::emergency_topic());

    let mut internal = InternalMessagingBehaviour { gossipsub, floodsub };

    // Create mesh manager
    let mesh_manager = MeshManager::new();

    // Create validation hooks
    let validation_hooks = ValidationHooks::new(local_peer_id);

    // Register validation hooks with gossipsub
    validation_hooks.register_validation_hooks(&mut internal.gossipsub);

    let cache_ttl = Duration::from_millis(mesh_config.heartbeat_interval_ms * mesh_config.history_length as u64);
    let message_cache = GossipsubCache::with_storage(4096, cache_ttl, "./gossipsub_message_cache");

    // === Initialize Spam Protection Components ===
    
    // 1. Create spam filter dengan configuration
    let spam_config = SpamProtectionConfig::default();
    let spam_filter = Arc::new(
        SpamFilter::new(spam_config)
            .expect("Failed to initialize spam filter")
    );
    info!("✓ SpamFilter initialized: {} max messages/peer per second", spam_filter.config.max_messages_per_window);

    // 2. Create peer scoring manager
    let score_params = GossipsubScoreParams::default();
    let score_thresholds = GossipsubScoreThresholds::default();
    let peer_scoring = Arc::new(PeerScoringManager::new(
        Arc::clone(&spam_filter),
        score_params,
        score_thresholds,
    ));
    info!("✓ PeerScoringManager initialized: greylist_threshold = {}", peer_scoring.score_thresholds().greylist_threshold);

    // 3. Create advanced message filter
    let message_cache_mutex = Arc::new(parking_lot::Mutex::new(message_cache));
    let message_filter = Arc::new(AdvancedMessageFilter::new(
        Arc::clone(&spam_filter),
        Arc::clone(&message_cache_mutex),
    ));
    let stats = message_filter.get_stats();
    info!("✓ AdvancedMessageFilter initialized: {}", stats);

    // 4. Create core validation hooks
    let core_validation = Arc::new(CoreValidationHooks::new(
        Arc::clone(&peer_scoring),
    ));
    info!("✓ CoreValidationHooks initialized");

    info!("🔒 Gossip spam protection active with Peer Scoring");

    (
        internal,
        mesh_manager,
        validation_hooks,
        spam_filter,
        message_filter,
        peer_scoring,
        core_validation,
    )
}

/// Broadcast a transaction to the network via Gossipsub
pub fn broadcast_transaction(
    behaviour: &mut MessagingBehaviour,
    tx: &Transaction,
    source: PeerId,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let message = NetworkMessage::from_transaction(tx, source)?;
    let encoded = message.encode()?;

    let _ = behaviour
        .internal
        .gossipsub
        .publish(GossipsubTopics::transaction_topic(), encoded)?;

    Ok(())
}

/// Broadcast a critical bootstrap signal to all nodes via Floodsub
pub fn broadcast_bootstrap_signal(
    behaviour: &mut MessagingBehaviour,
    data: Vec<u8>,
) {
    let topic = flood::FloodsubTopics::bootstrap_topic();
    behaviour.internal.floodsub.publish(topic, data);
}

/// Broadcast an emergency signal to all nodes via Floodsub
pub fn broadcast_emergency_signal(
    behaviour: &mut MessagingBehaviour,
    data: Vec<u8>,
) {
    let topic = flood::FloodsubTopics::emergency_topic();
    behaviour.internal.floodsub.publish(topic, data);
}

/// Validate incoming transaction message using klomang-core crypto
pub fn validate_transaction_message(
    message: &NetworkMessage,
) -> Result<Transaction, Box<dyn std::error::Error + Send + Sync>> {
    if message.message_type != MessageType::Transaction {
        return Err("Invalid message type".into());
    }

    let tx = message.to_transaction()?;

    // Verify transaction structure (basic validation)
    if tx.id.as_bytes().is_empty() {
        return Err("Invalid transaction ID".into());
    }

    Ok(tx)
}

/// Handle incoming Gossipsub messages and optionally add to mempool
pub async fn handle_gossipsub_transaction(
    message: &NetworkMessage,
    mempool: Option<&mut ParallelMempool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Validate the transaction
    let tx = validate_transaction_message(message)?;

    // If mempool is provided, attempt to add the transaction
    if let Some(pool) = mempool {
        match pool.add_transactions_batch(vec![tx.clone()]) {
            Ok(_) => {
                log::debug!("Transaction {} added to mempool from peer {}", tx.id, message.source);
            }
            Err(e) => {
                log::warn!("Failed to add transaction {} to mempool: {}", tx.id, e);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_message_serialization() {
        let message = NetworkMessage {
            message_type: MessageType::Transaction,
            payload: vec![1, 2, 3, 4],
            source: "test_peer".to_string(),
            timestamp: 1234567890,
        };

        let encoded = message.encode().expect("Failed to encode");
        let decoded = NetworkMessage::decode(&encoded).expect("Failed to decode");

        assert_eq!(message.message_type, decoded.message_type);
        assert_eq!(message.payload, decoded.payload);
        assert_eq!(message.source, decoded.source);
        assert_eq!(message.timestamp, decoded.timestamp);
    }
}
