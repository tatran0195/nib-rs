#![forbid(unsafe_code)]

//! Project and recoverable first-run initialization slices.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use nibleaf_core::{
    Actor, AppError, AppState, OrganizationId, ProjectId, Role, StrictJson, UserId,
    require_project_role,
};
use nibleaf_outbox::{ClaimedEvent, Failure, NewEvent, append};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Postgres, Transaction};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateProjectRequest {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
struct ProjectResponse {
    id: String,
    organization_id: String,
    name: String,
    slug: String,
    description: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
struct OnboardingResponse {
    id: Uuid,
    project_id: String,
    state: String,
    last_error: Option<String>,
}

#[derive(Debug, Serialize)]
struct Data<T> {
    data: T,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ProjectBootstrap {
    Empty,
    NativeStarter,
}

// Reviewed source snapshot of the customer-facing Better Auth starter literals
// in `packages/auth/src/server.ts`. Keep these files as plain Markdown rather
// than duplicating their long strings in transaction code; the PostgreSQL
// harness verifies each persisted content digest.
struct StarterPageDefinition {
    title: &'static str,
    slug: &'static str,
    icon: &'static str,
    content: &'static str,
    position: i32,
}

const STARTER_TOP_LEVEL_PAGES: [StarterPageDefinition; 3] = [
    StarterPageDefinition {
        title: "Getting started",
        slug: "getting-started",
        icon: "rocket",
        content: include_str!("starter_template/getting-started.md"),
        position: 0,
    },
    StarterPageDefinition {
        title: "Installation",
        slug: "installation",
        icon: "download",
        content: include_str!("starter_template/installation.md"),
        position: 1,
    },
    StarterPageDefinition {
        title: "Components tour",
        slug: "components-tour",
        icon: "puzzle",
        content: include_str!("starter_template/components-tour.md"),
        position: 2,
    },
];

const STARTER_API_CHILDREN: [StarterPageDefinition; 2] = [
    StarterPageDefinition {
        title: "Authentication",
        slug: "authentication",
        icon: "key",
        content: include_str!("starter_template/api-authentication.md"),
        position: 0,
    },
    StarterPageDefinition {
        title: "Create a widget",
        slug: "create-a-widget",
        icon: "sparkles",
        content: include_str!("starter_template/api-create-widget.md"),
        position: 1,
    },
];

const STARTER_FAQ: StarterPageDefinition = StarterPageDefinition {
    title: "FAQ",
    slug: "faq",
    icon: "help-circle",
    content: include_str!("starter_template/faq.md"),
    position: 4,
};

struct ProjectFoundation {
    organization_id: OrganizationId,
    project_id: ProjectId,
    language_id: String,
    branch_id: String,
    slug: String,
}

struct StarterPageScope<'a> {
    project: &'a str,
    language: &'a str,
    branch: &'a str,
}

struct StarterPageParent<'a> {
    id: &'a str,
    path: &'a str,
}

/// VSA route composition: handlers are adjacent to typed input, transaction,
/// persistence statements, and their authorization requirement.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects", post(create_project).get(list_projects))
        .route("/projects/{project_id}/onboarding", get(get_onboarding))
}

async fn create_project(
    State(state): State<AppState>,
    actor: Actor,
    StrictJson(request): StrictJson<CreateProjectRequest>,
) -> Result<(StatusCode, Json<Data<ProjectResponse>>), AppError> {
    let name = validate_project_name(&request.name)?;
    let description = request
        .description
        .as_deref()
        .map(validate_description)
        .transpose()?;

    let project = create_project_transaction(
        &state.pool,
        &actor.user_id,
        name.clone(),
        name.clone(),
        name,
        description,
        ProjectBootstrap::Empty,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(Data { data: project })))
}

