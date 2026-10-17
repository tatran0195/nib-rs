use std::{env, error::Error, fs, sync::mpsc as blocking_channel};

use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use super::*;
use crate::{SearchDocument, build_generation, open_generation_with_budget};

type TestResult = Result<(), Box<dyn Error>>;

fn service(
    config: SearchQueryConfig,
) -> Result<(SearchQueryService, SearchQueryDriver), Box<dyn Error>> {
    let pool =
        PgPoolOptions::new().connect_lazy("postgresql://unused:unused@127.0.0.1:1/unused")?;
    Ok(SearchQueryService::new(pool, None, config)?)
}

fn config() -> SearchQueryConfig {
    SearchQueryConfig {
        max_in_flight: 1,
        ..SearchQueryConfig::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_caller_keeps_cpu_slot_and_shutdown_joins_non_cooperative_region() -> TestResult {
    let (service, driver) = service(config())?;
    let stop = CancellationToken::new();
    let _stop_on_drop = stop.clone().drop_guard();
    let runner = tokio::spawn(driver.run(stop.clone()));
    let (started, mut observed) = oneshot::channel();
    let (release, blocked) = blocking_channel::channel();
    let input = Input::Probe(Box::new(move |_| {
        let _ = started.send(());
        blocked
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| SearchError::Integrity("probe release unavailable"))?;
        Ok(())
    }));
    let permit = service.reserve()?;
    let mut response = Box::pin(service.submit(input, permit));
    tokio::select! {
        result = &mut observed => result?,
        _ = &mut response => return Err("probe completed before its release".into()),
        () = tokio::time::sleep(Duration::from_secs(5)) => return Err("probe never started".into()),
    }
    drop(response);
    assert!(matches!(
        service.reserve(),
        Err(SearchQueryExecutionError::Busy)
    ));
    stop.cancel();
    timeout(Duration::from_secs(5), service.stopping.cancelled()).await?;
    assert_eq!(service.slots.available_permits(), 0);
    assert!(
        !runner.is_finished(),
        "driver must still own the running CPU job"
    );
    release.send(())?;
    timeout(Duration::from_secs(5), runner).await???;
    assert!(matches!(
        service.reserve(),
        Err(SearchQueryExecutionError::Stopping)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn response_deadline_does_not_free_capacity_until_actual_worker_exit() -> TestResult {
    let (service, driver) = service(SearchQueryConfig {
        timeout: Duration::from_secs(2),
        ..config()
    })?;
    let stop = CancellationToken::new();
    let _stop_on_drop = stop.clone().drop_guard();
    let runner = tokio::spawn(driver.run(stop.clone()));
    let (started, mut observed) = oneshot::channel();
    let (release, blocked) = blocking_channel::channel();
    let input = Input::Probe(Box::new(move |_| {
        let _ = started.send(());
        blocked
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| SearchError::Integrity("probe release unavailable"))?;
        Ok(())
    }));
    let mut response = Box::pin(service.submit(input, service.reserve()?));
    tokio::select! {
        result = &mut observed => result?,
        _ = &mut response => return Err("probe completed before its release".into()),
    }
    assert!(matches!(
        response.await,
        Err(SearchQueryExecutionError::Deadline)
    ));
    assert!(matches!(
        service.reserve(),
        Err(SearchQueryExecutionError::Busy)
    ));
    release.send(())?;
    let permit = timeout(
        Duration::from_secs(5),
        Arc::clone(&service.slots).acquire_owned(),
    )
    .await??;
    drop(permit);
    stop.cancel();
    timeout(Duration::from_secs(5), runner).await???;
    Ok(())
}

#[tokio::test]
async fn header_limits_and_debug_do_not_capture_private_input() -> TestResult {
    let (service, _driver) = service(config())?;
    assert!(!format!("{service:?}").contains("unused"));
    let mut headers = HeaderMap::new();
    headers.insert(
        "cookie",
        axum::http::HeaderValue::from_str(&"s".repeat(MAX_HEADER_BYTES + 1))?,
    );
    let result = service
        .search_published(
            &ProjectId::try_from("private-project".to_owned())?,
            &headers,
            None,
            PublishedSearchRequest {
                query: "private-query",
                version: None,
                language: None,
                limit: None,
            },
        )
        .await;
    assert!(matches!(
        result,
        Err(SearchQueryExecutionError::Search(
            SearchError::ResourceLimit(_)
        ))
    ));
    assert_eq!(service.slots.available_permits(), 1);
    Ok(())
}

#[test]
fn bounded_validation_refuses_manifest_count_and_source_excess_before_success() -> TestResult {
    let root = env::temp_dir().join(format!("nibleaf-query-bounds-{}", Uuid::now_v7()));
    let documents = ["one", "two"].map(|id| SearchDocument {
        page_id: id.to_owned(),
        language: "en".to_owned(),
        version_id: "main-id".to_owned(),
        version_slug: "main".to_owned(),
        path: format!("/{id}"),
        title: id.to_owned(),
        description: String::new(),
        icon: None,
        headings: String::new(),
        content: "budgetprobe allowed text".to_owned(),
    });
    let generation = build_generation(
        &root,
        "budget-project",
        "budget-deployment",
        Uuid::now_v7(),
        &documents,
    )?;
    let mut budget = QueryBudget {
        config: SearchQueryConfig::default(),
        started: Instant::now(),
        deadline: Instant::now() + Duration::from_secs(5),
        cancellation: CancellationToken::new(),
    };
    for limits in [
        SearchQueryConfig {
            max_manifest_bytes: 1,
            ..config()
        },
        SearchQueryConfig {
            max_documents: 1,
            ..config()
        },
        SearchQueryConfig {
            max_source_bytes: 1,
            ..config()
        },
    ] {
        budget.config = limits;
        assert!(matches!(
            open_generation_with_budget(
                &root,
                generation.project_id(),
                generation.deployment_id(),
                generation.generation_id(),
                Some(&budget),
            ),
            Err(SearchError::ResourceLimit(_))
        ));
    }
    budget.config = SearchQueryConfig::default();
    let (opened, _) = open_generation_with_budget(
        &root,
        generation.project_id(),
        generation.deployment_id(),
        generation.generation_id(),
        Some(&budget),
    )?;
    let hits =
        crate::query::search_member_with_budget(&opened, "budgetprobe", None, None, 2, &budget)?;
    assert_eq!(hits.len(), 2);
    budget.cancellation.cancel();
    assert!(matches!(
        open_generation_with_budget(
            &root,
            generation.project_id(),
            generation.deployment_id(),
            generation.generation_id(),
            Some(&budget),
        ),
        Err(SearchError::Cancelled)
    ));
    fs::remove_dir_all(root)?;
    Ok(())
}

async fn poll_until_submitted(
    response: &mut std::pin::Pin<
        Box<impl Future<Output = Result<Output, SearchQueryExecutionError>>>,
    >,
) -> TestResult {
    std::future::poll_fn(|context| match response.as_mut().poll(context) {
        std::task::Poll::Pending => std::task::Poll::Ready(Ok(())),
        std::task::Poll::Ready(_) => {
            std::task::Poll::Ready(Err("request ended before submission barrier".into()))
        }
    })
    .await
}

#[tokio::test]
async fn cancelled_queued_job_never_executes_or_acquires_a_database_connection() -> TestResult {
    let (service, driver) = service(config())?;
    let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let marker = Arc::clone(&ran);
    let input = Input::Probe(Box::new(move |_| {
        marker.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }));
    let mut response = Box::pin(service.submit(input, service.reserve()?));
    poll_until_submitted(&mut response).await?;
    drop(response);
    let stop = CancellationToken::new();
    let runner = tokio::spawn(driver.run(stop.clone()));
    let permit = timeout(
        Duration::from_secs(5),
        Arc::clone(&service.slots).acquire_owned(),
    )
    .await??;
    drop(permit);
    stop.cancel();
    timeout(Duration::from_secs(5), runner).await???;
    assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
    Ok(())
}

#[tokio::test]
async fn shutdown_closes_and_drains_already_queued_jobs_without_starting_them() -> TestResult {
    let (service, driver) = service(config())?;
    let input = Input::Probe(Box::new(|_| {
        Err(SearchError::Integrity("must not execute"))
    }));
    let mut response = Box::pin(service.submit(input, service.reserve()?));
    poll_until_submitted(&mut response).await?;
    let stop = CancellationToken::new();
    stop.cancel();
    driver.run(stop).await?;
    assert!(matches!(
        response.await,
        Err(SearchQueryExecutionError::Stopping)
    ));
    assert!(matches!(
        service.reserve(),
        Err(SearchQueryExecutionError::Stopping)
    ));
    Ok(())
}
