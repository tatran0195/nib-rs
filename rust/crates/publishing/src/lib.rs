#![forbid(unsafe_code)]

//! Immutable publishing slice. Draft state is snapshotted in a short database
//! transaction together with a durable native-search request and publication
//! completion event. The worker waits for registry promotion before transition
//! to READY. Public readers must select READY only.

use axum::{
    Json, Router,
    extract::{Extension, Path, State},
    http::StatusCode,
    routing::post,
};
use nibleaf_core::{
    Actor, AppError, AppState, ProjectId, Role, StrictJson, UserId, canonical_json_sha256,
    require_project_role,
};
use nibleaf_outbox::{ClaimedEvent, Failure, NewEvent, append};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

mod snapshot;

use snapshot::capture_snapshot;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishRequest {
    #[serde(default)]
    commit_message: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
struct PublicationResponse {
    id: String,
    project_id: String,
    version: i32,
    status: String,
    pages_count: i32,
    commit_message: Option<String>,
}

#[derive(Debug, Serialize)]
struct Data<T> {
    data: T,
}

#[derive(Clone, Copy, Debug)]
pub enum PublicationSearchDelivery {
    Local,
    Object,
}
impl PublicationSearchDelivery {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Object => "object",
        }
    }
}

pub fn router() -> Router<AppState> {
    router_with_delivery(PublicationSearchDelivery::Local)
}

/// Server-owned publication delivery; never a caller-selectable body field.
pub fn router_with_delivery(delivery: PublicationSearchDelivery) -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{project_id}/publications",
            post(request_publication).get(list_publications),
        )
        .layer(Extension(delivery))
}

async fn request_publication(
    Extension(delivery): Extension<PublicationSearchDelivery>,
    State(state): State<AppState>,
    actor: Actor,
    Path(project_id): Path<String>,
    StrictJson(request): StrictJson<PublishRequest>,
) -> Result<(StatusCode, Json<Data<PublicationResponse>>), AppError> {
    let project_id = ProjectId::try_from(project_id)?;
    let scope = require_project_role(&state.pool, &actor, project_id, Role::Member).await?;
    let commit_message = request
        .commit_message
        .as_deref()
        .map(validate_commit_message)
        .transpose()?;
    let mut transaction = state.pool.begin().await?;

    // Serialize publication requests per aggregate. The partial unique index
    // installed by the migration remains the final backstop if a future code
    // path accidentally bypasses this lock.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 774301))")
        .bind(scope.project_id.as_str())
        .execute(&mut *transaction)
        .await?;
    let taken_down: Option<bool> = sqlx::query_scalar(
        r#"SELECT "takedownAt" IS NOT NULL FROM project WHERE id = $1 AND "organizationId" = $2 FOR SHARE"#,
    )
    .bind(scope.project_id.as_str())
    .bind(scope.organization_id.as_str())
    .fetch_optional(&mut *transaction)
    .await?;
    match taken_down {
        Some(true) => return Err(AppError::Forbidden),
        Some(false) => {}
        None => return Err(AppError::not_found("project")),
    }
    let active: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM deployment
            WHERE "projectId" = $1 AND status IN ('PENDING'::"DeploymentStatus", 'BUILDING'::"DeploymentStatus")
        )
        "#,
    )
    .bind(scope.project_id.as_str())
    .fetch_one(&mut *transaction)
    .await?;
    if active {
        return Err(AppError::conflict(
            "a publication is already being prepared for this project",
        ));
    }

    let snapshot = capture_snapshot(&mut transaction, scope.project_id.as_str()).await?;
    let pages_count = i32::try_from(snapshot["pages"].as_array().map_or(0, Vec::len))
        .map_err(|_| AppError::validation("project contains too many pages to publish"))?;
    let version: i32 = sqlx::query_scalar(
        r#"SELECT COALESCE(MAX(version), 0) + 1 FROM deployment WHERE "projectId" = $1"#,
    )
    .bind(scope.project_id.as_str())
    .fetch_one(&mut *transaction)
    .await?;
    let deployment_id = Uuid::now_v7().to_string();
    let row = sqlx::query_as::<_, PublicationResponse>(
        r#"
        INSERT INTO deployment (
            id, "projectId", version, status, snapshot, "pagesCount", "commitMessage", "createdById", "createdAt", rust_search_delivery_mode
        ) VALUES ($1, $2, $3, 'PENDING'::"DeploymentStatus", $4, $5, $6, $7, NOW(),$8)
        RETURNING id, "projectId" AS project_id, version, status::text AS status,
                  "pagesCount" AS pages_count, "commitMessage" AS commit_message
        "#,
    )
    .bind(&deployment_id)
    .bind(scope.project_id.as_str())
    .bind(version)
    .bind(&snapshot)
    .bind(pages_count)
    .bind(&commit_message)
    .bind(actor.user_id.as_str())
    .bind(delivery.as_str())
    .fetch_one(&mut *transaction)
    .await?;
    let snapshot_sha256 = snapshot_digest(&snapshot);
    append_search_generation_request(
        &mut transaction,
        scope.organization_id.as_str(),
        scope.project_id.as_str(),
        &deployment_id,
        &snapshot_sha256,
    )
    .await?;
    append(
        &mut transaction,
        NewEvent {
            tenant_id: Some(scope.organization_id.as_str()),
            aggregate_type: "deployment",
            aggregate_id: &deployment_id,
            event_type: "publication.requested",
            schema_version: 1,
            payload: serde_json::json!({
                "deployment_id": deployment_id,
                "snapshot_sha256": snapshot_sha256,
            }),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await?;
    transaction.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(Data { data: row })))
}

