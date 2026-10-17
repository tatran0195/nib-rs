//! PostgreSQL-backed native identity session lifecycle.
//!
//! All callers must set the raw token only in an `HttpOnly` secure cookie and the
//! CSRF secret only in the corresponding request-bound mechanism. This slice
//! stores only SHA-256 digests and checks live user moderation state on every
//! authentication attempt.

use nibleaf_core::{Actor, UserId};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{
    IdentitySecretError, hash_identity_secret, issue_identity_secret, verify_identity_secret,
};

#[derive(Debug, Error)]
pub enum IdentitySessionError {
    #[error("identity secret generation failed")]
    Secret(#[from] IdentitySecretError),
    #[error("identity persistence failed")]
    Database(#[from] sqlx::Error),
    #[error("identity session lifetime must be between 1 minute and 30 days")]
    InvalidLifetime,
    #[error("identity account is unavailable")]
    AccountUnavailable,
    #[error("active organization is not a membership of this user")]
    InvalidActiveOrganization,
    #[error("identity session is unavailable")]
    Unavailable,
}

/// Raw values returned exactly once at session issuance. Do not log, serialize,
/// or put either value in an outbox payload.
pub struct IssuedSession {
    pub session_id: Uuid,
    pub session_token: String,
    pub csrf_secret: String,
    pub expires_at: OffsetDateTime,
}

impl std::fmt::Debug for IssuedSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IssuedSession")
            .field("session_id", &self.session_id)
            .field("session_token", &"[redacted]")
            .field("csrf_secret", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticatedSession {
    pub session_id: Uuid,
    pub user_id: UserId,
    pub active_organization_id: Option<String>,
    pub impersonated_by: Option<String>,
    pub expires_at: OffsetDateTime,
    csrf_secret_hash: Vec<u8>,
}

#[derive(FromRow)]
struct SessionRow {
    session_id: Uuid,
    user_id: String,
    token_hash: Vec<u8>,
    active_organization_id: Option<String>,
    impersonated_by: Option<String>,
    expires_at: OffsetDateTime,
    csrf_secret_hash: Vec<u8>,
}

const MIN_SESSION_LIFETIME: Duration = Duration::minutes(1);
const MAX_SESSION_LIFETIME: Duration = Duration::days(30);

/// Creates a native session only for a current, unsuspended, unbanned user.
/// When an active organization is supplied it must be an existing membership.
///
/// # Errors
///
/// Returns an unavailable account/session error without exposing account state,
/// an invalid lifetime/organization error for caller-controlled invalid input,
/// or a database/entropy error.
pub async fn issue_session(
    pool: &PgPool,
    user_id: &UserId,
    active_organization_id: Option<&str>,
    impersonated_by: Option<&str>,
    lifetime: Duration,
) -> Result<IssuedSession, IdentitySessionError> {
    if !(MIN_SESSION_LIFETIME..=MAX_SESSION_LIFETIME).contains(&lifetime) {
        return Err(IdentitySessionError::InvalidLifetime);
    }
    let mut transaction = pool.begin().await?;
    let issued = issue_session_in_transaction(
        &mut transaction,
        user_id,
        active_organization_id,
        impersonated_by,
        lifetime,
    )
    .await?;
    transaction.commit().await?;
    Ok(issued)
}

/// Issues a session inside a caller-owned transaction. This is crate-visible so
/// the support-impersonation slice can atomically persist its support-session
/// lifecycle and immutable start audit fact with the session itself.
pub(crate) async fn issue_session_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: &UserId,
    active_organization_id: Option<&str>,
    impersonated_by: Option<&str>,
    lifetime: Duration,
) -> Result<IssuedSession, IdentitySessionError> {
    let account_available: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM \"user\" WHERE id=$1 AND \"suspendedAt\" IS NULL AND banned=FALSE)",
    )
    .bind(user_id.as_str())
    .fetch_one(&mut **transaction)
    .await?;
    if !account_available {
        return Err(IdentitySessionError::AccountUnavailable);
    }
    if let Some(organization_id) = active_organization_id {
        let membership_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM member WHERE \"organizationId\"=$1 AND \"userId\"=$2)",
        )
        .bind(organization_id)
        .bind(user_id.as_str())
        .fetch_one(&mut **transaction)
        .await?;
        if !membership_exists {
            return Err(IdentitySessionError::InvalidActiveOrganization);
        }
    }
    let token = issue_identity_secret()?;
    let csrf = issue_identity_secret()?;
    let id = Uuid::now_v7();
    let expires_at = OffsetDateTime::now_utc() + lifetime;
    sqlx::query(
        r"INSERT INTO rust_identity_session (
            id, user_id, token_hash, csrf_secret_hash, active_organization_id,
            impersonated_by, expires_at
          ) VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(id)
    .bind(user_id.as_str())
    .bind(token.hash)
    .bind(csrf.hash)
    .bind(active_organization_id)
    .bind(impersonated_by)
    .bind(expires_at)
    .execute(&mut **transaction)
    .await?;
    Ok(IssuedSession {
        session_id: id,
        session_token: token.secret,
        csrf_secret: csrf.secret,
        expires_at,
    })
}

