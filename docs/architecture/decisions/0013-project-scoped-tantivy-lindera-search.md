# ADR-0013: project-scoped Tantivy search with Lindera analysis

**Status:** Accepted; implementation in progress  
**Date:** 2026-10-16  
**Decision owners:** Nibleaf platform / search

## Context

The requested target is per-project embedded search based on Tantivy and Lindera, replacing the current search implementations. Source inspection found **no PostgreSQL `pgvector` implementation** to replace. The current source instead contains:

- an in-process Orama full-text/fuzzy cache in `packages/search` and `apps/server/src/lib/search-cache.ts`; and
- an optional Qdrant BM25+dense hybrid path, indexing worker, diagnostics, integration configuration, and dashboard/API paths under `packages/qdrant`, `apps/worker/src/processors/search.ts`, and `apps/server/src/actions/search.ts`.

The selected product decisions are:

- use **lexical search only**; do not embed dense vectors or call an embedding provider;
- replace both Orama and the optional Qdrant path rather than retain a long-lived fallback/shadow mode;
- use Lindera for language-aware morphological tokenization, with separate physical indexes per project.

## Decision

1. Create a Rust `nibleaf-search` feature crate using established `tantivy` and `lindera` crates; [ADR-0014](0014-search-runtime-dependency-remediation.md) replaces the original wrapper with a small failure-checked adapter and records the temporary manifest-only upstream dependency patch. It is lexical-only. There is no vector model, pgvector extension, Qdrant collection, embedding provider, or semantic-ranking claim.
2. Each project receives an isolated physical Tantivy index rooted under a configured persistent index directory. Raw project IDs are never used as filesystem path components; directory names are a stable SHA-256 scope derivation.
3. An index generation is built from one immutable deployment snapshot. Build into an unpublishable generation-specific staging directory, commit, write a versioned manifest, validate, then atomically rename it to its final generation path. The manifest binds exact engine/analyzer/patch compatibility as well as generation/project/deployment identity, a non-content per-page checksum map, document count, and an order-independent aggregate document checksum; the index stores each document checksum for validation. A PostgreSQL registry leases/reclaims named builds and atomically promotes only a validated final generation. Member management reads the active registry pointer. [ADR-0015](0015-published-search-ceilings-and-ready-selection.md) refines public selection to the exact registry row of the highest READY deployment, including a retained superseded generation during the promotion gap. Failed/reclaimed builds never make unpublished data the public publication.
4. Index documents retain only the fields necessary for search result rendering and authorization-safe routing: opaque page ID, project ID, deployment ID, exact language, version ID/slug, title/path, description/icon, headings/sidebar text and bounded authored content. Format 2 stores the source fields and recomputes their checksum during validation; body lexical indexing does not discard MDX component text. The query path must require a project ID and revalidate project/reader authorization before opening an index; it must never search a global corpus and filter afterward.
5. Lindera is registered under stable tokenizer names with embedded IPADIC for Japanese and CC-CEDICT for Chinese. Japanese and Chinese documents index/query through their respective analyzer fields; other languages use an explicit Tantivy default-analyzer field rather than silently claiming former Orama-language parity. The binary/image size and persistent index-volume requirements are release gates.
6. The existing Node search HTTP/API/UI and worker paths must migrate to typed native API/worker contracts before Qdrant/Orama packages, provider configuration, dashboard integration cards, diagnostics, jobs, and documentation are removed. This is a compatibility migration, not a background replacement of a live index.

## Consequences

- Search is a local operational component of the modular monolith. API and worker instances that serve/build indexes must share a durable index volume or an equivalent explicitly designed replication mechanism; ephemeral container disks are not supported for enabled search.
- Search availability is per project: a damaged/missing project generation affects that project only and returns a classified unavailable/rebuilding result. It does not fall back to another tenant's corpus.
- Indexing is at-least-once internal work. A repeated build for the same deployment can safely produce/reuse the same validated generation; no exactly-once filesystem or promotion claim is made.
- Dense semantic retrieval and answer generation are intentionally removed from this slice. Any future semantic feature requires a new ADR with model, privacy, cost, erasure, and authorization design.
- Existing Orama/Qdrant runtime flags and provider status are compatibility surface until their callers migrate. They must not be silently removed from a dashboard/API release.

## Required vertical slices and release gates

