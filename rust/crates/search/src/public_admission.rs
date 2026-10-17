//! Read-only admission for a future native published-search route.
//!
//! This module intentionally does not expose an HTTP route or open a Tantivy
//! generation. It ports the security-critical, server-derived reader scope from
//! the legacy public-site boundary first, so a later route cannot substitute a
//! project member check for `PUBLIC`/`WORKSPACE`/`READERS` admission.

use std::collections::BTreeSet;

use axum::http::{HeaderMap, header};
use nibleaf_core::{Actor, AppError, ProjectId};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgConnection, PgPool};

/// Provenance of an admitted published-site viewer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishedViewerKind {
    /// The project's live access mode admits anonymous visitors.
    Public,
    /// A current project-organization member overrides reader restrictions.
    Workspace,
    /// A live, project-scoped reader session supplied the authorization scope.
    Reader,
}

/// Server-derived page scope for a future public search query.
///
/// `None` means every page in the selected published deployment; `Some`, even
/// when empty, is an exact allow-list. An empty allow-list is an admitted reader
/// with no current audience grants and must produce no results.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedSearchAdmission {
    project_id: String,
    kind: PublishedViewerKind,
    allowed_page_ids: Option<BTreeSet<String>>,
}

impl PublishedSearchAdmission {
    /// The exact project whose live evidence produced this admission. A query
    /// must reject using this scope with any other project's generation.
    #[must_use]
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    #[must_use]
    pub const fn kind(&self) -> PublishedViewerKind {
        self.kind
    }

    #[must_use]
    pub const fn allowed_page_ids(&self) -> Option<&BTreeSet<String>> {
        self.allowed_page_ids.as_ref()
    }

    fn full(project_id: &str, kind: PublishedViewerKind) -> Self {
        Self {
            project_id: project_id.to_owned(),
            kind,
            allowed_page_ids: None,
        }
    }

    fn pages(project_id: &str, page_ids: BTreeSet<String>) -> Self {
        Self {
            project_id: project_id.to_owned(),
            kind: PublishedViewerKind::Reader,
            allowed_page_ids: Some(page_ids),
        }
    }
}

#[derive(FromRow)]
struct ProjectAccessRow {
    access_mode: String,
    is_taken_down: bool,
    organization_id: String,
}

#[derive(FromRow)]
struct ReaderGrantRow {
    all_pages: bool,
    page_ids: Vec<String>,
}

/// Resolves a current published-site access decision without performing any
/// search, analytics, session touch, or filesystem operation.
///
/// The caller supplies a request-authenticated workspace actor when one exists
/// (native API identity middleware or the bounded legacy session bridge). It
/// must never construct an actor from a reader cookie. `None` is an ordinary
/// public request. A `None` return deliberately combines missing, taken-down,
/// and denied sites; a future HTTP route maps it to a concealed `404`.
///
/// `READERS` gives a project-organization member the same full-site emergency
/// access as legacy delivery, before trying a reader cookie. A valid reader
/// token is hashed and compared only inside its requested project; joins bind
/// every audience/grant row back to the same project before deriving a scope.
///
/// # Errors
/// Returns [`AppError::Database`] when PostgreSQL cannot safely read the live
/// project, member, reader-session, or audience-grant evidence. Authorization
/// denial, a taken-down project, and malformed reader-cookie evidence return
/// `Ok(None)` rather than an error so the future HTTP route can conceal them.
pub async fn admit_published_search(
    pool: &PgPool,
    project_id: &ProjectId,
    headers: &HeaderMap,
    workspace_actor: Option<&Actor>,
) -> Result<Option<PublishedSearchAdmission>, AppError> {
    let mut connection = pool.acquire().await?;
    admit_on_connection(&mut connection, project_id, headers, workspace_actor).await
}

