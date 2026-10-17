//! Transactional member-invitation aggregate and HTTP handlers.
//!
//! Token verification never substitutes for tenant authorization: creation and
//! cancellation lock the project scope, while acceptance locks the invitation
//! and current user row before it changes membership.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header::HeaderName},
    routing::post,
};
use nibleaf_core::{Actor, AppError, AppState, ProjectId, Role, StrictJson};
use nibleaf_outbox::{NewEvent, append};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, Postgres, Transaction};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{InvitationTokenClaims, sign_invitation_token, verify_invitation_token_with_keys};

const INVITATION_LOCK_SEED: i64 = 3_891_417;
// Matches the other membership mutations so acceptance cannot race a role,
// removal, or ownership transition for the same project.
const MEMBERSHIP_LOCK_SEED: i64 = 8_157_113;
const INVITATION_TTL: Duration = Duration::days(7);
const IDEMPOTENCY_HEADER: HeaderName = HeaderName::from_static("idempotency-key");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateInvitationRequest {
    recipient_email: String,
    role: InvitationRole,
    delivery_mode: DeliveryMode,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum InvitationRole {
    Admin,
    Member,
}

impl InvitationRole {
    const fn as_db(&self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DeliveryMode {
    Email,
    Manual,
}

impl DeliveryMode {
    const fn as_db(&self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Serialize, FromRow)]
struct InvitationResponse {
    id: Uuid,
    recipient_email: String,
    requested_role: String,
    delivery_mode: String,
    status: String,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    acceptance_url: Option<String>,
}

#[derive(Debug, Serialize)]
struct Data<T> {
    data: T,
}

#[derive(FromRow)]
struct InvitationRow {
    id: Uuid,
    project_id: String,
    organization_id: String,
    recipient_email: String,
    requested_role: String,
    delivery_mode: String,
    status: String,
    expires_at: OffsetDateTime,
    request_fingerprint: Vec<u8>,
}

#[derive(FromRow)]
struct ScopeRow {
    organization_id: String,
    role: String,
}

#[derive(FromRow)]
struct ActorEmailRow {
    email: String,
}

/// Invitation route composition. No legacy invitation URL is replaced by this
/// additive API until the worker delivery contract is proven end-to-end.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{project_id}/member-invitations",
            post(create_invitation),
        )
        .route(
            "/projects/{project_id}/member-invitations/{invitation_id}/cancel",
            post(cancel_invitation),
        )
        .route(
            "/member-invitations/{token}/accept",
            post(accept_invitation),
        )
}

async fn create_invitation(
    State(state): State<AppState>,
    actor: Actor,
    Path(project_id): Path<String>,
    headers: HeaderMap,
    StrictJson(request): StrictJson<CreateInvitationRequest>,
) -> Result<(StatusCode, Json<Data<InvitationResponse>>), AppError> {
    let project_id = ProjectId::try_from(project_id)?;
    let idempotency_key = required_idempotency_key(&headers)?;
    let email = canonical_email(&request.recipient_email)?;
    let fingerprint =
        request_fingerprint(&email, request.role.as_db(), request.delivery_mode.as_db());
    let mut transaction = state.pool.begin().await?;
    let scope = lock_scope(&mut transaction, &actor, &project_id, Role::Admin).await?;
    expire_pending(&mut transaction, project_id.as_str()).await?;

    let idempotency_hash = hash(idempotency_key.as_bytes());
    if let Some(existing) =
        invitation_for_idempotency(&mut transaction, &scope.organization_id, &idempotency_hash)
            .await?
    {
        if existing.request_fingerprint != fingerprint {
            return Err(AppError::conflict(
                "idempotency key was reused for a different invitation",
            ));
        }
        let response = response_for(existing, &state, false)?;
        transaction.commit().await?;
        return Ok((StatusCode::OK, Json(Data { data: response })));
    }

    let member_exists: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
            SELECT 1 FROM member AS m INNER JOIN "user" AS u ON u.id = m."userId"
            WHERE m."organizationId" = $1 AND lower(u.email) = $2
        )"#,
    )
    .bind(&scope.organization_id)
    .bind(&email)
    .fetch_one(&mut *transaction)
    .await?;
    if member_exists {
        return Err(AppError::conflict("recipient is already a member"));
    }

    let id = Uuid::now_v7();
    let expires_at = OffsetDateTime::now_utc() + INVITATION_TTL;
    let inserted = sqlx::query_as::<_, InvitationRow>(
        r"INSERT INTO member_invitation (
             id, project_id, organization_id, recipient_email, requested_role, delivery_mode,
             expires_at, inviter_id, idempotency_key_hash, request_fingerprint
           ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
           RETURNING id, project_id, organization_id, recipient_email, requested_role,
                     delivery_mode, status, expires_at, request_fingerprint, accepted_member_id",
    )
    .bind(id)
    .bind(project_id.as_str())
    .bind(&scope.organization_id)
    .bind(&email)
    .bind(request.role.as_db())
    .bind(request.delivery_mode.as_db())
    .bind(expires_at)
    .bind(actor.user_id.as_str())
    .bind(idempotency_hash)
    .bind(fingerprint)
    .fetch_one(&mut *transaction)
    .await?;

    if matches!(request.delivery_mode, DeliveryMode::Email) {
        sqlx::query("INSERT INTO member_invitation_delivery (invitation_id) VALUES ($1)")
            .bind(id)
            .execute(&mut *transaction)
            .await?;
        append(
            &mut transaction,
            NewEvent {
                tenant_id: Some(&scope.organization_id),
                aggregate_type: "member_invitation",
                aggregate_id: &id.to_string(),
                event_type: "membership.invitation.delivery.requested",
                schema_version: 1,
                payload: serde_json::json!({"invitation_id": id}),
                correlation_id: Uuid::now_v7(),
            },
        )
        .await?;
    }
    let response = response_for(inserted, &state, true)?;
    transaction.commit().await?;
    Ok((StatusCode::CREATED, Json(Data { data: response })))
}

