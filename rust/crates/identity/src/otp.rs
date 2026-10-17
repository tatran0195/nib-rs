//! Email-OTP generation and constant-time verification primitives.
//!
//! Challenge persistence supplies purpose, expiry, attempt counts, resend
//! rotation, and rate limiting. This module never logs or persists the raw code.

use std::fmt::{self, Debug, Formatter};

use getrandom::fill;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;

const OTP_UPPER_BOUND: u32 = 1_000_000;
const OTP_BYTES: usize = 4;
const OTP_HASH_BYTES: usize = 32;

/// A six-digit OTP and its database-safe digest. The code is returned exactly
/// once to the email-rendering path and must not be emitted to logs or outbox.
pub struct IssuedOtpCode {
    pub code: String,
    pub hash: Vec<u8>,
}

impl Debug for IssuedOtpCode {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssuedOtpCode")
            .field("code", &"[redacted]")
            .field("hash", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum OtpError {
    #[error("secure random generation failed")]
    Random,
    #[error("stored OTP digest has an invalid length")]
    InvalidStoredHash,
}

/// Issues a uniformly sampled six-digit code with leading zeros retained.
///
/// # Errors
///
/// Returns an error when the operating system random source is unavailable.
pub fn issue_otp_code() -> Result<IssuedOtpCode, OtpError> {
    let value = random_below(OTP_UPPER_BOUND)?;
    let code = format!("{value:06}");
    Ok(IssuedOtpCode {
        hash: hash_code(&code).to_vec(),
        code,
    })
}

/// Constant-time comparison against a stored OTP digest. Wrong-shape input is
/// a normal non-match rather than a separate authentication signal.
///
/// # Errors
///
/// Returns an error only for corrupt stored digest data.
pub fn verify_otp_code(candidate: &str, stored_hash: &[u8]) -> Result<bool, OtpError> {
    if stored_hash.len() != OTP_HASH_BYTES {
        return Err(OtpError::InvalidStoredHash);
    }
    if candidate.len() != 6 || !candidate.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(false);
    }
    Ok(bool::from(hash_code(candidate).ct_eq(stored_hash)))
}

fn random_below(upper_bound: u32) -> Result<u32, OtpError> {
    let rejected_above = u32::MAX - (u32::MAX % upper_bound);
    loop {
        let mut bytes = [0_u8; OTP_BYTES];
        fill(&mut bytes).map_err(|_| OtpError::Random)?;
        let value = u32::from_be_bytes(bytes);
        if value < rejected_above {
            return Ok(value % upper_bound);
        }
    }
}

fn hash_code(code: &str) -> [u8; OTP_HASH_BYTES] {
    Sha256::digest(code.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::{OtpError, issue_otp_code, verify_otp_code};

    #[test]
    fn code_has_six_digits_and_verifies_only_against_its_digest() -> Result<(), OtpError> {
        let issued = issue_otp_code()?;
        assert_eq!(issued.code.len(), 6);
        assert!(issued.code.bytes().all(|byte| byte.is_ascii_digit()));
        assert!(verify_otp_code(&issued.code, &issued.hash)?);
        assert!(!verify_otp_code("nototp", &issued.hash)?);
        Ok(())
    }

    #[test]
    fn corrupt_digest_is_not_treated_as_a_failed_attempt() -> Result<(), OtpError> {
        let issued = issue_otp_code()?;
        assert_eq!(
            verify_otp_code(&issued.code, b"invalid"),
            Err(OtpError::InvalidStoredHash)
        );
        Ok(())
    }
}
