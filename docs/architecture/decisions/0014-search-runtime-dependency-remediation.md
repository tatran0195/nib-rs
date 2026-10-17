# ADR-0014: checked native analysis and a bounded upstream dependency patch

**Status:** Implemented and locally verified; no production search cutover approval  
**Date:** 2026-10-16  
**Related:** [ADR-0013](0013-project-scoped-tantivy-lindera-search.md), [runtime migration](../../migration/native-search-runtime-upgrade.md), [verification report](../../verification-report.md)

## Verified context

The previous active native search graph used Tantivy 0.25.0, lindera-tantivy 2.0.0 and Lindera 2.3.4. Refreshed cargo-audit reported two `lru 0.12.5` unsoundness warnings (`RUSTSEC-2026-0253`, `RUSTSEC-2026-0002`) and the unmaintained `encoding` and `unsafe-libyaml` paths (`RUSTSEC-2021-0153`, `RUSTSEC-2024-0449`). Unlike optional SQLx RSA lockfile content, these packages had real workspace target paths.

Inspection of published crate manifests/source, not just release descriptions, established:

- Tantivy 0.26.2 requires `lru ^0.16.3`. This fixes the older `IterMut` advisory but still permits versions affected by `RUSTSEC-2026-0253`; that advisory requires at least 0.18.2. A published Tantivy upgrade alone does **not** resolve both warnings.
- The published lindera-tantivy 5.0.1 requires Tantivy 0.26.1 and Lindera/lindera-analysis 5.x. Lindera-analysis still brings in `serde_yaml_ng`/`unsafe-libyaml`. Its infallible Tantivy token-stream implementation also unwraps a fallible Lindera result. The application needs programmatic segmentation, not YAML, environment-selected configuration, or that failure behavior.
- Lindera 6.2.0 exposes `Segmenter` directly, uses `encoding_rs` in its dictionary graph, and declares Rust 1.88. IPADIC and CC-CEDICT can be embedded without the analysis/YAML crate.
- Tantivy's upstream development branch already uses a patched LRU requirement, but adopting an unreleased engine branch would include unrelated changes. This pass instead preserves the published 0.26.2 engine source.

The existing generation source checksum establishes content identity, **not** equivalence of postings produced by different engine/analyzer versions. Keeping manifest format 2 unchanged through this upgrade would be an unjustified compatibility assumption.

## Decision

1. Use established Tantivy **0.26.2**, Lindera **6.2.0**, and upstream `lru` **0.18.5**. Remove lindera-tantivy and its unused analysis/YAML graph. Keep lexical-only, per-project indexes and the existing Japanese/Chinese dictionary choice; add no vector, embedding, external tokenizer service, or fallback runtime.
2. Vendor the checksum-verified published Tantivy 0.26.2 archive under `rust/vendor/tantivy-0.26.2`. The **only** upstream file modification is the normalized `Cargo.toml` LRU requirement, from `0.16.3` to `=0.18.5`. No engine Rust source, unsafe code, package version, license, or index-format constant is rewritten. The original manifest and upstream file checksums are retained in the adjacent provenance inventory.
3. Treat this as a temporary, reviewed manifest-only dependency patch, not a general fork or an advisory exception. `check-search-dependencies.py` verifies all 374 upstream files, the sole manifest delta, registry-backed LRU/Lindera packages, exact dictionary release/feature pins, and declared dependency MSRVs. Its five regression cases cover a valid patch plus changed source, an added file, an unpatched dependency and an unrelated manifest edit. CI must run it.
4. Implement only the small application-owned Tantivy `Tokenizer`/`TokenStream` adapter around Lindera segmentation. Preserve source byte offsets, token positions and position lengths. Use normal mode, embedded IPADIC/CC-CEDICT, no user dictionary or text filters, omitted whitespace, and an explicit disabled unknown-word ladder. This choice preserves the previous grouping policy's intent, **not** a claim of identical old dictionaries, terms or relevance.
5. Do not panic or silently accept a failed segmentation. Tantivy's trait cannot return `Result`, so all tokenizer clones share a bounded first-error latch retaining the original source error. An internal failed stream is empty, but the builder joins indexing workers with `commit()`, explicitly joins remaining merge work with `wait_merging_threads()`, then checks the latch **before** writing a manifest or renaming the generation; query parsing checks it before result collection. A segmentation failure becomes typed unavailability/permanent build failure, never a successful partial index, zero-result response, or caller-syntax error. Tests inject a failing tokenizer at both boundaries.
6. Advance the **generation manifest to format 3**, with mandatory exact compiled Tantivy/index-format identity, Lindera identity, reviewed dependency-patch revision and analyzer-profile revision. Both on-disk open/reuse and registry promotion require the current identity. Native publication snapshot and source-document checksum formats remain **2**; outbox event schema remains **1**; registry DDL is unchanged.
7. Enable Cargo resolver **3** so updates respect the workspace's declared Rust 1.88 baseline. The prior lockfile's UUID release required 1.89, and a newly selected ordered-float release required 1.90. Resolution now uses UUID 1.26.1 and ordered-float 5.4.0. A real locked Rust **1.88.0** all-target workspace check passed; CI also gets that compiler gate. This is not a container-build claim.
8. Deny **all** cargo-audit warnings in CI. Keep only the pre-existing `RUSTSEC-2023-0071` exception, paired with its inactive-target guard. The refreshed raw audit still fails solely for inactive RSA; the exception audit with `--deny warnings` passes without informational warnings. No search advisory is ignored.