// A concrete connection lets the publication composition share one short,
// read-only database snapshot for admission, READY selection and live bounds.
pub(crate) async fn admit_on_connection(
    connection: &mut PgConnection,
    project_id: &ProjectId,
    headers: &HeaderMap,
    workspace_actor: Option<&Actor>,
) -> Result<Option<PublishedSearchAdmission>, AppError> {
    admit_on_connection_with_limit(connection, project_id, headers, workspace_actor, i64::MAX).await
}

pub(crate) async fn admit_on_connection_with_limit(
    connection: &mut PgConnection,
    project_id: &ProjectId,
    headers: &HeaderMap,
    workspace_actor: Option<&Actor>,
    max_pages: i64,
) -> Result<Option<PublishedSearchAdmission>, AppError> {
    let project = sqlx::query_as::<_, ProjectAccessRow>(
        r#"
        SELECT p."accessMode"::text AS access_mode,
               p."takedownAt" IS NOT NULL AS is_taken_down,
               p."organizationId" AS organization_id
        FROM project AS p
        WHERE p.id = $1
        "#,
    )
    .bind(project_id.as_str())
    .fetch_optional(&mut *connection)
    .await?;
    let Some(project) = project else {
        return Ok(None);
    };
    if project.is_taken_down {
        return Ok(None);
    }

    match project.access_mode.as_str() {
        "PUBLIC" => Ok(Some(PublishedSearchAdmission::full(
            project_id.as_str(),
            PublishedViewerKind::Public,
        ))),
        "WORKSPACE" => {
            if workspace_member(connection, &project.organization_id, workspace_actor).await? {
                Ok(Some(PublishedSearchAdmission::full(
                    project_id.as_str(),
                    PublishedViewerKind::Workspace,
                )))
            } else {
                Ok(None)
            }
        }
        "READERS" => {
            if workspace_member(connection, &project.organization_id, workspace_actor).await? {
                return Ok(Some(PublishedSearchAdmission::full(
                    project_id.as_str(),
                    PublishedViewerKind::Workspace,
                )));
            }
            reader_grants(connection, project_id.as_str(), headers, max_pages).await
        }
        // The database enum is expected to make this unreachable, but treating
        // unknown data as denied preserves the public non-disclosure boundary.
        _ => Ok(None),
    }
}

async fn workspace_member(
    connection: &mut PgConnection,
    organization_id: &str,
    workspace_actor: Option<&Actor>,
) -> Result<bool, AppError> {
    let Some(actor) = workspace_actor else {
        return Ok(false);
    };
    let member: Option<bool> = sqlx::query_scalar(
        r#"
        SELECT TRUE
        FROM member
        WHERE "organizationId" = $1
          AND "userId" = $2
        "#,
    )
    .bind(organization_id)
    .bind(actor.user_id.as_str())
    .fetch_optional(&mut *connection)
    .await?;
    Ok(member.unwrap_or(false))
}

