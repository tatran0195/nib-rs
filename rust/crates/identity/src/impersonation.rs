//! Bounded native support impersonation with durable lifecycle and audit facts.
//!
//! A support session is always a distinct one-hour native session for the target
//! account. The initiating operator session remains server-valid but is held in
//! a path-scoped `HttpOnly` browser cookie until support access ends. This lets an
//! operator leave safely without making the operator token ambient to customer
//! feature requests.

use nibleaf_core::{Actor, OrganizationId, UserId};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::session::issue_session_in_transaction;
use crate::{AuthenticatedSession, IdentitySessionError, IssuedSession};

const IMPERSONATION_LIFETIME: Duration = Duration::hours(1);
const END_REASON: &str = "impersonation_ended";
const REVOKE_REASON: &str = "impersonation_revoked";
const EXPIRE_REASON: &str = "impersonation_expired";
const MODERATION_SUSPENSION_REVOKE_REASON: &str = "moderation_suspension";
const EXPIRY_RECONCILER_ACTOR: &str = "system:identity-impersonation-expirer";
/// Largest number of due support contexts a single reconciliation transaction
/// may lock and terminalize. The worker always uses this ceiling.
pub const MAX_EXPIRY_RECONCILIATION_BATCH_SIZE: i64 = 100;

#[derive(Debug, Error)]
pub enum ImpersonationError {
    #[error("platform administrator authorization required")]
    Forbidden,
    #[error("support impersonation target was not found")]
    NotFound,
    #[error("a user cannot impersonate themself")]
    SelfImpersonation,
    #[error("support impersonation active-organization selection is invalid")]
    InvalidActiveOrganization,
    #[error("support impersonation persistence failed")]
    Database(#[from] sqlx::Error),
    #[error("support impersonation session creation failed")]
    Session(#[from] IdentitySessionError),
    #[error("support expiry reconciliation batch size must be between 1 and 100")]
    InvalidExpiryReconciliationBatchSize,
}

/// Opaque values emitted exactly once after a support session is created.
pub struct StartedImpersonation {
    pub impersonation_id: Uuid,
    pub issued_session: IssuedSession,
}

impl std::fmt::Debug for StartedImpersonation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StartedImpersonation")
            .field("impersonation_id", &self.impersonation_id)
            .field("issued_session", &self.issued_session)
            .finish()
    }
}

/// Result of ending or emergency-revoking a support session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImpersonationOutcome {
    pub changed: bool,
}

#[derive(FromRow)]
struct UserRow {
    user_id: String,
    role: String,
    banned: bool,
    suspended_at: Option<time::PrimitiveDateTime>,
}

#[derive(FromRow)]
struct ImpersonationRow {
    id: Uuid,
    operator_user_id: String,
    operator_session_id: Uuid,
    target_user_id: String,
    impersonation_session_id: Uuid,
    expires_at: OffsetDateTime,
}

/// Starts a one-hour native support session for `target_user_id`.
///
/// The caller must be a direct, live platform administrator. Support sessions
/// cannot recursively operate the platform boundary, and a target account must
/// currently be available for normal native authentication.
///
/// # Errors
///
/// Returns authorization, target, active-organization, self-impersonation,
/// session-creation, or persistence errors. The support session, lifecycle row,
/// and immutable `started` audit fact commit in the same transaction.
pub async fn start_impersonation(
    pool: &PgPool,
    actor: &Actor,
    target_user_id: &UserId,
    active_organization_id: Option<&OrganizationId>,
) -> Result<StartedImpersonation, ImpersonationError> {
    if actor.impersonated_by.is_some() {
        return Err(ImpersonationError::Forbidden);
    }
    if actor.user_id == *target_user_id {
        return Err(ImpersonationError::SelfImpersonation);
    }
    let operator_session_id = parse_session_id(&actor.session_id)?;
    let mut transaction = pool.begin().await?;
    let target = lock_operator_and_target(&mut transaction, actor, target_user_id).await?;
    if target.suspended_at.is_some() || target.banned {
        return Err(ImpersonationError::NotFound);
    }
    ensure_current_operator_session(&mut transaction, actor, operator_session_id).await?;
    expire_prior_operator_impersonations(&mut transaction, operator_session_id).await?;

    let issued_session = issue_session_in_transaction(
        &mut transaction,
        target_user_id,
        active_organization_id.map(OrganizationId::as_str),
        Some(actor.user_id.as_str()),
        IMPERSONATION_LIFETIME,
    )
    .await
    .map_err(map_start_session_error)?;
    let impersonation_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO rust_identity_impersonation
         (id, operator_user_id, operator_session_id, target_user_id, impersonation_session_id, expires_at)
         VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(impersonation_id)
    .bind(actor.user_id.as_str())
    .bind(operator_session_id)
    .bind(target_user_id.as_str())
    .bind(issued_session.session_id)
    .bind(issued_session.expires_at)
    .execute(&mut *transaction)
    .await?;
    insert_audit(
        &mut transaction,
        ImpersonationAuditInsert {
            impersonation_id,
            action: "started",
            actor_user_id: actor.user_id.as_str(),
            actor_session_id: Some(operator_session_id),
            actor_kind: "user",
            target_user_id: target_user_id.as_str(),
            impersonation_session_id: issued_session.session_id,
        },
    )
    .await?;
    transaction.commit().await?;
    Ok(StartedImpersonation {
        impersonation_id,
        issued_session,
    })
}