## Compatibility and operations consequences

- Existing manifest-1/2 generations are unavailable to upgraded binaries, even when Tantivy itself can read the underlying file format. Missing runtime identity is an error, never a default.
- Coordinate API/worker rollout. This pass supplies no rolling mixed-runtime compatibility, automatic index conversion, in-place rebuild, registry reset, or production backfill. Use the reviewed migration and a **new, consciously reviewed publication** until a separate safe rebuild workflow exists. Republishing current drafts can change published content.
- Keep historical generations/snapshots intact. Their bodies, staging data and backups remain tenant data under ADR-0012; erasure/GC/recovery work is not solved by this dependency upgrade.
- Japanese/Chinese recall, Unicode offsets and one Chinese compound-token fixture are characterized, but multilingual/Arabic/fuzzy/MDX relevance and generation-wide BM25 privacy/public-score review remain public-cutover gates.
- The temporary upstream patch adds maintenance responsibility. Replace it when a compatible official Tantivy release carries a patched LRU. Remove the vendor/provenance/path override together, revise compatibility identity as needed, and rerun provenance/pin, audit, MSRV, source and real PostgreSQL/API/worker gates. Do not remove the guard or add advisory ignores just to make an update pass.
- No public route, legacy Orama/Qdrant removal, production mutation or live-provider action is part of this decision.

## Evidence

The runtime-remediation milestone reported **84 Cargo cases** (historical count; [ADR-0015](0015-published-search-ceilings-and-ready-selection.md) records the subsequent current-source suite): 78 ordinary unit cases, one non-database Tantivy document-store integration case, and five PostgreSQL entry functions returning early in the ordinary command. All five database proofs were separately executed non-skip by the full PostgreSQL 17.11 API/worker harness. The document-store case creates and joins a real four-segment merge, then uses a one-block cache and verifies real cache hits/misses, eviction and exact stored-body reads. This tests API/storage compatibility, not general memory-safety or load/performance.

Sources: published manifests/source for [Tantivy 0.26.2](https://crates.io/crates/tantivy/0.26.2), [lindera-tantivy 5.0.1](https://crates.io/crates/lindera-tantivy/5.0.1), [Lindera 6.2.0](https://crates.io/crates/lindera/6.2.0), [LRU 0.18.5](https://crates.io/crates/lru/0.18.5), and the executed RustSec advisory database refresh. Exact commands, failures resolved during development, scope limits and retained logs are recorded in the verification report.
