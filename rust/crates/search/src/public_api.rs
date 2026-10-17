//! Explicit opt-in published reader transport. Management is a different route.
use crate::{
    PublishedActorResolver, PublishedSearchError, PublishedSearchRequest, SearchHit,
    SearchQueryExecutionError, SearchQueryService,
};
use axum::{
    Router,
    extract::{Extension, Path, RawQuery},
    http::{HeaderMap, Uri},
    response::{IntoResponse, Response},
    routing::get,
};
use nibleaf_core::{AppError, AppState, ProjectId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const PUBLIC_SEARCH_MATCHED_PATH: &str = "/v1/public/sites/{project_id}/search";

#[derive(Clone)]
struct PublicTransport {
    service: SearchQueryService,
    resolver: Arc<dyn PublishedActorResolver>,
}

/// The application must explicitly opt in and compose the identity port. No
/// legacy fallback or caller-controlled generation/page allow-list exists.
pub fn public_router(
    service: SearchQueryService,
    resolver: Arc<dyn PublishedActorResolver>,
) -> Router<AppState> {
    Router::new()
        .route("/public/sites/{project_id}/search", get(search))
        .layer(Extension(PublicTransport { service, resolver }))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransportQuery {
    #[serde(default)]
    q: String,
    lang: Option<String>,
    version: Option<String>,
    limit: Option<String>,
}

pub(crate) fn validate_transport(
    raw: Option<&str>,
) -> Result<crate::published::ValidatedRequest, PublishedSearchError> {
    let invalid =
        || PublishedSearchError::InvalidRequest("search parameters are invalid or ambiguous");
    let uri: Uri = format!("/?{}", raw.unwrap_or_default())
        .parse()
        .map_err(|_| invalid())?;
    let query = axum::extract::Query::<TransportQuery>::try_from_uri(&uri)
        .map_err(|_| invalid())?
        .0;
    let limit = query
        .limit
        .map(|value| {
            // Preserve legacy numeric integer coercion (7, 7.0 and 7e0), but never
            // accept nonfinite/fractional/out-of-range or duplicate parameters.
            let number: f64 = value.trim().parse().map_err(|_| invalid())?;
            if !number.is_finite() || number.fract() != 0.0 || !(1.0..=50.0).contains(&number) {
                return Err(invalid());
            }
            let normalized = format!("{number:.0}");
            normalized.parse::<usize>().map_err(|_| invalid())
        })
        .transpose()?;
    crate::published::validate_request(PublishedSearchRequest {
        query: &query.q,
        language: query.lang.as_deref(),
        version: query.version.as_deref(),
        limit,
    })
}

#[derive(Serialize)]
struct PublicHit {
    id: String,
    title: String,
    path: String,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    snippet: String,
    score: f32,
    language: String,
}
impl From<SearchHit> for PublicHit {
    fn from(hit: SearchHit) -> Self {
        Self {
            id: hit.page_id,
            title: hit.title,
            path: hit.path,
            description: hit.description,
            icon: hit.icon,
            snippet: hit.snippet,
            score: hit.score,
            language: hit.language,
        }
    }
}
#[derive(Serialize)]
struct Capabilities {
    answer: bool,
}
#[derive(Serialize)]
struct PublicData {
    hits: Vec<PublicHit>,
    runtime: &'static str,
    capabilities: Capabilities,
}
#[derive(Serialize)]
struct PublicEnvelope {
    data: PublicData,
}

async fn search(
    Extension(transport): Extension<PublicTransport>,
    Path(project_id): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let project_id = ProjectId::try_from(project_id).map_err(|_| AppError::not_found("site"))?;
    let result = transport
        .service
        .search_public_transport(&project_id, &headers, raw.as_deref(), transport.resolver)
        .await;
    let result = result
        .map_err(|error| transport_error(&error))?
        .ok_or_else(|| AppError::not_found("site"))?;
    let outcome = if result.zero { "zero" } else { "hits" };
    metrics::counter!("nibleaf_public_search_responses_total", "outcome" => outcome).increment(1);
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        result.bytes,
    )
        .into_response())
}

const MAX_PUBLIC_RESPONSE_BYTES: usize = 1024 * 1024;

pub(crate) struct PublicBody {
    bytes: Vec<u8>,
    zero: bool,
}

