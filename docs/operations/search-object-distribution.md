# Native object distribution and replica cache

**Status:** implemented; local PostgreSQL/S3-wire/worker/API/cache gates executed; final optimized/MSRV/audit receipt in verification report. **Not whole-backend or provider/production qualification.** See [ADR-0020](../architecture/decisions/0020-durable-search-artifact-publication-and-replica-cache.md), [whole readiness](../production-readiness.md) and [verification](../verification-report.md).

## Explicit mode and configuration

`NIBLEAF_SEARCH_DISTRIBUTION` is `local` (existing pre-cutover default) or `s3`. Missing mode with any search-specific S3 credential setting is rejected rather than silently choosing local. An explicit mode never retries another mode, engine, historical generation or snapshot. Invalid/empty/non-Unicode settings fail with setting-name-only diagnostics.

| Setting | Object-mode requirement/default | Bound/meaning |
|---|---|---|
| `NIBLEAF_SEARCH_ARTIFACT_S3_ENDPOINT` | required | Trusted operator HTTP(S) origin, no embedded credentials/path/query/fragment. HTTPS unless explicit HTTP allowance. Never browser input. |
| `NIBLEAF_SEARCH_ARTIFACT_S3_REGION` | required | Explicit SigV4 region (provider must support the configured S3 contract). |
| `NIBLEAF_SEARCH_ARTIFACT_S3_BUCKET` | required | Provisioned **private** bucket. No automatic creation/configuration/deletion. |
| `NIBLEAF_SEARCH_ARTIFACT_S3_ACCESS_KEY_ID` / `...SECRET_ACCESS_KEY` | required | Explicit credentials from secret manager/environment. No ambient AWS/IMDS discovery or fallback. Custom Debug/provider errors do not print values. |
| `NIBLEAF_SEARCH_ARTIFACT_ALLOW_HTTP` | false | Exact true/false. Local fixture uses loopback HTTP; this is not production TLS qualification. |
| `NIBLEAF_SEARCH_ARTIFACT_TIMEOUT_MS` | 30,000 | Positive, ≤60,000; provider request timeout, not guaranteed remote cancellation or a process hard drain bound. |
| `NIBLEAF_SEARCH_ARTIFACT_MAX_BYTES` | 64 MiB | Positive, ≤512 MiB. Bounded single-put/download buffers; no unbounded multipart stream. Worker validates max-bytes × concurrency ≤512 MiB, not aggregate build/RSS. |
| `NIBLEAF_SEARCH_INDEX_ROOT` | worker required | Private absolute **unshared local** build root; 0700/private spool and nonblocking process ownership lock. Not an NFS/shared-lock guarantee. API object mode does not read this root as fallback. |
| `NIBLEAF_SEARCH_QUERY_CACHE_ROOT` | API required | Dedicated private absolute **unshared local** root, parent provisioned, 0700 required. Not worker build root or an arbitrary directory. No automatic chmod of existing data. |
| `NIBLEAF_SEARCH_REPLICA_ID` | API required | Explicit nonnil UUID unique to the logical replica/root. Do not copy a marker to create a second replica or reuse an ID for a new physical root. |
| `NIBLEAF_SEARCH_CACHE_ENTRIES` | 8 | Positive, ≤128. Single-flight, bounded LRU; in-use roots cannot be evicted. |
| `NIBLEAF_SEARCH_CACHE_BYTES` | 512 MiB | Positive, ≤4 GiB of resident imported-file weights. |
| `NIBLEAF_SEARCH_CACHE_ENTRY_BYTES` | 256 MiB | Positive, ≤1 GiB and ≤total cache. Conservative reservation before cold load; temporary archive/transfer space is additional. |

Keep normal six query limits, private response-byte limit, native authority/identity and metrics credentials configured. The initial user workload is a **5,000-doc benchmark profile**, not approval of default upper limits or SLOs. Exact per-generation source/manifest/document limits and cooperative deadline/cancellation apply during runtime import and each query; offline operator import retains its separate trusted API.

The configured store requires prefix-list permission for a lazy first-page read-only readiness probe, GetObject and conditional PutObject on **`native-search/v1/*`**. It does not need a delete/purge permission for this implementation. Provision/review precise bucket policy, TLS, credentials/rotation, audit, versioning, retention/object lock and failure semantics externally. The key is canonical: project SHA-256 / generation UUID / compressed SHA-256. ETag is not a content digest.

## Durable publication and migration

Apply reviewed **`202610160014_search_object_distribution.sql`** after the existing chain. Startup checks delivery constraints, scoped artifact/materialization FKs and the required immutable/READY triggers; merely creating one table is insufficient. Snapshot/source **2**, manifest **3**, envelope **1** and outbox payload schema **1** are unchanged; relational mode/evidence is new.

- Deployment delivery target is server-captured at INSERT and immutable. Generation delivery must match it through a composite FK. Existing deployments/generations stay **local**; do not edit/relabel them as object-mode. Consciously publish a new native deployment for object delivery.
- Worker persists exact upload scope before sending bytes. Conditional create uses no internal provider retry; normal durable outbox redelivery is at-least-once. An existing object is read/size/SHA verified. A timeout/post-accept error records pending/unknown evidence, **not** remote cancellation or deletion success.
- Lease heartbeat covers source load, joined build/export, provider verification and promotion. Known lease loss stops later stages but active work is joined. Stale claimants cannot confirm another attempt, attach or advance READY. Serialized private spooling is quota-bound and only newly owned spool data is removed; crash leftovers can require operator review.
- Verified immutable artifact attachment and READY/pointer advancement share one fenced transaction. DB trigger rejects object READY without exact verified scope/document/hash/size/key evidence. Missing/corrupt selected object never falls back to master files/local mode, old READY or another engine.
- Old API binaries do not implement this new mode contract. **Quiesce/drain old readers/workers and complete the reviewed mixed-version/client/delivery rollout before enabling object publication.** Do not infer full expand/contract or renderer compatibility from the additive migration alone. The full native site adapter/auth/browser/remaining backend migration is still open.

