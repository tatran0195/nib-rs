#![forbid(unsafe_code)]

//! Project-scoped membership management.
//!
//! A Nibleaf project owns exactly one organization, so that organization is the
//! tenant boundary for members. Every mutation serializes on a project-scoped
//! PostgreSQL advisory lock and re-checks the actor inside the transaction.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use nibleaf_core::{Actor, AppError, AppState, ProjectId, Role, StrictJson, require_project_role};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Postgres, Transaction};
use time::OffsetDateTime;

mod invitation_delivery;
mod invitation_delivery_worker;
mod invitation_email_transport;
pub mod invitation_lifecycle;
mod invitation_token;
pub use invitation_delivery::{
    InvitationEmailProvider, InvitationEmailProviderError, resolve_invitation_email_provider,
};
pub use invitation_delivery_worker::deliver_invitation_email_event;
pub use invitation_email_transport::{
    InvitationEmail, InvitationEmailDeliveryError, ProviderReceipt, send_invitation_email,
};
pub use invitation_token::{
    InvitationTokenClaims, InvitationTokenError, sign_invitation_token, verify_invitation_token,
    verify_invitation_token_with_keys,
};

const MEMBERSHIP_LOCK_SEED: i64 = 8_157_113;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AssignableRole {
    Admin,
    Member,
}

