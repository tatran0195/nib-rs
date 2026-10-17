#![forbid(unsafe_code)]

//! Disposable PostgreSQL proof for native index generation leases and promotion.
//!
//! The ordinary workspace test command skips this file unless the integration
//! harness has migrated a disposable legacy-compatible PostgreSQL schema.

use std::{env, fs, time::Duration};

use nibleaf_search::registry::{
    ClaimOutcome, RegistryError, build_or_reuse_claimed_generation, canonical_snapshot_sha256,
    claim_generation_build, load_claimed_snapshot_documents, promote_claimed_generation,
    record_claim_failure, register_generation_build,
};
use sqlx::PgPool;
use uuid::Uuid;

#[tokio::test]
async fn reclaimed_build_cannot_promote_over_the_active_generation()
-> Result<(), Box<dyn std::error::Error>> {
    if env::var("NIBLEAF_SEARCH_REGISTRY_E2E").is_err() {
        return Ok(());
    }
    let database_url = env::var("NIBLEAF_TEST_DATABASE_URL")?;
    let pool = PgPool::connect(&database_url).await?;
    let suffix = Uuid::now_v7().simple().to_string();
    let tenant_id = format!("search-registry-tenant-{suffix}");
    let project_id = format!("search-registry-project-{suffix}");
    let root = env::temp_dir().join(format!("nibleaf-search-registry-{suffix}"));
    let first_deployment = format!("search-registry-deployment-one-{suffix}");
    let second_deployment = format!("search-registry-deployment-two-{suffix}");

    create_project(&pool, &tenant_id, &project_id, &suffix).await?;
    let first_snapshot = snapshot(
        &project_id,
        "first-page",
        "First guide",
        "First deployment content",
    );
    insert_deployment(&pool, &first_deployment, &project_id, 1, &first_snapshot).await?;
    let first = register(
        &pool,
        &tenant_id,
        &project_id,
        &first_deployment,
        &first_snapshot,
    )
    .await?;
    let first_claim = claim(&pool, &first, &tenant_id).await?;
    let first_manifest = build_and_validate(&pool, &root, &first_claim).await?;
    assert_incompatible_runtime_cannot_promote(&pool, &first_claim, &first_manifest).await?;
    promote_claimed_generation(&pool, &first_claim, &first_manifest).await?;
    assert_eq!(
        active_generation(&pool, &project_id).await?,
        first.generation_id
    );
    sqlx::query("UPDATE deployment SET status='READY'::\"DeploymentStatus\" WHERE id=$1")
        .bind(&first_deployment)
        .execute(&pool)
        .await?;

    let second_snapshot = snapshot(
        &project_id,
        "second-page",
        "Second guide",
        "Second deployment content",
    );
    insert_deployment(&pool, &second_deployment, &project_id, 2, &second_snapshot).await?;
    let second = register(
        &pool,
        &tenant_id,
        &project_id,
        &second_deployment,
        &second_snapshot,
    )
    .await?;
    let stale_claim = claim(&pool, &second, &tenant_id).await?;
    let stale_manifest = build_and_validate(&pool, &root, &stale_claim).await?;
    // Simulate a process dying after immutable files exist but before promotion.
    // A fresh claim must receive a different token and fence the stale claimant.
    sqlx::query(
        "UPDATE rust_search_generation SET leased_until=NOW() - interval '1 second' WHERE generation_id=$1",
    )
    .bind(second.generation_id)
    .execute(&pool)
    .await?;
    let reclaimed = claim(&pool, &second, &tenant_id).await?;
    assert!(reclaimed.reclaimed);
    assert_ne!(stale_claim.lease_token, reclaimed.lease_token);
    assert!(matches!(
        promote_claimed_generation(&pool, &stale_claim, &stale_manifest).await,
        Err(RegistryError::LeaseLost)
    ));
    assert_eq!(
        active_generation(&pool, &project_id).await?,
        first.generation_id
    );

    let second_manifest = build_and_validate(&pool, &root, &reclaimed).await?;
    promote_claimed_generation(&pool, &reclaimed, &second_manifest).await?;
    assert_eq!(
        active_generation(&pool, &project_id).await?,
        second.generation_id
    );
    assert_eq!(state(&pool, first.generation_id).await?, "superseded");
    assert_eq!(state(&pool, second.generation_id).await?, "ready");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM rust_search_generation WHERE project_id=$1 AND state='ready'",
        )
        .bind(&project_id)
        .fetch_one(&pool)
        .await?,
        1
    );
    // The publication consumer would perform this transition after promotion.
    // Mark it here so the legacy one-active-deployment constraint permits the
    // independent terminal-failure proof below.
    sqlx::query("UPDATE deployment SET status='READY'::\"DeploymentStatus\" WHERE id=$1")
        .bind(&second_deployment)
        .execute(&pool)
        .await?;

    assert_terminal_snapshot_failure(&pool, &tenant_id, &project_id, &suffix).await?;
    assert_navigation_only_snapshot_promotes_an_empty_generation(&pool, &root, &suffix).await?;
    assert_versioned_eligible_corpus_is_promoted(&pool, &root, &suffix).await?;
    fs::remove_dir_all(root)?;
    Ok(())
}