async fn create_project_transaction(
    pool: &sqlx::PgPool,
    actor_id: &UserId,
    organization_name: String,
    project_name: String,
    slug_seed: String,
    description: Option<String>,
    bootstrap: ProjectBootstrap,
) -> Result<ProjectResponse, AppError> {
    let mut transaction = pool.begin().await?;
    let foundation = create_project_foundation(
        &mut transaction,
        actor_id,
        &organization_name,
        &project_name,
        &slug_seed,
        description.as_deref(),
    )
    .await?;
    if bootstrap == ProjectBootstrap::NativeStarter {
        let scope = StarterPageScope {
            project: foundation.project_id.as_str(),
            language: &foundation.language_id,
            branch: &foundation.branch_id,
        };
        insert_native_starter_pages(&mut transaction, &scope).await?;
    }
    let onboarding_id = create_onboarding_run(&mut transaction, &foundation, actor_id).await?;
    append_project_initialized(&mut transaction, &foundation, onboarding_id).await?;
    if bootstrap == ProjectBootstrap::NativeStarter {
        append_starter_publication_request(&mut transaction, &foundation, actor_id).await?;
    }
    transaction.commit().await?;
    Ok(ProjectResponse {
        id: foundation.project_id.to_string(),
        organization_id: foundation.organization_id.to_string(),
        name: project_name,
        slug: foundation.slug,
        description,
    })
}

async fn create_project_foundation(
    transaction: &mut Transaction<'_, Postgres>,
    actor_id: &UserId,
    organization_name: &str,
    project_name: &str,
    slug_seed: &str,
    description: Option<&str>,
) -> Result<ProjectFoundation, AppError> {
    let organization_id = OrganizationId::new();
    let project_id = ProjectId::new();
    let slug = allocate_slug(transaction, slug_seed).await?;
    sqlx::query("INSERT INTO organization (id, name) VALUES ($1, $2)")
        .bind(organization_id.as_str())
        .bind(organization_name)
        .execute(&mut **transaction)
        .await?;
    sqlx::query(
        r#"INSERT INTO member (id, "organizationId", "userId", role, "createdAt")
           VALUES ($1, $2, $3, 'owner', NOW())"#,
    )
    .bind(Uuid::now_v7().to_string())
    .bind(organization_id.as_str())
    .bind(actor_id.as_str())
    .execute(&mut **transaction)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO project (
            id, "organizationId", name, slug, description, config, "accessMode", "createdAt", "updatedAt"
        ) VALUES ($1, $2, $3, $4, $5, '{"theme":{"version":1,"preset":"harbor"}}'::jsonb,
                  'PUBLIC'::"ProjectAccessMode", NOW(), NOW())
        "#,
    )
    .bind(project_id.as_str())
    .bind(organization_id.as_str())
    .bind(project_name)
    .bind(&slug)
    .bind(description)
    .execute(&mut **transaction)
    .await?;
    let language_id = Uuid::now_v7().to_string();
    sqlx::query(
        r#"
        INSERT INTO language (id, "projectId", code, label, direction, "isDefault", enabled, position, "createdAt", "updatedAt")
        VALUES ($1, $2, 'en', 'English', 'LTR'::"TextDirection", TRUE, TRUE, 0, NOW(), NOW())
        "#,
    )
    .bind(&language_id)
    .bind(project_id.as_str())
    .execute(&mut **transaction)
    .await?;
    let branch_id = Uuid::now_v7().to_string();
    sqlx::query(
        r#"
        INSERT INTO branch (id, "projectId", name, "isDefault", "createdAt", "updatedAt")
        VALUES ($1, $2, 'main', TRUE, NOW(), NOW())
        "#,
    )
    .bind(&branch_id)
    .bind(project_id.as_str())
    .execute(&mut **transaction)
    .await?;
    Ok(ProjectFoundation {
        organization_id,
        project_id,
        language_id,
        branch_id,
        slug,
    })
}

async fn create_onboarding_run(
    transaction: &mut Transaction<'_, Postgres>,
    foundation: &ProjectFoundation,
    actor_id: &UserId,
) -> Result<Uuid, AppError> {
    let onboarding_id = Uuid::now_v7();
    sqlx::query(
        r"
        INSERT INTO onboarding_run (id, project_id, tenant_id, state, created_by, created_at, updated_at)
        VALUES ($1, $2, $3, 'database_initialized', $4, NOW(), NOW())
        ",
    )
    .bind(onboarding_id)
    .bind(foundation.project_id.as_str())
    .bind(foundation.organization_id.as_str())
    .bind(actor_id.as_str())
    .execute(&mut **transaction)
    .await?;
    Ok(onboarding_id)
}

