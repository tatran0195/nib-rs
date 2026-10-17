# Verification report — Rust/Axum re-engineering pass

**Date:** 2026-10-16  
**Legacy Nibleaf revision originally assessed:** `ca1c9e4545605a5b3ef7bac87b31c1531a3c313a`  
**MedBrains revision originally assessed:** `1e2992f2ad4a3bb5181c8cfe7017ab9ed60127ca`

## Environment

- Rust stable installed for this pass: `rustc 1.99.0`, `cargo 1.99.0`.
- Rust **1.88.0** was also installed for the executed declared-baseline compiler check; the default compiler remains 1.99.0.
- PostgreSQL 17 was installed locally solely for disposable integration verification.
- Original Node environment was `v20.20.2`; Nibleaf declares Node `>=22.18.0`, and `pnpm` was unavailable. Original Nibleaf JS/TS gates were therefore **not executed** and are not represented as green.

## Current whole-backend readiness progress (2026-10-16)

**NOT PRODUCTION-READY.** The user confirmed **whole Rust backend**, **multi-host**, initial **5,000 searchable documents/project** benchmarking and **local disposable verification only**. This is broader than the preceding search candidate. See the [active ledger](production-readiness.md), [25-capability matrix](production-capabilities.json), [multi-host ADR-0018](architecture/decisions/0018-multi-host-search-distribution-and-retirement.md) and [authority/shutdown ADR-0019](architecture/decisions/0019-native-authority-contracts-and-owned-worker-drain.md). No production deployment, routing or original production-data change occurred.

### Concrete fixes and implementation

- **App verification unblocked:** Paraglide's installed TypeScript7 declaration path failed. Setup now generates the same JS then emits declarations through explicit `tsconfig.generated.json`/real CLI—not by omitting types. App tests resolve the real workspace RPC source instead of missing generated dist. Full discovery runs each existing file in a fresh supervised process with bounded heap/aggregate RSS; no mock business implementation, dropped assertion, hidden failure or assertion retry is used.
- **Explicit native authority:** exact `NIBLEAF_AUTHORITY=transition|native`. Native requires enabled native identity routes and rejects legacy migration bearer/cookie fallback in both private Actor extraction and public optional workspace identity. Native cookies/anonymous PUBLIC behavior remain. This is an implemented retirement path, not a claim that Better Auth/browser/admin/client retirement is finished.
- **Actual search caller/UI:** native authority plus a trusted origin selects the real server function/query hook/UI. Identity/abort and no-store/credential variation propagate through the BFF; native errors do not call a legacy engine/answer. The UI consumes capability/runtime metadata, hides native answers, suppresses native search click events and old results during revalidation/denial, and honors actual native results despite stale transition metadata. Native transport has an independent timeout and cleans rejected response streams. Other original clients/delivery and native auth UI remain open.
- **Wire contract repair:** member/invitation/session/OTP/support date-time DTOs now explicitly serialize RFC3339 strings rather than relying on time's tuple representation. Signed token/DB timestamp formats are not globally changed. Unit/real HTTP proof matches the already documented schema.
- **Portable replica primitives:** separate artifact envelope **1** binds native generation/project/deployment/source/file identities. Streaming digests, strict flat regular paths/unique keys, byte/count/manifest/window bounds, private new roots/files and full native validation reject unsafe/corrupt payloads. No overwrite/live pointer change. Actual APIs run on separate imported roots; corrupted replica does not affect the other and revocation is fresh on both. Remote object-store publisher/cache/controller and offline retirement are **not** implemented/qualified by these primitives.
- **Worker shutdown ownership repaired:** top-level select/drop and loop try_join/drop could abandon an accepted blocking build despite an awaited inner handle. HTTP, dispatch and reconciliation now cancel and join, drain accepted SQL/provider/blocking work and acknowledgement, then close the pool. Real lock-blocked delivery stays alive after SIGTERM and persists its ack. No hard failed-network/CPU/I/O/drain or external exactly-once claim.
- **Whole-backend blockers are explicit:** read-only discovery currently finds **34 native route declarations**, **164 legacy handler declarations**, **38 legacy source files**, **16 worker sources**, plus dynamic auth/MCP boundaries. Counts are not parity. The capability-based evaluator currently blocks on **23 code/mapping items and 3 environment/SLO/approval items**. Missing routes/providers/clients are not silently retired; only selected Google/dense-answer/raw-term retirements are authorized.

### Executed gates and evidence boundaries

| Gate | Result | Actual scope |
|---|---|---|
| fmt / locked all-target check / warnings-denied Clippy | Passed | Current workspace, operator binary and test targets; no added lint suppression. |
| Ordinary Cargo workspace run | Passed | **124 cases = 115 ordinary units + 1 real cache/merge integration + 8 conditional DB entry functions returning early in this ordinary run.** Search has **59** units; worker ownership adds **2** units. DB functions separately execute non-skip. |
| Real Rust 1.88 all-target compilation | Passed | Current source and newly direct archive edges, not manifest-only MSRV claim. Stable execution/compiler is 1.99.1. |
| Real PostgreSQL17.11 migration/API/worker harness | Passed | All **8 flagged DB functions** non-skip; original native/bridge/tenant/invitation/identity/publication flows; **39** public query assertions. |
| Native-only / independent-root HTTP proof | Passed | **15** assertions plus explicit first-replica corruption/revocation checks; current-cookie works, valid legacy credentials rejected, PUBLIC anonymous retained, RFC3339 dates, selected DB evidence and portable roots verified. Two local processes, not real separate hosts/providers. |
| Worker process ownership proof | Passed | Lock-blocked accepted SQL stays alive after SIGTERM, completes onboarding and persists outbox `delivered` before exit/log; observed ~3.56s after signal. Unit holds actual blocking/async joins through peer error/stop. |
| TCP completion blackhole/resume/external kill | Passed, limited claim | Deadline 503 while drain waits; restored network permits drain. Separate SIGKILL explicitly is **not** successful application drain. |
| Complete app regression | Passed | **89 files / 525 tests**, no failed/pending/todo/unexecuted cases, via the verified fresh-process bounded runner on Node22.23.3/pnpm10.30.3/TypeScript7. This is not the resource-heavy one-process command or a production frontend bundle build. |
| Focused native caller types/tests | Passed | Typed native client/gateway plus actual BFF-handler/capability UI tests. Full native app/auth/client migration is not inferred from these tests. |
| Provenance / architecture / shell/Python | Passed | Existing 374-file sole-LRU vendor patch, five Python provenance cases; three search preflight and four whole-backend discovery/readiness cases separately counted. |
| Refreshed raw audit | Expected exit 1 | Sole inactive `RUSTSEC-2023-0071`, zero advisory warnings; target-path guard and existing RSA-only warning-denying exception pass. No blanket clean-audit claim. |
| Optimized native build | Passed | Actual API/worker/operator `--release --locked --workspace --bins`; binaries hashed. Container definitions add explicit SIGTERM/tools target but Docker/image and hosted CI remain unexecuted. |
| Scoped release recovery/measurement | Passed, limited claim | Actual disposable PostgreSQL dump/restore, table-count match, one selected 5,000-doc artifact on two release APIs, corruption isolation/live revocation, schema-only empty-outbox release-worker ready/drain. Not full real provider/data-location backup coverage or online production recovery. |
| Whole-backend readiness evaluator | Correctly blocked | 25 capability dispositions; no false `ready` from test totals, route discovery, excluded stores or local-only environment. Code and environment are not ready. |

The monolithic legacy app attempt and an over-concurrent verification attempt are retained as historical failures. On the two-GiB host, running Node and a large native link together caused severe memory/I/O pressure; owned Vitest transform caches also accumulated to ~950 MiB in RAM-backed `/tmp`. Identified workspace caches were quarantined to disk, platform/PG files left untouched, and the runner now owns/cleans a private disk-backed TMPDIR per file. Incomplete/terminated attempts are not counted green. Final complete discovery and native gates are serialized/supervised. No application assertion was weakened to hide those resource failures.

### Release benchmark and backup scope

The current `production-release-recovery/result.json` is authoritative for measurements. It records an actual 5,000-document fixture, **20 sequential HTTP queries / 20 at concurrency two**, a burst, two private replica roots and native-only authority. These numbers are local warm fixture measurements, not approved SLOs, cold-disk, aggregate memory or largest production proof. DB dump and artifact plus all three optimized binary hashes are bound in the receipt. Only the explicitly new disposable restore/schema databases were seeded; source recovery reads are non-mutating. Worker startup smoke has **empty outbox and no provider replay**—restored effects are not blindly reissued.