async fn cancel_invitation(
    State(state): State<AppState>,
    actor: Actor,
    Path((project_id, invitation_id)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    let project_id = ProjectId::try_from(project_id)?;
    let invitation_id =
        Uuid::parse_str(&invitation_id).map_err(|_| AppError::not_found("invitation"))?;
    let mut transaction = state.pool.begin().await?;
    let scope = lock_scope(&mut transaction, &actor, &project_id, Role::Admin).await?;
    let updated = sqlx::query(
        "UPDATE member_invitation SET status = 'cancelled', cancelled_at = NOW(), updated_at = NOW() WHERE id = $1 AND project_id = $2 AND organization_id = $3 AND status = 'pending' AND expires_at > NOW()",
    )
    .bind(invitation_id)
    .bind(project_id.as_str())
    .bind(&scope.organization_id)
    .execute(&mut *transaction)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AppError::not_found("invitation"));
    }
    sqlx::query("UPDATE member_invitation_delivery SET state = 'dead_letter', last_error = 'cancelled', leased_until = NULL, lease_token = NULL, updated_at = NOW() WHERE invitation_id = $1 AND state <> 'sent'")
        .bind(invitation_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn accept_invitation(
    State(state): State<AppState>,
    actor: Actor,
    Path(token): Path<String>,
) -> Result<Json<Data<InvitationResponse>>, AppError> {
    let claims = verify_invitation_token_with_keys(
        &state.config.invitation_signing_key,
        &state.config.invitation_previous_signing_keys,
        &token,
        OffsetDateTime::now_utc(),
    )
    .map_err(|_| AppError::not_found("invitation"))?;
    let mut transaction = state.pool.begin().await?;
    let invitation = invitation_for_acceptance(&mut transaction, claims.invitation_id).await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, $2))")
        .bind(&invitation.project_id)
        .bind(MEMBERSHIP_LOCK_SEED)
        .execute(&mut *transaction)
        .await?;
    let actor_email: ActorEmailRow = sqlx::query_as(
        "SELECT email FROM \"user\" WHERE id = $1 AND \"suspendedAt\" IS NULL AND banned = FALSE FOR UPDATE",
    )
    .bind(actor.user_id.as_str())
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or(AppError::Unauthenticated)?;
    if canonical_email(&actor_email.email).map_err(|_| AppError::not_found("invitation"))?
        != invitation.recipient_email
        // The signed token stores seconds, while PostgreSQL preserves
        // microseconds. Compare the canonical signed representation, not the
        // in-memory subsecond timestamp.
        || invitation.expires_at.unix_timestamp() != claims.expires_at.unix_timestamp()
        || invitation.status == "cancelled"
        || invitation.status == "expired"
    {
        return Err(AppError::not_found("invitation"));
    }
    if invitation.status == "accepted" {
        let response = response_for(invitation, &state, false)?;
        transaction.commit().await?;
        return Ok(Json(Data { data: response }));
    }
    if invitation.status != "pending" || invitation.expires_at <= OffsetDateTime::now_utc() {
        if invitation.status == "pending" {
            sqlx::query("UPDATE member_invitation SET status='expired', updated_at=NOW() WHERE id=$1 AND status='pending'")
                .bind(claims.invitation_id)
                .execute(&mut *transaction)
                .await?;
        }
        return Err(AppError::not_found("invitation"));
    }
    let member_id = existing_or_new_member(&mut transaction, &invitation, &actor).await?;
    let accepted = sqlx::query_as::<_, InvitationRow>(
        "UPDATE member_invitation SET status='accepted', accepted_at=NOW(), accepted_member_id=$2, updated_at=NOW() WHERE id=$1 AND status='pending' RETURNING id, project_id, organization_id, recipient_email, requested_role, delivery_mode, status, expires_at, request_fingerprint, accepted_member_id",
    )
    .bind(claims.invitation_id)
    .bind(member_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(|| AppError::not_found("invitation"))?;
    let response = response_for(accepted, &state, false)?;
    transaction.commit().await?;
    Ok(Json(Data { data: response }))
}

