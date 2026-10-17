#![forbid(unsafe_code)]

mod supervision;

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    http::{
        HeaderMap, StatusCode,
        header::{AUTHORIZATION, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
    routing::get,
};
use nibleaf_core::{AppError, Config, PrometheusHandle};
use nibleaf_identity::{
    EncryptionKey, MAX_EXPIRY_RECONCILIATION_BATCH_SIZE, deliver_otp_email_event,
    expire_due_impersonations,
};
use nibleaf_outbox::{
    ClaimedEvent, Failure, backlog_snapshot, claim_batch, mark_delivered, mark_failed,
};
use nibleaf_search::registry::{
    ClaimOutcome, FailureClass, RegistryError, build_or_reuse_claimed_generation,
    claim_generation_build, load_claimed_snapshot_documents, parse_generation_request,
    promote_claimed_generation, record_claim_failure, register_generation_build_for_delivery,
    renew_generation_lease,
};
use sqlx::postgres::PgPoolOptions;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use uuid::Uuid;

use nibleaf_search::distribution::{
    ArtifactPublisher, ArtifactStore, DeliveryKind, SearchDistributionConfig,
};

#[derive(Clone)]
struct WorkerContext {
    config: Config,
    identity_delivery_keys: Arc<Vec<EncryptionKey>>,
    email_client: reqwest::Client,
    delivery: DeliveryKind,
    publisher: Option<Arc<ArtifactPublisher>>,
}

#[derive(Clone, Debug)]
struct HealthState {
    ready: Arc<AtomicBool>,
    config: Config,
    pool: sqlx::PgPool,
    metrics: PrometheusHandle,
    artifact_store: Option<Arc<ArtifactStore>>,
}

#[tokio::main]
async fn main() {
    let metrics = match nibleaf_core::telemetry::init("nibleaf-worker") {
        Ok(metrics) => metrics,
        Err(error) => {
            eprintln!("worker telemetry initialization failed: {error}");
            std::process::exit(1);
        }
    };
    if let Err(error) = run(metrics).await {
        error!(error = %error, "worker terminated");
        std::process::exit(1);
    }
}

async fn run(metrics: PrometheusHandle) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;
    let distribution = SearchDistributionConfig::from_env()?;
    distribution.validate_worker(config.worker_concurrency)?;
    let identity_delivery_keys = Arc::new(identity_delivery_key_ring(&config)?);
    let email_client = reqwest::Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .min_connections(1)
        .acquire_timeout(config.database_connect_timeout)
        .connect(&config.database_url)
        .await?;
    verify_schema(&pool).await?;
    let artifact_store = ArtifactStore::configured(&distribution)?.map(Arc::new);
    if let Some(store) = &artifact_store {
        store.probe().await?;
    }
    let publisher = if let Some(store) = &artifact_store {
        let root = config
            .search_index_root
            .clone()
            .ok_or("NIBLEAF_SEARCH_INDEX_ROOT is required for object worker")?;
        Some(ArtifactPublisher::open(pool.clone(), root, Arc::clone(store)).await?)
    } else {
        None
    };
    let context = WorkerContext {
        config: config.clone(),
        identity_delivery_keys,
        email_client,
        delivery: distribution.delivery_kind(),
        publisher,
    };
    let cancellation = CancellationToken::new();
    let ready = Arc::new(AtomicBool::new(true));
    let worker_address = config.worker_bind_addr;
    let concurrency = config.worker_concurrency;
    let listener = tokio::net::TcpListener::bind(worker_address).await?;
    let health = HealthState {
        ready: Arc::clone(&ready),
        config: config.clone(),
        pool: pool.clone(),
        metrics,
        artifact_store,
    };
    let server_cancel = cancellation.clone();
    let worker_cancel = cancellation.clone();

    info!(
        %worker_address,
        concurrency,
        expiry_reconciliation_interval_seconds = config
            .support_expiry_reconciliation_interval
            .as_secs(),
        "worker ready"
    );
    let shutdown_ready = Arc::clone(&ready);
    let server = axum::serve(listener, health_router(health)).with_graceful_shutdown(async move {
        tokio::select! {
            () = wait_for_shutdown_signal() => {},
            () = server_cancel.cancelled() => {},
        }
        shutdown_ready.store(false, Ordering::Release);
        server_cancel.cancel();
    });
    let workers = Box::pin(run_worker_loops(
        pool.clone(),
        context,
        worker_cancel,
        concurrency,
    ));
    // Keep both futures alive after signal/error. In particular, dropping the
    // dispatch JoinSet would detach an accepted spawn_blocking index build.
    let (server_result, worker_result) =
        supervision::join_owned(async { server.await }, workers, &cancellation).await;
    ready.store(false, Ordering::Release);
    pool.close().await;
    server_result?;
    worker_result?;
    info!("worker HTTP and accepted work drained");
    Ok(())
}