/// Creates the first, system-initiated publication for the rich native signup
/// starter. The project feature emits this tenant-scoped durable request only
/// after the project, default branch, and all starter pages commit together.
///
/// A user publication that wins the race suppresses this automatic one rather
/// than creating a surprising second first deployment. Redelivery is therefore
/// safe: any deployment for the project makes the event a no-op.
///
/// # Errors
/// Returns a permanent failure for an invalid/foreign event and retryable
/// failure when PostgreSQL cannot create the durable publication request.
pub async fn request_starter_publication(
    pool: &PgPool,
    event: &ClaimedEvent,
) -> Result<(), Failure> {
    request_starter_publication_for_delivery(pool, event, PublicationSearchDelivery::Local).await
}

/// Native worker captures the deployment's delivery target before its durable
/// search intent; local/object targets are never relabeled on retries.
/// # Errors
/// Preserves tenant/event/database failures from the normal starter workflow.
pub async fn request_starter_publication_for_delivery(
    pool: &PgPool,
    event: &ClaimedEvent,
    delivery: PublicationSearchDelivery,
) -> Result<(), Failure> {
    let request = parse_starter_publication_event(event)?;
    create_starter_publication(pool, &request, delivery).await
}

struct StarterPublicationRequest {
    tenant_id: String,
    project_id: ProjectId,
    created_by: UserId,
}

fn parse_starter_publication_event(
    event: &ClaimedEvent,
) -> Result<StarterPublicationRequest, Failure> {
    let tenant_id = event.tenant_id.clone().ok_or_else(|| Failure::Permanent {
        message: "starter publication must be tenant-scoped".to_owned(),
    })?;
    let project_id = event
        .payload
        .get("project_id")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::Permanent {
            message: "starter publication payload is missing project_id".to_owned(),
        })?
        .parse::<ProjectId>()
        .map_err(|_| Failure::Permanent {
            message: "starter publication project_id is invalid".to_owned(),
        })?;
    if event.aggregate_id != project_id.as_str() {
        return Err(Failure::Permanent {
            message: "starter publication aggregate mismatch".to_owned(),
        });
    }
    let created_by = event
        .payload
        .get("created_by_user_id")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::Permanent {
            message: "starter publication payload is missing created_by_user_id".to_owned(),
        })?
        .parse::<UserId>()
        .map_err(|_| Failure::Permanent {
            message: "starter publication created_by_user_id is invalid".to_owned(),
        })?;
    Ok(StarterPublicationRequest {
        tenant_id,
        project_id,
        created_by,
    })
}

