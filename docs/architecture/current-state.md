# Nibleaf current-state assessment

> **Current staged-search delta — ADR-0017 (2026-10-16):** the distinct opt-in native public transport, in-budget native/transition read-only identity, selected UTF-16/literal contract, scoped BM25/stable pre-limit ties, bounded owned JSON, aggregate-only metrics, offline rebuild and local load/network/HTTP proofs are implemented. It is **disabled by default**, with no production routing, UI/caller selection or legacy removal. Earlier statements below that transport/scoring are unimplemented describe the prior milestone and are superseded by the [current candidate contract and release boundary](../operations/public-search-candidate.md). Selected 200-UTF-16 native validation differs from installed Zod 4.5.4 scalar counting; no exact Unicode parity claim.

**Source assessment date:** 2026-10-13  
**Latest implemented/verified update:** 2026-10-16  
**Source examined:** Nibleaf `ca1c9e4545605a5b3ef7bac87b31c1531a3c313a`; MedBrains `1e2992f2ad4a3bb5181c8cfe7017ab9ed60127ca`  
**Status vocabulary:** **verified** means inspected in source or executed locally; **interpretation** is an architectural conclusion; **proposal** is target-state work.

## Executive finding

Nibleaf is already a substantial TypeScript modular monolith for authoring and serving documentation sites. It is not a simple CRUD service. Its core model is **one organization per project/site**, with `Member` rows providing per-site roles. The server is Hono-based, uses Better Auth and Prisma/PostgreSQL, and delegates durable-ish background work to BullMQ/Dragonfly. It also operates ClickHouse analytics, S3-compatible storage, Qdrant search, Git, exports, custom domains, private readers, integrations, and MCP.

The Rust work is therefore an additive, compatibility-conscious replacement track. It does **not** claim every existing capability has been ported. The implemented Rust slices are documented in [feature-inventory.md](feature-inventory.md).

## Evidence inspected

| Area | Verified evidence | Finding |
|---|---|---|
| HTTP composition | `apps/server/src/index.ts`, `server.ts`, `routes.ts`, `modules/**/handlers.ts` | Hono server with `/auth`, `/app`, `/public`, `/admin`, optional MCP, request IDs, error middleware, OpenAPI route and Scalar docs. |
| Product route inventory | `apps/server/src/modules/app/index.ts`, `modules/public/index.ts` and all route files | App routes cover projects, pages, languages, branches, deployments, exports, domains, assets, analytics, comments, OpenAPI, members, settings, private reader access, imports, integrations, Git, AI, API keys, workspace and notifications. Public routes cover site delivery, domains, Git webhooks, invitations, marketing events, assets, reader access and meta. |
| Business implementation | `apps/server/src/actions/{projects,pages,deployments,sites,search,export,git-*.ts}` | Existing route handlers are generally thin, but domain operations are centralized in broad `actions/` files shared by many modules rather than owned by a vertical slice. |
| Identity and tenancy | `packages/auth/src/server.ts`, `apps/server/src/middlewares/guard/index.ts`, `actions/projects.ts` | Better Auth has sessions, OTP email, Google OAuth configuration, organization membership, admin plugin data, suspension checks. Project guards resolve the project-owned organization and return `notFound` for non-members. |
| Schema/migrations | `packages/database/prisma/schema.prisma`; 30 `packages/database/prisma/migrations/*/migration.sql` files | PostgreSQL/Prisma schema has 52 models, enum constraints, FKs, unique constraints and indexes. Important entities include project, branch, language, page, deployment snapshots, readers, exports, integrations, Git state, usage and search diagnostics. |
| Async work | `packages/bullmq/src/**`, `apps/worker/src/{index,processors,reaper}.ts` | BullMQ queues: `publish`, `search`, `email`, `analytics`, `export`, `git`; bounded concurrency and retries configured; Dragonfly/Redis used. Worker includes a stale-deployment reaper and recurring analytics/export schedules. |
| Publishing | `actions/deployments.ts`, `worker/processors/publish.ts`, `actions/sites.ts` | Draft page rows are snapshotted into `Deployment.snapshot`; public site resolution uses READY deployments. Publish includes checks, retries and worker processing. |
| Search and analytics | `packages/{search,qdrant,clickhouse,usage}` | Search has in-process/Qdrant modes and diagnostic runs; ClickHouse is configurable with dual-write/shadow modes and privacy/redaction code. |
| External surfaces | `packages/storage`, Git/importer actions, integrations actions, export worker, domain code | S3-compatible storage, Playwright/PDF export, Git provider operations/webhooks, Cloudflare SaaS option, encrypted integration credential handling and custom domain provisioning are present. |
| Operations | `Dockerfile`, compose files, `docker-entrypoint.sh`, `operations/*.md`, `.github/workflows/ci.yml` | Separate API/worker/app/admin processes; production startup checks Better Auth secret; CI starts PostgreSQL and ClickHouse, applies Prisma migrations, runs type/lint/test/build gates. |
| MedBrains reference | `medbrains/medbrains/Cargo.toml`, `crates/medbrains-{identity,outbox,server-core,authz}`, `docs/architecture/cross-module-events.md` | Rust/Axum workspace with feature crates, shared server-core/authz/outbox crates, migrations and explicit cross-module events. It is a useful boundary reference, but its very large workspace (well over 100 listed members) is not a template for Nibleaf's initial decomposition. |

