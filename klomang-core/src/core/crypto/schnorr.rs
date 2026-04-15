use k256::schnorr::{SigningKey, VerifyingKey, Signature};
use k256::ecdsa::signature::{Signer, Verifier};
use rand::rngs::OsRng;
use crate::core::errors::CoreError;
use crate::core::state::transaction::{Transaction, SigHashType};
use crate::core::dag::BlockNode;
use blake3;

const TAG_TX_SIGN: &str = "KLOMANG_TX_V1";

pub struct KeyPairWrapper {
    signing_key: SigningKey,
}

impl KeyPairWrapper {
    pub fn new() -> Self {
        let signing_key = SigningKey::random(&mut OsRng);
        Self { signing_key }
    }

    pub fn from_seed(seed: u64) -> Result<Self, CoreError> {
        // Deterministic key derivation via blake3(seed || counter) with fallback to avoid zero scalar.
        for counter in 0u64..1024 {
            let mut hasher = blake3::Hasher::new();
            hasher.update(&seed.to_le_bytes());
            hasher.update(&counter.to_le_bytes());
            let digest = hasher.finalize();
            let mut secret_bytes = [0u8; 32];
            secret_bytes.copy_from_slice(&digest.as_bytes()[..32]);

            if let Ok(signing_key) = SigningKey::from_bytes(&secret_bytes) {
                return Ok(KeyPairWrapper { signing_key });
            }
        }

        Err(CoreError::CryptographicError(
            "Failed to derive deterministic keypair from seed".to_string(),
        ))
    }

    pub fn public_key(&self) -> VerifyingKey {
        *self.signing_key.verifying_key()
    }

    pub fn sign(&self, msg: &[u8]) -> Signature {
        self.signing_key.sign(msg)
    }
}

impl Default for KeyPairWrapper {
    fn default() -> Self {
        Self::new()
    }
}

pub fn verify(pubkey: &VerifyingKey, msg: &[u8], signature: &Signature) -> bool {
    pubkey.verify(msg, signature).is_ok()
}

/// BIP340-style tagged hash for domain separation
pub fn tagged_hash(tag: &str, data: &[u8]) -> [u8; 32] {
    let tag_hash = blake3::hash(tag.as_bytes());
    let mut hasher = blake3::Hasher::new();
    hasher.update(tag_hash.as_bytes());
    hasher.update(tag_hash.as_bytes());
    hasher.update(data);
    let hash_result = hasher.finalize();
    let mut result = [0u8; 32];
    result.copy_from_slice(&hash_result.as_bytes()[..32]);
    result
}

/// Serialize transaction for sighash computation
pub fn serialize_tx_for_sighash(
    tx: &Transaction,
    input_index: usize,
    sighash: SigHashType,
) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&tx.chain_id.to_be_bytes());
    
    match sighash {
        SigHashType::All => {
            for (idx, input) in tx.inputs.iter().enumerate() {
                if idx == input_index {
                    data.extend_from_slice(&input.pubkey);
                } else {
                    data.extend_from_slice(input.prev_tx.as_bytes());
                    data.extend_from_slice(&input.index.to_be_bytes());
                }
            }
            for output in &tx.outputs {
                data.extend_from_slice(&output.value.to_be_bytes());
                data.extend_from_slice(output.pubkey_hash.as_bytes());
            }
        },
        SigHashType::None => {
            for (idx, input) in tx.inputs.iter().enumerate() {
                if idx == input_index {
                    data.extend_from_slice(&input.pubkey);
                } else {
                    data.extend_from_slice(input.prev_tx.as_bytes());
                    data.extend_from_slice(&input.index.to_be_bytes());
                }
            }
        },
        SigHashType::Single => {
            for (idx, input) in tx.inputs.iter().enumerate() {
                if idx == input_index {
                    data.extend_from_slice(&input.pubkey);
                } else {
                    data.extend_from_slice(input.prev_tx.as_bytes());
                    data.extend_from_slice(&input.index.to_be_bytes());
                }
            }
            if input_index < tx.outputs.len() {
                let output = &tx.outputs[input_index];
                data.extend_from_slice(&output.value.to_be_bytes());
                data.extend_from_slice(output.pubkey_hash.as_bytes());
            }
        },
    }
    
    data.extend_from_slice(&tx.locktime.to_be_bytes());
    data
}

