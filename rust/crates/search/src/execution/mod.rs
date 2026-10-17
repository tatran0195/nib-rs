//! Feature-owned bounded query supervision. The application drives `run` to
//! completion alongside HTTP and never aborts it during normal shutdown.

pub(crate) mod budget;
mod member;

use std::{fmt, future::Future, path::PathBuf, sync::Arc, time::Duration};

use axum::http::HeaderMap;
use nibleaf_core::{Actor, AppError, ProjectId};
use sqlx::{PgPool, Postgres, pool::PoolConnection};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
    task::JoinSet,
    time::{Instant, sleep_until, timeout},
};
use tokio_util::sync::CancellationToken;

use crate::{
    PublishedSearchError, PublishedSearchRequest, PublishedSearchResult, SearchError,
    api::ValidatedSearchQuery,
    published::{self, ValidatedRequest},
};
use budget::QueryBudget;
use member::{MemberInput, MemberSearchResult, PreparedMemberSearch};

pub use budget::{SearchQueryConfig, SearchQueryConfigError};

const MAX_HEADER_BYTES: usize = 16 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SearchQueryExecutionError {
    #[error("search capacity is exhausted")]
    Busy,
    #[error("search service is stopping")]
    Stopping,
    #[error("search query deadline expired")]
    Deadline,
    #[error("search query was cancelled")]
    Cancelled,
    #[error("search worker failed")]
    WorkerFailed,
    #[error("published search failed")]
    Published(#[source] PublishedSearchError),
    #[error("member search failed")]
    Member(#[source] AppError),
    #[error("search index operation failed")]
    Search(#[source] SearchError),
}

impl From<SearchError> for SearchQueryExecutionError {
    fn from(error: SearchError) -> Self {
        match error {
            SearchError::Cancelled => Self::Cancelled,
            SearchError::Deadline => Self::Deadline,
            other => Self::Search(other),
        }
    }
}

impl From<AppError> for SearchQueryExecutionError {
    fn from(error: AppError) -> Self {
        if matches!(&error, AppError::Database(error) if database_timeout(error)) {
            Self::Deadline
        } else {
            Self::Member(error)
        }
    }
}

impl From<PublishedSearchError> for SearchQueryExecutionError {
    fn from(error: PublishedSearchError) -> Self {
        match error {
            PublishedSearchError::Search(error) => error.into(),
            PublishedSearchError::Database(ref error) if database_timeout(error) => Self::Deadline,
            PublishedSearchError::Admission(AppError::Database(ref error))
                if database_timeout(error) =>
            {
                Self::Deadline
            }
            other => Self::Published(other),
        }
    }
}

fn database_timeout(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "57014")
}

#[derive(Clone)]
pub struct SearchQueryService {
    sender: mpsc::Sender<Job>,
    slots: Arc<Semaphore>,
    stopping: CancellationToken,
    config: SearchQueryConfig,
}

impl fmt::Debug for SearchQueryService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SearchQueryService")
            .field("config", &self.config)
            .field("stopping", &self.stopping.is_cancelled())
            .finish_non_exhaustive()
    }
}

#[must_use = "the API must drive the query service and drain it before closing its database pool"]
pub struct SearchQueryDriver {
    receiver: mpsc::Receiver<Job>,
    pool: PgPool,
    root: Option<Arc<PathBuf>>,
    cache: Option<Arc<crate::distribution::ReplicaCache>>,
    service: SearchQueryService,
}

impl fmt::Debug for SearchQueryDriver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SearchQueryDriver")
            .field("config", &self.service.config)
            .finish_non_exhaustive()
    }
}

struct PublishedInput {
    project_id: ProjectId,
    headers: HeaderMap,
    actor: Option<Actor>,
    resolver: Option<Arc<dyn crate::PublishedActorResolver>>,
    request: Result<ValidatedRequest, PublishedSearchError>,
}

#[cfg(test)]
type ProbeWork = Box<dyn FnOnce(&QueryBudget) -> Result<(), SearchError> + Send>;

struct QueryLease {
    permit: OwnedSemaphorePermit,
    started: Instant,
    deadline: Instant,
}

enum Input {
    Published(Box<PublishedInput>),
    Member(Box<MemberInput>),
    #[cfg(test)]
    Probe(ProbeWork),
}

enum Output {
    Published(Option<PublishedSearchResult>),
    Public(Option<crate::public_api::PublicBody>),
    Member(MemberSearchResult),
    #[cfg(test)]
    Probe,
}

