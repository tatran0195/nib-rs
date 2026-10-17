//! Actual service/driver proof, reusing only the parent publication fixtures.

use axum::http::{HeaderMap, HeaderValue, header};
use nibleaf_core::AppError;
use nibleaf_search::{
    PublishedSearchError, PublishedSearchRequest, PublishedSearchResult, SearchError,
    SearchQueryConfig, SearchQueryExecutionError, SearchQueryService,
};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use super::{
    Duration, Fixture, TestResult, create_reader_scope, fixture, fs, json, publication, set_cap,
    set_mode, set_status,
};

type QueryResult = Result<Option<PublishedSearchResult>, SearchQueryExecutionError>;

pub(super) async fn verify(pool: &PgPool, suffix: &str) -> TestResult {
    let fixture = fixture(pool, suffix).await?;
    let published = publication(pool, &fixture, 1, "budgetprobe", 3).await?;
    set_status(pool, &published.request.deployment_id, "READY").await?;
    assert_service_limits(pool, &fixture).await?;
    assert_database_work_stays_owned_after_caller_drop(pool, &fixture).await?;
    assert_pool_acquisition_is_inside_deadline(&fixture).await?;
    assert_reader_grants_are_bounded_without_truncation(pool, &fixture).await?;
    fs::remove_dir_all(&fixture.root)?;
    Ok(())
}

fn request() -> PublishedSearchRequest<'static> {
    PublishedSearchRequest {
        query: "budgetprobe",
        version: None,
        language: None,
        limit: Some(50),
    }
}

async fn run_once(
    pool: &PgPool,
    fixture: &Fixture,
    config: SearchQueryConfig,
    headers: &HeaderMap,
) -> TestResult<QueryResult> {
    let (service, driver) =
        SearchQueryService::new(pool.clone(), Some(fixture.root.clone()), config)?;
    let shutdown = CancellationToken::new();
    let _stop_on_drop = shutdown.clone().drop_guard();
    let runner = tokio::spawn(driver.run(shutdown.clone()));
    let result = service
        .search_published(&fixture.project.project_id, headers, None, request())
        .await;
    shutdown.cancel();
    timeout(Duration::from_secs(8), runner).await???;
    Ok(result)
}

async fn assert_service_limits(pool: &PgPool, fixture: &Fixture) -> TestResult {
    let headers = HeaderMap::new();
    let result = run_once(pool, fixture, SearchQueryConfig::default(), &headers)
        .await??
        .ok_or("bounded query concealed valid publication")?;
    assert_eq!(result.hits.len(), 3);
    assert_eq!(result.publication_version, 1);
    for config in [
        SearchQueryConfig {
            max_snapshot_bytes: 1,
            ..SearchQueryConfig::default()
        },
        SearchQueryConfig {
            max_manifest_bytes: 1,
            ..SearchQueryConfig::default()
        },
        SearchQueryConfig {
            max_documents: 1,
            ..SearchQueryConfig::default()
        },
        SearchQueryConfig {
            max_source_bytes: 1,
            ..SearchQueryConfig::default()
        },
    ] {
        assert!(matches!(
            run_once(pool, fixture, config, &headers).await?,
            Err(SearchQueryExecutionError::Search(
                SearchError::ResourceLimit(_)
            ))
        ));
    }
    // Live control projection must not transfer unrelated theme/provider/answer
    // blobs or let them reset a valid cap. Frozen snapshot verification stays full.
    sqlx::query("UPDATE project SET config=$2 WHERE id=$1")
        .bind(fixture.project.project_id.as_str())
        .bind(json!({"styling":{"opaque":"x".repeat(70_000)},
                     "search":{"maxResults":50,"answer":"x".repeat(70_000)}}))
        .execute(pool)
        .await?;
    let result = run_once(pool, fixture, SearchQueryConfig::default(), &headers)
        .await??
        .ok_or("unrelated live config disabled search")?;
    assert_eq!(result.context.result_limit(), 3);
    assert_eq!(result.hits.len(), 3);
    sqlx::query("UPDATE project SET config=$2 WHERE id=$1")
        .bind(fixture.project.project_id.as_str())
        .bind(json!({"search":{"maxResults":"x".repeat(70_000)}}))
        .execute(pool)
        .await?;
    assert!(matches!(
        run_once(pool, fixture, SearchQueryConfig::default(), &headers).await?,
        Err(SearchQueryExecutionError::Search(
            SearchError::ResourceLimit(_)
        ))
    ));
    set_cap(pool, fixture, 50).await?;
    Ok(())
}

