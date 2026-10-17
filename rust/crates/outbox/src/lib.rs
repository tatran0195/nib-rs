#![forbid(unsafe_code)]

//! PostgreSQL transactional outbox.
//!
//! The API appends events using the same SQL transaction as its state change;
//! workers claim with leases and `FOR UPDATE SKIP LOCKED`. Delivery is
//! explicitly at-least-once, so consumers must remain idempotent.

use std::time::Duration;

use nibleaf_core::AppError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
pub struct NewEvent<'a> {
    /// `None` is reserved for platform-scoped workflows (for example, sign-in
    /// email sent before a user belongs to any organization).
    pub tenant_id: Option<&'a str>,
    pub aggregate_type: &'a str,
    pub aggregate_id: &'a str,
    pub event_type: &'a str,
    pub schema_version: i16,
    pub payload: Value,
    pub correlation_id: Uuid,
}

#[derive(Clone, Debug, FromRow, Serialize, Deserialize)]
pub struct ClaimedEvent {
    pub id: Uuid,
    pub tenant_id: Option<String>,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub event_type: String,
    pub schema_version: i16,
    pub payload: Value,
    pub attempts: i32,
    pub reclaimed: bool,
    pub lease_token: Uuid,
    pub leased_until: OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Failure {
    Retryable { message: String },
    Permanent { message: String },
}

/// Minimal retry schedule. It is bounded, jittered deterministically from the
/// event ID, and only applies after the transaction owning the lease is closed.
#[must_use]
pub fn retry_delay(event_id: Uuid, attempt: i32) -> Duration {
    let exponent = u32::try_from(attempt.clamp(1, 8)).unwrap_or(8);
    let base_seconds = 2_u64.saturating_pow(exponent).min(300);
    // Up to 20% deterministic jitter; the byte calculation avoids a random
    // dependency while spreading retries from a burst of related events.
    let jitter = u64::from(event_id.as_bytes()[0]) * base_seconds * 20 / 25_500;
    Duration::from_secs(base_seconds + jitter)
}

/// # Errors
///
/// Returns an error when the enclosing transaction cannot persist the event.
pub async fn append(
    transaction: &mut Transaction<'_, Postgres>,
    event: NewEvent<'_>,
) -> Result<Uuid, AppError> {
    let id = Uuid::now_v7();
    sqlx::query(
        r"
        INSERT INTO outbox_event (
            id, tenant_id, aggregate_type, aggregate_id, event_type,
            schema_version, payload, correlation_id
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ",
    )
    .bind(id)
    .bind(event.tenant_id)
    .bind(event.aggregate_type)
    .bind(event.aggregate_id)
    .bind(event.event_type)
    .bind(event.schema_version)
    .bind(event.payload)
    .bind(event.correlation_id)
    .execute(&mut **transaction)
    .await?;
    Ok(id)
}

/// Atomically obtains leases for pending events and events abandoned by a
/// crashed worker. No external call occurs while this statement's transaction
/// is held. A lease token fences stale workers from completing a later owner's
/// work.
/// # Errors
///
/// Returns an error when the claim query or lease conversion fails.
pub async fn claim_batch(
    pool: &PgPool,
    worker_id: Uuid,
    limit: i64,
    lease_for: Duration,
) -> Result<Vec<ClaimedEvent>, AppError> {
    let lease_seconds = i32::try_from(lease_for.as_secs())
        .map_err(|_| AppError::validation("outbox lease is too large"))?;
    let rows = sqlx::query_as::<_, ClaimedEvent>(
        r"
        WITH candidates AS (
            SELECT id, state = 'processing' AS reclaimed
            FROM outbox_event
            WHERE available_at <= NOW()
              AND (state = 'pending' OR (state = 'processing' AND leased_until < NOW()))
            ORDER BY available_at ASC, id ASC
            FOR UPDATE SKIP LOCKED
            LIMIT $1
        )
        UPDATE outbox_event AS event
        SET state = 'processing',
            -- A lease token identifies this specific claim, not merely this
            -- worker process. A restarted/reclaiming worker must not share a
            -- token with its own stale claim or it could acknowledge newer work.
            lease_token = gen_random_uuid(),
            leased_until = NOW() + make_interval(secs => $2),
            attempts = event.attempts + 1,
            last_error = NULL,
            worker_id = $3
        FROM candidates
        WHERE event.id = candidates.id
        RETURNING event.id, event.tenant_id, event.aggregate_type, event.aggregate_id,
                  event.event_type, event.schema_version, event.payload, event.attempts,
                  candidates.reclaimed, event.lease_token, event.leased_until
        ",
    )
    .bind(limit.clamp(1, 100))
    .bind(lease_seconds)
    .bind(worker_id.to_string())
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// # Errors
///
/// Returns an error when the database fails or the worker no longer owns the lease.
pub async fn mark_delivered(pool: &PgPool, event: &ClaimedEvent) -> Result<(), AppError> {
    let updated = sqlx::query(
        r"
        UPDATE outbox_event
        SET state = 'delivered', delivered_at = NOW(), leased_until = NULL, lease_token = NULL
        WHERE id = $1 AND state = 'processing' AND lease_token = $2
        ",
    )
    .bind(event.id)
    .bind(event.lease_token)
    .execute(pool)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict(
            "outbox lease was lost before delivery could be recorded",
        ));
    }
    Ok(())
}

/// # Errors
///
/// Returns an error when the database fails or the worker no longer owns the lease.
pub async fn mark_failed(
    pool: &PgPool,
    event: &ClaimedEvent,
    failure: Failure,
    max_attempts: i32,
) -> Result<(), AppError> {
    let (state, delay_seconds, message) = match failure {
        Failure::Retryable { message } if event.attempts < max_attempts => {
            let seconds =
                i32::try_from(retry_delay(event.id, event.attempts).as_secs()).unwrap_or(300);
            ("pending", seconds, message)
        }
        Failure::Retryable { message } | Failure::Permanent { message } => {
            ("dead_letter", 0, message)
        }
    };
    let updated = sqlx::query(
        r"
        UPDATE outbox_event
        SET state = $3,
            available_at = NOW() + make_interval(secs => $4),
            leased_until = NULL,
            lease_token = NULL,
            last_error = left($5, 2_000)
        WHERE id = $1 AND state = 'processing' AND lease_token = $2
        ",
    )
    .bind(event.id)
    .bind(event.lease_token)
    .bind(state)
    .bind(delay_seconds)
    .bind(message)
    .execute(pool)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict(
            "outbox lease was lost before failure could be recorded",
        ));
    }
    Ok(())
}

