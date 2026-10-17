# Nibleaf Rust/Axum implementation roadmap

> **Current staged-search delta — ADR-0017 (2026-10-16):** the distinct opt-in native public transport, in-budget native/transition read-only identity, selected UTF-16/literal contract, scoped BM25/stable pre-limit ties, bounded owned JSON, aggregate-only metrics, offline rebuild and local load/network/HTTP proofs are implemented. It is **disabled by default**, with no production routing, UI/caller selection or legacy removal. Earlier statements below that transport/scoring are unimplemented describe the prior milestone and are superseded by the [current candidate contract and release boundary](operations/public-search-candidate.md). Selected 200-UTF-16 native validation differs from installed Zod 4.5.4 scalar counting; no exact Unicode parity claim.

**Status date:** 2026-10-16  
**Purpose:** execution roadmap for the incremental Rust replacement. This is a capability and cutover plan, not a claim that the legacy TypeScript application is replaceable today.

## Delivery rules

1. Do not route an existing legacy capability to Rust until its row is marked **Done / routeable** and the listed verification evidence exists.
2. Use an expand-only schema migration before Rust starts writing new persistent state. Never destroy legacy data as a migration shortcut.
3. Tenant-owned mutations require a server-derived project/organization scope, explicit transaction boundary, database constraint, and cross-tenant negative test.
4. External effects are at-least-once. Persist intent in the PostgreSQL outbox and use an idempotent consumer; never claim exactly-once delivery.
5. A partial security workflow is not routeable. In particular, invitations, identity, private readers, API keys, integrations, uploads, and exports must ship as complete security slices.
6. Evidence is append-only: mark an item done only after the documented commands/tests actually pass.

## Current delivery summary

| Status | Meaning |
|---|---|
| **Done / routeable** | Implemented Rust behavior with current migration, API contract, and live PostgreSQL/API-worker evidence. A same-origin adapter may route only this contract after normal rollout approval. |
| **Foundation complete** | Schema, pure logic, or design is in place; no user-facing Rust route is exposed. It is not routeable. |
| **Next** | The next implementation sequence, ordered by dependency/security risk. |
| **Deferred** | Remains legacy-owned until an explicit design and verification slice is approved. |

## Completed implementation

### A. Platform, safety, and operations — **Done / routeable**

| Capability | Delivered behavior | Evidence / current limits |
|---|---|---|
| Modular Rust workspace | Axum API and worker binaries; feature crates for projects, membership, content, publishing, outbox, and shared kernel; dependency graph guard. | Locked workspace compile, tests, Clippy, and `check-architecture.sh` have passed. |
| Configuration and process lifecycle | Typed configuration, secret-redacted debug output, graceful SIGTERM/SIGINT, health endpoints, request body cap, request IDs, structured logging. | Docker image build is still unverified because no container engine is available. |
| API error contract | RFC 9457 responses for feature errors, JSON rejection, unmatched routes, method errors, body limits, timeouts, and panic responses. | E2E validates `404`, `405`/`Allow`, `413`, JSON `422`, and request-ID preservation. |
| Metrics admission and observability | Bearer-protected `/metrics`, local SQLx pool gauges, bounded HTTP labels, outbox backlog/reclaim/delivery metrics and runbook. | Alert thresholds, trace export, and production alert ownership remain open. |
| Legacy-session bridge | Current session/user lookup, expiry/suspension/ban checks, strict bearer or migration-cookie parsing, ambiguity rejection. | Full Better Auth issuance, OTP, OAuth, CSRF, and cookie rotation remain deferred. |

### B. Tenant project lifecycle — **Done / routeable**

| Capability | Delivered behavior | Route(s) |
|---|---|---|
| Project creation | Atomic organization/owner/project/default language/default branch/onboarding creation plus outbox intent. | `POST /v1/projects` |
| Project discovery | Lists projects visible to the authenticated member. | `GET /v1/projects` |
| Onboarding state | Recoverable onboarding run completed by worker. | `GET /v1/projects/{project_id}/onboarding` |
| Tenant fence | Missing and unauthorized project resources share `404`; no client organization scope is trusted. | Applied to all current project routes. |

### C. Content and publication core — **Done / routeable for dashboard draft workflows**

| Capability | Delivered behavior | Route(s) / constraints |
|---|---|---|
| Draft pages | Default-branch page create/list/read/update. | `/v1/projects/{project_id}/pages...` |
| Lost-write prevention | Required `expected_revision`; stale write receives `409`. | Database revision check and live E2E evidence. |
| Immutable publication | Snapshot captured with durable publication event; one active publication per project. | `GET`/`POST /v1/projects/{project_id}/publications` |
| Worker completion | `PENDING → BUILDING → READY` only after the matching native-search generation is promoted; terminal search failure becomes `FAILED`; idempotent redelivery behavior. | Local PostgreSQL outbox lease/fencing, registry, publication-ordering, starter, and ordinary-publication E2E. |

### D. Membership and ownership core — **Done / routeable**