/// Ends the active support context belonging to the supplied direct operator
/// session. The result is idempotent so a browser can always recover its
/// operator cookie after another administrator already revoked the support
/// session.
///
/// # Errors
///
/// Returns a persistence error when the lifecycle cannot be fenced. An invalid
/// resume session is rejected by HTTP admission before this command is called.
pub async fn end_impersonation(
    pool: &PgPool,
    operator_session: &AuthenticatedSession,
) -> Result<ImpersonationOutcome, ImpersonationError> {
    if operator_session.impersonated_by.is_some() {
        return Err(ImpersonationError::Forbidden);
    }
    let mut transaction = pool.begin().await?;
    ensure_current_resume_session(&mut transaction, operator_session).await?;
    let row =
        find_active_operator_impersonation(&mut transaction, operator_session.session_id).await?;
    let Some(row) = row else {
        transaction.commit().await?;
        return Ok(ImpersonationOutcome { changed: false });
    };
    if row.expires_at <= OffsetDateTime::now_utc() {
        expire_impersonation(&mut transaction, &row, TerminationActor::system()).await?;
        transaction.commit().await?;
        return Ok(ImpersonationOutcome { changed: false });
    }
    let changed = terminate_impersonation(
        &mut transaction,
        &row,
        TerminationActor::user(
            operator_session.user_id.as_str(),
            operator_session.session_id,
        ),
        "ended",
        END_REASON,
    )
    .await?;
    transaction.commit().await?;
    Ok(ImpersonationOutcome { changed })
}

/// Ends support access when the currently authenticated target support session
/// signs out through the generic native sign-out endpoint.
///
/// # Errors
///
/// Returns a persistence error if the lifecycle/audit termination cannot commit.
/// A normal non-impersonated session is a no-op for this helper.
pub async fn end_impersonation_from_support_session(
    pool: &PgPool,
    support_session: &AuthenticatedSession,
) -> Result<ImpersonationOutcome, ImpersonationError> {
    let Some(operator_user_id) = support_session.impersonated_by.as_deref() else {
        return Ok(ImpersonationOutcome { changed: false });
    };
    let mut transaction = pool.begin().await?;
    let row = sqlx::query_as::<_, ImpersonationRow>(
        "SELECT id, operator_user_id, operator_session_id, target_user_id, impersonation_session_id, expires_at
         FROM rust_identity_impersonation
         WHERE impersonation_session_id=$1 AND terminated_at IS NULL
         FOR UPDATE",
    )
    .bind(support_session.session_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(row) = row else {
        transaction.commit().await?;
        return Ok(ImpersonationOutcome { changed: false });
    };
    if row.target_user_id != support_session.user_id.as_str()
        || row.operator_user_id != operator_user_id
    {
        return Err(ImpersonationError::Forbidden);
    }
    if row.expires_at <= OffsetDateTime::now_utc() {
        expire_impersonation(&mut transaction, &row, TerminationActor::system()).await?;
        transaction.commit().await?;
        return Ok(ImpersonationOutcome { changed: false });
    }
    let changed = terminate_impersonation(
        &mut transaction,
        &row,
        TerminationActor::user(&row.operator_user_id, row.operator_session_id),
        "ended",
        END_REASON,
    )
    .await?;
    transaction.commit().await?;
    Ok(ImpersonationOutcome { changed })
}

/// Revokes a support context as a direct live platform administrator.
///
/// # Errors
///
/// Returns authorization, missing-target, or persistence errors. A previously
/// terminated context returns `changed=false` and does not rewrite its audit
/// history.
pub async fn revoke_impersonation(
    pool: &PgPool,
    actor: &Actor,
    impersonation_id: Uuid,
) -> Result<ImpersonationOutcome, ImpersonationError> {
    if actor.impersonated_by.is_some() {
        return Err(ImpersonationError::Forbidden);
    }
    let actor_session_id = parse_session_id(&actor.session_id)?;
    let mut transaction = pool.begin().await?;
    require_live_platform_administrator(&mut transaction, actor, actor_session_id).await?;
    let row = sqlx::query_as::<_, ImpersonationRow>(
        "SELECT id, operator_user_id, operator_session_id, target_user_id, impersonation_session_id, expires_at
         FROM rust_identity_impersonation WHERE id=$1 AND terminated_at IS NULL FOR UPDATE",
    )
    .bind(impersonation_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(row) = row else {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM rust_identity_impersonation WHERE id=$1)",
        )
        .bind(impersonation_id)
        .fetch_one(&mut *transaction)
        .await?;
        if exists {
            transaction.commit().await?;
            return Ok(ImpersonationOutcome { changed: false });
        }
        return Err(ImpersonationError::NotFound);
    };
    if row.expires_at <= OffsetDateTime::now_utc() {
        expire_impersonation(&mut transaction, &row, TerminationActor::system()).await?;
        transaction.commit().await?;
        return Ok(ImpersonationOutcome { changed: false });
    }
    let changed = terminate_impersonation(
        &mut transaction,
        &row,
        TerminationActor::user(actor.user_id.as_str(), actor_session_id),
        "revoked",
        REVOKE_REASON,
    )
    .await?;
    transaction.commit().await?;
    Ok(ImpersonationOutcome { changed })
}