async fn append_project_initialized(
    transaction: &mut Transaction<'_, Postgres>,
    foundation: &ProjectFoundation,
    onboarding_id: Uuid,
) -> Result<(), AppError> {
    append(
        transaction,
        NewEvent {
            tenant_id: Some(foundation.organization_id.as_str()),
            aggregate_type: "project",
            aggregate_id: foundation.project_id.as_str(),
            event_type: "project.initialized",
            schema_version: 1,
            payload: serde_json::json!({
                "project_id": foundation.project_id.as_str(),
                "onboarding_run_id": onboarding_id,
            }),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .map(|_| ())
}

async fn append_starter_publication_request(
    transaction: &mut Transaction<'_, Postgres>,
    foundation: &ProjectFoundation,
    actor_id: &UserId,
) -> Result<(), AppError> {
    append(
        transaction,
        NewEvent {
            tenant_id: Some(foundation.organization_id.as_str()),
            aggregate_type: "project",
            aggregate_id: foundation.project_id.as_str(),
            event_type: "starter.publication.requested",
            schema_version: 1,
            payload: serde_json::json!({
                "project_id": foundation.project_id.as_str(),
                "created_by_user_id": actor_id.as_str(),
            }),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .map(|_| ())
}

async fn insert_native_starter_pages(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &StarterPageScope<'_>,
) -> Result<(), AppError> {
    for page in STARTER_TOP_LEVEL_PAGES {
        insert_starter_page(transaction, scope, None, "PAGE", &page).await?;
    }
    let api_group = StarterPageDefinition {
        title: "API reference",
        slug: "api-reference",
        icon: "code",
        content: "",
        position: 3,
    };
    let api_group_id = insert_starter_page(transaction, scope, None, "GROUP", &api_group).await?;
    let api_group_parent = StarterPageParent {
        id: &api_group_id,
        path: api_group.slug,
    };
    for page in STARTER_API_CHILDREN {
        insert_starter_page(transaction, scope, Some(&api_group_parent), "PAGE", &page).await?;
    }
    insert_starter_page(transaction, scope, None, "PAGE", &STARTER_FAQ).await?;
    Ok(())
}

async fn insert_starter_page(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &StarterPageScope<'_>,
    parent: Option<&StarterPageParent<'_>>,
    kind: &str,
    page: &StarterPageDefinition,
) -> Result<String, AppError> {
    let id = Uuid::now_v7().to_string();
    let path = parent.map_or_else(
        || page.slug.to_owned(),
        |parent| format!("{}/{}", parent.path, page.slug),
    );
    sqlx::query(
        r#"
        INSERT INTO page (
            id, "projectId", "languageId", "branchId", "parentId", kind,
            title, slug, path, icon, content, position, "createdAt", "updatedAt"
        ) VALUES ($1, $2, $3, $4, $5, $6::"PageKind", $7, $8, $9, $10, $11, $12, NOW(), NOW())
        "#,
    )
    .bind(&id)
    .bind(scope.project)
    .bind(scope.language)
    .bind(scope.branch)
    .bind(parent.map(|parent| parent.id))
    .bind(kind)
    .bind(page.title)
    .bind(page.slug)
    .bind(&path)
    .bind(page.icon)
    .bind(page.content)
    .bind(page.position)
    .execute(&mut **transaction)
    .await?;
    Ok(id)
}

async fn list_projects(
    State(state): State<AppState>,
    actor: Actor,
) -> Result<Json<Data<Vec<ProjectResponse>>>, AppError> {
    let rows = sqlx::query_as::<_, ProjectResponse>(
        r#"
        SELECT p.id, p."organizationId" AS organization_id, p.name, p.slug, p.description
        FROM project AS p
        INNER JOIN member AS m ON m."organizationId" = p."organizationId"
        WHERE m."userId" = $1
        ORDER BY p."updatedAt" DESC
        LIMIT 100
        "#,
    )
    .bind(actor.user_id.as_str())
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(Data { data: rows }))
}

async fn get_onboarding(
    State(state): State<AppState>,
    actor: Actor,
    Path(project_id): Path<String>,
) -> Result<Json<Data<OnboardingResponse>>, AppError> {
    let project_id = ProjectId::try_from(project_id)?;
    let scope = require_project_role(&state.pool, &actor, project_id, Role::Member).await?;
    let row = sqlx::query_as::<_, OnboardingResponse>(
        r"
        SELECT id, project_id, state, last_error
        FROM onboarding_run
        WHERE project_id = $1 AND tenant_id = $2
        ORDER BY created_at DESC
        LIMIT 1
        ",
    )
    .bind(scope.project_id.as_str())
    .bind(scope.organization_id.as_str())
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::not_found("onboarding run"))?;
    Ok(Json(Data { data: row }))
}

/// Worker-side account-onboarding consumer. The identity feature creates the
/// user and this event atomically; project provisioning runs asynchronously as
/// in the verified legacy hook. Existing membership or a pending invitation
/// suppresses an unrelated starter workspace.
///
/// # Errors
/// Returns a permanent failure for malformed platform-scoped events and a
/// retryable failure when PostgreSQL cannot inspect or create the workspace.
pub async fn provision_identity_account(
    pool: &sqlx::PgPool,
    event: &ClaimedEvent,
) -> Result<(), Failure> {
    if event.tenant_id.is_some() {
        return Err(Failure::Permanent {
            message: "identity account provisioning must be platform-scoped".to_owned(),
        });
    }
    let user_id = event
        .payload
        .get("user_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| Failure::Permanent {
            message: "identity account provisioning payload is missing user_id".to_owned(),
        })?
        .parse::<UserId>()
        .map_err(|_| Failure::Permanent {
            message: "identity account provisioning user_id is invalid".to_owned(),
        })?;
    let user =
        sqlx::query_as::<_, (String, String)>("SELECT email, name FROM \"user\" WHERE id=$1")
            .bind(user_id.as_str())
            .fetch_optional(pool)
            .await
            .map_err(|_| Failure::Retryable {
                message: "database unavailable while loading identity account".to_owned(),
            })?
            .ok_or_else(|| Failure::Permanent {
                message: "identity account no longer exists".to_owned(),
            })?;
    let already_member: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM member WHERE \"userId\"=$1)")
            .bind(user_id.as_str())
            .fetch_one(pool)
            .await
            .map_err(|_| Failure::Retryable {
                message: "database unavailable while checking identity membership".to_owned(),
            })?;
    if already_member {
        return Ok(());
    }
    let invited: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
            SELECT 1 FROM invitation
             WHERE lower(email)=$1 AND status='pending' AND "expiresAt" > NOW()
            UNION ALL
            SELECT 1 FROM member_invitation
             WHERE recipient_email=$1 AND status='pending' AND expires_at > NOW()
        )"#,
    )
    .bind(user.0.trim().to_ascii_lowercase())
    .fetch_one(pool)
    .await
    .map_err(|_| Failure::Retryable {
        message: "database unavailable while checking identity invitation".to_owned(),
    })?;
    if invited {
        return Ok(());
    }
    let workspace_name = workspace_name(&user.1);
    create_project_transaction(
        pool,
        &user_id,
        workspace_name,
        "Documentation".to_owned(),
        "docs".to_owned(),
        Some("Your first documentation site.".to_owned()),
        ProjectBootstrap::NativeStarter,
    )
    .await
    .map(|_| ())
    .map_err(|_| Failure::Retryable {
        message: "database unavailable while provisioning identity workspace".to_owned(),
    })
}

