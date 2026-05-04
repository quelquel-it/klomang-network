use libp2p::PeerId;

/// AutoNAT configuration for public/private network status detection.
#[derive(Clone, Debug)]
pub struct AutoNatConfig {
    pub probe_interval_secs: u64,
}

impl Default for AutoNatConfig {
    fn default() -> Self {
        Self {
            probe_interval_secs: 30,
        }
    }
}

pub fn build_autonat_behaviour(local_peer_id: PeerId, _config: AutoNatConfig) -> libp2p::autonat::Behaviour {
    libp2p::autonat::Behaviour::new(local_peer_id, libp2p::autonat::Config::default())
}