async fn create_starter_publication(
    pool: &PgPool,
    request: &StarterPublicationRequest,
    delivery: PublicationSearchDelivery,
) -> Result<(), Failure> {
    let mut transaction = pool.begin().await.map_err(|_| Failure::Retryable {
        message: "database unavailable while starting starter publication".to_owned(),
    })?;
    lock_starter_project(&mut transaction, request).await?;
    if starter_deployment_exists(&mut transaction, request.project_id.as_str()).await? {
        transaction.commit().await.map_err(|_| Failure::Retryable {
            message: "database unavailable while completing starter publication".to_owned(),
        })?;
        return Ok(());
    }
    let snapshot = capture_snapshot(&mut transaction, request.project_id.as_str())
        .await
        .map_err(|error| match error {
            AppError::Database(_) | AppError::Unavailable => Failure::Retryable {
                message: "database unavailable while capturing starter publication".to_owned(),
            },
            _ => Failure::Permanent {
                message: "starter publication snapshot is invalid".to_owned(),
            },
        })?;
    let pages_count =
        i32::try_from(snapshot["pages"].as_array().map_or(0, Vec::len)).map_err(|_| {
            Failure::Permanent {
                message: "starter project contains too many pages to publish".to_owned(),
            }
        })?;
    let deployment_id = Uuid::now_v7().to_string();
    sqlx::query(
        r#"
        INSERT INTO deployment (
            id, "projectId", version, status, snapshot, "pagesCount", "commitMessage", "createdById", "createdAt", rust_search_delivery_mode
        ) VALUES ($1, $2, 1, 'PENDING'::"DeploymentStatus", $3, $4, 'Publish starter site', $5, NOW(),$6)
        "#,
    )
    .bind(&deployment_id)
    .bind(request.project_id.as_str())
    .bind(&snapshot)
    .bind(pages_count)
    .bind(request.created_by.as_str())
    .bind(delivery.as_str())
    .execute(&mut *transaction)
    .await
    .map_err(|_| Failure::Retryable {
        message: "database unavailable while creating starter publication".to_owned(),
    })?;
    let snapshot_sha256 = snapshot_digest(&snapshot);
    append_search_generation_request(
        &mut transaction,
        &request.tenant_id,
        request.project_id.as_str(),
        &deployment_id,
        &snapshot_sha256,
    )
    .await
    .map_err(|_| Failure::Retryable {
        message: "database unavailable while recording starter search intent".to_owned(),
    })?;
    append(
        &mut transaction,
        NewEvent {
            tenant_id: Some(&request.tenant_id),
            aggregate_type: "deployment",
            aggregate_id: &deployment_id,
            event_type: "publication.requested",
            schema_version: 1,
            payload: serde_json::json!({
                "deployment_id": deployment_id,
                "snapshot_sha256": snapshot_sha256,
                "auto": true,
            }),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .map_err(|_| Failure::Retryable {
        message: "database unavailable while recording starter publication".to_owned(),
    })?;
    transaction.commit().await.map_err(|_| Failure::Retryable {
        message: "database unavailable while committing starter publication".to_owned(),
    })?;
    Ok(())
}

/// Appends the search feature's durable request in the same publication
/// transaction as the immutable deployment. The payload is intentionally just
/// deployment identity: the search feature assigns the generation UUID after
/// it rechecks project/tenant/deployment bindings under its own lease.
async fn append_search_generation_request(
    transaction: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
    project_id: &str,
    deployment_id: &str,
    snapshot_sha256: &str,
) -> Result<(), AppError> {
    append(
        transaction,
        NewEvent {
            tenant_id: Some(tenant_id),
            aggregate_type: "search_generation",
            aggregate_id: deployment_id,
            event_type: "search.generation.requested",
            schema_version: 1,
            payload: serde_json::json!({
                "project_id": project_id,
                "deployment_id": deployment_id,
                "snapshot_sha256": snapshot_sha256,
            }),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .map(|_| ())
}

async fn lock_starter_project(
    transaction: &mut Transaction<'_, Postgres>,
    request: &StarterPublicationRequest,
) -> Result<(), Failure> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 774301))")
        .bind(request.project_id.as_str())
        .execute(&mut **transaction)
        .await
        .map_err(|_| Failure::Retryable {
            message: "database unavailable while locking starter publication".to_owned(),
        })?;
    let project = sqlx::query_as::<_, (String, bool)>(
        r#"SELECT "organizationId", "takedownAt" IS NOT NULL FROM project WHERE id=$1 FOR SHARE"#,
    )
    .bind(request.project_id.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| Failure::Retryable {
        message: "database unavailable while loading starter project".to_owned(),
    })?
    .ok_or_else(|| Failure::Permanent {
        message: "starter project no longer exists".to_owned(),
    })?;
    if project.0 != request.tenant_id {
        return Err(Failure::Permanent {
            message: "starter publication tenant does not own project".to_owned(),
        });
    }
    if project.1 {
        return Err(Failure::Permanent {
            message: "starter project has been taken down".to_owned(),
        });
    }
    Ok(())
}

async fn starter_deployment_exists(
    transaction: &mut Transaction<'_, Postgres>,
    project_id: &str,
) -> Result<bool, Failure> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM deployment WHERE \"projectId\"=$1)")
        .bind(project_id)
        .fetch_one(&mut **transaction)
        .await
        .map_err(|_| Failure::Retryable {
            message: "database unavailable while checking starter publication".to_owned(),
        })
}