### Deliberate legacy-deletion compatibility block

Native files/objects/replicas cannot be safely erased by the old synchronous Better Auth tenant workflow. Legacy project/tenant deletion now probes for the native generation schema and refuses **before** usage fencing, provider/queue effects or relational mutation; tenant-bearing legacy account deletion checks before any earlier ownership/member mutation. Unknown probe results/errors fail closed. Project API returns conflict reason `native_retention_lifecycle_required`. Native shared-workspace departure remains its own atomic route; sole-tenant native deletion remains blocked.

This is an explicit **fail-closed compatibility break after native schema expansion**, not deletion parity. It avoids a late artifact FK failure after irreversible legacy effects and does not race a per-project row-count check. Deploy/review this guard and quiesce destructive legacy requests before native activation/migrations. No ClickHouse/Qdrant/Git/BullMQ cleanup workflow was added or claimed; excluded stores are not erased. Externally managed repository/domain lifecycle remains separately fail closed. No production deletion was executed here.

## Replica-owned materialization and lifecycle

Preparation stays fresh READ ONLY / REPEATABLE READ under the existing shared query lease: workspace/native/transition/reader authorization, highest READY and live restrictions. Admission denial occurs before tenant artifact GET or native index open. Cache/catalog **operational writes** happen afterward on owned short transactions; these are not user query analytics, reader/session activity, or reusable authorization tickets.

An API owns `cache-owner.json` (replica/root UUID), a local process lock and only `entry-UUID` directories. Unexpected owner/data, symlinks, wrong permissions/ID or another process/root refuses startup. Catalog root identity cannot be silently replaced. Cold loads are single-flight, recheck after waiting and reserve bounded disk space. In-use Arc leases stay held through joined search/encoding; eviction only purges idle materializations. Each selected artifact verifies exact identity/compressed content, strict flat archive entries, decoder/data bounds and full native source/runtime/registry integrity. Each query still verifies its current scope/integrity; no result/statistics/admission cache is introduced.

Normal eviction/shutdown receipts `purged` **after** actual owned file cleanup. Failure latches cache unsafe/refuses work or returns failed drain; it is not purged. Crash retains ready/materializing catalog facts. On restart with the same owned root, only locally present owned entries are cleaned/receipted before serving. Missing/unreachable/offline locations are not swept from TTL or assumed absent. Materialization admission also refuses once a replica/root has 100,000 retained catalog rows, including purged history; this is a conservative growth quota, not an invented retention/deletion policy. A reviewed lifecycle/compaction policy is still required, and the limit must not be bypassed by treating unknown locations as purged. A copied backup/volume is a separate retained location requiring inventory; copying identity markers is not a valid replica-creation protocol.

Normal shutdown discards local cached entries; the next start fetches them cold. Provision local space for the configured cache plus bounded current compressed spool/transfer and filesystem overhead. Data ceilings are not aggregate RSS/CPU/IO/network/hard-shutdown guarantees. Process-manager termination/memory/ingress policy still needs actual multi-host qualification.

## Inspection (restricted operator)

```sql
SELECT delivery_kind,state,count(*) FROM rust_search_generation GROUP BY 1,2;
SELECT state,count(*) FROM rust_search_artifact_upload GROUP BY 1;
SELECT replica_id,root_id,state,count(*),sum(unpacked_bytes)
FROM rust_search_materialization GROUP BY 1,2,3;
```

Inspect pending/unknown uploads as retained effects; do not mark them deleted/failed merely from socket close, age or lease expiry. Scope/attachment records are immutable and intentionally block unreviewed relational deletion. No provider delete, GC or whole-tenant erasure API exists. A future reviewed lifecycle must account for worker/staging roots, all upload candidates, current/superseded objects, online/offline replicas, backups/versioning/locks and ambiguous writes.

Restoring an object-mode publication requires its **original bound compressed bytes/SHA**, not silently replacing its immutable reference with a newly reindexed tar (segment IDs can change). Restore verified blob bytes or create a conscious new publication. `search-ops verify-selected` verifies source/registry/index identity, not provider durability or all-location erasure.

## Executed local proof versus production qualification

- A ninth flagged PostgreSQL function proves mode/READY/scope/immutability, post-put ambiguity/reclaim/stale-token fencing, single-flight cold roots, corrupt object/master no-fallback, process/root ownership and actual shutdown purge receipts.
- Local S3 HTTP fixture verifies AWS4 signatures with explicit test keys despite hostile ambient AWS credentials, conditional puts, accepted-then-error/retry/content verification, READY withholding, two cold independent API roots, grants/revocation/no activity mutation, no storage I/O on denial, corruption/no local fallback, eviction, SIGKILL retained facts plus owned restart cleanup, and in-flight object IO/capacity/cleanup after a shorter client deadline. Eleven invalid startup cases fail closed without secret reflection; DEBUG logs are checked.
- This fixture is **not** AWS/R2/MinIO durability, IAM infrastructure, versioning/object-lock/delete behavior, a real second host, full backup scope or a production SLO. Local optimized 5,000-doc measurements are characterization only. Whole-backend/caller/provider/erasure/remaining-feature release gates remain blocked.
