//! Tenant-authorized management preparation on the service-owned connection.

use std::path::Path;

use nibleaf_core::{Actor, AppError, ProjectId, Role, authz::require_project_role_on_connection};
use sqlx::{Connection, FromRow, PgConnection};
use uuid::Uuid;

use super::{SearchQueryExecutionError, budget::QueryBudget};
use crate::{
    SearchError, SearchHit, api::ValidatedSearchQuery, open_generation_with_budget,
    query::search_member_with_budget,
};

pub(crate) struct MemberInput {
    pub(crate) project_id: ProjectId,
    pub(crate) actor: Actor,
    pub(crate) query: Result<ValidatedSearchQuery, AppError>,
}

#[derive(FromRow)]
struct ActiveGenerationRow {
    #[sqlx(flatten)]
    location: crate::distribution::location::GenerationLocation,
    tenant_id: String,
    snapshot_sha256: String,
    documents_sha256: String,
    document_count: i32,
    generation_id: Uuid,
    deployment_id: String,
}

pub(crate) struct PreparedMemberSearch {
    project_id: ProjectId,
    active: ActiveGenerationRow,
    query: ValidatedSearchQuery,
}

pub(crate) struct MemberSearchResult {
    pub(crate) deployment_id: String,
    pub(crate) generation_id: Uuid,
    pub(crate) hits: Vec<SearchHit>,
}

pub(crate) async fn prepare(
    connection: &mut PgConnection,
    input: MemberInput,
    budget: &QueryBudget,
) -> Result<PreparedMemberSearch, SearchQueryExecutionError> {
    budget.check()?;
    let mut transaction = connection.begin().await.map_err(AppError::from)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *transaction)
        .await
        .map_err(AppError::from)?;
    sqlx::query("SELECT set_config('statement_timeout', $1, true)")
        .bind(budget.statement_timeout()?)
        .execute(&mut *transaction)
        .await
        .map_err(AppError::from)?;
    budget.check()?;
    let scope = require_project_role_on_connection(
        &mut transaction,
        &input.actor,
        input.project_id,
        Role::Member,
    )
    .await?;
    // Preserve the existing member route's concealed authorization precedence
    // even for an invalid query. Invalid raw values aren't retained in the job.
    budget.check()?;
    let query = input.query?;
    let active = sqlx::query_as(
        r#"SELECT generation.generation_id, generation.deployment_id,
                generation.tenant_id,generation.snapshot_sha256,generation.documents_sha256,generation.document_count,
                generation.delivery_kind,artifact.format_version AS artifact_format,artifact.artifact_sha256,artifact.artifact_bytes,artifact.object_key AS artifact_key
           FROM rust_search_active_generation AS active
           INNER JOIN rust_search_generation AS generation
             ON generation.generation_id = active.generation_id
            AND generation.project_id = active.project_id
            AND generation.tenant_id = $2
           INNER JOIN deployment ON deployment.id = generation.deployment_id
            AND deployment."projectId" = generation.project_id
           LEFT JOIN rust_search_artifact artifact ON artifact.generation_id=generation.generation_id
             AND artifact.project_id=generation.project_id AND artifact.tenant_id=generation.tenant_id
           WHERE active.project_id = $1 AND generation.state = 'ready'
             AND deployment.status = 'READY'::"DeploymentStatus""#,
    )
    .bind(scope.project_id.as_str())
    .bind(scope.organization_id.as_str())
    .fetch_optional(&mut *transaction)
    .await
    .map_err(AppError::from)?
    .ok_or(AppError::Unavailable)?;
    budget.check()?;
    transaction.commit().await.map_err(AppError::from)?;
    budget.check()?;
    Ok(PreparedMemberSearch {
        project_id: scope.project_id,
        active,
        query,
    })
}

impl PreparedMemberSearch {
    pub(crate) fn artifact(
        &self,
    ) -> Result<Option<crate::distribution::ArtifactReference>, SearchError> {
        self.active
            .location
            .artifact(crate::distribution::location::GenerationIdentity {
                generation_id: self.active.generation_id,
                project_id: self.project_id.as_str(),
                tenant_id: &self.active.tenant_id,
                deployment_id: &self.active.deployment_id,
                snapshot_sha256: &self.active.snapshot_sha256,
                documents_sha256: &self.active.documents_sha256,
                document_count: u64::try_from(self.active.document_count)
                    .map_err(|_| SearchError::Integrity("registry document count"))?,
            })
    }

    pub(crate) fn execute(
        self,
        root: &Path,
        budget: &QueryBudget,
    ) -> Result<MemberSearchResult, SearchQueryExecutionError> {
        let (generation, manifest) = open_generation_with_budget(
            root,
            self.project_id.as_str(),
            &self.active.deployment_id,
            self.active.generation_id,
            Some(budget),
        )?;
        if manifest.documents_sha256 != self.active.documents_sha256
            || Some(manifest.document_count) != u64::try_from(self.active.document_count).ok()
        {
            return Err(SearchError::Integrity("member registry source binding").into());
        }
        let hits = search_member_with_budget(
            &generation,
            &self.query.q,
            self.query.version.as_deref(),
            self.query.language.as_deref(),
            self.query.limit,
            budget,
        )?;
        budget.check()?;
        Ok(MemberSearchResult {
            deployment_id: self.active.deployment_id,
            generation_id: self.active.generation_id,
            hits,
        })
    }
}
