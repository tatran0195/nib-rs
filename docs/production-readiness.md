# Whole-backend production readiness ledger

**User scope confirmed 2026-10-16:** the entire Rust backend cutover, **multi-host**, an initial **5,000 searchable documents/project** benchmark profile, and **local disposable verification only**. No production deployment/data changes authorized. Real traffic SLOs and provider/environment qualification are not invented from the benchmark profile.

## Current truth

The 115-case native search candidate receipt is a completed *historical milestone*, not whole-backend readiness. The original app/server/worker and provider integrations remain authoritative for many capabilities. A source-file reference scan cannot establish feature parity or approve production. Readiness is **blocked** while any retained feature, migration, security, reliability or environment gate below remains incomplete.

## Work queue (active)

1. **Legacy application verification — local gate resolved:** required i18n declarations/RPC source resolution and owned disk-backed temporary directories are fixed. The complete current 89-file/525-case discovery passes in supervised bounded processes without removing assertions. A full production bundle/real environment remains separately unqualified.
2. **Actual full capability/route inventory:** reconcile the older feature inventory against current Rust and legacy routes, schemas/migrations, worker processors and consumers; distinguish implemented code, executable proof, intentional retirement and remaining migration.
3. **Caller/UI/session/delivery:** wire a reviewed native path with no automatic engine fallback, fresh native/explicit authenticated transition credential handoff, `answer:false`, selected UTF-16/literal contracts and safe rendering. Full publication/site adapter and remaining reader/JWT/admin APIs are part of whole-backend scope.
4. **Multi-host architecture and implementation:** replace the unqualified shared-local-filesystem assumption with a concrete immutable artifact/distribution/fencing model. No NFS/local-lock equivalence, offline replica purge, TTL-as-quiescence or external exactly-once claim. PostgreSQL remains authoritative; source, ready publication, artifact integrity and tenant boundaries must remain explicit.
5. **Performance:** reproduce 5,000-document size/concurrency/latency/resource benchmarks; remove avoidable whole-corpus work on each query only with a reviewed immutable verification/cache model and fresh authorization. Propose, do not fabricate approval of, supported limits/SLOs.
6. **Retained data/erasure:** implement durable write fences/known-resource scopes, provider-contract behavior, ambiguous-write failure handling, replica/cache retirement and retry/reconciliation before native sole-tenant deletion can be enabled. Provider-managed domains/external Git lifecycle remain fail closed under standing policy. The user-excluded retained stores must not be claimed erased.
7. **Full remaining native slices:** project update/theme/delete, content navigation/languages/versions/comments, readers/JWT, assets/storage, exports/schedules, domains, API keys/MCP and other retained inventory capabilities need either complete native slices or explicitly approved disposition before whole-backend cutover. Existing invitation/authentication coverage must be re-read from code, not stale inventory text.
8. **Operations/release:** coordinated DB + artifact backup/restore; least privilege/secrets/permission and multi-host failure/replica drills; migration/rollback/reconciliation; old-engine/provider/job/config removal; native authentication authority/client migration and Better Auth retirement; CI and repeatable packaging; production environment qualification.

## Evidence rules

- Record each gate as executed-pass, executed-fail, implemented-not-executed or blocked-on-input/environment, with current source/artifact hashes.
- An expected local preflight refusal is not a completed release gate; no hard-coded `ready=false/true` substitutes for validation.
- No production claims based only on local mocks, filesystem copies, Dockerfile presence, configured CI, fixture load or successful unit counts.
- No production mutations, live migrations, provider deletes or routing changes. Any future destructive rehearsal requires an explicitly isolated disposable target.
- The original monolithic app command exceeded this sandbox's resources; the complete current per-file regression is now green (89 files / 525 cases), not a subset or a hidden retry. The initial memory observation was investigated: unsupported TypeScript7 declaration emission and a missing RPC build export were fixed, while owned Vitest transform caches filling RAM-backed `/tmp` were quarantined. The runner uses a private cleaned disk-backed TMPDIR; resource-terminated attempts remain explicit failures.

This ledger is in progress and not a production-ready attestation.

## Executed progress in the current batch

- Fixed i18n declaration emission for installed TypeScript 7 using explicit `tsconfig.generated.json` and the real compiler CLI, not by dropping declarations. App test resolver uses the real workspace RPC source instead of missing build output.
- Complete app suite: **89 files / 525 tests**, no failed/pending/todo/unexecuted cases, fresh supervised per-file processes with 512 MiB V8 heap, an aggregate RSS guard and private cleaned disk-backed temporary directories. The standard one-process suite exceeded this two-GiB sandbox's resources; the bounded complete discovery run, not a subset, is the recorded gate.
- API `NIBLEAF_AUTHORITY=native` refuses legacy migration credentials rather than retaining indefinite Better Auth fallback; native cookie and anonymous public semantics remain. Node native search authority now uses the actual BFF/UI path, forwards request identity/cancellation, marks BFF responses private, disables native answers/click analytics and avoids rendering old native results during revalidation/denial. Original transition remains explicitly selected until reviewed release.
- Fixed documented HTTP date-time fields to explicit RFC3339 rather than tuple serialization; token claims/DB timestamp representations remain unchanged.
- Added strict portable artifact envelope 1/import/export with private new roots/files and full native/source checks; current manifest 3/source 2 unchanged. Two local API processes use independent imported roots and shared live DB revocation. This is not an object-store controller or complete multi-host retirement model.
- Fixed a newly found worker root shutdown ownership gap: both top-level and dispatch/reconciliation futures now cancel and join rather than selecting-and-dropping accepted work. Unit blocking/async ownership tests and a real lock-blocked SQL delivery/SIGTERM/ack process proof are executed before claiming that gate passed.
- Optimized API/worker/operator builds, a fresh disposable DB restore and scoped archive/import/HTTP benchmark rehearsal are recorded separately from real provider/backup qualification. See the current verification report for final numbers; no SLO or production approval is invented.
- Read-only route/worker discovery and `check-whole-backend-readiness.py` derive blockers from **25 capability dispositions**, missing contract mapping, unapproved SLO and local-only environment. Current code/environment are not ready. Only the three explicit user retirements are allowed; unrelated analytics/billing/AI/Git capabilities are not silently removed.

**Still active:** authoritative multi-host artifact transport/cache/fencing/retirement; full native identity browser/admin client and Better Auth removal; full delivery/renderer and the original feature/API/worker/provider inventory; retained-store erasure and cross-runtime writes; complete backup/migration/replay/rollback; provider and real multi-host qualification. No production traffic/data was changed. Do not read this progress list as completion of those remaining capabilities.
