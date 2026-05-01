use std::fmt;

use libp2p::{identity::Keypair, noise, tls, PeerId};

use crate::network::core_network::peer_id::NodeIdentity;

/// Errors returned by secure channel configuration.
#[derive(Debug)]
pub enum SecureChannelError {
    Noise(String),
    Tls(String),
    IdentityMismatch(String),
}

impl fmt::Display for SecureChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SecureChannelError::Noise(msg) => write!(f, "Noise error: {}", msg),
            SecureChannelError::Tls(msg) => write!(f, "TLS error: {}", msg),
            SecureChannelError::IdentityMismatch(msg) => write!(f, "Identity mismatch: {}", msg),
        }
    }
}

impl std::error::Error for SecureChannelError {}

impl From<noise::Error> for SecureChannelError {
    fn from(err: noise::Error) -> Self {
        SecureChannelError::Noise(err.to_string())
    }
}

impl From<tls::certificate::GenError> for SecureChannelError {
    fn from(err: tls::certificate::GenError) -> Self {
        SecureChannelError::Tls(err.to_string())
    }
}

/// Combined secure channel configuration for libp2p transport.
pub struct SecureChannel {
    pub noise_config: noise::Config,
    pub tls_config: tls::Config,
}

impl SecureChannel {
    /// Build secure channel configs from a local node identity.
    pub fn from_identity(identity: &NodeIdentity) -> Result<Self, SecureChannelError> {
        let keypair: &Keypair = identity.keypair();
        let noise_config = noise::Config::new(keypair)?;
        let tls_config = tls::Config::new(keypair)?;

        Ok(SecureChannel {
            noise_config,
            tls_config,
        })
    }

    /// Verify the remote peer identity after a secure handshake.
    pub fn verify_peer_id(actual: &PeerId, expected: &PeerId) -> Result<(), SecureChannelError> {
        if actual != expected {
            Err(SecureChannelError::IdentityMismatch(format!(
                "expected peer {} but negotiated {}",
                expected, actual
            )))
        } else {
            Ok(())
        }
    }
}

/// Build a secure fallback order: first Noise, then TLS.
pub fn build_secure_channel(identity: &NodeIdentity) -> Result<SecureChannel, SecureChannelError> {
    SecureChannel::from_identity(identity)
}