async fn wait_for_selection_lock(pool: &PgPool, blocker: i32) -> TestResult<i32> {
    loop {
        let waiting: Option<i32> = sqlx::query_scalar(
            r"SELECT pid FROM pg_stat_activity WHERE wait_event_type='Lock'
               AND $1=ANY(pg_blocking_pids(pid))
               AND query LIKE '%nibleaf:published-search-selection%' LIMIT 1",
        )
        .bind(blocker)
        .fetch_optional(pool)
        .await?;
        if let Some(pid) = waiting {
            return Ok(pid);
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn assert_database_work_stays_owned_after_caller_drop(
    pool: &PgPool,
    fixture: &Fixture,
) -> TestResult {
    let mut blocker = pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    sqlx::query("LOCK TABLE rust_search_generation IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await?;
    let (service, driver) = SearchQueryService::new(
        pool.clone(),
        Some(fixture.root.clone()),
        SearchQueryConfig {
            max_in_flight: 1,
            timeout: Duration::from_secs(2),
            ..SearchQueryConfig::default()
        },
    )?;
    let shutdown = CancellationToken::new();
    let _stop_on_drop = shutdown.clone().drop_guard();
    let runner = tokio::spawn(driver.run(shutdown.clone()));
    let headers = HeaderMap::new();
    let mut searching =
        Box::pin(service.search_published(&fixture.project.project_id, &headers, None, request()));
    let observed = async {
        let waiting_pid = tokio::select! {
            result = wait_for_selection_lock(pool, blocker_pid) => result?,
            _ = &mut searching => return Err("query ended before reaching real database barrier".into()),
        };
        assert!(matches!(service.search_published(&fixture.project.project_id, &headers, None, request()).await,
            Err(SearchQueryExecutionError::Busy)));
        Ok::<i32, Box<dyn std::error::Error>>(waiting_pid)
    }.await;
    drop(searching);
    let held_after_drop = if observed.is_ok() {
        matches!(
            service
                .search_published(&fixture.project.project_id, &headers, None, request())
                .await,
            Err(SearchQueryExecutionError::Busy)
        )
    } else {
        false
    };
    shutdown.cancel();
    let drained = timeout(Duration::from_secs(8), runner).await;
    let no_abandoned_wait = if let Ok(waiting_pid) = observed.as_ref() {
        sqlx::query_scalar::<_, bool>(
            "SELECT NOT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event_type='Lock')",
        ).bind(waiting_pid).fetch_one(pool).await
    } else {
        Ok(false)
    };
    // Keep the blocking lock held until AFTER driver drain/server-timeout proof.
    blocker.rollback().await?;
    drained???;
    observed?;
    assert!(held_after_drop);
    assert!(
        no_abandoned_wait?,
        "drain must not abandon server work or free capacity early"
    );
    let setting: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(pool)
        .await?;
    assert_eq!(setting, "0", "SET LOCAL must not contaminate the pool");
    Ok(())
}

async fn assert_pool_acquisition_is_inside_deadline(fixture: &Fixture) -> TestResult {
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(4))
        .connect(&std::env::var("NIBLEAF_TEST_DATABASE_URL")?)
        .await?;
    let mut held = pool.acquire().await?;
    let (service, driver) = SearchQueryService::new(
        pool.clone(),
        Some(fixture.root.clone()),
        SearchQueryConfig {
            timeout: Duration::from_millis(100),
            ..SearchQueryConfig::default()
        },
    )?;
    let shutdown = CancellationToken::new();
    let runner = tokio::spawn(driver.run(shutdown.clone()));
    let result = service
        .search_published(
            &fixture.project.project_id,
            &HeaderMap::new(),
            None,
            request(),
        )
        .await;
    shutdown.cancel();
    timeout(Duration::from_secs(5), runner).await???;
    assert!(matches!(result, Err(SearchQueryExecutionError::Deadline)));
    let one: i32 = sqlx::query_scalar("SELECT 1").fetch_one(&mut *held).await?;
    assert_eq!(one, 1);
    drop(held);
    pool.close().await;
    Ok(())
}

async fn assert_reader_grants_are_bounded_without_truncation(
    pool: &PgPool,
    fixture: &Fixture,
) -> TestResult {
    set_mode(pool, fixture, "READERS").await?;
    let reader = format!("bounded-reader-{}", fixture.suffix);
    let audience = format!("bounded-audience-{}", fixture.suffix);
    let second_page = format!("bounded-second-page-{}", fixture.suffix);
    sqlx::query(
        r#"INSERT INTO page (id,"projectId","languageId","branchId",kind,title,slug,path,content,position,hidden,"createdAt","updatedAt",revision)
           SELECT $1,"projectId","languageId","branchId",kind,'Second grant page','bounded-second','bounded-second','draft only',1,FALSE,NOW(),NOW(),0
           FROM page WHERE id=$3 AND "projectId"=$2"#,
    ).bind(&second_page).bind(fixture.project.project_id.as_str()).bind(&fixture.allowed_page)
        .execute(pool).await?;
    create_reader_scope(
        pool,
        &fixture.project.project_id,
        &reader,
        &audience,
        &fixture.allowed_page,
        "bounded-reader-token",
    )
    .await?;
    sqlx::query(
        r#"INSERT INTO audience_grant (id,"projectId","audienceId","pageId","scopeKey","createdAt")
                   VALUES ($1,$2,$3,$4,$5,NOW())"#,
    )
    .bind(format!("bounded-extra-grant-{}", fixture.suffix))
    .bind(fixture.project.project_id.as_str())
    .bind(&audience)
    .bind(&second_page)
    .bind(format!("PAGE:{second_page}"))
    .execute(pool)
    .await?;
    let before: time::PrimitiveDateTime =
        sqlx::query_scalar("SELECT \"lastUsedAt\" FROM reader_session WHERE \"readerId\"=$1")
            .bind(&reader)
            .fetch_one(pool)
            .await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!(
            "nibleaf_reader_{}=bounded-reader-token",
            fixture.project.project_id.as_str()
        ))?,
    );
    let tiny = SearchQueryConfig {
        max_documents: 1,
        ..SearchQueryConfig::default()
    };
    assert!(matches!(
        run_once(pool, fixture, tiny, &headers).await?,
        Err(SearchQueryExecutionError::Published(
            PublishedSearchError::Admission(AppError::Unavailable)
        ))
    ));
    sqlx::query(
        r#"INSERT INTO audience_grant (id,"projectId","audienceId","pageId","scopeKey","createdAt")
                   VALUES ($1,$2,$3,NULL,'SITE',NOW())"#,
    )
    .bind(format!("bounded-site-grant-{}", fixture.suffix))
    .bind(fixture.project.project_id.as_str())
    .bind(&audience)
    .execute(pool)
    .await?;
    // A real site grant bypasses the unnecessary page array, NOT generation
    // limits. Reaching the document-count guard proves admission was full-site.
    assert!(matches!(
        run_once(pool, fixture, tiny, &headers).await?,
        Err(SearchQueryExecutionError::Search(
            SearchError::ResourceLimit("generation documents")
        ))
    ));
    let after: time::PrimitiveDateTime =
        sqlx::query_scalar("SELECT \"lastUsedAt\" FROM reader_session WHERE \"readerId\"=$1")
            .bind(&reader)
            .fetch_one(pool)
            .await?;
    assert_eq!(before, after);
    set_mode(pool, fixture, "PUBLIC").await?;
    Ok(())
}