async fn assert_incompatible_runtime_cannot_promote(
    pool: &PgPool,
    claim: &nibleaf_search::registry::ClaimedGeneration,
    manifest: &nibleaf_search::GenerationManifest,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut incompatible = manifest.clone();
    "2.3.4".clone_into(&mut incompatible.runtime.lindera);
    assert!(matches!(
        promote_claimed_generation(pool, claim, &incompatible).await,
        Err(RegistryError::Search(
            nibleaf_search::SearchError::Integrity(_)
        ))
    ));
    assert_eq!(state(pool, claim.request.generation_id).await?, "building");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM rust_search_active_generation WHERE project_id=$1",
        )
        .bind(&claim.request.project_id)
        .fetch_one(pool)
        .await?,
        0
    );
    Ok(())
}

async fn assert_navigation_only_snapshot_promotes_an_empty_generation(
    pool: &PgPool,
    root: &std::path::Path,
    suffix: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let tenant_id = format!("search-registry-empty-tenant-{suffix}");
    let project_id = format!("search-registry-empty-project-{suffix}");
    let deployment_id = format!("search-registry-empty-deployment-{suffix}");
    let mut snapshot = snapshot(&project_id, "navigation-group", "Navigation", "");
    snapshot["pages"][0]["kind"] = serde_json::json!("GROUP");
    create_project(pool, &tenant_id, &project_id, &format!("empty-{suffix}")).await?;
    insert_deployment(pool, &deployment_id, &project_id, 1, &snapshot).await?;
    let request = register(pool, &tenant_id, &project_id, &deployment_id, &snapshot).await?;
    let claimed = claim(pool, &request, &tenant_id).await?;
    let manifest = build_and_validate(pool, root, &claimed).await?;
    assert_eq!(manifest.document_count, 0);
    promote_claimed_generation(pool, &claimed, &manifest).await?;
    assert_eq!(
        active_generation(pool, &project_id).await?,
        request.generation_id
    );
    assert_eq!(state(pool, request.generation_id).await?, "ready");
    Ok(())
}

