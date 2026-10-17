//! Real database/control-plane proof; explicitly supplied in-memory provider.
//! S3 wire/provider behavior is separately exercised by the HTTP fixture.
use super::*;
use nibleaf_search::{
    SearchQueryConfig, SearchQueryService,
    artifacts::ArtifactLimits,
    distribution::{
        ArtifactPublisher, ArtifactStore, DeliveryKind, ReplicaCache, ReplicaCacheConfig, catalog,
    },
    registry::{promote_claimed_generation_with_artifact, register_generation_build_for_delivery},
};
use object_store::{ObjectStoreExt, path::Path as ObjectPath};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(super) async fn verify(pool: &PgPool, suffix: &str) -> TestResult {
    assert_schema_contract(pool).await?;
    let f = fixture(pool, suffix).await?;
    let request = object_request(pool, &f, 1).await?;
    let provider = Arc::new(object_store::memory::InMemory::new());
    let store = Arc::new(ArtifactStore::from_provider(
        provider.clone(),
        64 * 1024 * 1024,
    )?);
    let publisher =
        ArtifactPublisher::open(pool.clone(), f.root.clone(), Arc::clone(&store)).await?;
    assert_ready_guard(pool, &request).await?;
    publisher
        .fulfill(&request, f.project.organization_id.as_str(), "object-test")
        .await?;
    set_status(pool, &request.deployment_id, "READY").await?;
    assert_immutable_scope(pool, &request).await?;
    assert_cache_and_identity(pool, &f, &request, store.as_ref(), provider).await?;
    drop(publisher);
    fs::remove_dir_all(&f.root)?;
    let f = fixture(pool, &format!("{suffix}-race")).await?;
    assert_unknown_upload_and_stale_token(pool, &f).await?;
    fs::remove_dir_all(&f.root)?;
    Ok(())
}
async fn assert_schema_contract(pool: &PgPool) -> TestResult {
    assert!(nibleaf_search::distribution::schema_ready(pool).await?);
    sqlx::query("ALTER TABLE rust_search_materialization DROP CONSTRAINT rust_search_materialization_artifact_scope_fk").execute(pool).await?;
    assert!(!nibleaf_search::distribution::schema_ready(pool).await?);
    sqlx::query("ALTER TABLE rust_search_materialization ADD CONSTRAINT rust_search_materialization_artifact_scope_fk FOREIGN KEY(generation_id,project_id,tenant_id,artifact_sha256) REFERENCES rust_search_artifact(generation_id,project_id,tenant_id,artifact_sha256) ON DELETE RESTRICT").execute(pool).await?;
    sqlx::query(
        "ALTER TABLE rust_search_generation DISABLE TRIGGER rust_search_object_ready_guard",
    )
    .execute(pool)
    .await?;
    assert!(!nibleaf_search::distribution::schema_ready(pool).await?);
    sqlx::query("ALTER TABLE rust_search_generation ENABLE TRIGGER rust_search_object_ready_guard")
        .execute(pool)
        .await?;
    assert!(nibleaf_search::distribution::schema_ready(pool).await?);
    Ok(())
}