/// Resolves a raw opaque session token against a live account. Revoked, expired,
/// banned, suspended, malformed, and unknown sessions deliberately collapse to
/// `Unavailable` so callers cannot build an account/session oracle.
///
/// # Errors
///
/// Returns `Unavailable` for any unauthenticated outcome or persistence failure
/// only where a database error is necessary for operator diagnosis.
const SELECT_LIVE_SESSION: &str = r#"SELECT session.id AS session_id, session.user_id, session.token_hash,
                  session.active_organization_id, session.impersonated_by, session.expires_at,
                  session.csrf_secret_hash
           FROM rust_identity_session AS session
           INNER JOIN "user" AS user_account ON user_account.id = session.user_id
           LEFT JOIN "user" AS impersonator ON impersonator.id = session.impersonated_by
           WHERE session.token_hash=$1 AND session.revoked_at IS NULL AND session.expires_at > NOW()
             AND user_account."suspendedAt" IS NULL AND user_account.banned=FALSE
             AND (
               session.impersonated_by IS NULL
               OR (impersonator.role='admin' AND impersonator."suspendedAt" IS NULL AND impersonator.banned=FALSE)
             )

             AND (session.impersonated_by IS NOT NULL OR NOT EXISTS (
               SELECT 1 FROM rust_identity_impersonation AS support_kind WHERE support_kind.impersonation_session_id=session.id
             ))
             AND (session.impersonated_by IS NULL OR EXISTS (
               SELECT 1 FROM rust_identity_impersonation AS support
               INNER JOIN rust_identity_session AS origin ON origin.id=support.operator_session_id
               WHERE support.impersonation_session_id=session.id
                 AND support.target_user_id=session.user_id
                 AND support.operator_user_id=session.impersonated_by
                 AND support.terminated_at IS NULL AND support.expires_at>NOW()
                 AND origin.user_id=support.operator_user_id AND origin.impersonated_by IS NULL
                 AND origin.revoked_at IS NULL AND origin.expires_at>NOW()
             ))"#;

/// Resolves an opaque credential against live account/session/support facts.
/// # Errors
/// Returns unavailable for invalid authority, or a typed database/secret error.
pub async fn authenticate_session(
    pool: &PgPool,
    candidate_token: &str,
) -> Result<AuthenticatedSession, IdentitySessionError> {
    let candidate_hash = identity_hash(candidate_token)?;
    let statement = format!("{SELECT_LIVE_SESSION} FOR UPDATE OF session");
    let row = sqlx::query_as::<_, SessionRow>(&statement)
        .bind(candidate_hash)
        .fetch_optional(pool)
        .await?;
    verified_session(row, candidate_token)
}

/// Same live account/support/session checks without row locks or writes, on
/// the caller-owned READ ONLY snapshot. This is only identity, never project
/// authorization. Keeping it in identity avoids a search→identity dependency.
///
/// # Errors
/// Invalid/revoked/expired credentials are unavailable; database errors remain typed.
pub async fn authenticate_read_only_actor_on_connection(
    connection: &mut sqlx::PgConnection,
    candidate_token: &str,
) -> Result<Actor, IdentitySessionError> {
    let candidate_hash = identity_hash(candidate_token)?;
    let row = sqlx::query_as::<_, SessionRow>(SELECT_LIVE_SESSION)
        .bind(candidate_hash)
        .fetch_optional(connection)
        .await?;
    let session = verified_session(row, candidate_token)?;
    Ok(Actor {
        user_id: session.user_id,
        session_id: session.session_id.to_string(),
        impersonated_by: session.impersonated_by,
    })
}

fn verified_session(
    row: Option<SessionRow>,
    candidate_token: &str,
) -> Result<AuthenticatedSession, IdentitySessionError> {
    let row = row.ok_or(IdentitySessionError::Unavailable)?;
    if !verify_identity_secret(candidate_token, &row.token_hash)
        .map_err(|_| IdentitySessionError::Unavailable)?
    {
        return Err(IdentitySessionError::Unavailable);
    }
    Ok(AuthenticatedSession {
        session_id: row.session_id,
        user_id: UserId::try_from(row.user_id).map_err(|_| IdentitySessionError::Unavailable)?,
        active_organization_id: row.active_organization_id,
        impersonated_by: row.impersonated_by,
        expires_at: row.expires_at,
        csrf_secret_hash: row.csrf_secret_hash,
    })
}

