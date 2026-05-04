use libp2p::PeerId;

/// Relay client configuration for circuit relay v2 reservation behavior.
#[derive(Clone, Debug)]
pub struct RelayClientConfig {
    pub max_reservations: usize,
    pub max_reservations_per_peer: usize,
}

impl Default for RelayClientConfig {
    fn default() -> Self {
        Self {
            max_reservations: 3,
            max_reservations_per_peer: 1,
        }
    }
}

pub fn build_relay_client_behaviour(local_peer_id: PeerId) -> libp2p::relay::client::Behaviour {
    let (_transport, behaviour) = libp2p::relay::client::new(local_peer_id);
    behaviour
}