All **414 package identities and external Cargo.lock records** remain unchanged; only the search workspace's direct already-resolved `tar 0.4.46` / `zstd 0.13.3` edges are added. `time` explicitly declares its already-resolved well-known serializer helper feature. Native snapshot/source **2**, manifest **3**, runtime/analyzer identity, event **1** and migration `202610160013` remain; portable envelope **1** is separate, not relabeling. No new DDL or production erasure is introduced.

### Remaining work—this is not just an approval checkbox

Code still needs the complete original feature/client/worker/provider migration: full native identity browser/admin flow and Better Auth retirement; project/settings/theme/navigation/language/version/comment flows; full publication/delivery/renderer/reader-JWT; storage/assets/known-write fences; exports/schedules; domains/edge; analytics/usage/billing; API keys/MCP; integrations/Git/imports/OpenAPI/authoring/translation AI/notifications/addons/workspace/admin; authoritative multi-host artifact publication/cache/catalog/fencing/offline retirement; complete erasure/retention and rollback/reconciliation. See the matrix rather than treating a partial slice as complete.

Production qualification also needs actual multi-host topology/storage/provider/versioning/lock/failure/backup semantics, real workload/SLO approval and reviewed migration/deployment evidence. The user currently supplied **local-only** verification. No provider durability, real customer backup, offline replica erasure, full legacy-engine removal, production authentication cutover, image execution or production-ready attestation is claimed.

Current logs: `/home/user/verification/production-*`, complete app per-file JSON/logs, read-only backend inventory/readiness output and the current unsigned source/artifact receipt. Historical 115-/109-/96-case reports/receipts below are not current source attestations.

---

## Historical opt-in public-search milestone (115 cases, 2026-10-16)

**The consolidated local candidate batch is implemented and verified, not a production cutover.** [ADR-0017](architecture/decisions/0017-public-search-contract-and-ranking-privacy.md) records the user's selections: legacy-shaped wire envelope, scoped BM25, aggregate-only metrics and a staged release. [Candidate operations/release gates](operations/public-search-candidate.md) is the current contract/runbook. The earlier 109-/96-/84-case sections and receipts below are historical.

### Implemented in this batch

- An explicitly opt-in **GET `/v1/public/sites/{project_id}/search`**, absent by default unless `NIBLEAF_PUBLIC_SEARCH_ENABLED=true`; no current Node caller/production routing switch. Typed q/lang/version/limit, strict duplicate/unknown rejection, literal lexical text, blank results, legacy-shaped hit envelope with `runtime=tantivy` and `answer=false`.
- Public optional native/explicit transition identity runs **inside the capacity lease and READ ONLY / REPEATABLE READ view**, through one narrow app-composed port. No search→identity dependency, browser Actor/page-list authority or prefix-wide auth bypass. Native/bridge ambiguity and stale credentials fail closed. Reader JWT exchange remains a separate cookie handoff. Other feature authentication/CSRF is preserved.
- **Security correction:** the old native authenticator checked the operator account but not the originating support session, despite earlier documentation. Shared normal/read-only checks now require a live direct operator session and active correlated support context; stale CSRF rotation and nullable-operator-FK privilege conversion are fenced. Actual PostgreSQL READ ONLY/origin-revocation regressions pass.
- Authorization-scoped BM25 recomputes exact analyzer token totals, document count and term frequency only for permitted version/languages/pages. Stable title/page ties participate **before** TopDocs limit. Adversarial excluded-doc mutations assert exact allowed score bits/order at limits 1/2/50; field priority/Japanese/Chinese fixtures pass. This is not Orama/fuzzy/Arabic/semantic score parity or a timing/resource-pressure side-channel guarantee.
- Public JSON encoding remains inside the joined blocking job/lease with an actual **1 MiB serialized-byte limit**, no truncation/partial response. All public-prefix success/error/routing/identity/timeout responses are private/no-store and credential-varying. Actual DEBUG sentinels and bounded public/search metrics pass; no terms/user/project metric labels or native term-dashboard feed.
- Safe operator `search-ops verify-selected` / `verify` / **new-root-only** rebuild, canonical snapshot/registry/full-source/runtime checks, repeat/existing-root refusal and no live pointer overwrite/deletion. Read-only cutover preflight inventories projects and hashes **104 legacy matching source files**; it explicitly cannot manufacture production approval.
- A tested **unselected** TypeScript server-side caller factory validates input/output/capabilities, forwards only Cookie/Authorization, propagates abort, forbids redirects/shared caching, bounds response bytes and never falls back. Selecting it in `functions/site-search.ts`, session handoff, UI/settings/diagnostics/answer retirement and removal of both old engines remain reviewed release work.

**Unicode characterization correction:** installed Zod **4.5.4**, actually executed on supported Node, counts Unicode scalars for `.max(200)`. The earlier UTF-16 assumption was wrong. The user explicitly selected a **200-UTF-16-unit native policy**, so Rust and TypeScript now enforce it explicitly (up to 600 UTF-8 bytes). It is stricter for supplementary characters and is documented as a compatibility difference, not exact legacy Unicode parity. Management's separate 512-byte/nonblank contract is unchanged.

### Executed final local gates

The sandbox resumed without installed packages. Rust **1.99.1**, real **1.88.0**, PostgreSQL **17.11**, Node **22.23.3**, pnpm **10.30.3** and cargo-audit **0.22.2** were bootstrapped locally, without Docker. Commands ran in `rust/` unless noted.

| Gate | Result | Exact evidence boundary |
|---|---|---|
| fmt check; locked workspace/all-target check; warnings-denied Clippy | Passed | Current Rust source including CLI/test targets; no new lint/advisory suppression. |
| `cargo test --locked --workspace` | Passed | **115 Cargo cases = 106 ordinary units + 1 real cache/merge integration + 8 conditional DB entry functions returning early in ordinary Cargo. Search: 55 units.** All eight DB functions separately ran non-skip below. |
| Actual Rust 1.88.0 locked workspace/all-target check | Passed | Separate MSRV target, not metadata-only assertion. |
| Dependency/provenance, architecture, shell/Python syntax | Passed | 374 official vendor files/sole LRU patch, exact pins/MSRVs; five independent Python integrity cases. |
| Full disposable legacy/Rust migration/API/worker harness | Passed | All **eight** flagged DB functions non-skip, original critical flows and native public helper. |
| Actual public HTTP contract | Passed | **39 assertions:** all access modes, native/bridge/ambiguous/revoked identity, strict/Unicode/blank parameters, grants/zero/revocation/override/no heartbeat, concealed takedown/unpublished, unavailable selected files, actual Busy/deadline and error-cache policy. Busy with a locked native session proves no pre-lease write-capable auth on this exact transport. |
| TCP network-fault / shutdown / process-manager termination drill | Passed, bounded claim | SQL is already lock-waiting before completion is blackholed. Response deadline yields 503 but drain waits. Network resume permits normal drain; separate external SIGKILL is **not** recorded as successful application drain. No hard failed-network deadline claim. |
| Supported-Node candidate adapter and strict TypeScript | Passed | **13 tests**, TypeScript 7 `--ignoreConfig --strict --noEmit` for the candidate/test. No original app caller was silently switched. |
| Executable cutover-preflight tests / local DB inventory | Passed / intentionally blocked | Three independent Python tests; READ ONLY inventory of **15 disposable projects**, 104 matching legacy files. Exit 2 / `ready_for_production=false` is expected, not release approval. |
| Raw refreshed audit | Expected exit 1 | 414 packages / 1,329 advisories; only inactive `RUSTSEC-2023-0071`, zero advisory warnings; no blanket clean-audit claim. |
| Guarded existing RSA-only warnings-denied audit | Passed | No active target RSA path; no added ignore. |
| OpenAPI/CI/docs assertions | Passed | Distinct opt-in schema, literal/UTF-16/response/capability contract, default-disabled routing, configured eight-command DB and Node client CI jobs; **129 relative links across 22 deliverables / 17 ADRs**; not a hosted CI run. |

