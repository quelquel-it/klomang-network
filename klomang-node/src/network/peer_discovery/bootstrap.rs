use std::env;
use std::fs;
use std::path::Path;
use std::time::Duration;

use libp2p::{multiaddr::Protocol, Multiaddr, PeerId};

/// Default bootstrap nodes used for testnet connectivity when environment variables are missing.
const DEFAULT_BOOTSTRAP_NODES: &[&str] = &[
    "/ip4/127.0.0.1/tcp/4001/p2p/12D3KooWAbc1234567890",
    "/ip4/127.0.0.1/tcp/4002/p2p/12D3KooWDef1234567890",
];

/// Bootstrap configuration for the Kademlia discovery engine.
#[derive(Clone, Debug)]
pub struct BootstrapConfig {
    pub bootstrap_nodes: Vec<(PeerId, Multiaddr)>,
    pub max_retries: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub backoff_factor: f64,
}

impl Default for BootstrapConfig {
    fn default() -> Self {
        Self {
            bootstrap_nodes: Self::load_bootstrap_nodes(),
            max_retries: 8,
            initial_backoff: Duration::from_secs(5),
            max_backoff: Duration::from_secs(180),
            backoff_factor: 2.0,
        }
    }
}

impl BootstrapConfig {
    /// Load bootstrap node list from environment or fallback to the default hardcoded testnet list.
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(file_path) = env::var("KL_MANG_BOOTSTRAP_NODES_FILE") {
            if let Ok(contents) = fs::read_to_string(Path::new(&file_path)) {
                let nodes = contents
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .filter_map(Self::parse_multiaddr_entry)
                    .collect::<Vec<_>>();
                if !nodes.is_empty() {
                    config.bootstrap_nodes = nodes;
                }
            }
        }

        if let Ok(bootstrap_nodes) = env::var("KL_MANG_BOOTSTRAP_NODES") {
            let nodes = bootstrap_nodes
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .filter_map(Self::parse_multiaddr_entry)
                .collect::<Vec<_>>();
            if !nodes.is_empty() {
                config.bootstrap_nodes = nodes;
            }
        }

        config
    }

    fn parse_multiaddr_entry(entry: &str) -> Option<(PeerId, Multiaddr)> {
        let multiaddr = entry.parse::<Multiaddr>().ok()?;
        multiaddr.iter().find_map(|protocol| {
            if let Protocol::P2p(peer_id) = protocol {
                Some((peer_id, multiaddr.clone()))
            } else {
                None
            }
        })
    }

    fn load_bootstrap_nodes() -> Vec<(PeerId, Multiaddr)> {
        DEFAULT_BOOTSTRAP_NODES
            .iter()
            .filter_map(|entry| Self::parse_multiaddr_entry(entry))
            .collect()
    }
}

/// Manager for bootstrap retries and trusted bootstrap peers.
pub struct BootstrapManager {
    pub config: BootstrapConfig,
    attempts: u32,
}

impl BootstrapManager {
    pub fn new(config: BootstrapConfig) -> Self {
        Self {
            config,
            attempts: 0,
        }
    }

    /// Attach bootstrap peers to the Kademlia routing table.
    pub fn seed_kademlia<F>(&self, mut add_peer: F)
    where
        F: FnMut(PeerId, Multiaddr),
    {
        for (peer_id, addr) in &self.config.bootstrap_nodes {
            add_peer(*peer_id, addr.clone());
            log::info!("Added bootstrap trusted peer {} at {}", peer_id, addr);
        }
    }

    /// Calculate the next exponential retry interval after a bootstrap failure.
    pub fn next_backoff(&mut self) -> Duration {
        self.attempts = self.attempts.saturating_add(1);
        let factor = self.config.backoff_factor.powi(self.attempts.saturating_sub(1) as i32);
        let backoff_ms = (self.config.initial_backoff.as_millis() as f64 * factor)
            .min(self.config.max_backoff.as_millis() as f64)
            .round() as u64;
        let backoff = Duration::from_millis(backoff_ms);
        log::debug!(
            "Bootstrap retry backoff set to {:?} after {} attempts",
            backoff,
            self.attempts
        );
        backoff
    }

    /// Reset the retry counter after a successful bootstrap.
    pub fn reset_backoff(&mut self) {
        self.attempts = 0;
    }

    /// Check whether a peer is one of the trusted bootstrap peers.
    pub fn is_trusted_peer(&self, peer_id: &PeerId) -> bool {
        self.config
            .bootstrap_nodes
            .iter()
            .any(|(trusted, _)| trusted == peer_id)
    }
}
