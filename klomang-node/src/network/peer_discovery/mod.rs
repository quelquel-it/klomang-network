pub mod bootstrap;
pub mod kademlia;
pub mod mdns;
pub mod random_walk;
pub mod rendezvous;

use std::time::Duration;

use libp2p::identify::{Behaviour as IdentifyBehaviour, Config as IdentifyConfig, Event as IdentifyEvent};
use libp2p::kad::store::MemoryStore;
use libp2p::kad::Behaviour as KadBehaviour;
use libp2p::kad::Event as KadEvent;
use libp2p::mdns::tokio::Behaviour as MdnsBehaviour;
use libp2p::mdns::Event as MdnsEvent;
use libp2p::swarm::NetworkBehaviour;
use libp2p::{Multiaddr, PeerId};
use libp2p::rendezvous::client::{Behaviour as RendezvousBehaviour, Event as RendezvousEvent};
use self::bootstrap::BootstrapConfig;

pub use bootstrap::{BootstrapConfig as DiscoveryBootstrapConfig, BootstrapManager};
pub use kademlia::{DiscoveryMsg as KadDiscoveryMsg, KademliaConfig, KademliaDiscovery};
pub use mdns::{DiscoveryMsg as MdnsDiscoveryMsg, MdnsConfig, MdnsDiscovery};
pub use random_walk::{RandomWalkConfig, RandomWalkControl};
pub use rendezvous::{RendezvousConfig, RendezvousControl};

/// Combined discovery behaviour that exposes Kademlia, mDNS, Identify and Rendezvous.
#[derive(NetworkBehaviour)]
#[behaviour(out_event = "DiscoveryBehaviourEvent")]
pub struct DiscoveryBehaviour {
    pub kademlia: KadBehaviour<MemoryStore>,
    pub mdns: MdnsBehaviour,
    pub identify: IdentifyBehaviour,
    pub rendezvous: RendezvousBehaviour,
}

/// The mutable discovery state that is not part of the NetworkBehaviour.
pub struct DiscoveryState {
    pub bootstrap_manager: BootstrapManager,
    pub random_walk: RandomWalkControl,
    pub rendezvous_control: RendezvousControl,
    pub discovered_peers: Vec<PeerId>,
}

impl DiscoveryState {
    pub fn new(
        bootstrap_manager: BootstrapManager,
        random_walk: RandomWalkControl,
        rendezvous_control: RendezvousControl,
    ) -> Self {
        Self {
            bootstrap_manager,
            random_walk,
            rendezvous_control,
            discovered_peers: Vec::new(),
        }
    }
}

/// Events emitted by the combined discovery behaviour.
#[derive(Debug)]
pub enum DiscoveryBehaviourEvent {
    Kademlia(KadEvent),
    Mdns(MdnsEvent),
    Identify(IdentifyEvent),
    Rendezvous(RendezvousEvent),
}

impl From<KadEvent> for DiscoveryBehaviourEvent {
    fn from(event: KadEvent) -> Self {
        DiscoveryBehaviourEvent::Kademlia(event)
    }
}

impl From<MdnsEvent> for DiscoveryBehaviourEvent {
    fn from(event: MdnsEvent) -> Self {
        DiscoveryBehaviourEvent::Mdns(event)
    }
}

impl From<IdentifyEvent> for DiscoveryBehaviourEvent {
    fn from(event: IdentifyEvent) -> Self {
        DiscoveryBehaviourEvent::Identify(event)
    }
}

impl From<RendezvousEvent> for DiscoveryBehaviourEvent {
    fn from(event: RendezvousEvent) -> Self {
        DiscoveryBehaviourEvent::Rendezvous(event)
    }
}

/// Configuration for combined peer discovery.
#[derive(Clone, Debug)]
pub struct DiscoveryConfig {
    pub bootstrap: BootstrapConfig,
    pub kademlia: KademliaConfig,
    pub mdns: MdnsConfig,
    pub rendezvous: RendezvousConfig,
    pub random_walk: RandomWalkConfig,
}