/// Compute sighash for transaction input
pub fn compute_sighash(
    tx: &Transaction,
    input_index: usize,
    sighash: SigHashType,
) -> Result<[u8; 32], CoreError> {
    let serialized = serialize_tx_for_sighash(tx, input_index, sighash);
    Ok(tagged_hash(TAG_TX_SIGN, &serialized))
}

/// Verify Schnorr signature with BIP340-compliance
pub fn verify_schnorr(
    pubkey_bytes: &[u8; 32],
    sig_bytes: &[u8; 64],
    msg: &[u8],
) -> Result<bool, CoreError> {
    let pubkey = VerifyingKey::from_bytes(pubkey_bytes)
        .map_err(|_| CoreError::InvalidPublicKey)?;
    
    if sig_bytes.len() != 64 {
        return Err(CoreError::InvalidSignature);
    }
    
    let sig = Signature::try_from(&sig_bytes[..])
        .map_err(|_| CoreError::InvalidSignature)?;
    
    let msg_hash = tagged_hash(TAG_TX_SIGN, msg);
    Ok(pubkey.verify(&msg_hash, &sig).is_ok())
}

/// Batch verify multiple Schnorr signatures with parallel processing
pub fn batch_verify(
    items: &[(VerifyingKey, [u8; 32], Signature)],
) -> Result<bool, CoreError> {
    if items.is_empty() {
        return Ok(true);
    }
    
    // Use parallel verification for better TPS
    use std::thread;
    use std::sync::mpsc;
    
    let chunk_size = 10; // Process in chunks for optimal parallelism
    let chunks: Vec<_> = items.chunks(chunk_size).collect();
    
    if chunks.len() == 1 {
        // Single chunk, verify sequentially
        for (pubkey, msg, sig) in items {
            if pubkey.verify(msg, sig).is_err() {
                return Ok(false);
            }
        }
    } else {
        // Multiple chunks, use parallel verification
        let (tx, rx) = mpsc::channel();
        
        for chunk in chunks {
            let tx = tx.clone();
            let chunk = chunk.to_vec();
            thread::spawn(move || {
                let mut valid = true;
                for (pubkey, msg, sig) in chunk {
                    if pubkey.verify(&msg, &sig).is_err() {
                        valid = false;
                        break;
                    }
                }
                tx.send(valid).unwrap();
            });
        }
        
        // Drop the original sender so receiver knows when done
        drop(tx);
        
        // Collect results
        for result in rx {
            if !result {
                return Ok(false);
            }
        }
    }
    
    Ok(true)
}

/// Verify block signature menggunakan Schnorr
pub fn verify_block_signature(block: &BlockNode) -> bool {
    if let Some(sig_vec) = &block.header.signature {
        if sig_vec.len() != 64 {
            return false;
        }
        let mut sig_bytes = [0u8; 64];
        sig_bytes.copy_from_slice(sig_vec);
        
        // Serialize block header tanpa signature untuk verifikasi
        let mut header_without_sig = block.header.clone();
        header_without_sig.signature = None;
        
        let serialized = bincode::serialize(&header_without_sig).unwrap_or_default();
        let msg_hash = tagged_hash(TAG_TX_SIGN, &serialized);
        
        // Asumsikan pubkey ada di block (misalnya dari coinbase tx)
        // Untuk simplicity, gunakan pubkey dari tx pertama jika ada
        if let Some(coinbase_tx) = block.transactions.first() {
            if let Some(output) = coinbase_tx.outputs.first() {
                let pubkey_bytes = *output.pubkey_hash.as_bytes();
                return verify_schnorr(&pubkey_bytes, &sig_bytes, &msg_hash).unwrap_or(false);
            }
        }
    }
    false
}
