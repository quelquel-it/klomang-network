pub mod address;
pub mod discovery;
pub mod multiplexer;
pub mod peer_id;
pub mod protocols;
pub mod routing;
pub mod secure_channel;
pub mod transport;

pub use address::{
    build_multiaddr, parse_multiaddr, validate_multiaddr, CoreNetworkAddress, MultiaddrError,
};
pub use discovery::DiscoveryEngine;
pub use multiplexer::{CoreMultiplexer, MultiplexerConfig, StreamType};
pub use peer_id::{peer_id_from_core_public_key, IdentityError, IdentityKeyType, NodeIdentity};
pub use protocols::{PeerInfo, ProtocolHandler, ProtocolStats};
pub use routing::{PeerRecord, PeerRoutingTable};
pub use secure_channel::{build_secure_channel, SecureChannel, SecureChannelError};
pub use transport::{CoreNetworkTransport, KlomangTransport, TransportConfig, TransportError};

use std::collections::HashSet;
use std::future;
use std::path::PathBuf;
use std::pin::Pin;

use libp2p::futures::StreamExt;
use libp2p::noise::Config as NoiseConfig;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::tls::Config as TlsConfig;
use libp2p::yamux::Config as YamuxConfig;
use libp2p::{Multiaddr, PeerId, SwarmBuilder};
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};

use crate::network::connectivity::{ConnectivityBehaviour, ConnectivityBehaviourEvent, ConnectivityConfig, ConnectivityState};
use crate::network::messaging::{InternalMessagingBehaviour, InternalMessagingEvent, MessagingConfig};
use crate::network::peer_discovery::{DiscoveryConfig, PeerDiscoveryManager};
use crate::storage::cf::ColumnFamilyName;
use crate::storage::db::StorageDb;

#[derive(NetworkBehaviour)]
#[behaviour(out_event = "NodeBehaviourEvent")]
pub struct NodeBehaviour {
    discovery: crate::network::peer_discovery::DiscoveryBehaviour,
    connectivity: ConnectivityBehaviour,
    messaging: InternalMessagingBehaviour,
}

#[derive(Debug)]
pub enum NodeBehaviourEvent {
    Discovery(crate::network::peer_discovery::DiscoveryBehaviourEvent),
    Connectivity(crate::network::connectivity::ConnectivityBehaviourEvent),
    Messaging(InternalMessagingEvent),
}
impl From<crate::network::peer_discovery::DiscoveryBehaviourEvent> for NodeBehaviourEvent {
    fn from(event: crate::network::peer_discovery::DiscoveryBehaviourEvent) -> Self {
        NodeBehaviourEvent::Discovery(event)
    }
}

impl From<crate::network::connectivity::ConnectivityBehaviourEvent> for NodeBehaviourEvent {
    fn from(event: crate::network::connectivity::ConnectivityBehaviourEvent) -> Self {
        NodeBehaviourEvent::Connectivity(event)
    }
}

impl From<InternalMessagingEvent> for NodeBehaviourEvent {
    fn from(event: InternalMessagingEvent) -> Self {
        NodeBehaviourEvent::Messaging(event)
    }
}

/// Errors for network stack initialization.
#[derive(Debug)]
pub enum NetworkInitializationError {
    Identity(IdentityError),
    SecureChannel(SecureChannelError),
    Discovery(String),
    Swarm(String),
}

impl std::fmt::Display for NetworkInitializationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkInitializationError::Identity(err) => {
                write!(f, "Identity initialization failed: {}", err)
            }
            NetworkInitializationError::SecureChannel(err) => {
                write!(f, "Secure channel initialization failed: {}", err)
            }
            NetworkInitializationError::Discovery(err) => {
                write!(f, "Peer discovery initialization failed: {}", err)
            }
            NetworkInitializationError::Swarm(err) => {
                write!(f, "Swarm initialization failed: {}", err)
            }
        }
    }
}

impl std::error::Error for NetworkInitializationError {}

