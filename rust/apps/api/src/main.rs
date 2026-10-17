#![forbid(unsafe_code)]

mod optional_identity;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Extension, MatchedPath, State},
    http::{
        HeaderMap, HeaderName, Request, StatusCode,
        header::{ALLOW, AUTHORIZATION, CONTENT_TYPE},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use nibleaf_core::{Actor, AppError, AppState, Config, PrometheusHandle};
use nibleaf_identity::{
    RUST_IDENTITY_RESUME_COOKIE, authenticate_session, native_session_cookie,
    require_trusted_origin, verify_csrf,
};
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;
use tower_http::{
    catch_panic::CatchPanicLayer,
    limit::RequestBodyLimitLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use tracing::{error, info};

use nibleaf_search::distribution::{
    ArtifactStore, DeliveryKind, ReplicaCache, ReplicaCacheConfig, SearchDistributionConfig,
};
#[derive(Clone)]
struct ArtifactHealth(Option<Arc<ArtifactStore>>);

#[tokio::main]
async fn main() {
    let metrics = match nibleaf_core::telemetry::init("nibleaf-api") {
        Ok(metrics) => metrics,
        Err(error) => {
            eprintln!("API telemetry initialization failed: {error}");
            std::process::exit(1);
        }
    };
    if let Err(error) = run(metrics).await {
        error!(error = %error, "API terminated");
        std::process::exit(1);
    }
}

async fn run(metrics: PrometheusHandle) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;
    let query_config = nibleaf_search::SearchQueryConfig::from_env()?;
    let distribution = SearchDistributionConfig::from_env()?;
    let replica_config = if distribution.delivery_kind() == DeliveryKind::Object {
        Some(ReplicaCacheConfig::from_env()?)
    } else {
        None
    };
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .min_connections(1)
        .acquire_timeout(config.database_connect_timeout)
        .connect(&config.database_url)
        .await?;
    verify_schema(&pool).await?;
    let address = config.bind_addr;
    let artifact_store = ArtifactStore::configured(&distribution)?.map(Arc::new);
    if let Some(store) = &artifact_store {
        store.probe().await?;
    }
    let (queries, query_driver) =
        if let (Some(store), Some(replica)) = (&artifact_store, replica_config) {
            let cache = ReplicaCache::open(pool.clone(), store.as_ref().clone(), replica).await?;
            nibleaf_search::SearchQueryService::new_with_cache(pool.clone(), cache, query_config)?
        } else {
            nibleaf_search::SearchQueryService::new(
                pool.clone(),
                config.search_index_root.clone(),
                query_config,
            )?
        };
    let state = AppState::new(pool.clone(), config, metrics);
    let delivery = match distribution.delivery_kind() {
        DeliveryKind::Local => nibleaf_publishing::PublicationSearchDelivery::Local,
        DeliveryKind::Object => nibleaf_publishing::PublicationSearchDelivery::Object,
    };
    let app = router(state, queries, delivery, artifact_store);
    let listener = tokio::net::TcpListener::bind(address).await?;
    info!(%address, "API ready");
    let shutdown = CancellationToken::new();
    let server_shutdown = shutdown.clone();
    let signal_shutdown = shutdown.clone();
    // Join, do not select-and-drop the query driver on a signal/server error.
    // It keeps capacity leased until every accepted blocking job really exits.
    let server = async {
        let result = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                tokio::select! {
                    () = wait_for_shutdown_signal() => {},
                    () = signal_shutdown.cancelled() => {},
                }
                signal_shutdown.cancel();
            })
            .await;
        server_shutdown.cancel();
        result
    };
    let (server_result, query_result) = tokio::join!(server, query_driver.run(shutdown));
    pool.close().await;
    server_result?;
    query_result?;
    info!("API HTTP and search work drained");
    Ok(())
}