All **414 Cargo.lock package entries and the lock bytes** remain unchanged from the preceding milestone: `880c1a2bcb6d4cb7e83de7209897ec7233808dcb04a9cc0f583d612c12f6c1ec`. No new third-party package, DDL, snapshot/source/manifest/event protocol or artifact relabeling. Current snapshot/source **2**, manifest **3**, event **1**, required migration `202610160013` remain.

### Measured local load and recovery

The final logged probe used **5,000 documents**, a **5,415,930-byte snapshot**, default concurrency 2/deadline 5s, and real registration/build/promotion/preparation/full-integrity/scoped scoring. Sequential queries measured **811 / 786 / 761 ms**; eight simultaneous calls completed **2** and immediately rejected **6** Busy. Linux **test-process** high-water was **180,568 KiB** (includes build/tests, not a production API/cluster RSS guarantee). It was not an isolated cold-disk or largest-production benchmark.

The same test invoked verified new-root rebuild, refused repeat overwrite, verified selected DB/snapshot/index evidence, removed only its disposable original manifest to prove failure closed, and used rebuilt data with identical durable document identity. It does not restore real customer backups, swap live volumes, fence cross-runtime erasure or implement GC.

### Wider legacy app attempt — not green

A real supported-Node frozen install was executed (install scripts initially disabled), followed by the repository's required i18n generation. The first whole-app attempt failed missing generated Paraglide files; after generation, two full-suite attempts (two workers, then one fork/30s test timeout with bounded V8 heap) were **SIGKILLed**. The first server test's cold import exceeded default 5s on the initial attempt and passed in the sequential run. No whole-app-success, complete legacy baseline or isolated root cause is asserted. Candidate adapter tests/types passed separately. Existing caller/UI/renderer and broader app/release verification remain blockers; unrelated application behavior was not rewritten to force a green result.

### Current evidence and remaining release work

`/home/user/verification/search-batch-*` contains final formatter/check/Clippy/tests/MSRV/provenance/architecture/audit logs, the complete PostgreSQL run, actual public HTTP traces/metrics, network resume/force logs, Node adapter/types and failed broader-app logs, preflight inventory and contract/doc assertions. `public-search-candidate-evidence.json` binds current source/docs/artifacts as a **local unsigned receipt**, not a production attestation.

This batch closes the locally implemented transport/scoped-score/cache/cancellation/recovery/adapter proof work, **not every release condition**. Still required: actual production publication migration; explicit caller/UI/session handoff and renderer compatibility; reviewed removal of Orama/Qdrant/jobs/providers/flags; broader app suite; largest production concurrency/replica/storage/network and process-manager resource policy; real PostgreSQL+volume backups; retained-volume writer/reader fence/GC/tenant erasure and provider lifecycle; production thresholds/ownership and release approval. Sole-tenant erasure remains blocked. Public work has structural/cooperative bounds, not hard CPU/I/O/drain/aggregate RSS; global HTTP parsing/ingress and protected member pre-auth/response boundaries still have their own resource review. No production routing, original production-data mutation, container build or hosted CI execution occurred.

---

## Historical supervised query execution milestone (109 cases, 2026-10-16)

**Implemented and verified locally: bounded/cooperative, shutdown-owned search preparation and execution. The existing member HTTP route uses it; published search has a non-HTTP service facade only. No public route, production rollout or Orama/Qdrant removal.** See [ADR-0016](architecture/decisions/0016-supervised-search-query-lifecycle.md), the [runtime/configuration/incident guide](operations/search-query-runtime.md) and [compatibility matrix](architecture/native-public-search-compatibility.md).

### Current implementation and corrections

- A feature-owned `SearchQueryService`/inert `SearchQueryDriver` reserves shared capacity before feature preparation and published input copying. A bounded channel and bounded/reaped JoinSet retain ownership through SQL, cleanup and joined blocking work. Capacity overload is immediate; cancelled/expired callers do **not** free a still-running worker's slot.
- API bootstrap injects the service into the search slice, jointly drives HTTP and query drain, cancels on signals **and server exit/error**, joins both and closes the pool last. Core does not depend on search; these ephemeral reads are not outbox jobs.
- Member role/active selection use a short READ ONLY / REPEATABLE READ transaction; tenant concealment still precedes malformed-query detail once admitted. Published preparation preserves ADR-0015's fresh admission/highest-READY/live-restriction view. Global native/bridge authentication and HTTP extraction still precede this feature budget; optional public actor/session transport remains unwired.
- **SQL lifetime correction:** SQLx socket close/drop is not PostgreSQL query cancellation. In-flight preparation is awaited after a caller deadline/drop, retaining connection/lease until the server query reports completion. LOCAL `statement_timeout` is set once per transaction from its then-remaining budget; it is a per-statement guard, not a hard total/drain timer. Fractional milliseconds round up so the server does not cancel prematurely just before the client deadline; an exact arithmetic regression test covers this. Failed preparation attempts explicit close before releasing capacity; healthy connections return before CPU work.
- Defaults: concurrency **2**, feature response budget **5s**, snapshot/manifest **16 MiB each**, generation **50,000 documents**, cumulative validated source **64 MiB**. Validated API-only environment knobs and ranges are documented in the runbook. Published headers are bounded to 16 KiB; known live-control projection to 64 KiB. Snapshot SQL guards withhold oversized text, and reader page grants use DISTINCT/limit-plus-one refusal, not truncation; real full-site grants avoid that page-array construction. Unrelated large live configuration is not transferred or allowed to reset the cap.
- Bounded manifest/count/source validation and cooperative checkpoints preserve full integrity checks and reject interruptions without partial hits. Immutable Tantivy readers use manual reload. Existing success DTO/private-no-store and active-pointer management semantics remain; overload/deadline/data ceilings use the existing **503 `dependency-unavailable` Problem Details**, not client-query 422 or registry mutation/fallback.
- **Trace privacy correction:** the actual Tower 0.6.11 default span included URI/query text even with headers disabled. The API now records only bounded matched-route/method metadata, with `unmatched`/`OTHER` fallbacks, shared with HTTP metrics. Real DEBUG HTTP query/cookie/token/path/custom-method sentinels are absent from both API logs and metric labels. Query counters use bounded outcomes/reasons; the lifecycle histogram measures real work ownership, not merely response latency.

Published ceilings/live restrictions and all artifact formats remain unchanged: snapshot/source **2**, manifest **3**, event **1**, required migration `202610160013`; **no new DDL or artifact rewrite**. All **414 package identities and every external package entry/version/checksum** match the pre-query-execution lock; only local workspace dependency edges changed. New Cargo.lock SHA-256: `880c1a2bcb6d4cb7e83de7209897ec7233808dcb04a9cc0f583d612c12f6c1ec`.

### Executed final gates

Commands ran locally from `rust/` unless noted, with no Docker. Cloud CI and container-image execution were **not** performed.

| Gate | Result | Exact scope |
|---|---|---|
| `cargo fmt --all -- --check` | Passed | Current Rust workspace. |
| `cargo check --locked --workspace --all-targets` | Passed | Current workspace and test targets. |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Passed | No lint suppression added for the runtime. |
| `cargo test --locked --workspace` | Passed | **109 Cargo cases = 101 ordinary units + 1 real cache/merge integration + 7 conditional PostgreSQL entry functions returning early in this ordinary run.** Search has **50** units. All seven DB functions were separately executed non-skip below. |
| `python3 scripts/check-search-dependencies.py --self-test` | Passed | Five separate Python integrity cases; all 374 upstream vendor files and sole allowed LRU manifest delta, exact pins/features/MSRVs. Not added to the Cargo count. |
| `bash scripts/check-architecture.sh`; `bash -n scripts/*.sh` | Passed | Dependency direction and shell syntax, including the new finite HTTP helper. |
| `cargo +1.88.0 check --locked --workspace --all-targets` | Passed | Actual declared-baseline compiler and separate target directory, not just manifest metadata. Default compiler is 1.99.0. |
| Full migration/PostgreSQL/API/worker harness | Passed | Local PostgreSQL **17.11**, all seven flagged DB tests non-skip, real legacy/Rust migration chain, existing identity/invitation/onboarding/publication/member/cross-tenant/worker flows plus new query HTTP checks. |
| Query startup/HTTP/DEBUG privacy helper | Passed | All six zero-valued knobs fail startup; a valid tiny document ceiling yields existing 503 envelope; graceful drain observed; actual DEBUG private sentinels absent and bounded search/HTTP metrics present. |
| OpenAPI / CI / documentation assertions | Passed | Management-only schema/ADR-0016/UTF-8 annotation, explicit seven-command harness and DEBUG privacy wiring, PostgreSQL17/MSRV/provenance/sole-RSA CI configuration, **115 relative Markdown links across 20 deliverables** and **16 ADRs**. Local assertions, not hosted CI. |
| `bash scripts/check-inactive-rsa.sh` | Passed | No active target dependency path to RSA. |
| Refreshed raw `cargo audit` and JSON | Expected exit **1** | 1,329 advisories / 414 lock packages; sole inactive `RUSTSEC-2023-0071`; zero **advisory** warnings. No blanket clean-audit claim. |
| `cargo audit --no-fetch --ignore RUSTSEC-2023-0071 --deny warnings` | Passed | Only the existing guarded inactive-RSA exception; no additional advisory ignore. |

