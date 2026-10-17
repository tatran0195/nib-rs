#![forbid(unsafe_code)]

//! Draft content vertical slice: typed input, tenant authorization, optimistic
//! concurrency, and direct SQL projections live together.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use nibleaf_core::{
    Actor, AppError, AppState, PageId, ProjectId, Role, StrictJson, require_project_role,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Postgres, Transaction};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum PageKind {
    Page,
    Group,
}

impl PageKind {
    fn as_db(&self) -> &'static str {
        match self {
            Self::Page => "PAGE",
            Self::Group => "GROUP",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreatePageRequest {
    language_id: String,
    title: String,
    slug: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    kind: Option<PageKind>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdatePageRequest {
    expected_revision: i64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListPagesQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
struct PageResponse {
    id: String,
    language_id: String,
    branch_id: String,
    parent_id: Option<String>,
    kind: String,
    title: String,
    slug: String,
    path: String,
    content: String,
    position: i32,
    revision: i64,
}

#[derive(Debug, Serialize)]
struct Data<T> {
    data: T,
}

#[derive(Debug, Serialize)]
struct PageList {
    items: Vec<PageResponse>,
    next_cursor: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{project_id}/pages",
            post(create_page).get(list_pages),
        )
        .route(
            "/projects/{project_id}/pages/{page_id}",
            get(get_page).patch(update_page),
        )
}

async fn create_page(
    State(state): State<AppState>,
    actor: Actor,
    Path(project_id): Path<String>,
    StrictJson(request): StrictJson<CreatePageRequest>,
) -> Result<(StatusCode, Json<Data<PageResponse>>), AppError> {
    let project_id = ProjectId::try_from(project_id)?;
    let scope = require_project_role(&state.pool, &actor, project_id, Role::Member).await?;
    let title = validate_title(&request.title)?;
    let slug = validate_slug(&request.slug)?;
    validate_content(&request.content)?;
    let language_id = require_opaque(&request.language_id, "language id")?;
    let parent_id = request
        .parent_id
        .map(|value| require_opaque(&value, "parent id"))
        .transpose()?;
    let page_id = PageId::new();

    let mut transaction = state.pool.begin().await?;
    ensure_language(&mut transaction, scope.project_id.as_str(), &language_id).await?;
    let branch_id: String = sqlx::query_scalar(
        r#"SELECT id FROM branch WHERE "projectId" = $1 AND "isDefault" = TRUE LIMIT 1"#,
    )
    .bind(scope.project_id.as_str())
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(|| AppError::conflict("project is missing its required default branch"))?;

    let parent = if let Some(parent_id) = &parent_id {
        let row = sqlx::query_as::<_, ParentRow>(
            r#"
            SELECT id, path, "languageId" AS language_id, "branchId" AS branch_id
            FROM page
            WHERE id = $1 AND "projectId" = $2
            "#,
        )
        .bind(parent_id)
        .bind(scope.project_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| AppError::not_found("parent page"))?;
        if row.language_id != language_id || row.branch_id != branch_id {
            return Err(AppError::validation(
                "a child page must use its parent's language and default branch",
            ));
        }
        Some(row)
    } else {
        None
    };
    let path = match &parent {
        Some(parent) => format!("{}/{}", parent.path.trim_matches('/'), slug),
        None => slug.clone(),
    };
    let position: i32 = sqlx::query_scalar(
        r#"
        SELECT COALESCE(MAX(position), -1) + 1
        FROM page
        WHERE "projectId" = $1 AND "languageId" = $2 AND "branchId" = $3
          AND "parentId" IS NOT DISTINCT FROM $4
        "#,
    )
    .bind(scope.project_id.as_str())
    .bind(&language_id)
    .bind(&branch_id)
    .bind(parent_id.as_deref())
    .fetch_one(&mut *transaction)
    .await?;

    let result = sqlx::query_as::<_, PageResponse>(
        r#"
        INSERT INTO page (
            id, "projectId", "languageId", "branchId", "parentId", kind, title, slug,
            path, content, position, revision, "createdAt", "updatedAt"
        ) VALUES (
            $1, $2, $3, $4, $5, $6::"PageKind", $7, $8, $9, $10, $11, 0, NOW(), NOW()
        )
        RETURNING id, "languageId" AS language_id, "branchId" AS branch_id,
                  "parentId" AS parent_id, kind::text AS kind, title, slug, path, content, position, revision
        "#,
    )
    .bind(page_id.as_str())
    .bind(scope.project_id.as_str())
    .bind(&language_id)
    .bind(&branch_id)
    .bind(parent_id.as_deref())
    .bind(request.kind.unwrap_or(PageKind::Page).as_db())
    .bind(&title)
    .bind(&slug)
    .bind(&path)
    .bind(&request.content)
    .bind(position)
    .fetch_one(&mut *transaction)
    .await
    .map_err(map_page_write_error)?;
    transaction.commit().await?;

    Ok((StatusCode::CREATED, Json(Data { data: result })))
}

async fn list_pages(
    State(state): State<AppState>,
    actor: Actor,
    Path(project_id): Path<String>,
    Query(query): Query<ListPagesQuery>,
) -> Result<Json<Data<PageList>>, AppError> {
    let project_id = ProjectId::try_from(project_id)?;
    let scope = require_project_role(&state.pool, &actor, project_id, Role::Member).await?;
    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    let (after_position, after_id) = parse_cursor(query.cursor.as_deref())?;
    let mut rows = sqlx::query_as::<_, PageResponse>(
        r#"
        SELECT id, "languageId" AS language_id, "branchId" AS branch_id,
               "parentId" AS parent_id, kind::text AS kind, title, slug, path,
               content, position, revision
        FROM page
        WHERE "projectId" = $1
          AND "branchId" = (SELECT id FROM branch WHERE "projectId" = $1 AND "isDefault" = TRUE LIMIT 1)
          AND ($2::integer IS NULL OR position > $2 OR (position = $2 AND id > $3))
        ORDER BY position ASC, id ASC
        LIMIT $4
        "#,
    )
    .bind(scope.project_id.as_str())
    .bind(after_position)
    .bind(after_id)
    .bind(i64::try_from(limit + 1).map_err(|_| AppError::validation("page limit is too large"))?)
    .fetch_all(&state.pool)
    .await?;
    let next_cursor = if rows.len() > limit {
        let extra = rows.pop().ok_or(AppError::Internal)?;
        rows.last()
            .map(|page| format!("{}:{}", page.position, page.id))
            .or_else(|| Some(format!("{}:{}", extra.position, extra.id)))
    } else {
        None
    };
    Ok(Json(Data {
        data: PageList {
            items: rows,
            next_cursor,
        },
    }))
}

async fn get_page(
    State(state): State<AppState>,
    actor: Actor,
    Path((project_id, page_id)): Path<(String, String)>,
) -> Result<Json<Data<PageResponse>>, AppError> {
    let project_id = ProjectId::try_from(project_id)?;
    let scope = require_project_role(&state.pool, &actor, project_id, Role::Member).await?;
    let page_id = PageId::try_from(page_id)?;
    let page = fetch_page(&state.pool, scope.project_id.as_str(), page_id.as_str()).await?;
    Ok(Json(Data { data: page }))
}

async fn update_page(
    State(state): State<AppState>,
    actor: Actor,
    Path((project_id, page_id)): Path<(String, String)>,
    StrictJson(request): StrictJson<UpdatePageRequest>,
) -> Result<Json<Data<PageResponse>>, AppError> {
    if request.expected_revision < 0 {
        return Err(AppError::validation(
            "expected_revision must not be negative",
        ));
    }
    if request.title.is_none() && request.content.is_none() {
        return Err(AppError::validation(
            "at least one mutable field is required",
        ));
    }
    let title = request
        .title
        .map(|value| validate_title(&value))
        .transpose()?;
    if let Some(content) = &request.content {
        validate_content(content)?;
    }
    let project_id = ProjectId::try_from(project_id)?;
    let scope = require_project_role(&state.pool, &actor, project_id, Role::Member).await?;
    let page_id = PageId::try_from(page_id)?;
    let updated = sqlx::query_as::<_, PageResponse>(
        r#"
        UPDATE page
        SET title = COALESCE($3, title),
            content = COALESCE($4, content),
            revision = revision + 1,
            "updatedAt" = NOW()
        WHERE id = $1 AND "projectId" = $2 AND revision = $5
        RETURNING id, "languageId" AS language_id, "branchId" AS branch_id,
                  "parentId" AS parent_id, kind::text AS kind, title, slug, path, content, position, revision
        "#,
    )
    .bind(page_id.as_str())
    .bind(scope.project_id.as_str())
    .bind(title)
    .bind(request.content)
    .bind(request.expected_revision)
    .fetch_optional(&state.pool)
    .await?;
    let Some(page) = updated else {
        // An authorized caller gets an explicit optimistic-concurrency
        // conflict only if the page still exists in their tenant.
        let _current = fetch_page(&state.pool, scope.project_id.as_str(), page_id.as_str()).await?;
        return Err(AppError::conflict(
            "page was modified by another author; refresh and retry",
        ));
    };
    Ok(Json(Data { data: page }))
}

#[derive(FromRow)]
struct ParentRow {
    #[allow(dead_code)]
    id: String,
    path: String,
    language_id: String,
    branch_id: String,
}

async fn ensure_language(
    transaction: &mut Transaction<'_, Postgres>,
    project_id: &str,
    language_id: &str,
) -> Result<(), AppError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM language WHERE id = $1 AND \"projectId\" = $2)",
    )
    .bind(language_id)
    .bind(project_id)
    .fetch_one(&mut **transaction)
    .await?;
    if !exists {
        return Err(AppError::not_found("language"));
    }
    Ok(())
}

async fn fetch_page(
    pool: &sqlx::PgPool,
    project_id: &str,
    page_id: &str,
) -> Result<PageResponse, AppError> {
    sqlx::query_as::<_, PageResponse>(
        r#"
        SELECT id, "languageId" AS language_id, "branchId" AS branch_id,
               "parentId" AS parent_id, kind::text AS kind, title, slug, path,
               content, position, revision
        FROM page
        WHERE id = $1 AND "projectId" = $2
        "#,
    )
    .bind(page_id)
    .bind(project_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::not_found("page"))
}

fn map_page_write_error(error: sqlx::Error) -> AppError {
    if let sqlx::Error::Database(database) = &error
        && database.code().as_deref() == Some("23505")
    {
        return AppError::conflict(
            "a page with this path already exists in the selected language and branch",
        );
    }
    AppError::Database(error)
}

fn require_opaque(value: &str, label: &str) -> Result<String, AppError> {
    if value.is_empty()
        || value.len() > 191
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(AppError::validation(format!("invalid {label}")));
    }
    Ok(value.to_owned())
}

