//! Platform-administrator moderation commands and durable audit facts.
//!
//! Moderation is deliberately not tenant-scoped: it operates on global user and
//! project records. Each command therefore verifies the live platform role in
//! the same PostgreSQL transaction as the state transition, session revocation,
//! and immutable audit insert.

use nibleaf_core::{Actor, ProjectId, UserId};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::impersonation::revoke_active_target_impersonations_for_moderation;

const USER_SUSPENSION_REASON: &str = "Suspended by a Nibleaf operator";
const NATIVE_SESSION_REVOCATION_REASON: &str = "moderation_suspension";

#[derive(Debug, Error)]
pub enum ModerationError {
    #[error("platform administrator authorization required")]
    Forbidden,
    #[error("moderation target was not found")]
    NotFound,
    #[error("platform administrators cannot be suspended")]
    AdministratorProtected,
    #[error("moderation reason is invalid")]
    InvalidReason,
    #[error("moderation persistence failed")]
    Database(#[from] sqlx::Error),
}

/// The durable result of one moderation command. `changed=false` means the
/// requested state already held; the request was nevertheless audited.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModerationOutcome {
    pub changed: bool,
}

#[derive(FromRow)]
struct UserModerationRow {
    user_id: String,
    role: String,
    banned: bool,
    suspended_at: Option<time::PrimitiveDateTime>,
}

/// Suspends a regular account, terminalizes any target-bound native support
/// contexts, revokes all native sessions, removes transitional Better Auth
/// sessions, and records the platform operator's action atomically.
///
/// Platform administrators are protected from suspension so the administration
/// boundary cannot be used to remove the final operating path.
///
/// # Errors
///
/// Returns an authorization, protected-administrator, missing-target, or
/// persistence error. All mutations and the audit fact are rolled back together
/// when persistence fails.
pub async fn suspend_user(
    pool: &PgPool,
    actor: &Actor,
    target_user_id: &UserId,
) -> Result<ModerationOutcome, ModerationError> {
    let actor_session_id =
        Uuid::parse_str(&actor.session_id).map_err(|_| ModerationError::Forbidden)?;
    let mut transaction = pool.begin().await?;
    let target = lock_actor_and_target_user(&mut transaction, actor, target_user_id).await?;
    if target.role == "admin" {
        return Err(ModerationError::AdministratorProtected);
    }

    let changed = target.suspended_at.is_none();
    if changed {
        sqlx::query(
            r#"UPDATE "user"
               SET "suspendedAt"=NOW(), banned=TRUE, "banReason"=$2, "banExpires"=NULL
               WHERE id=$1"#,
        )
        .bind(target_user_id.as_str())
        .bind(USER_SUSPENSION_REASON)
        .execute(&mut *transaction)
        .await?;
    }
    // A support session runs as the target account. Lock and terminalize every
    // matching lifecycle row before the broad target-session revocation below,
    // so native moderation, session fencing, and immutable support evidence
    // either commit together or roll back together.
    revoke_active_target_impersonations_for_moderation(
        &mut transaction,
        actor.user_id.as_str(),
        actor_session_id,
        target_user_id,
    )
    .await?;
    if changed {
        sqlx::query(
            "UPDATE rust_identity_session
             SET revoked_at=NOW(), revoke_reason=$2
             WHERE user_id=$1 AND revoked_at IS NULL",
        )
        .bind(target_user_id.as_str())
        .bind(NATIVE_SESSION_REVOCATION_REASON)
        .execute(&mut *transaction)
        .await?;
        // Better Auth remains a bounded migration bridge. Deleting its
        // database-backed sessions prevents a transitional cookie bypassing
        // the native lockout before the bridge is retired.
        sqlx::query("DELETE FROM \"session\" WHERE \"userId\"=$1")
            .bind(target_user_id.as_str())
            .execute(&mut *transaction)
            .await?;
    }
    insert_audit(
        &mut transaction,
        ModerationAuditInsert::user(
            actor,
            "user_suspended",
            target_user_id,
            Some(USER_SUSPENSION_REASON),
            changed,
        ),
    )
    .await?;
    transaction.commit().await?;
    Ok(ModerationOutcome { changed })
}