### Actual lifecycle and database evidence

The new exact `bounded_query_service_owns_database_and_cpu_lifecycles` test uses real publications, registry promotion, indexes and PostgreSQL—not a mocked preparation:

- successful query respects the frozen cap; snapshot, manifest, whole-generation document and cumulative source limits fail closed;
- large unrelated live theme/answer blobs cannot disable search/reset the valid cap; an oversized known cap is refused;
- an ACCESS EXCLUSIVE registry-table barrier makes the query visibly wait on the server; a second query is Busy before database preparation, and remains Busy after the first caller is dropped;
- shutdown drains while that blocker remains held, relying on real server `statement_timeout`, then verifies no matching lock-waiting backend remains;
- a one-connection exhausted pool honors a 100ms feature response deadline; LOCAL timeout does not contaminate a reused connection;
- distinct page-grant overflow fails unavailable, a real full-site grant bypasses the page array but not generation limits, and reader heartbeat/activity is unchanged.

Deterministic blocked-CPU units separately prove caller-drop/deadline capacity retention and shutdown waiting for actual blocking exit; cancelled queued work never executes, and shutdown closes/drains already queued work. The API metadata unit and typed management-error unit also ran. The existing published same-MVCC-view/promotion-gap/cross-project reader and registry/publication/lease/native-identity proofs all reran non-skip.

The first full HTTP attempt caught a **test expectation error**, not a server regression: the helper expected `/unavailable`; the preserved existing problem type is `/dependency-unavailable`. The corrected exact-envelope assertion and complete harness rerun pass. Earlier focused/development logs remain historical, not the final evidence set.

### Evidence and honest release boundary

Current logs are `/home/user/verification/query-execution-{fmt,check,clippy,workspace-tests,dependency-guard,architecture,msrv-check,inactive-rsa}.log`, `postgres-query-execution-final.log`, `query-execution-http-{privacy,traces}.log`, `query-execution-http-metrics.prom`, `query-execution-audit-{raw,exception}.log`, raw audit JSON, and `query-execution-contract-docs.log`. `query-execution-evidence.json` contains current source/document/artifact SHA-256 inventories and executed-result metadata; it is a **local unsigned receipt**, not a production attestation. It supersedes the earlier 96-/84-case receipts for current-source claims. Source tests use two build jobs, disabled incremental/dev/test debug output and a separate MSRV target to limit sandbox disk usage.

**Still not proved or authorized:** a hard shutdown/CPU/I/O deadline, aggregate RSS, largest-project/concurrent throughput or failed-network/storage/forced-termination behavior. Network loss can stall tracked SQL/drain; JSON/Tantivy/dictionary/filesystem calls are not hard-preemptible. PostgreSQL still renders full JSONB before the transfer-size gate; full snapshot hashing and generation scanning remain **O(publication size)**. Completed HTTP response memory, proxy/access logs and dependency panic hooks have their own resource/privacy boundary. Integrity-preserving cache/materialization and measured staging/load/incident proof remain gates.

No optional public identity transport, public blank/Unicode/response/cache/error/analytics contract, BM25 ranking/score privacy decision, public-site route, renderer/client migration, native retained-volume erasure/GC/recovery, legacy Orama/Qdrant removal or production cutover is claimed. Sole-tenant erasure remains fail-closed. No production data or original TypeScript runtime was modified by this continuation.

---

## Historical published-search composition milestone (96 cases, 2026-10-16)

**Implemented and verified locally: user-selected published ceilings plus live restrictions, shared immutable defaults, and highest-READY selection. This is non-HTTP work; public delivery/cutover remains blocked.** See [ADR-0015](architecture/decisions/0015-published-search-ceilings-and-ready-selection.md), the [policy/migration guide](migration/native-public-search-scope.md) and [compatibility matrix](architecture/native-public-search-compatibility.md).

### What the current source implements

- `prepare_published_search` composes fresh project-bound public/workspace/reader admission, highest-version READY publication selection, matching tenant/project registry evidence and live restrictions in one short **REPEATABLE READ / READ ONLY** transaction. It commits before returning a one-shot operation. Publication capture's existing READ COMMITTED/lock protocol is unchanged.
- `PreparedPublishedSearch::execute` is consuming and synchronous. Preparation spawns no task and receives no filesystem root; neither prepared operations nor library results are serialized public contracts. The future transport must own complete execution/concurrency/cancellation/shutdown. Request Debug omits raw query and unvalidated selector text.
- Selected snapshot/source format **2**, runtime-bound manifest **3**, event **1** and required migration `202610160013` are unchanged. Execution verifies the full snapshot's canonical hash, shared typed scope, current runtime/index and registry document count/digest. Existing compatible artifacts are not relabeled or rewritten.
- Version/defaults and eligibility are publication-bound. Eligible languages are `published-enabled ∩ same-project live-enabled`; the effective cap is the lesser request/default, published and live cap. Live settings cannot raise the ceiling or add excluded locales. A missing/disabled default yields an empty fallback rather than forced serving; malformed known caps fail closed. Live relaxations within the published ceiling remain allowed.
- Language-set, version and live reader-page terms are mandatory **before TopDocs**, including empty sets and field-targeted query syntax. Highest READY is selected **before** the registry join: its retained `superseded` generation can still serve while a newer promoted deployment is incomplete/failed. A broken latest READY publication never falls back to older data or another engine. Member-management active-pointer behavior is unchanged.

**Corrected source characterization:** legacy `getPublished::overlayLiveChrome` overlays live search configuration and language enabled flags; the old matrix understated this. The selected native ceiling/default/malformed-control policy is an explicit compatibility decision, not a claim that legacy controls were already frozen.

### Executed current-source gates

Commands run from `rust/` unless noted. All ran locally; GitHub-hosted CI and a container build were **not** executed.

| Command / gate | Result | Actual scope |
|---|---|---|
| `cargo fmt --all -- --check` | Passed | Entire current workspace; no upstream vendor reformat. |
| `cargo check --locked --workspace --all-targets` | Passed | Stable Rust 1.99.0; current apps/features/test targets. Cargo.lock and third-party source unchanged by this follow-on. |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Passed | No lint suppression added. |
| `cargo test --locked --workspace` | Passed | **96 Cargo cases = 89 ordinary unit + 1 real Tantivy cache/merge integration + 6 environment-gated PostgreSQL entry functions returning early.** All six database functions ran non-skip separately below. The search crate has 39 unit cases. |
| `python3 scripts/check-search-dependencies.py --self-test` | Passed | Provenance/exact pin/feature/MSRV guard, 374 unchanged upstream files with sole authorized LRU manifest delta, and five independent Python integrity cases; not part of the Cargo count. |
| `cargo +1.88.0 check --locked --workspace --all-targets` | Passed | Actual installed declared-minimum Rust 1.88.0, not metadata inference. Separate target directory; no image-build claim. |
| `bash scripts/check-architecture.sh`; `bash -n scripts/*.sh` | Passed | Locked dependency direction and shell syntax. |
| `NIBLEAF_TEST_DATABASE_URL=… bash scripts/test-postgres-integration.sh` | Passed | Final full non-Docker PostgreSQL **17.11** legacy/Rust migration, six non-skip DB functions, invitation/identity/outbox/API/worker harness and frozen member-publication HTTP scenarios. The new composition is explicitly run non-skip through its own environment flag/selected test. No public route is installed. |
| OpenAPI / CI YAML / documentation assertions | Passed | OpenAPI 3.1.1 management shape/no public route and ADR-0015 description; CI PostgreSQL 17/MSRV/integrity/sole RSA exception; explicit non-skip composition harness selection; **82 local Markdown links across 17 current/historical documents**, 15 ADRs. These are local configuration assertions, not a cloud CI run. |
| `bash scripts/check-inactive-rsa.sh` | Passed | RSA still has no Nibleaf workspace target path. |
| Refreshed raw `cargo audit` + JSON assertions | **Expected exit 1** | Only inactive `rsa 0.9.10` / `RUSTSEC-2023-0071`; **zero warnings**, 1,329 advisories and 414 lockfile dependencies. No clean raw-audit claim. |
| `cargo audit --no-fetch --ignore RUSTSEC-2023-0071 --deny warnings` | Passed | Sole existing guarded inactive-RSA exception; no search advisory ignore. |