enum Prepared {
    Published(Option<Box<crate::PreparedPublishedSearch>>),
    Public(Option<Box<crate::PreparedPublishedSearch>>),
    Member(Box<PreparedMemberSearch>),
    #[cfg(test)]
    Probe(ProbeWork),
}

struct Job {
    input: Input,
    budget: QueryBudget,
    reply: oneshot::Sender<Result<Output, SearchQueryExecutionError>>,
    // Must outlive all preparation, cleanup and joined blocking work, not just
    // the caller's response deadline. No task aborts are used by this driver.
    _permit: OwnedSemaphorePermit,
}

impl SearchQueryService {
    /// Creates an inert service/driver pair. No task is spawned here. The API
    /// owns and drives the driver alongside HTTP, signalling then joining both
    /// on every server-exit path. Admission is immediate, with no waiting queue.
    ///
    /// # Errors
    /// Refuses invalid operational limits before any query can be submitted.
    pub fn new(
        pool: PgPool,
        root: Option<PathBuf>,
        config: SearchQueryConfig,
    ) -> Result<(Self, SearchQueryDriver), SearchQueryConfigError> {
        config.validate()?;
        let (sender, receiver) = mpsc::channel(config.max_in_flight);
        let service = Self {
            sender,
            slots: Arc::new(Semaphore::new(config.max_in_flight)),
            stopping: CancellationToken::new(),
            config,
        };
        let driver = SearchQueryDriver {
            receiver,
            pool,
            root: root.map(Arc::new),
            cache: None,
            service: service.clone(),
        };
        Ok((service, driver))
    }

    /// Same bounded/owned driver with server-selected object materialization.
    /// # Errors
    /// Refuses invalid query limits before accepting any work.
    pub fn new_with_cache(
        pool: PgPool,
        cache: Arc<crate::distribution::ReplicaCache>,
        config: SearchQueryConfig,
    ) -> Result<(Self, SearchQueryDriver), SearchQueryConfigError> {
        let (service, mut driver) = Self::new(pool, None, config)?;
        driver.cache = Some(cache);
        Ok((service, driver))
    }

    /// Runs fresh published admission/selection and consuming execution inside
    /// one capacity lease and deadline. An Actor must already be authenticated;
    /// reader cookies are never workspace credentials. No public route or DTO
    /// is introduced. Missing/denied/unpublished remains concealed `None`.
    ///
    /// # Errors
    /// Typed capacity, shutdown, deadline, input/database and integrity errors.
    /// A timed-out/dropped caller never releases a still-running worker's slot.
    pub async fn search_published(
        &self,
        project_id: &ProjectId,
        headers: &HeaderMap,
        actor: Option<&Actor>,
        request: PublishedSearchRequest<'_>,
    ) -> Result<Option<PublishedSearchResult>, SearchQueryExecutionError> {
        let permit = self.reserve()?;
        let bytes = headers.iter().try_fold(0_usize, |total, (key, value)| {
            total
                .checked_add(key.as_str().len())?
                .checked_add(value.len())
        });
        if bytes.is_none_or(|bytes| bytes > MAX_HEADER_BYTES) {
            return Err(rejected(
                SearchError::ResourceLimit("request header bytes").into(),
            ));
        }
        let input = Input::Published(Box::new(PublishedInput {
            project_id: project_id.clone(),
            headers: headers.clone(),
            actor: actor.cloned(),
            resolver: None,
            request: published::validate_request(request),
        }));
        match self.submit(input, permit).await? {
            Output::Published(result) => Ok(result),
            Output::Member(_) | Output::Public(_) => Err(SearchQueryExecutionError::WorkerFailed),
            #[cfg(test)]
            Output::Probe => Err(SearchQueryExecutionError::WorkerFailed),
        }
    }

    pub(crate) async fn search_public_transport(
        &self,
        project_id: &ProjectId,
        headers: &HeaderMap,
        raw_query: Option<&str>,
        resolver: Arc<dyn crate::PublishedActorResolver>,
    ) -> Result<Option<crate::public_api::PublicBody>, SearchQueryExecutionError> {
        let lease = self.reserve()?;
        let bytes = headers.iter().try_fold(0_usize, |total, (key, value)| {
            total
                .checked_add(key.as_str().len())?
                .checked_add(value.len())
        });
        if bytes.is_none_or(|bytes| bytes > MAX_HEADER_BYTES)
            || raw_query.is_some_and(|raw| raw.len() > 4096)
        {
            return Err(rejected(
                SearchError::ResourceLimit("public input bytes").into(),
            ));
        }
        let input = Input::Published(Box::new(PublishedInput {
            project_id: project_id.clone(),
            headers: headers.clone(),
            actor: None,
            resolver: Some(resolver),
            request: crate::public_api::validate_transport(raw_query),
        }));
        match self.submit(input, lease).await? {
            Output::Public(result) => Ok(result),
            Output::Member(_) | Output::Published(_) => {
                Err(SearchQueryExecutionError::WorkerFailed)
            }
            #[cfg(test)]
            Output::Probe => Err(SearchQueryExecutionError::WorkerFailed),
        }
    }

