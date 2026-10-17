use metrics_exporter_prometheus::{BuildError, PrometheusBuilder, PrometheusHandle};
use sqlx::PgPool;
use thiserror::Error;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

#[derive(Debug, Error)]
pub enum TelemetryInitError {
    #[error("unable to initialize structured tracing: {0}")]
    Tracing(#[from] tracing_subscriber::util::TryInitError),
    #[error("unable to initialize Prometheus metrics: {0}")]
    Metrics(#[from] BuildError),
}

/// Initialise structured JSON logs and the process-local Prometheus recorder.
///
/// Metrics are rendered by the API/worker's explicitly exposed `/metrics`
/// handler. The only global metric label is the bounded service name; callers
/// must never add tenant, actor, raw path, token, or error-message labels.
///
/// # Errors
///
/// Returns an error when this process cannot install its tracing subscriber or
/// Prometheus recorder. Startup intentionally fails in that case instead of
/// running with silently absent observability.
pub fn init(service: &'static str) -> Result<PrometheusHandle, TelemetryInitError> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let formatting = fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(true);
    tracing_subscriber::registry()
        .with(filter)
        .with(formatting)
        .try_init()?;
    let metrics = PrometheusBuilder::new()
        .add_global_label("service", service)
        .install_recorder()?;
    tracing::info!(service, "telemetry initialized");
    Ok(metrics)
}

/// Samples bounded connection-pool gauges at metrics-scrape time.
///
/// `SQLx` exposes total and idle connections without inspecting database state;
/// `in_use` is the non-idle portion of this process's pool. The values describe
/// the local pool only, so they are safe to expose without tenant labels.
pub fn record_database_pool_metrics(pool: &PgPool) {
    let total = pool.size();
    let idle = u32::try_from(pool.num_idle()).unwrap_or(total).min(total);
    let in_use = total.saturating_sub(idle);
    metrics::gauge!("nibleaf_database_pool_connections", "state" => "total").set(f64::from(total));
    metrics::gauge!("nibleaf_database_pool_connections", "state" => "idle").set(f64::from(idle));
    metrics::gauge!("nibleaf_database_pool_connections", "state" => "in_use")
        .set(f64::from(in_use));
}
