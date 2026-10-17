#![forbid(unsafe_code)]

//! Disposable PostgreSQL proof that publication availability is gated by the
//! durable native-search generation state rather than by outbox delivery order.

use std::env;

use nibleaf_core::canonical_json_sha256;
use nibleaf_outbox::{ClaimedEvent, Failure};
use nibleaf_publishing::complete_publication;
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

#[tokio::test]
async fn publication_waits_for_search_promotion_and_fails_on_terminal_search_error()
-> Result<(), Box<dyn std::error::Error>> {
    if env::var("NIBLEAF_PUBLICATION_SEARCH_E2E").is_err() {
        return Ok(());
    }
    let database_url = env::var("NIBLEAF_TEST_DATABASE_URL")?;
    let pool = PgPool::connect(&database_url).await?;
    let suffix = Uuid::now_v7().simple().to_string();
    let tenant_id = format!("publication-search-tenant-{suffix}");
    let project_id = format!("publication-search-project-{suffix}");
    create_project(&pool, &tenant_id, &project_id, &suffix).await?;

    let waiting_deployment_id = format!("publication-search-waiting-{suffix}");
    let snapshot = serde_json::json!({
        "schema_version":2, "project":{"id":project_id}, "pages":[],
        "languages":[{"id":"en", "code":"en", "is_default":true, "enabled":true}],
        "versions":[{"id":"main", "slug":"main", "is_default":true}]
    });
    insert_deployment(&pool, &waiting_deployment_id, &project_id, 1, &snapshot).await?;
    insert_generation(
        &pool,
        Uuid::now_v7(),
        &tenant_id,
        &project_id,
        &waiting_deployment_id,
        &snapshot,
        false,
    )
    .await?;
    let waiting_event = publication_event(&tenant_id, &waiting_deployment_id);
    assert!(matches!(
        complete_publication(&pool, &waiting_event).await,
        Err(Failure::Retryable { .. })
    ));
    assert_eq!(
        deployment_status(&pool, &waiting_deployment_id).await?,
        "BUILDING"
    );
    assert_eq!(
        outbox_count(&pool, &waiting_deployment_id, "publication.ready").await?,
        0
    );

    sqlx::query(
        r"UPDATE rust_search_generation
           SET state = 'ready', document_count = 0, documents_sha256 = $2,
               completed_at = NOW(), updated_at = NOW()
           WHERE project_id = $1 AND deployment_id = $3",
    )
    .bind(&project_id)
    .bind("a".repeat(64))
    .bind(&waiting_deployment_id)
    .execute(&pool)
    .await?;
    complete_publication(&pool, &waiting_event)
        .await
        .map_err(|failure| {
            std::io::Error::other(format!("unexpected publication failure: {failure:?}"))
        })?;
    assert_eq!(
        deployment_status(&pool, &waiting_deployment_id).await?,
        "READY"
    );
    assert_eq!(
        outbox_count(&pool, &waiting_deployment_id, "publication.ready").await?,
        1
    );

    let failed_deployment_id = format!("publication-search-failed-{suffix}");
    insert_deployment(&pool, &failed_deployment_id, &project_id, 2, &snapshot).await?;
    insert_generation(
        &pool,
        Uuid::now_v7(),
        &tenant_id,
        &project_id,
        &failed_deployment_id,
        &snapshot,
        true,
    )
    .await?;
    let failed_event = publication_event(&tenant_id, &failed_deployment_id);
    assert!(matches!(
        complete_publication(&pool, &failed_event).await,
        Err(Failure::Permanent { .. })
    ));
    assert_eq!(
        deployment_status(&pool, &failed_deployment_id).await?,
        "FAILED"
    );
    assert_eq!(
        outbox_count(&pool, &failed_deployment_id, "publication.ready").await?,
        0
    );
    // This proof invokes the consumer directly and is not a normal outbox
    // lease. Remove its asserted completion evidence so it cannot become the
    // oldest unrelated event in the harness's separate lease-fencing proof.
    sqlx::query("DELETE FROM outbox_event WHERE aggregate_id = $1")
        .bind(&waiting_deployment_id)
        .execute(&pool)
        .await?;
    Ok(())
}