fn router(
    state: AppState,
    queries: nibleaf_search::SearchQueryService,
    delivery: nibleaf_publishing::PublicationSearchDelivery,
    artifact_store: Option<Arc<ArtifactStore>>,
) -> Router {
    let request_id = HeaderName::from_static("x-request-id");
    let identity_state = state.clone();
    let v1 = nibleaf_projects::router()
        .merge(nibleaf_membership::router())
        .merge(nibleaf_content::router())
        .merge(nibleaf_publishing::router_with_delivery(delivery))
        .merge(nibleaf_search::router(queries.clone()));
    let v1 = if state.config.public_search_enabled {
        v1.merge(nibleaf_search::public_router(
            queries,
            std::sync::Arc::new(optional_identity::OptionalWorkspaceIdentity {
                native_enabled: state.config.native_identity_enabled,
                authority: state.config.authentication_authority,
            }),
        ))
    } else {
        v1
    };
    let v1 = if state.config.native_identity_enabled {
        v1.merge(nibleaf_identity::router())
    } else {
        v1
    };
    Router::new()
        .route("/health/live", get(liveness))
        .route("/health/ready", get(readiness))
        .route("/metrics", get(prometheus_metrics))
        .nest("/v1", v1)
        .with_state(state)
        .layer(Extension(ArtifactHealth(artifact_store)))
        // The existing dashboard should use a same-origin proxy. No permissive
        // CORS rule is installed by default, avoiding ambient credential use.
        .layer(RequestBodyLimitLayer::new(2 * 1024 * 1024))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(30),
        ))
        .layer(CatchPanicLayer::new())
        .layer(PropagateRequestIdLayer::new(request_id.clone()))
        .layer(SetRequestIdLayer::new(request_id, MakeRequestUuid))
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &Request<Body>| {
                let (method, route) = request_observation(request);
                // DefaultMakeSpan includes the raw URI even with headers off.
                // Never put query strings, credentials or arbitrary paths here.
                tracing::info_span!("http_request", method, route)
            }),
        )
        .layer(middleware::from_fn(record_http_metrics))
        .layer(middleware::from_fn_with_state(
            identity_state,
            admit_native_identity,
        ))
        // Normalize framework-generated rejections (routing, body limit,
        // timeout, and panic responses) into the same RFC 9457 envelope as
        // feature errors. This is outermost so it also sees layer responses.
        .layer(middleware::from_fn(normalize_framework_errors))
        .layer(middleware::from_fn(public_search_response_policy))
}

async fn liveness() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({"status":"live"})))
}

async fn readiness(
    State(state): State<AppState>,
    Extension(artifact): Extension<ArtifactHealth>,
) -> impl IntoResponse {
    let ready = tokio::time::timeout(
        Duration::from_secs(1),
        sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&state.pool),
    )
    .await;
    let storage_ready = if let Some(store) = artifact.0 {
        matches!(
            tokio::time::timeout(Duration::from_secs(1), store.probe()).await,
            Ok(Ok(()))
        )
    } else {
        true
    };
    match ready {
        Ok(Ok(_)) if storage_ready => (StatusCode::OK, Json(serde_json::json!({"status":"ready"}))),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"status":"not_ready"})),
        ),
    }
}