fn identity_delivery_key_ring(
    config: &Config,
) -> Result<Vec<EncryptionKey>, nibleaf_identity::EncryptionError> {
    let mut keys = Vec::with_capacity(1 + config.identity_delivery_previous_keys.len());
    keys.push(EncryptionKey::new(
        config.identity_delivery_encryption_key_version,
        &config.identity_delivery_encryption_key,
    )?);
    for previous in &config.identity_delivery_previous_keys {
        keys.push(EncryptionKey::new(previous.version, &previous.material)?);
    }
    Ok(keys)
}

fn health_router(state: HealthState) -> Router {
    Router::new()
        .route(
            "/health/live",
            get(|| async { (StatusCode::OK, Json(serde_json::json!({"status":"live"}))) }),
        )
        .route("/health/ready", get(readiness))
        .route("/metrics", get(prometheus_metrics))
        .with_state(state)
}

async fn readiness(State(state): State<HealthState>) -> impl IntoResponse {
    let storage_ready = if let Some(store) = &state.artifact_store {
        matches!(
            tokio::time::timeout(Duration::from_secs(1), store.probe()).await,
            Ok(Ok(()))
        )
    } else {
        true
    };
    let status = if state.ready.load(Ordering::Acquire) && storage_ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(
            serde_json::json!({"status": if status == StatusCode::OK { "ready" } else { "not_ready" }}),
        ),
    )
}