/// Lifts a user suspension. Existing sessions remain revoked; the account must
/// establish a fresh session after the operator has restored access.
///
/// # Errors
///
/// Returns an authorization, missing-target, or persistence error. All changes
/// and the audit fact are atomic.
pub async fn unsuspend_user(
    pool: &PgPool,
    actor: &Actor,
    target_user_id: &UserId,
) -> Result<ModerationOutcome, ModerationError> {
    let mut transaction = pool.begin().await?;
    let target = lock_actor_and_target_user(&mut transaction, actor, target_user_id).await?;
    let changed = target.suspended_at.is_some();
    if changed {
        sqlx::query(
            r#"UPDATE "user"
               SET "suspendedAt"=NULL, banned=FALSE, "banReason"=NULL, "banExpires"=NULL
               WHERE id=$1"#,
        )
        .bind(target_user_id.as_str())
        .execute(&mut *transaction)
        .await?;
    }
    insert_audit(
        &mut transaction,
        ModerationAuditInsert::user(actor, "user_unsuspended", target_user_id, None, changed),
    )
    .await?;
    transaction.commit().await?;
    Ok(ModerationOutcome { changed })
}

/// Takes a documentation project offline at the public-serving and publishing
/// boundary. The free-form reason is deliberately bounded before persistence.
///
/// # Errors
///
/// Returns an authorization, missing-project, invalid-reason, or persistence
/// error. The project state and audit fact commit atomically.
pub async fn takedown_project(
    pool: &PgPool,
    actor: &Actor,
    target_project_id: &ProjectId,
    reason: Option<&str>,
) -> Result<ModerationOutcome, ModerationError> {
    let reason = normalized_reason(reason)?;
    let mut transaction = pool.begin().await?;
    require_live_platform_administrator(&mut transaction, actor).await?;
    let currently_taken_down: bool =
        sqlx::query_scalar("SELECT \"takedownAt\" IS NOT NULL FROM project WHERE id=$1 FOR UPDATE")
            .bind(target_project_id.as_str())
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(ModerationError::NotFound)?;
    let changed = !currently_taken_down;
    if changed {
        sqlx::query("UPDATE project SET \"takedownAt\"=NOW(), \"takedownReason\"=$2 WHERE id=$1")
            .bind(target_project_id.as_str())
            .bind(&reason)
            .execute(&mut *transaction)
            .await?;
    }
    insert_audit(
        &mut transaction,
        ModerationAuditInsert::project(
            actor,
            "project_taken_down",
            target_project_id,
            reason.as_deref(),
            changed,
        ),
    )
    .await?;
    transaction.commit().await?;
    Ok(ModerationOutcome { changed })
}

/// Restores a project previously taken down by a platform administrator.
///
/// # Errors
///
/// Returns an authorization, missing-project, or persistence error. The project
/// state and audit fact commit atomically.
pub async fn restore_project(
    pool: &PgPool,
    actor: &Actor,
    target_project_id: &ProjectId,
) -> Result<ModerationOutcome, ModerationError> {
    let mut transaction = pool.begin().await?;
    require_live_platform_administrator(&mut transaction, actor).await?;
    let currently_taken_down: bool =
        sqlx::query_scalar("SELECT \"takedownAt\" IS NOT NULL FROM project WHERE id=$1 FOR UPDATE")
            .bind(target_project_id.as_str())
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(ModerationError::NotFound)?;
    if currently_taken_down {
        sqlx::query("UPDATE project SET \"takedownAt\"=NULL, \"takedownReason\"=NULL WHERE id=$1")
            .bind(target_project_id.as_str())
            .execute(&mut *transaction)
            .await?;
    }
    insert_audit(
        &mut transaction,
        ModerationAuditInsert::project(
            actor,
            "project_restored",
            target_project_id,
            None,
            currently_taken_down,
        ),
    )
    .await?;
    transaction.commit().await?;
    Ok(ModerationOutcome {
        changed: currently_taken_down,
    })
}

async fn require_live_platform_administrator(
    transaction: &mut Transaction<'_, Postgres>,
    actor: &Actor,
) -> Result<(), ModerationError> {
    if actor.impersonated_by.is_some() {
        return Err(ModerationError::Forbidden);
    }
    // Lock the authority row rather than merely reading it. A concurrent legacy
    // role demotion or suspension must serialize before/after this command; it
    // must not leave a just-demoted operator able to commit a moderation write.
    let is_administrator: Option<bool> = sqlx::query_scalar(
        r#"SELECT role='admin' AND "suspendedAt" IS NULL AND banned=FALSE
            FROM "user" WHERE id=$1 FOR UPDATE"#,
    )
    .bind(actor.user_id.as_str())
    .fetch_optional(&mut **transaction)
    .await?;
    if is_administrator == Some(true) {
        Ok(())
    } else {
        Err(ModerationError::Forbidden)
    }
}

