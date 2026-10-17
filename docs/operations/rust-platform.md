# Rust platform operations runbook

## Configuration

| Variable | Required | Default | Purpose |
|---|---:|---|---|
| `NIBLEAF_DATABASE_URL` | yes (or legacy `POSTGRES_URL`) | — | PostgreSQL connection URL. Do not log it. |
| `NIBLEAF_BIND_ADDR` | no | `0.0.0.0:4311` | API bind address. |
| `NIBLEAF_WORKER_BIND_ADDR` | no | `0.0.0.0:4312` | Worker health bind address. |
| `NIBLEAF_WORKER_CONCURRENCY` | no | `4` | Claimed event batch concurrency, 1–64. |
| `NIBLEAF_SEARCH_INDEX_ROOT` | yes wherever native publication/search is enabled | unset | Persistent **shared** filesystem root for native Tantivy generation builds. If an otherwise-valid native search build event arrives while unset, the worker dead-letters it rather than use ephemeral disk. See [native-search-generations.md](native-search-generations.md). |
| `NIBLEAF_AUTHORITY` | no during transition; native required for final cutover | transition | Exact `transition` or `native`; native requires native identity routes and rejects migration bearer/cookie fallback in private and public workspace authentication. Node search BFF uses the same explicit authority. It does not itself complete the whole-backend/client cutover. |
| `NIBLEAF_NATIVE_API_ORIGIN` | Node BFF only in native authority | unset | Trusted internal HTTP(S) origin, not supplied by browser input; native failure never falls back to legacy search. |
| `NIBLEAF_PUBLIC_SEARCH_ENABLED` | no | false | Explicit staged public-search route. Only exact true/false accepted when present; never an automatic legacy fallback. See [candidate contract/release gates](public-search-candidate.md). |
| `NIBLEAF_SEARCH_QUERY_*` | no; API only | concurrency 2, timeout 5s; data ceilings | Six validated settings bound admitted query preparation and joined execution. See the complete [query runtime table and caveats](search-query-runtime.md). Invalid values fail API startup; worker build settings are independent. |
| `NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY` | yes | — | Base64-encoded 32-byte AEAD delivery key, distinct from invitation HMAC. Required even while native routes are disabled. Rotation/version configuration is in [identity operations](native-identity-otp-delivery.md). |
| `NIBLEAF_METRICS_BEARER_TOKEN` | yes | — | Non-empty bearer token required to scrape either process’s operator-only `/metrics`. Store in the platform secret manager; never log it. |
| `NIBLEAF_INVITATION_SIGNING_KEY` | yes | — | Standard-Base64 encoded 32-byte HMAC key for the ADR-0007 invitation-token primitive. Required at API/worker startup before invitation routes ship; store in the platform secret manager. |
| `NIBLEAF_INVITATION_PREVIOUS_SIGNING_KEYS` | no | empty | Comma-separated prior Standard-Base64 32-byte HMAC keys accepted only to verify unexpired pre-rotation invitation links. Remove after the maximum invitation TTL. |
| `NIBLEAF_APP_URL` | no | `http://localhost:4310` | Base URL used by the invitation delivery worker when minting signed acceptance links. |
| `NIBLEAF_EMAIL_FROM` / `EMAIL_FROM` | no | `nibleaf@localhost` | Sender address for invitation delivery. The Nibleaf-prefixed value overrides the legacy variable. |
| `NIBLEAF_POSTMARK_API_KEY` / `POSTMARK_API_KEY` and `NIBLEAF_POSTMARK_MESSAGE_STREAM` / `POSTMARK_MESSAGE_STREAM` | no | — | Postmark delivery configuration; selected ahead of SMTP when non-empty. Prefixed values override legacy variables. |
| `NIBLEAF_SMTP_URL` / `SMTP_URL` | no | — | SMTP fallback delivery configuration; selected only when Postmark is absent. The prefixed value overrides the legacy variable. |
| `RUST_LOG` | no | `info` | Structured tracing filter. |