async fn list_publications(
    State(state): State<AppState>,
    actor: Actor,
    Path(project_id): Path<String>,
) -> Result<Json<Data<Vec<PublicationResponse>>>, AppError> {
    let project_id = ProjectId::try_from(project_id)?;
    let scope = require_project_role(&state.pool, &actor, project_id, Role::Member).await?;
    let rows = sqlx::query_as::<_, PublicationResponse>(
        r#"
        SELECT id, "projectId" AS project_id, version, status::text AS status,
               "pagesCount" AS pages_count, "commitMessage" AS commit_message
        FROM deployment
        WHERE "projectId" = $1
        ORDER BY version DESC
        LIMIT 100
        "#,
    )
    .bind(scope.project_id.as_str())
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(Data { data: rows }))
}

/// Worker-side idempotent consumer of `publication.requested`.
///
/// Rendering, assets, and notification work remain separate durable workflows.
/// Native search is already a leased outbox workflow: this completion consumer
/// only reads its terminal registry state under the deployment transaction. It
/// must return a classified failure and never call an external service while a
/// database transaction is open.
///
/// # Errors
///
/// Returns a permanent failure for malformed or terminal events and a retryable
/// failure when PostgreSQL cannot safely complete the transition.
pub async fn complete_publication(pool: &PgPool, event: &ClaimedEvent) -> Result<(), Failure> {
    let deployment_id = event
        .payload
        .get("deployment_id")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::Permanent {
            message: "publication.requested payload is missing deployment_id".to_owned(),
        })?;
    let mut transaction = pool.begin().await.map_err(|_| Failure::Retryable {
        message: "database unavailable while starting publication".to_owned(),
    })?;
    let row = sqlx::query_as::<_, WorkerDeploymentRow>(
        r#"
        SELECT d."projectId" AS project_id, d.status::text AS status,
               p."organizationId" AS tenant_id
        FROM deployment AS d
        INNER JOIN project AS p ON p.id = d."projectId"
        WHERE d.id = $1
        FOR UPDATE
        "#,
    )
    .bind(deployment_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|_| Failure::Retryable {
        message: "database unavailable while loading publication".to_owned(),
    })?
    .ok_or_else(|| Failure::Permanent {
        message: "deployment no longer exists".to_owned(),
    })?;
    if event.tenant_id.as_deref() != Some(row.tenant_id.as_str()) {
        return Err(Failure::Permanent {
            message: "event tenant does not own deployment".to_owned(),
        });
    }
    match row.status.as_str() {
        "READY" => {
            transaction.commit().await.map_err(|_| Failure::Retryable {
                message: "database unavailable while completing publication".to_owned(),
            })?;
            return Ok(());
        }
        "PENDING" | "BUILDING" => {}
        "FAILED" => {
            return Err(Failure::Permanent {
                message: "publication is terminally failed".to_owned(),
            });
        }
        _ => {
            return Err(Failure::Permanent {
                message: "unknown publication state".to_owned(),
            });
        }
    }
    sqlx::query(r#"UPDATE deployment SET status = 'BUILDING'::"DeploymentStatus" WHERE id = $1"#)
        .bind(deployment_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| Failure::Retryable {
            message: "database unavailable while claiming publication".to_owned(),
        })?;
    // Search is independently leased and built outside this transaction. The
    // publication event retries while the matching registry row is absent or
    // non-terminal, so no outbox ordering assumption can expose READY first.
    match search_generation_readiness(&mut transaction, &row.project_id, deployment_id).await? {
        SearchGenerationReadiness::Waiting => {
            transaction.commit().await.map_err(|_| Failure::Retryable {
                message: "database unavailable while waiting for search generation".to_owned(),
            })?;
            return Err(Failure::Retryable {
                message: "search generation is not ready".to_owned(),
            });
        }
        SearchGenerationReadiness::Failed => {
            sqlx::query(
                r#"UPDATE deployment
                    SET status = 'FAILED'::"DeploymentStatus", "completedAt" = NOW(),
                        error = 'native_search_generation_failed'
                    WHERE id = $1"#,
            )
            .bind(deployment_id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| Failure::Retryable {
                message: "database unavailable while recording search failure".to_owned(),
            })?;
            transaction.commit().await.map_err(|_| Failure::Retryable {
                message: "database unavailable while completing failed publication".to_owned(),
            })?;
            return Err(Failure::Permanent {
                message: "search generation failed permanently".to_owned(),
            });
        }
        SearchGenerationReadiness::Ready => {}
    }
    finalize_publication_ready(transaction, event, deployment_id).await
}

