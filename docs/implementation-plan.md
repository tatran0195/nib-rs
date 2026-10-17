# Rust/Axum re-engineering implementation plan

> **Current staged-search delta — ADR-0017 (2026-10-16):** the distinct opt-in native public transport, in-budget native/transition read-only identity, selected UTF-16/literal contract, scoped BM25/stable pre-limit ties, bounded owned JSON, aggregate-only metrics, offline rebuild and local load/network/HTTP proofs are implemented. It is **disabled by default**, with no production routing, UI/caller selection or legacy removal. Earlier statements below that transport/scoring are unimplemented describe the prior milestone and are superseded by the [current candidate contract and release boundary](operations/public-search-candidate.md). Selected 200-UTF-16 native validation differs from installed Zod 4.5.4 scalar counting; no exact Unicode parity claim.

The execution-status source of truth is the detailed [implementation roadmap](implementation-roadmap.md). This document remains the concise phase checklist.

## Completed in this pass

- [x] Clone and inspect Nibleaf and MedBrains at recorded revisions.
- [x] Record current-state evidence, feature inventory, risk register, target architecture and seventeen ADRs.
- [x] Create `rust/` workspace with API/worker binaries and feature-oriented crates.
- [x] Implement configuration, structured tracing, graceful process signals, body limit, request IDs, liveness/readiness.
- [x] Implement legacy-session bridge and server-side project membership fence.
- [x] Implement membership core: tenant-scoped list/role change/remove, single-owner transfer, transactional re-authorization and database owner/role constraints.
- [x] Implement expand-only PostgreSQL migrations: page CAS/path constraints, onboarding/outbox, active deployment guard, and membership ownership invariants.
- [x] Implement project initialization/onboarding with one transaction plus durable event.
- [x] Implement core page create/list/read/update with typed bodies, tenant checks, keyset pagination and required revision.
- [x] Implement immutable publication request and PostgreSQL outbox worker with leases, retry/dead-letter state and idempotent publication completion.
- [x] Implement and verify non-HTTP published search: highest-READY selection, shared immutable defaults, user-selected published ceilings/live restrictions, one read-only MVCC view, consuming synchronous execution and real concurrency/failure/reader proofs.
- [x] Implement and verify shared query admission, tracked SQL/preparation, operational data ceilings, joined blocking shutdown and privacy-safe bounded HTTP/query telemetry (ADR-0016); no hard preemption/RSS/load-readiness claim.
- [x] Run Rust formatter/check/clippy/tests, fresh legacy/Rust migration and a real PostgreSQL API/worker scenario.
- [x] Add migration, API and operational documentation plus CI/architecture-check scaffolding.

## Next coherent slices

### P0: compatibility and security before any broad cutover

1. **Identity transition:** native email-OTP issuance, opaque cookies/CSRF, profile/email change, moderation, support lifecycle, and native self-deletion that preserves shared workspaces are implemented behind the rollout gate. Complete the remaining verified Better Auth compatibility/cutover matrix, dashboard adapter migration, organization hooks, and the ADR-0012 sole-member retained-store deletion workflow (the inspected cross-runtime writer inventory, shared write fence, durable external-step receipts, and reconciliation) before moving broad authority or retiring Better Auth. Google is intentionally disabled at the first native cutover under ADR-0008 rather than retained legacy-only.
2. **Invitation lifecycle:** ADR-0007's additive signed-token and durable-delivery implementation now exposes the verified Rust routes. Both Postmark and SMTP transport contracts, creation/cancellation/acceptance/idempotency, dead-letter behavior, and PostgreSQL API/worker E2E are complete. Legacy Better Auth invitation creation/links remain authoritative until the documented [dashboard adapter and controlled cutover](migration/rust-invitation-cutover.md) is approved.
3. **Public reader delivery:** serve only READY snapshots. A read-only Rust admission primitive now derives live-takedown-concealed `PUBLIC`/`WORKSPACE`/`READERS` scope from project membership or a project-scoped active reader session/audience grant, with real PostgreSQL denial/revocation/zero-grant/cross-project proof. It remains deliberately unrouteable: wire it to a public route only after its transition-session transport and all result filtering semantics are covered.
4. **Publication/search parity:** native snapshot/source-2 capture and manifest-3 build/promotion now freeze every version/locale, excludes hidden/noindex/disabled-language pages, stores result fields/snippets and applies exact reader/version/locale query intersections before result collection. The full local PostgreSQL/API/worker proof passes, including zero-document promotion and later-draft isolation. Follow the explicit format migration; do not infer old visibility or feed native snapshots to a legacy renderer. Active dependency warnings are now resolved under ADR-0014 with direct checked Lindera 6.2 analysis, a provenance-checked Tantivy/LRU manifest patch, warning-denying audit and real Rust 1.88 compilation. Follow the runtime migration; patch maintenance remains required. ADR-0015 implements the non-HTTP published-default/ceiling/live-restriction and latest-READY composition, including the promotion gap and one read-only MVCC view. ADR-0016 now implements the shared shutdown-owned query service/driver, bounded preparation/transfer/validation, tracked SQL and real cancelled-caller/drain/privacy proofs. Next implement optional actor/session transport and reviewed public request/result/error/cache/analytics/relevance/privacy contracts, then verify largest-project/load/hard-failure operations before adding/routing a public endpoint. Native authentication still precedes the feature budget; no hard drain/CPU/I/O or aggregate RSS guarantee is claimed. The current full-snapshot/full-generation integrity proof is not load-readiness evidence. Redirect/link/grammar checks, assets/rendering and rollback parity also remain open.
5. **Observability extension:** the initial bounded Prometheus HTTP/outbox metrics and `/metrics` endpoint are implemented; add OpenTelemetry traces, pool/backlog/publication gauges, alert thresholds and runbook validation before broader traffic.

### P1: operationally sensitive services

6. **Files/exports:** storage authorization/validation, explicit retention and async export jobs; evaluate durable schedule implementation for legacy export schedules.
7. **Search/analytics:** characterize search relevance offline/pre-cutover, verify tenant/erasure/rebuild behavior, and migrate callers before removing both Orama and Qdrant; no native fallback/shadow mode. ClickHouse privacy/reconciliation remains a separate analytics slice.
8. **API keys/MCP:** key hash/rotation/scopes/audit and protocol contract tests.
9. **Git/import/webhooks/integrations:** encrypted credentials, provider retries, SSRF controls, signatures/replay and controlled provider-server tests.
10. **Domains/custom edge:** DNS/TLS/provider state-machine compatibility and safe domain ownership verification.

## Acceptance gates for every later slice

1. Feature inventory behavior and explicit legacy disposition updated.
2. Migration and production ordering designed before code where schema changes.
3. Typed request/input, tenant authorization and DB constraint identified.
4. External side effects use outbox/job semantics when commit coupling is required.
5. Unit + real PostgreSQL integration + cross-tenant negative + concurrency/failure tests added.
6. OpenAPI schema and compatibility note updated.
7. Relevant formatter/check/clippy/test/migration/E2E gates run and actual results recorded.
8. Metrics, logs, runbook, retention and recovery plan included.

## Explicit non-goals of this pass

No destructive production data migration was run. No TypeScript server code was removed. No claim is made that the Rust workspace has completed retained-store tenant erasure, public search/caller cutover, storage, exports, integrations, Git, domains, private readers, public site delivery, or full Better Auth/dashboard authority; native email OTP and platform-admin slices are additive behind their rollout gate.