async fn prometheus_metrics(
    State(state): State<HealthState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if !state.config.authorizes_metrics(headers.get(AUTHORIZATION)) {
        return Err(AppError::Unauthenticated);
    }
    nibleaf_core::telemetry::record_database_pool_metrics(&state.pool);
    record_outbox_backlog_metrics(&state.pool).await;
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

async fn record_outbox_backlog_metrics(pool: &sqlx::PgPool) {
    match backlog_snapshot(pool).await {
        Ok(snapshot) => {
            metrics::gauge!("nibleaf_outbox_snapshot_up").set(1.0);
            metrics::gauge!("nibleaf_outbox_events", "state" => "pending").set(snapshot.pending);
            metrics::gauge!("nibleaf_outbox_events", "state" => "processing")
                .set(snapshot.processing);
            metrics::gauge!("nibleaf_outbox_events", "state" => "dead_letter")
                .set(snapshot.dead_letter);
            metrics::gauge!("nibleaf_outbox_oldest_open_seconds")
                .set(snapshot.oldest_open_seconds.unwrap_or(0.0));
        }
        Err(error) => {
            metrics::gauge!("nibleaf_outbox_snapshot_up").set(0.0);
            metrics::counter!("nibleaf_outbox_snapshot_errors_total").increment(1);
            warn!(error = ?error, "unable to sample outbox backlog for metrics");
        }
    }
}

async fn run_worker_loops(
    pool: sqlx::PgPool,
    context: WorkerContext,
    cancellation: CancellationToken,
    concurrency: usize,
) -> Result<(), AppError> {
    let interval = context.config.support_expiry_reconciliation_interval;
    let dispatch = run_dispatch_loop(pool.clone(), context, cancellation.clone(), concurrency);
    let reconcile = run_support_expiry_reconciliation_loop(pool, interval, cancellation.clone());
    let (dispatch_result, reconcile_result) =
        supervision::join_owned(dispatch, reconcile, &cancellation).await;
    dispatch_result?;
    reconcile_result?;
    Ok(())
}

async fn run_support_expiry_reconciliation_loop(
    pool: sqlx::PgPool,
    interval: Duration,
    cancellation: CancellationToken,
) -> Result<(), AppError> {
    loop {
        if cancellation.is_cancelled() {
            return Ok(());
        }
        let started = std::time::Instant::now();
        match expire_due_impersonations(&pool, MAX_EXPIRY_RECONCILIATION_BATCH_SIZE).await {
            Ok(expired) => {
                metrics::counter!(
                    "nibleaf_identity_impersonation_expiry_reconciliations_total",
                    "outcome" => "success"
                )
                .increment(1);
                metrics::histogram!(
                    "nibleaf_identity_impersonation_expiry_reconciliation_duration_seconds",
                    "outcome" => "success"
                )
                .record(started.elapsed().as_secs_f64());
                if expired > 0 {
                    metrics::counter!("nibleaf_identity_impersonation_expired_total")
                        .increment(expired as u64);
                    info!(expired, "reconciled expired native support sessions");
                }
            }
            Err(error) => {
                metrics::counter!(
                    "nibleaf_identity_impersonation_expiry_reconciliations_total",
                    "outcome" => "error"
                )
                .increment(1);
                metrics::counter!(
                    "nibleaf_identity_impersonation_expiry_reconciliation_errors_total"
                )
                .increment(1);
                metrics::histogram!(
                    "nibleaf_identity_impersonation_expiry_reconciliation_duration_seconds",
                    "outcome" => "error"
                )
                .record(started.elapsed().as_secs_f64());
                warn!(error = ?error, "support expiry reconciliation failed; will retry");
            }
        }
        tokio::select! {
            () = cancellation.cancelled() => return Ok(()),
            () = tokio::time::sleep(interval) => {},
        }
    }
}

async fn run_dispatch_loop(
    pool: sqlx::PgPool,
    context: WorkerContext,
    cancellation: CancellationToken,
    concurrency: usize,
) -> Result<(), AppError> {
    let worker_id = Uuid::now_v7();
    let lease = Duration::from_secs(60);
    loop {
        if cancellation.is_cancelled() {
            return Ok(());
        }
        let claimed = claim_batch(
            &pool,
            worker_id,
            i64::try_from(concurrency)
                .map_err(|_| AppError::validation("worker concurrency is too large"))?,
            lease,
        )
        .await?;
        if claimed.is_empty() {
            tokio::select! {
                () = cancellation.cancelled() => return Ok(()),
                () = tokio::time::sleep(Duration::from_millis(500)) => {},
            }
            continue;
        }
        metrics::counter!("nibleaf_outbox_claimed_total").increment(claimed.len() as u64);
        let reclaimed = claimed.iter().filter(|event| event.reclaimed).count();
        if reclaimed > 0 {
            metrics::counter!("nibleaf_outbox_reclaimed_total").increment(reclaimed as u64);
        }
        let mut jobs = JoinSet::new();
        for event in claimed {
            let pool = pool.clone();
            let context = context.clone();
            let search_worker_id = worker_id.to_string();
            jobs.spawn(async move { deliver(&pool, &context, &search_worker_id, event).await });
        }
        while let Some(result) = jobs.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    metrics::counter!("nibleaf_outbox_delivery_persist_errors_total").increment(1);
                    warn!(error = ?error, "outbox delivery record could not be persisted; lease will recover");
                }
                Err(error) => {
                    metrics::counter!("nibleaf_outbox_delivery_persist_errors_total").increment(1);
                    error!(error = ?error, "outbox task panicked; lease will recover");
                }
            }
        }
    }
}

async fn deliver(
    pool: &sqlx::PgPool,
    context: &WorkerContext,
    search_worker_id: &str,
    event: ClaimedEvent,
) -> Result<(), AppError> {
    let config = &context.config;
    let identity_delivery_keys = context.identity_delivery_keys.as_slice();
    let email_client = &context.email_client;
    let started = std::time::Instant::now();
    let outcome = match event.event_type.as_str() {
        "project.initialized" => nibleaf_projects::complete_onboarding(pool, &event).await,
        "identity.account.provision_requested" => {
            nibleaf_projects::provision_identity_account(pool, &event).await
        }
        "publication.requested" => nibleaf_publishing::complete_publication(pool, &event).await,
        "search.generation.requested" => {
            deliver_search_generation_request(pool, context, search_worker_id, &event).await
        }
        "starter.publication.requested" => {
            nibleaf_publishing::request_starter_publication_for_delivery(
                pool,
                &event,
                match context.delivery {
                    DeliveryKind::Local => nibleaf_publishing::PublicationSearchDelivery::Local,
                    DeliveryKind::Object => nibleaf_publishing::PublicationSearchDelivery::Object,
                },
            )
            .await
        }
        "membership.invitation.delivery.requested" => {
            nibleaf_membership::deliver_invitation_email_event(pool, config, &event).await
        }
        "identity.otp.delivery.requested" => {
            deliver_otp_email_event(
                pool,
                &config.invitation_email,
                identity_delivery_keys,
                email_client,
                &event,
            )
            .await
        }
        // This event is intentionally durable even before a search/notifier is
        // ported. Marking it delivered makes the deferred capability visible
        // rather than silently dropping its transition.
        "publication.ready" => Ok(()),
        _ => Err(Failure::Permanent {
            message: format!("unsupported event type: {}", event.event_type),
        }),
    };
    match outcome {
        Ok(()) => {
            mark_delivered(pool, &event).await?;
            metrics::counter!("nibleaf_outbox_delivery_total", "outcome" => "delivered")
                .increment(1);
            metrics::histogram!("nibleaf_outbox_delivery_duration_seconds", "outcome" => "delivered")
                .record(started.elapsed().as_secs_f64());
        }
        Err(failure) => {
            let outcome = match &failure {
                Failure::Retryable { .. } => "retryable_failure",
                Failure::Permanent { .. } => "permanent_failure",
            };
            mark_failed(pool, &event, failure, 8).await?;
            metrics::counter!("nibleaf_outbox_delivery_total", "outcome" => outcome).increment(1);
            metrics::histogram!("nibleaf_outbox_delivery_duration_seconds", "outcome" => outcome)
                .record(started.elapsed().as_secs_f64());
        }
    }
    Ok(())
}

