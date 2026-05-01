pub mod connectivity;
pub mod core_network;
pub mod peer_discovery;

pub use connectivity::{ConnectivityBehaviour, ConnectivityConfig};
pub use core_network::{
    build_multiaddr, build_secure_channel, initialize_network_stack, parse_multiaddr,
    peer_id_from_core_public_key, validate_multiaddr, validate_peer_legitimacy, CoreMultiplexer,
    CoreNetworkAddress, CoreNetworkTransport, DiscoveryEngine, IdentityError, IdentityKeyType,
    KlomangTransport, MultiaddrError, MultiplexerConfig, NetworkInitializationError, NodeIdentity,
    PeerInfo, PeerRecord, PeerRoutingTable, ProtocolHandler, ProtocolStats, SecureChannel,
    SecureChannelError, StreamType, TransportConfig, TransportError,
};

pub use peer_discovery::{
    DiscoveryBehaviour, DiscoveryConfig, KademliaConfig, KademliaDiscovery, MdnsConfig,
    MdnsDiscovery, PeerDiscoveryManager,
};
