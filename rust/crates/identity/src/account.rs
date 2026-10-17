//! Native account resolution and durable first-workspace provisioning intent.
//!
//! Identity owns account creation because the verified OTP is its authority;
//! project/workspace realization remains a separate worker-owned feature action.

use nibleaf_core::UserId;
use nibleaf_outbox::{NewEvent, append};
use serde_json::json;
use sqlx::PgPool;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum AccountResolutionError {
    #[error("account persistence failed")]
    Database(#[from] sqlx::Error),
    #[error("account provisioning intent could not be persisted")]
    Outbox(#[from] nibleaf_core::AppError),
    #[error("new account sign-up is disabled")]
    SignupDisabled,
    #[error("account name is invalid")]
    InvalidName,
}

/// Resolves an existing canonical-email account or atomically creates one with
/// a platform-scoped provisioning intent. An OTP has already proven control of
/// `normalized_email` before this function may be called.
///
/// An existing account never has its display name changed by sign-in. A newly
/// created account has no Better Auth session/account rows; native sessions are
/// intentionally isolated in `rust_identity_session`.
///
/// # Errors
/// Returns `SignupDisabled` without creating an account when no matching user
/// exists and sign-up is disabled. Persistence errors roll back both new user
/// and provisioning outbox intent together.
pub async fn resolve_or_create_otp_account(
    pool: &PgPool,
    normalized_email: &str,
    requested_name: Option<&str>,
    disable_signup: bool,
) -> Result<UserId, AccountResolutionError> {
    let mut transaction = pool.begin().await?;
    let existing: Option<String> =
        sqlx::query_scalar("SELECT id FROM \"user\" WHERE lower(email) = $1 FOR UPDATE")
            .bind(normalized_email)
            .fetch_optional(&mut *transaction)
            .await?;
    if let Some(id) = existing {
        transaction.commit().await?;
        return UserId::try_from(id).map_err(|_| AccountResolutionError::InvalidName);
    }
    if disable_signup {
        return Err(AccountResolutionError::SignupDisabled);
    }
    let name = account_name(normalized_email, requested_name)?;
    let user_id = UserId::new();
    sqlx::query(
        r#"INSERT INTO "user" (
              id, name, email, "emailVerified", role, banned, "createdAt", "updatedAt"
           ) VALUES ($1, $2, $3, TRUE, 'user', FALSE, NOW(), NOW())"#,
    )
    .bind(user_id.as_str())
    .bind(name)
    .bind(normalized_email)
    .execute(&mut *transaction)
    .await?;
    append(
        &mut transaction,
        NewEvent {
            tenant_id: None,
            aggregate_type: "identity_account",
            aggregate_id: user_id.as_str(),
            event_type: "identity.account.provision_requested",
            schema_version: 1,
            payload: json!({"user_id": user_id.as_str()}),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await?;
    transaction.commit().await?;
    Ok(user_id)
}

fn account_name(
    email: &str,
    requested_name: Option<&str>,
) -> Result<String, AccountResolutionError> {
    let candidate = requested_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map_or_else(
            || {
                email
                    .split_once('@')
                    .map_or_else(|| "Nibleaf user".to_owned(), |(local, _)| local.to_owned())
            },
            ToOwned::to_owned,
        );
    if candidate.chars().count() > 120 || candidate.chars().any(char::is_control) {
        return Err(AccountResolutionError::InvalidName);
    }
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::{AccountResolutionError, account_name};

    #[test]
    fn new_account_name_is_bounded_and_never_empty() {
        assert!(matches!(
            account_name("alex@example.test", None).as_deref(),
            Ok("alex")
        ));
        assert!(matches!(
            account_name("alex@example.test", Some("  Alex  ")).as_deref(),
            Ok("Alex")
        ));
        assert!(matches!(
            account_name("x@example.test", Some("\u{0000}")),
            Err(AccountResolutionError::InvalidName)
        ));
    }
}
