use libp2p::autonat::{Behaviour as AutoNatBehaviour, Event as AutoNatEvent, NatStatus};
use libp2p::PeerId;
use libp2p::Multiaddr;

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

/// AutoNAT manager that tracks network status and discovered public addresses.
pub struct AutoNatManager {
    pub behaviour: AutoNatBehaviour,
    pub last_status: NatStatus,
    pub public_addrs: Vec<Multiaddr>,
}

impl AutoNatManager {
    pub fn new(local_peer_id: PeerId, _config: AutoNatConfig) -> Self {
        let behaviour = AutoNatBehaviour::new(local_peer_id);
        Self {
            behaviour,
            last_status: NatStatus::Unknown,
            public_addrs: Vec::new(),
        }
    }

    pub fn into_behaviour(self) -> AutoNatBehaviour {
        self.behaviour
    }

    pub fn handle_event(&mut self, event: AutoNatEvent) -> Option<(NatStatus, Vec<Multiaddr>)> {
        match event {
            AutoNatEvent::StatusChanged { old, new } => {
                self.last_status = new.clone();
                if let NatStatus::Public(addr) = &new {
                    self.public_addrs.push(addr.clone());
                }
                Some((new, self.public_addrs.clone()))
            }
            _ => None,
        }
    }
}
