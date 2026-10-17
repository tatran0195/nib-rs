//! Measured local corpus/burst and non-destructive offline rebuild proof.
use super::*;
use axum::http::HeaderMap;
use nibleaf_search::{
    PublishedSearchRequest, SearchQueryConfig, SearchQueryExecutionError, SearchQueryService,
};
use std::{sync::Arc, time::Instant};
use tokio::{sync::Barrier, task::JoinSet};
use tokio_util::sync::CancellationToken;

struct LoadData {
    fixture: Fixture,
    request: SearchBuildRequest,
    manifest: nibleaf_search::GenerationManifest,
    snapshot_bytes: usize,
}
pub(super) async fn verify(pool: &PgPool, suffix: &str) -> TestResult {
    let data = build_load(pool, suffix).await?;
    let mut measurements = measure_load(pool, &data.fixture).await?;
    recover_offline(&data)?;
    measurements["documents"] = json!(5000);
    measurements["snapshot_json_bytes"] = json!(data.snapshot_bytes);
    measurements["process_high_water"] =
        json!(fs::read_to_string("/proc/self/status").ok().and_then(|s| {
            s.lines()
                .find(|line| line.starts_with("VmHWM:"))
                .map(str::to_owned)
        }));
    measurements["cold_disk_claim"] = json!(false);
    measurements["production_load_claim"] = json!(false);
    measurements["offline_rebuild_and_corruption"] = json!(true);
    eprintln!("local-load-recovery {measurements}");
    fs::remove_dir_all(&data.fixture.root)?;
    fs::remove_dir_all(data.fixture.root.with_extension("offline-rebuild"))?;
    Ok(())
}
async fn build_load(pool: &PgPool, suffix: &str) -> TestResult<LoadData> {
    let fixture = fixture(pool, suffix).await?;
    let deployment = format!("load-deployment-{suffix}");
    let mut snapshot = snapshot(&fixture, "loadprobe", 12);
    let mut pages = Vec::new();
    let language = format!("reader-language-{}", fixture.project.project_id.as_str());
    let version = format!("reader-branch-{}", fixture.project.project_id.as_str());
    for number in 0..5000 {
        let mut value = page(
            &format!("load-{number:05}"),
            &language,
            "en",
            &version,
            "loadprobe",
            true,
        );
        value["content"] = json!("loadprobe safe lexical corpus ordinary text ".repeat(16));
        pages.push(value);
    }
    snapshot["pages"] = json!(pages);
    insert_deployment(pool, &fixture, &deployment, 1, &snapshot).await?;
    let mut tx = pool.begin().await?;
    let request = register_generation_build(
        &mut tx,
        &fixture.project.organization_id,
        fixture.project.project_id.as_str(),
        &deployment,
        &canonical_snapshot_sha256(&snapshot),
    )
    .await?;
    tx.commit().await?;
    let ClaimOutcome::Claimed(claim) = claim_generation_build(
        pool,
        &request,
        Some(&fixture.project.organization_id),
        "load-recovery-proof",
        Duration::from_secs(30),
    )
    .await?
    else {
        return Err("load claim unavailable".into());
    };
    let documents = load_claimed_snapshot_documents(pool, &claim).await?;
    let manifest = tokio::task::block_in_place(|| {
        build_or_reuse_claimed_generation(&fixture.root, &claim, &documents)
    })?;
    promote_claimed_generation(pool, &claim, &manifest).await?;
    set_status(pool, &deployment, "READY").await?;
    let snapshot_bytes = snapshot.to_string().len();
    Ok(LoadData {
        fixture,
        request,
        manifest,
        snapshot_bytes,
    })
}
async fn measure_load(pool: &PgPool, fixture: &Fixture) -> TestResult<Value> {
    let (service, driver) = SearchQueryService::new(
        pool.clone(),
        Some(fixture.root.clone()),
        SearchQueryConfig::default(),
    )?;
    let stop = CancellationToken::new();
    let _stop_on_drop = stop.clone().drop_guard();
    let runner = tokio::spawn(driver.run(stop.clone()));
    let mut elapsed_ms = Vec::new();
    for _ in 0..3 {
        let started = Instant::now();
        let result = service
            .search_published(
                &fixture.project.project_id,
                &HeaderMap::new(),
                None,
                request_input(),
            )
            .await?
            .ok_or("load corpus concealed")?;
        assert_eq!(result.hits.len(), 12);
        elapsed_ms.push(started.elapsed().as_millis());
    }
    let barrier = Arc::new(Barrier::new(9));
    let mut jobs = JoinSet::new();
    for _ in 0..8 {
        let service = service.clone();
        let project = fixture.project.project_id.clone();
        let barrier = Arc::clone(&barrier);
        jobs.spawn(async move {
            barrier.wait().await;
            service
                .search_published(&project, &HeaderMap::new(), None, request_input())
                .await
        });
    }
    barrier.wait().await;
    let mut completed = 0;
    let mut busy = 0;
    while let Some(result) = jobs.join_next().await {
        match result? {
            Ok(Some(result)) => {
                assert_eq!(result.hits.len(), 12);
                completed += 1;
            }
            Err(SearchQueryExecutionError::Busy) => busy += 1,
            _ => return Err("unexpected burst outcome".into()),
        }
    }
    assert_eq!(completed + busy, 8);
    assert!(completed > 0 && busy > 0);
    stop.cancel();
    runner.await??;
    Ok(json!({"sequential_ms":elapsed_ms,"burst_requests":8,"completed":completed,"busy":busy}))
}
fn recover_offline(data: &LoadData) -> TestResult {
    let LoadData {
        fixture,
        request,
        manifest,
        ..
    } = data;
    let deployment = &request.deployment_id;
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_search-ops"));
    let rebuilt_root = fixture.root.with_extension("offline-rebuild");
    let result = std::process::Command::new(binary)
        .args([
            "rebuild-new-root",
            rebuilt_root.to_str().ok_or("root utf8")?,
            fixture.project.project_id.as_str(),
            deployment,
        ])
        .env(
            "NIBLEAF_DATABASE_URL",
            env::var("NIBLEAF_TEST_DATABASE_URL")?,
        )
        .output()?;
    if !result.status.success() {
        return Err("offline new-root rebuild failed".into());
    }
    let receipt: Value = serde_json::from_slice(&result.stdout)?;
    assert_eq!(receipt["documents"], 5000);
    assert_eq!(receipt["serving_pointer_changed"], false);
    let verified = std::process::Command::new(binary)
        .args([
            "verify-selected",
            rebuilt_root.to_str().ok_or("root utf8")?,
            fixture.project.project_id.as_str(),
            deployment,
        ])
        .env(
            "NIBLEAF_DATABASE_URL",
            env::var("NIBLEAF_TEST_DATABASE_URL")?,
        )
        .output()?;
    assert!(verified.status.success());
    // A repeat must refuse the existing root rather than overwrite even a test volume.
    let repeat = std::process::Command::new(binary)
        .args([
            "rebuild-new-root",
            rebuilt_root.to_str().ok_or("root utf8")?,
            fixture.project.project_id.as_str(),
            deployment,
        ])
        .env(
            "NIBLEAF_DATABASE_URL",
            env::var("NIBLEAF_TEST_DATABASE_URL")?,
        )
        .output()?;
    assert!(!repeat.status.success());
    let restored = nibleaf_search::open_generation(
        &rebuilt_root,
        fixture.project.project_id.as_str(),
        deployment,
        request.generation_id,
    )?;
    assert_eq!(
        nibleaf_search::validate_generation(&restored)?.documents_sha256,
        manifest.documents_sha256
    );
    // Damage only this disposable original root, then demonstrate failure closed
    // and a separately rebuilt root still producing identical scoped results.
    fs::remove_file(
        generation_directory(
            &fixture.root,
            fixture.project.project_id.as_str(),
            request.generation_id,
        )
        .join("manifest.json"),
    )?;
    assert!(
        nibleaf_search::open_generation(
            &fixture.root,
            fixture.project.project_id.as_str(),
            deployment,
            request.generation_id
        )
        .is_err()
    );
    assert_eq!(
        nibleaf_search::search_generation(&restored, "loadprobe", 12)?.len(),
        12
    );
    Ok(())
}
fn request_input() -> PublishedSearchRequest<'static> {
    PublishedSearchRequest {
        query: "loadprobe",
        language: None,
        version: None,
        limit: Some(50),
    }
}