### Real PostgreSQL proof detail

The new `published_search_uses_one_ready_snapshot_with_live_restrictions` entry creates its own disposable project and drives real registration/claim/build/promotion before composed queries. It verifies published/live/request cap intersection, disabled/new/excluded locale behavior, frozen defaults after live draft changes, promotion-before-READY, promoted-but-failed newer deployment, missing registry evidence, failed generation with valid files, snapshot/digest/runtime tampering and missing files. None may silently select an older READY publication.

A real PostgreSQL ACCESS EXCLUSIVE table lock pauses selection **after admission**. Another connection commits cap/language changes before the lock is released. The in-flight operation sees its consistent old view; a subsequent request sees the new restrictions. This is a controlled interleaving, not a sleep-only race assumption or a production test hook. The test uses a multithreaded runtime and caller-owned `block_in_place` execution, not a spawned request task.

Composed workspace/member override and reader page/zero/revocation behavior also pass; reader last-used time is unchanged. Private/taken-down published sites are concealed even with an index root that cannot be opened. A unit fixture with a higher-scoring denied language/page proves allowed hits survive at limit 1; query syntax cannot remove the mandatory intersections.

Development Clippy checks rejected a needless owned argument, obsolete empty-vector assertion style and an oversized proof helper. These were corrected/refactored before the final suite, without suppressing lint. The initial async convenience prototype was replaced by explicit preparation/consuming execution so new library preparation does not introduce an unowned background-work lifecycle.

Historical milestone evidence: `/home/user/verification/published-search-{fmt,check,clippy,workspace-tests,dependency-guard,architecture,msrv-check,inactive-rsa}.log`, `postgres-published-search-final.log`, `published-search-audit-{raw,exception}.log` and raw audit JSON. `published-search-evidence.json` records the final source/document/log SHA-256 inventory after documentation verification; it is a local receipt, not a signed production attestation. Source tests use two build jobs, disabled incremental/dev/test debug output and a separate MSRV target directory to bound sandbox storage. Previous runtime/format-2 receipts and counts below are historical.

### Remaining gates, not completion claims

No native public HTTP route, optional native/transition actor/session transport, approved public request/result/error/cache/analytics contract, reader JWT lifecycle, full relevance/generation-wide BM25 privacy review, client/renderer adapter, Orama/Qdrant removal, production rollout, safe rebuild/GC/erasure or recovery drill was performed. Public/library query limits are not silently declared compatible: the library remains nonblank/512 bytes while legacy public input permits blank/max 200 JS UTF-16 units.

Current execution transfers/rehashes the full selected snapshot and scans the full generation: **O(publication size)**. Largest-project memory/latency, bounded complete DB/transfer/blocking lifecycle, cancellation/shutdown and integrity-preserving caching/materialization are hard public-route gates, not benchmarked features. A superseded generation can still serve highest READY; future GC must protect it and in-flight readers. The temporary dependency patch and inactive-RSA exception still need maintenance. Original JS/TS, cloud CI, container-build and live-provider limitations remain unchanged.

## Prior native search runtime/dependency milestone (2026-10-16; historical 84-case suite)

This section preserves the earlier runtime-remediation evidence. The current source/count and remaining gates are in the follow-on above.

**The four active search audit warnings are resolved; public search cutover is still blocked.** Current code uses released Tantivy **0.26.2** source with exactly one normalized-manifest edit selecting upstream **LRU 0.18.5**, plus direct **Lindera 6.2.0**. The unused lindera-tantivy/analysis/YAML graph and old `encoding` package are removed. The vendored engine's Rust source, license, version and on-disk format constant are unchanged; all 374 archive files and the sole allowed manifest delta are verified by a provenance/pin guard.

Native publication snapshots and source-document checksums remain **format 2**. Generation manifests are now **format 3**, with mandatory exact compiled engine/index-format, Lindera, dependency-patch and analyzer-profile identity. Open/reuse and registry promotion refuse old manifests or mismatched/missing runtime evidence. This is not an in-place conversion: follow [ADR-0014](architecture/decisions/0014-search-runtime-dependency-remediation.md) and the [runtime migration](migration/native-search-runtime-upgrade.md), using a new reviewed publication. Existing artifacts were not rewritten.

The small native tokenizer adapter retains source byte offsets/positions. A shared first-error latch retains segmentation failures across Tantivy tokenizer clones. The builder explicitly joins merge threads after the synchronous commit barrier, then checks the latch before writing a manifest or renaming a generation; parsing checks it before collecting results. Injected failures prove neither path succeeds with partial postings/zero hits or misclassifies the failure as caller syntax. The real registry test rejects a changed runtime while preserving `building` and an absent active pointer, then promotes the compatible manifest.

### Executed runtime-remediation gates (prior milestone)

Commands run from `rust/` unless noted. These were executed **locally**, not claimed as completed GitHub-hosted jobs.

| Command / gate | Result | Exact scope |
|---|---|---|
| `cargo fmt --all -- --check` | Passed | Current Nibleaf workspace source; upstream vendored source is not reformatted. |
| `cargo check --locked --workspace --all-targets` | Passed | Stable Rust 1.99.0; all apps, features and test targets use the intentional updated lockfile. |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Passed | No warnings or added lint suppression. |
| `cargo test --locked --workspace` | Passed | **84 Cargo cases:** 78 ordinary unit cases, one non-database Tantivy store-cache integration case, and five environment-gated PostgreSQL entry functions returning early. All five were separately run non-skip below. |
| `python3 scripts/check-search-dependencies.py --self-test` | Passed | Exact engine/analyzer/dictionary registry pins; all 374 upstream files and sole LRU edit; declared dependency MSRVs; five independent positive/negative Python integrity cases. These are not added to the Cargo count. |
| `cargo +1.88.0 check --locked --workspace --all-targets` | Passed | Real installed Rust **1.88.0**, matching the declared floor/production-image compiler, not metadata inference. Cargo resolver **3** selected compatible UUID 1.26.1 and ordered-float 5.4.0 instead of releases requiring newer Rust. No container image was built. |
| `bash scripts/check-architecture.sh`; `bash -n scripts/*.sh` | Passed | Locked crate-direction check and shell syntax. |
| `NIBLEAF_TEST_DATABASE_URL=… bash scripts/test-postgres-integration.sh` | Passed | Complete final non-Docker **PostgreSQL 17.11** harness on the guarded disposable DB. Real migration chains, incompatible-runtime promotion refusal, reclaim/reuse/lease fencing, eligible/empty corpora, reader grants and scope, publication ordering and frozen member HTTP queries, plus all prior invitation/identity/outbox/API/worker cases. No conditional database proof is skipped in this run. |
| OpenAPI 3.1.1 / CI YAML / local Markdown-link assertions | Passed | Current management contract/format description; no public route; PostgreSQL 17 CI service; new patch/pin check and Rust 1.88 job, one RSA-only audit exception with `--deny warnings`; local deliverable links. Workflow configuration is verified, not a cloud-run receipt. |
| `bash scripts/check-inactive-rsa.sh` | Passed | No all-target Nibleaf workspace path reaches RSA. |
| Refreshed raw `cargo audit` / JSON assertions | **Expected failure, exit 1** | Exactly one vulnerability: inactive `rsa 0.9.10`, `RUSTSEC-2023-0071`; **zero informational warnings**. cargo-audit 0.22.2 loaded 1,329 advisories. This is not an unqualified clean raw audit. |
| `cargo audit --ignore RUSTSEC-2023-0071 --deny warnings` | Passed without warnings | Only the existing guarded inactive-RSA exception. No search advisory or warning was ignored; CI now denies all warnings. |

