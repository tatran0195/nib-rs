# ADR-0018: multi-host artifacts, replica materialization and retirement

**Status:** partial local primitives implemented; full multi-host controller/provider/retirement not production-ready  
**Date:** 2026-10-16  
**User-selected target:** whole Rust backend, multi-host, 5,000-document initial benchmark, local-only qualification.

## Context and boundaries

The earlier shared-local-filesystem deployment assumption does not establish multi-host correctness. NFS/distributed filesystems cannot be treated as local atomic rename/flock, and an expired replica/worker lease does not prove its process stopped or its retained bytes disappeared. The existing native generation protocol remains snapshot/source 2 and runtime-bound manifest 3. Stored bodies, staging data, archives, replica roots and backups are retained tenant data.

The repository already uses S3-compatible object storage, but production provider/versioning/object-lock/durability/failure semantics have not been supplied or qualified. This ADR does not claim such a provider is available or tested. A concrete provider/controller implementation must pass the contracts below before serving multi-host traffic.

## Target design to implement and qualify

- PostgreSQL remains publication/tenant/authorization authority. Select highest READY first and its exact generation; never use another tenant, older publication, local active pointer or old engine as fallback.
- Publish immutable, content-digested generation envelopes to a reviewed authoritative artifact store. Generation/project/deployment/runtime/source identity and compressed bytes must be bound in durable registry evidence before the publication is READY for multi-host delivery. Upload retries are at-least-once; conditional create/idempotency and full existing-content verification are required, not ETag-as-content-hash or external exactly-once claims.
- API replicas materialize verified generations into private, unshared roots. Cache identity is generation plus immutable artifact/source/runtime identity, never a reusable admission ticket. Fresh live native/transition/reader/role/grant restrictions remain per request. A bounded single-flight/materialization/cache lifecycle must be owned through cancellation/shutdown.
- Cold/unavailable/corrupt selected evidence fails closed. A cache miss must not select an older generation or legacy engine. Immutable validation can be amortized only through a reviewed trusted-immutability model, not by dropping source integrity or caching stale authorization. The present full per-query validation/scoped statistics remain until that change is implemented/proved.
- Build/promotion/remote upload, replica registration, managed writes, retention/GC and tenant erasure require explicit fences and durable receipts. Offline/unreachable replicas remain unresolved retained locations: TTL expiry or deleting a DB row cannot be a purge acknowledgement. An in-flight or ambiguous provider write is not cancelled merely by closing its client socket. Final erasure must stay blocked until required quiescence/purge/provider facts are actually established.
- Preserve the standing fail-closed externally managed repository/domain policy unless a separately reviewed lifecycle exists. Excluded ClickHouse/Qdrant/Git/BullMQ retained scopes are neither deleted nor claimed erased by native erasure; excluding provider cleanup is not permission for an unreviewed destructive action.

## Implemented local distribution primitive

`nibleaf_search::artifacts` and `search-ops export-artifact/import-artifact` implement a **separate envelope format 1**, not relabeling native manifests. This is an offline/operator primitive, not the above publisher/cache/controller.

- Tar+zstd uses established already-resolved `tar 0.4.46` / `zstd 0.13.3`; only workspace dependency edges are added. Closed descriptor schema binds project/deployment/generation, source digest/count and each file's size/SHA-256; duplicate JSON file keys are rejected.
- A trusted expected whole-compressed SHA-256 and exact identity are required on import. Flat regular filenames only; traversal, symlink/hardlink/PAX/sparse/duplicate/unknown/missing entries are rejected. Compressed bytes, uncompressed sum, file count, manifest/descriptor sizes and decoder window are bounded. Actual streamed export bytes are hashed, not just before/after metadata.
- No export into the source generation or overwrite of an existing file. No import into an existing root. New roots/files use 0700/0600 on Unix. Failure may leave a private quarantined partial root/file without success receipt; no serving DB/pointer is changed. Full native runtime/manifest/stored-source validation runs after extraction.
- CLI filesystem/hash/codec work runs in a joined blocking operation. `verify-selected` must still bind a materialized root to PostgreSQL; an envelope alone is not publication/authorization authority.
- Tests exercise round-trip exact-source/query equivalence, private permissions, digest/scope/size/file/manifest refusal, malformed special/traversal/duplicate/missing/payload/format/duplicate-JSON envelopes, output/source protection and source symlinks.
- The real PostgreSQL/API harness exports/imports one test generation, runs a native-only API on an independent root, verifies selected DB evidence, corrupts only that replica while the first stays available, and checks current session revocation on both. This is **two local processes**, not two physical hosts or a qualified object store.

## Remaining release gates (not completed by this ADR)

Authoritative object-store transport and immutable registry attachment; crash/ambiguous-upload/reclaim tests; bounded cold materialization and cache invalidation/eviction; complete artifact/replica/data-location catalog; distributed writer/read/retirement fences; offline replica and backup erasure/retention; real provider versioning/lock/delete semantics; process/host/network loss and startup/readiness behavior; full publication/renderer/client migration; actual multi-host deployment and load/SLO qualification. Whole-backend feature gaps outside search remain in the readiness ledger.
