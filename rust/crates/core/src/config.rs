use std::{
    env,
    fmt::{self, Debug, Formatter},
    net::SocketAddr,
    path::PathBuf,
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use http::HeaderValue;
use subtle::ConstantTimeEq;

#[derive(Clone, Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("NIBLEAF_DATABASE_URL (or legacy POSTGRES_URL) is required")]
    MissingDatabaseUrl,
    #[error("NIBLEAF_METRICS_BEARER_TOKEN is required and must not be empty")]
    MissingMetricsBearerToken,
    #[error("NIBLEAF_INVITATION_SIGNING_KEY is required")]
    MissingInvitationSigningKey,
    #[error("an invitation signing key must be base64-encoded 32 bytes")]
    InvalidInvitationSigningKey,
    #[error("NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY is required")]
    MissingIdentityDeliveryEncryptionKey,
    #[error("the identity delivery encryption key must be base64-encoded 32 bytes")]
    InvalidIdentityDeliveryEncryptionKey,
    #[error(
        "NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY_VERSION must be an integer from 1 through 65535"
    )]
    InvalidIdentityDeliveryEncryptionKeyVersion,
    #[error(
        "NIBLEAF_IDENTITY_DELIVERY_PREVIOUS_KEYS must contain at most two distinct version:base64-32-byte-key entries"
    )]
    InvalidIdentityDeliveryPreviousKeys,
    #[error("bind address is invalid: {0}")]
    InvalidBindAddress(#[from] std::net::AddrParseError),
    #[error("NIBLEAF_WORKER_CONCURRENCY must be between 1 and 64")]
    InvalidWorkerConcurrency,
    #[error("NIBLEAF_SUPPORT_EXPIRY_RECONCILIATION_INTERVAL_SECONDS must be between 5 and 300")]
    InvalidSupportExpiryReconciliationInterval,
    #[error("NIBLEAF_DISABLE_SIGNUP must be true or false when set")]
    InvalidDisableSignup,
    #[error("NIBLEAF_NATIVE_IDENTITY_ENABLED must be true or false when set")]
    InvalidNativeIdentityEnabled,
    #[error("NIBLEAF_PUBLIC_SEARCH_ENABLED must be true or false when set")]
    InvalidPublicSearchEnabled,
    #[error(
        "NIBLEAF_AUTHORITY must be transition or native; native requires NIBLEAF_NATIVE_IDENTITY_ENABLED=true"
    )]
    InvalidAuthenticationAuthority,
}

/// Native authority never falls back to Better Auth tables/credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticationAuthority {
    Transition,
    Native,
}
impl AuthenticationAuthority {
    fn parse(value: Option<&str>, native_enabled: bool) -> Result<Self, ConfigError> {
        match value {
            None | Some("transition") => Ok(Self::Transition),
            Some("native") if native_enabled => Ok(Self::Native),
            _ => Err(ConfigError::InvalidAuthenticationAuthority),
        }
    }
    fn from_env(native_enabled: bool) -> Result<Self, ConfigError> {
        let value = match env::var("NIBLEAF_AUTHORITY") {
            Ok(value) => Some(value),
            Err(env::VarError::NotPresent) => None,
            Err(env::VarError::NotUnicode(_)) => {
                return Err(ConfigError::InvalidAuthenticationAuthority);
            }
        };
        Self::parse(value.as_deref(), native_enabled)
    }
}

/// Process configuration. Secrets are held only for their required lifetime;
/// its custom `Debug` implementation redacts them as a defense in depth.
#[derive(Clone)]
pub struct InvitationEmailConfig {
    pub application_url: String,
    pub from: String,
    pub postmark_api_key: Option<String>,
    pub postmark_message_stream: Option<String>,
    pub smtp_url: Option<String>,
}

