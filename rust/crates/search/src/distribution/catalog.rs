//! Durable retained object/replica facts. No TTL means purge, no delete API.
use super::{ArtifactReference, object_key};
use crate::{
    GenerationManifest, SearchError,
    artifacts::ArtifactReceipt,
    registry::{ClaimedGeneration, RegistryError},
};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

async fn tx(pool: &PgPool) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout='5s'")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}
/// Persist exact attempted provider scope before a put, under the current claim.
/// # Errors
/// Rejects lease/scope mismatch and any conflicting prior immutable scope.
pub async fn begin_upload(
    pool: &PgPool,
    claim: &ClaimedGeneration,
    receipt: &ArtifactReceipt,
) -> Result<ArtifactReference, RegistryError> {
    let reference = from_receipt(claim, receipt)?;
    let mut tx = tx(pool).await?;
    let changed=sqlx::query(r"
      INSERT INTO rust_search_artifact_upload(generation_id,project_id,tenant_id,deployment_id,snapshot_sha256,
        artifact_sha256,artifact_bytes,documents_sha256,document_count,object_key,state,claim_token)
      SELECT g.generation_id,g.project_id,g.tenant_id,g.deployment_id,g.snapshot_sha256,$6,$7,$8,$9,$10,'pending',$11
      FROM rust_search_generation g WHERE g.generation_id=$1 AND g.project_id=$2 AND g.tenant_id=$3 AND g.deployment_id=$4
        AND g.snapshot_sha256=$5 AND g.state='building' AND g.delivery_kind='object' AND g.lease_token=$11
      ON CONFLICT(generation_id,artifact_sha256) DO UPDATE
        SET claim_token=EXCLUDED.claim_token,attempts=rust_search_artifact_upload.attempts+1,state='pending',last_error=NULL,updated_at=NOW()
      WHERE rust_search_artifact_upload.artifact_bytes=EXCLUDED.artifact_bytes
        AND rust_search_artifact_upload.documents_sha256=EXCLUDED.documents_sha256
        AND rust_search_artifact_upload.document_count=EXCLUDED.document_count
        AND rust_search_artifact_upload.object_key=EXCLUDED.object_key
        AND NOT EXISTS(SELECT 1 FROM rust_search_artifact a WHERE a.generation_id=EXCLUDED.generation_id)
    ").bind(reference.generation_id).bind(&reference.project_id).bind(&reference.tenant_id).bind(&reference.deployment_id).bind(&reference.snapshot_sha256)
        .bind(&reference.artifact_sha256).bind(i64::try_from(reference.artifact_bytes).map_err(|_|RegistryError::Promotion("artifact bytes"))?)
        .bind(&reference.documents_sha256).bind(i32::try_from(reference.document_count).map_err(|_|RegistryError::Promotion("artifact documents"))?)
        .bind(&reference.object_key).bind(claim.lease_token).execute(&mut *tx).await?;
    if changed.rows_affected() != 1 {
        return Err(RegistryError::LeaseLost);
    }
    tx.commit().await?;
    Ok(reference)
}
fn from_receipt(
    claim: &ClaimedGeneration,
    receipt: &ArtifactReceipt,
) -> Result<ArtifactReference, RegistryError> {
    if receipt.format_version != 1
        || receipt.generation_id != claim.request.generation_id
        || receipt.project_id != claim.request.project_id
        || receipt.deployment_id != claim.request.deployment_id
    {
        return Err(RegistryError::Promotion("artifact scope mismatch"));
    }
    let reference = ArtifactReference {
        generation_id: receipt.generation_id,
        project_id: receipt.project_id.clone(),
        tenant_id: claim.tenant_id.clone(),
        deployment_id: receipt.deployment_id.clone(),
        snapshot_sha256: claim.request.snapshot_sha256.clone(),
        artifact_sha256: receipt.artifact_sha256.clone(),
        artifact_bytes: receipt.artifact_bytes,
        documents_sha256: receipt.documents_sha256.clone(),
        document_count: receipt.document_count,
        object_key: object_key(
            &receipt.project_id,
            receipt.generation_id,
            &receipt.artifact_sha256,
        ),
    };
    reference.validate()?;
    Ok(reference)
}
/// Record confirmation or ambiguity without deleting previous provider facts.
/// # Errors
/// A stale claimant cannot confirm or rewrite a newer attempt/attachment.
pub async fn upload_result(
    pool: &PgPool,
    claim: &ClaimedGeneration,
    reference: &ArtifactReference,
    verified: bool,
) -> Result<(), RegistryError> {
    let mut tx = tx(pool).await?;
    let result=sqlx::query(r"UPDATE rust_search_artifact_upload u SET state=$4,last_error=$5,updated_at=NOW()
      WHERE u.generation_id=$1 AND u.artifact_sha256=$2 AND u.claim_token=$3
        AND NOT EXISTS(SELECT 1 FROM rust_search_artifact a WHERE a.generation_id=u.generation_id)
        AND ($4='unknown' OR EXISTS(SELECT 1 FROM rust_search_generation g WHERE g.generation_id=u.generation_id
          AND g.state='building' AND g.delivery_kind='object' AND g.lease_token=$3))")
        .bind(reference.generation_id).bind(&reference.artifact_sha256).bind(claim.lease_token)
        .bind(if verified{"verified"}else{"unknown"}).bind(if verified{None}else{Some("provider_put_unconfirmed")})
        .execute(&mut *tx).await?;
    if result.rows_affected() != 1 {
        return Err(RegistryError::LeaseLost);
    }
    tx.commit().await?;
    Ok(())
}
pub(crate) async fn attach(
    tx: &mut Transaction<'_, Postgres>,
    claim: &ClaimedGeneration,
    manifest: &GenerationManifest,
    reference: &ArtifactReference,
) -> Result<(), RegistryError> {
    reference.validate()?;
    if reference.generation_id != claim.request.generation_id
        || reference.project_id != claim.request.project_id
        || reference.tenant_id != claim.tenant_id
        || reference.deployment_id != claim.request.deployment_id
        || reference.snapshot_sha256 != claim.request.snapshot_sha256
        || reference.documents_sha256 != manifest.documents_sha256
        || reference.document_count != manifest.document_count
    {
        return Err(RegistryError::Promotion("artifact manifest binding"));
    }
    let result=sqlx::query(r"INSERT INTO rust_search_artifact(generation_id,project_id,tenant_id,deployment_id,snapshot_sha256,format_version,
        artifact_sha256,artifact_bytes,documents_sha256,document_count,object_key)
      SELECT generation_id,project_id,tenant_id,deployment_id,snapshot_sha256,1,artifact_sha256,artifact_bytes,documents_sha256,document_count,object_key
      FROM rust_search_artifact_upload WHERE generation_id=$1 AND artifact_sha256=$2 AND claim_token=$3 AND state='verified'")
        .bind(reference.generation_id).bind(&reference.artifact_sha256).bind(claim.lease_token).execute(&mut **tx).await?;
    if result.rows_affected() != 1 {
        return Err(RegistryError::LeaseLost);
    }
    Ok(())
}

/// A logical replica may not silently reuse an unrelated physical root identity.
/// # Errors
/// Rejects identity collisions; no prior retained location is inferred purged.
pub async fn register_root(pool: &PgPool, replica: Uuid, root: Uuid) -> Result<(), SearchError> {
    let mut tx = tx(pool)
        .await
        .map_err(|_| SearchError::Distribution("replica catalog"))?;
    sqlx::query("INSERT INTO rust_search_replica_root(replica_id,root_id) VALUES($1,$2) ON CONFLICT DO NOTHING")
        .bind(replica).bind(root).execute(&mut *tx).await.map_err(|_|SearchError::Distribution("replica catalog"))?;
    let existing: Option<Uuid> =
        sqlx::query_scalar("SELECT root_id FROM rust_search_replica_root WHERE replica_id=$1")
            .bind(replica)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| SearchError::Distribution("replica catalog"))?;
    if existing != Some(root) {
        return Err(SearchError::Distribution("replica root identity conflict"));
    }
    tx.commit()
        .await
        .map_err(|_| SearchError::Distribution("replica catalog"))?;
    Ok(())
}
pub(crate) async fn materializing(
    pool: &PgPool,
    id: Uuid,
    replica: Uuid,
    root: Uuid,
    reference: &ArtifactReference,
) -> Result<(), SearchError> {
    let mut tx = tx(pool)
        .await
        .map_err(|_| SearchError::Distribution("materialization catalog"))?;
    let changed=sqlx::query(r"INSERT INTO rust_search_materialization(materialization_id,replica_id,root_id,generation_id,project_id,tenant_id,artifact_sha256,state)
      SELECT $1,$2,$3,a.generation_id,a.project_id,a.tenant_id,a.artifact_sha256,'materializing'
      FROM rust_search_artifact a WHERE a.generation_id=$4 AND a.project_id=$5 AND a.tenant_id=$6 AND a.artifact_sha256=$7
        AND EXISTS(SELECT 1 FROM rust_search_replica_root r WHERE r.replica_id=$2 AND r.root_id=$3)
        AND (SELECT count(*) FROM rust_search_materialization m WHERE m.replica_id=$2 AND m.root_id=$3)<100000")
        .bind(id).bind(replica).bind(root).bind(reference.generation_id).bind(&reference.project_id).bind(&reference.tenant_id).bind(&reference.artifact_sha256)
        .execute(&mut *tx).await.map_err(|_|SearchError::Distribution("materialization catalog"))?;
    if changed.rows_affected() != 1 {
        return Err(SearchError::Integrity(
            "materialization scope or retained catalog quota",
        ));
    }
    tx.commit()
        .await
        .map_err(|_| SearchError::Distribution("materialization catalog"))?;
    Ok(())
}
pub(crate) async fn location_state(
    pool: &PgPool,
    id: Uuid,
    replica: Uuid,
    root: Uuid,
    state: &'static str,
    bytes: Option<u64>,
) -> Result<(), SearchError> {
    let bytes = bytes
        .map(i64::try_from)
        .transpose()
        .map_err(|_| SearchError::ResourceLimit("materialization bytes"))?;
    let mut tx = tx(pool)
        .await
        .map_err(|_| SearchError::Distribution("materialization catalog"))?;
    let updated=sqlx::query(r"UPDATE rust_search_materialization SET state=$4,unpacked_bytes=COALESCE($5,unpacked_bytes),updated_at=NOW(),
          purged_at=CASE WHEN $4='purged' THEN COALESCE(purged_at,NOW()) ELSE NULL END
      WHERE materialization_id=$1 AND replica_id=$2 AND root_id=$3 AND (state!='purged' OR $4='purged')
        AND ($4!='ready' OR state='materializing')").bind(id).bind(replica).bind(root).bind(state).bind(bytes)
        .execute(&mut *tx).await.map_err(|_|SearchError::Distribution("materialization catalog"))?;
    if updated.rows_affected() != 1 {
        return Err(SearchError::Distribution("materialization catalog scope"));
    }
    tx.commit()
        .await
        .map_err(|_| SearchError::Distribution("materialization catalog"))?;
    Ok(())
}