async fn assert_versioned_eligible_corpus_is_promoted(
    pool: &PgPool,
    root: &std::path::Path,
    suffix: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let tenant_id = format!("search-corpus-tenant-{suffix}");
    let project_id = format!("search-corpus-project-{suffix}");
    let deployment_id = format!("search-corpus-deployment-{suffix}");
    create_project(pool, &tenant_id, &project_id, &format!("corpus-{suffix}")).await?;
    let mut input = snapshot(&project_id, "visible", "Guide", "corpusprobe");
    input["languages"] = serde_json::json!([
        {"id":"lang-en", "code":"en", "is_default":true, "enabled":true},
        {"id":"lang-fr", "code":"fr", "is_default":false, "enabled":false}
    ]);
    input["versions"] = serde_json::json!([
        {"id":"branch-main", "slug":"main", "is_default":true},
        {"id":"branch-v2", "slug":"v2", "is_default":false}
    ]);
    let visible = input["pages"][0].clone();
    let mut hidden = visible.clone();
    hidden["id"] = serde_json::json!("hidden");
    hidden["hidden"] = serde_json::json!(true);
    let mut noindex = visible.clone();
    noindex["id"] = serde_json::json!("noindex");
    noindex["config"] = serde_json::json!({"seo":{"noindex":true}});
    let mut disabled = visible.clone();
    disabled["id"] = serde_json::json!("disabled");
    disabled["language_id"] = serde_json::json!("lang-fr");
    disabled["language_code"] = serde_json::json!("fr");
    let mut v2 = visible.clone();
    v2["id"] = serde_json::json!("visible-v2");
    v2["version_id"] = serde_json::json!("branch-v2");
    input["pages"] = serde_json::json!([visible, hidden, noindex, disabled, v2]);
    insert_deployment(pool, &deployment_id, &project_id, 1, &input).await?;
    let request = register(pool, &tenant_id, &project_id, &deployment_id, &input).await?;
    let claimed = claim(pool, &request, &tenant_id).await?;
    let documents = load_claimed_snapshot_documents(pool, &claimed).await?;
    assert_eq!(documents.len(), 2);
    assert_eq!(documents[1].version_slug, "v2");
    let manifest = build_and_validate(pool, root, &claimed).await?;
    assert_eq!(manifest.format_version, 3);
    assert_eq!(manifest.document_count, 2);
    promote_claimed_generation(pool, &claimed, &manifest).await?;
    assert_eq!(
        active_generation(pool, &project_id).await?,
        request.generation_id
    );
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<(), nibleaf_search::SearchError> {
        let generation = nibleaf_search::open_generation(
            &root,
            &project_id,
            &deployment_id,
            request.generation_id,
        )?;
        let hits = nibleaf_search::search_generation(&generation, "corpusprobe", 50)?;
        assert_eq!(hits.len(), 2);
        assert!(
            hits.iter()
                .all(|hit| hit.page_id == "visible" || hit.page_id == "visible-v2")
        );
        let hits = nibleaf_search::search_generation_for_version(
            &generation,
            "corpusprobe",
            Some("v2"),
            Some("en"),
            1,
        )?;
        assert_eq!(hits[0].page_id, "visible-v2");
        assert_eq!(hits[0].description, "Summary");
        Ok(())
    })
    .await??;
    Ok(())
}