impl Debug for InvitationEmailConfig {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InvitationEmailConfig")
            .field("application_url", &self.application_url)
            .field("from", &self.from)
            .field(
                "postmark_api_key",
                &self.postmark_api_key.as_ref().map(|_| "[redacted]"),
            )
            .field("postmark_message_stream", &self.postmark_message_stream)
            .field("smtp_url", &self.smtp_url.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

/// Versioned retained key material for decrypting already-queued identity OTP
/// delivery payloads during a bounded encryption-key rotation. New payloads
/// always use the separately configured current key.
#[derive(Clone)]
pub struct IdentityDeliveryPreviousKey {
    pub version: u16,
    pub material: Vec<u8>,
}

impl Debug for IdentityDeliveryPreviousKey {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IdentityDeliveryPreviousKey")
            .field("version", &self.version)
            .field("material", &"[redacted]")
            .finish()
    }
}

#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub metrics_bearer_token: String,
    pub invitation_email: InvitationEmailConfig,
    pub invitation_signing_key: Vec<u8>,
    pub invitation_previous_signing_keys: Vec<Vec<u8>>,
    /// Current-key material for short-lived encrypted identity delivery
    /// payloads. It is intentionally distinct from signing credentials.
    pub identity_delivery_encryption_key: Vec<u8>,
    /// The version written into newly encrypted OTP-delivery ciphertexts.
    pub identity_delivery_encryption_key_version: u16,
    /// At most two retained keys for worker-side decryption of pending delivery
    /// ciphertexts created before a bounded rotation. They are never used to
    /// encrypt new payloads.
    pub identity_delivery_previous_keys: Vec<IdentityDeliveryPreviousKey>,
    pub trusted_origins: Vec<String>,
    pub disable_signup: bool,
    /// Explicit rollout gate. Native routes stay absent until an operator has
    /// completed environment validation and a planned migration step.
    pub native_identity_enabled: bool,
    pub authentication_authority: AuthenticationAuthority,
    pub public_search_enabled: bool,
    /// Required in environments that process native publication work or serve
    /// the authenticated native-search query. A worker records a terminal
    /// generation failure rather than indexing on an ephemeral disk when it is
    /// absent; API query admission returns unavailable without it.
    pub search_index_root: Option<PathBuf>,
    pub bind_addr: SocketAddr,
    pub worker_bind_addr: SocketAddr,
    pub database_connect_timeout: Duration,
    pub worker_concurrency: usize,
    /// Bounded cadence for durable reconciliation of expired native support
    /// sessions. A short interval limits audit lag without turning the worker
    /// into a constant database poller.
    pub support_expiry_reconciliation_interval: Duration,
    pub environment: String,
}

impl Debug for Config {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Config")
            .field("database_url", &"[redacted]")
            .field("metrics_bearer_token", &"[redacted]")
            .field("invitation_email", &self.invitation_email)
            .field("invitation_signing_key", &"[redacted]")
            .field("invitation_previous_signing_keys", &"[redacted]")
            .field("identity_delivery_encryption_key", &"[redacted]")
            .field(
                "identity_delivery_encryption_key_version",
                &self.identity_delivery_encryption_key_version,
            )
            .field(
                "identity_delivery_previous_keys",
                &self.identity_delivery_previous_keys,
            )
            .field("trusted_origins", &self.trusted_origins)
            .field("disable_signup", &self.disable_signup)
            .field("native_identity_enabled", &self.native_identity_enabled)
            .field("authentication_authority", &self.authentication_authority)
            .field("public_search_enabled", &self.public_search_enabled)
            .field("search_index_root", &self.search_index_root)
            .field("bind_addr", &self.bind_addr)
            .field("worker_bind_addr", &self.worker_bind_addr)
            .field("database_connect_timeout", &self.database_connect_timeout)
            .field("worker_concurrency", &self.worker_concurrency)
            .field(
                "support_expiry_reconciliation_interval",
                &self.support_expiry_reconciliation_interval,
            )
            .field("environment", &self.environment)
            .finish()
    }
}

fn optional_non_blank_env(primary: &str, legacy: &str) -> Option<String> {
    env::var(primary)
        .ok()
        .or_else(|| env::var(legacy).ok())
        .filter(|value| !value.trim().is_empty())
}

fn parse_bool(value: Option<String>, error: ConfigError) -> Result<bool, ConfigError> {
    Ok(value
        .map(|value| match value.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(error),
        })
        .transpose()?
        .unwrap_or(false))
}

fn parse_native_identity_enabled(value: Option<String>) -> Result<bool, ConfigError> {
    parse_bool(
        value.filter(|value| !value.trim().is_empty()),
        ConfigError::InvalidNativeIdentityEnabled,
    )
}

fn read_trusted_origins(application_url: &str) -> Vec<String> {
    env::var("NIBLEAF_TRUSTED_ORIGINS")
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|origins| !origins.is_empty())
        .unwrap_or_else(|| vec![application_url.to_owned()])
}

