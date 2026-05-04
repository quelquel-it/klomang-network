pub mod autonat;
pub mod dcutr;
pub mod relay;

use std::collections::{HashMap, HashSet};
use libp2p::autonat::{Behaviour as AutoNatBehaviour, Event as AutoNatEvent};
use libp2p::core::multiaddr::Protocol;
use libp2p::dcutr::{Behaviour as DcUtRBehaviour, Event as DcUtREvent};
use libp2p::relay::client::{Behaviour as RelayClientBehaviour, Event as RelayClientEvent};
use libp2p::swarm::NetworkBehaviour;
use libp2p::{Multiaddr, PeerId};

use crate::network::connectivity::autonat::{build_autonat_behaviour, AutoNatConfig};
use crate::network::connectivity::dcutr::{build_dcutr_behaviour, DcUtRConfig};
use crate::network::connectivity::relay::RelayClientConfig;

/// Connectivity behaviour for AutoNAT, Relay v2, and DCUtR hole punching.
#[derive(NetworkBehaviour)]
#[behaviour(out_event = "ConnectivityBehaviourEvent")]
pub struct ConnectivityBehaviour {
    pub autonat: AutoNatBehaviour,
    pub relay_client: RelayClientBehaviour,
    pub dcutr: DcUtRBehaviour,
}

/// Events emitted by the combined connectivity behaviour.
#[derive(Debug)]
pub enum ConnectivityBehaviourEvent {
    AutoNat(AutoNatEvent),
    Relay(RelayClientEvent),
    DcUtR(DcUtREvent),
}

impl From<AutoNatEvent> for ConnectivityBehaviourEvent {
    fn from(event: AutoNatEvent) -> Self {
        ConnectivityBehaviourEvent::AutoNat(event)
    }
}

impl From<RelayClientEvent> for ConnectivityBehaviourEvent {
    fn from(event: RelayClientEvent) -> Self {
        ConnectivityBehaviourEvent::Relay(event)
    }
}

impl From<DcUtREvent> for ConnectivityBehaviourEvent {
    fn from(event: DcUtREvent) -> Self {
        ConnectivityBehaviourEvent::DcUtR(event)
    }
}

impl ConnectivityBehaviour {
    pub fn new(
        local_peer_id: PeerId,
        relay_behaviour: RelayClientBehaviour,
        config: ConnectivityConfig,
    ) -> Self {
        let autonat = build_autonat_behaviour(local_peer_id.clone(), config.autonat.clone());
        let dcutr = build_dcutr_behaviour(local_peer_id.clone(), config.dcutr.clone());

        ConnectivityBehaviour {
            autonat,
            relay_client: relay_behaviour,
            dcutr,
        }
    }
}

/// Tracks pending relay candidates and active reservations separately from the network behaviour.
#[derive(Debug)]
pub struct ConnectivityState {
    relay_candidates: Vec<Multiaddr>,
    active_reservations: HashSet<Multiaddr>,
    reservations_by_peer: HashMap<PeerId, usize>,
    config: RelayClientConfig,
}

impl ConnectivityState {
    pub fn new(config: RelayClientConfig) -> Self {
        ConnectivityState {
            relay_candidates: Vec::new(),
            active_reservations: HashSet::new(),
            reservations_by_peer: HashMap::new(),
            config,
        }
    }

    fn peer_from_candidate(addr: &Multiaddr) -> Option<PeerId> {
        addr.iter()
            .find_map(|protocol| match protocol {
                Protocol::P2p(peer_id) => PeerId::from_multihash(peer_id.into()).ok(),
                _ => None,
            })
    }

    pub fn register_relay_candidate(&mut self, relay_peer_id: PeerId, listen_addrs: &[Multiaddr]) {
        if self.relay_candidates.len() >= self.config.max_reservations {
            return;
        }

        for addr in listen_addrs {
            if self.relay_candidates.len() >= self.config.max_reservations {
                break;
            }

            if addr.iter().any(|protocol| matches!(protocol, Protocol::P2pCircuit)) {
                continue;
            }

            let mut candidate = addr.clone();
            if !candidate.iter().any(|protocol| matches!(protocol, Protocol::P2p(_))) {
                candidate = candidate.with(Protocol::P2p(relay_peer_id.into()));
            }
            candidate = candidate.with(Protocol::P2pCircuit);

            if self.relay_candidates.contains(&candidate) {
                continue;
            }

            let peer = Self::peer_from_candidate(&candidate);
            if let Some(peer_id) = peer {
                let count = self.reservations_by_peer.get(&peer_id).copied().unwrap_or(0);
                if count >= self.config.max_reservations_per_peer {
                    continue;
                }
            }

            self.relay_candidates.push(candidate);
        }
    }

    pub fn reserve_relay_slots(&mut self) -> Vec<Multiaddr> {
        let mut reserved = Vec::new();

        for candidate in &self.relay_candidates {
            if self.active_reservations.len() + reserved.len() >= self.config.max_reservations {
                break;
            }

            if self.active_reservations.contains(candidate) {
                continue;
            }

            let maybe_peer = Self::peer_from_candidate(candidate);
            if let Some(peer_id) = maybe_peer {
                let count = self.reservations_by_peer.get(&peer_id).copied().unwrap_or(0);
                if count >= self.config.max_reservations_per_peer {
                    continue;
                }
                self.reservations_by_peer.insert(peer_id, count + 1);
            }

            self.active_reservations.insert(candidate.clone());
            reserved.push(candidate.clone());
        }

        reserved
    }
}

/// Connectivity configuration for NAT traversal.
#[derive(Clone, Debug)]
pub struct ConnectivityConfig {
    pub autonat: AutoNatConfig,
    pub relay: RelayClientConfig,
    pub dcutr: DcUtRConfig,
}

impl Default for ConnectivityConfig {
    fn default() -> Self {
        Self {
            autonat: AutoNatConfig::default(),
            relay: RelayClientConfig::default(),
            dcutr: DcUtRConfig::default(),
        }
    }
}