| Capability | Delivered behavior | Route(s) / constraints |
|---|---|---|
| Member list | Project-tenant-scoped membership list. | `GET /v1/projects/{project_id}/members` |
| Role management | Only `admin` / `member` directly assignable. | `PATCH /v1/projects/{project_id}/members/{member_id}` |
| Removal | Admin/owner-bounded removal with last-owner protection. | `DELETE /v1/projects/{project_id}/members/{member_id}` |
| Ownership transfer | Current owner transfers only to existing admin. Project advisory lock and in-transaction reauthorization prevent stale races. | `POST /v1/projects/{project_id}/members/transfer-ownership` |
| Database invariants | Supported-role check, exactly-one-owner migration preflight, partial owner index. | Live concurrent transfer E2E: one `200`, one re-authorized `404`. |

### E. Native search — **Member management implemented; public delivery not routeable**

- Snapshot/source format 2 captures all versions/locales and eligibility/rendering metadata; publication readiness waits for a fenced project-local generation. Current **manifest 3** binds exact engine/analyzer/patch identity and rejects earlier manifests without relabeling them.
- The member-management query and project-bound public admission are verified. [ADR-0015](architecture/decisions/0015-published-search-ceilings-and-ready-selection.md) adds a non-HTTP highest-READY/default resolver, user-selected published ceilings intersected with live restrictions, exact pre-collector language/page/version scope and one read-only MVCC view. A one-shot operation is consumed synchronously; preparation spawns no task. No public route/session/response/cache/analytics contract is installed.
- [ADR-0016](architecture/decisions/0016-supervised-search-query-lifecycle.md) adds the API-driven query service/driver: immediate shared capacity; tracked SQL and joined blocking work retained after caller drop; snapshot/manifest/document/source ceilings; coordinated HTTP/query shutdown/pool close; bounded trace/metric metadata. Real DB/CPU/HTTP/DEBUG privacy proofs pass. Global auth/extraction precedes the budget; hard preemption/drain/RSS, largest-project load and failure drills remain gates.
- [ADR-0014](architecture/decisions/0014-search-runtime-dependency-remediation.md) resolves the active audit warnings using direct Lindera 6.2.0, a checked manifest-only Tantivy 0.26.2/LRU 0.18.5 patch, failure barriers, strict audit and a real Rust 1.88.0 all-target check. Patch maintenance remains required.
- Follow the [source-format](migration/native-search-format2.md) and [runtime](migration/native-search-runtime-upgrade.md) migrations. New reviewed publication, not old-file mutation, is the implemented upgrade path. Public/caller/Orama-Qdrant removal, relevance/BM25 privacy, operations and native erasure remain gated.

## Invitation lifecycle: selected next P0

**Status: Implemented and verified for additive Rust routing; legacy Better Auth invitations remain authoritative until an explicit dashboard adapter/cutover approval.**

### Completed foundations

| Item | Status | Notes |
|---|---|---|
| Legacy behavior analysis | Done | Verified seven-day legacy expiry, normalized lowercase email, Better Auth acceptance, best-effort BullMQ email, Postmark-first/SMTP-fallback provider precedence. |
| Security decision | Done | ADR-0007 requires an additive Rust aggregate; legacy `invitation` table and links remain authoritative until cutover. |
| Schema foundation | Done | `member_invitation` and `member_invitation_delivery` tables with project/tenant FKs, role/status/delivery/idempotency/token-secrecy constraints, pending-recipient unique index, and delivery lease fields. |
| Migration evidence | Done | Real PostgreSQL E2E creates a valid foundation row and proves duplicate pending recipient rejection. |
| Signed token primitive | Done | Versioned HMAC-SHA-256, URL-safe token, expiry/tamper checks, current/previous-key verification. |
| Signing-key configuration | Done | Required Base64 32-byte current key, bounded previous-key ring, redacted config fields. |
| Provider selection rule | Done | Postmark precedence, SMTP fallback, explicit unconfigured result; unit-tested. |

### Completed Rust lifecycle

1. **Provider configuration and transports — Done**
   - Typed legacy-compatible `EMAIL_FROM`, `POSTMARK_API_KEY`, `POSTMARK_MESSAGE_STREAM`, and `SMTP_URL` values, with Nibleaf-prefixed overrides.
   - Postmark HTTPS and SMTP TLS transports, Postmark stream payload, header/mailbox validation, receipt extraction, and permanent/retryable classification.
   - In-process fake Postmark HTTP and SMTP protocol contract tests prove provider selection, request shape, acknowledgement receipt handling, and SMTP multipart exchange.

2. **Invitation aggregate write slice — Done**
   - `POST /v1/projects/{project_id}/member-invitations` requires `Idempotency-Key`, locks the project scope, normalizes recipient email, expires stale pending records, rejects existing members, and writes aggregate/delivery/outbox atomically.
   - Supports `manual` copyable signed URL and `email` durable delivery intent without persisting a raw secret.

3. **Worker delivery consumer — Done**
   - `membership.invitation.delivery.requested` is lease-fenced, mints links only at send time, records provider acknowledgement, and persists retry/dead-letter state.
   - Missing provider configuration is a permanent delivery error and is verified in PostgreSQL; no outbox row is falsely marked delivered.
   - SMTP remains at-least-once if provider acknowledgement is lost; operational recovery must preserve that fact.

