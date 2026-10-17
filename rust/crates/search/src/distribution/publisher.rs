//! Shutdown-owned worker build/export/put/verify/promotion with live lease.
use super::{ArtifactReference, ArtifactStore, catalog};
use crate::{
    GenerationManifest, SearchError,
    artifacts::{ArtifactLimits, ArtifactReceipt, export_artifact},
    registry::{self, ClaimOutcome, ClaimedGeneration, RegistryError, SearchBuildRequest},
};
use sqlx::PgPool;
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub struct ArtifactPublisher {
    pool: PgPool,
    root: Arc<PathBuf>,
    store: Arc<ArtifactStore>,
    _lock: File,
    uploads: Mutex<()>,
}
impl std::fmt::Debug for ArtifactPublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactPublisher").finish_non_exhaustive()
    }
}
impl ArtifactPublisher {
    /// Owns an unshared private build root/process lock and a bounded serialized
    /// upload spool. No provider write or generation promotion at construction.
    /// # Errors
    /// Unsafe/reused roots or local ownership failures refuse worker startup.
    pub async fn open(
        pool: PgPool,
        root: PathBuf,
        store: Arc<ArtifactStore>,
    ) -> Result<Arc<Self>, SearchError> {
        if !root.is_absolute() {
            return Err(SearchError::Distribution("worker root must be absolute"));
        }
        let directory = root.clone();
        let lock = tokio::task::spawn_blocking(move || open_root(&directory))
            .await
            .map_err(|_| SearchError::Distribution("worker root task"))??;
        Ok(Arc::new(Self {
            pool,
            root: Arc::new(root),
            store,
            _lock: lock,
            uploads: Mutex::new(()),
        }))
    }
    /// Drives the complete accepted generation lifecycle. Renewal failure
    /// cancels further stages but never drops active SQL/CPU/provider work.
    /// # Errors
    /// Records current-claim failures; stale workers cannot attach/promote.
    pub async fn fulfill(
        &self,
        request: &SearchBuildRequest,
        tenant: &str,
        worker: &str,
    ) -> Result<(), RegistryError> {
        let lease = Duration::from_secs(300);
        let outcome =
            registry::claim_generation_build(&self.pool, request, Some(tenant), worker, lease)
                .await?;
        let ClaimOutcome::Claimed(claim) = outcome else {
            return Ok(());
        };
        let invalidated = CancellationToken::new();
        let workflow = self.run_claim(&claim, &invalidated);
        tokio::pin!(workflow);
        let mut lost = None;
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.tick().await;
        let result = loop {
            tokio::select! {
                biased;
                result=&mut workflow=>break result,
                _=interval.tick()=>{
                    if lost.is_none() && let Err(error)=registry::renew_generation_lease(&self.pool,&claim,lease).await {
                        invalidated.cancel();lost=Some(error);
                    }
                }
            }
        };
        if let Some(error) = lost {
            return Err(error);
        }
        if let Err(error) = result {
            registry::record_claim_failure(&self.pool, &claim, &error).await?;
            return Err(error);
        }
        Ok(())
    }
    async fn run_claim(
        &self,
        claim: &ClaimedGeneration,
        invalidated: &CancellationToken,
    ) -> Result<(), RegistryError> {
        let documents = registry::load_claimed_snapshot_documents(&self.pool, claim).await?;
        check(invalidated)?;
        let root = Arc::clone(&self.root);
        let build_claim = claim.clone();
        let manifest = tokio::task::spawn_blocking(move || {
            registry::build_or_reuse_claimed_generation(&root, &build_claim, &documents)
        })
        .await
        .map_err(|_| RegistryError::BuildTask)??;
        check(invalidated)?;
        let _upload = tokio::select! {guard=self.uploads.lock()=>guard,()=invalidated.cancelled()=>return Err(RegistryError::LeaseLost)};
        check(invalidated)?;
        let (spool, receipt) = self.export(claim).await?;
        let uploaded = self
            .upload(
                claim,
                &manifest,
                &spool.join("artifact.tar.zst"),
                &receipt,
                invalidated,
            )
            .await;
        // Even an ambiguous provider response retains a durable attempted key;
        // remove only this joined local spool before any READY transition.
        tokio::task::spawn_blocking(move || fs::remove_dir_all(spool))
            .await
            .map_err(|_| RegistryError::BuildTask)?
            .map_err(SearchError::from)?;
        let reference = uploaded?;
        check(invalidated)?;
        registry::promote_claimed_generation_with_artifact(&self.pool, claim, &manifest, &reference)
            .await
    }
    async fn export(
        &self,
        claim: &ClaimedGeneration,
    ) -> Result<(PathBuf, ArtifactReceipt), RegistryError> {
        let root = Arc::clone(&self.root);
        let claim = claim.clone();
        let maximum = self.store.maximum_bytes();
        tokio::task::spawn_blocking(move || {
            let spool = root.join(".object-spool");
            let mut total = 0_u64;
            let mut count = 0;
            for entry in fs::read_dir(&spool).map_err(SearchError::from)? {
                let entry = entry.map_err(SearchError::from)?;
                let meta = fs::symlink_metadata(entry.path()).map_err(SearchError::from)?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| RegistryError::BuildConfiguration)?;
                if !meta.is_dir()
                    || meta.file_type().is_symlink()
                    || name
                        .strip_prefix("spool-")
                        .and_then(|s| uuid::Uuid::parse_str(s).ok())
                        .is_none()
                {
                    return Err(RegistryError::Promotion("unsafe worker spool"));
                }
                for file in fs::read_dir(entry.path()).map_err(SearchError::from)? {
                    let file = file.map_err(SearchError::from)?;
                    let meta = fs::symlink_metadata(file.path()).map_err(SearchError::from)?;
                    if !meta.file_type().is_file() || file.file_name() != "artifact.tar.zst" {
                        return Err(RegistryError::Promotion("unsafe worker spool"));
                    }
                    total = total
                        .checked_add(meta.len())
                        .ok_or(RegistryError::BuildConfiguration)?;
                }
                count += 1;
            }
            if count >= 4096
                || total
                    .checked_add(maximum)
                    .is_none_or(|value| value > 512 * 1024 * 1024)
            {
                return Err(SearchError::ResourceLimit("worker spool quota").into());
            }
            let generation = crate::open_generation(
                &root,
                &claim.request.project_id,
                &claim.request.deployment_id,
                claim.request.generation_id,
            )?;
            let container = spool.join(format!("spool-{}", claim.lease_token));
            private_directory(&container)?;
            let path = container.join("artifact.tar.zst");
            match export_artifact(
                &generation,
                &path,
                ArtifactLimits {
                    max_compressed_bytes: maximum,
                    ..ArtifactLimits::default()
                },
            ) {
                Ok(receipt) => Ok((container, receipt)),
                Err(error) => {
                    // The path is a new claim-token name created only here.
                    fs::remove_dir_all(&container).map_err(SearchError::from)?;
                    Err(error.into())
                }
            }
        })
        .await
        .map_err(|_| RegistryError::BuildTask)?
    }
    async fn upload(
        &self,
        claim: &ClaimedGeneration,
        manifest: &GenerationManifest,
        spool: &Path,
        receipt: &ArtifactReceipt,
        invalidated: &CancellationToken,
    ) -> Result<ArtifactReference, RegistryError> {
        if receipt.documents_sha256 != manifest.documents_sha256
            || receipt.document_count != manifest.document_count
        {
            return Err(RegistryError::Promotion("artifact source mismatch"));
        }
        check(invalidated)?;
        let reference = catalog::begin_upload(&self.pool, claim, receipt).await?;
        check(invalidated)?;
        if let Err(error) = self.store.put_verified(spool, receipt).await {
            catalog::upload_result(&self.pool, claim, &reference, false).await?;
            return Err(error.into());
        }
        check(invalidated)?;
        catalog::upload_result(&self.pool, claim, &reference, true).await?;
        Ok(reference)
    }
}
fn check(invalidated: &CancellationToken) -> Result<(), RegistryError> {
    if invalidated.is_cancelled() {
        Err(RegistryError::LeaseLost)
    } else {
        Ok(())
    }
}
fn open_root(root: &Path) -> Result<File, SearchError> {
    if !root.exists() {
        private_directory(root)?;
    }
    let meta = fs::symlink_metadata(root)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(SearchError::Integrity("worker object root"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(SearchError::Integrity("worker root permissions"));
        }
    }
    let path = root.join(".object-worker.lock");
    match fs::symlink_metadata(&path) {
        Ok(meta) if !meta.file_type().is_file() => {
            return Err(SearchError::Integrity("worker lock"));
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).write(true).read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(path)?;
    if !fs4::fs_std::FileExt::try_lock_exclusive(&lock)
        .map_err(|_| SearchError::Distribution("worker root already owned"))?
    {
        return Err(SearchError::Distribution("worker root already owned"));
    }
    let spool = root.join(".object-spool");
    if !spool.exists() {
        private_directory(&spool)?;
    }
    let meta = fs::symlink_metadata(&spool)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(SearchError::Integrity("worker spool"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(SearchError::Integrity("worker spool permissions"));
        }
    }
    Ok(lock)
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
