//! Host-only session-cookie construction and origin admission.

use thiserror::Error;
use time::Duration;
use url::Url;

/// The native Rust session cookie name is distinct from Better Auth's cookie so
/// migration traffic cannot accidentally authenticate in the wrong authority.
pub const RUST_IDENTITY_COOKIE: &str = "nibleaf_identity";

/// A path-scoped, `HttpOnly` operator-session escrow cookie used only to restore
/// the operator after a one-hour native support impersonation ends. It is never
/// accepted as ambient feature authentication.
pub const RUST_IDENTITY_RESUME_COOKIE: &str = "nibleaf_identity_resume";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionCookiePolicy {
    pub secure: bool,
    pub max_age: Duration,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum CookiePolicyError {
    #[error("session cookie lifetime must be positive")]
    InvalidLifetime,
    #[error("request origin is not trusted")]
    UntrustedOrigin,
}

/// Builds a host-only session cookie. Callers must use only tokens issued by
/// `issue_session`; this function does not accept arbitrary user input.
///
/// # Errors
///
/// Returns an error for an invalid lifetime.
pub fn session_set_cookie(
    opaque_token: &str,
    policy: SessionCookiePolicy,
) -> Result<String, CookiePolicyError> {
    let seconds = policy.max_age.whole_seconds();
    if seconds <= 0 {
        return Err(CookiePolicyError::InvalidLifetime);
    }
    let secure = if policy.secure { "; Secure" } else { "" };
    Ok(format!(
        "{RUST_IDENTITY_COOKIE}={opaque_token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={seconds}{secure}"
    ))
}

/// Clears the host-only native session cookie on sign-out/revocation.
#[must_use]
pub fn cleared_session_cookie(secure: bool) -> String {
    cleared_cookie(RUST_IDENTITY_COOKIE, "/", secure)
}

/// Builds a narrowly scoped operator-session escrow cookie for the support exit
/// endpoint. The operator token remains inaccessible to page JavaScript and is
/// not sent to any other API route while support access is active.
///
/// # Errors
///
/// Returns an error for an invalid lifetime.
pub fn resume_session_set_cookie(
    opaque_token: &str,
    policy: SessionCookiePolicy,
) -> Result<String, CookiePolicyError> {
    let seconds = policy.max_age.whole_seconds();
    if seconds <= 0 {
        return Err(CookiePolicyError::InvalidLifetime);
    }
    let secure = if policy.secure { "; Secure" } else { "" };
    Ok(format!(
        "{RUST_IDENTITY_RESUME_COOKIE}={opaque_token}; Path=/v1/identity/admin/impersonations/stop; HttpOnly; SameSite=Lax; Max-Age={seconds}{secure}"
    ))
}

/// Clears the path-scoped support operator-resume cookie.
#[must_use]
pub fn cleared_resume_session_cookie(secure: bool) -> String {
    cleared_cookie(
        RUST_IDENTITY_RESUME_COOKIE,
        "/v1/identity/admin/impersonations/stop",
        secure,
    )
}

fn cleared_cookie(name: &str, path: &str, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("{name}=; Path={path}; HttpOnly; SameSite=Lax; Max-Age=0{secure}")
}

/// Enforces exact normalized-origin membership for unsafe browser requests.
/// Originless requests are not browser CSRF candidates and are admitted here;
/// endpoint authentication policy still applies to them separately.
///
/// # Errors
///
/// Returns `UntrustedOrigin` for malformed or unlisted browser origins.
pub fn require_trusted_origin(
    origin: Option<&str>,
    trusted_origins: &[String],
) -> Result<(), CookiePolicyError> {
    let Some(origin) = origin else {
        return Ok(());
    };
    let origin = normalized_origin(origin).ok_or(CookiePolicyError::UntrustedOrigin)?;
    if trusted_origins
        .iter()
        .filter_map(|candidate| normalized_origin(candidate))
        .any(|candidate| candidate == origin)
    {
        Ok(())
    } else {
        Err(CookiePolicyError::UntrustedOrigin)
    }
}

fn normalized_origin(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    if url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || url.username() != ""
        || url.password().is_some()
    {
        return None;
    }
    match url.scheme() {
        "https" | "http" => Some(url.origin().ascii_serialization()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use super::{
        CookiePolicyError, RUST_IDENTITY_RESUME_COOKIE, SessionCookiePolicy,
        cleared_resume_session_cookie, cleared_session_cookie, require_trusted_origin,
        resume_session_set_cookie, session_set_cookie,
    };

    #[test]
    fn session_cookie_is_host_only_http_only_lax_and_explicitly_cleared()
    -> Result<(), CookiePolicyError> {
        let cookie = session_set_cookie(
            "opaque-token",
            SessionCookiePolicy {
                secure: true,
                max_age: Duration::days(7),
            },
        )?;
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Secure"));
        assert!(!cookie.contains("Domain="));
        assert!(cleared_session_cookie(true).contains("Max-Age=0"));
        Ok(())
    }

    #[test]
    fn resume_cookie_is_http_only_and_scoped_to_support_exit() -> Result<(), CookiePolicyError> {
        let cookie = resume_session_set_cookie(
            "operator-token",
            SessionCookiePolicy {
                secure: true,
                max_age: Duration::hours(1),
            },
        )?;
        assert!(cookie.starts_with(&format!("{RUST_IDENTITY_RESUME_COOKIE}=operator-token;")));
        assert!(cookie.contains("Path=/v1/identity/admin/impersonations/stop"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Secure"));
        assert!(!cookie.contains("Domain="));
        assert!(cleared_resume_session_cookie(true).contains("Max-Age=0"));
        Ok(())
    }

    #[test]
    fn unsafe_origins_require_exact_normalized_match() {
        let origins = vec!["https://app.example.test".to_owned()];
        assert!(require_trusted_origin(Some("https://app.example.test"), &origins).is_ok());
        assert!(matches!(
            require_trusted_origin(Some("https://evil.example.test"), &origins),
            Err(CookiePolicyError::UntrustedOrigin)
        ));
        assert!(matches!(
            require_trusted_origin(Some("https://app.example.test/path"), &origins),
            Err(CookiePolicyError::UntrustedOrigin)
        ));
    }
}
