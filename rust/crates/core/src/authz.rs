use axum::extract::FromRequestParts;
use http::{HeaderMap, header, request::Parts};
use serde::Serialize;
use sqlx::{FromRow, PgConnection, PgPool};
use time::OffsetDateTime;

use crate::{AppError, OrganizationId, ProjectId, UserId};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Owner,
    Admin,
    Member,
}

impl Role {
    #[must_use]
    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "owner" => Some(Self::Owner),
            "admin" => Some(Self::Admin),
            "member" => Some(Self::Member),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_db(&self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }

    #[must_use]
    pub fn permits(&self, required: &Self) -> bool {
        let rank = |role: &Self| match role {
            Self::Member => 1_u8,
            Self::Admin => 2,
            Self::Owner => 3,
        };
        rank(self) >= rank(required)
    }
}

/// Request-authenticated workspace identity from native admission or the
/// explicit legacy transition bridge. Reader cookies never construct an Actor;
/// tenant authorization always uses the authenticated customer `user_id`.
#[derive(Clone, Debug)]
pub struct Actor {
    pub user_id: UserId,
    pub session_id: String,
    /// Set only for an authenticated support-impersonation session. Feature
    /// authorization still uses `user_id` (the customer); operator-only routes
    /// must reject this context explicitly.
    pub impersonated_by: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ProjectScope {
    pub project_id: ProjectId,
    pub organization_id: OrganizationId,
    pub role: Role,
}

#[derive(FromRow)]
struct SessionRow {
    user_id: String,
    session_id: String,
    impersonated_by: Option<String>,
}

fn credential_from(parts: &Parts) -> Result<String, AppError> {
    credential_from_headers(&parts.headers)
}

/// Resolves exactly one migration credential. An explicit `Authorization`
/// header takes precedence over cookies, and a malformed or ambiguous header
/// must not silently fall back to a cookie. Multiple Cookie header fields are
/// valid in modern HTTP, but multiple migration-session values are not.
fn credential_from_headers(headers: &HeaderMap) -> Result<String, AppError> {
    let mut authorization = headers.get_all(header::AUTHORIZATION).iter();
    if let Some(value) = authorization.next() {
        if authorization.next().is_some() {
            return Err(AppError::Unauthenticated);
        }
        let value = value.to_str().map_err(|_| AppError::Unauthenticated)?;
        if let Some(token) = value.strip_prefix("Bearer ")
            && !token.trim().is_empty()
        {
            return Ok(token.trim().to_owned());
        }
        return Err(AppError::Unauthenticated);
    }

    // Better Auth's default cookie name varies by deployment. This explicit
    // migration cookie lets the existing dashboard proxy exchange an opaque
    // session token without accepting arbitrary cookie names.
    let mut session = None;
    for header_value in headers.get_all(header::COOKIE) {
        let cookie = header_value
            .to_str()
            .map_err(|_| AppError::Unauthenticated)?;
        for pair in cookie.split(';') {
            let (name, value) = pair.trim().split_once('=').unwrap_or_default();
            if name == "nibleaf_session"
                && !value.is_empty()
                && session.replace(value.to_owned()).is_some()
            {
                return Err(AppError::Unauthenticated);
            }
        }
    }
    session.ok_or(AppError::Unauthenticated)
}

/// Optional transition identity for read-only public composition. Reader
/// cookies are ignored; an explicitly supplied invalid/ambiguous migration
/// credential fails closed instead of becoming anonymous.
///
/// # Errors
/// Returns unavailable/unauthenticated/database errors without credential text.
pub async fn optional_transition_actor_on_connection(
    connection: &mut PgConnection,
    headers: &HeaderMap,
) -> Result<Option<Actor>, AppError> {
    let mut cookie_count = 0;
    for value in headers.get_all(header::COOKIE) {
        let value = value.to_str().map_err(|_| AppError::Unauthenticated)?;
        cookie_count += value
            .split(';')
            .filter(|pair| pair.trim().split('=').next() == Some("nibleaf_session"))
            .count();
    }
    let cookie_present = cookie_count != 0;
    if cookie_count > 1 {
        return Err(AppError::Unauthenticated);
    }
    let has_header = headers.contains_key(header::AUTHORIZATION);
    if !has_header && !cookie_present {
        return Ok(None);
    }
    if has_header && cookie_present {
        return Err(AppError::Unauthenticated);
    }
    let token = credential_from_headers(headers)?;
    if token.len() > 512 {
        return Err(AppError::Unauthenticated);
    }
    let row = sqlx::query_as::<_, SessionRow>(
        r#"SELECT s."userId" AS user_id, s.id AS session_id, s."impersonatedBy" AS impersonated_by
           FROM session AS s INNER JOIN "user" AS u ON u.id=s."userId"
           LEFT JOIN "user" AS operator ON operator.id=s."impersonatedBy"
           WHERE s.token=$1 AND s."expiresAt">NOW() AND u."suspendedAt" IS NULL AND u.banned=FALSE
             AND (s."impersonatedBy" IS NULL OR (operator.role='admin' AND operator."suspendedAt" IS NULL AND operator.banned=FALSE))"#,
    ).bind(token).fetch_optional(connection).await?.ok_or(AppError::Unauthenticated)?;
    Ok(Some(Actor {
        user_id: UserId::try_from(row.user_id)?,
        session_id: row.session_id,
        impersonated_by: row.impersonated_by,
    }))
}

impl FromRequestParts<crate::AppState> for Actor {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &crate::AppState,
    ) -> Result<Self, Self::Rejection> {
        // API identity admission inserts an already-authenticated native actor
        // after its cookie/origin/CSRF checks. Keeping this extension in core
        // lets existing feature crates remain transport-agnostic and avoids a
        // core → identity dependency cycle.
        if let Some(actor) = parts.extensions.get::<Self>() {
            return Ok(actor.clone());
        }
        if state.config.authentication_authority == crate::AuthenticationAuthority::Native {
            return Err(AppError::Unauthenticated);
        }
        let token = credential_from(parts)?;
        // Current Nibleaf sessions are opaque tokens stored in its PostgreSQL
        // session table. Expiry and platform suspension are evaluated on every
        // request, so revocation is not delegated to client claims.
        let row = sqlx::query_as::<_, SessionRow>(
            r#"
            SELECT s."userId" AS user_id, s.id AS session_id,
                   s."impersonatedBy" AS impersonated_by
            FROM "session" AS s
            INNER JOIN "user" AS u ON u.id = s."userId"
            WHERE s.token = $1
              AND s."expiresAt" > NOW()
              AND u."suspendedAt" IS NULL
              AND u.banned = FALSE
            "#,
        )
        .bind(token)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::from)?
        .ok_or(AppError::Unauthenticated)?;

