use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::time::Duration;

use libp2p::kad::{self, store::MemoryStore, Behaviour, Config, Event, QueryId, RecordKey};
use libp2p::{multiaddr::Protocol, Multiaddr, PeerId};

/// Bootstrap node addresses for initial peer discovery.
/// These should be replaced with actual testnet/mainnet bootstrap nodes.
const BOOTSTRAP_NODES: &[&str] = &[
    "/ip4/127.0.0.1/tcp/4001/p2p/12D3KooWAbc1234567890", // Placeholder local bootstrap
    "/ip4/127.0.0.1/tcp/4002/p2p/12D3KooWDef1234567890", // Placeholder local bootstrap
];

/// Kademlia-based peer discovery engine.
pub struct DiscoveryEngine {
    kad_behaviour: Behaviour<MemoryStore>,
    bootstrap_nodes: Vec<Multiaddr>,
    active_queries: HashSet<QueryId>,
    bootstrap_started: bool,
}

impl DiscoveryEngine {
    /// Create a new discovery engine with Kademlia DHT.
    pub fn new(local_peer_id: PeerId) -> Self {
        let mut config = Config::default();
        config.set_query_timeout(Duration::from_secs(60));
        config.set_replication_factor(NonZeroUsize::new(20).unwrap());
        config.set_parallelism(NonZeroUsize::new(3).unwrap());

        let store = MemoryStore::new(local_peer_id);
        let mut kad_behaviour = Behaviour::with_config(local_peer_id, store, config);
        kad_behaviour.set_mode(Some(kad::Mode::Server));

        let bootstrap_nodes = BOOTSTRAP_NODES
            .iter()
            .filter_map(|addr| addr.parse().ok())
            .collect();

        Self {
            kad_behaviour,
            bootstrap_nodes,
            active_queries: HashSet::new(),
            bootstrap_started: false,
        }
    }

    /// Start the bootstrap process to connect to known peers.
    pub fn bootstrap(&mut self) -> Result<kad::QueryId, kad::NoKnownPeers> {
        // Add bootstrap nodes to the routing table
        for addr in &self.bootstrap_nodes {
            if let Some(Protocol::P2p(peer_id)) = addr.iter().last() {
                self.kad_behaviour.add_address(&peer_id, addr.clone());
            }
        }

        // Start the bootstrap process
        let result = self.kad_behaviour.bootstrap()?;
        self.bootstrap_started = true;
        Ok(result)
    }

    /// Start discovering peers by querying the DHT.
    pub fn start_peer_discovery(&mut self, target_peer: Option<PeerId>) {
        let target = target_peer.unwrap_or_else(|| PeerId::random());

        let query_id = self.kad_behaviour.get_closest_peers(target);
        self.active_queries.insert(query_id);
    }

    /// Put a record into the DHT (for testing/debugging).
    pub fn put_record(&mut self, key: RecordKey, value: Vec<u8>) -> QueryId {
        let record = kad::Record {
            key,
            value,
            publisher: None,
            expires: None,
        };

        self.kad_behaviour
            .put_record(record, kad::Quorum::One)
            .unwrap()
    }

    /// Get a record from the DHT.
    pub fn get_record(&mut self, key: RecordKey) -> QueryId {
        self.kad_behaviour.get_record(key)
    }

    /// Handle incoming Kademlia events.
    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::OutboundQueryProgressed { id, result, .. } => {
                match result {
                    kad::QueryResult::Bootstrap(Ok(kad::BootstrapOk {
                        peer,
                        num_remaining,
                    })) => {
                        println!(
                            "Bootstrap: Connected to peer {}, {} remaining",
                            peer, num_remaining
                        );
                    }
                    kad::QueryResult::Bootstrap(Err(err)) => {
                        eprintln!("Bootstrap failed: {:?}", err);
                    }
                    kad::QueryResult::GetClosestPeers(Ok(kad::GetClosestPeersOk {
                        peers, ..
                    })) => {
                        println!("Found {} closest peers", peers.len());
                        for peer in peers {
                            println!("  Peer: {}", peer);
                        }
                    }
                    kad::QueryResult::GetClosestPeers(Err(err)) => {
                        eprintln!("GetClosestPeers failed: {:?}", err);
                    }
                    kad::QueryResult::PutRecord(Ok(_)) => {
                        println!("Record put successfully");
                    }
                    kad::QueryResult::PutRecord(Err(err)) => {
                        eprintln!("PutRecord failed: {:?}", err);
                    }
                    kad::QueryResult::GetRecord(Ok(_)) => {
                        println!("Retrieved records");
                    }
                    kad::QueryResult::GetRecord(Err(err)) => {
                        eprintln!("GetRecord failed: {:?}", err);
                    }
                    _ => {} // Handle other query types as needed
                }

                self.active_queries.remove(&id);
            }
            Event::RoutingUpdated { peer, .. } => {
                println!("Routing table updated for peer: {}", peer);
            }
            Event::InboundRequest { request } => {
                match request {
                    kad::InboundRequest::FindNode { .. } => {
                        // Handle find node requests
                    }
                    kad::InboundRequest::GetRecord { .. } => {
                        // Handle get record requests
                    }
                    kad::InboundRequest::PutRecord { .. } => {
                        // Handle put record requests
                    }
                    kad::InboundRequest::GetProvider { .. } => {
                        // Handle get provider requests
                    }
                    kad::InboundRequest::AddProvider { .. } => {
                        // Handle add provider requests
                    }
                }
            }
            _ => {}
        }
    }

    /// Get the Kademlia behaviour for swarm integration.
    pub fn behaviour(&mut self) -> &mut Behaviour<MemoryStore> {
        &mut self.kad_behaviour
    }

    /// Check if bootstrap is complete.
    pub fn is_bootstrapped(&self) -> bool {
        self.bootstrap_started && self.active_queries.is_empty()
    }

    /// Get the number of active queries.
    pub fn active_query_count(&self) -> usize {
        self.active_queries.len()
    }
}
