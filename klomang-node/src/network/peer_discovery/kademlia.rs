use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::time::Duration;

use libp2p::kad::{
    self, store::MemoryStore, Behaviour, Config, Event, GetRecordOk, InboundRequest, QueryId,
    QueryResult, RecordKey,
};
use libp2p::{Multiaddr, PeerId};

/// Configuration for Kademlia DHT discovery.
#[derive(Clone, Debug)]
pub struct KademliaConfig {
    pub query_timeout: Duration,
    pub replication_factor: u32,
    pub parallelism: u32,
    pub bootstrap_nodes: Vec<(PeerId, Multiaddr)>,
}

impl Default for KademliaConfig {
    fn default() -> Self {
        Self {
            query_timeout: Duration::from_secs(120),
            replication_factor: 20,
            parallelism: 3,
            bootstrap_nodes: Vec::new(),
        }
    }
}

/// Kademlia DHT discovery engine for peer discovery and routing.
pub struct KademliaDiscovery {
    behaviour: Behaviour<MemoryStore>,
    config: KademliaConfig,
    bootstrap_started: bool,
    active_queries: HashSet<QueryId>,
    bootstrap_attempts: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
    backoff_factor: f64,
}

impl KademliaDiscovery {
    /// Create a new Kademlia discovery engine.
    pub fn new(local_peer_id: PeerId, config: KademliaConfig) -> Self {
        let mut kad_config = Config::default();
        kad_config.set_query_timeout(config.query_timeout);
        kad_config.set_replication_factor(
            NonZeroUsize::new(config.replication_factor as usize)
                .unwrap_or_else(|| NonZeroUsize::new(20).unwrap()),
        );
        kad_config.set_parallelism(
            NonZeroUsize::new(config.parallelism as usize)
                .unwrap_or_else(|| NonZeroUsize::new(3).unwrap()),
        );

        let store = MemoryStore::new(local_peer_id);
        let mut behaviour = Behaviour::with_config(local_peer_id, store, kad_config);
        behaviour.set_mode(Some(kad::Mode::Server));

        Self {
            behaviour,
            config,
            bootstrap_started: false,
            active_queries: HashSet::new(),
            bootstrap_attempts: 0,
            initial_backoff: Duration::from_secs(5),
            max_backoff: Duration::from_secs(180),
            backoff_factor: 2.0,
        }
    }

    /// Initialize bootstrap process to connect to known peers.
    pub fn bootstrap(&mut self) -> Result<(), kad::NoKnownPeers> {
        // Add bootstrap nodes to the routing table
        for (peer_id, addr) in &self.config.bootstrap_nodes {
            self.behaviour.add_address(peer_id, addr.clone());
        }

        // Start bootstrap if we have bootstrap nodes
        if !self.config.bootstrap_nodes.is_empty() {
            self.behaviour.bootstrap()?;
            self.bootstrap_started = true;
            log::info!(
                "Kademlia bootstrap initiated with {} bootstrap nodes",
                self.config.bootstrap_nodes.len()
            );
        }

        Ok(())
    }

    /// Add a peer to the routing table.
    pub fn add_peer(&mut self, peer_id: PeerId, addr: Multiaddr) {
        self.behaviour.add_address(&peer_id, addr.clone());
        log::debug!("Added peer {} to Kademlia routing table", peer_id);
    }

    /// Start a Kademlia random walk to discover peers from the DHT.
    pub fn random_walk(&mut self) -> QueryId {
        let target = PeerId::random();
        let query_id = self.behaviour.get_closest_peers(target);
        self.active_queries.insert(query_id);
        log::trace!("Started Kademlia random walk query {}", query_id);
        query_id
    }

    /// Get a record from the DHT.
    pub fn get_record(&mut self, key: RecordKey) -> QueryId {
        let query_id = self.behaviour.get_record(key);
        self.active_queries.insert(query_id);
        query_id
    }

    /// Put a record into the DHT.
    pub fn put_record(
        &mut self,
        key: RecordKey,
        value: Vec<u8>,
    ) -> Result<QueryId, kad::store::Error> {
        let record = kad::Record {
            key,
            value,
            publisher: None,
            expires: None,
        };

        let query_id = self.behaviour.put_record(record, kad::Quorum::One)?;
        self.active_queries.insert(query_id);
        Ok(query_id)
    }

    /// Get the closest peers to a target PeerId.
    pub fn get_closest_peers(&mut self, target: PeerId) -> QueryId {
        let query_id = self.behaviour.get_closest_peers(target);
        self.active_queries.insert(query_id);
        query_id
    }

