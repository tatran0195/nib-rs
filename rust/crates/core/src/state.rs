use std::sync::Arc;

use metrics_exporter_prometheus::PrometheusHandle;
use sqlx::PgPool;

use crate::Config;

/// Immutable process dependencies. `PgPool` is internally concurrent; product
/// state is persisted in PostgreSQL rather than kept behind application locks.
#[derive(Clone, Debug)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub metrics: PrometheusHandle,
}

impl AppState {
    #[must_use]
    pub fn new(pool: PgPool, config: Config, metrics: PrometheusHandle) -> Self {
        Self {
            pool,
            config: Arc::new(config),
            metrics,
        }
    }
}