async fn lock_scope(
    transaction: &mut Transaction<'_, Postgres>,
    actor: &Actor,
    project_id: &ProjectId,
    required: Role,
) -> Result<ScopeRow, AppError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, $2))")
        .bind(project_id.as_str())
        .bind(INVITATION_LOCK_SEED)
        .execute(&mut **transaction)
        .await?;
    let scope = sqlx::query_as::<_, ScopeRow>(
        r#"SELECT p."organizationId" AS organization_id, m.role FROM project p
           INNER JOIN member m ON m."organizationId"=p."organizationId" AND m."userId"=$2
           WHERE p.id=$1 FOR UPDATE OF p, m"#,
    )
    .bind(project_id.as_str())
    .bind(actor.user_id.as_str())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(|| AppError::not_found("project"))?;
    let role = Role::from_db(&scope.role).ok_or(AppError::Forbidden)?;
    if !role.permits(&required) {
        return Err(AppError::not_found("project"));
    }
    Ok(scope)
}

async fn expire_pending(
    transaction: &mut Transaction<'_, Postgres>,
    project_id: &str,
) -> Result<(), AppError> {
    sqlx::query("UPDATE member_invitation SET status='expired', updated_at=NOW() WHERE project_id=$1 AND status='pending' AND expires_at <= NOW()")
        .bind(project_id).execute(&mut **transaction).await?;
    Ok(())
}

async fn invitation_for_idempotency(
    transaction: &mut Transaction<'_, Postgres>,
    organization_id: &str,
    key: &[u8],
) -> Result<Option<InvitationRow>, AppError> {
    sqlx::query_as("SELECT id, project_id, organization_id, recipient_email, requested_role, delivery_mode, status, expires_at, request_fingerprint, accepted_member_id FROM member_invitation WHERE organization_id=$1 AND idempotency_key_hash=$2 FOR UPDATE")
        .bind(organization_id).bind(key).fetch_optional(&mut **transaction).await.map_err(AppError::from)
}

