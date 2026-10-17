# ADR-0016: supervised search query admission, limits and drain

**Status:** implemented and verified locally for member HTTP and non-HTTP published search; public delivery remains gated  
**Date:** 2026-10-16  
**Related:** [ADR-0013](0013-project-scoped-tantivy-lindera-search.md), [ADR-0015](0015-published-search-ceilings-and-ready-selection.md), [query operations](../../operations/search-query-runtime.md), [public compatibility](../native-public-search-compatibility.md)

## Verified problems

1. The existing member HTTP handler started `spawn_blocking(...).await` per request. A cancelled handler or the outer HTTP timeout could drop the join handle without retaining ownership of the still-running blocking task. HTTP graceful shutdown alone did not establish search-worker drain.
2. ADR-0015 deliberately returned a consuming synchronous operation rather than hiding that lifecycle in another per-request task. Its complete snapshot transfer/hash and full-generation proof still needed resource admission before preparation.
3. SQLx 0.8.6 does not expose a PostgreSQL protocol cancellation-token API. Closing a socket is not proof that an executing/lock-waiting backend stopped. Releasing a query permit at caller cancellation could accumulate abandoned database work even with a small local pool.
4. `tower-http` 0.6.11's actual `DefaultMakeSpan` records `uri` even with `include_headers(false)`. At DEBUG this could include private `q` values. Existing metrics also used arbitrary HTTP method text. Disabling header recording was not sufficient query privacy.

## Decision

### Feature-owned service, application-owned lifecycle

- Add `SearchQueryService` and an inert `SearchQueryDriver`; construction spawns no task. The service accepts only typed member or published operations, not arbitrary production closures, browser generation paths or page allow-lists.
- The API creates one service and injects it into the search router with an Axum extension. `AppState`/core do not depend on the search feature. Both member HTTP and the non-HTTP published facade share one immediate capacity gate.
- Reserve a semaphore lease **before feature database preparation and published input copying**, with no waiting-capacity queue. Only admitted messages enter a channel of that same finite capacity. Bound live/completed JoinSet records as well as running work.
- The driver owns every preparation future and its awaited blocking join handle. A response timeout/disconnection cancels interest but **does not release the worker's lease**. Shutdown closes admission, rejects queued work, signals active guards and joins all accepted lifecycles; it never aborts active query tasks.
- API `run` uses `tokio::join!` for HTTP and the driver, signals cancellation on server exit as well as SIGTERM/SIGINT, then closes the SQLx pool. It does not select-and-drop the driver on an error. Process-manager forced termination remains the final operational deadline.

### Preparation and cancellation

- Member role authorization and active-generation selection now share one short read-only REPEATABLE READ transaction through the same concrete-connection tenant fence. Authorization still precedes malformed-query reporting; active-pointer/member semantics are unchanged. The caller's native/bridge Actor was already authenticated by the API boundary.
- Published preparation preserves ADR-0015's fresh admission, highest READY, same-tenant/project registry evidence and live-restriction MVCC view. Optional actor/session transport remains unimplemented; the library accepts only a trusted Actor when supplied.
- A query deadline includes pool acquisition and feature preparation. Each transaction sets a **LOCAL** PostgreSQL `statement_timeout` once from its then-remaining budget before admission/selection; this is a per-statement guard, not a total preparation timer or a fresh remaining-time setting on every statement. Cancellation checks prevent starting another costly phase after interest expires.
- Once a SQL query has begun, retain its future, connection and capacity until it reports completion, including server-side timeout. Do **not** drop it merely because the HTTP future ends. Await failed-connection close before releasing the lease; healthy committed connections return before filesystem/CPU execution.
- Snapshot transfer uses a SQL-side text-byte gate **after highest-READY selection**, never a filter that selects an older publication. Oversized text is withheld and a typed failure marker is returned. This still renders/de-TOASTs the complete snapshot on PostgreSQL and is not a server-memory bound or a materialized integrity design.
- Live controls project only the known cap (preserving malformed known shapes) and matching captured language codes. Unrelated live theme/provider/answer blobs are not transferred or allowed to reset a valid cap. The immutable full snapshot is still hashed and validated.
- Reader page arrays have a maximum-plus-one bound and fail closed on overflow, never truncate grants. A concrete whole-site grant bypasses the unnecessary page array, not generation validation limits. Reader activity remains unchanged.

### Execution limits and privacy

