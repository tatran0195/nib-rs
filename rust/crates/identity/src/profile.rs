//! Native identity profile and active-workspace compatibility primitives.
//!
//! This slice deliberately owns only the self-service identity fields that the
//! existing dashboard reads or changes (`name`, `email`, verification state,
//! and the active workspace context). It does not turn an active workspace into
//! an authorization grant: feature slices must continue to authorize from the
//! object and live membership they operate on.

use nibleaf_core::OrganizationId;
use sqlx::{FromRow, PgPool};
use thiserror::Error;

use crate::AuthenticatedSession;

#[derive(Debug, Error)]
pub enum IdentityProfileError {
    #[error("identity profile persistence failed")]
    Database(#[from] sqlx::Error),
    #[error("profile name is invalid")]
    InvalidName,
    #[error("identity session is unavailable")]
    Unavailable,
}

/// The current self-service profile together with a presentation-only active
/// workspace. `active_organization_id` is always a live membership when it is
/// present: a stale stored session preference falls back to the first live
/// membership, matching the existing dashboard middleware behavior.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentityProfile {
    pub user_id: String,
    pub name: String,
    pub email: String,
    pub email_verified: bool,
    pub active_organization_id: Option<String>,
}

#[derive(FromRow)]
struct ProfileRow {
    user_id: String,
    name: String,
    email: String,
    email_verified: bool,
    active_organization_id: Option<String>,
}

/// Reads the self-service profile for a session already authenticated by the
/// native session slice. The query repeats the current-session/account fence so
/// a concurrent revocation, suspension, ban, or expiry cannot be presented as a
/// successful profile read.
///
/// # Errors
/// Returns `Unavailable` when the session/account is no longer usable and a
/// database error when the profile cannot be read.
pub async fn current_profile(
    pool: &PgPool,
    session: &AuthenticatedSession,
) -> Result<IdentityProfile, IdentityProfileError> {
    let row = sqlx::query_as::<_, ProfileRow>(
        r#"SELECT user_account.id AS user_id,
                  user_account.name,
                  user_account.email,
                  user_account."emailVerified" AS email_verified,
                  COALESCE(
                    (
                      SELECT selected_member."organizationId"
                      FROM member AS selected_member
                      WHERE selected_member."userId" = session.user_id
                        AND selected_member."organizationId" = session.active_organization_id
                      LIMIT 1
                    ),
                    (
                      SELECT fallback_member."organizationId"
                      FROM member AS fallback_member
                      WHERE fallback_member."userId" = session.user_id
                      ORDER BY fallback_member."createdAt" ASC, fallback_member.id ASC
                      LIMIT 1
                    )
                  ) AS active_organization_id
           FROM rust_identity_session AS session
           INNER JOIN "user" AS user_account ON user_account.id = session.user_id
           WHERE session.id = $1
             AND session.user_id = $2
             AND session.revoked_at IS NULL
             AND session.expires_at > NOW()
             AND user_account."suspendedAt" IS NULL
             AND user_account.banned = FALSE
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
             )"#,
    )
    .bind(session.session_id)
    .bind(session.user_id.as_str())
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Err(IdentityProfileError::Unavailable);
    };
    Ok(IdentityProfile {
        user_id: row.user_id,
        name: row.name,
        email: row.email,
        email_verified: row.email_verified,
        active_organization_id: row.active_organization_id,
    })
}