The storage integration test creates four committed segments, schedules a real merge and joins it through the same `wait_merging_threads()` lifecycle barrier used by the builder. It then limits the real Tantivy cache to one block, repeatedly retrieves exact bodies across forward/reverse reads, and asserts measured cache hits, misses and eviction. It is an API/storage compatibility proof, **not** a memory-safety proof or performance/load benchmark. Japanese and Chinese recall and byte offsets pass; the Chinese characterization explicitly records `搜索引擎` as one dictionary compound, not an assumed `搜索` token. This does not establish complete relevance parity.

Development checks first exposed the two higher-MSRV lockfile choices, the changed Tantivy `TopDocs` API (now explicit score ordering), an incorrect Chinese fixture assumption and required analyzer registration in the source-tamper test setup. These were corrected before the final gates above; Clippy's function-length rule was met by extracting a focused PostgreSQL proof helper, not suppressing the lint.

Runtime-remediation evidence is retained under `/home/user/verification/`: `search-runtime-{fmt,check,clippy,workspace-tests,dependency-guard,architecture,msrv-check,inactive-rsa}.log`, `postgres-search-runtime-final.log`, `search-runtime-audit-{raw,exception}.log`, and raw audit JSON. `search-runtime-evidence.json` records the verified source/log SHA-256 inventory and command outcomes; it is a local receipt, not a signed production attestation. The earlier format-2 logs remain historical evidence. Compilation/test/harness runs used `CARGO_INCREMENTAL=0`, development/test debug info disabled and two build jobs to bound derived sandbox storage; test coverage is unchanged. The separate Rust 1.88 target directory avoids sharing compiler artifacts.

### Still not completed

At this earlier milestone the public default/cap resolver and active/latest-READY composition were not implemented. ADR-0015 above now supplies their non-HTTP composition. Optional actor/session transport, public route/schema/cache/errors/analytics, full relevance/BM25 privacy review, renderer adapter, Orama/Qdrant removal, bounded execution/load/operations, native rebuild/GC/erasure/recovery, production rollout and restore drills remain unclaimed. The temporary manifest patch must be maintained/replaced with a compatible official release. See [the public compatibility matrix](architecture/native-public-search-compatibility.md). Original JS/TS checks, container build and live-provider limitations below remain unchanged.

## Initial foundation verification (historical baseline)

The table below records the original foundation pass, not the current test count. Later sections preserve identity and format-2 milestone results; the current runtime/dependency result is recorded above. Commands run from `nibleaf/rust` unless noted.

| Command | Result | Evidence / scope |
|---|---|---|
| `cargo fmt --all -- --check` | Passed | Workspace is formatted. |
| `cargo check --locked --workspace --all-targets` | Passed | API, worker, core, outbox and feature crates compile without permitting a lockfile update. |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Passed | Warnings are errors; no global lint suppression added. |
| `cargo test --locked --workspace` | Passed | 26 unit tests passed: IDs, role hierarchy, credential parsing, invitation signing-key validation, signed invitation-token round-trip/tamper/expiry/previous-key rotation, provider precedence/fallback, fake Postmark HTTP contract, fake SMTP protocol contract, invitation email/header validation, invitation input validation, membership ID validation, content validation/cursor, outbox backoff, project slug/name, and snapshot digest/message bounds. The conditional PostgreSQL fence test is exercised by the E2E harness. |
| `bash scripts/check-architecture.sh` | Passed | Cargo metadata verifies core/outbox/features/apps dependency direction. |
| `bash scripts/test-postgres-integration.sh` | Passed | Fresh PostgreSQL 17 DB, real legacy/Rust migrations, invitation lifecycle API/worker E2E (manual/idempotent/mismatch/accept/cancel/dead-letter), same-worker lease fencing, membership/ownership invariants, and bounded Prometheus scrape assertions. |
| Raw `cargo audit` | **Failed (recorded)** | Only RustSec `RUSTSEC-2023-0071` in inactive `rsa 0.9.10`; the advisory reports no fixed version. |
| `bash scripts/check-inactive-rsa.sh` | Passed | `cargo tree --locked --workspace --target all -i rsa` has no Nibleaf workspace target path; the guard fails if one appears. |
| `cargo audit --ignore RUSTSEC-2023-0071` | Passed with documented exception | The CI exception is narrow and documented in workflow and risk R-17; it must be removed if a path becomes reachable or a patch appears. |
| YAML parse of CI workflow and OpenAPI | Passed | PostgreSQL service configuration and the implemented OpenAPI 3.1.1 document parse successfully. |

## Native identity follow-up verification

After the additive native identity, encrypted delivery, account provisioning, native actor admission, rollout-gate, and narrow account-deletion changes, the following **then-current-source** checks were rerun on 2026-10-15:

| Command | Result | Evidence / scope |
|---|---|---|
| `cargo fmt --all -- --check` | Passed | Workspace source was already formatted; the command made no changes. |
| `cargo check --locked --workspace --all-targets` | Passed | Locked API, worker, shared, and feature targets compile. |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Passed | No warnings are admitted. |
| `cargo test --locked --workspace` | Passed | 52 Rust test cases passed, including generic email transport contracts, bounded current-plus-previous identity-delivery key-ring configuration/decryption, native secret/OTP/cookie/account/profile/HTTP parser tests, moderation-reason validation, one-hour support-session bounds, path-scoped operator-resume cookie properties, bounded support-expiry reconciliation configuration, project-isolated Tantivy/Lindera generation/query tests (including Japanese IPADIC and Chinese CC-CEDICT recall), and env-gated PostgreSQL tests in skip mode. |
| `bash scripts/check-architecture.sh` | Passed | The final crate dependency graph still meets the declared boundaries. |
| `bash scripts/test-postgres-integration.sh` | Passed | Applied the actual legacy plus Rust migration chains, including the system-actor expiry-reconciler, immutable account-deletion-audit, and shared-workspace-departure-audit migrations; deliberately downgraded the deletion-audit action constraint to prove the API refuses startup before that latest migration is restored; started API and worker with a current version-2 identity-delivery key plus a retained version-1 key, exercised both disabled and enabled native identity gates, provider refusal, opaque OTP request/verify/session/sign-out including fake-handle rejection, self-profile mutation/validation and CSRF fence, exact-Origin CSRF recovery rotation with stale-token rejection, deterministic active-workspace fallback plus membership-fenced selection, native actor/feature CSRF fences, exact/missing/untrusted-Origin and CSRF native account-deletion rejection, zero-membership and shared-workspace deletion with post-commit cookie clearing/immutable evidence/pending-provisioning cancellation, atomic last-owner transfer, mixed shared/sole membership rollback, sole-member and worker-owned-provisioning conflicts, stale-session rejection, native platform moderation including target-bound support termination, bounded native support impersonation (start/session attribution, emergency revoke, scoped-resume recovery, normal end, moderator suspension, scheduled system expiry, immutable facts and reconciliation metrics), generic UUID-shaped email-change request/current-proof continuations for malformed, taken, rate-limited, and fake paths (including a persisted current-mailbox decoy for a taken target), HTTP current/new proof transitions driven only by returned continuation handles, replay rejection, encrypted delivery intent, native rich-starter provisioning with exact content hashes, automatic publication/re-delivery fencing, cross-tenant event rejection, and prior E2E flows. |

The native HTTP and actor-admission paths are **not exposed by default**: the disposable harness explicitly started both `NIBLEAF_NATIVE_IDENTITY_ENABLED=false` and `true` API variants. The disabled variant returned `404` for native identity routes and did not admit a valid native cookie as a feature actor; the enabled/unconfigured variant returned `503` before acknowledging an email-change proof request. `false` remains the production-safe default and leaves Better Auth transition behavior authoritative.

