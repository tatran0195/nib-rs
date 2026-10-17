use std::{env, fmt, path::PathBuf, time::Duration};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryKind {
    Local,
    Object,
}
impl DeliveryKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Object => "object",
        }
    }
}
#[derive(Debug, thiserror::Error)]
#[error("invalid search distribution configuration: {0}")]
pub struct DistributionConfigError(pub(crate) &'static str);

#[derive(Clone)]
pub struct S3Config {
    pub(crate) endpoint: String,
    pub(crate) region: String,
    pub(crate) bucket: String,
    pub(crate) access_key: String,
    pub(crate) secret_key: String,
    pub(crate) allow_http: bool,
    pub(crate) timeout: Duration,
    pub(crate) max_archive_bytes: u64,
}
impl fmt::Debug for S3Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3Config")
            .field("credentials", &"[redacted]")
            .field("timeout", &self.timeout)
            .field("max_archive_bytes", &self.max_archive_bytes)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Debug)]
pub enum SearchDistributionConfig {
    Local,
    S3(S3Config),
}
const S3_KEYS: [&str; 5] = [
    "NIBLEAF_SEARCH_ARTIFACT_S3_ENDPOINT",
    "NIBLEAF_SEARCH_ARTIFACT_S3_REGION",
    "NIBLEAF_SEARCH_ARTIFACT_S3_BUCKET",
    "NIBLEAF_SEARCH_ARTIFACT_S3_ACCESS_KEY_ID",
    "NIBLEAF_SEARCH_ARTIFACT_S3_SECRET_ACCESS_KEY",
];
fn read(name: &'static str) -> Result<Option<String>, DistributionConfigError> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(DistributionConfigError(name)),
    }
}
fn required(name: &'static str) -> Result<String, DistributionConfigError> {
    read(name)?
        .filter(|v| !v.is_empty())
        .ok_or(DistributionConfigError(name))
}
fn number(name: &'static str, default: u64, maximum: u64) -> Result<u64, DistributionConfigError> {
    let value = read(name)?.map_or(Ok(default), |value| {
        value.parse().map_err(|_| DistributionConfigError(name))
    })?;
    if value == 0 || value > maximum {
        return Err(DistributionConfigError(name));
    }
    Ok(value)
}
impl SearchDistributionConfig {
    /// Explicit native storage mode. Incomplete object settings never silently
    /// fall back to local storage or ambient/metadata-server credentials.
    /// # Errors
    /// Returns only the offending setting name, never a supplied value/secret.
    pub fn from_env() -> Result<Self, DistributionConfigError> {
        match read("NIBLEAF_SEARCH_DISTRIBUTION")?.as_deref() {
            None => {
                for key in S3_KEYS.into_iter().chain([
                    "NIBLEAF_SEARCH_QUERY_CACHE_ROOT",
                    "NIBLEAF_SEARCH_REPLICA_ID",
                    "NIBLEAF_SEARCH_ARTIFACT_ALLOW_HTTP",
                    "NIBLEAF_SEARCH_ARTIFACT_TIMEOUT_MS",
                    "NIBLEAF_SEARCH_ARTIFACT_MAX_BYTES",
                    "NIBLEAF_SEARCH_CACHE_ENTRIES",
                    "NIBLEAF_SEARCH_CACHE_BYTES",
                    "NIBLEAF_SEARCH_CACHE_ENTRY_BYTES",
                ]) {
                    if read(key)?.is_some() {
                        return Err(DistributionConfigError("NIBLEAF_SEARCH_DISTRIBUTION"));
                    }
                }
                Ok(Self::Local)
            }
            Some("local") => Ok(Self::Local),
            Some("s3") => {
                let endpoint = required(S3_KEYS[0])?;
                let url =
                    url::Url::parse(&endpoint).map_err(|_| DistributionConfigError(S3_KEYS[0]))?;
                let allow_http = match read("NIBLEAF_SEARCH_ARTIFACT_ALLOW_HTTP")?.as_deref() {
                    None | Some("false") => false,
                    Some("true") => true,
                    _ => {
                        return Err(DistributionConfigError(
                            "NIBLEAF_SEARCH_ARTIFACT_ALLOW_HTTP",
                        ));
                    }
                };
                if !url.username().is_empty()
                    || url.password().is_some()
                    || url.query().is_some()
                    || url.fragment().is_some()
                    || url.path() != "/"
                    || !(url.scheme() == "https" || (allow_http && url.scheme() == "http"))
                {
                    return Err(DistributionConfigError(S3_KEYS[0]));
                }
                Ok(Self::S3(S3Config {
                    endpoint,
                    region: required(S3_KEYS[1])?,
                    bucket: required(S3_KEYS[2])?,
                    access_key: required(S3_KEYS[3])?,
                    secret_key: required(S3_KEYS[4])?,
                    allow_http,
                    timeout: Duration::from_millis(number(
                        "NIBLEAF_SEARCH_ARTIFACT_TIMEOUT_MS",
                        30000,
                        60000,
                    )?),
                    max_archive_bytes: number(
                        "NIBLEAF_SEARCH_ARTIFACT_MAX_BYTES",
                        64 * 1024 * 1024,
                        512 * 1024 * 1024,
                    )?,
                }))
            }
            _ => Err(DistributionConfigError("NIBLEAF_SEARCH_DISTRIBUTION")),
        }
    }
    #[must_use]
    pub const fn delivery_kind(&self) -> DeliveryKind {
        match self {
            Self::Local => DeliveryKind::Local,
            Self::S3(_) => DeliveryKind::Object,
        }
    }
    /// Guard total compressed upload buffers, not aggregate build/process RSS.
    /// # Errors
    /// Refuses a worker concurrency/archive product above 512 MiB.
    pub fn validate_worker(&self, concurrency: usize) -> Result<(), DistributionConfigError> {
        if let Self::S3(config) = self {
            let total = config.max_archive_bytes.checked_mul(
                u64::try_from(concurrency)
                    .map_err(|_| DistributionConfigError("NIBLEAF_WORKER_CONCURRENCY"))?,
            );
            if total.is_none_or(|bytes| bytes > 512 * 1024 * 1024) {
                return Err(DistributionConfigError("NIBLEAF_SEARCH_ARTIFACT_MAX_BYTES"));
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct ReplicaCacheConfig {
    pub root: PathBuf,
    pub replica_id: Uuid,
    pub max_entries: usize,
    pub max_bytes: u64,
    pub max_entry_bytes: u64,
}
impl ReplicaCacheConfig {
    pub(crate) fn validate(&self) -> Result<(), DistributionConfigError> {
        if !self.root.is_absolute()
            || self.replica_id.is_nil()
            || !(1..=128).contains(&self.max_entries)
            || self.max_bytes == 0
            || self.max_bytes > 4 * 1024 * 1024 * 1024
            || self.max_entry_bytes == 0
            || self.max_entry_bytes > 1024 * 1024 * 1024
            || self.max_entry_bytes > self.max_bytes
        {
            return Err(DistributionConfigError("NIBLEAF_SEARCH_QUERY_CACHE_ROOT"));
        }
        Ok(())
    }
    /// A dedicated private local root, not the worker build/shared filesystem.
    /// # Errors
    /// Refuses absent/invalid settings and impossible entry/total limits.
    pub fn from_env() -> Result<Self, DistributionConfigError> {
        let root = PathBuf::from(required("NIBLEAF_SEARCH_QUERY_CACHE_ROOT")?);
        if !root.is_absolute() {
            return Err(DistributionConfigError("NIBLEAF_SEARCH_QUERY_CACHE_ROOT"));
        }
        let replica_id = Uuid::parse_str(&required("NIBLEAF_SEARCH_REPLICA_ID")?)
            .map_err(|_| DistributionConfigError("NIBLEAF_SEARCH_REPLICA_ID"))?;
        let value = Self {
            root,
            replica_id,
            max_entries: usize::try_from(number("NIBLEAF_SEARCH_CACHE_ENTRIES", 8, 128)?)
                .map_err(|_| DistributionConfigError("NIBLEAF_SEARCH_CACHE_ENTRIES"))?,
            max_bytes: number(
                "NIBLEAF_SEARCH_CACHE_BYTES",
                512 * 1024 * 1024,
                4 * 1024 * 1024 * 1024,
            )?,
            max_entry_bytes: number(
                "NIBLEAF_SEARCH_CACHE_ENTRY_BYTES",
                256 * 1024 * 1024,
                1024 * 1024 * 1024,
            )?,
        };
        if value.replica_id.is_nil() || value.max_entry_bytes > value.max_bytes {
            return Err(DistributionConfigError("NIBLEAF_SEARCH_CACHE_ENTRY_BYTES"));
        }
        Ok(value)
    }
}