impl AssignableRole {
    const fn as_role(&self) -> Role {
        match self {
            Self::Admin => Role::Admin,
            Self::Member => Role::Member,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateMemberRoleRequest {
    role: AssignableRole,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransferOwnershipRequest {
    member_id: String,
}

#[derive(Debug, Serialize, FromRow)]
struct MemberResponse {
    id: String,
    user_id: String,
    name: String,
    email: String,
    role: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
struct Data<T> {
    data: T,
}

#[derive(FromRow)]
struct MutationScopeRow {
    organization_id: String,
    member_id: String,
    role: String,
}

struct MutationScope {
    organization_id: String,
    actor_member_id: String,
    role: Role,
}

/// Membership and complete invitation-lifecycle route composition. Acceptance,
/// cancellation, expiry, signed manual links, durable email and idempotency are
/// implemented together; the earlier record-only invitation gap is closed.
pub fn router() -> Router<AppState> {
    Router::new()
        .merge(invitation_lifecycle::router())
        .route("/projects/{project_id}/members", get(list_members))
        .route(
            "/projects/{project_id}/members/{member_id}",
            patch(update_member_role).delete(remove_member),
        )
        .route(
            "/projects/{project_id}/members/transfer-ownership",
            post(transfer_ownership),
        )
}

async fn list_members(
    State(state): State<AppState>,
    actor: Actor,
    Path(project_id): Path<String>,
) -> Result<Json<Data<Vec<MemberResponse>>>, AppError> {
    let project_id = ProjectId::try_from(project_id)?;
    let scope = require_project_role(&state.pool, &actor, project_id, Role::Member).await?;
    let members = sqlx::query_as::<_, MemberResponse>(
        r#"
        SELECT m.id, m."userId" AS user_id, u.name, u.email, m.role,
               m."createdAt" AT TIME ZONE 'UTC' AS created_at
        FROM member AS m
        INNER JOIN "user" AS u ON u.id = m."userId"
        WHERE m."organizationId" = $1
        ORDER BY m."createdAt" ASC, m.id ASC
        "#,
    )
    .bind(scope.organization_id.as_str())
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(Data { data: members }))
}

async fn update_member_role(
    State(state): State<AppState>,
    actor: Actor,
    Path((project_id, member_id)): Path<(String, String)>,
    StrictJson(request): StrictJson<UpdateMemberRoleRequest>,
) -> Result<Json<Data<MemberResponse>>, AppError> {
    let project_id = ProjectId::try_from(project_id)?;
    let member_id = validate_member_id(&member_id)?;
    let mut transaction = state.pool.begin().await?;
    let scope = lock_membership_scope(&mut transaction, &actor, &project_id, Role::Admin).await?;
    let mut target = lock_member(&mut transaction, &scope.organization_id, &member_id).await?;
    let target_role = Role::from_db(&target.role).ok_or(AppError::Forbidden)?;
    let requested_role = request.role.as_role();
    if !scope.role.permits(&target_role) {
        return Err(AppError::Forbidden);
    }
    if !scope.role.permits(&requested_role) {
        return Err(AppError::Forbidden);
    }
    if target_role == Role::Owner
        && owner_count(&mut transaction, &scope.organization_id).await? <= 1
    {
        return Err(AppError::conflict(
            "you cannot demote the last owner; transfer ownership instead",
        ));
    }
    sqlx::query("UPDATE member SET role = $3 WHERE id = $1 AND \"organizationId\" = $2")
        .bind(&member_id)
        .bind(&scope.organization_id)
        .bind(requested_role.as_db())
        .execute(&mut *transaction)
        .await?;
    target.role = requested_role.as_db().to_owned();
    transaction.commit().await?;
    Ok(Json(Data { data: target }))
}

async fn transfer_ownership(
    State(state): State<AppState>,
    actor: Actor,
    Path(project_id): Path<String>,
    StrictJson(request): StrictJson<TransferOwnershipRequest>,
) -> Result<Json<Data<MemberResponse>>, AppError> {
    let project_id = ProjectId::try_from(project_id)?;
    let target_member_id = validate_member_id(&request.member_id)?;
    let mut transaction = state.pool.begin().await?;
    let scope = lock_membership_scope(&mut transaction, &actor, &project_id, Role::Owner).await?;
    let mut target =
        lock_member(&mut transaction, &scope.organization_id, &target_member_id).await?;
    if target.id == scope.actor_member_id {
        return Err(AppError::conflict(
            "choose another member to transfer ownership to",
        ));
    }
    if Role::from_db(&target.role) != Some(Role::Admin) {
        return Err(AppError::conflict(
            "ownership can only be transferred to an admin",
        ));
    }

    // The membership migration's partial unique index is the final duplicate
    // owner backstop. Demoting every legacy owner first also repairs historical
    // multi-owner data if the transaction is run before that index is enabled.
    sqlx::query(
        "UPDATE member SET role = 'admin' WHERE \"organizationId\" = $1 AND role = 'owner'",
    )
    .bind(&scope.organization_id)
    .execute(&mut *transaction)
    .await?;
    let promoted =
        sqlx::query("UPDATE member SET role = 'owner' WHERE id = $1 AND \"organizationId\" = $2")
            .bind(&target_member_id)
            .bind(&scope.organization_id)
            .execute(&mut *transaction)
            .await?;
    if promoted.rows_affected() != 1 {
        return Err(AppError::not_found("member"));
    }
    Role::Owner.as_db().clone_into(&mut target.role);
    transaction.commit().await?;
    Ok(Json(Data { data: target }))
}

async fn remove_member(
    State(state): State<AppState>,
    actor: Actor,
    Path((project_id, member_id)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    let project_id = ProjectId::try_from(project_id)?;
    let member_id = validate_member_id(&member_id)?;
    let mut transaction = state.pool.begin().await?;
    let scope = lock_membership_scope(&mut transaction, &actor, &project_id, Role::Admin).await?;
    let target = lock_member(&mut transaction, &scope.organization_id, &member_id).await?;
    let target_role = Role::from_db(&target.role).ok_or(AppError::Forbidden)?;
    if !scope.role.permits(&target_role) {
        return Err(AppError::Forbidden);
    }
    if target_role == Role::Owner
        && owner_count(&mut transaction, &scope.organization_id).await? <= 1
    {
        return Err(AppError::conflict(
            "you cannot remove the last owner; transfer ownership first",
        ));
    }
    let deleted = sqlx::query("DELETE FROM member WHERE id = $1 AND \"organizationId\" = $2")
        .bind(&member_id)
        .bind(&scope.organization_id)
        .execute(&mut *transaction)
        .await?;
    if deleted.rows_affected() != 1 {
        return Err(AppError::not_found("member"));
    }
    transaction.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn lock_membership_scope(
    transaction: &mut Transaction<'_, Postgres>,
    actor: &Actor,
    project_id: &ProjectId,
    required: Role,
) -> Result<MutationScope, AppError> {
    // Every Rust membership mutation obtains this lock before reading roles, so
    // role updates/removals/transfers cannot interleave and strand an org with
    // no owner or two owners at the application layer.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, $2))")
        .bind(project_id.as_str())
        .bind(MEMBERSHIP_LOCK_SEED)
        .execute(&mut **transaction)
        .await?;
    let row = sqlx::query_as::<_, MutationScopeRow>(
        r#"
        SELECT p."organizationId" AS organization_id, m.id AS member_id, m.role
        FROM project AS p
        INNER JOIN member AS m
            ON m."organizationId" = p."organizationId"
           AND m."userId" = $2
        WHERE p.id = $1
        FOR UPDATE OF p, m
        "#,
    )
    .bind(project_id.as_str())
    .bind(actor.user_id.as_str())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(|| AppError::not_found("project"))?;
    let role = Role::from_db(&row.role).ok_or(AppError::Forbidden)?;
    if !role.permits(&required) {
        return Err(AppError::not_found("project"));
    }
    Ok(MutationScope {
        organization_id: row.organization_id,
        actor_member_id: row.member_id,
        role,
    })
}

async fn lock_member(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: &str,
    member_id: &str,
) -> Result<MemberResponse, AppError> {
    sqlx::query_as::<_, MemberResponse>(
        r#"
        SELECT m.id, m."userId" AS user_id, u.name, u.email, m.role,
               m."createdAt" AT TIME ZONE 'UTC' AS created_at
        FROM member AS m
        INNER JOIN "user" AS u ON u.id = m."userId"
        WHERE m.id = $1 AND m."organizationId" = $2
        FOR UPDATE OF m
        "#,
    )
    .bind(member_id)
    .bind(organization_id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(|| AppError::not_found("member"))
}

async fn owner_count(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: &str,
) -> Result<i64, AppError> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM member WHERE \"organizationId\" = $1 AND role = 'owner'",
    )
    .bind(organization_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(AppError::from)
}

fn validate_member_id(value: &str) -> Result<String, AppError> {
    if value.is_empty()
        || value.len() > 191
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(AppError::validation("invalid member id"));
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::validate_member_id;

    #[test]
    fn member_identifier_is_opaque() {
        assert!(validate_member_id("member-1").is_ok());
        assert!(validate_member_id("member id").is_err());
    }
}
