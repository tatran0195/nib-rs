//! Authenticated project-member search transport for active native generations.
//!
//! This is deliberately not the public-site replacement route: public reader
//! access is a separate security-sensitive vertical slice. It gives an already
//! tenant-authorized project member a typed, no-store query surface without
//! widening filesystem or index scope beyond the registry-selected generation.

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderValue, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
    routing::get,
};
use nibleaf_core::{Actor, AppError, AppState, ProjectId};
use serde::{Deserialize, Serialize};

use crate::{SearchError, SearchQueryExecutionError, SearchQueryService};

const DEFAULT_RESULT_LIMIT: usize = 12;
const MAX_QUERY_BYTES: usize = 512;
const MAX_SCOPE_BYTES: usize = 191;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectSearchQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct Data<T> {
    data: T,
}

#[derive(Debug, Serialize)]
struct ProjectSearchResponse {
    deployment_id: String,
    generation_id: String,
    hits: Vec<ProjectSearchHit>,
}

#[derive(Debug, Serialize)]
struct ProjectSearchHit {
    version_id: String,
    version_slug: String,
    language: String,
    page_id: String,
    path: String,
    score: f32,
    title: String,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    snippet: String,
}

#[derive(Debug)]
pub(crate) struct ValidatedSearchQuery {
    pub(crate) q: String,
    pub(crate) language: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) limit: usize,
}

/// Returns the Axum routes for tenant-authorized queries of the active native
/// generation. It contains no public-reader admission; adding a public endpoint
/// without the legacy reader scope port would be a confidentiality regression.
pub fn router(queries: SearchQueryService) -> Router<AppState> {
    Router::new()
        .route("/projects/{project_id}/search", get(search_project))
        .layer(Extension(queries))
}

async fn search_project(
    State(_state): State<AppState>,
    Extension(queries): Extension<SearchQueryService>,
    actor: Actor,
    Path(project_id): Path<String>,
    query: Result<Query<ProjectSearchQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Response, AppError> {
    let project_id = ProjectId::try_from(project_id)?;
    let query = query
        .map_err(|_| AppError::validation("search query parameters are invalid"))
        .and_then(|Query(query)| validate_query(query));
    let result = queries
        .search_member(project_id, actor, query)
        .await
        .map_err(execution_error)?;
    let hits = result
        .hits
        .into_iter()
        .map(|hit| ProjectSearchHit {
            version_id: hit.version_id,
            version_slug: hit.version_slug,
            language: hit.language,
            page_id: hit.page_id,
            path: hit.path,
            score: hit.score,
            title: hit.title,
            description: hit.description,
            icon: hit.icon,
            snippet: hit.snippet,
        })
        .collect();
    let mut response = Json(Data {
        data: ProjectSearchResponse {
            deployment_id: result.deployment_id,
            generation_id: result.generation_id.to_string(),
            hits,
        },
    })
    .into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    Ok(response)
}

fn validate_query(request: ProjectSearchQuery) -> Result<ValidatedSearchQuery, AppError> {
    let query = request.q.trim().to_owned();
    if query.is_empty() || query.len() > MAX_QUERY_BYTES {
        return Err(AppError::validation(
            "q must be non-empty and at most 512 bytes",
        ));
    }
    let language = request
        .language
        .map(|language| language.trim().to_owned())
        .filter(|language| !language.is_empty());
    if language
        .as_deref()
        .is_some_and(|language| language.len() > MAX_SCOPE_BYTES)
    {
        return Err(AppError::validation("language must be at most 191 bytes"));
    }
    let limit = request.limit.unwrap_or(DEFAULT_RESULT_LIMIT);
    if !(1..=50).contains(&limit) {
        return Err(AppError::validation("limit must be between 1 and 50"));
    }
    let version = request
        .version
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if version
        .as_deref()
        .is_some_and(|value| value.len() > MAX_SCOPE_BYTES)
    {
        return Err(AppError::validation("version must be at most 191 bytes"));
    }
    Ok(ValidatedSearchQuery {
        q: query,
        language,
        version,
        limit,
    })
}

fn execution_error(error: SearchQueryExecutionError) -> AppError {
    match error {
        SearchQueryExecutionError::Member(error) => error,
        SearchQueryExecutionError::Search(error) => search_error(&error),
        SearchQueryExecutionError::Busy
        | SearchQueryExecutionError::Stopping
        | SearchQueryExecutionError::Deadline
        | SearchQueryExecutionError::Cancelled
        | SearchQueryExecutionError::WorkerFailed
        | SearchQueryExecutionError::Published(_) => AppError::Unavailable,
    }
}

fn search_error(error: &SearchError) -> AppError {
    match error {
        SearchError::InvalidInput(_) | SearchError::Query(_) => {
            AppError::validation("search query is invalid")
        }
        SearchError::Distribution(_)
        | SearchError::Cancelled
        | SearchError::Deadline
        | SearchError::ResourceLimit(_)
        | SearchError::Integrity(_)
        | SearchError::Storage(_)
        | SearchError::Manifest(_)
        | SearchError::Tantivy(_)
        | SearchError::Tokenizer(_) => AppError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::{ProjectSearchQuery, validate_query};

    #[test]
    fn runtime_failures_keep_the_management_problem_contract() {
        for error in [
            super::SearchQueryExecutionError::Busy,
            super::SearchQueryExecutionError::Stopping,
            super::SearchQueryExecutionError::Deadline,
            super::SearchQueryExecutionError::Cancelled,
            super::SearchQueryExecutionError::WorkerFailed,
            super::SearchQueryExecutionError::Search(crate::SearchError::ResourceLimit("bound")),
        ] {
            assert_eq!(
                super::execution_error(error).status(),
                axum::http::StatusCode::SERVICE_UNAVAILABLE
            );
        }
        assert_eq!(
            super::execution_error(super::SearchQueryExecutionError::Member(
                nibleaf_core::AppError::not_found("project")
            ))
            .status(),
            axum::http::StatusCode::NOT_FOUND
        );
        assert_eq!(
            super::execution_error(crate::SearchError::InvalidInput("query").into()).status(),
            axum::http::StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[test]
    fn query_contract_is_bounded_and_defaults_to_twelve_results()
    -> Result<(), nibleaf_core::AppError> {
        let query = validate_query(ProjectSearchQuery {
            q: "  Tokyo  ".to_owned(),
            language: Some(" ja ".to_owned()),
            version: Some(" main ".to_owned()),
            limit: None,
        })?;
        assert_eq!(query.q, "Tokyo");
        assert_eq!(query.language.as_deref(), Some("ja"));
        assert_eq!(query.version.as_deref(), Some("main"));
        assert_eq!(query.limit, 12);
        assert!(
            validate_query(ProjectSearchQuery {
                q: " ".to_owned(),
                language: None,
                version: None,
                limit: Some(1),
            })
            .is_err()
        );
        assert!(
            validate_query(ProjectSearchQuery {
                q: "valid".to_owned(),
                language: None,
                version: None,
                limit: Some(51),
            })
            .is_err()
        );
        assert!(
            validate_query(ProjectSearchQuery {
                q: "valid".to_owned(),
                language: None,
                version: Some("v".repeat(192)),
                limit: None
            })
            .is_err()
        );
        assert!(
            serde_json::from_value::<ProjectSearchQuery>(
                serde_json::json!({"q":"valid", "allowed_page_ids":["page"]})
            )
            .is_err()
        );
        Ok(())
    }
}