const SEARCH_BUILD_LEASE_SECONDS: u64 = 300;
const SEARCH_BUILD_LEASE_HEARTBEAT_SECONDS: u64 = 60;

/// Registers publication-owned search intent before beginning a build. The
/// source event is already durable, so a crash after registry registration but
/// before promotion redelivers the same event and reuses the assigned UUID.
async fn deliver_search_generation_request(
    pool: &sqlx::PgPool,
    context: &WorkerContext,
    worker_id: &str,
    event: &ClaimedEvent,
) -> Result<(), Failure> {
    if event.schema_version != 1 {
        return Err(Failure::Permanent {
            message: "unsupported native search generation event schema".to_owned(),
        });
    }
    let request =
        parse_generation_request(&event.payload).map_err(|error| registry_failure(&error))?;
    if event.aggregate_type != "search_generation" || event.aggregate_id != request.deployment_id {
        return Err(Failure::Permanent {
            message: "native search generation aggregate does not match deployment".to_owned(),
        });
    }
    let tenant_id = event
        .tenant_id
        .as_deref()
        .ok_or_else(|| Failure::Permanent {
            message: "native search generation request must be tenant-scoped".to_owned(),
        })?;
    let mut transaction = pool.begin().await.map_err(|_| Failure::Retryable {
        message: "database unavailable while registering search generation".to_owned(),
    })?;
    let build_request = register_generation_build_for_delivery(
        &mut transaction,
        tenant_id,
        &request.project_id,
        &request.deployment_id,
        &request.snapshot_sha256,
        context.delivery,
    )
    .await
    .map_err(|error| registry_failure(&error))?;
    transaction.commit().await.map_err(|_| Failure::Retryable {
        message: "database unavailable while committing search generation registration".to_owned(),
    })?;
    if let Some(publisher) = &context.publisher {
        return publisher
            .fulfill(&build_request, tenant_id, worker_id)
            .await
            .map_err(|error| registry_failure(&error));
    }
    let Some(index_root) = context.config.search_index_root.as_deref() else {
        return record_search_configuration_failure(pool, tenant_id, worker_id, &build_request)
            .await;
    };
    deliver_registered_search_generation_build(
        pool,
        index_root,
        worker_id,
        tenant_id,
        &build_request,
    )
    .await
}

/// Records a terminal registry state for a missing persistent index root. The
/// row is registered first, so publication completion can observe `failed`
/// rather than waiting forever on an absent generation after the outbox event
/// is dead-lettered.
async fn record_search_configuration_failure(
    pool: &sqlx::PgPool,
    tenant_id: &str,
    worker_id: &str,
    request: &nibleaf_search::registry::SearchBuildRequest,
) -> Result<(), Failure> {
    let outcome = claim_generation_build(
        pool,
        request,
        Some(tenant_id),
        worker_id,
        Duration::from_secs(SEARCH_BUILD_LEASE_SECONDS),
    )
    .await
    .map_err(|error| registry_failure(&error))?;
    match outcome {
        ClaimOutcome::AlreadyReady => Ok(()),
        ClaimOutcome::Claimed(claim) => {
            Err(record_search_claim_failure(pool, &claim, RegistryError::BuildConfiguration).await)
        }
    }
}