1. **Index-core crate:** path-safe project generation layout; deterministic schema/document construction; Lindera tokenization; query parsing; immutable manifest with stored-document checksum validation; per-project isolation unit tests. **Implemented and unit-verified.**
2. **Durable registry/build state:** PostgreSQL migration and worker lease/reclaim/heartbeat path; canonical deployment-snapshot identity binding; atomic active-pointer promotion and previous-generation retention; terminal-state evidence; PostgreSQL integration race proof. **Implemented and non-skip verified against local disposable PostgreSQL 17.11.**
3. **Publication integration:** the publication transaction records `search.generation.requested` and completion intent atomically using the same canonical snapshot identity. The worker idempotently registers/reuses the generation, and publication remains `BUILDING` until promotion; it retries absent/pending/building state and becomes `FAILED` on terminal generation state. Empty page sets are valid lexical generations so navigation-only projects do not regress. **Implemented and non-skip verified for registry ordering, native starter publication, and ordinary API publication against local disposable PostgreSQL 17.11.**
4. **Query API and authorization:** the typed Axum member-management route is implemented with exact version/locale filters, additive rendering/version fields, no-store responses, joined blocking work and PostgreSQL cross-project denial. Separate read-only live public admission is project-bound and PostgreSQL-proven for access modes, reader session/audiences, zero grants, revocation and no activity mutation. Non-HTTP `search_admitted_generation` applies mandatory exact version, locale and reader page terms before TopDocs; real-grant and limit-1 proofs demonstrate no grant bypass or post-limit loss. There is still **no public native route**. ADR-0015 implements shared immutable defaults, user-selected published ceilings plus live restrictions and highest-READY selection in one short read-only MVCC view, followed by consuming synchronous execution. Optional actor/session transport, public schema/cache/errors/analytics/relevance/privacy, bounded complete execution/cancellation/shutdown and operations remain cutover blockers in [the compatibility matrix](../native-public-search-compatibility.md).
5. **Legacy/dashboard cutover:** migrate Node callers/UI to the native contract; remove Qdrant, Orama, vector/embedding configuration, integration exposure, BullMQ search jobs, and obsolete operational documentation in a separately reviewed compatibility release.
6. **Operations:** persistent-volume readiness checks, index disk/segment/latency metrics, corrupt-generation recovery drill, backup/restore story, and safe per-project rebuild operator procedure.

## Format-2 expansion and remaining release gates

Native publication capture now reads every branch/language/page collection at one PostgreSQL statement snapshot, freezes version slugs and variable-substituted text, and rejects cross-project language/branch references before acceptance. Typed extraction excludes groups, hidden/noindex pages and disabled locales while retaining their facts in the publication. Document identity includes all exact routing/rendering fields; source recomputation and exact schema matching are promotion/open gates. Valid empty generation promotion remains supported.

Snapshot 1 and manifest-1/2 generations are deliberately refused. Snapshot/source format stays 2; [ADR-0014](0014-search-runtime-dependency-remediation.md) advances the current manifest to 3 with exact engine/analyzer identity. Apply both the [source-format migration](../../migration/native-search-format2.md) and [runtime upgrade](../../migration/native-search-runtime-upgrade.md), using a new reviewed publication rather than editing historical snapshots/indexes. Native snapshot format 2 is not a full legacy site-renderer adapter. The event schema and PostgreSQL registry layout do not change.

Full authored bodies are now stored in the index for immutable source verification and snippets; the volume, staging, superseded files and backups are retained tenant data. Native erasure/write fencing, safe rebuild/GC and recovery drills remain gated, not implicitly supplied by relational cascade. The formerly active search dependency warnings are resolved under ADR-0014: unchanged Tantivy 0.26.2 source with a sole LRU 0.18.5 manifest patch, direct Lindera 6.2.0 without the unused YAML wrapper, and checked segmentation failures. Refreshed audit passes with all warnings denied under only the existing inactive-RSA exception; raw audit still fails for that inactive RSA advisory. Patch provenance/pins, runtime compatibility, future upstream replacement and the other public/operations release gates remain mandatory.

## Verification standard

- Japanese tokenization/recall tests using IPADIC and non-Japanese contract tests.
- Project A/B isolation tests for build, query, corrupt/missing generation, and filesystem scope derivation.
- Durable promotion/reclaim tests proving a failed new generation cannot replace a prior readable one.
- PostgreSQL/API integration tests for authorization and publication/index ordering.
- A migration test proving no live caller remains on Orama/Qdrant before those dependencies and configuration are removed.


## Query lifecycle follow-up — 2026-10-16

[ADR-0016](0016-supervised-search-query-lifecycle.md) now implements a feature-owned query service and application-driven driver for member HTTP and a non-HTTP published facade. Shared immediate admission, bounded transfer/validation, tracked SQL after caller cancellation, joined CPU work, coordinated API drain and bounded trace/metric privacy have real local DB/CPU/HTTP proofs. This does not expose public HTTP, resolve optional native/transition identity transport or prove hard CPU/I/O/drain/RSS/load bounds. The earlier milestone counts in this decision are historical; the current [verification report](../../verification-report.md) and [runtime guide](../../operations/search-query-runtime.md) carry the new evidence/limits. No new DDL, content policy or artifact format is introduced by this follow-up.