API and worker refuse startup if they cannot connect to PostgreSQL or if `NIBLEAF_METRICS_BEARER_TOKEN` or the exact-32-byte decoded `NIBLEAF_INVITATION_SIGNING_KEY` is missing/invalid. The delivery-encryption key is also required. Publication now atomically enqueues native search and publication intent; API/worker require the registry/pointer tables and `202610160013` zero-document constraint, with the remaining platform/identity/membership schema checks performed by the respective binaries. Apply the complete reviewed migration chain, not just the first platform migration.

## Disposable PostgreSQL integration environment

The integration runner uses a separate local database and drops only its `public` schema on every run. It refuses a database name without `nibleaf_rust_test`, and its three seeded test users/session tokens are valid only in that disposable database.

```bash
# One-time local setup; prints an export command. It creates only the
# nibleaf_test role and nibleaf_rust_test database if absent.
./scripts/setup-test-postgres.sh
export NIBLEAF_TEST_DATABASE_URL='postgresql://nibleaf_test:…@127.0.0.1:5432/nibleaf_rust_test'
bash scripts/test-postgres-integration.sh
```

`rust/.env.integration.example` documents the non-secret local shape. Never point this runner at a shared, staging, or production database.

## Build and run

```bash
cd rust
cargo build --locked --release --workspace --bins
NIBLEAF_DATABASE_URL='postgresql://…' NIBLEAF_METRICS_BEARER_TOKEN='from-secret-manager' NIBLEAF_INVITATION_SIGNING_KEY='base64-32-byte-secret' NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY='distinct-base64-32-byte-secret' NIBLEAF_SEARCH_INDEX_ROOT='/var/lib/nibleaf/search' ./target/release/nibleaf-api
NIBLEAF_DATABASE_URL='postgresql://…' NIBLEAF_METRICS_BEARER_TOKEN='from-secret-manager' NIBLEAF_INVITATION_SIGNING_KEY='base64-32-byte-secret' NIBLEAF_IDENTITY_DELIVERY_ENCRYPTION_KEY='distinct-base64-32-byte-secret' NIBLEAF_SEARCH_INDEX_ROOT='/var/lib/nibleaf/search' ./target/release/nibleaf-worker
```

Build one image per binary with the additive Rust container definition (defined, not locally Docker-built):

```bash
docker build --file rust/Dockerfile --target api --tag nibleaf-api:local .
docker build --file rust/Dockerfile --target worker --tag nibleaf-worker:local .
docker build --file rust/Dockerfile --target tools --tag nibleaf-search-tools:local .
```

The root Dockerfile remains the legacy Node deployment path. Do not replace it until the compatibility adapter and the deferred feature cutover criteria are approved.

Production deployment sequence:

1. Backup/verify PostgreSQL restore readiness.
2. Run legacy migrations required by the current production revision.
3. Apply every `rust/migrations/*.sql` file in lexical order as a one-shot migration job after running the documented preflight queries.
4. Deploy worker, verify `GET /health/live` and `/health/ready` on `:4312`.
5. Deploy API, verify the same endpoints on `:4311`.
6. Route only approved Rust vertical slices via a same-origin adapter.

## Health and shutdown