impl From<IdentityError> for NetworkInitializationError {
    fn from(err: IdentityError) -> Self {
        NetworkInitializationError::Identity(err)
    }
}

impl From<SecureChannelError> for NetworkInitializationError {
    fn from(err: SecureChannelError) -> Self {
        NetworkInitializationError::SecureChannel(err)
    }
}

/// Validate if a peer is legitimate (not a spammer) using klomang-core crypto.
/// This is a basic validation - in production, this could involve checking
/// against a whitelist or validating cryptographic proofs.
pub fn validate_peer_legitimacy(peer_id: &PeerId, peer_info: &PeerInfo) -> bool {
    // Basic checks: peer must support required protocols
    if !peer_info.supports_required_protocols() {
        return false;
    }

    // Check if peer ID can be derived from a valid public key
    // This is a basic sanity check - in production, you might want
    // more sophisticated validation like checking against known validators
    match peer_id_from_core_public_key(&peer_id.to_bytes()) {
        Ok(derived_peer_id) => derived_peer_id == *peer_id,
        Err(_) => false,
    }
}

/// Initialize the Klomang network stack and return the local PeerId along with a handle
/// for the background swarm task. This initializes identity, secure channels, and the
/// peer discovery swarm based on Kademlia and mDNS.
pub fn initialize_network_stack() -> Result<(PeerId, JoinHandle<()>), NetworkInitializationError> {
    let identity_path = PathBuf::from("node_identity.key");
    let identity = NodeIdentity::load_or_generate(&identity_path, IdentityKeyType::Secp256k1)?;
    let _secure_channel = build_secure_channel(&identity)?;

    let mut discovery_manager =
        PeerDiscoveryManager::new(identity.keypair().clone(), DiscoveryConfig::default())
            .map_err(|err| NetworkInitializationError::Discovery(err.to_string()))?;

    discovery_manager
        .bootstrap()
        .map_err(|err| NetworkInitializationError::Discovery(err.to_string()))?;

    let (discovery_behaviour, discovery_state) = discovery_manager.into_parts();
    let connectivity_config = ConnectivityConfig::default();
    let mut connectivity_state = ConnectivityState::new(connectivity_config.relay.clone());
    
    let messaging_config = MessagingConfig::default();
    let local_peer_id_clone = identity.peer_id().clone();
    let connectivity_config_clone = connectivity_config.clone();
    let (messaging, mesh_manager, validation_hooks, _spam_filter, message_filter, _peer_scoring, core_validation) =
        crate::network::messaging::initialize_messaging_behaviour(
            local_peer_id_clone.clone(),
            messaging_config.clone(),
        );

    let mut swarm = SwarmBuilder::with_existing_identity(identity.keypair().clone())
        .with_tokio()
        .with_tcp(
            Default::default(),
            (TlsConfig::new, NoiseConfig::new),
            YamuxConfig::default,
        )
        .map_err(|err| NetworkInitializationError::Swarm(err.to_string()))?
        .with_relay_client(TlsConfig::new, YamuxConfig::default)
        .map_err(|err| NetworkInitializationError::Swarm(err.to_string()))?
        .with_behaviour(move |_, relay| {
            let connectivity = ConnectivityBehaviour::new(
                local_peer_id_clone.clone(),
                relay,
                connectivity_config_clone.clone(),
            );
            NodeBehaviour {
                discovery: discovery_behaviour,
                connectivity,
                messaging,
            }
        })
        .map_err(|err| NetworkInitializationError::Swarm(err.to_string()))?
        .build();

    let listen_addr: Multiaddr = "/ip4/0.0.0.0/tcp/0".parse().expect("valid listen address");
    swarm
        .listen_on(listen_addr)
        .map_err(|err| NetworkInitializationError::Swarm(err.to_string()))?;

    let discovery_db = StorageDb::new("./peer_discovery_store")
        .map_err(|err| NetworkInitializationError::Discovery(err.to_string()))?;

    let local_peer_id = identity.peer_id().clone();
    println!("Local PeerID: {}", local_peer_id);

    let rendezvous_interval_duration = std::time::Duration::from_millis(
        discovery_state
            .rendezvous_control
            .config
            .discovery_interval_ms,
    );
    let random_walk_interval_duration = discovery_state.random_walk.config.interval;

    let handle = tokio::spawn(async move {
        let discovery_db = discovery_db;
        let mut discovery_state = discovery_state;
        let mut swarm = swarm;
        let mesh_manager = mesh_manager;
        let validation_hooks = validation_hooks;
        mesh_manager.log_status().await;
        let mut random_walk_interval = interval(random_walk_interval_duration);
        random_walk_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut rendezvous_interval = interval(rendezvous_interval_duration);
        rendezvous_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
let mut bootstrap_retry_sleep: Option<Pin<Box<tokio::time::Sleep>>> = None;
        let mut random_walk_queries: HashSet<libp2p::kad::QueryId> = HashSet::new();

        if let Err(err) = discovery_state
            .rendezvous_control
            .register_all(&mut swarm.behaviour_mut().discovery.rendezvous)
        {
            log::warn!("Initial rendezvous registration failed: {}", err);
        }

        loop {
            tokio::select! {
                event = swarm.select_next_some() => {
                    match event {
                        SwarmEvent::NewListenAddr { address, .. } => {
                            log::info!("Listening on {}", address);
                        }
                        SwarmEvent::Behaviour(behaviour_event) => match behaviour_event {
                            NodeBehaviourEvent::Discovery(discovery_event) => match discovery_event {
                                crate::network::peer_discovery::DiscoveryBehaviourEvent::Kademlia(kad_event) => {
                                    match kad_event {
                                        libp2p::kad::Event::OutboundQueryProgressed { id, result, .. } => {
                                            match result {
                                                libp2p::kad::QueryResult::Bootstrap(Ok(_)) => {
                                                    let now = std::time::SystemTime::now()
                                                        .duration_since(std::time::UNIX_EPOCH)
                                                        .unwrap_or_default()
                                                        .as_secs();
                                                    let _ = discovery_db.put(
                                                        ColumnFamilyName::Default,
                                                        b"discovery:last_bootstrap",
                                                        format!("success:{}", now).as_bytes(),
                                                    );
                                                    log::info!("Kademlia bootstrap succeeded");
                                                    bootstrap_retry_sleep = None;
                                                    discovery_state.bootstrap_manager.reset_backoff();
                                                }
                                                libp2p::kad::QueryResult::Bootstrap(Err(err)) => {
                                                    let now = std::time::SystemTime::now()
                                                        .duration_since(std::time::UNIX_EPOCH)
                                                        .unwrap_or_default()
                                                        .as_secs();
                                                    let _ = discovery_db.put(
                                                        ColumnFamilyName::Default,
                                                        b"discovery:last_bootstrap",
                                                        format!("failed:{}:{:?}", now, err).as_bytes(),
                                                    );
                                                    let backoff = discovery_state.bootstrap_manager.next_backoff();
                                                    log::warn!("Bootstrap failed, retrying in {:?}: {:?}", backoff, err);
                                                    bootstrap_retry_sleep = Some(Box::pin(tokio::time::sleep(backoff)));
                                                }
                                                libp2p::kad::QueryResult::GetClosestPeers(Ok(closest)) => {
                                                    if random_walk_queries.remove(&id) {
                                                        for peer in &closest.peers {
                                                            let key = format!("discovery:random_walk:{}", peer);
                                                            let now = std::time::SystemTime::now()
                                                                .duration_since(std::time::UNIX_EPOCH)
                                                                .unwrap_or_default()
                                                                .as_secs();
                                                            let _ = discovery_db.put(
                                                                ColumnFamilyName::Default,
                                                                key.as_bytes(),
                                                                now.to_string().as_bytes(),
                                                            );
                                                        }
                                                        log::info!("Random walk discovered {} peers", closest.peers.len());
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                        libp2p::kad::Event::RoutingUpdated { peer, is_new_peer, .. } => {
                                            if is_new_peer {
                                                let key = format!("discovery:routing:{}", peer);
                                                let now = std::time::SystemTime::now()
                                                    .duration_since(std::time::UNIX_EPOCH)
                                                    .unwrap_or_default()
                                                    .as_secs();
                                                let _ = discovery_db.put(
                                                    ColumnFamilyName::Default,
                                                    key.as_bytes(),
                                                    now.to_string().as_bytes(),
                                                );
                                                log::debug!("Routing table updated for peer {}", peer);
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                crate::network::peer_discovery::DiscoveryBehaviourEvent::Mdns(mdns_event) => {
                                    log::debug!("mDNS event: {:?}", mdns_event);
                                }
                                crate::network::peer_discovery::DiscoveryBehaviourEvent::Identify(identify_event) => {
                                    match identify_event {
                                        libp2p::identify::Event::Received { peer_id, info } => {
                                            if info.protocol_version.is_empty() {
                                                log::warn!("Identify information missing protocol version from {}", peer_id);
                                            }

                                            let peer_info = PeerInfo::from_identify(peer_id, &info);
                                            if validate_peer_legitimacy(&peer_id, &peer_info) {
                                                let key = format!("discovery:identify:{}", peer_id);
                                                let now = std::time::SystemTime::now()
                                                    .duration_since(std::time::UNIX_EPOCH)
                                                    .unwrap_or_default()
                                                    .as_secs();
                                                let _ = discovery_db.put(
                                                    ColumnFamilyName::Default,
                                                    key.as_bytes(),
                                                    now.to_string().as_bytes(),
                                                );
                                                log::debug!("Peer {} passed identity validation", peer_id);
                                                connectivity_state.register_relay_candidate(
                                                    peer_id,
                                                    &peer_info.listen_addresses,
                                                );
                                            } else {
                                                log::warn!("Peer {} failed identity verification", peer_id);
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                crate::network::peer_discovery::DiscoveryBehaviourEvent::Rendezvous(rendezvous_event) => {
                                    let peers = discovery_state
                                        .rendezvous_control
                                        .handle_event(rendezvous_event);
                                    for (peer_id, addrs) in peers {
                                        for addr in addrs {
                                            swarm.behaviour_mut().discovery.kademlia.add_address(&peer_id, addr.clone());
                                            let key = format!("discovery:rendezvous:{}", peer_id);
                                            let now = std::time::SystemTime::now()
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .unwrap_or_default()
                                                .as_secs();
                                            let _ = discovery_db.put(
                                                ColumnFamilyName::Default,
                                                key.as_bytes(),
                                                now.to_string().as_bytes(),
                                            );
                                        }
                                    }
                                }
                            }
                            NodeBehaviourEvent::Connectivity(connectivity_event) => {
                                match connectivity_event {
                                    ConnectivityBehaviourEvent::AutoNat(event) => {
                                        if let libp2p::autonat::Event::StatusChanged { new, .. } = event {
                                            log::info!("Network status updated: {:?}", new);

                                            if let libp2p::autonat::NatStatus::Public(addr) = new.clone() {
                                                swarm.add_external_address(addr);
                                            }

                                            if matches!(new, libp2p::autonat::NatStatus::Private) {
                                                let reservation_addrs = connectivity_state.reserve_relay_slots();
                                                for addr in reservation_addrs {
                                                    if let Err(err) = swarm.listen_on(addr.clone()) {
                                                        log::warn!(
                                                            "Relay reservation listen failed on {}: {}",
                                                            addr,
                                                            err
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    ConnectivityBehaviourEvent::DcUtR(event) => {
                                        let libp2p::dcutr::Event { remote_peer_id, result } = event;
                                        if result.is_ok() {
                                            let is_valid = match peer_id_from_core_public_key(&remote_peer_id.to_bytes()) {
                                                Ok(derived) => derived == remote_peer_id,
                                                Err(_) => false,
                                            };

                                            if is_valid {
                                                log::info!(
                                                    "Connection upgraded to Direct via DCUtR with {}",
                                                    remote_peer_id
                                                );
                                            } else {
                                                log::warn!(
                                                    "Direct upgrade succeeded for invalid peer {}",
                                                    remote_peer_id
                                                );
                                            }
                                        }
                                    }
                                    ConnectivityBehaviourEvent::Relay(event) => {
                                        log::debug!("Relay client event: {:?}", event);
                                    }
                                }
                            }
                            NodeBehaviourEvent::Messaging(messaging_event) => {
                                use crate::network::messaging::InternalMessagingEvent;
                                use crate::network::messaging::message_filter::FilterResult;
                                use libp2p::gossipsub::{Event as GossipsubEvent, MessageAcceptance};
                                use libp2p::floodsub::FloodsubEvent;
                                match messaging_event {
                                    InternalMessagingEvent::Gossipsub(GossipsubEvent::Message {
                                        propagation_source,
                                        message_id,
                                        message
                                    }) => {
                                        // Early filtering (duplicate/size/rate-limit/greylist)
                                        let filter_result = message_filter.filter_message(
                                            &propagation_source,
                                            &message,
                                        );

                                        let acceptance = match filter_result {
                                            FilterResult::Accept => {
                                                // Check Klomang-core spam potential before full validation
                                                match core_validation
                                                    .verify_spam_potential(&propagation_source, &message)
                                                    .await
                                                {
                                                    crate::network::messaging::core_validation_hooks::ValidationHookResult::Valid => {
                                                        validation_hooks
                                                            .validate_message(
                                                                &propagation_source,
                                                                &message_id,
                                                                &message,
                                                            )
                                                            .await
                                                    }
                                                    crate::network::messaging::core_validation_hooks::ValidationHookResult::Invalid(_) => {
                                                        log::warn!(
                                                            "Message {} from {} failed core spam validation",
                                                            message_id,
                                                            propagation_source
                                                        );
                                                        MessageAcceptance::Reject
                                                    }
                                                }
                                            }
                                            FilterResult::DuplicateDrop => {
                                                log::debug!(
                                                    "Duplicate message early-drop {} from {}",
                                                    message_id,
                                                    propagation_source
                                                );
                                                MessageAcceptance::Ignore
                                            }
                                            FilterResult::OversizedDrop => {
                                                log::warn!(
                                                    "Oversized message early-drop {} from {}",
                                                    message_id,
                                                    propagation_source
                                                );
                                                MessageAcceptance::Reject
                                            }
                                            FilterResult::RateLimitDrop => {
                                                log::warn!(
                                                    "Rate limit drop for peer {} on message {}",
                                                    propagation_source,
                                                    message_id
                                                );
                                                MessageAcceptance::Ignore
                                            }
                                            FilterResult::GreylistDrop => {
                                                log::warn!(
                                                    "Greylisted peer {} sent message {}",
                                                    propagation_source,
                                                    message_id
                                                );
                                                MessageAcceptance::Ignore
                                            }
                                            FilterResult::Reject(reason) => {
                                                log::warn!(
                                                    "Rejected message {} from {} due to filter failure: {}",
                                                    message_id,
                                                    propagation_source,
                                                    reason
                                                );
                                                MessageAcceptance::Reject
                                            }
                                        };

                                        match &acceptance {
                                            MessageAcceptance::Accept => {
                                                log::debug!("Accepted gossipsub message {} from {}", message_id, propagation_source);
                                                let topic = libp2p::gossipsub::IdentTopic::new(message.topic.as_str());
                                                mesh_manager
                                                    .update_peer_score(&propagation_source, &topic, true, true)
                                                    .await;

                                                if message.topic.as_str() == "klomang/transactions/v1" {
                                                    if let Ok(network_msg) = crate::network::messaging::NetworkMessage::decode(&message.data) {
                                                        if let Ok(_tx) = network_msg.to_transaction() {
                                                            let _ = mesh_manager.transaction_mesh.get_mesh_stats().await;
                                                        }
                                                    }
                                                } else if message.topic.as_str() == "klomang/blocks/v1" {
                                                    let _ = mesh_manager.block_mesh.get_mesh_stats().await;
                                                }
                                            }
                                            MessageAcceptance::Reject => {
                                                log::warn!("Rejected gossipsub message {} from {}", message_id, propagation_source);

                                                let topic = libp2p::gossipsub::IdentTopic::new(message.topic.as_str());
                                                mesh_manager
                                                    .update_peer_score(&propagation_source, &topic, false, false)
                                                    .await;

                                                if mesh_manager.should_disconnect_peer(&propagation_source).await {
                                                    log::warn!("Disconnecting peer {} due to low score", propagation_source);
                                                    let _ = swarm.disconnect_peer_id(propagation_source);
                                                }
                                            }
                                            MessageAcceptance::Ignore => {
                                                log::debug!("Ignored gossipsub message {} from {}", message_id, propagation_source);
                                            }
                                        }

                                        if let Err(err) = swarm.behaviour_mut().messaging.gossipsub.report_message_validation_result(
                                            &message_id,
                                            &propagation_source,
                                            acceptance,
                                        ) {
                                            log::warn!(
                                                "Failed to report gossipsub validation result for message {}: {:?}",
                                                message_id,
                                                err
                                            );
                                        }
                                    }
                                    InternalMessagingEvent::Floodsub(FloodsubEvent::Message(message)) => {
                                        log::info!("Received floodsub message from {}", message.source);
                                        // Emergency/Bootstrap signal handler
                                    }
                                    _ => {
                                        log::debug!("Other messaging event: {:?}", messaging_event);
                                    }
                                }
                            }
                        },
                        _ => {}
                    }
                }
                _ = random_walk_interval.tick() => {
                    if discovery_state.random_walk.should_walk() {
                        let target = discovery_state.random_walk.next_target();
                        let query_id = swarm.behaviour_mut().discovery.kademlia.get_closest_peers(target);
                        random_walk_queries.insert(query_id);
                    }
                }
                _ = rendezvous_interval.tick() => {
                    discovery_state
                        .rendezvous_control
                        .discover_via_rendezvous(&mut swarm.behaviour_mut().discovery.rendezvous);
                    if let Err(err) = discovery_state
                        .rendezvous_control
                        .register_all(&mut swarm.behaviour_mut().discovery.rendezvous)
                    {
                        log::warn!("Rendezvous registration error: {}", err);
                    }
                }
                _ = async {
                    if let Some(sleep) = bootstrap_retry_sleep.as_mut() {
                        sleep.as_mut().await;
                    } else {
                        future::pending::<()>().await;
                    }
                } => {
                    if bootstrap_retry_sleep.is_some() {
                        discovery_state.bootstrap_manager.seed_kademlia(|peer_id, addr| {
                            swarm.behaviour_mut().discovery.kademlia.add_address(&peer_id, addr.clone());
                        });
                        match swarm.behaviour_mut().discovery.kademlia.bootstrap() {
                            Ok(_) => {
                                log::info!("Bootstrap retry succeeded");
                                bootstrap_retry_sleep = None;
                                discovery_state.bootstrap_manager.reset_backoff();
                            }
                            Err(err) => {
                                let backoff = discovery_state.bootstrap_manager.next_backoff();
                                log::warn!("Bootstrap retry failed, next retry in {:?}: {}", backoff, err);
                                bootstrap_retry_sleep = Some(Box::pin(tokio::time::sleep(backoff)));
                            }
                        }
                    }
                }
            }
        }
    });

    Ok((local_peer_id, handle))
}