async fn prometheus_metrics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if !state.config.authorizes_metrics(headers.get(AUTHORIZATION)) {
        return Err(AppError::Unauthenticated);
    }
    nibleaf_core::telemetry::record_database_pool_metrics(&state.pool);
    let handle = state.metrics.clone();
    let payload = tokio::task::spawn_blocking(move || {
        handle.run_upkeep();
        handle.render()
    })
    .await
    .map_err(|_| AppError::Unavailable)?;
    Ok((
        [(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        payload,
    )
        .into_response())
}

/// Authenticates a native cookie once for all feature routes and injects the
/// core `Actor` consumed by their existing extractors. Native cookie traffic
/// must not also present a legacy credential, and every unsafe method is
/// origin- and CSRF-fenced before the handler can mutate state.
async fn admit_native_identity(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Result<Response, AppError> {
    // Exact registered read-only transport only: its optional identity resolver
    // runs under the search lease/read-only snapshot. Other routes keep native
    // authentication and unsafe-method CSRF unchanged; no prefix auth bypass.
    if state.config.public_search_enabled
        && request
            .extensions()
            .get::<MatchedPath>()
            .is_some_and(|path| path.as_str() == nibleaf_search::PUBLIC_SEARCH_MATCHED_PATH)
    {
        return Ok(next.run(request).await);
    }
    if !state.config.native_identity_enabled {
        return Ok(next.run(request).await);
    }
    // Ending support access uses a path-scoped operator-resume cookie. Admit the
    // exact route to its own strict-origin/session handler even when the current
    // impersonated cookie has already expired or been emergency-revoked; otherwise
    // revocation could strand the operator in the customer browser context.
    if request.uri().path() == "/v1/identity/admin/impersonations/stop"
        && cookie_contains_name(request.headers(), RUST_IDENTITY_RESUME_COOKIE)?
    {
        return Ok(next.run(request).await);
    }
    let Some(token) = native_session_cookie(request.headers())? else {
        return Ok(next.run(request).await);
    };
    if request.headers().contains_key(AUTHORIZATION)
        || cookie_contains_name(request.headers(), "nibleaf_session")?
    {
        return Err(AppError::Unauthenticated);
    }
    let authenticated = authenticate_session(&state.pool, &token)
        .await
        .map_err(|_| AppError::Unauthenticated)?;
    let method = request.method();
    let safe_method = method == axum::http::Method::GET
        || method == axum::http::Method::HEAD
        || method == axum::http::Method::OPTIONS;
    // A fresh browser tab has the HttpOnly session cookie but may not have the
    // one-time in-memory CSRF secret returned at login. The exact native CSRF
    // recovery route rotates and returns it after doing its own explicit exact
    // trusted-Origin check; every other unsafe request remains API-wide CSRF
    // fenced here.
    let csrf_recovery = request.uri().path() == "/v1/identity/csrf";
    if !safe_method {
        require_trusted_origin(
            request
                .headers()
                .get(axum::http::header::ORIGIN)
                .and_then(|value| value.to_str().ok()),
            &state.config.trusted_origins,
        )
        .map_err(|_| AppError::Forbidden)?;
        if !csrf_recovery {
            let csrf = request
                .headers()
                .get("x-csrf-token")
                .and_then(|value| value.to_str().ok())
                .ok_or(AppError::Unauthenticated)?;
            verify_csrf(&authenticated, csrf).map_err(|_| AppError::Unauthenticated)?;
        }
    }
    request.extensions_mut().insert(Actor {
        user_id: authenticated.user_id,
        session_id: authenticated.session_id.to_string(),
        impersonated_by: authenticated.impersonated_by,
    });
    Ok(next.run(request).await)
}

fn cookie_contains_name(headers: &HeaderMap, name: &str) -> Result<bool, AppError> {
    for header_value in headers.get_all(axum::http::header::COOKIE) {
        let cookie = header_value
            .to_str()
            .map_err(|_| AppError::Unauthenticated)?;
        if cookie
            .split(';')
            .filter_map(|pair| pair.trim().split('=').next())
            .any(|key| key == name)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn public_search_response_policy(request: Request<Body>, next: Next) -> Response {
    let public = request.uri().path().starts_with("/v1/public/");
    let mut response = next.run(request).await;
    if public {
        if response.status() == StatusCode::BAD_REQUEST {
            let request_id = response.headers().get("x-request-id").cloned();
            response = AppError::validation("public request is malformed").into_response();
            if let Some(request_id) = request_id {
                response.headers_mut().insert("x-request-id", request_id);
            }
        }
        nibleaf_search::apply_private_search_policy(&mut response);
    }
    response
}

async fn normalize_framework_errors(request: Request<Body>, next: Next) -> Response {
    let response = next.run(request).await;
    let is_problem = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/problem+json"));
    if is_problem {
        return response;
    }

    let error = match response.status() {
        StatusCode::NOT_FOUND => AppError::not_found("route"),
        StatusCode::METHOD_NOT_ALLOWED => AppError::MethodNotAllowed,
        StatusCode::PAYLOAD_TOO_LARGE => AppError::PayloadTooLarge,
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => AppError::RequestTimeout,
        StatusCode::INTERNAL_SERVER_ERROR => AppError::Internal,
        _ => return response,
    };
    // Routing emits `Allow` for a 405 and request-ID middleware has already
    // attached its correlation header. Preserve both when replacing the body.
    let allow = response.headers().get(ALLOW).cloned();
    let request_id = response.headers().get("x-request-id").cloned();
    let mut normalized = error.into_response();
    if let Some(value) = allow {
        normalized.headers_mut().insert(ALLOW, value);
    }
    if let Some(value) = request_id {
        normalized
            .headers_mut()
            .insert(HeaderName::from_static("x-request-id"), value);
    }
    normalized
}

/// Bounded metadata shared by traces and metrics. Neither query strings nor
/// raw fallback paths/headers nor caller-defined HTTP method names are logged.
fn request_observation(request: &Request<Body>) -> (&'static str, &str) {
    let method = match request.method().as_str() {
        "GET" => "GET",
        "HEAD" => "HEAD",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "OPTIONS" => "OPTIONS",
        "CONNECT" => "CONNECT",
        "TRACE" => "TRACE",
        _ => "OTHER",
    };
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str);
    (method, route)
}

async fn record_http_metrics(request: Request<Body>, next: Next) -> Response {
    let (method, route) = request_observation(&request);
    let route = route.to_owned();
    let started = Instant::now();
    let response = next.run(request).await;
    let status_class = format!("{}xx", response.status().as_u16() / 100);
    metrics::counter!(
        "nibleaf_http_requests_total",
        "method" => method,
        "route" => route.clone(),
        "status_class" => status_class.clone()
    )
    .increment(1);
    metrics::histogram!(
        "nibleaf_http_request_duration_seconds",
        "method" => method,
        "route" => route,
        "status_class" => status_class
    )
    .record(started.elapsed().as_secs_f64());
    response
}

async fn verify_schema(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    if !nibleaf_search::distribution::schema_ready(pool).await? {
        return Err(sqlx::Error::Protocol(
            "required Rust platform migrations have not been applied".to_owned(),
        ));
    }

    let outbox: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('public.outbox_event')::text")
            .fetch_one(pool)
            .await?;
    let membership_index: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('public.member_one_owner_per_organization')::text")
            .fetch_one(pool)
            .await?;
    let identity_challenge: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('public.rust_identity_otp_challenge')::text")
            .fetch_one(pool)
            .await?;
    let identity_delivery: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('public.rust_identity_otp_delivery')::text")
            .fetch_one(pool)
            .await?;
    let account_deletion_audit: Option<String> = sqlx::query_scalar(
        "SELECT to_regclass('public.rust_identity_account_deletion_audit')::text",
    )
    .fetch_one(pool)
    .await?;
    // Migration 202610150011 expands this constrained action set. Checking the
    // table alone would let an older schema start, only to fail a valid shared-
    // workspace deletion after browser admission. Fail at startup instead.
    let account_deletion_action_constraint: Option<String> = sqlx::query_scalar(
        r"SELECT pg_get_constraintdef(constraint_row.oid)
           FROM pg_constraint AS constraint_row
           INNER JOIN pg_class AS relation ON relation.oid = constraint_row.conrelid
           INNER JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
           WHERE namespace.nspname = 'public'
             AND relation.relname = 'rust_identity_account_deletion_audit'
             AND constraint_row.conname = 'rust_identity_account_deletion_audit_action'",
    )
    .fetch_optional(pool)
    .await?;
    let shared_departure_audit_ready = account_deletion_action_constraint
        .as_deref()
        .is_some_and(|definition| definition.contains("self_deleted_after_membership_departure"));
    let search_generation_schema_ready = search_generation_schema_ready(pool).await?;
    if outbox.is_none()
        || membership_index.is_none()
        || identity_challenge.is_none()
        || identity_delivery.is_none()
        || account_deletion_audit.is_none()
        || !shared_departure_audit_ready
        || !search_generation_schema_ready
    {
        return Err(sqlx::Error::Protocol(
            "required Rust platform migrations have not been applied".to_owned(),
        ));
    }
    Ok(())
}

/// Verifies the expanded registry constraint required when publication creates
/// native search work. A table-only check would admit the pre-empty-generation
/// schema and make navigation-only publications fail after API admission.
async fn search_generation_schema_ready(pool: &sqlx::PgPool) -> Result<bool, sqlx::Error> {
    let registry: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('public.rust_search_generation')::text")
            .fetch_one(pool)
            .await?;
    let active_pointer: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('public.rust_search_active_generation')::text")
            .fetch_one(pool)
            .await?;
    let document_count_constraint: Option<String> = sqlx::query_scalar(
        r"SELECT pg_get_constraintdef(constraint_row.oid)
           FROM pg_constraint AS constraint_row
           INNER JOIN pg_class AS relation ON relation.oid = constraint_row.conrelid
           INNER JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
           WHERE namespace.nspname = 'public'
             AND relation.relname = 'rust_search_generation'
             AND constraint_row.conname = 'rust_search_generation_document_count_check'",
    )
    .fetch_optional(pool)
    .await?;
    Ok(registry.is_some()
        && active_pointer.is_some()
        && document_count_constraint
            .as_deref()
            .is_some_and(|definition| definition.contains("document_count >= 0")))
}

async fn wait_for_shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            error!(error = ?error, "unable to listen for Ctrl-C");
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(error) => error!(error = ?error, "unable to listen for SIGTERM"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_observation_omits_private_uri_headers_and_unknown_method_text()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = Request::builder()
            .method("PRIVATE-METHOD-SENTINEL")
            .uri("/private-path-sentinel?q=private-query-sentinel")
            .header("cookie", "secret=private-cookie-sentinel")
            .header("authorization", "Bearer private-token-sentinel")
            .body(Body::empty())?;
        let fields = request_observation(&request);
        assert_eq!(fields, ("OTHER", "unmatched"));
        assert!(!format!("{fields:?}").contains("private"));
        Ok(())
    }
}