async fn finalize_publication_ready(
    mut transaction: Transaction<'_, Postgres>,
    event: &ClaimedEvent,
    deployment_id: &str,
) -> Result<(), Failure> {
    sqlx::query(
        r#"UPDATE deployment SET status = 'READY'::"DeploymentStatus", "completedAt" = NOW(), error = NULL WHERE id = $1"#,
    )
    .bind(deployment_id)
    .execute(&mut *transaction)
    .await
    .map_err(|_| Failure::Retryable {
        message: "database unavailable while finalizing publication".to_owned(),
    })?;
    append(
        &mut transaction,
        NewEvent {
            tenant_id: event.tenant_id.as_deref(),
            aggregate_type: "deployment",
            aggregate_id: deployment_id,
            event_type: "publication.ready",
            schema_version: 1,
            payload: serde_json::json!({"deployment_id": deployment_id}),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .map_err(|_| Failure::Retryable {
        message: "database unavailable while recording publication completion".to_owned(),
    })?;
    transaction.commit().await.map_err(|_| Failure::Retryable {
        message: "database unavailable while committing publication".to_owned(),
    })
}

enum SearchGenerationReadiness {
    Failed,
    Ready,
    Waiting,
}

async fn search_generation_readiness(
    transaction: &mut Transaction<'_, Postgres>,
    project_id: &str,
    deployment_id: &str,
) -> Result<SearchGenerationReadiness, Failure> {
    let state: Option<String> = sqlx::query_scalar(
        r"SELECT state FROM rust_search_generation
           WHERE project_id = $1 AND deployment_id = $2
           FOR SHARE",
    )
    .bind(project_id)
    .bind(deployment_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| Failure::Retryable {
        message: "database unavailable while checking search generation".to_owned(),
    })?;
    Ok(match state.as_deref() {
        Some("ready") => SearchGenerationReadiness::Ready,
        Some("pending" | "building") | None => SearchGenerationReadiness::Waiting,
        Some("failed" | "superseded" | _) => SearchGenerationReadiness::Failed,
    })
}

#[derive(FromRow)]
struct WorkerDeploymentRow {
    project_id: String,
    status: String,
    tenant_id: String,
}

fn validate_commit_message(value: &str) -> Result<String, AppError> {
    let value = value.trim().to_owned();
    if value.chars().count() > 500 {
        return Err(AppError::validation(
            "commit_message must not exceed 500 characters",
        ));
    }
    Ok(value)
}

fn snapshot_digest(snapshot: &Value) -> String {
    canonical_json_sha256(snapshot)
}

#[cfg(test)]
mod tests {
    use super::{snapshot_digest, validate_commit_message};

    #[test]
    fn snapshot_digest_is_deterministic() {
        let snapshot = serde_json::json!({"page": "hello"});
        assert_eq!(snapshot_digest(&snapshot), snapshot_digest(&snapshot));
    }

    #[test]
    fn commit_message_is_bounded() {
        assert!(validate_commit_message("ok").is_ok());
        assert!(validate_commit_message(&"x".repeat(501)).is_err());
    }
}
