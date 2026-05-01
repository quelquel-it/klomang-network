use std::collections::HashMap;
use std::time::Duration;

use libp2p::identify::{
    Behaviour as IdentifyBehaviour, Config as IdentifyConfig, Event as IdentifyEvent,
    Info as IdentifyInfo,
};
use libp2p::ping::{Behaviour as PingBehaviour, Config as PingConfig, Event as PingEvent};
use libp2p::{Multiaddr, PeerId};

/// Protocol handler for Identify and Ping protocols.
pub struct ProtocolHandler {
    identify_behaviour: IdentifyBehaviour,
    ping_behaviour: PingBehaviour,
    peer_info: HashMap<PeerId, PeerInfo>,
}

/// Extended peer information gathered from protocols.
#[derive(Clone, Debug)]
pub struct PeerInfo {
    pub peer_id: PeerId,
    pub protocol_version: Option<String>,
    pub agent_version: Option<String>,
    pub listen_addresses: Vec<Multiaddr>,
    pub observed_address: Option<Multiaddr>,
    pub protocols: Vec<String>,
    pub last_ping: Option<Duration>,
    pub ping_count: u32,
}

impl PeerInfo {
    /// Create new peer info from Identify information.
    pub fn from_identify(peer_id: PeerId, info: &IdentifyInfo) -> Self {
        Self {
            peer_id,
            protocol_version: Some(info.protocol_version.clone()),
            agent_version: Some(info.agent_version.clone()),
            listen_addresses: info.listen_addrs.clone(),
            observed_address: Some(info.observed_addr.clone()),
            protocols: info.protocols.iter().map(|p| p.to_string()).collect(),
            last_ping: None,
            ping_count: 0,
        }
    }

    /// Update ping information.
    pub fn update_ping(&mut self, rtt: Duration) {
        self.last_ping = Some(rtt);
        self.ping_count += 1;
    }

    /// Get the average ping time if available.
    pub fn average_ping(&self) -> Option<Duration> {
        self.last_ping // For now, just return last ping; could track average
    }

    /// Check if this peer supports required protocols.
    pub fn supports_required_protocols(&self) -> bool {
        // Check for basic required protocols
        let required = ["ping", "identify"];
        required
            .iter()
            .all(|req| self.protocols.iter().any(|p| p.contains(req)))
    }
}

impl ProtocolHandler {
    /// Create a new protocol handler with Identify and Ping behaviours.
    pub fn new(local_public_key: libp2p::identity::PublicKey) -> Self {
        let identify_config = IdentifyConfig::new("klomang/1.0.0".to_string(), local_public_key)
            .with_push_listen_addr_updates(true)
            .with_interval(Duration::from_secs(30)); // Identify every 30 seconds

        let identify_behaviour = IdentifyBehaviour::new(identify_config);

        let ping_config = PingConfig::new()
            .with_interval(Duration::from_secs(30)) // Ping every 30 seconds
            .with_timeout(Duration::from_secs(10));

        let ping_behaviour = PingBehaviour::new(ping_config);

        Self {
            identify_behaviour,
            ping_behaviour,
            peer_info: HashMap::new(),
        }
    }

    /// Handle incoming Identify events.
    pub fn handle_identify_event(&mut self, event: IdentifyEvent) {
        match event {
            IdentifyEvent::Received { peer_id, info } => {
                println!("Received identify info from peer {}", peer_id);
                println!("  Protocol version: {}", info.protocol_version);
                println!("  Agent version: {}", info.agent_version);
                println!("  Listen addresses: {:?}", info.listen_addrs);
                println!("  Observed address: {:?}", info.observed_addr);
                println!("  Supported protocols: {:?}", info.protocols);

                let peer_info = PeerInfo::from_identify(peer_id, &info);
                self.peer_info.insert(peer_id, peer_info);
            }
            IdentifyEvent::Sent { peer_id } => {
                println!("Sent identify info to peer {}", peer_id);
            }
            IdentifyEvent::Pushed { peer_id, .. } => {
                println!("Pushed identify info to peer {}", peer_id);
            }
            IdentifyEvent::Error { peer_id, error } => {
                eprintln!("Identify error with peer {}: {:?}", peer_id, error);
            }
        }
    }

    /// Handle incoming Ping events.
    pub fn handle_ping_event(&mut self, event: PingEvent) {
        // libp2p ping events are handled internally
        // This function is here for future extension
        match event {
            _ => {
                // Ping events don't expose timing information directly
                // The ping behaviour handles success/failure internally
            }
        }
    }

    /// Get information about a specific peer.
    pub fn get_peer_info(&self, peer_id: &PeerId) -> Option<&PeerInfo> {
        self.peer_info.get(peer_id)
    }

    /// Get all known peer information.
    pub fn get_all_peer_info(&self) -> Vec<&PeerInfo> {
        self.peer_info.values().collect()
    }

    /// Get peers with good connectivity (recent ping success).
    pub fn get_healthy_peers(&self) -> Vec<&PeerInfo> {
        self.peer_info
            .values()
            .filter(|info| {
                info.last_ping.is_some() &&
                info.last_ping.unwrap() < Duration::from_millis(500) && // < 500ms latency
                info.supports_required_protocols()
            })
            .collect()
    }

    /// Remove peer information when a peer disconnects.
    pub fn remove_peer(&mut self, peer_id: &PeerId) {
        self.peer_info.remove(peer_id);
    }

    /// Get the Identify behaviour for swarm integration.
    pub fn identify_behaviour(&mut self) -> &mut IdentifyBehaviour {
        &mut self.identify_behaviour
    }

    /// Get the Ping behaviour for swarm integration.
    pub fn ping_behaviour(&mut self) -> &mut PingBehaviour {
        &mut self.ping_behaviour
    }

    /// Get protocol statistics.
    pub fn get_stats(&self) -> ProtocolStats {
        let total_peers = self.peer_info.len();
        let healthy_peers = self.get_healthy_peers().len();

        ProtocolStats {
            total_peers,
            healthy_peers,
        }
    }
}

/// Protocol handler statistics.
#[derive(Debug, Clone)]
pub struct ProtocolStats {
    pub total_peers: usize,
    pub healthy_peers: usize,
}

impl ProtocolStats {
    /// Calculate health percentage.
    pub fn health_percentage(&self) -> f64 {
        if self.total_peers == 0 {
            0.0
        } else {
            (self.healthy_peers as f64 / self.total_peers as f64) * 100.0
        }
    }
}
