use std::fmt;
use std::fs;
use std::path::Path;

use libp2p::identity::{self, ed25519, secp256k1, DecodingError, Keypair};
use libp2p::PeerId;
use zeroize::Zeroizing;

/// Supported identity key types for the Klomang node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityKeyType {
    Ed25519,
    Secp256k1,
}

impl fmt::Display for IdentityKeyType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdentityKeyType::Ed25519 => write!(f, "Ed25519"),
            IdentityKeyType::Secp256k1 => write!(f, "Secp256k1"),
        }
    }
}

/// Errors returned by node identity operations.
#[derive(Debug)]
pub enum IdentityError {
    Io(String),
    Decode(String),
    Encode(String),
    InvalidPublicKey(String),
    UnsupportedKeyType(String),
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdentityError::Io(msg) => write!(f, "I/O error: {}", msg),
            IdentityError::Decode(msg) => write!(f, "Decode error: {}", msg),
            IdentityError::Encode(msg) => write!(f, "Encode error: {}", msg),
            IdentityError::InvalidPublicKey(msg) => write!(f, "Invalid public key: {}", msg),
            IdentityError::UnsupportedKeyType(msg) => write!(f, "Unsupported key type: {}", msg),
        }
    }
}

impl std::error::Error for IdentityError {}

impl From<std::io::Error> for IdentityError {
    fn from(err: std::io::Error) -> Self {
        IdentityError::Io(err.to_string())
    }
}

impl From<DecodingError> for IdentityError {
    fn from(err: DecodingError) -> Self {
        IdentityError::Decode(err.to_string())
    }
}

/// Represents a node identity with a local keypair and derived PeerId.
pub struct NodeIdentity {
    peer_id: PeerId,
    keypair: Keypair,
    key_type: IdentityKeyType,
}

impl NodeIdentity {
    /// Create a NodeIdentity from a local libp2p keypair.
    pub fn from_keypair(keypair: Keypair) -> Result<Self, IdentityError> {
        let peer_id = PeerId::from_public_key(&keypair.public());
        let key_type = match keypair.public().key_type() {
            libp2p::identity::KeyType::Ed25519 => IdentityKeyType::Ed25519,
            libp2p::identity::KeyType::Secp256k1 => IdentityKeyType::Secp256k1,
            other => return Err(IdentityError::UnsupportedKeyType(format!("{:?}", other))),
        };
        Ok(Self {
            peer_id,
            keypair,
            key_type,
        })
    }

    /// Generate a fresh node identity from the requested key type.
    pub fn generate(key_type: IdentityKeyType) -> Self {
        let keypair = match key_type {
            IdentityKeyType::Ed25519 => Keypair::generate_ed25519(),
            IdentityKeyType::Secp256k1 => Keypair::generate_secp256k1(),
        };

        Self {
            peer_id: PeerId::from_public_key(&keypair.public()),
            keypair,
            key_type,
        }
    }

    /// Load an identity from a protobuf-encoded keypair file.
    pub fn load(path: &Path) -> Result<Self, IdentityError> {
        let encoded = fs::read(path)?;
        let keypair = Keypair::from_protobuf_encoding(&encoded)?;
        Self::from_keypair(keypair)
    }

    /// Save the identity keypair to disk in protobuf form.
    pub fn save(&self, path: &Path) -> Result<(), IdentityError> {
        let bytes = Zeroizing::new(self.keypair.to_protobuf_encoding()?);
        fs::write(path, &*bytes)?;
        Ok(())
    }

    /// Load or generate an identity, persisting it to disk for reuse.
    pub fn load_or_generate(path: &Path, key_type: IdentityKeyType) -> Result<Self, IdentityError> {
        if path.exists() {
            Self::load(path)
        } else {
            let identity = Self::generate(key_type);
            identity.save(path)?;
            Ok(identity)
        }
    }

    /// Return the active libp2p PeerId.
    pub fn peer_id(&self) -> &PeerId {
        &self.peer_id
    }

    /// Return the local libp2p identity keypair.
    pub fn keypair(&self) -> &Keypair {
        &self.keypair
    }

    /// Return the configured key type.
    pub fn key_type(&self) -> IdentityKeyType {
        self.key_type
    }
}

/// Derive a libp2p PeerId from a raw public key produced by klomang-core.
/// Supports Secp256k1 and Ed25519 material depending on the core key format.
pub fn peer_id_from_core_public_key(public_key_bytes: &[u8]) -> Result<PeerId, IdentityError> {
    // Try Ed25519 first (32 bytes)
    if public_key_bytes.len() == 32 {
        if let Ok(ed25519_pub) = ed25519::PublicKey::try_from_bytes(public_key_bytes) {
            let public_key = identity::PublicKey::from(ed25519_pub);
            return Ok(PeerId::from_public_key(&public_key));
        }
    }

    // Try standard secp256k1 compressed format (33 bytes) or uncompressed (65 bytes)
    if let Ok(secp_pub) = secp256k1::PublicKey::try_from_bytes(public_key_bytes) {
        let public_key = identity::PublicKey::from(secp_pub);
        return Ok(PeerId::from_public_key(&public_key));
    }

    Err(IdentityError::InvalidPublicKey(format!(
        "Unsupported core public key format for PeerId derivation (len: {})",
        public_key_bytes.len()
    )))
}
