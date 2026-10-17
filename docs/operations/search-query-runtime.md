# Search query runtime: limits, ownership and failure handling

> **Current staged-search delta — ADR-0017 (2026-10-16):** the distinct opt-in native public transport, in-budget native/transition read-only identity, selected UTF-16/literal contract, scoped BM25/stable pre-limit ties, bounded owned JSON, aggregate-only metrics, offline rebuild and local load/network/HTTP proofs are implemented. It is **disabled by default**, with no production routing, UI/caller selection or legacy removal. Earlier statements below that transport/scoring are unimplemented describe the prior milestone and are superseded by the [current candidate contract and release boundary](../operations/public-search-candidate.md). Selected 200-UTF-16 native validation differs from installed Zod 4.5.4 scalar counting; no exact Unicode parity claim.

**Status:** implemented; local source, PostgreSQL 17.11 and real HTTP proofs passed without Docker. Member management uses this runtime; published search has a non-HTTP service facade only. No public reader/site route, traffic switch or production load approval.  
**Decision:** [ADR-0016](../architecture/decisions/0016-supervised-search-query-lifecycle.md)  
**Related:** [generation/storage operations](native-search-generations.md), [published scope policy](../migration/native-public-search-scope.md), [verification](../verification-report.md)

## What is bounded

One feature-owned `SearchQueryService` and its application-driven `SearchQueryDriver` share a semaphore/channel/JoinSet budget. Admission is immediate: no unbounded waiting queue, and no preparing or copying a published request before acquiring a lease. Every admitted preparation/connection cleanup/blocking join stays owned until the actual work ends. A caller deadline or disconnect does **not** make a still-running worker available for reuse.

The existing member route is `GET /v1/projects/{project_id}/search`. The driver starts alongside HTTP, closes admission on shutdown, rejects queued work, cancels in-flight guards, joins work and allows the API to close the pool last. A service constructed without driving its returned driver is inert and must not be used as serving wiring. Do not detach or abort that driver.

Global HTTP authentication/extraction happens **before** this feature budget. In particular, native session authentication uses a write-capable transaction/row lock and cannot simply be moved into the published read-only transaction. Optional anonymous/native/bridge public transport remains a separate gate; a Rust-supplied workspace Actor must be trusted and already authenticated. Reader JWTs are not workspace Actors or direct search credentials.

## API-only configuration

These integer settings are read at API startup. An absent setting uses the default; empty, malformed, non-Unicode, zero or above-maximum values fail startup naming the setting, not its value. They do not configure worker build limits or implicitly constrain trusted offline/direct library calls. Restart the API after an approved change.

| Variable | Default | Accepted range | Meaning |
|---|---:|---:|---|
| `NIBLEAF_SEARCH_QUERY_CONCURRENCY` | 2 | 1–32 | Shared admitted query lifecycles per API process, including preparation and cancelled-but-running work. |
| `NIBLEAF_SEARCH_QUERY_TIMEOUT_MS` | 5000 | 1–30000 ms | Feature response/preparation/execution deadline, excluding preceding global HTTP authentication/extraction. Not a hard work/drain deadline. |
| `NIBLEAF_SEARCH_QUERY_MAX_SNAPSHOT_BYTES` | 16777216 | 1–134217728 bytes | Selected publication's PostgreSQL-rendered snapshot text transferred for full canonical verification. |
| `NIBLEAF_SEARCH_QUERY_MAX_MANIFEST_BYTES` | 16777216 | 1–134217728 bytes | Bounded manifest read, including file growth between metadata check and read. |
| `NIBLEAF_SEARCH_QUERY_MAX_DOCUMENTS` | 50000 | 1–500000 | Whole validated generation count; also the maximum reader page-grant set before overflow refusal. Not merely the hit count or selected version/language count. |
| `NIBLEAF_SEARCH_QUERY_MAX_SOURCE_BYTES` | 67108864 | 1–1073741824 bytes | Cumulative stored-source text validated across the generation. Not an allocator/RSS limit. |