const DEFAULT_SUPPORT_EXPIRY_RECONCILIATION_INTERVAL_SECONDS: u64 = 30;
const MIN_SUPPORT_EXPIRY_RECONCILIATION_INTERVAL_SECONDS: u64 = 5;
const MAX_SUPPORT_EXPIRY_RECONCILIATION_INTERVAL_SECONDS: u64 = 300;

fn parse_support_expiry_reconciliation_interval(
    value: Option<String>,
) -> Result<Duration, ConfigError> {
    let seconds = match value {
        Some(value) => value
            .parse::<u64>()
            .map_err(|_| ConfigError::InvalidSupportExpiryReconciliationInterval)?,
        None => DEFAULT_SUPPORT_EXPIRY_RECONCILIATION_INTERVAL_SECONDS,
    };
    if !(MIN_SUPPORT_EXPIRY_RECONCILIATION_INTERVAL_SECONDS
        ..=MAX_SUPPORT_EXPIRY_RECONCILIATION_INTERVAL_SECONDS)
        .contains(&seconds)
    {
        return Err(ConfigError::InvalidSupportExpiryReconciliationInterval);
    }
    Ok(Duration::from_secs(seconds))
}

fn decode_key(value: &str, error: ConfigError) -> Result<Vec<u8>, ConfigError> {
    let decoded = STANDARD.decode(value).map_err(|_| error.clone())?;
    if decoded.len() != 32 {
        return Err(error);
    }
    Ok(decoded)
}

fn decode_invitation_signing_key(value: &str) -> Result<Vec<u8>, ConfigError> {
    decode_key(value, ConfigError::InvalidInvitationSigningKey)
}

fn decode_identity_delivery_encryption_key(value: &str) -> Result<Vec<u8>, ConfigError> {
    decode_key(value, ConfigError::InvalidIdentityDeliveryEncryptionKey)
}

const MAX_IDENTITY_DELIVERY_PREVIOUS_KEYS: usize = 2;

fn parse_identity_delivery_key_version(value: Option<String>) -> Result<u16, ConfigError> {
    match value {
        None => Ok(1),
        Some(value) => value
            .parse::<u16>()
            .ok()
            .filter(|version| *version > 0)
            .ok_or(ConfigError::InvalidIdentityDeliveryEncryptionKeyVersion),
    }
}

fn parse_identity_delivery_previous_keys(
    value: Option<String>,
    current_version: u16,
    current_material: &[u8],
) -> Result<Vec<IdentityDeliveryPreviousKey>, ConfigError> {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    let entries = value.split(',').collect::<Vec<_>>();
    if entries.is_empty()
        || entries.len() > MAX_IDENTITY_DELIVERY_PREVIOUS_KEYS
        || entries.iter().any(|entry| entry.trim().is_empty())
    {
        return Err(ConfigError::InvalidIdentityDeliveryPreviousKeys);
    }
    let mut keys = Vec::with_capacity(entries.len());
    for entry in entries {
        let (version, material) = entry
            .trim()
            .split_once(':')
            .ok_or(ConfigError::InvalidIdentityDeliveryPreviousKeys)?;
        if material.contains(':') {
            return Err(ConfigError::InvalidIdentityDeliveryPreviousKeys);
        }
        let version = version
            .parse::<u16>()
            .ok()
            .filter(|version| *version > 0)
            .ok_or(ConfigError::InvalidIdentityDeliveryPreviousKeys)?;
        let material = decode_key(material, ConfigError::InvalidIdentityDeliveryPreviousKeys)?;
        if version == current_version
            || material == current_material
            || keys.iter().any(|key: &IdentityDeliveryPreviousKey| {
                key.version == version || key.material == material
            })
        {
            return Err(ConfigError::InvalidIdentityDeliveryPreviousKeys);
        }
        keys.push(IdentityDeliveryPreviousKey { version, material });
    }
    Ok(keys)
}