    pub(crate) async fn search_member(
        &self,
        project_id: ProjectId,
        actor: Actor,
        query: Result<ValidatedSearchQuery, AppError>,
    ) -> Result<MemberSearchResult, SearchQueryExecutionError> {
        let permit = self.reserve()?;
        let input = Input::Member(Box::new(MemberInput {
            project_id,
            actor,
            query,
        }));
        match self.submit(input, permit).await? {
            Output::Member(result) => Ok(result),
            Output::Published(_) | Output::Public(_) => {
                Err(SearchQueryExecutionError::WorkerFailed)
            }
            #[cfg(test)]
            Output::Probe => Err(SearchQueryExecutionError::WorkerFailed),
        }
    }

    fn reserve(&self) -> Result<QueryLease, SearchQueryExecutionError> {
        if self.stopping.is_cancelled() {
            return Err(rejected(SearchQueryExecutionError::Stopping));
        }
        Arc::clone(&self.slots)
            .try_acquire_owned()
            .map(|permit| {
                let started = Instant::now();
                QueryLease {
                    permit,
                    started,
                    deadline: started + self.config.timeout,
                }
            })
            .map_err(|_| {
                if self.slots.is_closed() {
                    SearchQueryExecutionError::Stopping
                } else {
                    SearchQueryExecutionError::Busy
                }
            })
            .map_err(rejected)
    }

    async fn submit(
        &self,
        input: Input,
        lease: QueryLease,
    ) -> Result<Output, SearchQueryExecutionError> {
        let cancellation = self.stopping.child_token();
        let _cancel_on_drop = cancellation.clone().drop_guard();
        let budget = QueryBudget {
            config: self.config,
            started: lease.started,
            deadline: lease.deadline,
            cancellation,
        };
        let deadline = budget.deadline;
        let (reply, response) = oneshot::channel();
        self.sender
            .try_send(Job {
                input,
                budget,
                reply,
                _permit: lease.permit,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Closed(_) => SearchQueryExecutionError::Stopping,
                mpsc::error::TrySendError::Full(_) => SearchQueryExecutionError::Busy,
            })
            .map_err(rejected)?;
        tokio::select! {
            result = response => {
                if Instant::now() >= deadline { return Err(SearchQueryExecutionError::Deadline); }
                result.map_err(|_| SearchQueryExecutionError::WorkerFailed)?
            },
            () = sleep_until(deadline) => Err(SearchQueryExecutionError::Deadline),
            () = self.stopping.cancelled() => Err(SearchQueryExecutionError::Stopping),
        }
    }
}

fn rejected(error: SearchQueryExecutionError) -> SearchQueryExecutionError {
    let reason = match &error {
        SearchQueryExecutionError::Busy => "capacity",
        SearchQueryExecutionError::Stopping => "stopping",
        _ => "input_limit",
    };
    metrics::counter!("nibleaf_search_query_rejected_total", "reason" => reason).increment(1);
    error
}