async fn object_request(
    pool: &PgPool,
    f: &Fixture,
    version: i32,
) -> TestResult<SearchBuildRequest> {
    let deployment = format!("object-deployment-{version}-{}", f.suffix);
    let captured = snapshot(f, "objectprobe", 3);
    sqlx::query(r#"INSERT INTO deployment(id,"projectId",version,status,snapshot,"pagesCount","createdAt",rust_search_delivery_mode)
        VALUES($1,$2,$3,'PENDING',$4,6,NOW(),'object')"#).bind(&deployment).bind(f.project.project_id.as_str()).bind(version).bind(&captured).execute(pool).await?;
    let mut tx = pool.begin().await?;
    assert!(
        nibleaf_search::registry::register_generation_build(
            &mut tx,
            f.project.organization_id.as_str(),
            f.project.project_id.as_str(),
            &deployment,
            &canonical_snapshot_sha256(&captured)
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    let mut tx = pool.begin().await?;
    let request = register_generation_build_for_delivery(
        &mut tx,
        f.project.organization_id.as_str(),
        f.project.project_id.as_str(),
        &deployment,
        &canonical_snapshot_sha256(&captured),
        DeliveryKind::Object,
    )
    .await?;
    tx.commit().await?;
    Ok(request)
}
async fn assert_ready_guard(pool: &PgPool, request: &SearchBuildRequest) -> TestResult {
    let result=sqlx::query("UPDATE rust_search_generation SET state='ready',documents_sha256=repeat('0',64),document_count=0,completed_at=NOW() WHERE generation_id=$1")
        .bind(request.generation_id).execute(pool).await;
    assert!(result.is_err());
    let state: String =
        sqlx::query_scalar("SELECT state FROM rust_search_generation WHERE generation_id=$1")
            .bind(request.generation_id)
            .fetch_one(pool)
            .await?;
    assert_eq!(state, "pending");
    Ok(())
}
async fn assert_immutable_scope(pool: &PgPool, request: &SearchBuildRequest) -> TestResult {
    assert!(
        sqlx::query("UPDATE deployment SET rust_search_delivery_mode='local' WHERE id=$1")
            .bind(&request.deployment_id)
            .execute(pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE rust_search_artifact SET object_key='other' WHERE generation_id=$1")
            .bind(request.generation_id)
            .execute(pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM rust_search_artifact WHERE generation_id=$1")
            .bind(request.generation_id)
            .execute(pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query(
            "UPDATE rust_search_artifact_upload SET state='unknown' WHERE generation_id=$1"
        )
        .bind(request.generation_id)
        .execute(pool)
        .await
        .is_err()
    );
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM rust_search_artifact_upload WHERE generation_id=$1 AND state='verified'").bind(request.generation_id).fetch_one(pool).await?;
    assert_eq!(count, 1);
    Ok(())
}
fn cache_config(f: &Fixture, tag: &str, replica: Uuid) -> ReplicaCacheConfig {
    ReplicaCacheConfig {
        root: f.root.with_extension(tag),
        replica_id: replica,
        max_entries: 1,
        max_bytes: 64 * 1024 * 1024,
        max_entry_bytes: 32 * 1024 * 1024,
    }
}
fn input() -> PublishedSearchRequest<'static> {
    PublishedSearchRequest {
        query: "objectprobe",
        language: None,
        version: None,
        limit: Some(50),
    }
}
async fn assert_cache_and_identity(
    pool: &PgPool,
    f: &Fixture,
    request: &SearchBuildRequest,
    store: &ArtifactStore,
    provider: Arc<object_store::memory::InMemory>,
) -> TestResult {
    let cfg = cache_config(f, "replica-a", Uuid::now_v7());
    let cache = ReplicaCache::open(pool.clone(), store.clone(), cfg.clone()).await?;
    assert!(
        ReplicaCache::open(pool.clone(), store.clone(), cfg.clone())
            .await
            .is_err()
    );
    let conflict = cache_config(f, "replica-conflict", cfg.replica_id);
    assert!(
        ReplicaCache::open(pool.clone(), store.clone(), conflict.clone())
            .await
            .is_err()
    );
    fs::remove_dir_all(&conflict.root)?;
    let (service, driver) = SearchQueryService::new_with_cache(
        pool.clone(),
        Arc::clone(&cache),
        SearchQueryConfig::default(),
    )?;
    let stop = CancellationToken::new();
    let _guard = stop.clone().drop_guard();
    let runner = tokio::spawn(driver.run(stop.clone()));
    let headers = HeaderMap::new();
    let (left, right) = tokio::join!(
        service.search_published(&f.project.project_id, &headers, None, input()),
        service.search_published(&f.project.project_id, &headers, None, input())
    );
    assert_eq!(left?.ok_or("object concealed")?.hits.len(), 3);
    assert_eq!(right?.ok_or("object concealed")?.hits.len(), 3);
    let ids:Vec<Uuid>=sqlx::query_scalar("SELECT materialization_id FROM rust_search_materialization WHERE replica_id=$1 AND state='ready'").bind(cfg.replica_id).fetch_all(pool).await?;
    assert_eq!(
        ids.len(),
        1,
        "single-flight created duplicate materializations"
    );
    let native = cfg.root.join(format!("entry-{}", ids[0])).join("native");
    let manifest = generation_directory(
        &native,
        f.project.project_id.as_str(),
        request.generation_id,
    )
    .join("manifest.json");
    let held = manifest.with_extension("held");
    fs::rename(&manifest, &held)?;
    assert!(
        service
            .search_published(&f.project.project_id, &headers, None, input())
            .await
            .is_err()
    );
    fs::rename(held, manifest)?;
    stop.cancel();
    runner.await??;
    drop(cache);
    let ready: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM rust_search_materialization WHERE replica_id=$1 AND state!='purged'",
    )
    .bind(cfg.replica_id)
    .fetch_one(pool)
    .await?;
    assert_eq!(ready, 0);
    assert_eq!(fs::read_dir(&cfg.root)?.count(), 2);
    let reopened = ReplicaCache::open(pool.clone(), store.clone(), cfg.clone()).await?;
    reopened.close().await?;
    drop(reopened);
    fs::remove_dir_all(&cfg.root)?;
    assert_remote_corruption_is_not_local_fallback(pool, f, request, store, provider).await
}
async fn assert_remote_corruption_is_not_local_fallback(
    pool: &PgPool,
    f: &Fixture,
    request: &SearchBuildRequest,
    store: &ArtifactStore,
    provider: Arc<object_store::memory::InMemory>,
) -> TestResult {
    let key: String =
        sqlx::query_scalar("SELECT object_key FROM rust_search_artifact WHERE generation_id=$1")
            .bind(request.generation_id)
            .fetch_one(pool)
            .await?;
    let path = ObjectPath::from(key);
    let original = provider.get(&path).await?.bytes().await?;
    provider.put(&path, "corrupt".into()).await?;
    let cfg = cache_config(f, "replica-corrupt", Uuid::now_v7());
    let cache = ReplicaCache::open(pool.clone(), store.clone(), cfg.clone()).await?;
    let (service, driver) = SearchQueryService::new_with_cache(
        pool.clone(),
        Arc::clone(&cache),
        SearchQueryConfig::default(),
    )?;
    let stop = CancellationToken::new();
    let _guard = stop.clone().drop_guard();
    let runner = tokio::spawn(driver.run(stop.clone()));
    assert!(
        service
            .search_published(&f.project.project_id, &HeaderMap::new(), None, input())
            .await
            .is_err()
    );
    // Master/build files are present, but no query may fall back to them.
    assert!(
        generation_directory(
            &f.root,
            f.project.project_id.as_str(),
            request.generation_id
        )
        .exists()
    );
    provider.put(&path, original.into()).await?;
    assert_eq!(
        service
            .search_published(&f.project.project_id, &HeaderMap::new(), None, input())
            .await?
            .ok_or("restored object concealed")?
            .hits
            .len(),
        3
    );
    stop.cancel();
    runner.await??;
    drop(cache);
    fs::remove_dir_all(cfg.root)?;
    Ok(())
}
async fn assert_unknown_upload_and_stale_token(pool: &PgPool, f: &Fixture) -> TestResult {
    let request = object_request(pool, f, 1).await?;
    let ClaimOutcome::Claimed(first) = claim_generation_build(
        pool,
        &request,
        Some(&f.project.organization_id),
        "same-worker",
        Duration::from_secs(30),
    )
    .await?
    else {
        return Err("first claim unavailable".into());
    };
    let documents = load_claimed_snapshot_documents(pool, &first).await?;
    let manifest = tokio::task::block_in_place(|| {
        build_or_reuse_claimed_generation(&f.root, &first, &documents)
    })?;
    let generation = nibleaf_search::open_generation(
        &f.root,
        f.project.project_id.as_str(),
        &request.deployment_id,
        request.generation_id,
    )?;
    let archive = f.root.with_extension("attempt.tar.zst");
    let receipt = tokio::task::block_in_place(|| {
        nibleaf_search::artifacts::export_artifact(&generation, &archive, ArtifactLimits::default())
    })?;
    let provider = Arc::new(object_store::memory::InMemory::new());
    let store = ArtifactStore::from_provider(provider, 64 * 1024 * 1024)?;
    let reference = catalog::begin_upload(pool, &first, &receipt).await?;
    store.put_verified(&archive, &receipt).await?;
    catalog::upload_result(pool, &first, &reference, false).await?;
    sqlx::query("UPDATE rust_search_generation SET leased_until=NOW()-interval '1 second' WHERE generation_id=$1").bind(request.generation_id).execute(pool).await?;
    let ClaimOutcome::Claimed(second) = claim_generation_build(
        pool,
        &request,
        Some(&f.project.organization_id),
        "same-worker",
        Duration::from_secs(30),
    )
    .await?
    else {
        return Err("second claim unavailable".into());
    };
    assert_ne!(first.lease_token, second.lease_token);
    assert!(
        catalog::upload_result(pool, &first, &reference, true)
            .await
            .is_err()
    );
    let current = catalog::begin_upload(pool, &second, &receipt).await?;
    store.put_verified(&archive, &receipt).await?;
    catalog::upload_result(pool, &second, &current, true).await?;
    assert!(
        promote_claimed_generation_with_artifact(pool, &first, &manifest, &reference)
            .await
            .is_err()
    );
    assert!(
        nibleaf_search::registry::promote_claimed_generation(pool, &second, &manifest)
            .await
            .is_err()
    );
    promote_claimed_generation_with_artifact(pool, &second, &manifest, &current).await?;
    fs::remove_file(archive)?;
    Ok(())
}
