use super::{ArtifactReference, ArtifactStore, ReplicaCacheConfig, catalog};
use crate::{
    SearchError,
    artifacts::{ArtifactExpectation, ArtifactLimits, import_artifact_runtime},
    execution::budget::QueryBudget,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

const MARKER: &str = "cache-owner.json";
const LOCK: &str = "cache-owner.lock";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    namespace: String,
    replica_id: Uuid,
    root_id: Uuid,
}
struct DiskRoot {
    _lock: File,
    root_id: Uuid,
    prior: Vec<Uuid>,
}
#[derive(Debug)]
pub(crate) struct CacheEntry {
    pub(crate) root: Arc<PathBuf>,
    id: Uuid,
    bytes: u64,
}
struct Record {
    entry: Arc<CacheEntry>,
    used: u64,
}
#[derive(Default)]
struct State {
    entries: BTreeMap<String, Record>,
    bytes: u64,
    tick: u64,
}
/// Artifact cache only, never an authorization/preparation/result ticket cache.
pub struct ReplicaCache {
    pool: PgPool,
    store: ArtifactStore,
    config: ReplicaCacheConfig,
    disk: DiskRoot,
    entries: RwLock<State>,
    single_flight: Mutex<()>,
    poisoned: AtomicBool,
    closing: AtomicBool,
}
impl std::fmt::Debug for ReplicaCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplicaCache")
            .field("replica_id", &self.config.replica_id)
            .field("poisoned", &self.poisoned.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}
