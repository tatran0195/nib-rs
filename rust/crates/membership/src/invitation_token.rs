//! Versioned signed acceptance-token primitive for ADR-0007.
//!
//! The token is intentionally self-contained: workers can mint an acceptance
//! URL from an invitation row without persisting a raw secret in the aggregate
//! or outbox payload. Acceptance still checks current invitation state and the
//! authenticated recipient in PostgreSQL; signature validity alone grants
//! nothing.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvitationTokenClaims {
    pub invitation_id: Uuid,
    pub expires_at: OffsetDateTime,
}

#[derive(Debug, Error)]
pub enum InvitationTokenError {
    #[error("invitation signing key is invalid")]
    InvalidSigningKey,
    #[error("invitation token is malformed")]
    Malformed,
    #[error("invitation token signature is invalid")]
    InvalidSignature,
    #[error("invitation token has expired")]
    Expired,
}

/// Signs a versioned acceptance token. Callers must source `expires_at` from
/// the invitation aggregate, never from a client request.
///
/// # Errors
///
/// Returns an error if the signing key cannot initialize the HMAC primitive.
pub fn sign_invitation_token(
    signing_key: &[u8],
    claims: InvitationTokenClaims,
) -> Result<String, InvitationTokenError> {
    let body = format!(
        "v1.{}.{}",
        claims.invitation_id.hyphenated(),
        claims.expires_at.unix_timestamp()
    );
    let signature = signature(signing_key, body.as_bytes())?;
    Ok(format!("{body}.{}", URL_SAFE_NO_PAD.encode(signature)))
}

/// Verifies token integrity and expiry. It does not load database state; the
/// invitation acceptance transaction must still lock and authorize the row.
///
/// # Errors
///
/// Returns a classified malformed, signature, expiry, or key error.
pub fn verify_invitation_token(
    signing_key: &[u8],
    token: &str,
    now: OffsetDateTime,
) -> Result<InvitationTokenClaims, InvitationTokenError> {
    let mut segments = token.split('.');
    let version = segments.next().ok_or(InvitationTokenError::Malformed)?;
    let id = segments.next().ok_or(InvitationTokenError::Malformed)?;
    let expiry = segments.next().ok_or(InvitationTokenError::Malformed)?;
    let encoded_signature = segments.next().ok_or(InvitationTokenError::Malformed)?;
    if version != "v1" || segments.next().is_some() {
        return Err(InvitationTokenError::Malformed);
    }
    let invitation_id = Uuid::parse_str(id).map_err(|_| InvitationTokenError::Malformed)?;
    let expiry_seconds = expiry
        .parse::<i64>()
        .map_err(|_| InvitationTokenError::Malformed)?;
    let expires_at = OffsetDateTime::from_unix_timestamp(expiry_seconds)
        .map_err(|_| InvitationTokenError::Malformed)?;
    let provided = URL_SAFE_NO_PAD
        .decode(encoded_signature)
        .map_err(|_| InvitationTokenError::Malformed)?;
    let body = format!("v1.{id}.{expiry}");
    let mut mac = HmacSha256::new_from_slice(signing_key)
        .map_err(|_| InvitationTokenError::InvalidSigningKey)?;
    mac.update(body.as_bytes());
    mac.verify_slice(&provided)
        .map_err(|_| InvitationTokenError::InvalidSignature)?;
    if expires_at <= now {
        return Err(InvitationTokenError::Expired);
    }
    Ok(InvitationTokenClaims {
        invitation_id,
        expires_at,
    })
}

/// Verifies against the current key first, then a bounded previous-key ring for
/// key rotation. Only a signature mismatch proceeds to an older key; malformed
/// and expired tokens are terminal and do not trigger further attempts.
///
/// # Errors
///
/// Returns a classified token error when no key validates the token.
pub fn verify_invitation_token_with_keys(
    current_key: &[u8],
    previous_keys: &[Vec<u8>],
    token: &str,
    now: OffsetDateTime,
) -> Result<InvitationTokenClaims, InvitationTokenError> {
    match verify_invitation_token(current_key, token, now) {
        Ok(claims) => Ok(claims),
        Err(InvitationTokenError::InvalidSignature) => {
            for key in previous_keys {
                match verify_invitation_token(key, token, now) {
                    Ok(claims) => return Ok(claims),
                    Err(InvitationTokenError::InvalidSignature) => {}
                    Err(error) => return Err(error),
                }
            }
            Err(InvitationTokenError::InvalidSignature)
        }
        Err(error) => Err(error),
    }
}

fn signature(signing_key: &[u8], body: &[u8]) -> Result<Vec<u8>, InvitationTokenError> {
    let mut mac = HmacSha256::new_from_slice(signing_key)
        .map_err(|_| InvitationTokenError::InvalidSigningKey)?;
    mac.update(body);
    Ok(mac.finalize().into_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use time::{Duration, OffsetDateTime};
    use uuid::Uuid;

    use super::{
        InvitationTokenClaims, InvitationTokenError, sign_invitation_token,
        verify_invitation_token, verify_invitation_token_with_keys,
    };

    const SIGNING_KEY: &[u8] = b"this test-only invitation signing key has enough bytes";

    #[test]
    fn signed_token_round_trips_and_is_url_safe() -> Result<(), InvitationTokenError> {
        let now = OffsetDateTime::UNIX_EPOCH + Duration::hours(1);
        let claims = InvitationTokenClaims {
            invitation_id: Uuid::from_u128(0x11111111_2222_3333_4444_555555555555),
            expires_at: now + Duration::days(7),
        };
        let token = sign_invitation_token(SIGNING_KEY, claims)?;
        assert!(!token.contains('+'));
        assert!(!token.contains('/'));
        assert_eq!(verify_invitation_token(SIGNING_KEY, &token, now)?, claims);
        Ok(())
    }

    #[test]
    fn token_tampering_and_expiry_fail_closed() -> Result<(), InvitationTokenError> {
        let now = OffsetDateTime::UNIX_EPOCH + Duration::hours(1);
        let claims = InvitationTokenClaims {
            invitation_id: Uuid::nil(),
            expires_at: now + Duration::seconds(1),
        };
        let token = sign_invitation_token(SIGNING_KEY, claims)?;
        let tampered = token.replacen("v1.", "v2.", 1);
        assert!(matches!(
            verify_invitation_token(SIGNING_KEY, &tampered, now),
            Err(InvitationTokenError::Malformed)
        ));
        assert!(matches!(
            verify_invitation_token(SIGNING_KEY, &token, claims.expires_at),
            Err(InvitationTokenError::Expired)
        ));
        Ok(())
    }

    #[test]
    fn a_previous_key_verifies_a_pre_rotation_token() -> Result<(), InvitationTokenError> {
        let now = OffsetDateTime::UNIX_EPOCH + Duration::hours(1);
        let old_key = b"a previous test-only invitation signing key bytes";
        let claims = InvitationTokenClaims {
            invitation_id: Uuid::nil(),
            expires_at: now + Duration::days(1),
        };
        let token = sign_invitation_token(old_key, claims)?;
        assert_eq!(
            verify_invitation_token_with_keys(SIGNING_KEY, &[old_key.to_vec()], &token, now)?,
            claims
        );
        Ok(())
    }
}