fn parse_session_id(value: &str) -> Result<Uuid, ImpersonationError> {
    Uuid::parse_str(value).map_err(|_| ImpersonationError::Forbidden)
}

fn map_start_session_error(error: IdentitySessionError) -> ImpersonationError {
    match error {
        IdentitySessionError::AccountUnavailable => ImpersonationError::NotFound,
        IdentitySessionError::InvalidActiveOrganization => {
            ImpersonationError::InvalidActiveOrganization
        }
        other => ImpersonationError::Session(other),
    }
}

async fn lock_operator_and_target(
    transaction: &mut Transaction<'_, Postgres>,
    actor: &Actor,
    target_user_id: &UserId,
) -> Result<UserRow, ImpersonationError> {
    let rows: Vec<UserRow> = sqlx::query_as(
        r#"SELECT id AS user_id, role, banned, "suspendedAt" AS suspended_at
           FROM "user" WHERE id = ANY($1) ORDER BY id FOR UPDATE"#,
    )
    .bind(vec![actor.user_id.as_str(), target_user_id.as_str()])
    .fetch_all(&mut **transaction)
    .await?;
    let operator_is_administrator = rows
        .iter()
        .find(|row| row.user_id == actor.user_id.as_str())
        .is_some_and(|row| row.role == "admin" && !row.banned && row.suspended_at.is_none());
    if !operator_is_administrator {
        return Err(ImpersonationError::Forbidden);
    }
    rows.into_iter()
        .find(|row| row.user_id == target_user_id.as_str())
        .ok_or(ImpersonationError::NotFound)
}

async fn ensure_current_operator_session(
    transaction: &mut Transaction<'_, Postgres>,
    actor: &Actor,
    session_id: Uuid,
) -> Result<(), ImpersonationError> {
    let valid: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM rust_identity_session
           WHERE id=$1 AND user_id=$2 AND impersonated_by IS NULL
             AND revoked_at IS NULL AND expires_at > NOW()
           FOR UPDATE
         )",
    )
    .bind(session_id)
    .bind(actor.user_id.as_str())
    .fetch_one(&mut **transaction)
    .await?;
    if valid {
        Ok(())
    } else {
        Err(ImpersonationError::Forbidden)
    }
}

async fn ensure_current_resume_session(
    transaction: &mut Transaction<'_, Postgres>,
    session: &AuthenticatedSession,
) -> Result<(), ImpersonationError> {
    let valid: bool = sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM rust_identity_session
           WHERE id=$1 AND user_id=$2 AND impersonated_by IS NULL
             AND revoked_at IS NULL AND expires_at > NOW()
           FOR UPDATE
         )",
    )
    .bind(session.session_id)
    .bind(session.user_id.as_str())
    .fetch_one(&mut **transaction)
    .await?;
    if valid {
        Ok(())
    } else {
        Err(ImpersonationError::Forbidden)
    }
}

