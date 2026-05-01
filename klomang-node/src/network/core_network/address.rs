use libp2p::{
    identity,
    multiaddr::{Multiaddr, Protocol},
    PeerId,
};
use std::net::IpAddr;

/// Core network address helper with Multiaddr validation and conversion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreNetworkAddress {
    pub address: Multiaddr,
    pub peer_id: PeerId,
}

/// Errors returned by the core networking address layer.
#[derive(Debug)]
pub enum MultiaddrError {
    InvalidMultiaddr(String),
    MissingPeerId,
    InvalidPeerId(String),
    InvalidPublicKey(String),
}

impl std::fmt::Display for MultiaddrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MultiaddrError::InvalidMultiaddr(msg) => write!(f, "Invalid multiaddr: {}", msg),
            MultiaddrError::MissingPeerId => write!(f, "Multiaddr is missing /p2p/PeerId"),
            MultiaddrError::InvalidPeerId(msg) => write!(f, "Invalid PeerId: {}", msg),
            MultiaddrError::InvalidPublicKey(msg) => write!(f, "Invalid public key: {}", msg),
        }
    }
}

impl std::error::Error for MultiaddrError {}

/// Parse and validate a Multiaddr, insisting on an IP, TCP, and /p2p/PeerId suffix.
pub fn parse_multiaddr(addr: &str) -> Result<CoreNetworkAddress, MultiaddrError> {
    let multiaddr: Multiaddr = addr
        .parse()
        .map_err(|e: libp2p::multiaddr::Error| MultiaddrError::InvalidMultiaddr(e.to_string()))?;

    let mut peer_id = None;
    for protocol in multiaddr.iter() {
        if let Protocol::P2p(peer_id_value) = protocol {
            peer_id = Some(peer_id_value);
            break;
        }
    }

    let peer_id = peer_id.ok_or(MultiaddrError::MissingPeerId)?;
    Ok(CoreNetworkAddress {
        address: multiaddr,
        peer_id,
    })
}

/// Validate a multiaddr string for the core networking stack.
pub fn validate_multiaddr(addr: &str) -> bool {
    parse_multiaddr(addr).is_ok()
}

/// Derive a libp2p PeerId from a Klomang Core public key blob.
///
/// The public key is expected to be a compressed secp256k1 public key bytes vector.
pub fn peer_id_from_core_public_key(pubkey_bytes: &[u8]) -> Result<PeerId, MultiaddrError> {
    let secp_pub = identity::secp256k1::PublicKey::try_from_bytes(pubkey_bytes)
        .map_err(|e| MultiaddrError::InvalidPublicKey(e.to_string()))?;

    let public_key = identity::PublicKey::from(secp_pub);
    Ok(PeerId::from_public_key(&public_key))
}

/// Build a well-formed multiaddr from an IP address, TCP port, and libp2p PeerId.
pub fn build_multiaddr(ip: IpAddr, port: u16, peer_id: PeerId) -> Multiaddr {
    let mut addr = Multiaddr::empty();
    match ip {
        IpAddr::V4(ipv4) => addr.push(Protocol::Ip4(ipv4)),
        IpAddr::V6(ipv6) => addr.push(Protocol::Ip6(ipv6)),
    }
    addr.push(Protocol::Tcp(port));
    addr.push(Protocol::P2p(peer_id.into()));
    addr
}