Native account deletion was exercised at its zero-membership and shared-workspace boundaries. The harness seeded valid opaque native sessions and proved missing CSRF returns `401`, originless/untrusted-origin requests return `403`, neither rejection deletes/audits/cancels anything, and a valid direct request returns `204` with `Cache-Control: no-store` and a cleared native cookie only after commit. It proved user/session removal, immutable audit evidence, and a pending `identity.account.provision_requested` transition to `dead_letter`. It also deleted a shared-workspace owner, proved the workspace survived with its other member promoted to owner, retained exactly one member, and recorded the distinct departure audit action. A sole-member account returned `409` without clearing its cookie or changing user/member rows; a leased `processing` provisioning intent also returned `409`; and a revoked session returned `401` with no deletion fact. The direct PostgreSQL lifecycle test independently covers zero/shared departure, existing-owner preservation, successor transfer, malformed historical ownership fail-closed behavior, a mixed shared/sole-membership rollback with no partial departure, concurrent final-owner/member departures without a PostgreSQL deadlock or ownerless tenant, sole-member, worker-ownership, stale-session, and impersonation paths. It also creates test-only delayed triggers for two lock proofs: an all-or-nothing provisioner-style organization/member transaction is raced against the actual deletion function, so its member FK insert is lock-fenced and its foundation transaction rolls back without an orphan organization; separately, a shared-owner departure holds the locked organization while a successor is promoted, and a concurrent member insert times out rather than changing the planned membership classification. This is not verification of **sole-member tenant** retained-store erasure; that remains intentionally unavailable.

Native platform moderation was also exercised against the migrated PostgreSQL schema. A native non-admin received `403`; a platform administrator could not suspend themself; suspension set the legacy-compatible ban/suspension fields, invalidated a real native OTP session, deleted a real bridge session, and wrote an operator/session/target audit fact. The harness separately created a live native support session for that target, then suspended the target and proved the same moderation transaction wrote the support lifecycle/audit `revoked` fact with the moderator’s user/session attribution, rejected the target cookie, and still allowed scoped operator recovery. Repeating suspension returned `changed=false` and appended a second moderation audit fact; a direct SQL `UPDATE` was rejected by the immutable-audit trigger. The harness then restored the account without restoring its session, rejected an unsafe takedown reason, proved a taken-down project rejects publication with `403`, restored the project, and verified the complete native moderation audit sequence.

Native support impersonation was exercised against the same migrated schema. A platform admin could not impersonate themself; a successful start issued a distinct target session whose session response carried the originating operator ID and whose audit trail started with `started`. A support session for an admin target was still forbidden from invoking platform moderation. The harness emergency-revoked that context using the operator session, verified target-cookie rejection, then called the explicit-Origin scoped-resume stop route to restore the original operator cookie despite the revoked target cookie. A second context was normally stopped and a third used generic native sign-out, producing `started,revoked,started,ended,started,ended` immutable audit evidence. It then created a live support context for a regular target and suspended that target through native moderation, proving the moderator-attributed `revoked` fact and target-cookie rejection occur atomically while scoped operator recovery remains available. Finally it started another context, forced it due in PostgreSQL, and observed the live worker terminalize it once as `expired` under `system:identity-impersonation-expirer` with a null actor session, revoke the target cookie, preserve scoped operator recovery, and export reconciliation metrics; a direct audit `UPDATE` was rejected. This covers native API/worker behavior only: Better Auth/dashboard redirect and banner migration and historical legacy-event reconciliation remain open.

## Historical native search format-2 and scope milestone (2026-10-16)

At this earlier milestone, native publication capture and search shared an explicit **format-2** contract. The capture/source rules remain current; the manifest and dependency state below are superseded by the runtime-3 verification above. Capture reads project plus every branch/language/page collection at one PostgreSQL statement snapshot after the project share lock, stores collision-safe version slugs/default flags, enabled-language state, hidden/SEO config and description/icon/sidebar/body fields, and freezes bounded variable substitution. Cross-project language **and** branch references are rejected before deployment/outbox acceptance. Later draft, language-toggle or branch-name changes cannot modify that immutable corpus.

Typed extraction excludes `GROUP`, hidden, noindex and disabled-language pages; missing/ambiguous/malformed version/language/visibility facts fail closed. Valid zero-document generations still build, validate, promote and activate. Format-2 document identity includes exact version/locale and rendering/source fields; opening/promotion require the closed schema and recompute stored-source checksums instead of merely trusting copied digest fields. Unsupported format-1 input persists terminal failure; old manifest formats are refused. This is an intentional [format migration](migration/native-search-format2.md), not relabeling historical data.

`GET /v1/projects/{project_id}/search` remains **member-only management search**, with private/no-store responses, exact optional version/locale filters, additive version/description/icon/snippet fields and typed Problem Details for bad parameters. Separate read-only `PUBLIC`/`WORKSPACE`/`READERS` admission is now project-bound. Non-HTTP `search_admitted_generation` intersects exact version, locale and server-derived reader page terms **before** Tantivy result collection; real-grant/limit-1 proof prevents both grant bypass and post-limit loss. Admission and query do not mutate legacy reader activity. **No native public search HTTP route, full public default/session/cache/analytics contract, legacy renderer adapter or Orama/Qdrant cutover is claimed.**

Final executed gates:

| Command | Result | Evidence / scope |
|---|---|---|
| `cargo fmt --all -- --check` | Passed | Final workspace source is formatted. |
| `cargo check --locked --workspace --all-targets` | Passed | API/worker/feature and integration targets compile against the updated lockfile; the intentional dependency addition is `pulldown-cmark` for deterministic text/snippet projection. |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Passed | Warnings denied; no new lint suppression. |
| `cargo test --locked --workspace` | Passed | **75 test cases** reported by Cargo: 70 ordinary unit cases plus five environment-gated PostgreSQL functions returning early in this command. The latter are separately executed non-skip by the real harness. Added format-2 eligibility/ambiguity/checksum/source-tamper tests, exact version/regional-language/page-before-limit filters, query-syntax bypass attempts, description/headings recall, Unicode snippets, and version-slug/variable tests. |
| `bash -n scripts/*.sh` | Passed | Includes the extracted publication/search scope HTTP proof. |
| `bash scripts/check-architecture.sh` | Passed | Feature/application dependency direction remains valid; publishing does not import search implementation. |
| `NIBLEAF_TEST_DATABASE_URL=… bash scripts/test-postgres-integration.sh` | Passed | Complete final harness on local non-Docker **PostgreSQL 17.11**, guarded disposable `nibleaf_rust_test` only: real legacy/Rust migrations, stale-schema startup refusal, registry reclaim/reuse/stale-token fencing, mixed eligible versioned corpus and zero-document promotion, unsupported old-format terminal failure, public/workspace/reader admission plus real scope query, publication ordering, native starter, frozen multi-version/locale member HTTP capture/query, cross-project locale/branch refusal with zero accepted deployment/search intents, description/icon/headings/snippets, rejected client page scope, absent public route, and all prior invitation/identity/outbox/API/worker proofs. |
| OpenAPI 3.1.1 and CI YAML assertions | Passed | Management route includes exact `version`/`language` and additive required rendering/version fields; no public route added. PostgreSQL CI service remains version 17. |
| `bash scripts/check-inactive-rsa.sh` | Passed | No Nibleaf workspace target path reaches `rsa`; the exception is still inactive. |
| `cargo audit` (cargo-audit 0.22.2, refreshed advisory DB) | **Failed as recorded** | One hard vulnerability: inactive `rsa 0.9.10`, `RUSTSEC-2023-0071`, no fixed upgrade. **Also four allowed warnings on active search dependency paths**, detailed below. |
| `cargo audit --ignore RUSTSEC-2023-0071` | Passed **with warnings**, not clean | Only the existing single-purpose RSA exception is used; no warning/advisory ignore was added for search. Active warnings remain a production release gate. |

### Historical dependency warnings that motivated ADR-0014

`cargo tree --locked --workspace --target all -i …` establishes these active paths:

- `lru 0.12.5 → tantivy 0.25.0 → nibleaf-search`: `RUSTSEC-2026-0253` (`LruCache::pop` panic-safety/use-after-free warning) and `RUSTSEC-2026-0002` (`IterMut` aliasing/Stacked Borrows warning).
- `encoding 0.2.33 → lindera-dictionary 2.3.4` and its embedded dictionary/build paths: unmaintained warning `RUSTSEC-2021-0153`.
- `unsafe-libyaml 0.2.11 → serde_yaml_ng 0.10.0 → lindera 2.3.4`: unmaintained warning `RUSTSEC-2024-0449`.

Reachability is **not** a claim that a particular unsafe method was exercised or exploited. It does mean these are not inactive lockfile-only packages like RSA. At that milestone a compatible upgrade or reviewed upstream remediation was required (risk R-23); source tests and an audit that allowed warnings did not close the gate. The current ADR-0014 verification above resolves those four warnings, while retaining patch maintenance and public/operations gates.