impl ReplicaCache {
    /// Acquires the local process/root identity, reconciles only locally present
    /// owned entries and records actual cleanup. Offline/missing locations are
    /// not silently inferred purged. No task is spawned beyond awaited I/O joins.
    /// # Errors
    /// Unknown owner/files, duplicate local process/root identity, catalog or
    /// filesystem failures refuse startup; no serving fallback is selected.
    pub async fn open(
        pool: PgPool,
        store: ArtifactStore,
        config: ReplicaCacheConfig,
    ) -> Result<Arc<Self>, SearchError> {
        config
            .validate()
            .map_err(|_| SearchError::Distribution("replica cache configuration"))?;
        let cfg = config.clone();
        let disk = tokio::task::spawn_blocking(move || bootstrap(&cfg))
            .await
            .map_err(|_| SearchError::Distribution("cache startup task"))??;
        catalog::register_root(&pool, config.replica_id, disk.root_id).await?;
        let cache = Arc::new(Self {
            pool,
            store,
            config,
            disk,
            entries: RwLock::new(State::default()),
            single_flight: Mutex::new(()),
            poisoned: AtomicBool::new(false),
            closing: AtomicBool::new(false),
        });
        for id in &cache.disk.prior {
            cache.purge(*id).await?;
        }
        Ok(cache)
    }
    pub(crate) async fn acquire(
        &self,
        reference: &ArtifactReference,
        budget: &QueryBudget,
    ) -> Result<Arc<CacheEntry>, SearchError> {
        budget.check()?;
        reference.validate()?;
        if self.poisoned.load(Ordering::Acquire) || self.closing.load(Ordering::Acquire) {
            return Err(SearchError::Distribution("cache unavailable"));
        }
        if reference.document_count > budget.config.max_documents as u64 {
            return Err(SearchError::ResourceLimit("artifact document count"));
        }
        let key = reference.object_key.clone();
        if let Some(entry) = self.hit(&key).await {
            return Ok(entry);
        }
        metrics::counter!("nibleaf_search_cache_requests_total","outcome"=>"miss").increment(1);
        // Only waiting for another owned materializer is interruptible. Once
        // this job starts I/O/CPU/catalog work its future remains owned/joined.
        let _flight = tokio::select! {
            guard=self.single_flight.lock()=>guard,
            ()=budget.cancellation.cancelled()=>return Err(SearchError::Cancelled),
            ()=tokio::time::sleep_until(budget.deadline)=>return Err(SearchError::Deadline),
        };
        budget.check()?;
        if self.poisoned.load(Ordering::Acquire) || self.closing.load(Ordering::Acquire) {
            return Err(SearchError::Distribution("cache unavailable"));
        }
        if let Some(entry) = self.hit(&key).await {
            return Ok(entry);
        }
        self.make_room().await?;
        budget.check()?;
        let id = Uuid::now_v7();
        catalog::materializing(
            &self.pool,
            id,
            self.config.replica_id,
            self.disk.root_id,
            reference,
        )
        .await?;
        let started = std::time::Instant::now();
        let result = self.materialize(id, reference, budget).await;
        match result {
            Ok(entry) => {
                let mut state = self.entries.write().await;
                state.tick = state.tick.saturating_add(1);
                let used = state.tick;
                state.bytes = state
                    .bytes
                    .checked_add(entry.bytes)
                    .ok_or(SearchError::ResourceLimit("cache bytes"))?;
                state.entries.insert(
                    key,
                    Record {
                        entry: Arc::clone(&entry),
                        used,
                    },
                );
                metrics::counter!("nibleaf_search_cache_requests_total","outcome"=>"materialized")
                    .increment(1);
                metrics::histogram!("nibleaf_search_cache_materialization_seconds")
                    .record(started.elapsed().as_secs_f64());
                Ok(entry)
            }
            Err(error) => {
                metrics::counter!("nibleaf_search_cache_requests_total","outcome"=>"failed")
                    .increment(1);
                if self.purge(id).await.is_err() {
                    self.poisoned.store(true, Ordering::Release);
                    return Err(SearchError::Distribution("cache cleanup required"));
                }
                Err(error)
            }
        }
    }
    async fn hit(&self, key: &str) -> Option<Arc<CacheEntry>> {
        let mut state = self.entries.write().await;
        state.tick = state.tick.saturating_add(1);
        let used = state.tick;
        let record = state.entries.get_mut(key)?;
        record.used = used;
        metrics::counter!("nibleaf_search_cache_requests_total","outcome"=>"hit").increment(1);
        Some(Arc::clone(&record.entry))
    }
    async fn make_room(&self) -> Result<(), SearchError> {
        loop {
            let victim = {
                let mut state = self.entries.write().await;
                if state.entries.len() < self.config.max_entries
                    && state.bytes
                        <= self
                            .config
                            .max_bytes
                            .saturating_sub(self.config.max_entry_bytes)
                {
                    return Ok(());
                }
                let key = state
                    .entries
                    .iter()
                    .filter(|(_, r)| Arc::strong_count(&r.entry) == 1)
                    .min_by_key(|(_, r)| r.used)
                    .map(|(key, _)| key.clone())
                    .ok_or(SearchError::ResourceLimit("cache entries in use"))?;
                let record = state
                    .entries
                    .remove(&key)
                    .ok_or(SearchError::Integrity("cache state"))?;
                state.bytes = state.bytes.saturating_sub(record.entry.bytes);
                record.entry.id
            };
            if let Err(error) = self.purge(victim).await {
                self.poisoned.store(true, Ordering::Release);
                return Err(error);
            }
            metrics::counter!("nibleaf_search_cache_requests_total","outcome"=>"evicted")
                .increment(1);
        }
    }
    async fn materialize(
        &self,
        id: Uuid,
        reference: &ArtifactReference,
        budget: &QueryBudget,
    ) -> Result<Arc<CacheEntry>, SearchError> {
        let container = self.config.root.join(format!("entry-{id}"));
        let create = container.clone();
        tokio::task::spawn_blocking(move || private_directory(&create))
            .await
            .map_err(|_| SearchError::Distribution("cache directory task"))??;
        budget.check()?;
        let data = self.store.get_verified(reference).await?;
        budget.check()?;
        let expected = reference.clone();
        let target = container.clone();
        let limits = ArtifactLimits {
            max_compressed_bytes: self.store.maximum_bytes(),
            max_unpacked_bytes: self.config.max_entry_bytes,
            max_manifest_bytes: budget.config.max_manifest_bytes as u64,
            ..ArtifactLimits::default()
        };
        let import_budget = budget.clone();
        let result = tokio::task::spawn_blocking(move || {
            let archive = target.join("artifact.tar.zst");
            let mut file = private_file(&archive)?;
            file.write_all(&data)?;
            file.sync_all()?;
            drop(file);
            let native = target.join("native");
            let (receipt, bytes) = import_artifact_runtime(
                &archive,
                &native,
                &ArtifactExpectation {
                    project_id: expected.project_id.clone(),
                    deployment_id: expected.deployment_id.clone(),
                    generation_id: expected.generation_id,
                    artifact_sha256: expected.artifact_sha256.clone(),
                },
                limits,
                &import_budget,
            )?;
            if receipt.document_count != expected.document_count
                || receipt.documents_sha256 != expected.documents_sha256
            {
                return Err(SearchError::Integrity("materialized registry binding"));
            }
            fs::remove_file(archive)?;
            Ok((native, bytes))
        })
        .await
        .map_err(|_| SearchError::Distribution("cache import task"))??;
        budget.check()?;
        catalog::location_state(
            &self.pool,
            id,
            self.config.replica_id,
            self.disk.root_id,
            "ready",
            Some(result.1),
        )
        .await?;
        budget.check()?;
        Ok(Arc::new(CacheEntry {
            root: Arc::new(result.0),
            id,
            bytes: result.1,
        }))
    }
    async fn purge(&self, id: Uuid) -> Result<(), SearchError> {
        let path = self.config.root.join(format!("entry-{id}"));
        tokio::task::spawn_blocking(move || match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
                fs::remove_dir_all(path).map_err(SearchError::from)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err(SearchError::Integrity("cache cleanup path")),
            Err(error) => Err(SearchError::from(error)),
        })
        .await
        .map_err(|_| SearchError::Distribution("cache cleanup task"))??;
        catalog::location_state(
            &self.pool,
            id,
            self.config.replica_id,
            self.disk.root_id,
            "purged",
            None,
        )
        .await
    }
    /// Drive after every query job has actually joined, before pool closure.
    /// # Errors
    /// In-use/unsafe/failed cleanup is retained evidence, never a purge success.
    pub async fn close(&self) -> Result<(), SearchError> {
        self.closing.store(true, Ordering::Release);
        let ids = {
            let mut state = self.entries.write().await;
            if state
                .entries
                .values()
                .any(|r| Arc::strong_count(&r.entry) != 1)
            {
                return Err(SearchError::Distribution("cache still in use"));
            }
            let ids = state
                .entries
                .values()
                .map(|r| r.entry.id)
                .collect::<Vec<_>>();
            state.entries.clear();
            state.bytes = 0;
            ids
        };
        for id in ids {
            if let Err(error) = self.purge(id).await {
                self.poisoned.store(true, Ordering::Release);
                return Err(error);
            }
        }
        if self.poisoned.load(Ordering::Acquire) {
            return Err(SearchError::Distribution("cache cleanup required"));
        }
        Ok(())
    }
}
fn private_file(path: &Path) -> Result<File, SearchError> {
    let mut opts = OpenOptions::new();
    opts.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    Ok(opts.open(path)?)
}
fn private_directory(path: &Path) -> Result<(), SearchError> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}
fn bootstrap(config: &ReplicaCacheConfig) -> Result<DiskRoot, SearchError> {
    if !config.root.exists() {
        private_directory(&config.root)?;
    }
    let meta = fs::symlink_metadata(&config.root)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(SearchError::Integrity("cache root"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(SearchError::Integrity("cache root permissions"));
        }
    }
    let lock_path = config.root.join(LOCK);
    match fs::symlink_metadata(&lock_path) {
        Ok(meta) if !meta.file_type().is_file() => {
            return Err(SearchError::Integrity("cache lock path"));
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    let mut opts = OpenOptions::new();
    opts.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let lock = opts.open(lock_path)?;
    if !fs4::fs_std::FileExt::try_lock_exclusive(&lock)
        .map_err(|_| SearchError::Distribution("cache root already in use"))?
    {
        return Err(SearchError::Distribution("cache root already in use"));
    }
    let marker = config.root.join(MARKER);
    let mut prior = Vec::new();
    let owner = if marker.exists() {
        let meta = fs::symlink_metadata(&marker)?;
        if !meta.file_type().is_file() || meta.len() > 2048 {
            return Err(SearchError::Integrity("cache owner marker"));
        }
        let mut bytes = Vec::new();
        File::open(&marker)?.take(2049).read_to_end(&mut bytes)?;
        serde_json::from_slice::<Owner>(&bytes)?
    } else {
        for entry in fs::read_dir(&config.root)? {
            if entry?.file_name() != LOCK {
                return Err(SearchError::Integrity("unowned cache root"));
            }
        }
        let owner = Owner {
            namespace: "nibleaf-search-cache-v1".to_owned(),
            replica_id: config.replica_id,
            root_id: Uuid::now_v7(),
        };
        let mut file = private_file(&marker)?;
        serde_json::to_writer(&mut file, &owner)?;
        file.sync_all()?;
        owner
    };
    if owner.namespace != "nibleaf-search-cache-v1"
        || owner.replica_id != config.replica_id
        || owner.root_id.is_nil()
    {
        return Err(SearchError::Integrity("cache owner identity"));
    }
    for entry in fs::read_dir(&config.root)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| SearchError::Integrity("cache entry name"))?;
        if matches!(name.as_str(), MARKER | LOCK) {
            continue;
        }
        let meta = fs::symlink_metadata(entry.path())?;
        let id = name
            .strip_prefix("entry-")
            .and_then(|id| Uuid::parse_str(id).ok())
            .ok_or(SearchError::Integrity("unknown cache entry"))?;
        if id.is_nil() || !meta.is_dir() || meta.file_type().is_symlink() || prior.len() >= 128 {
            return Err(SearchError::Integrity("unsafe cache entry"));
        }
        prior.push(id);
    }
    Ok(DiskRoot {
        _lock: lock,
        root_id: owner.root_id,
        prior,
    })
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