Fixed serving-input ceilings: **16 KiB** total supplied published headers before copying; **64 KiB** projected live controls; **512 UTF-8 bytes** for library query input, **191 UTF-8 bytes** for optional selectors, result limit **1–50**. Management validates its trimmed strings; the library's bound is not a choice of the future public legacy-compatible contract. The old public contract uses 200 JavaScript UTF-16 units and allows blank queries; that decision remains open.

Example defaults (non-secret operational configuration):

```dotenv
NIBLEAF_SEARCH_QUERY_CONCURRENCY=2
NIBLEAF_SEARCH_QUERY_TIMEOUT_MS=5000
NIBLEAF_SEARCH_QUERY_MAX_SNAPSHOT_BYTES=16777216
NIBLEAF_SEARCH_QUERY_MAX_MANIFEST_BYTES=16777216
NIBLEAF_SEARCH_QUERY_MAX_DOCUMENTS=50000
NIBLEAF_SEARCH_QUERY_MAX_SOURCE_BYTES=67108864
```

**These are resource ceilings, not publication content policy.** Raising them cannot widen published eligibility, page grants or the frozen result cap. A smaller requested result count/version scope does not bypass whole-generation integrity validation. Defaults may reject a previously valid oversized member generation with 503; inventory real corpora and size the deployment before rollout. Do not increase all ceilings/concurrency together without measured memory/CPU/I/O headroom. Total replicas multiply pressure on PostgreSQL and the shared volume.

## PostgreSQL and filesystem behavior

- Query preparation acquires a pool connection inside the budget. Tenant authorization/selection use a short **READ ONLY / REPEATABLE READ** transaction. Unauthorized member requests retain concealed 404 precedence over malformed query details once admitted.
- `statement_timeout` is set **LOCAL once per preparation transaction** from the then-remaining budget (fractional milliseconds rounded up, not down). It bounds a normal in-progress statement; it is not a per-request total execution timer and is not reset for every statement. Cooperative checks stop later phases after expiry. It must not leak into a reused connection; the real pool-reuse proof checks that.
- If the caller ends while SQL is in flight, keep awaiting that SQL and keep its connection/permit. The normal server timeout bounds a locked statement; closing TCP alone is not a PostgreSQL cancellation guarantee. Failed preparation explicitly awaits connection close (up to one second after the query reports completion). Healthy committed connections return before CPU execution.
- The snapshot SQL guard withholds oversized text, **after** selecting highest READY. It does not choose an older deployment. PostgreSQL still renders/de-TOASTs full JSONB to measure text size; this ceiling does **not** bound server memory or all server-side CPU.
- Project live controls transfer only the known cap and captured language codes. Unrelated theme/answer/provider blobs are excluded; malformed known cap shapes and an oversized known projection fail closed rather than reset upward. Extremely large locale inventories can hit the fixed projection ceiling.
- Reader pages use DISTINCT plus limit-and-overflow detection; overflow is unavailable, never truncation or unrestricted scope. Only an actual whole-site grant bypasses page-array construction, and it still must pass generation budgets. Admission does not update reader activity.
- After commit, owned blocking execution verifies the canonical snapshot, runtime-bound manifest, registry/document evidence and full stored source. It checks cancellation/deadline between major phases, documents and hits. Immutable readers explicitly use manual reload. No partial successful result is returned on interruption or size failure, and query failures do not change registry state or fall back to legacy/older data.

## Failure contract and monitoring

Member overload, stopping, elapsed budget, worker failure and operational-size rejection use the existing **503 `https://nibleaf.dev/problems/dependency-unavailable`** Problem Details. Ordinary malformed member queries stay 422; membership concealment stays 404 when capacity is available. Existing private/no-store successful member responses and DTOs are unchanged. A public success/error/cache/Retry-After contract is **not** implied.

Scrape the operator-authenticated API `/metrics`:

