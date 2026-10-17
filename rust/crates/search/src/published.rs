//! Non-HTTP, read-only published-search composition.
//!
//! Admission, latest-READY selection and live restrictions share one short
//! REPEATABLE READ / READ ONLY transaction. It is committed before a one-shot
//! prepared operation is returned for caller-owned blocking execution. This is not
//! a public route, a session transport, or a serialized public API contract.

use std::{fmt, path::Path};

use axum::http::HeaderMap;
use nibleaf_core::{Actor, AppError, ProjectId};
use serde_json::Value;
use sqlx::{Connection, FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    MAX_RESULTS, PublishedSearchAdmission, PublishedSearchContext, SearchError, SearchHit,
    execution::budget::{QueryBudget, checkpoint},
    open_generation_with_budget,
    public_admission::admit_on_connection_with_limit,
    publication_scope::{LiveSearchControls, resolve_context},
    query::search_published_generation_with_budget,
    registry::{RegistryError, canonical_snapshot_sha256},
    snapshot::snapshot_scope,
};

/// Narrow app-composed identity port. It authenticates optional workspace
/// credentials on the already leased/read-only connection, never page grants.
/// Implementations must not start tasks, commit, or perform writes.
pub trait PublishedActorResolver: Send + Sync + fmt::Debug {
    fn resolve<'a>(
        &'a self,
        connection: &'a mut PgConnection,
        headers: &'a HeaderMap,
    ) -> ActorResolution<'a>;
}

pub type ActorResolution<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Option<Actor>, AppError>> + Send + 'a>,
>;

/// Client-selectable input for the library composition, not an HTTP DTO.
/// The selected public policy is at most 200 UTF-16 units with blank queries
/// allowed. Management diagnostics keep their separate nonblank/512-byte bound.
/// Serving uses the opt-in transport and supervised service, not direct async CPU work.
#[derive(Clone, Copy)]
pub struct PublishedSearchRequest<'a> {
    pub query: &'a str,
    pub version: Option<&'a str>,
    pub language: Option<&'a str>,
    pub limit: Option<usize>,
}

impl fmt::Debug for PublishedSearchRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublishedSearchRequest")
            .field("query_bytes", &self.query.len())
            .field("has_version", &self.version.is_some())
            .field("has_language", &self.language.is_some())
            .field("limit", &self.limit)
            .finish()
    }
}

/// Internal library output. Deliberately not Serialize: publication IDs, raw
/// BM25 scores and this envelope are not an approved public HTTP contract.
#[derive(Debug)]
pub struct PublishedSearchResult {
    pub deployment_id: String,
    pub generation_id: Uuid,
    pub publication_version: i32,
    pub context: PublishedSearchContext,
    pub hits: Vec<SearchHit>,
}