## Verified execution baseline

| Command | Result | Notes |
|---|---|---|
| `git clone --depth 1` for both repositories | Passed | Exact revisions above. |
| Nibleaf source test/type/lint suite | **Not run** | Sandbox started with Node `v20.20.2`, while Nibleaf declares Node `>=22.18.0`; `pnpm` was absent. It would not be evidence-valid to claim this baseline green. |
| Legacy Nibleaf migrations, in lexical migration order, on local PostgreSQL 17 | Passed | Applied every `packages/database/prisma/migrations/*/migration.sql` to a fresh `nibleaf_rust_test` database. PostgreSQL emitted only benign notices documented in the verification report. |
| Rust migration `rust/migrations/202610130001_rust_platform.sql` | Passed | Verified creation of `outbox_event`, `onboarding_run`, and `page.revision`. |

## Verified native search/publication delta — 2026-10-16

The original TypeScript findings below remain the legacy baseline. Current native evidence is in `rust/crates/search/src/{lib.rs,snapshot.rs,query.rs,registry.rs,runtime.rs,tokenizer.rs,public_admission.rs,publication_scope.rs,published.rs,execution/,api.rs}`, `rust/crates/publishing/src/{lib.rs,snapshot.rs}`, the search PostgreSQL tests, and `rust/scripts/test-search-publication-scope.sh`.

- **Implemented:** lexical-only per-project Tantivy/Lindera generations with SHA-256-derived filesystem scope, source-format-2 documents and immutable format-3 runtime-bound manifests, recomputed stored-source checksums, canonical full-snapshot binding, PostgreSQL lease/reclaim fencing, atomic active-pointer promotion, and publication readiness ordering. Navigation-only/wholly ineligible corpora can promote a valid zero-document generation.
- **Implemented:** native snapshot format 2 captures all branches/versions, every locale and enabled/default state, exact page bindings, visibility/SEO config, description/icon/sidebar fields and authored body. One PostgreSQL input statement freezes these collections; capture rejects foreign-project locale/branch references before deployment/outbox acceptance. Variable values are bounded and frozen into title/summary/body. Later draft edits cannot alter the index.
- **Implemented:** authenticated member-only management search with private/no-store responses, exact version/locale filters, version/result metadata and bounded plain-text snippets. Typed parameter failures use Problem Details; page scope cannot be supplied by a browser.
- **Implemented prerequisite, not HTTP exposure:** read-only `PUBLIC`/`WORKSPACE`/`READERS` admission uses live takedown/member/session/audience facts without activity mutation. Its project-bound scope is consumed by a separate non-HTTP helper that intersects exact version, locale and reader page grants before result collection. Real PostgreSQL proofs include page/site/zero grants, revocation, member override and cross-project misuse denial.
- **Implemented, non-HTTP:** [ADR-0015](decisions/0015-published-search-ceilings-and-ready-selection.md) composes fresh admission, the highest READY publication and live restrictions in one read-only MVCC view, then returns a one-shot synchronous operation. The user selected published ceilings plus live restrictions: exact language-set intersection and the lesser request/published/live cap. Promotion-gap/failed-newer selection, blocked widening, draft-default isolation, snapshot/registry/runtime corruption, real reader scope and a lock-controlled concurrent-change proof pass. The source review corrected the earlier assumption that legacy caps/enabled flags were wholly frozen; they are live-overlaid.
- **Implemented, bounded/cooperative:** [ADR-0016](decisions/0016-supervised-search-query-lifecycle.md) adds a feature-owned query service/driver for member HTTP and future non-HTTP published consumers. Immediate shared capacity, tracked SQL, snapshot/manifest/document/source ceilings, reader-grant overflow refusal, joined CPU lifecycle and coordinated API drain/pool close have real DB/CPU/HTTP proofs. The old Tower span included raw URI despite header suppression; bounded route/method-only tracing now passes an actual DEBUG privacy-sentinel test. Full work-lifecycle metrics distinguish response completion from real resource release.
- **Verified locally:** the full disposable PostgreSQL 17.11 migration/API/worker harness, including frozen multi-version/multi-language capture/results and empty-generation promotion, runs without Docker. Final Rust gate/count/audit details are in [the verification report](../verification-report.md).
- **Implemented:** dependency remediation under [ADR-0014](decisions/0014-search-runtime-dependency-remediation.md): Tantivy 0.26.2 with a checked, sole LRU 0.18.5 manifest patch; direct Lindera 6.2.0 analysis without the wrapper/YAML graph; checked tokenizer failure barriers; strict audit warnings denied under only the existing inactive-RSA exception. All-target compilation also passed on the declared Rust 1.88.0 baseline; resolver 3 prevents selecting known higher-MSRV releases.
- **Explicit breaks/gates:** snapshot 1 and manifest 1/2 generations are refused, not reinterpreted. Snapshot/source format remains 2; current manifest format is 3 with exact engine/analyzer identity. Follow both the [source-format migration](../migration/native-search-format2.md) and [runtime upgrade](../migration/native-search-runtime-upgrade.md); use a new reviewed publication, not an old-manifest edit. Native snapshots are not drop-in legacy site-renderer input. No public native route, legacy caller migration, Orama/Qdrant removal, or production cutover is complete. Optional actor/session transport, public request/result/cache/error/analytics/relevance contracts, hard-resource/largest-project load/failure verification beyond the implemented cooperative service, operations and temporary dependency-patch maintenance remain release gates. The [selected scope policy](../migration/native-public-search-scope.md) is not a public route or legacy parity claim.