async fn assert_terminal_snapshot_failure(
    pool: &PgPool,
    tenant_id: &str,
    project_id: &str,
    suffix: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // A terminal snapshot defect must satisfy the registry's failed-state
    // evidence constraint so publication can observe and fail its deployment.
    let mut missing_content = snapshot(project_id, "broken-page", "Broken", "");
    if let Some(page) = missing_content["pages"][0].as_object_mut() {
        page.remove("content");
    }
    let legacy_format = serde_json::json!({"schema_version":1,"pages":[]});
    for (version, invalid_snapshot) in [(3, missing_content), (4, legacy_format)] {
        let deployment_id = format!("search-registry-deployment-failed-{version}-{suffix}");
        insert_deployment(pool, &deployment_id, project_id, version, &invalid_snapshot).await?;
        let request = register(
            pool,
            tenant_id,
            project_id,
            &deployment_id,
            &invalid_snapshot,
        )
        .await?;
        let claimed = claim(pool, &request, tenant_id).await?;
        let Err(failure) = load_claimed_snapshot_documents(pool, &claimed).await else {
            return Err(
                std::io::Error::other("invalid/legacy snapshot unexpectedly loaded").into(),
            );
        };
        record_claim_failure(pool, &claimed, &failure).await?;
        assert_eq!(state(pool, request.generation_id).await?, "failed");
        let completed: bool = sqlx::query_scalar(
            "SELECT completed_at IS NOT NULL FROM rust_search_generation WHERE generation_id=$1",
        )
        .bind(request.generation_id)
        .fetch_one(pool)
        .await?;
        assert!(completed);
        // Mirror the completion consumer so the one-active-publication guard
        // allows the next independent failure fixture in this same project.
        sqlx::query("UPDATE deployment SET status='FAILED'::\"DeploymentStatus\" WHERE id=$1")
            .bind(&deployment_id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

async fn create_project(
    pool: &PgPool,
    tenant_id: &str,
    project_id: &str,
    suffix: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO organization (id,name,slug) VALUES ($1,'Search registry test',$2)")
        .bind(tenant_id)
        .bind(format!("search-registry-{suffix}"))
        .execute(pool)
        .await?;
    sqlx::query(
        r#"INSERT INTO project (
              id, "organizationId", name, slug, config, "accessMode", "createdAt", "updatedAt"
           ) VALUES ($1,$2,'Search registry test',$3,'{}'::jsonb,'PUBLIC'::"ProjectAccessMode",NOW(),NOW())"#,
    )
    .bind(project_id)
    .bind(tenant_id)
    .bind(format!("search-registry-project-{suffix}"))
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_deployment(
    pool: &PgPool,
    deployment_id: &str,
    project_id: &str,
    version: i32,
    snapshot: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO deployment (
              id, "projectId", version, status, snapshot, "pagesCount", "createdAt"
           ) VALUES ($1,$2,$3,'BUILDING'::"DeploymentStatus",$4,1,NOW())"#,
    )
    .bind(deployment_id)
    .bind(project_id)
    .bind(version)
    .bind(snapshot)
    .execute(pool)
    .await?;
    Ok(())
}

async fn register(
    pool: &PgPool,
    tenant_id: &str,
    project_id: &str,
    deployment_id: &str,
    snapshot: &serde_json::Value,
) -> Result<nibleaf_search::registry::SearchBuildRequest, RegistryError> {
    let mut transaction = pool.begin().await?;
    let request = register_generation_build(
        &mut transaction,
        tenant_id,
        project_id,
        deployment_id,
        &canonical_snapshot_sha256(snapshot),
    )
    .await?;
    transaction.commit().await?;
    Ok(request)
}

async fn claim(
    pool: &PgPool,
    request: &nibleaf_search::registry::SearchBuildRequest,
    tenant_id: &str,
) -> Result<nibleaf_search::registry::ClaimedGeneration, RegistryError> {
    match claim_generation_build(
        pool,
        request,
        Some(tenant_id),
        "search-registry-e2e-worker",
        Duration::from_secs(30),
    )
    .await?
    {
        ClaimOutcome::Claimed(claim) => Ok(claim),
        ClaimOutcome::AlreadyReady => Err(RegistryError::Promotion(
            "expected a new generation claim in integration proof",
        )),
    }
}

async fn build_and_validate(
    pool: &PgPool,
    root: &std::path::Path,
    claim: &nibleaf_search::registry::ClaimedGeneration,
) -> Result<nibleaf_search::GenerationManifest, RegistryError> {
    let documents = load_claimed_snapshot_documents(pool, claim).await?;
    let root = root.to_path_buf();
    let claim = claim.clone();
    tokio::task::spawn_blocking(move || {
        build_or_reuse_claimed_generation(&root, &claim, &documents)
    })
    .await
    .map_err(|_| RegistryError::Promotion("joined test build failed"))?
}

async fn active_generation(pool: &PgPool, project_id: &str) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT generation_id FROM rust_search_active_generation WHERE project_id=$1",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
}

async fn state(pool: &PgPool, generation_id: Uuid) -> Result<String, sqlx::Error> {
    sqlx::query_scalar("SELECT state FROM rust_search_generation WHERE generation_id=$1")
        .bind(generation_id)
        .fetch_one(pool)
        .await
}

fn snapshot(project_id: &str, page_id: &str, title: &str, content: &str) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 2,
        "project": {"id": project_id},
        "languages": [{"id":"lang-en", "code":"en", "is_default":true, "enabled":true}],
        "versions": [{"id":"branch-main", "slug":"main", "is_default":true}],
        "pages": [{
            "id": page_id, "language_id":"lang-en", "language_code":"en", "version_id":"branch-main",
            "kind":"PAGE", "title":title, "path":page_id, "content":content,
            "hidden":false, "description":"Summary", "icon":"book", "config":null
        }]
    })
}