fn workspace_name(display_name: &str) -> String {
    let first = display_name.split_whitespace().next().unwrap_or_default();
    if first.is_empty() {
        "My workspace".to_owned()
    } else {
        format!("{first}'s workspace")
    }
}

/// Worker-side consumer for the durable initialization step. The database
/// creation itself was atomic in `create_project`; this follow-up state records
/// that asynchronous provisioning has been observed. Re-delivery is harmless.
///
/// # Errors
///
/// Returns a permanent failure for malformed or inconsistent events and a
/// retryable failure when PostgreSQL cannot persist completion.
pub async fn complete_onboarding(pool: &sqlx::PgPool, event: &ClaimedEvent) -> Result<(), Failure> {
    let run_id = event
        .payload
        .get("onboarding_run_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| Failure::Permanent {
            message: "project.initialized payload is missing onboarding_run_id".to_owned(),
        })?
        .parse::<Uuid>()
        .map_err(|_| Failure::Permanent {
            message: "project.initialized onboarding_run_id is invalid".to_owned(),
        })?;
    let project_id = event
        .payload
        .get("project_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| Failure::Permanent {
            message: "project.initialized payload is missing project_id".to_owned(),
        })?;
    if project_id != event.aggregate_id {
        return Err(Failure::Permanent {
            message: "project.initialized aggregate mismatch".to_owned(),
        });
    }
    let result = sqlx::query(
        r"
        UPDATE onboarding_run
        SET state = 'complete', updated_at = NOW(), last_error = NULL
        WHERE id = $1 AND project_id = $2 AND tenant_id = $3
          AND state IN ('database_initialized', 'complete')
        ",
    )
    .bind(run_id)
    .bind(project_id)
    .bind(&event.tenant_id)
    .execute(pool)
    .await
    .map_err(|_| Failure::Retryable {
        message: "database unavailable while completing onboarding".to_owned(),
    })?;
    if result.rows_affected() != 1 {
        return Err(Failure::Permanent {
            message: "onboarding run no longer exists or has an invalid state".to_owned(),
        });
    }
    Ok(())
}