/// Executes one already-registered native search build. The registry lease is
/// independent of the outbox lease: duplicate/reclaimed outbox deliveries are
/// safe because only the current registry token may promote. The blocking
/// Tantivy build remains in a joined task while the async worker renews that
/// token; there is no detached build lifecycle.
async fn deliver_registered_search_generation_build(
    pool: &sqlx::PgPool,
    index_root: &Path,
    worker_id: &str,
    tenant_id: &str,
    request: &nibleaf_search::registry::SearchBuildRequest,
) -> Result<(), Failure> {
    let lease = Duration::from_secs(SEARCH_BUILD_LEASE_SECONDS);
    let outcome = claim_generation_build(pool, request, Some(tenant_id), worker_id, lease)
        .await
        .map_err(|error| registry_failure(&error))?;
    let ClaimOutcome::Claimed(claim) = outcome else {
        metrics::counter!("nibleaf_search_generation_claims_total", "outcome" => "already_ready")
            .increment(1);
        return Ok(());
    };
    metrics::counter!(
        "nibleaf_search_generation_claims_total",
        "outcome" => if claim.reclaimed { "reclaimed" } else { "claimed" }
    )
    .increment(1);

    let documents = match load_claimed_snapshot_documents(pool, &claim).await {
        Ok(documents) => documents,
        Err(error) => return Err(record_search_claim_failure(pool, &claim, error).await),
    };
    let root = index_root.to_path_buf();
    let build_claim = claim.clone();
    let mut build = tokio::task::spawn_blocking(move || {
        build_or_reuse_claimed_generation(&root, &build_claim, &documents)
    });
    let mut lost_lease = None;
    let manifest = loop {
        tokio::select! {
            joined = &mut build => {
                break match joined {
                    Ok(result) => result,
                    Err(_) => Err(RegistryError::BuildTask),
                };
            }
            () = tokio::time::sleep(Duration::from_secs(SEARCH_BUILD_LEASE_HEARTBEAT_SECONDS)) => {
                if lost_lease.is_none()
                    && let Err(error) = renew_generation_lease(pool, &claim, lease).await
                {
                    // Do not detach or abort filesystem work mid-write. It
                    // runs to its own completion but can no longer promote.
                    lost_lease = Some(error);
                }
            }
        }
    };
    if let Some(error) = lost_lease {
        return Err(registry_failure(&error));
    }
    let manifest = match manifest {
        Ok(manifest) => manifest,
        Err(error) => return Err(record_search_claim_failure(pool, &claim, error).await),
    };
    promote_claimed_generation(pool, &claim, &manifest)
        .await
        .map_err(|error| registry_failure(&error))?;
    metrics::counter!("nibleaf_search_generation_promotions_total").increment(1);
    Ok(())
}

async fn record_search_claim_failure(
    pool: &sqlx::PgPool,
    claim: &nibleaf_search::registry::ClaimedGeneration,
    error: RegistryError,
) -> Failure {
    match record_claim_failure(pool, claim, &error).await {
        Ok(()) => registry_failure(&error),
        Err(record_error) => registry_failure(&record_error),
    }
}

fn registry_failure(error: &RegistryError) -> Failure {
    let message = error.stable_code().to_owned();
    match error.failure_class() {
        FailureClass::Permanent => Failure::Permanent { message },
        FailureClass::Retryable => Failure::Retryable { message },
    }
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
    let identity_delivery: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('public.rust_identity_otp_delivery')::text")
            .fetch_one(pool)
            .await?;
    let impersonation: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('public.rust_identity_impersonation')::text")
            .fetch_one(pool)
            .await?;
    let impersonation_audit: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('public.rust_identity_impersonation_audit')::text")
            .fetch_one(pool)
            .await?;
    let search_generation_schema_ready = search_generation_schema_ready(pool).await?;
    let expiry_reconciler_audit_contract = if impersonation_audit.is_some() {
        sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM pg_constraint
               WHERE conrelid='public.rust_identity_impersonation_audit'::regclass
                 AND conname='rust_identity_impersonation_audit_actor_check'
             )",
        )
        .fetch_one(pool)
        .await?
    } else {
        false
    };
    if outbox.is_none()
        || identity_delivery.is_none()
        || impersonation.is_none()
        || !search_generation_schema_ready
        || !expiry_reconciler_audit_contract
    {
        return Err(sqlx::Error::Protocol(
            "required Rust platform migrations, including native support expiry reconciliation, have not been applied"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Verifies the expanded registry constraint before a worker accepts a
/// publication-triggered native search build. The first registry migration had
/// a positive-only document count and would reject a valid navigation-only
/// deployment after work was admitted.
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