    /// Handle Kademlia events.
    pub fn handle_event(&mut self, event: Event) -> Vec<DiscoveryMsg> {
        let mut messages = Vec::new();

        match event {
            Event::OutboundQueryProgressed { id, result, .. } => {
                match result {
                    QueryResult::Bootstrap(Ok(bootstrap_ok)) => {
                        log::info!(
                            "Bootstrap progress: {} peers remaining",
                            bootstrap_ok.num_remaining
                        );
                        messages.push(DiscoveryMsg::BootstrapProgress {
                            peer: bootstrap_ok.peer,
                            remaining: bootstrap_ok.num_remaining,
                        });
                    }
                    QueryResult::Bootstrap(Err(err)) => {
                        log::warn!("Bootstrap error: {:?}", err);
                        messages.push(DiscoveryMsg::BootstrapFailed);
                    }
                    QueryResult::GetClosestPeers(Ok(closest)) => {
                        log::info!("Found {} closest peers", closest.peers.len());
                        for peer in &closest.peers {
                            messages.push(DiscoveryMsg::PeerDiscovered(*peer));
                        }
                    }
                    QueryResult::GetClosestPeers(Err(err)) => {
                        log::warn!("GetClosestPeers error: {:?}", err);
                    }
                    QueryResult::PutRecord(Ok(_)) => {
                        log::debug!("Record stored in DHT");
                        messages.push(DiscoveryMsg::RecordStored);
                    }
                    QueryResult::PutRecord(Err(err)) => {
                        log::warn!("PutRecord error: {:?}", err);
                    }
                    QueryResult::GetRecord(Ok(get_record_ok)) => match get_record_ok {
                        GetRecordOk::FoundRecord(_record) => {
                            log::debug!("Retrieved one record from DHT");
                            messages.push(DiscoveryMsg::RecordsRetrieved(1));
                        }
                        GetRecordOk::FinishedWithNoAdditionalRecord { cache_candidates } => {
                            log::debug!("GetRecord finished with no additional records, {} cache candidates", cache_candidates.len());
                        }
                    },
                    QueryResult::GetRecord(Err(err)) => {
                        log::warn!("GetRecord error: {:?}", err);
                    }
                    _ => {}
                }

                self.active_queries.remove(&id);
            }
            Event::RoutingUpdated { peer, .. } => {
                log::debug!("Routing table updated for peer: {}", peer);
                messages.push(DiscoveryMsg::RoutingUpdated(peer));
            }
            Event::InboundRequest { request } => match request {
                InboundRequest::FindNode { .. } => {
                    log::trace!("Received FindNode request");
                }
                InboundRequest::GetRecord { .. } => {
                    log::trace!("Received GetRecord request");
                }
                InboundRequest::PutRecord { .. } => {
                    log::trace!("Received PutRecord request");
                }
                InboundRequest::GetProvider { .. } => {
                    log::trace!("Received GetProvider request");
                }
                InboundRequest::AddProvider { .. } => {
                    log::trace!("Received AddProvider request");
                }
            },
            _ => {}
        }

        messages
    }

    /// Get the Kademlia behaviour for Swarm integration.
    pub fn behaviour_mut(&mut self) -> &mut Behaviour<MemoryStore> {
        &mut self.behaviour
    }

    /// Consume this discovery engine and return the inner behaviour for Swarm integration.
    pub fn into_behaviour(self) -> Behaviour<MemoryStore> {
        self.behaviour
    }

    /// Get the next exponential backoff duration after a bootstrap failure.
    pub fn next_bootstrap_backoff(&mut self) -> Duration {
        self.bootstrap_attempts = self.bootstrap_attempts.saturating_add(1);
        let factor = self.backoff_factor.powi(self.bootstrap_attempts.saturating_sub(1) as i32);
        let backoff_ms = (self.initial_backoff.as_millis() as f64 * factor)
            .min(self.max_backoff.as_millis() as f64)
            .round() as u64;
        let backoff = Duration::from_millis(backoff_ms);
        log::debug!("Kademlia bootstrap backoff: {:?} (attempt {})", backoff, self.bootstrap_attempts);
        backoff
    }

    /// Reset the bootstrap backoff after a successful bootstrap.
    pub fn reset_bootstrap_backoff(&mut self) {
        self.bootstrap_attempts = 0;
    }

    /// Check if bootstrap is complete.
    pub fn is_bootstrapped(&self) -> bool {
        self.bootstrap_started && self.active_queries.is_empty()
    }

    /// Get the number of active queries.
    pub fn active_query_count(&self) -> usize {
        self.active_queries.len()
    }

    /// Get the number of peers in the routing table.
    pub fn peer_count(&mut self) -> usize {
        self.behaviour.kbuckets().count()
    }
}

/// Messages emitted by Kademlia discovery.
#[derive(Debug, Clone)]
pub enum DiscoveryMsg {
    PeerDiscovered(PeerId),
    BootstrapProgress { peer: PeerId, remaining: u32 },
    BootstrapFailed,
    RecordStored,
    RecordsRetrieved(usize),
    RoutingUpdated(PeerId),
}
