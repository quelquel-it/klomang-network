use libp2p::PeerId;

/// DCUtR configuration for coordinated hole punching.
#[derive(Clone, Debug)]
pub struct DcUtRConfig {
    pub max_upgrade_attempts: u8,
}

impl Default for DcUtRConfig {
    fn default() -> Self {
        Self {
            max_upgrade_attempts: 3,
        }
    }
}

pub fn build_dcutr_behaviour(local_peer_id: PeerId, _config: DcUtRConfig) -> libp2p::dcutr::Behaviour {
    libp2p::dcutr::Behaviour::new(local_peer_id)
}
