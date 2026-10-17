use std::fmt::{self, Debug, Formatter};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use getrandom::fill;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;

const SECRET_BYTES: usize = 32;
const HASH_BYTES: usize = 32;

/// A one-time browser secret and its database-safe digest.
///
/// The raw value is returned once to the caller for a secure cookie or form
/// value; it must never be persisted, logged, or serialized into an outbox.
pub struct IssuedIdentitySecret {
    pub secret: String,
    pub hash: Vec<u8>,
}

impl Debug for IssuedIdentitySecret {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssuedIdentitySecret")
            .field("secret", &"[redacted]")
            .field("hash", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum IdentitySecretError {
    #[error("secure random generation failed")]
    Random,
    #[error("identity secret is malformed")]
    Malformed,
    #[error("identity secret digest has an invalid length")]
    InvalidStoredHash,
}

/// Issues a 256-bit URL-safe opaque secret and SHA-256 digest.
///
/// # Errors
///
/// Returns an error when the operating system's cryptographic random source is
/// unavailable.
pub fn issue_identity_secret() -> Result<IssuedIdentitySecret, IdentitySecretError> {
    let mut bytes = [0_u8; SECRET_BYTES];
    fill(&mut bytes).map_err(|_| IdentitySecretError::Random)?;
    Ok(IssuedIdentitySecret {
        secret: URL_SAFE_NO_PAD.encode(bytes),
        hash: digest(&bytes).to_vec(),
    })
}

/// Validates a candidate secret against a stored SHA-256 digest in constant
/// time. The stored digest must be exactly 32 bytes; malformed candidates do
/// not authenticate.
///
/// # Errors
///
/// Returns `InvalidStoredHash` only for corrupt database data. A malformed
/// candidate is a normal non-match and returns `Ok(false)`.
pub fn verify_identity_secret(
    candidate: &str,
    stored_hash: &[u8],
) -> Result<bool, IdentitySecretError> {
    if stored_hash.len() != HASH_BYTES {
        return Err(IdentitySecretError::InvalidStoredHash);
    }
    let Some(candidate_hash) = hash_identity_secret(candidate)? else {
        return Ok(false);
    };
    Ok(bool::from(candidate_hash.ct_eq(stored_hash)))
}

/// Hashes a canonical identity secret for indexed lookup without accepting an
/// arbitrary string as a token. `None` means the candidate is malformed.
///
/// # Errors
///
/// This currently cannot fail beyond malformed input, which maps to `Ok(None)`;
/// the `Result` keeps callers aligned with other identity-secret APIs.
pub fn hash_identity_secret(candidate: &str) -> Result<Option<Vec<u8>>, IdentitySecretError> {
    let candidate = match URL_SAFE_NO_PAD.decode(candidate) {
        Ok(bytes) if bytes.len() == SECRET_BYTES => bytes,
        _ => return Ok(None),
    };
    Ok(Some(digest(&candidate).to_vec()))
}

fn digest(bytes: &[u8]) -> [u8; HASH_BYTES] {
    Sha256::digest(bytes).into()
}

#[cfg(test)]
mod tests {
    use super::{IdentitySecretError, issue_identity_secret, verify_identity_secret};

    #[test]
    fn issued_secret_is_url_safe_and_verifies_only_against_its_digest()
    -> Result<(), IdentitySecretError> {
        let issued = issue_identity_secret()?;
        assert!(!issued.secret.contains(['+', '/', '=']));
        assert!(verify_identity_secret(&issued.secret, &issued.hash)?);
        assert!(!verify_identity_secret("not-a-valid-secret", &issued.hash)?);
        Ok(())
    }

    #[test]
    fn corrupt_stored_hash_is_a_hard_error() -> Result<(), IdentitySecretError> {
        let issued = issue_identity_secret()?;
        assert_eq!(
            verify_identity_secret(&issued.secret, b"too-short"),
            Err(IdentitySecretError::InvalidStoredHash)
        );
        Ok(())
    }
}