impl Config {
    /// Loads process configuration from the environment.
    ///
    /// # Errors
    ///
    /// Returns an error when the database URL, bind address, or worker
    /// concurrency is missing or invalid.
    pub fn from_env() -> Result<Self, ConfigError> {
        let database_url = env::var("NIBLEAF_DATABASE_URL")
            .or_else(|_| env::var("POSTGRES_URL"))
            .map_err(|_| ConfigError::MissingDatabaseUrl)?;
        let metrics_bearer_token = env::var("NIBLEAF_METRICS_BEARER_TOKEN")
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or(ConfigError::MissingMetricsBearerToken)?;
        let invitation_signing_key = env::var("NIBLEAF_INVITATION_SIGNING_KEY")
            .map_err(|_| ConfigError::MissingInvitationSigningKey)
            .and_then(|value| decode_invitation_signing_key(&value))?;
        let invitation_previous_signing_keys = env::var("NIBLEAF_INVITATION_PREVIOUS_SIGNING_KEYS")
            .unwrap_or_default()
            .split(',')
            .filter(|value| !value.is_empty())
            .map(decode_invitation_signing_key)
            .collect::<Result<Vec<_>, _>>()?;
        let identity_delivery_encryption_key = env::var("NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY")
            .map_err(|_| ConfigError::MissingIdentityDeliveryEncryptionKey)
            .and_then(|value| decode_identity_delivery_encryption_key(&value))?;
        let identity_delivery_encryption_key_version = parse_identity_delivery_key_version(
            env::var("NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY_VERSION").ok(),
        )?;
        let identity_delivery_previous_keys = parse_identity_delivery_previous_keys(
            env::var("NIBLEAF_IDENTITY_DELIVERY_PREVIOUS_KEYS").ok(),
            identity_delivery_encryption_key_version,
            &identity_delivery_encryption_key,
        )?;
        let invitation_email = InvitationEmailConfig {
            application_url: env::var("NIBLEAF_APP_URL")
                .unwrap_or_else(|_| "http://localhost:4310".to_owned()),
            // Keep the legacy variable names usable throughout the migration;
            // prefixed values override them when both are present.
            from: optional_non_blank_env("NIBLEAF_EMAIL_FROM", "EMAIL_FROM")
                .unwrap_or_else(|| "nibleaf@localhost".to_owned()),
            postmark_api_key: optional_non_blank_env(
                "NIBLEAF_POSTMARK_API_KEY",
                "POSTMARK_API_KEY",
            ),
            postmark_message_stream: optional_non_blank_env(
                "NIBLEAF_POSTMARK_MESSAGE_STREAM",
                "POSTMARK_MESSAGE_STREAM",
            ),
            smtp_url: optional_non_blank_env("NIBLEAF_SMTP_URL", "SMTP_URL"),
        };
        let trusted_origins = read_trusted_origins(&invitation_email.application_url);
        let disable_signup = parse_bool(
            optional_non_blank_env("NIBLEAF_DISABLE_SIGNUP", "DISABLE_SIGNUP"),
            ConfigError::InvalidDisableSignup,
        )?;
        let native_identity_enabled =
            parse_native_identity_enabled(env::var("NIBLEAF_NATIVE_IDENTITY_ENABLED").ok())?;
        let public_search_enabled = match env::var("NIBLEAF_PUBLIC_SEARCH_ENABLED") {
            Ok(value) => parse_bool(Some(value), ConfigError::InvalidPublicSearchEnabled)?,
            Err(env::VarError::NotPresent) => false,
            Err(env::VarError::NotUnicode(_)) => {
                return Err(ConfigError::InvalidPublicSearchEnabled);
            }
        };
        let search_index_root = env::var("NIBLEAF_SEARCH_INDEX_ROOT")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from);
        let bind_addr = env::var("NIBLEAF_BIND_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:4311".to_owned())
            .parse()?;
        let worker_bind_addr = env::var("NIBLEAF_WORKER_BIND_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:4312".to_owned())
            .parse()?;
        let worker_concurrency = env::var("NIBLEAF_WORKER_CONCURRENCY")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(4);
        if !(1..=64).contains(&worker_concurrency) {
            return Err(ConfigError::InvalidWorkerConcurrency);
        }
        let support_expiry_reconciliation_interval = parse_support_expiry_reconciliation_interval(
            env::var("NIBLEAF_SUPPORT_EXPIRY_RECONCILIATION_INTERVAL_SECONDS").ok(),
        )?;