impl SearchQueryDriver {
    /// Stop admission on cancellation, cancel queued/in-flight queries, then
    /// join every accepted lifecycle. Running blocking work is never aborted or
    /// detached; its permit survives a caller timeout/disconnection. This is a
    /// cooperative drain, not a hard preemption or exact memory/RSS guarantee.
    /// # Errors
    /// Reports failed materialization cleanup/catalog receipt instead of a
    /// successful drain claim. Accepted work has still been joined first.
    pub async fn run(mut self, shutdown: CancellationToken) -> Result<(), SearchError> {
        let mut tasks = JoinSet::new();
        let mut draining = false;
        loop {
            // Bound even completed task records under sustained rapid traffic.
            while let Some(result) = tasks.try_join_next() {
                observe_join(result);
            }
            if draining && tasks.is_empty() {
                break;
            }
            tokio::select! {
                biased;
                () = shutdown.cancelled(), if !draining => {
                    draining = true;
                    self.service.stopping.cancel();
                    self.service.slots.close();
                    self.receiver.close();
                    while let Ok(job) = self.receiver.try_recv() {
                        if job.reply.send(Err(SearchQueryExecutionError::Stopping)).is_err() {
                            metrics::counter!("nibleaf_search_query_abandoned_total").increment(1);
                        }
                    }
                }
                result = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(result) = result { observe_join(result); }
                }
                job = self.receiver.recv(), if !draining && tasks.len() < self.service.config.max_in_flight => {
                    let Some(job) = job else {
                        shutdown.cancel();
                        continue;
                    };
                    tasks.spawn(run_job(job, self.pool.clone(), self.root.clone(),self.cache.clone()));
                }
            }
        }
        if let Some(cache) = &self.cache {
            cache.close().await?;
        }
        Ok(())
    }
}

fn observe_join(result: Result<(), tokio::task::JoinError>) {
    if let Err(error) = result {
        // Never render a panic payload: a dependency could include private text.
        tracing::error!(
            panicked = error.is_panic(),
            cancelled = error.is_cancelled(),
            "search supervisor task failed"
        );
        metrics::counter!("nibleaf_search_query_worker_failures_total").increment(1);
    }
}

async fn run_job(
    job: Job,
    pool: PgPool,
    root: Option<Arc<PathBuf>>,
    cache: Option<Arc<crate::distribution::ReplicaCache>>,
) {
    let result = execute_job(job.input, pool, root, cache, &job.budget).await;
    let outcome = match &result {
        Ok(_) => "completed",
        Err(SearchQueryExecutionError::Cancelled) => "cancelled",
        Err(SearchQueryExecutionError::Deadline) => "deadline",
        Err(_) => "failed",
    };
    metrics::counter!("nibleaf_search_query_finished_total", "outcome" => outcome).increment(1);
    metrics::histogram!("nibleaf_search_query_lifecycle_seconds", "outcome" => outcome)
        .record(job.budget.started.elapsed().as_secs_f64());
    if job.reply.send(result).is_err() {
        metrics::counter!("nibleaf_search_query_abandoned_total").increment(1);
    }
    // job._permit drops only after joined execution / connection cleanup.
}

async fn execute_job(
    input: Input,
    pool: PgPool,
    root: Option<Arc<PathBuf>>,
    cache: Option<Arc<crate::distribution::ReplicaCache>>,
    budget: &QueryBudget,
) -> Result<Output, SearchQueryExecutionError> {
    budget.check()?;
    #[cfg(test)]
    if let Input::Probe(probe) = input {
        return execute_blocking(Prepared::Probe(probe), root, budget, None).await;
    }
    let mut connection = interrupt(
        async {
            pool.acquire()
                .await
                .map_err(AppError::from)
                .map_err(Into::into)
        },
        budget,
    )
    .await?;
    let prepared = tracked_preparation(prepare(input, &mut connection, budget), budget).await;
    if prepared.is_err() {
        // The server query has completed (including statement_timeout). Close
        // the failed transaction's connection explicitly while retaining the
        // lease rather than returning rollback/protocol cleanup to a new query.
        if !matches!(
            timeout(Duration::from_secs(1), connection.close()).await,
            Ok(Ok(()))
        ) {
            tracing::warn!("search connection could not close cleanly");
            metrics::counter!("nibleaf_search_query_connection_close_failures_total").increment(1);
        }
    } else {
        drop(connection);
    }
    let prepared = prepared?;
    budget.check()?;
    if matches!(
        &prepared,
        Prepared::Published(None) | Prepared::Public(None)
    ) {
        return Ok(if matches!(&prepared, Prepared::Public(None)) {
            Output::Public(None)
        } else {
            Output::Published(None)
        });
    }
    let artifact = match &prepared {
        Prepared::Published(Some(prepared)) | Prepared::Public(Some(prepared)) => {
            prepared.artifact()?
        }
        Prepared::Member(prepared) => prepared.artifact()?,
        _ => None,
    };
    let materialization = match (&cache, artifact) {
        (Some(cache), Some(artifact)) => Some(cache.acquire(&artifact, budget).await?),
        (None, None) => None,
        _ => return Err(SearchError::Distribution("query delivery configuration mismatch").into()),
    };
    let root = materialization
        .as_ref()
        .map_or(root, |entry| Some(Arc::clone(&entry.root)));
    execute_blocking(prepared, root, budget, materialization).await
}