async fn require_live_platform_administrator(
    transaction: &mut Transaction<'_, Postgres>,
    actor: &Actor,
    actor_session_id: Uuid,
) -> Result<(), ImpersonationError> {
    let valid: Option<bool> = sqlx::query_scalar(
        r#"SELECT role='admin' AND "suspendedAt" IS NULL AND banned=FALSE
            FROM "user" WHERE id=$1 FOR UPDATE"#,
    )
    .bind(actor.user_id.as_str())
    .fetch_optional(&mut **transaction)
    .await?;
    if valid != Some(true) {
        return Err(ImpersonationError::Forbidden);
    }
    ensure_current_operator_session(transaction, actor, actor_session_id).await
}

async fn expire_prior_operator_impersonations(
    transaction: &mut Transaction<'_, Postgres>,
    operator_session_id: Uuid,
) -> Result<(), ImpersonationError> {
    let rows: Vec<ImpersonationRow> = sqlx::query_as(
        "SELECT id, operator_user_id, operator_session_id, target_user_id, impersonation_session_id, expires_at
         FROM rust_identity_impersonation
         WHERE operator_session_id=$1 AND terminated_at IS NULL AND expires_at <= NOW()
         FOR UPDATE",
    )
    .bind(operator_session_id)
    .fetch_all(&mut **transaction)
    .await?;
    for row in rows {
        expire_impersonation(transaction, &row, TerminationActor::system()).await?;
    }
    Ok(())
}