fn validate_title(value: &str) -> Result<String, AppError> {
    let value = value.trim();
    if !(1..=200).contains(&value.chars().count()) {
        return Err(AppError::validation(
            "title must contain 1 to 200 characters",
        ));
    }
    Ok(value.to_owned())
}

fn validate_slug(value: &str) -> Result<String, AppError> {
    let value = value.trim();
    if !(1..=120).contains(&value.len())
        || !value.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
        || value.starts_with('-')
        || value.ends_with('-')
    {
        return Err(AppError::validation(
            "slug must use lowercase ASCII letters, digits, and interior hyphens",
        ));
    }
    Ok(value.to_owned())
}

fn validate_content(value: &str) -> Result<(), AppError> {
    if value.len() > 1_000_000 {
        return Err(AppError::validation("content must not exceed 1 MiB"));
    }
    Ok(())
}

fn parse_cursor(value: Option<&str>) -> Result<(Option<i32>, Option<String>), AppError> {
    let Some(value) = value else {
        return Ok((None, None));
    };
    let (position, id) = value
        .split_once(':')
        .ok_or_else(|| AppError::validation("invalid page cursor"))?;
    let position = position
        .parse::<i32>()
        .map_err(|_| AppError::validation("invalid page cursor"))?;
    let id = require_opaque(id, "page cursor")?;
    Ok((Some(position), Some(id)))
}

#[cfg(test)]
mod tests {
    use super::{parse_cursor, validate_slug};

    #[test]
    fn rejects_unsafe_slug() {
        assert!(validate_slug("../private").is_err());
        assert!(validate_slug("ok-slug-2").is_ok());
    }

    #[test]
    fn cursor_is_strict() {
        assert_eq!(
            parse_cursor(Some("3:page-a")).ok(),
            Some((Some(3), Some("page-a".to_owned())))
        );
        assert!(parse_cursor(Some("bad")).is_err());
    }
}
