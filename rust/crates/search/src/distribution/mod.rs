//! Native immutable object publication and replica-owned local materialization.
pub(crate) mod cache;
pub mod catalog;
mod config;
pub(crate) mod location;
mod publisher;
mod store;
pub use cache::ReplicaCache;
pub use config::{
    DeliveryKind, DistributionConfigError, ReplicaCacheConfig, SearchDistributionConfig,
};
pub use publisher::ArtifactPublisher;
pub use store::ArtifactStore;

use crate::SearchError;
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct ArtifactReference {
    pub generation_id: Uuid,
    pub project_id: String,
    pub tenant_id: String,
    pub deployment_id: String,
    pub snapshot_sha256: String,
    pub artifact_sha256: String,
    pub artifact_bytes: u64,
    pub documents_sha256: String,
    pub document_count: u64,
    pub object_key: String,
}
impl ArtifactReference {
    /// Validates server-owned relational evidence before any object request.
    /// # Errors
    /// Refuses malformed or noncanonical scope/content/key metadata.
    pub fn validate(&self) -> Result<(), SearchError> {
        for id in [&self.project_id, &self.tenant_id, &self.deployment_id] {
            crate::validate_identifier(id)?;
        }
        for digest in [
            &self.snapshot_sha256,
            &self.artifact_sha256,
            &self.documents_sha256,
        ] {
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(SearchError::Integrity("artifact reference digest"));
            }
        }
        if self.generation_id.is_nil()
            || self.artifact_bytes == 0
            || self.artifact_bytes > 512 * 1024 * 1024
            || self.object_key
                != object_key(&self.project_id, self.generation_id, &self.artifact_sha256)
        {
            return Err(SearchError::Integrity("artifact reference scope"));
        }
        Ok(())
    }
}
#[must_use]
pub fn object_key(project: &str, generation: Uuid, digest: &str) -> String {
    format!(
        "native-search/v1/{}/{generation}/{digest}.tar.zst",
        hex::encode(Sha256::digest(project.as_bytes()))
    )
}

/// Startup contract for object/local immutable delivery metadata.
/// # Errors
/// Preserves database failures; false means migration14 is incomplete.
pub async fn schema_ready(pool: &sqlx::PgPool) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(r"SELECT
      to_regclass('public.rust_search_artifact') IS NOT NULL
      AND to_regclass('public.rust_search_artifact_upload') IS NOT NULL
      AND to_regclass('public.rust_search_materialization') IS NOT NULL
      AND to_regclass('public.rust_search_replica_root') IS NOT NULL
      AND (SELECT count(*) FROM pg_constraint WHERE convalidated AND conname IN (
        'deployment_search_delivery_check','deployment_search_delivery_scope_key',
        'rust_search_generation_delivery_check','rust_search_generation_delivery_fk','rust_search_generation_distribution_scope_key',
        'rust_search_upload_scope_fk','rust_search_upload_key_check','rust_search_artifact_upload_fk','rust_search_artifact_scope_fk',
        'rust_search_materialization_root_fk','rust_search_materialization_artifact_scope_fk'))=11
      AND (SELECT count(*) FROM pg_trigger WHERE NOT tgisinternal AND tgenabled IN ('O','A') AND tgname IN (
        'deployment_search_delivery_immutable','rust_search_generation_delivery_immutable','rust_search_artifact_immutable',
        'rust_search_upload_scope_immutable','rust_search_materialization_scope_guard','rust_search_replica_root_immutable',
        'rust_search_artifact_attachment_guard','rust_search_object_ready_guard'))=8")
       .fetch_one(pool).await
}

#[cfg(test)]
mod tests;