struct BoundedWriter<'a> {
    bytes: Vec<u8>,
    budget: Option<&'a crate::execution::budget::QueryBudget>,
}
impl std::io::Write for BoundedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        crate::execution::budget::checkpoint(self.budget)
            .map_err(|_| std::io::Error::other("search interrupted"))?;
        if bytes.len() > MAX_PUBLIC_RESPONSE_BYTES.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("public response byte limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// Executed by the owned blocking job while its lease remains held. The HTTP
// worker never synchronously serializes a potentially large search result.
pub(crate) fn encode_hits(
    hits: Vec<SearchHit>,
    budget: Option<&crate::execution::budget::QueryBudget>,
) -> Result<PublicBody, crate::SearchError> {
    if hits.iter().any(|hit| !hit.score.is_finite()) {
        return Err(crate::SearchError::Integrity("non-finite public score"));
    }
    let zero = hits.is_empty();
    let envelope = PublicEnvelope {
        data: PublicData {
            hits: hits.into_iter().map(Into::into).collect(),
            runtime: "tantivy",
            capabilities: Capabilities { answer: false },
        },
    };
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        budget,
    };
    let encoded = serde_json::to_writer(&mut writer, &envelope);
    crate::execution::budget::checkpoint(budget)?;
    encoded.map_err(|_| crate::SearchError::ResourceLimit("public response bytes"))?;
    Ok(PublicBody {
        bytes: writer.bytes,
        zero,
    })
}

fn transport_error(error: &SearchQueryExecutionError) -> AppError {
    match error {
        SearchQueryExecutionError::Published(PublishedSearchError::InvalidRequest(message)) => {
            AppError::validation(*message)
        }
        SearchQueryExecutionError::Published(PublishedSearchError::Admission(
            AppError::Unauthenticated,
        )) => AppError::Unauthenticated,
        _ => AppError::Unavailable,
    }
}

// Enforced by the application outside the framework response normalizer too:
// it must cover method/path/timeout/identity failures, not only successful JSON.
pub fn apply_private_policy(response: &mut Response) {
    use axum::http::{
        HeaderValue,
        header::{CACHE_CONTROL, VARY},
    };
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    response
        .headers_mut()
        .insert(VARY, HeaderValue::from_static("Cookie, Authorization"));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_json_is_bounded_and_does_not_truncate_or_expose_nonfinite_scores() {
        let make = |score| SearchHit {
            page_id: "page".to_owned(),
            language: "en".to_owned(),
            version_id: "main".to_owned(),
            version_slug: "main".to_owned(),
            path: "guide".to_owned(),
            title: "x".repeat(MAX_PUBLIC_RESPONSE_BYTES),
            description: String::new(),
            icon: None,
            snippet: String::new(),
            score,
        };
        assert!(matches!(
            encode_hits(vec![make(1.0)], None),
            Err(crate::SearchError::ResourceLimit(_))
        ));
        assert!(matches!(
            encode_hits(vec![make(f32::NAN)], None),
            Err(crate::SearchError::Integrity(_))
        ));
        let empty = encode_hits(Vec::new(), None);
        assert!(empty.is_ok());
    }

    #[test]
    fn input_is_strict_and_blank_defaults_are_allowed() {
        for value in [
            None,
            Some("q="),
            Some("q=++"),
            Some("q=word&limit=7.0"),
            Some("q=word&limit=7e0"),
        ] {
            assert!(validate_transport(value).is_ok());
        }
        for value in [
            "q=a&q=b",
            "q=a&unknown=b",
            "q=a&limit=NaN",
            "limit=0",
            "limit=2.1",
            "limit=",
            "lang=a&lang=b",
        ] {
            assert!(validate_transport(Some(value)).is_err());
        }
    }
    #[test]
    fn utf16_units_are_not_utf8_bytes_or_unicode_scalar_count() {
        for (text, valid) in [
            ("界".repeat(200), true),
            ("界".repeat(201), false),
            ("😀".repeat(100), true),
            ("😀".repeat(101), false),
        ] {
            let request = PublishedSearchRequest {
                query: &text,
                language: None,
                version: None,
                limit: None,
            };
            assert_eq!(crate::published::validate_request(request).is_ok(), valid);
        }
    }
}