async fn find_active_operator_impersonation(
    transaction: &mut Transaction<'_, Postgres>,
    operator_session_id: Uuid,
) -> Result<Option<ImpersonationRow>, ImpersonationError> {
    sqlx::query_as(
        "SELECT id, operator_user_id, operator_session_id, target_user_id, impersonation_session_id, expires_at
         FROM rust_identity_impersonation
         WHERE operator_session_id=$1 AND terminated_at IS NULL
         FOR UPDATE",
    )
    .bind(operator_session_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(ImpersonationError::from)
}

/// Terminalizes up to `batch_size` due support contexts under row locks.
///
/// Expiry is recorded as a system action rather than attributing a time-based
/// transition to the next human who happens to encounter it. Rows are selected
/// in deterministic expiry order with `SKIP LOCKED`, so independently running
/// workers make bounded progress without waiting for one another or writing
/// duplicate terminal audit facts.
///
/// # Errors
///
/// Returns an error if `batch_size` is not in `1..=100` or if PostgreSQL cannot
/// lock, terminalize, audit, and commit the selected lifecycle rows atomically.
pub async fn expire_due_impersonations(
    pool: &PgPool,
    batch_size: i64,
) -> Result<usize, ImpersonationError> {
    if !(1..=MAX_EXPIRY_RECONCILIATION_BATCH_SIZE).contains(&batch_size) {
        return Err(ImpersonationError::InvalidExpiryReconciliationBatchSize);
    }
    let mut transaction = pool.begin().await?;
    let rows: Vec<ImpersonationRow> = sqlx::query_as(
        "SELECT id, operator_user_id, operator_session_id, target_user_id, impersonation_session_id, expires_at
         FROM rust_identity_impersonation
         WHERE terminated_at IS NULL AND expires_at <= NOW()
         ORDER BY expires_at ASC, id ASC
         LIMIT $1
         FOR UPDATE SKIP LOCKED",
    )
    .bind(batch_size)
    .fetch_all(&mut *transaction)
    .await?;
    let mut expired = 0;
    for row in rows {
        if expire_impersonation(&mut transaction, &row, TerminationActor::system()).await? {
            expired += 1;
        }
    }
    transaction.commit().await?;
    Ok(expired)
}

/// Terminalizes live support contexts for an account that native moderation has
/// suspended. The moderation caller already holds the target account row lock,
/// and this helper adds lifecycle row locks before revoking their target
/// sessions. This keeps user suspension, support-session revocation, and both
/// immutable audit streams in one PostgreSQL transaction.
pub(crate) async fn revoke_active_target_impersonations_for_moderation(
    transaction: &mut Transaction<'_, Postgres>,
    actor_user_id: &str,
    actor_session_id: Uuid,
    target_user_id: &UserId,
) -> Result<(), sqlx::Error> {
    let rows: Vec<ImpersonationRow> = sqlx::query_as(
        "SELECT id, operator_user_id, operator_session_id, target_user_id, impersonation_session_id, expires_at
         FROM rust_identity_impersonation
         WHERE target_user_id=$1 AND terminated_at IS NULL
         ORDER BY id ASC
         FOR UPDATE",
    )
    .bind(target_user_id.as_str())
    .fetch_all(&mut **transaction)
    .await?;
    let now = OffsetDateTime::now_utc();
    for row in rows {
        if row.expires_at <= now {
            expire_impersonation(transaction, &row, TerminationActor::system()).await?;
        } else {
            terminate_impersonation(
                transaction,
                &row,
                TerminationActor::user(actor_user_id, actor_session_id),
                "revoked",
                MODERATION_SUSPENSION_REVOKE_REASON,
            )
            .await?;
        }
    }
    Ok(())
}

async fn expire_impersonation(
    transaction: &mut Transaction<'_, Postgres>,
    row: &ImpersonationRow,
    actor: TerminationActor<'_>,
) -> Result<bool, sqlx::Error> {
    terminate_impersonation(transaction, row, actor, "expired", EXPIRE_REASON).await
}

async fn terminate_impersonation(
    transaction: &mut Transaction<'_, Postgres>,
    row: &ImpersonationRow,
    actor: TerminationActor<'_>,
    action: &'static str,
    revoke_reason: &'static str,
) -> Result<bool, sqlx::Error> {
    // The lifecycle update is the terminal fence. Do it before appending the
    // immutable audit row so an unexpected caller or future path cannot create
    // a duplicate terminal fact after another transaction won the transition.
    let changed = sqlx::query(
        "UPDATE rust_identity_impersonation
         SET terminated_at=NOW(), terminated_by_user_id=$2,
             terminated_by_session_id=$3, termination_reason=$4
         WHERE id=$1 AND terminated_at IS NULL",
    )
    .bind(row.id)
    .bind(actor.user_id)
    .bind(actor.session_id)
    .bind(action)
    .execute(&mut **transaction)
    .await?
    .rows_affected()
        == 1;
    if !changed {
        return Ok(false);
    }
    sqlx::query(
        "UPDATE rust_identity_session
         SET revoked_at=NOW(), revoke_reason=$2
         WHERE id=$1 AND revoked_at IS NULL",
    )
    .bind(row.impersonation_session_id)
    .bind(revoke_reason)
    .execute(&mut **transaction)
    .await?;
    insert_audit(
        transaction,
        ImpersonationAuditInsert {
            impersonation_id: row.id,
            action,
            actor_user_id: actor.user_id,
            actor_session_id: actor.session_id,
            actor_kind: actor.kind,
            target_user_id: &row.target_user_id,
            impersonation_session_id: row.impersonation_session_id,
        },
    )
    .await?;
    Ok(true)
}

#[derive(Clone, Copy)]
struct TerminationActor<'a> {
    user_id: &'a str,
    session_id: Option<Uuid>,
    kind: &'static str,
}

impl<'a> TerminationActor<'a> {
    const fn user(user_id: &'a str, session_id: Uuid) -> Self {
        Self {
            user_id,
            session_id: Some(session_id),
            kind: "user",
        }
    }

    const fn system() -> Self {
        Self {
            user_id: EXPIRY_RECONCILER_ACTOR,
            session_id: None,
            kind: "system",
        }
    }
}

struct ImpersonationAuditInsert<'a> {
    impersonation_id: Uuid,
    action: &'a str,
    actor_user_id: &'a str,
    actor_session_id: Option<Uuid>,
    actor_kind: &'static str,
    target_user_id: &'a str,
    impersonation_session_id: Uuid,
}

async fn insert_audit(
    transaction: &mut Transaction<'_, Postgres>,
    audit: ImpersonationAuditInsert<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO rust_identity_impersonation_audit
         (id, impersonation_id, action, actor_user_id, actor_session_id, actor_kind, target_user_id, impersonation_session_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(Uuid::now_v7())
    .bind(audit.impersonation_id)
    .bind(audit.action)
    .bind(audit.actor_user_id)
    .bind(audit.actor_session_id)
    .bind(audit.actor_kind)
    .bind(audit.target_user_id)
    .bind(audit.impersonation_session_id)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::IMPERSONATION_LIFETIME;

    #[test]
    fn support_session_lifetime_is_exactly_one_hour() {
        assert_eq!(IMPERSONATION_LIFETIME, time::Duration::hours(1));
    }
}