/// Verifies the request's CSRF secret against the authenticated session.
///
/// # Errors
///
/// Returns `Unavailable` for a malformed/mismatched CSRF secret and a hard
/// error only if persisted digest data is corrupt.
pub fn verify_csrf(
    session: &AuthenticatedSession,
    candidate: &str,
) -> Result<(), IdentitySessionError> {
    match verify_identity_secret(candidate, &session.csrf_secret_hash) {
        Ok(true) => Ok(()),
        Ok(false) | Err(IdentitySecretError::InvalidStoredHash) => {
            Err(IdentitySessionError::Unavailable)
        }
        Err(error) => Err(IdentitySessionError::Secret(error)),
    }
}

/// Rotates and returns a native session's CSRF secret. This recovery primitive
/// is for a browser that still has a valid `HttpOnly` identity cookie but no
/// longer has its one-time in-memory/session-storage CSRF value (for example,
/// a newly opened tab). Its HTTP route must require an exact trusted Origin;
/// ordinary unsafe routes continue to require this returned secret.
///
/// # Errors
/// Returns `Unavailable` if the exact session, its account, or the originating
/// support operator is no longer active. A successful rotation invalidates the
/// prior secret for this session.
pub async fn rotate_csrf_secret(
    pool: &PgPool,
    session: &AuthenticatedSession,
) -> Result<String, IdentitySessionError> {
    let csrf = issue_identity_secret()?;
    let changed = sqlx::query(
        r#"UPDATE rust_identity_session AS session
           SET csrf_secret_hash = $3
           WHERE session.id = $1
             AND session.user_id = $2
             AND session.revoked_at IS NULL
             AND session.expires_at > NOW()
             AND EXISTS (
               SELECT 1
               FROM "user" AS user_account
               WHERE user_account.id = session.user_id
                 AND user_account."suspendedAt" IS NULL
                 AND user_account.banned = FALSE
             )
             AND (
               session.impersonated_by IS NULL
               OR EXISTS (
                 SELECT 1
                 FROM "user" AS impersonator
                 WHERE impersonator.id = session.impersonated_by
                   AND impersonator.role = 'admin'
                   AND impersonator."suspendedAt" IS NULL
                   AND impersonator.banned = FALSE
               )
             )
             AND (session.impersonated_by IS NOT NULL OR NOT EXISTS (
               SELECT 1 FROM rust_identity_impersonation AS support_kind WHERE support_kind.impersonation_session_id=session.id
             ))
             AND (session.impersonated_by IS NULL OR EXISTS (
               SELECT 1 FROM rust_identity_impersonation AS support
               INNER JOIN rust_identity_session AS origin ON origin.id=support.operator_session_id
               WHERE support.impersonation_session_id=session.id
                 AND support.target_user_id=session.user_id
                 AND support.operator_user_id=session.impersonated_by
                 AND support.terminated_at IS NULL AND support.expires_at>NOW()
                 AND origin.user_id=support.operator_user_id AND origin.impersonated_by IS NULL
                 AND origin.revoked_at IS NULL AND origin.expires_at>NOW()
             ))"#,
    )
    .bind(session.session_id)
    .bind(session.user_id.as_str())
    .bind(csrf.hash)
    .execute(pool)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(IdentitySessionError::Unavailable);
    }
    Ok(csrf.secret)
}

/// Revokes exactly one native session. The user scope prevents cross-account
/// session revocation; repeated calls are safe and report whether they changed
/// a live session.
///
/// # Errors
///
/// Returns a persistence error if the revocation fact cannot be recorded.
pub async fn revoke_session(
    pool: &PgPool,
    session_id: Uuid,
    user_id: &UserId,
    reason: &str,
) -> Result<bool, IdentitySessionError> {
    if reason.is_empty() || reason.len() > 120 || reason.chars().any(char::is_control) {
        return Err(IdentitySessionError::Unavailable);
    }
    let result = sqlx::query(
        "UPDATE rust_identity_session SET revoked_at=NOW(), revoke_reason=$3 WHERE id=$1 AND user_id=$2 AND revoked_at IS NULL",
    )
    .bind(session_id)
    .bind(user_id.as_str())
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

fn identity_hash(candidate: &str) -> Result<Vec<u8>, IdentitySessionError> {
    hash_identity_secret(candidate)?.ok_or(IdentitySessionError::Unavailable)
}