/// Updates the authenticated person's display name. The same SQL statement
/// verifies the exact native session and current account state, so a stale
/// `Actor` extension cannot mutate a profile after revocation or moderation.
///
/// # Errors
/// Returns `InvalidName` for blank, control-containing, or overlong input;
/// returns `Unavailable` if the session/account is no longer live.
pub async fn update_profile_name(
    pool: &PgPool,
    session: &AuthenticatedSession,
    requested_name: &str,
) -> Result<IdentityProfile, IdentityProfileError> {
    let name = validated_profile_name(requested_name)?;
    let changed = sqlx::query(
        r#"UPDATE "user" AS user_account
           SET name = $3, "updatedAt" = NOW()
           WHERE user_account.id = $1
             AND user_account."suspendedAt" IS NULL
             AND user_account.banned = FALSE
             AND EXISTS (
               SELECT 1
               FROM rust_identity_session AS session
               WHERE session.id = $2
                 AND session.user_id = user_account.id
                 AND session.revoked_at IS NULL
                 AND session.expires_at > NOW()
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
             )"#,
    )
    .bind(session.user_id.as_str())
    .bind(session.session_id)
    .bind(name)
    .execute(pool)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(IdentityProfileError::Unavailable);
    }
    current_profile(pool, session).await
}

/// Persists one active-workspace preference only when the requested workspace
/// is a current membership of the session's user. This compatibility preference
/// never supplies authorization to feature routes.
///
/// # Errors
/// Returns `InvalidActiveOrganization` for a workspace outside the user's live
/// memberships, and `Unavailable` when the session/account can no longer act.
pub async fn select_active_organization(
    pool: &PgPool,
    session: &AuthenticatedSession,
    organization_id: &OrganizationId,
) -> Result<IdentityProfile, crate::IdentitySessionError> {
    let is_membership: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM member WHERE \"organizationId\" = $1 AND \"userId\" = $2)",
    )
    .bind(organization_id.as_str())
    .bind(session.user_id.as_str())
    .fetch_one(pool)
    .await?;
    if !is_membership {
        return Err(crate::IdentitySessionError::InvalidActiveOrganization);
    }

    // Recheck the membership in the update itself. The initial check keeps the
    // caller-visible invalid-workspace validation, while this predicate fences
    // a concurrent membership removal before a preference can be persisted.
    let changed = sqlx::query(
        r#"UPDATE rust_identity_session AS session
           SET active_organization_id = $3
           WHERE session.id = $1
             AND session.user_id = $2
             AND session.revoked_at IS NULL
             AND session.expires_at > NOW()
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
             AND EXISTS (
               SELECT 1
               FROM "user" AS user_account
               WHERE user_account.id = session.user_id
                 AND user_account."suspendedAt" IS NULL
                 AND user_account.banned = FALSE
             )
             AND EXISTS (
               SELECT 1
               FROM member
               WHERE "organizationId" = $3 AND "userId" = session.user_id
             )"#,
    )
    .bind(session.session_id)
    .bind(session.user_id.as_str())
    .bind(organization_id.as_str())
    .execute(pool)
    .await?;
    if changed.rows_affected() != 1 {
        return Err(crate::IdentitySessionError::Unavailable);
    }
    current_profile(pool, session)
        .await
        .map_err(|error| match error {
            IdentityProfileError::Database(error) => crate::IdentitySessionError::Database(error),
            IdentityProfileError::InvalidName | IdentityProfileError::Unavailable => {
                crate::IdentitySessionError::Unavailable
            }
        })
}

fn validated_profile_name(requested_name: &str) -> Result<String, IdentityProfileError> {
    let name = requested_name.trim();
    if name.is_empty() || name.chars().count() > 120 || name.chars().any(char::is_control) {
        return Err(IdentityProfileError::InvalidName);
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{IdentityProfileError, validated_profile_name};

    #[test]
    fn profile_name_is_trimmed_bounded_and_nonempty() {
        assert!(matches!(
            validated_profile_name("  Alex  ").as_deref(),
            Ok("Alex")
        ));
        assert!(matches!(
            validated_profile_name("  \t "),
            Err(IdentityProfileError::InvalidName)
        ));
        assert!(matches!(
            validated_profile_name("A\u{0000}"),
            Err(IdentityProfileError::InvalidName)
        ));
        assert!(matches!(
            validated_profile_name(&"x".repeat(121)),
            Err(IdentityProfileError::InvalidName)
        ));
    }
}
