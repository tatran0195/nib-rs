use super::ArtifactReference;
use crate::SearchError;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(FromRow)]
pub(crate) struct GenerationLocation {
    pub(crate) delivery_kind: Option<String>,
    pub(crate) artifact_format: Option<i16>,
    pub(crate) artifact_sha256: Option<String>,
    pub(crate) artifact_bytes: Option<i64>,
    pub(crate) artifact_key: Option<String>,
}
#[derive(Clone, Copy)]
pub(crate) struct GenerationIdentity<'a> {
    pub(crate) generation_id: Uuid,
    pub(crate) project_id: &'a str,
    pub(crate) tenant_id: &'a str,
    pub(crate) deployment_id: &'a str,
    pub(crate) snapshot_sha256: &'a str,
    pub(crate) documents_sha256: &'a str,
    pub(crate) document_count: u64,
}
impl GenerationLocation {
    pub(crate) fn artifact(
        &self,
        id: GenerationIdentity<'_>,
    ) -> Result<Option<ArtifactReference>, SearchError> {
        match self.delivery_kind.as_deref() {
            Some("local") => Ok(None),
            Some("object") => {
                if self.artifact_format != Some(1) {
                    return Err(SearchError::Integrity("artifact attachment missing"));
                }
                let artifact = ArtifactReference {
                    generation_id: id.generation_id,
                    project_id: id.project_id.to_owned(),
                    tenant_id: id.tenant_id.to_owned(),
                    deployment_id: id.deployment_id.to_owned(),
                    snapshot_sha256: id.snapshot_sha256.to_owned(),
                    documents_sha256: id.documents_sha256.to_owned(),
                    document_count: id.document_count,
                    artifact_sha256: self
                        .artifact_sha256
                        .clone()
                        .ok_or(SearchError::Integrity("artifact digest missing"))?,
                    artifact_bytes: self
                        .artifact_bytes
                        .and_then(|bytes| u64::try_from(bytes).ok())
                        .ok_or(SearchError::Integrity("artifact bytes missing"))?,
                    object_key: self
                        .artifact_key
                        .clone()
                        .ok_or(SearchError::Integrity("artifact key missing"))?,
                };
                artifact.validate()?;
                Ok(Some(artifact))
            }
            _ => Err(SearchError::Integrity("generation delivery missing")),
        }
    }
}
