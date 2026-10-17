use super::{ArtifactReference, SearchDistributionConfig, object_key};
use crate::{SearchError, artifacts::ArtifactReceipt};
use bytes::Bytes;
use futures_util::StreamExt;
use object_store::{
    ObjectStore, ObjectStoreExt, PutMode, PutOptions, RetryConfig,
    aws::{AmazonS3Builder, S3ConditionalPut},
    client::ClientOptions,
    path::Path as ObjectPath,
};
use sha2::{Digest, Sha256};
use std::{fmt, path::Path, sync::Arc};

#[derive(Clone)]
pub struct ArtifactStore {
    inner: Arc<dyn ObjectStore>,
    maximum: u64,
}
impl fmt::Debug for ArtifactStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArtifactStore")
            .field("maximum", &self.maximum)
            .finish_non_exhaustive()
    }
}
impl ArtifactStore {
    /// Embeds an explicitly supplied trusted store implementation (including
    /// provider-contract fixtures). HTTP bootstrap uses only configured S3.
    /// # Errors
    /// Refuses unbounded/zero transfer limits.
    pub fn from_provider(inner: Arc<dyn ObjectStore>, maximum: u64) -> Result<Self, SearchError> {
        if maximum == 0 || maximum > 512 * 1024 * 1024 {
            return Err(SearchError::ResourceLimit("artifact provider bytes"));
        }
        Ok(Self { inner, maximum })
    }
    /// Builds the explicitly credentialed native S3 adapter without ambient
    /// environment/IMDS discovery or bucket creation. Provider errors are masked.
    /// # Errors
    /// Invalid client/provider configuration returns only a stable category.
    pub fn configured(config: &SearchDistributionConfig) -> Result<Option<Self>, SearchError> {
        let SearchDistributionConfig::S3(config) = config else {
            return Ok(None);
        };
        let options = ClientOptions::new()
            .with_allow_http(config.allow_http)
            .with_timeout(config.timeout)
            .with_connect_timeout(config.timeout.min(std::time::Duration::from_secs(5)));
        let inner = AmazonS3Builder::new()
            .with_bucket_name(&config.bucket)
            .with_region(&config.region)
            .with_endpoint(&config.endpoint)
            .with_access_key_id(&config.access_key)
            .with_secret_access_key(&config.secret_key)
            .with_allow_http(config.allow_http)
            .with_virtual_hosted_style_request(false)
            .with_conditional_put(S3ConditionalPut::ETagMatch)
            .with_retry(RetryConfig {
                max_retries: 0,
                retry_timeout: config.timeout,
                ..RetryConfig::default()
            })
            .with_client_options(options)
            .build()
            .map_err(|_| SearchError::Distribution("provider configuration"))?;
        Ok(Some(Self {
            inner: Arc::new(inner),
            maximum: config.max_archive_bytes,
        }))
    }
    #[must_use]
    pub const fn maximum_bytes(&self) -> u64 {
        self.maximum
    }
    /// Read-only prefix/list permission probe. Only the first lazy page is
    /// consumed; no bucket creation, write or unbounded full listing is used.
    /// # Errors
    /// Any provider/network/permission failure is unavailable, not success.
    pub async fn probe(&self) -> Result<(), SearchError> {
        let prefix = ObjectPath::from("native-search/v1");
        let mut listing = self.inner.list(Some(&prefix));
        if let Some(result) = listing.next().await {
            result.map_err(|_| SearchError::Distribution("provider probe"))?;
        }
        Ok(())
    }
    /// Conditional single-attempt put, followed by actual bounded content
    /// verification (also on `AlreadyExists`). An error is not remote cancellation.
    /// # Errors
    /// Preserves integrity/size refusal and stable provider failures; never
    /// overwrites, trusts `ETag`, or automatically retries another engine/store.
    pub async fn put_verified(
        &self,
        file: &Path,
        receipt: &ArtifactReceipt,
    ) -> Result<(), SearchError> {
        if receipt.artifact_bytes == 0 || receipt.artifact_bytes > self.maximum {
            return Err(SearchError::ResourceLimit("artifact upload bytes"));
        }
        let expected = receipt.clone();
        let file = file.to_owned();
        let payload = tokio::task::spawn_blocking(move || {
            let metadata = std::fs::symlink_metadata(&file)?;
            if !metadata.file_type().is_file() || metadata.len() != expected.artifact_bytes {
                return Err(SearchError::Integrity("artifact upload file"));
            }
            let mut file = std::fs::File::open(file)?;
            let mut bytes = Vec::with_capacity(
                usize::try_from(expected.artifact_bytes)
                    .map_err(|_| SearchError::ResourceLimit("artifact upload bytes"))?,
            );
            std::io::Read::read_to_end(
                &mut std::io::Read::take(&mut file, expected.artifact_bytes + 1),
                &mut bytes,
            )?;
            if bytes.len() as u64 != expected.artifact_bytes
                || hex::encode(Sha256::digest(&bytes)) != expected.artifact_sha256
            {
                return Err(SearchError::Integrity("artifact upload digest"));
            }
            Ok(Bytes::from(bytes))
        })
        .await
        .map_err(|_| SearchError::Distribution("artifact file task"))??;
        let key = ObjectPath::from(object_key(
            &receipt.project_id,
            receipt.generation_id,
            &receipt.artifact_sha256,
        ));
        let result = self
            .inner
            .put_opts(&key, payload.into(), PutOptions::from(PutMode::Create))
            .await;
        match result {
            Ok(_)
            | Err(
                object_store::Error::AlreadyExists { .. }
                | object_store::Error::Precondition { .. },
            ) => {}
            Err(_) => return Err(SearchError::Distribution("artifact put unconfirmed")),
        }
        self.read_key(&key, receipt.artifact_bytes, &receipt.artifact_sha256)
            .await?;
        Ok(())
    }
    /// Downloads only the exact server-selected immutable reference into a
    /// bounded byte buffer. No caller-controlled URL/key or old fallback.
    /// # Errors
    /// Missing/short/large/corrupt/network data fails closed.
    pub async fn get_verified(
        &self,
        reference: &ArtifactReference,
    ) -> Result<Vec<u8>, SearchError> {
        reference.validate()?;
        self.read_key(
            &ObjectPath::from(reference.object_key.as_str()),
            reference.artifact_bytes,
            &reference.artifact_sha256,
        )
        .await
    }
    async fn read_key(
        &self,
        key: &ObjectPath,
        size: u64,
        digest: &str,
    ) -> Result<Vec<u8>, SearchError> {
        if size == 0 || size > self.maximum {
            return Err(SearchError::ResourceLimit("artifact transfer bytes"));
        }
        let result = self
            .inner
            .get(key)
            .await
            .map_err(|_| SearchError::Distribution("artifact get"))?;
        if result.meta.size != size {
            return Err(SearchError::Integrity("artifact content length"));
        }
        let mut stream = result.into_stream();
        let mut bytes = Vec::with_capacity(
            usize::try_from(size)
                .map_err(|_| SearchError::ResourceLimit("artifact transfer bytes"))?,
        );
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| SearchError::Distribution("artifact get body"))?;
            if chunk.len() as u64 > size.saturating_sub(bytes.len() as u64) {
                return Err(SearchError::ResourceLimit("artifact transfer bytes"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let expected = digest.to_owned();
        tokio::task::spawn_blocking(move || {
            if bytes.len() as u64 != size || hex::encode(Sha256::digest(&bytes)) != expected {
                return Err(SearchError::Integrity("artifact content digest"));
            }
            Ok(bytes)
        })
        .await
        .map_err(|_| SearchError::Distribution("artifact digest task"))?
    }
}