#[derive(Debug, thiserror::Error)]
pub enum PublishedSearchError {
    #[error("published search admission failed")]
    Admission(#[from] AppError),
    #[error("published search database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("published search input is invalid: {0}")]
    InvalidRequest(&'static str),
    #[error("published search is unavailable: {0}")]
    Unavailable(&'static str),
    #[error("published search snapshot JSON is malformed")]
    SnapshotJson(#[source] serde_json::Error),
    #[error("published search snapshot scope is incompatible")]
    SnapshotScope(#[from] RegistryError),
    #[error("published search controls are malformed")]
    Controls(#[source] serde_json::Error),
    #[error("published search index operation failed")]
    Search(#[from] SearchError),
}

/// One-shot, request-local operation holding server-derived admission and
/// selection. Not Clone or Serialize, and not a reusable authorization ticket.
/// Execute promptly through the service's bounded, shutdown-owned
/// blocking executor; never cache it or run synchronous index work on an async
/// worker. Preparation itself starts no background task.
#[must_use]
pub struct PreparedPublishedSearch {
    project_id: String,
    admission: PublishedSearchAdmission,
    selected: SelectedPublication,
    request: ValidatedRequest,
}

impl fmt::Debug for PreparedPublishedSearch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedPublishedSearch")
            .field("generation_id", &self.selected.generation_id)
            .field("query_bytes", &self.request.query.len())
            .finish_non_exhaustive()
    }
}

impl PreparedPublishedSearch {
    pub(crate) fn artifact(
        &self,
    ) -> Result<Option<crate::distribution::ArtifactReference>, SearchError> {
        if self.selected.snapshot_oversized || self.selected.controls_oversized {
            return Err(SearchError::ResourceLimit(
                "publication snapshot or live controls",
            ));
        }
        let selected = &self.selected;
        selected
            .location
            .artifact(crate::distribution::location::GenerationIdentity {
                generation_id: selected
                    .generation_id
                    .ok_or(SearchError::Integrity("generation missing"))?,
                project_id: &self.project_id,
                tenant_id: &selected.tenant_id,
                deployment_id: &selected.deployment_id,
                snapshot_sha256: selected
                    .snapshot_sha256
                    .as_deref()
                    .ok_or(SearchError::Integrity("snapshot digest missing"))?,
                documents_sha256: selected
                    .documents_sha256
                    .as_deref()
                    .ok_or(SearchError::Integrity("source digest missing"))?,
                document_count: selected
                    .document_count
                    .and_then(|count| u64::try_from(count).ok())
                    .ok_or(SearchError::Integrity("document count missing"))?,
            })
    }

    /// Consumes this request-local scope to verify and search exactly its
    /// selected immutable generation. This method is synchronous; its caller
    /// owns concurrency, cancellation and shutdown of the blocking lifecycle.
    ///
    /// # Errors
    /// Preserves immutable snapshot/registry, configuration, index, tokenizer
    /// and query failures. An unavailable latest READY generation never falls
    /// back to an older deployment or another engine.
    pub fn execute(self, index_root: &Path) -> Result<PublishedSearchResult, PublishedSearchError> {
        self.execute_with_budget(index_root, None)
    }

    pub(crate) fn execute_with_budget(
        self,
        index_root: &Path,
        budget: Option<&QueryBudget>,
    ) -> Result<PublishedSearchResult, PublishedSearchError> {
        execute_selected(
            index_root,
            &self.project_id,
            &self.admission,
            self.selected,
            &self.request,
            budget,
        )
    }
}

pub(crate) struct ValidatedRequest {
    query: String,
    version: Option<String>,
    language: Option<String>,
    limit: Option<usize>,
}

#[derive(FromRow)]
struct SelectedPublication {
    tenant_id: String,
    #[sqlx(flatten)]
    location: crate::distribution::location::GenerationLocation,
    deployment_id: String,
    publication_version: i32,
    snapshot_json: Option<String>,
    live_controls_json: String,
    generation_id: Option<Uuid>,
    generation_state: Option<String>,
    snapshot_sha256: Option<String>,
    documents_sha256: Option<String>,
    document_count: Option<i32>,
    snapshot_oversized: bool,
    controls_oversized: bool,
}

/// Prepares a server-selected published deployment for a freshly admitted
/// viewer. No deployment/generation path, page list or trusted actor can be
/// supplied by browser data through this library's request type.
///
/// `None` conceals missing, denied, taken-down and never-published sites before
/// any filesystem access. Executing a broken/incompatible latest READY
/// publication is an error, never a fallback to an older publication or a
/// different search engine.
/// The caller must authenticate any optional workspace actor; reader cookies
/// are handled solely by project-bound reader-session admission.
///
/// # Errors
/// Preserves typed admission/database and input failures. Immutable snapshot,
/// registry and index failures are checked by the consuming execution method.
/// No raw-query logging, analytics,
/// session activity updates, public HTTP response or cache policy is added.
pub async fn prepare_published_search(
    pool: &PgPool,
    project_id: &ProjectId,
    headers: &HeaderMap,
    workspace_actor: Option<&Actor>,
    request: PublishedSearchRequest<'_>,
) -> Result<Option<PreparedPublishedSearch>, PublishedSearchError> {
    let mut connection = pool.acquire().await?;
    prepare_on_connection(
        &mut connection,
        project_id,
        headers,
        workspace_actor,
        validate_request(request),
        None,
        None,
    )
    .await
}

pub(crate) async fn prepare_on_connection(
    connection: &mut PgConnection,
    project_id: &ProjectId,
    headers: &HeaderMap,
    workspace_actor: Option<&Actor>,
    request: Result<ValidatedRequest, PublishedSearchError>,
    budget: Option<&QueryBudget>,
    resolver: Option<&dyn PublishedActorResolver>,
) -> Result<Option<PreparedPublishedSearch>, PublishedSearchError> {
    checkpoint(budget)?;
    let mut transaction = connection.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *transaction)
        .await?;
    if let Some(budget) = budget {
        sqlx::query("SELECT set_config('statement_timeout', $1, true)")
            .bind(budget.statement_timeout()?)
            .execute(&mut *transaction)
            .await?;
    }
    checkpoint(budget)?;
    let resolved_actor = if let Some(resolver) = resolver {
        resolver.resolve(&mut transaction, headers).await?
    } else {
        None
    };
    checkpoint(budget)?;
    let workspace_actor = if resolver.is_some() {
        resolved_actor.as_ref()
    } else {
        workspace_actor
    };
    let max_pages = budget.map_or(i64::MAX, |value| {
        i64::try_from(value.config.max_documents).unwrap_or(i64::MAX)
    });
    let admission = admit_on_connection_with_limit(
        &mut transaction,
        project_id,
        headers,
        workspace_actor,
        max_pages,
    )
    .await?;
    checkpoint(budget)?;
    let Some(admission) = admission else {
        transaction.commit().await?;
        return Ok(None);
    };
    let selected = select_publication(&mut transaction, project_id.as_str(), budget).await?;
    checkpoint(budget)?;
    transaction.commit().await?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    Ok(Some(PreparedPublishedSearch {
        project_id: project_id.as_str().to_owned(),
        admission,
        selected,
        request: request?,
    }))
}

const SELECT_PUBLICATION: &str = r#"
        /* nibleaf:published-search-selection */
        WITH latest AS MATERIALIZED (
          SELECT id, "projectId", version, snapshot
          FROM deployment
          WHERE "projectId" = $1 AND status = 'READY'::"DeploymentStatus"
          ORDER BY version DESC, id DESC
          LIMIT 1
        )
        , selected AS MATERIALIZED (
        SELECT latest.*, project."organizationId",
               latest.snapshot::text AS snapshot_text,
               jsonb_build_object(
                 'config', CASE WHEN jsonb_typeof(project.config) = 'object'
                   THEN jsonb_build_object('search',
                     CASE WHEN jsonb_typeof(project.config->'search') = 'object'
                       THEN CASE WHEN (project.config->'search') ? 'maxResults'
                         THEN jsonb_build_object('maxResults', project.config#>'{search,maxResults}')
                         ELSE '{}'::jsonb END
                       ELSE project.config->'search' END)
                   ELSE project.config END,
                 'languages', (
                   SELECT COALESCE(jsonb_agg(
                     jsonb_build_object('code', language.code, 'enabled', language.enabled)
                     ORDER BY language.code
                   ), '[]'::jsonb)
                   FROM language WHERE language."projectId" = $1
                     AND EXISTS (
                       SELECT 1 FROM jsonb_array_elements(
                         CASE WHEN jsonb_typeof(latest.snapshot->'languages') = 'array'
                           THEN latest.snapshot->'languages' ELSE '[]'::jsonb END
                       ) AS frozen_language
                       WHERE frozen_language->>'code' = language.code
                     )
                 )
               )::text AS controls_text
        FROM latest INNER JOIN project ON project.id = latest."projectId"
        WHERE project."takedownAt" IS NULL
        )
        SELECT selected.id AS deployment_id, selected.version AS publication_version, selected."organizationId" AS tenant_id,
               generation.delivery_kind, artifact.format_version AS artifact_format,artifact.artifact_sha256,
               artifact.artifact_bytes,artifact.object_key AS artifact_key,
               CASE WHEN octet_length(snapshot_text) <= $2 THEN snapshot_text END AS snapshot_json,
               COALESCE(octet_length(snapshot_text) > $2, FALSE) AS snapshot_oversized,
               CASE WHEN octet_length(controls_text) <= $3 THEN controls_text ELSE '{}' END AS live_controls_json,
               octet_length(controls_text) > $3 AS controls_oversized,
               generation.generation_id, generation.state AS generation_state,
               generation.snapshot_sha256, generation.documents_sha256,
               generation.document_count
        FROM selected
        LEFT JOIN rust_search_generation AS generation
          ON generation.project_id = selected."projectId"
         AND generation.deployment_id = selected.id
         AND generation.tenant_id = selected."organizationId"
        LEFT JOIN rust_search_artifact artifact ON artifact.generation_id=generation.generation_id
         AND artifact.project_id=generation.project_id AND artifact.tenant_id=generation.tenant_id
        "#;

async fn select_publication(
    connection: &mut PgConnection,
    project_id: &str,
    budget: Option<&QueryBudget>,
) -> Result<Option<SelectedPublication>, sqlx::Error> {
    sqlx::query_as(SELECT_PUBLICATION)
        .bind(project_id)
        .bind(budget.map_or(i64::MAX, |value| {
            i64::try_from(value.config.max_snapshot_bytes).unwrap_or(i64::MAX)
        }))
        .bind(if budget.is_some() {
            65_536_i64
        } else {
            i64::MAX
        })
        .fetch_optional(connection)
        .await
}

pub(crate) fn validate_request(
    request: PublishedSearchRequest<'_>,
) -> Result<ValidatedRequest, PublishedSearchError> {
    if request.query.encode_utf16().count() > 200 {
        return Err(PublishedSearchError::InvalidRequest(
            "query must be at most 200 UTF-16 code units",
        ));
    }
    if request
        .limit
        .is_some_and(|limit| !(1..=MAX_RESULTS).contains(&limit))
    {
        return Err(PublishedSearchError::InvalidRequest(
            "limit must be between 1 and 50",
        ));
    }
    Ok(ValidatedRequest {
        query: request.query.trim().to_owned(),
        version: selector(request.version)?,
        language: selector(request.language)?,
        limit: request.limit,
    })
}

fn selector(value: Option<&str>) -> Result<Option<String>, PublishedSearchError> {
    if value.is_some_and(|value| value.len() > 191) {
        return Err(PublishedSearchError::InvalidRequest(
            "version and language must be at most 191 bytes",
        ));
    }
    Ok(value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned))
}

fn decode_snapshot(json: Option<String>) -> Result<Value, PublishedSearchError> {
    let json = json.ok_or(PublishedSearchError::Unavailable("snapshot is missing"))?;
    serde_json::from_str(&json).map_err(PublishedSearchError::SnapshotJson)
}

fn execute_selected(
    root: &Path,
    project_id: &str,
    admission: &PublishedSearchAdmission,
    selected: SelectedPublication,
    request: &ValidatedRequest,
    budget: Option<&QueryBudget>,
) -> Result<PublishedSearchResult, PublishedSearchError> {
    checkpoint(budget)?;
    if selected.snapshot_oversized || selected.controls_oversized {
        return Err(SearchError::ResourceLimit("publication snapshot or live controls").into());
    }
    let generation_id = selected
        .generation_id
        .ok_or(PublishedSearchError::Unavailable(
            "latest READY publication has no native generation",
        ))?;
    if !matches!(
        selected.generation_state.as_deref(),
        Some("ready" | "superseded")
    ) {
        return Err(PublishedSearchError::Unavailable(
            "latest READY publication generation is not readable",
        ));
    }
    let context = {
        let snapshot = decode_snapshot(selected.snapshot_json)?;
        if selected.snapshot_sha256.as_deref()
            != Some(canonical_snapshot_sha256(&snapshot).as_str())
        {
            return Err(PublishedSearchError::Unavailable(
                "snapshot does not match its durable generation binding",
            ));
        }
        checkpoint(budget)?;
        let scope = snapshot_scope(&snapshot, project_id)?;
        let live: LiveSearchControls = serde_json::from_str(&selected.live_controls_json)
            .map_err(PublishedSearchError::Controls)?;
        resolve_context(
            &scope,
            &live,
            request.version.as_deref(),
            request.language.as_deref(),
            request.limit,
        )?
    };
    checkpoint(budget)?;
    let (generation, manifest) = open_generation_with_budget(
        root,
        project_id,
        &selected.deployment_id,
        generation_id,
        budget,
    )?;
    let expected_count = selected
        .document_count
        .and_then(|count| u64::try_from(count).ok());
    if expected_count != Some(manifest.document_count)
        || selected.documents_sha256.as_deref() != Some(manifest.documents_sha256.as_str())
    {
        return Err(PublishedSearchError::Unavailable(
            "generation manifest does not match the selected registry evidence",
        ));
    }
    let hits = search_published_generation_with_budget(
        &generation,
        admission,
        &request.query,
        context.version_id(),
        context.languages(),
        context.result_limit(),
        budget,
    )?;
    checkpoint(budget)?;
    Ok(PublishedSearchResult {
        deployment_id: selected.deployment_id,
        generation_id,
        publication_version: selected.publication_version,
        context,
        hits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(query: &str) -> PublishedSearchRequest<'_> {
        PublishedSearchRequest {
            query,
            version: None,
            language: None,
            limit: None,
        }
    }

    #[test]
    fn library_bounds_and_selector_normalization_are_explicit() -> Result<(), PublishedSearchError>
    {
        let mut input = request("  topicprobe  ");
        input.version = Some(" next ");
        input.language = Some(" en-GB ");
        let normalized = validate_request(input)?;
        assert_eq!(normalized.query, "topicprobe");
        assert_eq!(normalized.version.as_deref(), Some("next"));
        assert_eq!(normalized.language.as_deref(), Some("en-GB"));
        assert!(validate_request(request("  ")).is_ok());
        assert!(validate_request(request(&"q".repeat(201))).is_err());
        for limit in [0, MAX_RESULTS + 1] {
            let mut input = request("q");
            input.limit = Some(limit);
            assert!(validate_request(input).is_err());
        }
        assert!(selector(Some(&"v".repeat(192))).is_err());
        assert_eq!(selector(Some("  "))?, None);
        Ok(())
    }

    #[test]
    fn request_debug_never_contains_query_or_unvalidated_selector_text() {
        let mut input = request("private-query-sentinel");
        input.version = Some("private-version-sentinel");
        input.language = Some("private-language-sentinel");
        let output = format!("{input:?}");
        assert!(!output.contains("private-"));
        assert!(output.contains("query_bytes"));
    }
}