- `GET /health/live`: process is running; it does not prove database reachability.
- `GET /health/ready`: API can query PostgreSQL; worker has passed startup schema validation.
- `SIGTERM`/`SIGINT`: API stops accepting HTTP/query work, cancels guards and jointly drains HTTP plus owned SQL/blocking query work before closing the pool. A caller timeout does not free an executing query slot. No hard drain/network/CPU/filesystem deadline or aggregate RSS guarantee; see [query shutdown operations](search-query-runtime.md#shutdown-and-incident-response). Worker stops new claims and jointly drains health HTTP, dispatch and reconciliation, retaining accepted SQL/provider/blocking jobs through completion and acknowledgement before pool closure. A root signal/error no longer drops the dispatch future/JoinSet. Hard network/CPU/I/O/drain bounds still require process-manager policy; forced termination may leave an unacknowledged 60-second lease for recovery.

## Outbox inspection and recovery

Use a restricted operator role:

```sql
SELECT state, count(*)
FROM outbox_event
GROUP BY state;

SELECT id, tenant_id, event_type, attempts, available_at, last_error, created_at
FROM outbox_event
WHERE state IN ('pending', 'dead_letter')
ORDER BY created_at ASC
LIMIT 100;

SELECT now() - min(created_at) AS oldest_pending_age
FROM outbox_event
WHERE state IN ('pending', 'processing');
```

A `processing` row with an expired `leased_until` is automatically reclaimable. Each reclaim receives a new lease token, so an old claimant cannot acknowledge the newer claim. Do **not** manually mark an active lease delivered. For a reviewed terminal failure, replay using the outbox API/library after recording an operator audit action; the current first slice exposes `replay_dead_letter` at the library level, not yet an administrative HTTP endpoint.

## Publication recovery

```sql
SELECT id, "projectId", version, status, error, "createdAt", "completedAt"
FROM deployment
WHERE status <> 'READY'
ORDER BY "createdAt";
```

- `PENDING`/`BUILDING` plus a pending/expired `publication.requested` event: let worker reclaim it.
- `READY`: safe for a redelivered event; consumer is idempotent.
- `FAILED`: treated as terminal by the first worker; investigate and create a deliberate new publication rather than force-mutating it.

## Metrics and monitoring

Both binaries expose Prometheus text format at `GET /metrics` on their existing health listener (`:4311` API, `:4312` worker by default). Scrapes must provide `Authorization: Bearer $NIBLEAF_METRICS_BEARER_TOKEN`; an absent or invalid token receives RFC 9457 `401` plus `WWW-Authenticate: Bearer`. Treat these as **operator-only** endpoints: do not route them through the public site or dashboard ingress; restrict at the network layer as defense in depth, and store/rotate the bearer token through the platform secret manager.

Implemented metrics are deliberately bounded:

- both binaries: `nibleaf_database_pool_connections{state=total|idle|in_use}` sampled at scrape time; values describe only the local SQLx pool;
- API: `nibleaf_http_requests_total` and `nibleaf_http_request_duration_seconds`, labelled only by `service`, a known HTTP method or `OTHER`, Axum route template or `unmatched`, and status class; API tracing uses the same bounded metadata and no raw URI/headers;
- API query runtime: bounded rejection/outcome counters and actual full-work lifecycle histogram, plus abandonment/worker/connection-close/interrupted-preparation counters; exact names/labels are in the [query guide](search-query-runtime.md#failure-contract-and-monitoring);
- worker: `nibleaf_outbox_claimed_total`, `nibleaf_outbox_reclaimed_total`, `nibleaf_outbox_delivery_total{outcome=...}`, `nibleaf_outbox_delivery_duration_seconds{outcome=...}`, and `nibleaf_outbox_delivery_persist_errors_total`;
- worker scrape-time outbox snapshot: `nibleaf_outbox_snapshot_up`, `nibleaf_outbox_snapshot_errors_total`, `nibleaf_outbox_events{state=pending|processing|dead_letter}`, and `nibleaf_outbox_oldest_open_seconds`. Snapshot failure sets `_up` to zero; do not treat retained gauge values as current while it is zero.

The integration harness asserts that the worker exports the bounded outbox snapshot, reclaimed-lease and pool metrics, and that an API dynamic project-page request is exported with the route template (`/v1/projects/{project_id}/pages`) rather than a raw project ID. It also executes actual DEBUG-level private query/cookie/token/path/custom-method sentinel requests and checks both API logs and metric labels.

Before broader production cutover, add and alert on:

- pool saturation or sustained lack of idle connections, together with PostgreSQL server-side connection pressure;
- `nibleaf_outbox_snapshot_up == 0`, pending/processing oldest age, dead-letter count, retry delivery outcomes and reclaim rate;
- publication state counts and time-to-ready;
- trace correlation/export and concrete alert thresholds.

Never use tenant IDs, project IDs, user IDs, arbitrary URL paths, session tokens or raw errors as metric labels. Send raw identifiers only to access-controlled logs/traces where needed.

## Backup / restore

Back up PostgreSQL before migrations. Restore rehearsal must include legacy tables plus `outbox_event` and `onboarding_run`; restoring only primary content can otherwise lose durable side-effect intent. After restore, inspect pending events and decide whether each external effect is safe to replay. Outbox is at-least-once—never assume an external destination did not already receive an event before the restore point.