4. **Acceptance and cancellation slice — Done**
   - Authenticated recipient-matched acceptance, signing-key rotation validation, membership locking, atomic member insertion/acceptance, safe replay, and project-admin cancellation are implemented.
   - Invalid, expired, cancelled, recipient-mismatched, and unauthorized cases share `404` behavior.

5. **Verification and remaining cutover work — Partial**
   - Real PostgreSQL API/worker E2E covers create/idempotency/mismatch/accept replay/cancel and unconfigured delivery dead-letter behavior.
   - **Still required before legacy traffic changes:** OpenAPI/dashboard adapter documentation, explicit legacy link coexistence/cutover approval, a live provider credential smoke test in an approved non-production account, and dedicated concurrent-acceptance/provider-crash-reclaim scenarios.

## Next P0 after invitations

### 1. Identity transition — **Active P0: full verified Better Auth parity before cutover**

- The evidence inventory is recorded in [identity parity inventory](architecture/identity-parity-inventory.md). The deployed product is passwordless email-OTP plus optional Google, not password sign-in. Under [ADR-0008](architecture/decisions/0008-native-identity-email-otp-cutover.md), Google is intentionally disabled at the first native cutover rather than left legacy-owned; this is a documented user-facing break.
- Implement isolated Rust identity/session/OTP state, secure cookies/CSRF, moderation, organization/admin hooks, account deletion, and compatibility tests before moving auth issuance. The direct native zero-membership and shared-workspace-departure deletion subset is now implemented and tested (ADR-0011); sole-member tenant erasure remains a required retained-store parity slice, not a completed checkbox. ADR-0012 records the prerequisite cross-runtime write fence and durable external-step lifecycle; it is deliberately not exposed before those are implemented and tested.
- Maintain a read-only transition bridge only for bounded legacy-session coexistence; do not dual-issue or cut over `/api/auth` until endpoint/cookie/state matrices pass.

### 2. Public reader and access — **Next after identity decision**

- Serve only `READY` snapshots.
- Port live takedown/access-mode decisions before content rendering.
- Port workspace members, invited readers, audiences, JWT issuer/audience/JWKS rotation, replay protection, and page-scoped grants as one security contract.
- Add anonymous/private cross-access tests before public traffic is routed.

### 3. Publication parity — **Parallel characterization, not routeable public delivery**

- Characterize and port snapshot validation, redirects, link/grammar checks, asset manifest/rendering, rollback, notification follow-up, and remaining publication failure states. The native publication transaction appends generation intent and withholds `READY` until fenced promotion; its local disposable PostgreSQL proof has passed.
- Keep legacy public delivery authoritative. Format-2 scope/corpus/result fields, non-HTTP reader filtering and ADR-0015 highest-READY/default/ceiling/live-restriction composition are verified. Optional actor/session transport, public contract/cache/errors/analytics/relevance/privacy, bounded complete execution/cancellation/shutdown/load verification, dependency/operations gates and full publication-renderer parity must pass before a distinct public route/cutover.

## P1 deferred capability roadmap

| Order | Capability | Required design and verification before implementation |
|---:|---|---|
| P1.1 | Assets and object storage | Authorization before presigning, content/size checks, ownership/key policy, cleanup/retention tests. |
| P1.2 | Exports and schedules | Durable export state machine, rendering bounds, cancellation, retention, timezone/misfire semantics, real scheduler decision. |
| P1.3 | Search and analytics | Offline/pre-cutover relevance characterization, native patch-maintenance/operations/erasure gates, caller migration and Orama/Qdrant removal (no native fallback/shadow mode); separate ClickHouse privacy/reconciliation. |
| P1.4 | API keys and MCP | Hashing, scopes, rotation/revocation, audit, protocol and cross-tenant tests. |
| P1.5 | Git/import/webhooks/integrations | Credential encryption/rotation, SSRF policy, redirects, signatures/replay, provider test servers. |
| P1.6 | Domains and custom edge | Domain ownership verification, DNS/TLS state machine, provider reconciliation, canonical routing. |
| P1.7 | Notifications and platform administration | Delivery preferences, mail/provider semantics, impersonation/support audit, moderation policy. |

## Required exit criteria for every routeable slice

- [ ] Feature inventory and legacy disposition updated.
- [ ] Expand migration and production ordering reviewed.
- [ ] Typed inputs, tenant scope, authorization and database constraints implemented.
- [ ] Transaction/outbox and failure semantics documented.
- [ ] Unit, real PostgreSQL, authorization-negative, concurrency, failure/retry, and API contract tests pass.
- [ ] OpenAPI, compatibility, operations, metrics, alert/recovery notes, and risk register updated.
- [ ] Formatter, locked check, Clippy, tests, architecture guard, PostgreSQL E2E, RSA guard, and audit are run with results recorded.
- [ ] Explicit approval exists before same-origin adapter traffic changes.