/// Bounded outbox state snapshot used by the worker's scrape-time metrics.
/// It intentionally exposes no tenant, aggregate, event, or error dimensions.
#[derive(Clone, Debug, FromRow)]
pub struct BacklogSnapshot {
    pub pending: f64,
    pub processing: f64,
    pub dead_letter: f64,
    pub oldest_open_seconds: Option<f64>,
}

/// # Errors
///
/// Returns an error when PostgreSQL cannot calculate the operational snapshot.
pub async fn backlog_snapshot(pool: &PgPool) -> Result<BacklogSnapshot, AppError> {
    sqlx::query_as::<_, BacklogSnapshot>(
        r"
        SELECT
            (count(*) FILTER (WHERE state = 'pending'))::double precision AS pending,
            (count(*) FILTER (WHERE state = 'processing'))::double precision AS processing,
            (count(*) FILTER (WHERE state = 'dead_letter'))::double precision AS dead_letter,
            extract(epoch FROM NOW() - min(created_at) FILTER (
                WHERE state IN ('pending', 'processing')
            ))::double precision AS oldest_open_seconds
        FROM outbox_event
        ",
    )
    .fetch_one(pool)
    .await
    .map_err(AppError::from)
}

/// Explicit administrative replay. The caller is expected to authorize this at
/// the transport boundary and record a separate audit event. It never replays a
/// currently leased event.
/// # Errors
///
/// Returns an error when the replay state cannot be persisted.
pub async fn replay_dead_letter(pool: &PgPool, event_id: Uuid) -> Result<bool, AppError> {
    let result = sqlx::query(
        r"
        UPDATE outbox_event
        SET state = 'pending', available_at = NOW(), lease_token = NULL,
            leased_until = NULL, last_error = NULL
        WHERE id = $1 AND state = 'dead_letter'
        ",
    )
    .bind(event_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::retry_delay;
    use uuid::Uuid;

    #[test]
    fn retry_delay_is_bounded_and_monotonic_before_cap() {
        let event = Uuid::nil();
        assert!(retry_delay(event, 2) >= retry_delay(event, 1));
        assert!(retry_delay(event, 99) <= Duration::from_secs(360));
    }
}
