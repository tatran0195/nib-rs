#![forbid(unsafe_code)]

//! Database-backed proof of the outbox fencing invariant.
//!
//! The ordinary workspace test command skips this test unless the E2E harness
//! supplies a disposable event ID. The harness runs it between creating the
//! project event and starting the worker.

use std::{env, io, time::Duration};

use nibleaf_outbox::claim_batch;
use sqlx::PgPool;
use uuid::Uuid;

#[tokio::test]
async fn same_worker_reclaim_receives_a_fresh_lease_token() -> Result<(), Box<dyn std::error::Error>>
{
    let event_id = match env::var("NIBLEAF_LEASE_FENCE_EVENT_ID") {
        Ok(value) => Uuid::parse_str(&value)?,
        Err(env::VarError::NotPresent) => return Ok(()),
        Err(error) => return Err(Box::new(error) as Box<dyn std::error::Error>),
    };
    let database_url = env::var("NIBLEAF_TEST_DATABASE_URL")?;
    let pool = PgPool::connect(&database_url).await?;
    let worker_id = Uuid::now_v7();

    let first = claim_event(&pool, worker_id, event_id).await?;
    assert!(
        !first.reclaimed,
        "a pending event must not be reported as reclaimed"
    );
    let expired = sqlx::query(
        r"
        UPDATE outbox_event
        SET leased_until = NOW() - interval '1 second'
        WHERE id = $1 AND state = 'processing' AND lease_token = $2
        ",
    )
    .bind(event_id)
    .bind(first.lease_token)
    .execute(&pool)
    .await?;
    if expired.rows_affected() != 1 {
        return Err(io::Error::other("could not expire the first outbox lease").into());
    }

    let second = claim_event(&pool, worker_id, event_id).await?;
    assert!(
        second.reclaimed,
        "an expired processing event must be reported as reclaimed"
    );
    assert_ne!(
        first.lease_token, second.lease_token,
        "a reclaim must fence the previous claim even for the same worker"
    );

    // Restore the exact simulated-crash state that the E2E worker scenario
    // expects. The harness is intentionally destructive only to its named DB.
    let restored = sqlx::query(
        r"
        UPDATE outbox_event
        SET state = 'processing',
            lease_token = $2,
            leased_until = NOW() - interval '1 minute',
            worker_id = 'crashed-test-worker',
            attempts = 1,
            last_error = NULL
        WHERE id = $1
        ",
    )
    .bind(event_id)
    .bind(Uuid::from_u128(1))
    .execute(&pool)
    .await?;
    if restored.rows_affected() != 1 {
        return Err(io::Error::other("could not restore the simulated crash lease").into());
    }
    Ok(())
}

async fn claim_event(
    pool: &PgPool,
    worker_id: Uuid,
    event_id: Uuid,
) -> Result<nibleaf_outbox::ClaimedEvent, Box<dyn std::error::Error>> {
    claim_batch(pool, worker_id, 1, Duration::from_secs(60))
        .await?
        .into_iter()
        .find(|event| event.id == event_id)
        .ok_or_else(|| io::Error::other("expected outbox event was not claimed").into())
}