impl DiscoveryConfig {
    /// Load peer discovery configuration from environment variables.
    ///
    /// Supported environment variables:
    /// - `KL_MANG_BOOTSTRAP_NODES`: comma-separated multiaddrs with `/p2p/<PeerId>`.
    /// - `KL_MANG_BOOTSTRAP_NODES_FILE`: path to a newline-separated bootstrap list.
    /// - `KL_MANG_RENDEZVOUS_NODES`: comma-separated multiaddrs with `/p2p/<PeerId>`.
    /// - `KL_MANG_RANDOM_WALK_INTERVAL_SECS`: interval between random walk queries.
    pub fn from_env() -> Self {
        let mut config = DiscoveryConfig::default();
        config.bootstrap = BootstrapConfig::from_env();
        config.random_walk = RandomWalkConfig::from_env();

        if let Ok(rendezvous_nodes) = std::env::var("KL_MANG_RENDEZVOUS_NODES") {
            config.rendezvous.rendezvous_nodes = rendezvous_nodes
                .split(',')
                .filter_map(|entry| {
                    let entry = entry.trim();
                    if entry.is_empty() {
                        return None;
                    }
                    match entry.parse::<Multiaddr>() {
                        Ok(multiaddr) => multiaddr.iter().find_map(|protocol| {
                            if let libp2p::multiaddr::Protocol::P2p(peer_id) = protocol {
                                Some((peer_id, multiaddr.clone()))
                            } else {
                                None
                            }
                        }),
                        Err(err) => {
                            log::warn!("Invalid rendezvous address {}: {}", entry, err);
                            None
                        }
                    }
                })
                .collect();
        }

        config.kademlia.bootstrap_nodes = config.bootstrap.bootstrap_nodes.clone();
        config
    }
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            bootstrap: BootstrapConfig::default(),
            kademlia: KademliaConfig::default(),
            mdns: MdnsConfig::default(),
            rendezvous: RendezvousConfig::default(),
            random_walk: RandomWalkConfig::default(),
        }
    }
}

/// Peer discovery manager that combines Kademlia, mDNS, Identify and Rendezvous.
pub struct PeerDiscoveryManager {
    kademlia: KademliaDiscovery,
    mdns: MdnsDiscovery,
    identify: IdentifyBehaviour,
    rendezvous: RendezvousBehaviour,
    bootstrap_manager: BootstrapManager,
    random_walk: RandomWalkControl,
    rendezvous_control: RendezvousControl,
}

impl PeerDiscoveryManager {
    /// Create a new peer discovery manager.
    pub fn new(
        local_keypair: libp2p::identity::Keypair,
        config: DiscoveryConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let local_peer_id = PeerId::from_public_key(&local_keypair.public());

        let kademlia = KademliaDiscovery::new(local_peer_id.clone(), config.kademlia.clone());
        let mdns = MdnsDiscovery::new(local_peer_id.clone(), config.mdns.clone())?;

        let identify_config = IdentifyConfig::new(
            "klomang/1.0.0".to_string(),
            local_keypair.public().clone(),
        )
        .with_push_listen_addr_updates(true)
        .with_interval(Duration::from_secs(30));

        let identify = IdentifyBehaviour::new(identify_config);
        let rendezvous = RendezvousBehaviour::new(local_keypair.clone());

        Ok(Self {
            kademlia,
            mdns,
            identify,
            rendezvous,
            bootstrap_manager: BootstrapManager::new(config.bootstrap.clone()),
            random_walk: RandomWalkControl::new(config.random_walk.clone()),
            rendezvous_control: RendezvousControl::new(config.rendezvous.clone()),
        })
    }

    /// Initialize bootstrap process for Kademlia.
    pub fn bootstrap(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.bootstrap_manager
            .seed_kademlia(|peer_id, addr| self.kademlia.add_peer(peer_id, addr));
        self.kademlia.bootstrap()?;
        self.bootstrap_manager.reset_backoff();
        log::info!("Peer discovery initialized with Kademlia and mDNS");
        Ok(())
    }

    /// Consume the manager and return the combined discovery behaviour.
    pub fn into_behaviour(self) -> DiscoveryBehaviour {
        DiscoveryBehaviour {
            kademlia: self.kademlia.into_behaviour(),
            mdns: self.mdns.into_behaviour(),
            identify: self.identify,
            rendezvous: self.rendezvous,
        }
    }

    /// Consume the manager and return the discovery state.
    pub fn into_state(self) -> DiscoveryState {
        DiscoveryState::new(
            self.bootstrap_manager,
            self.random_walk,
            self.rendezvous_control,
        )
    }

    /// Consume the manager and return both the behaviour and state.
    pub fn into_parts(self) -> (DiscoveryBehaviour, DiscoveryState) {
        let PeerDiscoveryManager {
            kademlia,
            mdns,
            identify,
            rendezvous,
            bootstrap_manager,
            random_walk,
            rendezvous_control,
        } = self;

        let discovery_state = DiscoveryState::new(bootstrap_manager, random_walk, rendezvous_control);
        let discovery_behaviour = DiscoveryBehaviour {
            kademlia: kademlia.into_behaviour(),
            mdns: mdns.into_behaviour(),
            identify,
            rendezvous,
        };
        (discovery_behaviour, discovery_state)
    }
}