| Metric | Bounded labels / interpretation |
|---|---|
| `nibleaf_search_query_rejected_total` | `reason=capacity|stopping|input_limit`; rejection before accepted work. |
| `nibleaf_search_query_finished_total` | `outcome=completed|cancelled|deadline|failed` for a job run by the driver. |
| `nibleaf_search_query_lifecycle_seconds` | Same outcomes; from lease acquisition through real work/cleanup, not just how quickly an HTTP response ended. Histogram is not a hard duration guarantee. |
| `nibleaf_search_query_abandoned_total` | Reply receiver gone when the driver sends a completion or drains queued work. |
| `nibleaf_search_query_worker_failures_total` | Unexpected supervisor/blocking join failure; investigate. |
| `nibleaf_search_query_connection_close_failures_total` | Failed/timed-out explicit connection close after preparation. |
| `nibleaf_search_query_interrupted_preparation_errors_total` | Preparation completed with an error after response interest ended; includes expected server timeouts. |

Queued shutdown rejections do not run a job and are not lifecycle-histogram samples. No query-specific active/pool-wait/size-breakdown gauge is claimed. Correlate the bounded lifecycle/outcome counters with existing HTTP 503/latency and local pool metrics, PostgreSQL activity/locks, CPU/RSS, shared-volume latency and shutdown duration; alert thresholds still require measured staging load.

API traces contain only a matched route template or `unmatched` and a standard HTTP method or `OTHER`; no raw URI, query, headers, cookie/token, fallback path or custom-method text. The same metadata bounds HTTP metrics. This corrects `DefaultMakeSpan::include_headers(false)`, which still included URI. The real DEBUG-level HTTP sentinel test covers application/Tower tracing and metric labels. Proxy/browser/access logs, unexpected dependency panic hooks and public analytics still need their own privacy review; this is not a blanket all-systems no-leak claim.

## Shutdown and incident response

1. On SIGTERM/SIGINT or HTTP-server exit, stop routing new traffic and let the API's coordinated HTTP/query drain run. `API HTTP and search work drained` is emitted after both futures complete and the pool is closed.
2. A completed deadline response can coexist with an occupied query slot until SQL/blocking work really ends. Do not reset/replace the semaphore, detach handles, return a connection early or spawn replacement workers to hide this occupancy.
3. A network partition can prevent preparation/connection progress; dictionary, JSON, filesystem and Tantivy calls cannot necessarily be preempted mid-call. There is **no hard shutdown deadline, aggregate RSS guarantee or absolute CPU/I/O bound**. Configure the process manager's ultimate termination grace from staged evidence; forced termination is a distinct incident, not a successful application drain. Read-only query transactions do not carry durable side effects.
4. For sustained capacity 503s, inspect actual lifecycle duration, PostgreSQL locks/pool/network and storage, then reduce pressure or review corpus/configuration. Do not automatically widen publication caps, skip integrity, serve partial hits or choose an older READY generation.
5. For size failures, compare actual selected generation/snapshot against reviewed resource limits without logging private page contents. Raising limits needs headroom proof; republishing content must be conscious. Never edit a final manifest, silently drop documents, or delete retained generations merely to fit the limit.

## Executed proof and remaining release gates

From `rust/`, on a guarded disposable non-Docker database:

```bash
export NIBLEAF_TEST_DATABASE_URL='postgresql://nibleaf_test:…@127.0.0.1:5432/nibleaf_rust_test'
bash scripts/test-postgres-integration.sh
```

The complete harness executed all seven conditional DB functions non-skip and real API/worker/migration flows. Its service proof covers all four data budgets, live-control projection, lock-blocked cancellation/capacity retention/drain, deadline-bounded pool wait, LOCAL non-leakage and bounded reader grants. `test-search-query-runtime.sh` covers all six invalid-zero startup knobs, valid tiny-limit 503, graceful drain, actual DEBUG privacy sentinels and bounded search/HTTP metrics. Ordinary Cargo additionally runs deterministic blocked-CPU, caller-drop/deadline, queued-cancellation, shutdown and typed-error tests.

Still gated: optional public identity transport and reviewed public input/output/cache/error/analytics contracts; BM25 generation-wide ranking/score privacy and language relevance; largest-project/concurrent load/RSS and hard-network/storage/forced-termination drills; integrity-preserving serving metadata/cache design if needed; source/runtime recovery, retained-volume GC/erasure and client/renderer/Orama-Qdrant removal. No production thresholds, public endpoint, cloud CI execution, Docker execution or cutover is claimed by this guide.