fn validate_project_name(value: &str) -> Result<String, AppError> {
    let name = value.trim();
    if !(1..=120).contains(&name.chars().count()) {
        return Err(AppError::validation(
            "name must contain 1 to 120 characters",
        ));
    }
    Ok(name.to_owned())
}

fn validate_description(value: &str) -> Result<String, AppError> {
    let value = value.trim().to_owned();
    if value.chars().count() > 1_000 {
        return Err(AppError::validation(
            "description must not exceed 1000 characters",
        ));
    }
    Ok(value)
}

fn slug_base(name: &str) -> String {
    let mut output = String::new();
    let mut dashed = false;
    for character in name.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            output.push(character);
            dashed = false;
        } else if !output.is_empty() && !dashed {
            output.push('-');
            dashed = true;
        }
    }
    output
        .trim_matches('-')
        .chars()
        .take(50)
        .collect::<String>()
        .trim_end_matches('-')
        .to_owned()
}

async fn allocate_slug(
    transaction: &mut Transaction<'_, Postgres>,
    name: &str,
) -> Result<String, AppError> {
    let base = {
        let candidate = slug_base(name);
        if candidate.is_empty() {
            "docs".to_owned()
        } else {
            candidate
        }
    };
    for number in 1..=100_i32 {
        let candidate = if number == 1 {
            base.clone()
        } else {
            format!("{base}-{number}")
        };
        let taken: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM project WHERE slug = $1)")
                .bind(&candidate)
                .fetch_one(&mut **transaction)
                .await?;
        if !taken {
            return Ok(candidate);
        }
    }
    Ok(format!(
        "{base}-{}",
        &Uuid::now_v7().simple().to_string()[..8]
    ))
}

#[cfg(test)]
mod tests {
    use super::{slug_base, validate_project_name};

    #[test]
    fn slug_is_bounded_and_stable() {
        assert_eq!(slug_base("  Docs / API -- Site "), "docs-api-site");
        assert_eq!(slug_base("💡"), "");
    }

    #[test]
    fn name_boundary_is_checked() {
        assert!(validate_project_name(" ").is_err());
        assert!(validate_project_name("valid").is_ok());
    }
}
