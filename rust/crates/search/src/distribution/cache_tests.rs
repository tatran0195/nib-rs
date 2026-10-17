use super::*;
use crate::{SearchQueryConfig, distribution::object_key};
use std::{env, time::Duration};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
type TestResult = Result<(), Box<dyn std::error::Error>>;
fn config() -> ReplicaCacheConfig {
    ReplicaCacheConfig {
        root: env::temp_dir().join(format!("cache-safety-{}", Uuid::now_v7())),
        replica_id: Uuid::now_v7(),
        max_entries: 1,
        max_bytes: 1024,
        max_entry_bytes: 1024,
    }
}
#[test]
fn local_owner_lock_unknown_files_and_identity_changes_fail_closed() -> TestResult {
    let cfg = config();
    let disk = bootstrap(&cfg)?;
    assert!(bootstrap(&cfg).is_err());
    let root = disk.root_id;
    drop(disk);
    let disk = bootstrap(&cfg)?;
    assert_eq!(disk.root_id, root);
    drop(disk);
    let mut wrong = cfg.clone();
    wrong.replica_id = Uuid::now_v7();
    assert!(bootstrap(&wrong).is_err());
    fs::write(cfg.root.join("unknown-operator-data"), b"preserve")?;
    assert!(bootstrap(&cfg).is_err());
    assert_eq!(
        fs::read(cfg.root.join("unknown-operator-data"))?,
        b"preserve"
    );
    fs::remove_dir_all(cfg.root)?;
    Ok(())
}
#[cfg(unix)]
#[test]
fn a_broken_lock_symlink_is_never_followed() -> TestResult {
    use std::os::unix::fs::symlink;
    let cfg = config();
    let disk = bootstrap(&cfg)?;
    drop(disk);
    fs::remove_file(cfg.root.join(LOCK))?;
    let outside = cfg.root.with_extension("outside");
    symlink(&outside, cfg.root.join(LOCK))?;
    assert!(bootstrap(&cfg).is_err());
    assert!(!outside.exists());
    fs::remove_dir_all(cfg.root)?;
    Ok(())
}
fn cache(cfg: ReplicaCacheConfig) -> Result<ReplicaCache, SearchError> {
    let disk = bootstrap(&cfg)?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgresql://unused:unused@127.0.0.1:1/unused")
        .map_err(|_| SearchError::Distribution("unit lazy pool"))?;
    let store =
        ArtifactStore::from_provider(Arc::new(object_store::memory::InMemory::new()), 1024)?;
    Ok(ReplicaCache {
        pool,
        store,
        config: cfg,
        disk,
        entries: RwLock::new(State::default()),
        single_flight: Mutex::new(()),
        poisoned: AtomicBool::new(false),
        closing: AtomicBool::new(false),
    })
}
#[tokio::test]
async fn active_arc_lease_prevents_eviction_or_close_before_real_work_finishes() -> TestResult {
    let cfg = config();
    let cache = cache(cfg.clone())?;
    let entry = Arc::new(CacheEntry {
        root: Arc::new(cfg.root.join("native")),
        id: Uuid::now_v7(),
        bytes: 512,
    });
    let _accepted = Arc::clone(&entry);
    {
        let mut state = cache.entries.write().await;
        state.bytes = 512;
        state
            .entries
            .insert("key".to_owned(), Record { entry, used: 0 });
    }
    assert!(matches!(
        cache.make_room().await,
        Err(SearchError::ResourceLimit(_))
    ));
    assert!(cache.close().await.is_err());
    assert_eq!(cache.entries.read().await.entries.len(), 1);
    drop(cache);
    fs::remove_dir_all(cfg.root)?;
    Ok(())
}
#[tokio::test]
async fn waiting_single_flight_is_deadlined_before_catalog_or_network_work() -> TestResult {
    let cfg = config();
    let cache = cache(cfg.clone())?;
    let flight_guard = cache.single_flight.lock().await;
    let id = Uuid::now_v7();
    let reference = ArtifactReference {
        generation_id: id,
        project_id: "project".to_owned(),
        tenant_id: "tenant".to_owned(),
        deployment_id: "deployment".to_owned(),
        snapshot_sha256: "0".repeat(64),
        artifact_sha256: "1".repeat(64),
        artifact_bytes: 1,
        documents_sha256: "2".repeat(64),
        document_count: 0,
        object_key: object_key("project", id, &"1".repeat(64)),
    };
    let started = Instant::now();
    let budget = QueryBudget {
        config: SearchQueryConfig::default(),
        started,
        deadline: started + Duration::from_millis(20),
        cancellation: CancellationToken::new(),
    };
    assert!(matches!(
        cache.acquire(&reference, &budget).await,
        Err(SearchError::Deadline)
    ));
    assert_eq!(fs::read_dir(&cfg.root)?.count(), 2);
    drop(flight_guard);
    drop(cache);
    fs::remove_dir_all(cfg.root)?;
    Ok(())
}
