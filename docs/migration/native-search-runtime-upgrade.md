# Native search runtime upgrade: manifest 3, snapshot/source format 2

**Status:** implemented and verified locally; no production rollout or data rewrite executed  
**Date:** 2026-10-16  
**Related:** [ADR-0014](../architecture/decisions/0014-search-runtime-dependency-remediation.md), [snapshot/source format 2](native-search-format2.md), [operations](../operations/native-search-generations.md)

## What changes

The native stack now uses released Tantivy 0.26.2 source with a reviewed **manifest-only** LRU 0.18.5 dependency patch, plus Lindera 6.2.0 through a small programmatic adapter. The unused lindera-tantivy/analysis/YAML graph is removed. This resolves the four previously active search audit warnings without adding an advisory exception.

Source-equivalent pages can still produce different terms after an analyzer/dictionary upgrade. The generation manifest therefore advances to **3** and adds mandatory runtime evidence:

```json
{
  "format_version": 3,
  "runtime": {
    "tantivy": "tantivy v0.26.2, index_format v7",
    "tantivy_dependency_patch": "nibleaf-lru-0.18.5-v1",
    "lindera": "6.2.0",
    "analyzer_revision": 1
  }
}
```

This is a fragment, not a complete manifest. Existing project/deployment/generation IDs, document counts, checksum maps and aggregate source digest are still mandatory. Both open/reuse validation and registry promotion compare the runtime **exactly**, not as a semver compatibility range. The schema is closed; missing runtime metadata is not defaulted.

The reviewed analyzer revision is normal Lindera mode, embedded IPADIC/CC-CEDICT from the pinned 6.2.0 release family, no user dictionary or character/token filters, omitted whitespace, disabled unknown-word ladder, and native adapter revision one. A future profile/configuration change requires compatibility review and a new revision. Japanese/Chinese fixtures passed; identical historical relevance is not promised.

| Contract | Current version / behavior |
|---|---|
| Native publication snapshot | **2**, unchanged by this runtime upgrade |
| Source-document checksum / stored result schema | **2**, unchanged |
| Generation manifest | **3**, mandatory runtime binding |
| Tantivy's own index format | **7**, bound through the compiled engine identity; its ability to open old files is not analyzer-compatibility evidence |
| Search/publication outbox event | **1**, unchanged |
| PostgreSQL registry tables / required migration | Unchanged; migrations still required through `202610160013` |
| Management HTTP request/result shape | Unchanged from the format-2 milestone |
| Native public HTTP route | Still absent |

## Existing artifacts fail closed

- A manifest-1/2 generation is **not** a compatible current generation. An admitted management request receives `503`, not old postings queried with a new tokenizer or a legacy fallback.
- A format-3 generation with missing or mismatched engine/analyzer/patch metadata is also refused. There is no automatic relabeling or inferred version.
- Native snapshot format **2** remains valid current build input. Native snapshot format **1** still fails permanently as documented in the earlier migration.
- An existing final directory that fails the new runtime gate is not reused or overwritten. One generation per deployment remains a registry constraint. This pass adds no search-only rebuild/backfill command that bypasses it.

## Coordinated rollout order

1. Inventory native snapshot and manifest versions with read-only tooling; back up PostgreSQL and the shared index volume together. A production inventory/restore drill has not been performed here.
2. Review every native publication's consumer. Native snapshot 2 is still not a legacy `SiteSnapshot` renderer adapter, and the legacy public search path remains authoritative until its separate cutover.
3. Quiesce native publication admission and review/drain work before switching the API and worker cohort. Do not run mixed manifest-2/3 builders/readers as an assumed zero-downtime upgrade. Plan admitted management-search unavailability while old active generations are incompatible.
4. Stage the exact reviewed dependency graph: run the vendored integrity/pin guard, strict audit with the single inactive-RSA exception, locked Rust source suite, declared Rust 1.88 compiler check, and real PostgreSQL/API/worker harness. Inspect the temporary patch's provenance and plan its eventual upstream replacement.
5. Create a **new reviewed publication** from the intended draft state. This is an explicit content decision: current drafts may differ from the last published snapshot. The new deployment/generation UUIDs preserve history and produce manifest 3 without rewriting an older directory.
6. Confirm snapshot 2, manifest 3/current runtime identity, eligible document count, exact version/locale/reader scope, ready registry state and matching READY deployment before relying on the management query. The existing atomic promotion/readiness protocol still applies.
7. Retain old generation/snapshot evidence. Do not edit manifests, reset ready rows, repoint active rows manually, rename old directories into new identities, or purge files as a migration shortcut. Safe rebuild/GC, receipted erasure and recovery drills remain separately gated work.

No production migration, traffic switch, destructive data operation, provider call or rollback drill was executed. The local test harness resets only its deliberately disposable `nibleaf_rust_test` schema.

## Executed verification

- Mandatory/closed runtime identity, old-manifest refusal and changed-runtime open failure.
- Explicit merge-thread join before immutable-path rename, with a real four-segment merge/cache-eviction contract test. Injected tokenizer failure is rejected at the post-commit/pre-manifest barrier and the post-parse/pre-query barrier; no panic or successful partial/empty response.
- Japanese/Chinese recall, exact locale/version/page filters, immutable source checksums and valid zero-document generations.
- Real PostgreSQL refusal of an incompatible runtime promotion, leaving the registry `building` with no active pointer, followed by successful current-runtime promotion.
- Full local PostgreSQL 17.11 migration/API/worker suite, including frozen multi-version publication and all reader grants.
- **84 Cargo cases at the runtime-remediation milestone** (see current follow-on counts in the verification report), five separate Python integrity-guard cases, warnings-denied Clippy, exact-pinned source graph, real Rust 1.88.0 all-target compilation, and refreshed warning-free audit under the sole existing inactive-RSA exception.

See the [verification report](../verification-report.md) for commands/logs and the [public compatibility matrix](../architecture/native-public-search-compatibility.md) for still-open routing/session/contract/privacy/relevance/operations gates. The [published-scope follow-on](native-public-search-scope.md) now implements the user-selected ceiling/live-restriction defaults and highest-READY composition without changing these formats. Dependency remediation and that non-HTTP composition are not public-cutover approval.