See [native-public-search-compatibility.md](native-public-search-compatibility.md) for implemented prerequisites versus remaining public contracts.

## Important flows traced

### Project creation / tenant boundary

1. `POST /app/projects` invokes `createProject` in `actions/projects.ts`.
2. A transaction creates a dedicated `Organization`, default usage plan, owner `Member`, `Project`, add-on rows, default `Language`, and default `Branch`.
3. Project guards resolve the project to its organization then check the current user's membership (`middlewares/guard/index.ts`).
4. Each project is globally slugged and all normal project resources are scoped by `projectId`.

**Interpretation:** the existing “project owns exactly one organization” model is a useful, clear shared-table tenant partition. A new Rust API must never accept a client-supplied organization ID as authority.

### Draft editing / publishing

1. Page handlers call `actions/pages.ts`; page trees are scoped by project, language and branch.
2. `POST /app/projects/:projectId/deployments` creates a `PENDING` deployment and sends a BullMQ publish job.
3. Worker `processors/publish.ts` validates/builds and marks a deployment READY or FAILED.
4. Public site actions select a READY snapshot rather than mutable draft rows.

**Interpretation:** immutable snapshots and reader selection of the last READY deployment are essential behavior to preserve. The original database lacks a page revision column and lacks a unified transactional outbox for all side effects.

### Async work

1. Actions call `createJob` against BullMQ.
2. A Redis producer is deliberately configured fail-fast, and workers use queue retry/stall settings.
3. Durable state is feature-specific for some flows (search index runs, Git operations, exports, usage checkpoints), but enqueueing and relational changes are not universally in one PostgreSQL transaction.

**Interpretation:** BullMQ has useful operational behavior but cannot atomically commit with PostgreSQL. The Rust target uses a PostgreSQL outbox for committed operations that require a side effect.

## Architectural concerns and migration drivers

These are not claims that the original system is unsafe everywhere; they are concrete reasons not to mechanically port it.

1. **Cross-cutting action layer:** `actions/` is useful but lets unrelated route modules reach shared internals. Rust slices should own the use case, validation and SQL together.
2. **Async atomicity is uneven:** direct BullMQ enqueue cannot be one transaction with a project/deployment write. A failed enqueue after a commit can leave required work absent.
3. **In-memory request limiter:** the existing rate limiter explicitly documents that it is per-process, so effective limits scale with replicas. A distributed policy is required for multi-instance security limits.
4. **Page write concurrency:** current page updates use a mutable `updatedAt` but no API-level required compare-and-swap revision. The Rust migration adds `page.revision` and the Rust update requires it.
5. **Application-only page-path uniqueness:** existing sibling uniqueness is application controlled. The Rust migration adds `(projectId, languageId, branchId, path)` uniqueness after a safe duplicate preflight.
6. **Worker recovery varies by feature:** the existing stale-deployment reaper is a periodic corrective mechanism. The target's outbox uses explicit leases, ownership tokens, retry state and terminal dead letters.
7. **Very broad rewrite risk:** Git, readers, exports, search and analytics carry real contracts and must be ported only with controlled test fixtures and data reconciliation; their implemented prerequisites and remaining deferred/cutover work are tracked explicitly rather than removed.

## MedBrains principles adopted—and not adopted

Adopted:

- composable Axum routers per feature;
- small shared kernel plus separate feature/outbox crates;
- direct SQLx where it makes persistence and transaction scope explicit;
- explicit cross-feature durable events;
- compile-time crate dependency boundaries.

Not copied blindly:

- MedBrains' many-crate scale and broad infrastructure surface;
- its domain-specific healthcare authorization/topology assumptions;
- any claim that its reference workspace itself proves Nibleaf production readiness.

See [target-architecture.md](target-architecture.md) and the ADR set for the resulting decisions.