        Ok(Self {
            database_url,
            metrics_bearer_token,
            invitation_email,
            invitation_signing_key,
            invitation_previous_signing_keys,
            identity_delivery_encryption_key,
            identity_delivery_encryption_key_version,
            identity_delivery_previous_keys,
            trusted_origins,
            disable_signup,
            native_identity_enabled,
            authentication_authority: AuthenticationAuthority::from_env(native_identity_enabled)?,
            public_search_enabled,
            search_index_root,
            bind_addr,
            worker_bind_addr,
            database_connect_timeout: Duration::from_secs(5),
            worker_concurrency,
            support_expiry_reconciliation_interval,
            environment: env::var("NIBLEAF_ENV").unwrap_or_else(|_| "development".to_owned()),
        })
    }

    /// Verifies the operator credential without accepting it as an application
    /// session. Constant-time equality avoids making token-prefix guesses a
    /// useful signal on a network-visible metrics listener.
    #[must_use]
    pub fn authorizes_metrics(&self, authorization: Option<&HeaderValue>) -> bool {
        let Some(value) = authorization.and_then(|value| value.to_str().ok()) else {
            return false;
        };
        let Some(provided) = value.strip_prefix("Bearer ") else {
            return false;
        };
        bool::from(
            self.metrics_bearer_token
                .as_bytes()
                .ct_eq(provided.as_bytes()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConfigError, decode_invitation_signing_key, parse_bool,
        parse_identity_delivery_key_version, parse_identity_delivery_previous_keys,
        parse_support_expiry_reconciliation_interval,
    };

    #[test]
    fn native_authority_is_explicit_and_requires_native_routes() {
        use super::AuthenticationAuthority as Authority;
        assert_eq!(
            Authority::parse(None, false).ok(),
            Some(Authority::Transition)
        );
        assert_eq!(
            Authority::parse(Some("native"), true).ok(),
            Some(Authority::Native)
        );
        for (value, enabled) in [
            ("", true),
            ("Native", true),
            ("legacy", true),
            ("native", false),
        ] {
            assert!(Authority::parse(Some(value), enabled).is_err());
        }
    }

    #[test]
    fn invitation_signing_key_requires_exactly_32_decoded_bytes() {
        assert!(
            decode_invitation_signing_key("MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=").is_ok()
        );
        assert!(decode_invitation_signing_key("not base64").is_err());
        assert!(decode_invitation_signing_key("c2hvcnQ=").is_err());
    }

    #[test]
    fn identity_delivery_key_ring_is_bounded_and_unambiguous() -> Result<(), ConfigError> {
        let current = b"0123456789abcdef0123456789abcdef";
        let old_one = "1:ZmVkY2JhOTg3NjU0MzIxMGZlZGNiYTk4NzY1NDMyMTA=";
        let old_two = "2:YWJjZGVmMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODk=";
        let keys = parse_identity_delivery_previous_keys(
            Some(format!("{old_one},{old_two}")),
            3,
            current,
        )?;
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].version, 1);
        assert_eq!(keys[1].version, 2);
        assert!(matches!(
            parse_identity_delivery_previous_keys(Some(old_one.to_owned()), 1, current),
            Err(ConfigError::InvalidIdentityDeliveryPreviousKeys)
        ));
        assert!(matches!(
            parse_identity_delivery_previous_keys(
                Some(format!("{old_one},{old_two},3:{old_one}")),
                4,
                current,
            ),
            Err(ConfigError::InvalidIdentityDeliveryPreviousKeys)
        ));
        assert!(matches!(parse_identity_delivery_key_version(None), Ok(1)));
        assert!(matches!(
            parse_identity_delivery_key_version(Some("0".to_owned())),
            Err(ConfigError::InvalidIdentityDeliveryEncryptionKeyVersion)
        ));
        Ok(())
    }

    #[test]
    fn support_expiry_reconciliation_interval_is_bounded() {
        assert!(matches!(
            parse_support_expiry_reconciliation_interval(None),
            Ok(interval) if interval.as_secs() == 30
        ));
        assert!(matches!(
            parse_support_expiry_reconciliation_interval(Some("5".to_owned())),
            Ok(interval) if interval.as_secs() == 5
        ));
        for value in ["0", "4", "301", "invalid"] {
            assert!(matches!(
                parse_support_expiry_reconciliation_interval(Some(value.to_owned())),
                Err(ConfigError::InvalidSupportExpiryReconciliationInterval)
            ));
        }
    }

    #[test]
    fn rollout_booleans_fail_closed() {
        assert!(matches!(
            parse_bool(
                Some("true".to_owned()),
                ConfigError::InvalidNativeIdentityEnabled
            ),
            Ok(true)
        ));
        assert!(matches!(
            parse_bool(
                Some("false".to_owned()),
                ConfigError::InvalidNativeIdentityEnabled
            ),
            Ok(false)
        ));
        assert!(matches!(
            parse_bool(
                Some("yes".to_owned()),
                ConfigError::InvalidNativeIdentityEnabled
            ),
            Err(ConfigError::InvalidNativeIdentityEnabled)
        ));
    }
}
