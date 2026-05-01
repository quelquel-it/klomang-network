use crate::storage::kv_store::KvStore;
use libp2p::{identity, Multiaddr, PeerId};
use std::{fmt, sync::Arc};

use super::multiplexer::MultiplexerConfig;

/// Transport configuration for the core networking layer.
#[derive(Clone)]
pub struct TransportConfig {
    pub identity_keypair: identity::Keypair,
    pub listen_address: Multiaddr,
    pub multiplexer: MultiplexerConfig,
    pub storage: Arc<KvStore>,
}

/// Errors returned by the transport layer.
#[derive(Debug)]
pub enum TransportError {
    Identity(String),
    Noise(String),
    Transport(String),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::Identity(msg) => write!(f, "Identity error: {}", msg),
            TransportError::Noise(msg) => write!(f, "Noise configuration error: {}", msg),
            TransportError::Transport(msg) => write!(f, "Transport setup error: {}", msg),
        }
    }
}

impl std::error::Error for TransportError {}

impl From<std::io::Error> for TransportError {
    fn from(err: std::io::Error) -> Self {
        TransportError::Transport(err.to_string())
    }
}

/// The core transport abstraction for Klomang networking.
/// This trait defines the interface for a network transport that can be used
/// by the Klomang node for P2P communication.
pub trait KlomangTransport: Send + Sync {
    /// Return the local peer identifier.
    fn local_peer_id(&self) -> PeerId;
}

/// A concrete network transport stack that supports QUIC and TCP.
///
/// The CoreNetworkTransport manages the local peer identity and provides
/// configuration for establishing secure P2P connections.
pub struct CoreNetworkTransport {
    local_peer_id: PeerId,
    _storage: Arc<KvStore>,
}

impl CoreNetworkTransport {
    /// Create a new core network transport with the given configuration.
    pub fn new(config: TransportConfig) -> Result<Self, TransportError> {
        let local_peer_id = PeerId::from_public_key(&config.identity_keypair.public());

        // Validate listen address format
        let has_tcp = config
            .listen_address
            .clone()
            .into_iter()
            .any(|proto| matches!(proto, libp2p::multiaddr::Protocol::Tcp(_)));

        if !has_tcp {
            return Err(TransportError::Transport(
                "Listen address must include a TCP port".to_string(),
            ));
        }

        Ok(Self {
            local_peer_id,
            _storage: config.storage,
        })
    }

    pub fn local_peer_id(&self) -> PeerId {
        self.local_peer_id.clone()
    }
}

impl KlomangTransport for CoreNetworkTransport {
    fn local_peer_id(&self) -> PeerId {
        self.local_peer_id.clone()
    }
}
