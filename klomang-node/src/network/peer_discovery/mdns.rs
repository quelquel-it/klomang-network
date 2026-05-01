use std::time::Duration;

use libp2p::mdns::tokio::Behaviour as MdnsBehaviour;
use libp2p::mdns::Event as MdnsEvent;
use libp2p::{Multiaddr, PeerId};

/// Configuration for mDNS discovery.
#[derive(Clone, Debug)]
pub struct MdnsConfig {
    pub query_interval_ms: u64,
}

impl Default for MdnsConfig {
    fn default() -> Self {
        Self {
            query_interval_ms: 5000,
        }
    }
}

/// mDNS discovery for local network peer discovery.
pub struct MdnsDiscovery {
    behaviour: MdnsBehaviour,
}

impl MdnsDiscovery {
    /// Create a new mDNS discovery engine.
    pub fn new(
        local_peer_id: PeerId,
        config: MdnsConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mdns_config = libp2p::mdns::Config {
            query_interval: Duration::from_millis(config.query_interval_ms),
            ..Default::default()
        };

        let mdns = MdnsBehaviour::new(mdns_config, local_peer_id)?;

        Ok(Self { behaviour: mdns })
    }

    /// Handle mDNS events and return discovered peers.
    pub fn handle_event(&mut self, event: MdnsEvent) -> Vec<DiscoveryMsg> {
        let mut messages = Vec::new();

        match event {
            MdnsEvent::Discovered(list) => {
                for (peer_id, addr) in list {
                    log::info!("mDNS discovered peer: {}", peer_id);
                    messages.push(DiscoveryMsg::PeerDiscovered { peer_id, addr });
                }
            }
            MdnsEvent::Expired(list) => {
                for (peer_id, _addrs) in list {
                    log::info!("mDNS expired peer: {}", peer_id);
                    messages.push(DiscoveryMsg::PeerExpired(peer_id));
                }
            }
        }

        messages
    }

    /// Get the mDNS behaviour for Swarm integration.
    pub fn behaviour(&mut self) -> &mut MdnsBehaviour {
        &mut self.behaviour
    }

    /// Consume the discovery engine and return the underlying behaviour.
    pub fn into_behaviour(self) -> MdnsBehaviour {
        self.behaviour
    }
}

/// Messages emitted by mDNS discovery.
#[derive(Debug, Clone)]
pub enum DiscoveryMsg {
    PeerDiscovered { peer_id: PeerId, addr: Multiaddr },
    PeerExpired(PeerId),
}