- Default API query concurrency is **2**, response budget **5,000 ms**, snapshot/manifest byte ceilings **16 MiB each**, generation count **50,000**, and cumulative validated source text **64 MiB**. Public request headers are bounded to **16 KiB** before copying; the live-controls projection is bounded to **64 KiB**. See the operations guide for configuration/ranges and scope.
- Bound manifest reads (including growth after metadata inspection), declared and actual document count before TopDocs allocation, and cumulative stored-source bytes. Check cancellation/deadline before/after major phases and between documents/hits. Expired operations never return partial successful hits.
- Use explicit `ReloadPolicy::Manual` for immutable-generation validation/query readers; no per-query automatic commit-watching/reload lifecycle is needed. This does not change analyzer/index formats or rewrite artifacts.
- Member overload, elapsed budgets and operational-size failures use the existing **503 `dependency-unavailable` Problem Details** contract, not partial results, bad-query 422, generation-state mutation or fallback. Ordinary query validation remains 422 and tenant concealment remains 404 when capacity is available.
- Replace the API default URI span with bounded route-template/method fields, no URI, headers or raw fallback path. Custom HTTP methods become `OTHER` in both traces and metrics. Query service metrics use only bounded reason/outcome labels; no raw query, cookie, actor/project ID or exception payload labels. Unexpected join failures log booleans, not panic text.

## Deliberate limits and compatibility

This is **structural bounded admission/input/validation with cooperative cancellation**, not a hard CPU/network/filesystem deadline or exact RSS guarantee. A Tantivy/Lindera/JSON/filesystem operation cannot necessarily be interrupted mid-call. A network partition can delay SQL completion/drain beyond the response budget; retaining the lease is safer than accepting replacement work while the backend may still run. Largest-project load/RSS, failed-network/storage and process-manager termination drills remain public-route gates.

The query still rehashes the full snapshot and scans the full generation, O(publication size). No integrity cache, materialized serving envelope, process-isolated hard preemption or production throughput claim is introduced. Global authentication and HTTP parsing precede the feature lease and remain subject to the API's separate admission/pool/HTTP limits. Completed HTTP response memory and proxy/browser logs require their own review.

New **operational** limits can make an otherwise valid oversized member generation return 503. Size settings are server configuration, not publish/content policy; inventory and size the deployment before rollout. Raising a query resource limit does not raise ADR-0015's content/cap ceiling. Trusted offline build/verification/direct library APIs are not the serving executor and do not inherit these API limits automatically. No silent artifact rewrite or older-generation serving is permitted.

No new database migration or artifact protocol: snapshot/source **2**, manifest **3**, event **1**, required migration `202610160013`. Cargo.lock changes only local workspace dependency edges to already-resolved Tokio-util/metrics/tracing packages; all **414** package identities, external entries, versions and checksums are unchanged. No third-party source patch or advisory exception is added. Queries remain ephemeral, not outbox jobs.

## Executed evidence

- **109 Cargo cases:** 101 ordinary units, one real store-cache/merge integration and seven conditional database entry functions returning early in ordinary Cargo. Search has 50 units. Five Python integrity cases are counted separately.
- All **seven** DB functions separately executed non-skip in the final local non-Docker PostgreSQL **17.11** migration/API/worker harness.
- Deterministic blocked-CPU tests prove dropped/expired callers keep capacity and shutdown waits for actual worker exit; cancelled queued jobs never run, and shutdown drains them.
- A real PostgreSQL table-lock barrier proves Busy before a second preparation; dropping the caller retains the slot. The driver drains while the blocker remains held, then proves no abandoned lock waiter remains after server timeout. A one-connection exhausted pool proves acquisition is within the response deadline. LOCAL timeout does not leak into the pool.
- Real publication/registry/index queries verify snapshot, manifest, document, source, live-control and reader-grant limits; site-grant behavior and unchanged reader activity; unrelated large live configuration is ignored rather than transferred/reset.
- Real HTTP checks reject each zero-valued startup setting, return the existing 503 envelope for a valid tiny generation limit, and observe graceful drain. With actual application/Tower DEBUG logging, private query/cookie/token/path/custom-method sentinels are absent from logs and metrics.
- Formatter, locked workspace/all-target check, warnings-denied Clippy, architecture/shell/provenance checks, real Rust **1.88.0** all-target compilation and the guarded RSA-only audit pass. Refreshed raw audit still exits 1 solely for inactive RSA; zero advisory warnings.

Cloud CI/container execution, native public HTTP/session/schema/cache/analytics/ranking privacy, legacy Orama/Qdrant removal, full renderer cutover, tenant erasure/recovery and production rollout are not claimed. See the current verification report for logs and local unsigned evidence receipt.