/// Locks both user rows in lexical identifier order. This avoids a lock-order
/// inversion if two platform administrators concurrently target one another.
async fn lock_actor_and_target_user(
    transaction: &mut Transaction<'_, Postgres>,
    actor: &Actor,
    target_user_id: &UserId,
) -> Result<UserModerationRow, ModerationError> {
    if actor.impersonated_by.is_some() {
        return Err(ModerationError::Forbidden);
    }
    let rows: Vec<UserModerationRow> = sqlx::query_as(
        r#"SELECT id AS user_id, role, banned, "suspendedAt" AS suspended_at
           FROM "user"
           WHERE id = ANY($1)
           ORDER BY id
           FOR UPDATE"#,
    )
    .bind(vec![actor.user_id.as_str(), target_user_id.as_str()])
    .fetch_all(&mut **transaction)
    .await?;
    let actor_is_administrator = rows
        .iter()
        .find(|row| row.user_id == actor.user_id.as_str())
        .is_some_and(|row| row.role == "admin" && row.suspended_at.is_none() && !row.banned);
    if !actor_is_administrator {
        return Err(ModerationError::Forbidden);
    }
    rows.into_iter()
        .find(|row| row.user_id == target_user_id.as_str())
        .ok_or(ModerationError::NotFound)
}

struct ModerationAuditInsert<'a> {
    actor: &'a Actor,
    action: &'a str,
    target_user_id: Option<&'a str>,
    target_project_id: Option<&'a str>,
    reason: Option<&'a str>,
    changed: bool,
}

impl<'a> ModerationAuditInsert<'a> {
    fn user(
        actor: &'a Actor,
        action: &'a str,
        target_user_id: &'a UserId,
        reason: Option<&'a str>,
        changed: bool,
    ) -> Self {
        Self {
            actor,
            action,
            target_user_id: Some(target_user_id.as_str()),
            target_project_id: None,
            reason,
            changed,
        }
    }

    fn project(
        actor: &'a Actor,
        action: &'a str,
        target_project_id: &'a ProjectId,
        reason: Option<&'a str>,
        changed: bool,
    ) -> Self {
        Self {
            actor,
            action,
            target_user_id: None,
            target_project_id: Some(target_project_id.as_str()),
            reason,
            changed,
        }
    }
}

async fn insert_audit(
    transaction: &mut Transaction<'_, Postgres>,
    audit: ModerationAuditInsert<'_>,
) -> Result<(), ModerationError> {
    sqlx::query(
        "INSERT INTO rust_identity_moderation_audit
         (id, actor_user_id, actor_session_id, action, target_user_id, target_project_id, reason, changed)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(Uuid::now_v7())
    .bind(audit.actor.user_id.as_str())
    .bind(&audit.actor.session_id)
    .bind(audit.action)
    .bind(audit.target_user_id)
    .bind(audit.target_project_id)
    .bind(audit.reason)
    .bind(audit.changed)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn normalized_reason(reason: Option<&str>) -> Result<Option<String>, ModerationError> {
    let Some(reason) = reason else {
        return Ok(None);
    };
    let reason = reason.trim();
    if reason.is_empty() {
        return Ok(None);
    }
    if reason.chars().count() > 500 || reason.chars().any(char::is_control) {
        return Err(ModerationError::InvalidReason);
    }
    Ok(Some(reason.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::{ModerationError, normalized_reason};

    #[test]
    fn normalizes_optional_takedown_reasons() -> Result<(), ModerationError> {
        assert_eq!(normalized_reason(None)?, None);
        assert_eq!(normalized_reason(Some("  "))?, None);
        assert_eq!(
            normalized_reason(Some(" Policy breach "))?,
            Some("Policy breach".to_owned())
        );
        Ok(())
    }

    #[test]
    fn rejects_unsafe_or_oversized_takedown_reason() {
        assert!(matches!(
            normalized_reason(Some("line one\nline two")),
            Err(ModerationError::InvalidReason)
        ));
        assert!(matches!(
            normalized_reason(Some(&"a".repeat(501))),
            Err(ModerationError::InvalidReason)
        ));
    }
}
