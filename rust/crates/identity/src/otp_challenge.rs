//! Transactional PostgreSQL email-OTP challenge lifecycle.

use nibleaf_core::UserId;
use nibleaf_outbox::{NewEvent, append};
use serde_json::json;
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{
    EncryptionError, EncryptionKey, IssuedOtpCode, OtpError, encrypt_secret, issue_otp_code,
    verify_otp_code,
};

const OTP_TTL: Duration = Duration::minutes(10);
const RESEND_WINDOW: Duration = Duration::minutes(5);
const MAX_SENDS_PER_WINDOW: i64 = 3;
const OTP_DELIVERY_AAD: &[u8] = b"nibleaf.identity.otp.delivery.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OtpPurpose {
    SignIn,
    VerifyEmail,
    ChangeEmail,
}

impl OtpPurpose {
    #[must_use]
    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "sign_in" => Some(Self::SignIn),
            "verify_email" => Some(Self::VerifyEmail),
            "change_email" => Some(Self::ChangeEmail),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_db(self) -> &'static str {
        match self {
            Self::SignIn => "sign_in",
            Self::VerifyEmail => "verify_email",
            Self::ChangeEmail => "change_email",
        }
    }
}

#[derive(Debug, Error)]
pub enum OtpChallengeError {
    #[error("OTP persistence failed")]
    Database(#[from] sqlx::Error),
    #[error("OTP generation failed")]
    Otp(#[from] OtpError),
    #[error("OTP delivery secret encryption failed")]
    Encryption(#[from] EncryptionError),
    #[error("OTP delivery intent could not be persisted")]
    Outbox(#[from] nibleaf_core::AppError),
    #[error("invalid email address")]
    InvalidEmail,
    #[error("OTP send rate is limited")]
    RateLimited,
    #[error("OTP challenge was rejected")]
    Rejected,
}

#[derive(Debug, Error)]
pub enum EmailChangeError {
    #[error("email change persistence failed")]
    Database(#[from] sqlx::Error),
    #[error("email change OTP flow failed")]
    Otp(#[from] OtpChallengeError),
    #[error("email change was rejected")]
    Rejected,
}

/// Opaque acknowledgement for a two-stage email-change flow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmailChangeChallenge {
    pub challenge_id: Uuid,
    pub expires_at: OffsetDateTime,
}

pub struct IssuedOtpChallenge {
    pub challenge_id: Uuid,
    pub code: String,
    pub expires_at: OffsetDateTime,
}

impl std::fmt::Debug for IssuedOtpChallenge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IssuedOtpChallenge")
            .field("challenge_id", &self.challenge_id)
            .field("code", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// OTP challenge acknowledgement safe to return to an HTTP handler. It omits
/// both the code and the durable delivery identifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuedOtpChallenge {
    pub challenge_id: Uuid,
    pub expires_at: OffsetDateTime,
}

/// The verified challenge facts available only to the authenticated application
/// flow after one-time consumption. They must never be serialized directly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsumedOtpChallenge {
    pub normalized_email: String,
    pub purpose: OtpPurpose,
}

#[derive(FromRow)]
struct ChallengeRow {
    id: Uuid,
    normalized_email: String,
    purpose: String,
    code_hash: Vec<u8>,
    expires_at: OffsetDateTime,
    attempts: i16,
    max_attempts: i16,
    state: String,
}

#[derive(FromRow)]
struct EmailChangeRow {
    id: Uuid,
    old_normalized_email: String,
    new_normalized_email: String,
    expires_at: OffsetDateTime,
}

/// Rotates any pending OTP for the email/purpose and creates a new ten-minute
/// challenge. This low-level helper has no delivery intent; HTTP flows must use
/// durable queue functions instead.
///
/// # Errors
/// Returns a generic rate-limit error without exposing account existence.
pub async fn issue_otp_challenge(
    pool: &PgPool,
    email: &str,
    purpose: OtpPurpose,
) -> Result<IssuedOtpChallenge, OtpChallengeError> {
    let email = canonical_email(email)?;
    let mut transaction = pool.begin().await?;
    let issued = issue_in_transaction(&mut transaction, &email, purpose).await?;
    transaction.commit().await?;
    Ok(issued)
}

/// Atomically creates an OTP challenge, stores its encrypted delivery payload,
/// and appends a platform-scoped outbox event. The browser receives no code.
///
/// # Errors
/// Returns a generic rate-limit error without exposing account existence.
pub async fn queue_otp_challenge_delivery(
    pool: &PgPool,
    email: &str,
    purpose: OtpPurpose,
    encryption_key: &EncryptionKey,
) -> Result<QueuedOtpChallenge, OtpChallengeError> {
    let email = canonical_email(email)?;
    let mut transaction = pool.begin().await?;
    let issued = issue_in_transaction(&mut transaction, &email, purpose).await?;
    queue_issued_delivery(&mut transaction, &issued, encryption_key).await?;
    transaction.commit().await?;
    Ok(QueuedOtpChallenge {
        challenge_id: issued.challenge_id,
        expires_at: issued.expires_at,
    })
}

/// Starts a two-stage email change by proving the currently registered email.
///
/// Every ordinary request for a live account queues a current-mailbox OTP—even
/// when the requested new address is malformed, unchanged, or already used.
/// This deliberate decoy delivery prevents the caller from turning arrival of a
/// current-mailbox OTP into an email-ownership oracle. Only an eligible target
/// receives an active email-change state record; a decoy challenge cannot send
/// a code to a new mailbox when it is later submitted to verification.
///
/// A new request atomically cancels any prior active change before rotating the
/// current-mailbox proof. The returned handle is safe to serialize but is not a
/// credential: later stages still bind it to this user and a one-time OTP.
///
/// # Errors
/// Returns `Rejected` only for an unavailable user. Rate limiting and durable
/// delivery/persistence failures retain their specific internal error forms.
pub async fn request_email_change(
    pool: &PgPool,
    user_id: &UserId,
    new_email: &str,
    encryption_key: &EncryptionKey,
) -> Result<EmailChangeChallenge, EmailChangeError> {
    let mut transaction = pool.begin().await?;
    let current_email: Option<String> =
        sqlx::query_scalar("SELECT lower(email) FROM \"user\" WHERE id=$1 FOR UPDATE")
            .bind(user_id.as_str())
            .fetch_optional(&mut *transaction)
            .await?;
    let Some(current_email) = current_email else {
        return Err(EmailChangeError::Rejected);
    };

    // A later request necessarily invalidates any prior current/new proof. Do
    // this in the same transaction as OTP rotation so no stale state remains
    // actionable after the browser receives a newer continuation handle.
    sqlx::query(
        "UPDATE rust_identity_email_change SET state='cancelled', updated_at=NOW() WHERE user_id=$1 AND state IN ('pending_current','pending_new')",
    )
    .bind(user_id.as_str())
    .execute(&mut *transaction)
    .await?;
    let issued =
        issue_in_transaction(&mut transaction, &current_email, OtpPurpose::VerifyEmail).await?;
    queue_issued_delivery(&mut transaction, &issued, encryption_key).await?;

    let eligible_target = match canonical_email(new_email) {
        Ok(new_normalized_email) if new_normalized_email != current_email => {
            let target_taken: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM \"user\" WHERE lower(email)=$1)")
                    .bind(&new_normalized_email)
                    .fetch_one(&mut *transaction)
                    .await?;
            (!target_taken).then_some(new_normalized_email)
        }
        Ok(_) | Err(OtpChallengeError::InvalidEmail) => None,
        Err(error) => return Err(EmailChangeError::Otp(error)),
    };
    if let Some(new_normalized_email) = eligible_target {
        sqlx::query(
            "INSERT INTO rust_identity_email_change (id,user_id,old_normalized_email,new_normalized_email,current_challenge_id,expires_at) VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(Uuid::now_v7())
        .bind(user_id.as_str())
        .bind(current_email)
        .bind(new_normalized_email)
        .bind(issued.challenge_id)
        .bind(issued.expires_at)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(EmailChangeChallenge {
        challenge_id: issued.challenge_id,
        expires_at: issued.expires_at,
    })
}

/// Verifies the current mailbox and, in the same transaction, creates and
/// queues the OTP for the requested new mailbox.
///
/// # Errors
/// Returns `Rejected` for any stale, foreign, expired, or invalid transition.
pub async fn verify_current_email_change(
    pool: &PgPool,
    user_id: &UserId,
    current_challenge_id: Uuid,
    code: &str,
    encryption_key: &EncryptionKey,
) -> Result<EmailChangeChallenge, EmailChangeError> {
    let mut transaction = pool.begin().await?;
    let change = sqlx::query_as::<_, EmailChangeRow>(
        "SELECT id,old_normalized_email,new_normalized_email,expires_at FROM rust_identity_email_change WHERE user_id=$1 AND current_challenge_id=$2 AND state='pending_current' FOR UPDATE",
    )
    .bind(user_id.as_str())
    .bind(current_challenge_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(change) = change else {
        return Err(EmailChangeError::Rejected);
    };
    if change.expires_at <= OffsetDateTime::now_utc() {
        cancel_change(&mut transaction, change.id).await?;
        transaction.commit().await?;
        return Err(EmailChangeError::Rejected);
    }
    let facts = match consume_in_transaction(&mut transaction, current_challenge_id, code).await {
        Ok(facts) => facts,
        Err(OtpChallengeError::Rejected) => {
            transaction.commit().await?;
            return Err(EmailChangeError::Rejected);
        }
        Err(error) => return Err(EmailChangeError::Otp(error)),
    };
    if facts.purpose != OtpPurpose::VerifyEmail
        || facts.normalized_email != change.old_normalized_email
    {
        return Err(EmailChangeError::Rejected);
    }
    let target_taken: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM \"user\" WHERE lower(email)=$1 AND id<>$2)",
    )
    .bind(&change.new_normalized_email)
    .bind(user_id.as_str())
    .fetch_one(&mut *transaction)
    .await?;
    if target_taken {
        cancel_change(&mut transaction, change.id).await?;
        transaction.commit().await?;
        return Err(EmailChangeError::Rejected);
    }
    let issued = issue_in_transaction(
        &mut transaction,
        &change.new_normalized_email,
        OtpPurpose::ChangeEmail,
    )
    .await?;
    queue_issued_delivery(&mut transaction, &issued, encryption_key).await?;
    sqlx::query(
        "UPDATE rust_identity_email_change SET state='pending_new',new_challenge_id=$2,current_verified_at=NOW(),updated_at=NOW() WHERE id=$1 AND state='pending_current'",
    )
    .bind(change.id)
    .bind(issued.challenge_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(EmailChangeChallenge {
        challenge_id: issued.challenge_id,
        expires_at: issued.expires_at,
    })
}

/// Consumes the new-mailbox OTP and atomically changes the user's email.
///
/// # Errors
/// Returns `Rejected` for any stale, foreign, expired, or invalid transition.
pub async fn confirm_email_change(
    pool: &PgPool,
    user_id: &UserId,
    new_challenge_id: Uuid,
    code: &str,
) -> Result<(), EmailChangeError> {
    let mut transaction = pool.begin().await?;
    let change = sqlx::query_as::<_, EmailChangeRow>(
        "SELECT id,old_normalized_email,new_normalized_email,expires_at FROM rust_identity_email_change WHERE user_id=$1 AND new_challenge_id=$2 AND state='pending_new' FOR UPDATE",
    )
    .bind(user_id.as_str())
    .bind(new_challenge_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(change) = change else {
        return Err(EmailChangeError::Rejected);
    };
    if change.expires_at <= OffsetDateTime::now_utc() {
        cancel_change(&mut transaction, change.id).await?;
        transaction.commit().await?;
        return Err(EmailChangeError::Rejected);
    }
    let facts = match consume_in_transaction(&mut transaction, new_challenge_id, code).await {
        Ok(facts) => facts,
        Err(OtpChallengeError::Rejected) => {
            transaction.commit().await?;
            return Err(EmailChangeError::Rejected);
        }
        Err(error) => return Err(EmailChangeError::Otp(error)),
    };
    if facts.purpose != OtpPurpose::ChangeEmail
        || facts.normalized_email != change.new_normalized_email
    {
        return Err(EmailChangeError::Rejected);
    }
    let updated = sqlx::query(
        "UPDATE \"user\" SET email=$2, \"emailVerified\"=TRUE, \"updatedAt\"=NOW() WHERE id=$1 AND lower(email)=$3",
    )
    .bind(user_id.as_str())
    .bind(&change.new_normalized_email)
    .bind(&change.old_normalized_email)
    .execute(&mut *transaction)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(EmailChangeError::Rejected);
    }
    sqlx::query(
        "UPDATE rust_identity_email_change SET state='completed',completed_at=NOW(),updated_at=NOW() WHERE id=$1 AND state='pending_new'",
    )
    .bind(change.id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(())
}

async fn queue_issued_delivery(
    transaction: &mut Transaction<'_, Postgres>,
    issued: &IssuedOtpChallenge,
    encryption_key: &EncryptionKey,
) -> Result<Uuid, OtpChallengeError> {
    let delivery_id = Uuid::now_v7();
    let ciphertext = encrypt_secret(encryption_key, issued.code.as_bytes(), OTP_DELIVERY_AAD)?;
    sqlx::query(
        "INSERT INTO rust_identity_otp_delivery (id, challenge_id, ciphertext) VALUES ($1, $2, $3)",
    )
    .bind(delivery_id)
    .bind(issued.challenge_id)
    .bind(ciphertext)
    .execute(&mut **transaction)
    .await?;
    let aggregate_id = issued.challenge_id.to_string();
    append(
        transaction,
        NewEvent {
            tenant_id: None,
            aggregate_type: "identity_otp_challenge",
            aggregate_id: &aggregate_id,
            event_type: "identity.otp.delivery.requested",
            schema_version: 1,
            payload: json!({"delivery_id": delivery_id}),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await?;
    Ok(delivery_id)
}

async fn issue_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    email: &str,
    purpose: OtpPurpose,
) -> Result<IssuedOtpChallenge, OtpChallengeError> {
    let recent_sends: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM rust_identity_otp_challenge WHERE normalized_email=$1 AND purpose=$2 AND created_at > NOW() - make_interval(secs => $3)",
    )
    .bind(email)
    .bind(purpose.as_db())
    .bind(RESEND_WINDOW.whole_seconds())
    .fetch_one(&mut **transaction)
    .await?;
    if recent_sends >= MAX_SENDS_PER_WINDOW {
        return Err(OtpChallengeError::RateLimited);
    }
    sqlx::query(
        "UPDATE rust_identity_otp_challenge SET state='expired', updated_at=NOW() WHERE normalized_email=$1 AND purpose=$2 AND state='pending'",
    )
    .bind(email)
    .bind(purpose.as_db())
    .execute(&mut **transaction)
    .await?;
    let IssuedOtpCode { code, hash } = issue_otp_code()?;
    let id = Uuid::now_v7();
    let expires_at = OffsetDateTime::now_utc() + OTP_TTL;
    sqlx::query(
        "INSERT INTO rust_identity_otp_challenge (id, normalized_email, purpose, code_hash, expires_at) VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(email)
    .bind(purpose.as_db())
    .bind(hash)
    .bind(expires_at)
    .execute(&mut **transaction)
    .await?;
    Ok(IssuedOtpChallenge {
        challenge_id: id,
        code,
        expires_at,
    })
}

/// Consumes the current OTP challenge exactly once.
///
/// # Errors
/// Returns `Rejected` for unknown, expired, consumed, locked, malformed, or
/// wrong-code challenges without exposing which condition occurred.
pub async fn consume_otp_challenge(
    pool: &PgPool,
    challenge_id: Uuid,
    code: &str,
) -> Result<ConsumedOtpChallenge, OtpChallengeError> {
    let mut transaction = pool.begin().await?;
    let outcome = consume_in_transaction(&mut transaction, challenge_id, code).await;
    match outcome {
        Ok(facts) => {
            transaction.commit().await?;
            Ok(facts)
        }
        Err(OtpChallengeError::Rejected) => {
            transaction.commit().await?;
            Err(OtpChallengeError::Rejected)
        }
        Err(error) => Err(error),
    }
}

async fn consume_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    challenge_id: Uuid,
    code: &str,
) -> Result<ConsumedOtpChallenge, OtpChallengeError> {
    let row = sqlx::query_as::<_, ChallengeRow>(
        "SELECT id, normalized_email, purpose, code_hash, expires_at, attempts, max_attempts, state FROM rust_identity_otp_challenge WHERE id=$1 FOR UPDATE",
    )
    .bind(challenge_id)
    .fetch_optional(&mut **transaction)
    .await?;
    let Some(row) = row else {
        return Err(OtpChallengeError::Rejected);
    };
    if row.state != "pending" || row.expires_at <= OffsetDateTime::now_utc() {
        if row.state == "pending" {
            sqlx::query("UPDATE rust_identity_otp_challenge SET state='expired', updated_at=NOW() WHERE id=$1")
                .bind(row.id).execute(&mut **transaction).await?;
        }
        return Err(OtpChallengeError::Rejected);
    }
    match verify_otp_code(code, &row.code_hash) {
        Ok(true) => {
            sqlx::query("UPDATE rust_identity_otp_challenge SET state='consumed', consumed_at=NOW(), updated_at=NOW() WHERE id=$1 AND state='pending'")
                .bind(row.id).execute(&mut **transaction).await?;
            let purpose = OtpPurpose::from_db(&row.purpose).ok_or(OtpChallengeError::Rejected)?;
            Ok(ConsumedOtpChallenge {
                normalized_email: row.normalized_email,
                purpose,
            })
        }
        Ok(false) | Err(OtpError::InvalidStoredHash) => {
            let attempts = row.attempts.saturating_add(1);
            let state = if attempts >= row.max_attempts {
                "locked"
            } else {
                "pending"
            };
            sqlx::query("UPDATE rust_identity_otp_challenge SET attempts=$2, state=$3, updated_at=NOW() WHERE id=$1")
                .bind(row.id).bind(attempts).bind(state).execute(&mut **transaction).await?;
            Err(OtpChallengeError::Rejected)
        }
        Err(error) => Err(OtpChallengeError::Otp(error)),
    }
}

async fn cancel_change(
    transaction: &mut Transaction<'_, Postgres>,
    change_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE rust_identity_email_change SET state='cancelled', updated_at=NOW() WHERE id=$1 AND state IN ('pending_current','pending_new')")
        .bind(change_id).execute(&mut **transaction).await?;
    Ok(())
}

fn canonical_email(value: &str) -> Result<String, OtpChallengeError> {
    let email = value.trim().to_ascii_lowercase();
    if email.len() < 3
        || email.len() > 320
        || email.contains(char::is_whitespace)
        || email.matches('@').count() != 1
        || email.starts_with('@')
        || email.ends_with('@')
    {
        return Err(OtpChallengeError::InvalidEmail);
    }
    Ok(email)
}

#[cfg(test)]
mod tests {
    use super::{OtpChallengeError, OtpPurpose, canonical_email};

    #[test]
    fn email_and_purpose_are_strict() {
        assert!(matches!(
            canonical_email(" Person@Example.Test ").as_deref(),
            Ok("person@example.test")
        ));
        assert!(matches!(
            canonical_email("no-at"),
            Err(OtpChallengeError::InvalidEmail)
        ));
        assert_eq!(OtpPurpose::ChangeEmail.as_db(), "change_email");
    }
}