async fn interrupt<T>(
    work: impl Future<Output = Result<T, SearchQueryExecutionError>>,
    budget: &QueryBudget,
) -> Result<T, SearchQueryExecutionError> {
    tokio::select! {
        result = work => result,
        () = budget.cancellation.cancelled() => Err(SearchQueryExecutionError::Cancelled),
        () = sleep_until(budget.deadline) => Err(SearchQueryExecutionError::Deadline),
    }
}

// Do not drop a running SQL future on a response timeout/disconnect. TCP close
// alone is not PostgreSQL query cancellation: releasing capacity then could
// accumulate abandoned server work. statement_timeout bounds normal server
// execution; keep the connection/lease until the query reports completion.
// A network partition can still delay drain: there is no hard I/O-preemption
// promise. The API process manager owns its ultimate termination deadline.
async fn tracked_preparation<T>(
    work: impl Future<Output = Result<T, SearchQueryExecutionError>>,
    budget: &QueryBudget,
) -> Result<T, SearchQueryExecutionError> {
    tokio::pin!(work);
    let interrupted = tokio::select! {
        result = &mut work => return result,
        () = budget.cancellation.cancelled() => SearchQueryExecutionError::Cancelled,
        () = sleep_until(budget.deadline) => SearchQueryExecutionError::Deadline,
    };
    if work.await.is_err() {
        metrics::counter!("nibleaf_search_query_interrupted_preparation_errors_total").increment(1);
    }
    Err(interrupted)
}

async fn prepare(
    input: Input,
    connection: &mut PoolConnection<Postgres>,
    budget: &QueryBudget,
) -> Result<Prepared, SearchQueryExecutionError> {
    match input {
        Input::Published(input) => {
            let public = input.resolver.is_some();
            let prepared = published::prepare_on_connection(
                connection,
                &input.project_id,
                &input.headers,
                input.actor.as_ref(),
                input.request,
                Some(budget),
                input.resolver.as_deref(),
            )
            .await?
            .map(Box::new);
            Ok(if public {
                Prepared::Public(prepared)
            } else {
                Prepared::Published(prepared)
            })
        }
        Input::Member(input) => Ok(Prepared::Member(Box::new(
            member::prepare(connection, *input, budget).await?,
        ))),
        #[cfg(test)]
        Input::Probe(probe) => Ok(Prepared::Probe(probe)),
    }
}

async fn execute_blocking(
    prepared: Prepared,
    root: Option<Arc<PathBuf>>,
    budget: &QueryBudget,
    materialization: Option<Arc<crate::distribution::cache::CacheEntry>>,
) -> Result<Output, SearchQueryExecutionError> {
    let budget = budget.clone();
    // Never race/drop this join handle against the request deadline. The
    // response can end earlier, but the supervisor keeps the slot until join.
    let result = tokio::task::spawn_blocking(move || {
        let completed = (|| {
            budget.check()?;
            match prepared {
                Prepared::Published(None) => Ok(Output::Published(None)),
                Prepared::Published(Some(prepared)) => {
                    let root = root.ok_or(AppError::Unavailable)?;
                    Ok(Output::Published(Some(
                        (*prepared).execute_with_budget(&root, Some(&budget))?,
                    )))
                }
                Prepared::Public(None) => Ok(Output::Public(None)),
                Prepared::Public(Some(prepared)) => {
                    let root = root.ok_or(AppError::Unavailable)?;
                    let result = (*prepared).execute_with_budget(&root, Some(&budget))?;
                    let body = crate::public_api::encode_hits(result.hits, Some(&budget))?;
                    budget.check()?;
                    Ok(Output::Public(Some(body)))
                }
                Prepared::Member(prepared) => {
                    let root = root.ok_or(AppError::Unavailable)?;
                    Ok(Output::Member((*prepared).execute(&root, &budget)?))
                }
                #[cfg(test)]
                Prepared::Probe(probe) => {
                    probe(&budget)?;
                    budget.check()?;
                    Ok(Output::Probe)
                }
            }
        })();
        drop(materialization);
        completed
    })
    .await;
    result.map_err(|error| {
        tracing::error!(
            panicked = error.is_panic(),
            cancelled = error.is_cancelled(),
            "search blocking worker failed"
        );
        metrics::counter!("nibleaf_search_query_worker_failures_total").increment(1);
        SearchQueryExecutionError::WorkerFailed
    })?
}

#[cfg(test)]
mod tests;