fn publication_event(tenant_id: &str, deployment_id: &str) -> ClaimedEvent {
    ClaimedEvent {
        id: Uuid::now_v7(),
        tenant_id: Some(tenant_id.to_owned()),
        aggregate_type: "deployment".to_owned(),
        aggregate_id: deployment_id.to_owned(),
        event_type: "publication.requested".to_owned(),
        schema_version: 1,
        payload: serde_json::json!({"deployment_id": deployment_id}),
        attempts: 1,
        reclaimed: false,
        lease_token: Uuid::now_v7(),
        leased_until: OffsetDateTime::now_utc() + Duration::minutes(1),
    }
}

async fn create_project(
    pool: &PgPool,
    tenant_id: &str,
    project_id: &str,
    suffix: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO organization (id, name, slug) VALUES ($1, 'Publication search test', $2)",
    )
    .bind(tenant_id)
    .bind(format!("publication-search-{suffix}"))
    .execute(pool)
    .await?;
    sqlx::query(
        r#"INSERT INTO project (
              id, "organizationId", name, slug, config, "accessMode", "createdAt", "updatedAt"
           ) VALUES ($1, $2, 'Publication search test', $3, '{}'::jsonb,
                     'PUBLIC'::"ProjectAccessMode", NOW(), NOW())"#,
    )
    .bind(project_id)
    .bind(tenant_id)
    .bind(format!("publication-search-project-{suffix}"))
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_deployment(
    pool: &PgPool,
    deployment_id: &str,
    project_id: &str,
    version: i32,
    snapshot: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO deployment (
              id, "projectId", version, status, snapshot, "pagesCount", "createdAt"
           ) VALUES ($1, $2, $3, 'PENDING'::"DeploymentStatus", $4, 0, NOW())"#,
    )
    .bind(deployment_id)
    .bind(project_id)
    .bind(version)
    .bind(snapshot)
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_generation(
    pool: &PgPool,
    generation_id: Uuid,
    tenant_id: &str,
    project_id: &str,
    deployment_id: &str,
    snapshot: &serde_json::Value,
    failed: bool,
) -> Result<(), sqlx::Error> {
    if failed {
        sqlx::query(
            r"INSERT INTO rust_search_generation (
                       generation_id, project_id, deployment_id, tenant_id, snapshot_sha256,
                       state, last_error, completed_at
                   ) VALUES ($1, $2, $3, $4, $5, 'failed', 'proof failure', NOW())",
        )
        .bind(generation_id)
        .bind(project_id)
        .bind(deployment_id)
        .bind(tenant_id)
        .bind(canonical_json_sha256(snapshot))
        .execute(pool)
        .await?;
    } else {
        sqlx::query(
            r"INSERT INTO rust_search_generation (
                       generation_id, project_id, deployment_id, tenant_id, snapshot_sha256, state
                   ) VALUES ($1, $2, $3, $4, $5, 'pending')",
        )
        .bind(generation_id)
        .bind(project_id)
        .bind(deployment_id)
        .bind(tenant_id)
        .bind(canonical_json_sha256(snapshot))
        .execute(pool)
        .await?;
    }
    Ok(())
}

async fn deployment_status(pool: &PgPool, deployment_id: &str) -> Result<String, sqlx::Error> {
    sqlx::query_scalar("SELECT status::text FROM deployment WHERE id = $1")
        .bind(deployment_id)
        .fetch_one(pool)
        .await
}

async fn outbox_count(
    pool: &PgPool,
    deployment_id: &str,
    event_type: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM outbox_event WHERE aggregate_id = $1 AND event_type = $2",
    )
    .bind(deployment_id)
    .bind(event_type)
    .fetch_one(pool)
    .await
}
