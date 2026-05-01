use libp2p::multiaddr::Multiaddr;
use libp2p::PeerId;
use libp2p::rendezvous::client::{Behaviour as RendezvousBehaviour, Event as RendezvousEvent, RegisterError};
use libp2p::rendezvous::{Cookie, Namespace};

/// Configuration for Rendezvous-based discovery and registration.
#[derive(Clone, Debug)]
pub struct RendezvousConfig {
    pub rendezvous_nodes: Vec<(PeerId, Multiaddr)>,
    pub namespace: String,
    pub query_limit: Option<u64>,
    pub register_ttl_secs: Option<u64>,
    pub discovery_interval_ms: u64,
}

impl Default for RendezvousConfig {
    fn default() -> Self {
        Self {
            rendezvous_nodes: Vec::new(),
            namespace: "klomang-network-v1".to_string(),
            query_limit: Some(16),
            register_ttl_secs: Some(libp2p::rendezvous::DEFAULT_TTL),
            discovery_interval_ms: 60_000,
        }
    }
}

/// Controller for Rendezvous discovery operations.
pub struct RendezvousControl {
    pub config: RendezvousConfig,
    pub namespace: Namespace,
    pub last_cookie: Option<Cookie>,
}

impl RendezvousControl {
    pub fn new(config: RendezvousConfig) -> Self {
        let namespace = Namespace::new(config.namespace.clone())
            .unwrap_or_else(|_| Namespace::new("klomang-network-v1".to_string()).unwrap());

        Self {
            config,
            namespace,
            last_cookie: None,
        }
    }

    /// Register this node with all configured rendezvous peers.
    pub fn register_all(&mut self, behaviour: &mut RendezvousBehaviour) -> Result<(), RegisterError> {
        for (peer_id, _) in &self.config.rendezvous_nodes {
            behaviour.register(self.namespace.clone(), *peer_id, self.config.register_ttl_secs)?;
            log::info!("Registered with Rendezvous node {} in namespace {}", peer_id, self.namespace);
        }
        Ok(())
    }

    /// Discover peers through configured rendezvous peers.
    pub fn discover_via_rendezvous(&mut self, behaviour: &mut RendezvousBehaviour) {
        for (peer_id, _) in &self.config.rendezvous_nodes {
            behaviour.discover(
                Some(self.namespace.clone()),
                self.last_cookie.clone(),
                self.config.query_limit,
                *peer_id,
            );
            log::debug!("Sent Rendezvous discovery request to {}", peer_id);
        }
    }

    /// Process a Rendezvous event and return discovered peer registrations.
    pub fn handle_event(&mut self, event: RendezvousEvent) -> Vec<(PeerId, Vec<Multiaddr>)> {
        match event {
            RendezvousEvent::Discovered { registrations, cookie, .. } => {
                self.last_cookie = Some(cookie);
                let mut peers = Vec::new();
                for registration in registrations {
                    let peer_id = registration.record.peer_id();
                    let addrs = registration
                        .record
                        .addresses()
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>();
                    peers.push((peer_id, addrs));
                }
                peers
            }
            _ => Vec::new(),
        }
    }
}
