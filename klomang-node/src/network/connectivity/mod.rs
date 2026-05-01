pub mod autonat;
pub mod hole_punching;

use libp2p::swarm::NetworkBehaviour;
use libp2p::PeerId;

use crate::network::connectivity::autonat::{AutoNatConfig, AutoNatManager};
use crate::network::connectivity::hole_punching::{HolePunchingConfig, HolePunchingManager};

/// Connectivity behaviour for AutoNAT, DCUtR, and relay-based NAT traversal.
#[derive(NetworkBehaviour)]
#[behaviour(out_event = "ConnectivityBehaviourEvent")]
pub struct ConnectivityBehaviour {
    pub autonat: libp2p::autonat::Behaviour,
    pub dcutr: libp2p::dcutr::Behaviour,
    pub relay_client: libp2p::relay::client::Behaviour,
}

/// Events emitted by the combined connectivity behaviour.
#[derive(Debug)]
pub enum ConnectivityBehaviourEvent {
    AutoNat(libp2p::autonat::Event),
    DcUtR(libp2p::dcutr::Event),
    Relay(libp2p::relay::client::Event),
}

impl From<libp2p::autonat::Event> for ConnectivityBehaviourEvent {
    fn from(event: libp2p::autonat::Event) -> Self {
        ConnectivityBehaviourEvent::AutoNat(event)
    }
}

impl From<libp2p::dcutr::Event> for ConnectivityBehaviourEvent {
    fn from(event: libp2p::dcutr::Event) -> Self {
        ConnectivityBehaviourEvent::DcUtR(event)
    }
}

impl From<libp2p::relay::client::Event> for ConnectivityBehaviourEvent {
    fn from(event: libp2p::relay::client::Event) -> Self {
        ConnectivityBehaviourEvent::Relay(event)
    }
}

impl ConnectivityBehaviour {
    pub fn new(local_peer_id: PeerId, config: ConnectivityConfig) -> Self {
        let autonat = AutoNatManager::new(local_peer_id.clone(), config.autonat)
            .into_behaviour();
        let hole_punching = HolePunchingManager::new(local_peer_id.clone(), config.hole_punching);

        ConnectivityBehaviour {
            autonat,
            dcutr: hole_punching.into_dcutr_behaviour(),
            relay_client: hole_punching.into_relay_behaviour(),
        }
    }
}

/// Connectivity configuration for NAT traversal.
#[derive(Clone, Debug)]
pub struct ConnectivityConfig {
    pub autonat: AutoNatConfig,
    pub hole_punching: HolePunchingConfig,
}

impl Default for ConnectivityConfig {
    fn default() -> Self {
        Self {
            autonat: AutoNatConfig::default(),
            hole_punching: HolePunchingConfig::default(),
        }
    }
}