async fn invitation_for_acceptance(
    transaction: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<InvitationRow, AppError> {
    sqlx::query_as("SELECT id, project_id, organization_id, recipient_email, requested_role, delivery_mode, status, expires_at, request_fingerprint, accepted_member_id FROM member_invitation WHERE id=$1 FOR UPDATE")
        .bind(id).fetch_optional(&mut **transaction).await?.ok_or_else(|| AppError::not_found("invitation"))
}

async fn existing_or_new_member(
    transaction: &mut Transaction<'_, Postgres>,
    invitation: &InvitationRow,
    actor: &Actor,
) -> Result<String, AppError> {
    if let Some(id) = sqlx::query_scalar::<_, String>(
        "SELECT id FROM member WHERE \"organizationId\"=$1 AND \"userId\"=$2 FOR UPDATE",
    )
    .bind(&invitation.organization_id)
    .bind(actor.user_id.as_str())
    .fetch_optional(&mut **transaction)
    .await?
    {
        return Ok(id);
    }
    let id = Uuid::now_v7().to_string();
    sqlx::query("INSERT INTO member (id, \"organizationId\", \"userId\", role, \"createdAt\") VALUES ($1,$2,$3,$4,NOW())")
        .bind(&id).bind(&invitation.organization_id).bind(actor.user_id.as_str()).bind(&invitation.requested_role).execute(&mut **transaction).await?;
    Ok(id)
}

fn response_for(
    row: InvitationRow,
    state: &AppState,
    newly_created: bool,
) -> Result<InvitationResponse, AppError> {
    let acceptance_url = if row.delivery_mode == "manual" && row.status == "pending" {
        let token = sign_invitation_token(
            &state.config.invitation_signing_key,
            InvitationTokenClaims {
                invitation_id: row.id,
                expires_at: row.expires_at,
            },
        )
        .map_err(|_| AppError::Internal)?;
        Some(format!(
            "{}/invitations/{}",
            state
                .config
                .invitation_email
                .application_url
                .trim_end_matches('/'),
            token
        ))
    } else {
        None
    };
    let _ = newly_created;
    Ok(InvitationResponse {
        id: row.id,
        recipient_email: row.recipient_email,
        requested_role: row.requested_role,
        delivery_mode: row.delivery_mode,
        status: row.status,
        expires_at: row.expires_at,
        acceptance_url,
    })
}

fn required_idempotency_key(headers: &HeaderMap) -> Result<String, AppError> {
    let value = headers
        .get(&IDEMPOTENCY_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| AppError::validation("Idempotency-Key header is required"))?;
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(AppError::validation("invalid Idempotency-Key header"));
    }
    Ok(value.to_owned())
}

fn canonical_email(value: &str) -> Result<String, AppError> {
    let email = value.trim().to_ascii_lowercase();
    if email.len() < 3
        || email.len() > 320
        || email.contains(char::is_whitespace)
        || email.matches('@').count() != 1
        || email.starts_with('@')
        || email.ends_with('@')
    {
        return Err(AppError::validation("invalid recipient email"));
    }
    Ok(email)
}
fn hash(value: &[u8]) -> Vec<u8> {
    Sha256::digest(value).to_vec()
}
fn request_fingerprint(email: &str, role: &str, mode: &str) -> Vec<u8> {
    hash(format!("{email}\0{role}\0{mode}").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::{canonical_email, required_idempotency_key};
    use axum::http::{HeaderMap, HeaderValue, header::HeaderName};

    #[test]
    fn invitation_wire_expiry_matches_the_date_time_contract() -> Result<(), serde_json::Error> {
        let response = super::InvitationResponse {
            id: uuid::Uuid::nil(),
            recipient_email: "test@example.test".to_owned(),
            requested_role: "member".to_owned(),
            delivery_mode: "manual".to_owned(),
            status: "pending".to_owned(),
            expires_at: time::OffsetDateTime::UNIX_EPOCH,
            acceptance_url: None,
        };
        assert_eq!(
            serde_json::to_value(response)?["expires_at"].as_str(),
            Some("1970-01-01T00:00:00Z")
        );
        Ok(())
    }

    #[test]
    fn canonicalizes_email_and_rejects_ambiguous_address() {
        assert!(matches!(
            canonical_email(" Invitee@Example.Test ").as_deref(),
            Ok("invitee@example.test")
        ));
        assert!(canonical_email("not-an-email").is_err());
    }
    #[test]
    fn idempotency_key_is_required_and_bounded() {
        let headers = HeaderMap::new();
        assert!(required_idempotency_key(&headers).is_err());
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("idempotency-key"),
            HeaderValue::from_static("request-1"),
        );
        assert!(matches!(
            required_idempotency_key(&headers).as_deref(),
            Ok("request-1")
        ));
    }
}