        Ok(Self {
            user_id: UserId::try_from(row.user_id)?,
            session_id: row.session_id,
            impersonated_by: row.impersonated_by,
        })
    }
}

#[derive(FromRow)]
struct ProjectScopeRow {
    organization_id: String,
    role: String,
}

/// Resolves membership through the project's *own* organization. This is the
/// mandatory tenant fence for all tenant-owned mutations in the first Rust
/// slices. It returns `NotFound` for missing or unauthorized projects to avoid
/// an object-existence oracle.
///
/// # Errors
///
/// Returns an authorization, not-found, identifier, or database error. Missing
/// and unauthorized projects deliberately share the same `NotFound` response.
pub async fn require_project_role(
    pool: &PgPool,
    actor: &Actor,
    project_id: ProjectId,
    required: Role,
) -> Result<ProjectScope, AppError> {
    let mut connection = pool.acquire().await?;
    require_project_role_on_connection(&mut connection, actor, project_id, required).await
}

/// The same tenant fence on a caller-owned connection, allowing a feature to
/// bind authorization and selection to one transaction/read snapshot.
///
/// # Errors
/// Identical authorization/error semantics to [`require_project_role`].
pub async fn require_project_role_on_connection(
    connection: &mut PgConnection,
    actor: &Actor,
    project_id: ProjectId,
    required: Role,
) -> Result<ProjectScope, AppError> {
    let row = sqlx::query_as::<_, ProjectScopeRow>(
        r#"
        SELECT p."organizationId" AS organization_id, m.role
        FROM project AS p
        INNER JOIN member AS m
            ON m."organizationId" = p."organizationId"
           AND m."userId" = $2
        WHERE p.id = $1
        "#,
    )
    .bind(project_id.as_str())
    .bind(actor.user_id.as_str())
    .fetch_optional(connection)
    .await?
    .ok_or_else(|| AppError::not_found("project"))?;

    let role = Role::from_db(&row.role).ok_or(AppError::Forbidden)?;
    if !role.permits(&required) {
        return Err(AppError::not_found("project"));
    }

    Ok(ProjectScope {
        project_id,
        organization_id: OrganizationId::try_from(row.organization_id)?,
        role,
    })
}

/// Kept here to make time injection easy in deterministic tests when a feature
/// needs a request receipt. The platform never trusts a client-provided clock.
#[must_use]
pub fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue, header};

    use super::{AppError, Role, credential_from_headers};

    #[test]
    fn role_hierarchy_is_monotonic() {
        assert!(Role::Owner.permits(&Role::Admin));
        assert!(Role::Admin.permits(&Role::Member));
        assert!(!Role::Member.permits(&Role::Admin));
    }

    #[test]
    fn a_single_bearer_credential_wins_over_cookie() -> Result<(), AppError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer bearer-session"),
        );
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("nibleaf_session=cookie-session"),
        );
        assert_eq!(credential_from_headers(&headers)?, "bearer-session");
        Ok(())
    }

    #[test]
    fn malformed_authorization_does_not_fall_back_to_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic ignored"),
        );
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("nibleaf_session=cookie-session"),
        );
        assert!(credential_from_headers(&headers).is_err());
    }

    #[test]
    fn duplicate_credentials_are_rejected() {
        let mut duplicate_authorization = HeaderMap::new();
        duplicate_authorization.append(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer first-session"),
        );
        duplicate_authorization.append(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer second-session"),
        );
        assert!(credential_from_headers(&duplicate_authorization).is_err());

        let mut duplicate_cookie = HeaderMap::new();
        duplicate_cookie.append(
            header::COOKIE,
            HeaderValue::from_static("nibleaf_session=first-session"),
        );
        duplicate_cookie.append(
            header::COOKIE,
            HeaderValue::from_static("nibleaf_session=second-session"),
        );
        assert!(credential_from_headers(&duplicate_cookie).is_err());
    }
}