Stored format-2 bodies and retained staging/superseded indexes are also tenant data. No native purge/GC/rebuild/recovery drill or sole-tenant erasure is implemented; the erasure/fence inventory now names these native writers/files without re-including the user-excluded ClickHouse/Qdrant/Git/BullMQ scope. Generation-wide BM25 statistics versus reader-local legacy indexes also need ranking/privacy/public-score review before any restricted-reader HTTP exposure. Remaining public/default/session/selection/cache/analytics/relevance/operations and renderer compatibility gates are in [the public compatibility matrix](architecture/native-public-search-compatibility.md).

The historical format-2 run logs are retained in `/home/user/verification/format2-{fmt,check,clippy,workspace-tests,architecture}.log`, `postgres-search-format2-final.log`, `inactive-rsa-format2.log`, and `audit-format2-{raw,exception}.log`. These are local verification evidence, not production rollout/drill receipts. `CARGO_INCREMENTAL=0` was used for compilation/test/harness runs to bound this sandbox's derived build storage; coverage is unchanged.

## Follow-on reliability and dependency review

A post-pass review found that an outbox lease token must identify a **claim**, not only a worker process: otherwise a stale delivery from a worker could acknowledge that same worker's later reclaim. `claim_batch` now uses PostgreSQL `gen_random_uuid()` for every claimed row while retaining `worker_id` only as diagnostics. The E2E harness includes a database-backed same-worker reclaim test that proves the lease token changes before restoring the crash-recovery scenario. The current source was rechecked on 2026-10-16 with formatting, locked workspace/all-target compilation, Clippy with warnings denied, unit tests, architecture-boundary verification, and the local non-Docker PostgreSQL integration script. Invitation transport contracts additionally run against faked Postmark HTTP and SMTP servers; no check claims exactly-once external delivery or uses a live provider credential.

The contemporaneous advisory refresh identified four fixable `rustls 0.23.45` advisories (`RUSTSEC-2026-0346`, `-0347`, `-0348`, and `-0349`). The lockfile was updated to `rustls 0.23.46`, then the complete locked source/database quality suite and audit were rerun. That refresh left the inactive `rsa` advisory as the only hard vulnerability. The subsequent format-2 audit also recorded four active search-dependency warnings; ADR-0014 and the current verification above later resolved those warnings without extending the RSA exception.

A bounded Prometheus recorder is now installed in each binary and exposed on its operator-only `/metrics` health listener. Both endpoints require a distinct non-empty bearer token at startup and return RFC 9457 `401` plus `WWW-Authenticate: Bearer` to unauthenticated scrapes. Both binaries sample local SQLx pool total/idle/in-use gauges at scrape time. API metrics use method, Axum route template and status-class labels; worker metrics cover outbox claims, reclaims, delivery classifications/duration, acknowledgement persistence errors, a scrape-time bounded backlog snapshot with pending/processing/dead-letter counts, oldest-open age and snapshot health, plus native support-expiry reconciliation success/error counters, duration histograms, error count, and expired-context count. The PostgreSQL E2E asserts metrics authentication, the snapshot/pool/reclaim/reconciliation exports, and that an actual dynamic project route exports its template, not a raw project ID.

## Real PostgreSQL migration/E2E result

`test-postgres-integration.sh` did the following successfully on a database named `nibleaf_rust_test`:

1. Dropped and recreated only the disposable `public` schema.
2. Applied every existing Nibleaf Prisma `migration.sql` file in lexical order.
3. Applied every Rust migration in lexical order, including the member-role/one-owner invariants, transactional system-actor support-expiry reconciler schema, immutable account-deletion audit schema, and its constrained shared-workspace departure action expansion.
4. Seeded disposable legacy/native user and session fixtures, including owner, invitation recipient, unrelated actor, moderation/support targets and an expired owner session.
5. Started current Rust API and verified absent and incorrect metrics credentials receive RFC 9457 `401` (the absent case includes `WWW-Authenticate`), while a distinct bearer token permits the operator scrape.
6. Verified the session bridge rejects the expired session and a duplicated migration cookie, and accepts the explicit migration cookie for the current session.
7. Verified framework-owned unmatched route (`404`), method mismatch (`405` plus `Allow`) and over-2-MiB body (`413`) responses are RFC 9457 documents; the route error retained `X-Request-Id`.
8. Authenticated a legacy session and created a project; inserted one valid manual-mode invitation foundation row and proved its duplicate-pending-recipient database constraint rejects a second pending invitation.
9. Ran `postgres_lease_fencing` against the new `project.initialized` event: the first pending claim was not marked reclaimed; the same worker's expired second claim was marked reclaimed and had a different lease token.
10. Restored the explicit simulated-crash state (`attempts=1`, expired lease), then started the current Rust worker; it reclaimed the event and produced `delivered:2`, exposed the bounded backlog/reclaim/pool metrics, then completed onboarding.
11. Created a manual invitation through the API, proved the same idempotency key replays the same aggregate, rejected acceptance by a logged-in non-recipient with `404`, accepted it as the matching user, replayed that acceptance safely, and proved exactly one membership row exists.
12. Created and cancelled a second manual invitation, proving the terminal cancellation state. Created an email-mode invitation without provider configuration and proved both its delivery receipt and outbox event enter `dead_letter`; it is not misreported as sent.
13. Created a page; stale optimistic update returned HTTP `409`. The unrelated user's project onboarding request returned tenant-safe `404`.
14. Reused the accepted invitation's admin membership, listed members, exercised role bounds, rejected direct `owner` assignment with an RFC 9457 `422` validation document, raced two ownership-transfer requests, observed exactly one `200` and one re-authorized `404`, verified one owner in SQL, and rejected a former owner's later transfer with `404`.
15. Rejected foreign locale/branch capture before acceptance, captured format-2 publication with multiple versions/locales and eligibility metadata, edited later drafts, promoted the correct frozen corpus, and verified exact member HTTP search with immutable result fields and no public route.
16. Suspended the owner in PostgreSQL and verified the still-unexpired session immediately received `401`, proving the per-request suspension fence.
17. Consumed a native new-account provisioning event and asserted its `Documentation` project has the exact seven-page starter topology, a `READY` version-one `Publish starter site` deployment, hashes for all six starter content pages, a duplicate `starter.publication.requested` delivery that did not create another deployment, and a forged cross-tenant starter event that dead-lettered without publishing.

This verifies the implemented slices' migration, tenant fence, page concurrency, legacy-session expiry/suspension/credential-admission fences, outbox lease recovery, metric-admission boundary and publication state transition. It does **not** verify deferred legacy capabilities.

## Baseline limitations and failures

- Existing TypeScript `pnpm` test/type/lint/build gates were not run because the available Node version was below the repository's declared floor and `pnpm` was unavailable. This is a tooling limitation, not a pass.
- The four active search dependency warnings are resolved in the current graph. The raw audit still fails for inactive RSA; only its existing exception is accepted. Temporary upstream-patch maintenance and the remaining public/operations/erasure gates prohibit a blanket security or production-cutover claim.
- No load/performance benchmark has been run. The first Rust slice has not been sized for production traffic.
- No credentials for live third-party providers were used. Invitation mail transports were verified against in-process fake Postmark HTTP and SMTP protocol servers; storage, Git, Cloudflare, Qdrant, ClickHouse, OpenRouter, and all other deferred providers were not exercised.
- SQLx dynamic queries are validated by real PostgreSQL migration/E2E, but compile-time query metadata/offline verification is not configured yet.
- `rust/Dockerfile` was syntax-reviewed and documents additive API/worker images, but an image build was not executed in this sandbox because no container engine was available. CI currently verifies source and PostgreSQL behavior, not image construction.

## Reproduction

```bash
cd nibleaf/rust
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
bash scripts/check-architecture.sh
python3 scripts/check-search-dependencies.py --self-test
rustup toolchain install 1.88.0 --profile minimal
cargo +1.88.0 check --locked --workspace --all-targets
NIBLEAF_TEST_DATABASE_URL='postgresql://nibleaf_test:…@127.0.0.1:5432/nibleaf_rust_test' \
  bash scripts/test-postgres-integration.sh
cargo audit                         # still fails solely on inactive rsa; zero informational warnings
bash scripts/check-inactive-rsa.sh  # fails if rsa reaches a workspace target
cargo audit --ignore RUSTSEC-2023-0071 --deny warnings # passes; only the existing inactive RSA exception
bash -n scripts/*.sh
```
