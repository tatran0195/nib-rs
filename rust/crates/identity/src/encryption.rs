//! Versioned authenticated encryption for short-lived identity secrets.
//!
//! This is for OTP delivery payloads and OAuth PKCE verifier state only. Raw
//! session tokens remain one-way hashes and are never encryptable/recoverable.

use chacha20poly1305::{
    Key, XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use getrandom::fill;
use thiserror::Error;

const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 24;
const VERSION_BYTES: usize = 2;

#[derive(Clone)]
pub struct EncryptionKey {
    pub version: u16,
    key: [u8; KEY_BYTES],
}

impl std::fmt::Debug for EncryptionKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EncryptionKey")
            .field("version", &self.version)
            .field("key", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum EncryptionError {
    #[error("encryption key must be exactly 32 bytes")]
    InvalidKey,
    #[error("secure random generation failed")]
    Random,
    #[error("encrypted payload is malformed")]
    Malformed,
    #[error("encrypted payload key version is unavailable")]
    UnknownKeyVersion,
    #[error("encrypted payload authentication failed")]
    Authentication,
}

impl EncryptionKey {
    /// Creates a versioned key from secret-manager material.
    ///
    /// # Errors
    /// Returns `InvalidKey` unless the material is exactly 32 bytes.
    pub fn new(version: u16, material: &[u8]) -> Result<Self, EncryptionError> {
        let key = material
            .try_into()
            .map_err(|_| EncryptionError::InvalidKey)?;
        Ok(Self { version, key })
    }
}

/// Encrypts a payload with XChaCha20-Poly1305. Format: big-endian key version,
/// 24-byte random nonce, then AEAD ciphertext/tag. Associated data binds the
/// ciphertext to an explicit feature context.
///
/// # Errors
/// Returns random-source or encryption failures.
pub fn encrypt_secret(
    key: &EncryptionKey,
    plaintext: &[u8],
    associated_data: &[u8],
) -> Result<Vec<u8>, EncryptionError> {
    let mut nonce = [0_u8; NONCE_BYTES];
    fill(&mut nonce).map_err(|_| EncryptionError::Random)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key.key));
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: associated_data,
            },
        )
        .map_err(|_| EncryptionError::Authentication)?;
    let mut payload = Vec::with_capacity(VERSION_BYTES + NONCE_BYTES + ciphertext.len());
    payload.extend_from_slice(&key.version.to_be_bytes());
    payload.extend_from_slice(&nonce);
    payload.extend_from_slice(&ciphertext);
    Ok(payload)
}

/// Decrypts with the matching key version. Current and retained previous keys
/// belong in the caller-provided bounded ring.
///
/// # Errors
/// Returns classified malformed, unavailable-key, or authentication failures.
pub fn decrypt_secret(
    keys: &[EncryptionKey],
    payload: &[u8],
    associated_data: &[u8],
) -> Result<Vec<u8>, EncryptionError> {
    if payload.len() <= VERSION_BYTES + NONCE_BYTES {
        return Err(EncryptionError::Malformed);
    }
    let version = u16::from_be_bytes([payload[0], payload[1]]);
    let key = keys
        .iter()
        .find(|key| key.version == version)
        .ok_or(EncryptionError::UnknownKeyVersion)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key.key));
    cipher
        .decrypt(
            XNonce::from_slice(&payload[VERSION_BYTES..VERSION_BYTES + NONCE_BYTES]),
            Payload {
                msg: &payload[VERSION_BYTES + NONCE_BYTES..],
                aad: associated_data,
            },
        )
        .map_err(|_| EncryptionError::Authentication)
}

#[cfg(test)]
mod tests {
    use super::{EncryptionError, EncryptionKey, decrypt_secret, encrypt_secret};

    #[test]
    fn encrypted_secret_round_trips_with_a_bounded_rotation_ring() -> Result<(), EncryptionError> {
        let previous = EncryptionKey::new(7, &[7; 32])?;
        let current = EncryptionKey::new(8, &[8; 32])?;
        let old_payload = encrypt_secret(&previous, b"old-one-time-secret", b"identity-otp")?;
        let current_payload = encrypt_secret(&current, b"new-one-time-secret", b"identity-otp")?;
        let ring = [current.clone(), previous.clone()];
        assert_eq!(
            decrypt_secret(&ring, &old_payload, b"identity-otp")?,
            b"old-one-time-secret"
        );
        assert_eq!(
            decrypt_secret(&ring, &current_payload, b"identity-otp")?,
            b"new-one-time-secret"
        );
        assert!(matches!(
            decrypt_secret(
                std::slice::from_ref(&current),
                &old_payload,
                b"identity-otp"
            ),
            Err(EncryptionError::UnknownKeyVersion)
        ));
        assert!(matches!(
            decrypt_secret(&ring, &current_payload, b"other"),
            Err(EncryptionError::Authentication)
        ));
        Ok(())
    }
}