async fn reader_grants(
    connection: &mut PgConnection,
    project_id: &str,
    headers: &HeaderMap,
    max_pages: i64,
) -> Result<Option<PublishedSearchAdmission>, AppError> {
    let Some(token) = reader_cookie(headers, project_id) else {
        return Ok(None);
    };
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let scope = sqlx::query_as::<_, ReaderGrantRow>(
        r#"
        WITH candidate AS MATERIALIZED (
          SELECT session.id, reader.id AS reader_id, session."projectId" AS project_id
          FROM reader_session AS session
          INNER JOIN reader ON reader.id = session."readerId"
            AND reader."projectId" = session."projectId"
          WHERE session."projectId" = $1 AND session."tokenHash" = $2
            AND session."revokedAt" IS NULL AND session."expiresAt" > NOW()
            AND reader."revokedAt" IS NULL AND reader.status = 'ACTIVE'
        ), scoped_grants AS MATERIALIZED (
          SELECT page_grant.id, page_grant."pageId" AS page_id
          FROM candidate
          INNER JOIN reader_audience AS membership ON membership."readerId" = candidate.reader_id
          INNER JOIN audience ON audience.id = membership."audienceId"
            AND audience."projectId" = candidate.project_id
          INNER JOIN audience_grant AS page_grant ON page_grant."audienceId" = audience.id
            AND page_grant."projectId" = candidate.project_id
        ), site AS (
          SELECT EXISTS(SELECT 1 FROM scoped_grants WHERE id IS NOT NULL AND page_id IS NULL) AS all_pages
        )
        SELECT site.all_pages,
          CASE WHEN site.all_pages THEN ARRAY[]::TEXT[]
          ELSE ARRAY(SELECT DISTINCT page_id FROM scoped_grants
                     WHERE page_id IS NOT NULL ORDER BY page_id LIMIT $3) END AS page_ids
        FROM candidate CROSS JOIN site
        "#,
    )
    .bind(project_id)
    .bind(token_hash)
    .bind(max_pages.saturating_add(1))
    .fetch_optional(&mut *connection)
    .await?;
    let Some(scope) = scope else {
        return Ok(None);
    };
    if scope.all_pages {
        return Ok(Some(PublishedSearchAdmission::full(
            project_id,
            PublishedViewerKind::Reader,
        )));
    }
    if i64::try_from(scope.page_ids.len()).unwrap_or(i64::MAX) > max_pages {
        return Err(AppError::Unavailable);
    }
    Ok(Some(PublishedSearchAdmission::pages(
        project_id,
        scope.page_ids.into_iter().collect(),
    )))
}

/// Matches the legacy per-project reader-cookie name: project identifiers are
/// reduced to ASCII alphanumeric, `_`, and `-` before becoming a cookie name.
fn reader_cookie_name(project_id: &str) -> String {
    let suffix: String = project_id
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .collect();
    format!("nibleaf_reader_{suffix}")
}

/// Reads one percent-decoded reader token. Duplicate values, malformed HTTP
/// cookies, malformed percent escapes, and invalid UTF-8 deny admission rather
/// than selecting an arbitrary credential.
fn reader_cookie(headers: &HeaderMap, project_id: &str) -> Option<String> {
    let expected = reader_cookie_name(project_id);
    let mut token = None;
    for value in headers.get_all(header::COOKIE) {
        let raw = value.to_str().ok()?;
        for pair in raw.split(';') {
            let Some((name, encoded)) = pair.trim().split_once('=') else {
                continue;
            };
            if name != expected || encoded.is_empty() {
                continue;
            }
            let decoded = decode_uri_component(encoded)?;
            if decoded.is_empty() || token.replace(decoded).is_some() {
                return None;
            }
        }
    }
    token
}

/// Minimal `decodeURIComponent`-compatible decoding for legacy cookie values.
/// Reader tokens are generated as base64url, but preserving percent decoding
/// accepts legacy browser serialization without translating `+` into a space.
fn decode_uri_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let high = *bytes.get(index + 1)?;
        let low = *bytes.get(index + 2)?;
        let digit = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        };
        decoded.push((digit(high)? << 4) | digit(low)?);
        index += 3;
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header};

    use super::{decode_uri_component, reader_cookie, reader_cookie_name};

    #[test]
    fn project_cookie_name_matches_legacy_ascii_reduction() {
        assert_eq!(
            reader_cookie_name("project_ABC-12!"),
            "nibleaf_reader_project_ABC-12"
        );
    }

    #[test]
    fn reader_cookie_decodes_percent_encoding_but_rejects_ambiguity() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("other=skip; nibleaf_reader_project=token%2Dvalue"),
        );
        assert_eq!(
            reader_cookie(&headers, "project"),
            Some("token-value".to_owned())
        );

        headers.append(
            header::COOKIE,
            HeaderValue::from_static("nibleaf_reader_project=other"),
        );
        assert_eq!(reader_cookie(&headers, "project"), None);
        assert_eq!(decode_uri_component("a+b%2Fc"), Some("a+b/c".to_owned()));
        assert_eq!(decode_uri_component("bad%XZ"), None);
    }
}
