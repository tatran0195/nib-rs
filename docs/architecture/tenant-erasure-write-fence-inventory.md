# Sole-tenant erasure: cross-runtime write-fence inventory

**Status:** discovery inventory; required before exposing ADR-0012 erasure  
**Date:** 2026-10-16

## Purpose

A retained-store deletion is unsafe unless every producer that can create data after an external delete sees the same tenant fence. This inventory is based on current source inspection, not README claims. It turns ADR-0012's “cross-runtime write fence” into concrete implementation work.

`Organization` and `Project` are effectively one-to-one for current sites (`Project.organizationId` is unique), but historical projectless organizations exist. A final workflow must scope/fence the organization first and must reject or separately handle projectless tenants rather than fabricate a project ID.

**Scope change — 2026-10-16:** ClickHouse, Qdrant, Git synchronization, and BullMQ are excluded by user direction. They are retained below only as source evidence; they are not native erasure gates or deletion work. A managed completion record must not claim that excluded systems were erased.

## Existing partial fence

`packages/database/src/index.ts` already has a narrow, useful ClickHouse usage/analytics fence:

- `beginUsageDeletion(organizationId, projectId)` takes a tenant advisory lock, locks the organization, sets `usage_storage_marker.deletionPendingAt`, and reports pending usage checkpoints.
- `markUsageStoragePending`, `markAnalyticsStoragePending`, and `runWithTenantAnalyticsWriteFence` reject post-fence work while allowing an already-recorded checkpoint to drain.
- `apps/worker/src/processors/analytics.ts` uses that fence for analytics writes and drains pre-fence usage checkpoints before marking them written.

It is **not** a whole-tenant write fence: it does not prevent object uploads, export rendering, publishing/search indexing, or all legacy dashboard writes. It therefore cannot alone authorize relational deletion after ClickHouse work.

## Writer and consumer inventory

| Scope | Current source evidence | Current behavior during `usage_storage_marker` deletion fence | Required ADR-0012 gate before erasure is exposed |
|---|---|---|---|
| Analytics facts and ClickHouse usage | `apps/server/src/lib/usage-ingest.ts`, `apps/worker/src/lib/usage-ingest.ts`, `apps/worker/src/processors/analytics.ts`, `packages/database/src/index.ts` | Partially fenced/durable as above. | **Excluded.** Do not add native erasure work or claim these facts are deleted. |
| Search-vector indexing | `apps/worker/src/processors/search.ts`; Qdrant client in `packages/qdrant/src/client.ts` | No tenant-erasure fence. | **Excluded.** ADR-0013 replaces future application search with project-scoped Tantivy/Lindera; no native Qdrant erasure work is required. |
| Publication-triggered search work | `apps/worker/src/processors/publish.ts`, `apps/server/src/actions/deployments.ts`, `packages/auth/src/server.ts`, Rust `crates/publishing/src/lib.rs` | Legacy publishers queue external search; native durable intents are addressed separately below. | Only legacy BullMQ/Qdrant behavior is excluded. Native publication/index writers need the managed fence once they retain files. |
| Rust starter/onboarding publication | Rust `crates/projects/src/lib.rs`, `crates/publishing/src/{lib.rs,snapshot.rs}` and worker dispatch | No tenant-erasure fence; emits native index work. | Once native retained indexes exist, fence new publication/build intents and drain/join in-flight work before any native index purge. This does not re-include BullMQ/Qdrant. |
| Native Tantivy source/index files | `rust/crates/search/src/{lib.rs,registry.rs}`, `rust/apps/worker/src/main.rs`; format-2 stored bodies/result fields and retained staging/superseded generations | No tenant-erasure fence or purge/GC receipt exists. PostgreSQL cascade does not erase the project-hashed directory. | Include native build/reclaim/promotion writers, retained generations and in-flight readers in the managed fence/quiescence design; prove no post-fence file recreation and record exact project-directory purge success before claiming tenant erasure. Backups follow an explicit retention policy. |
| Asset upload bytes | `apps/server/src/actions/assets.ts`; storage primitives in `packages/storage/src/index.ts` | No fence; direct byte upload and presigned PUT issuance can occur after a future cleanup. | Check the fence before allocating/uploading an asset and prevent new presigned PUTs. Erasure must delete exact asset keys plus `projects/<projectId>/assets/` pagination-safe prefix. |
| Export artifact bytes | `apps/server/src/actions/exports.ts`, `apps/worker/src/processors/export.ts` | No fence; pending jobs and schedules can render/upload after an external cleanup. | Fence request creation, schedule dispatch, and render worker; cancel/drain queued jobs; delete each artifact key and `projects/<projectId>/exports/` prefix before cascade. |
| Git synchronization | Prisma `GitConnection` family; `apps/server/src/actions/git/*`; `apps/worker/src/processors/git.ts` | Outside native erasure scope. | **Excluded.** Do not add a native provider lifecycle or completion claim for Git-synchronized resources. |
| Provider-managed custom hostname | Prisma `Domain`; `apps/server/src/actions/domains.ts` | Not eligible when provider is not `INGRESS` (currently `CLOUDFLARE_SAAS`). | Preflight block before any erasure fence, external deletion, or relational mutation. No implicit Cloudflare hostname/route removal. |
| Ingress-managed custom domain | `apps/server/src/actions/domains.ts` | No provider-owned resource is removed by relational cascade, but a user-controlled DNS record can remain pointed at the service. | Explicitly retain the record; relational removal must make resolution fail safely. Document this as disconnection of Nibleaf routing, not DNS deletion. |
| Outbox/BullMQ payloads | `outbox_event`, `packages/bullmq`, Rust `crates/outbox` and `apps/worker` | No universal fence. | **BullMQ is excluded.** Do not add native BullMQ cancellation/reconciliation work in this scope; Rust outbox behavior must be evaluated separately if a managed storage workflow later needs it. |

## Implementation order

1. Introduce one additive organization/project erasure-fence record for the managed storage/export/native-index scope, with opaque lifecycle ID, timestamp, and immutable request evidence. It must survive final relational deletion; do not use a foreign key that cascades away the record.
2. Wire managed asset/export and native publication/index producers and consumers to the fence. This must ship in legacy Node and Rust binaries before a route can create a fence.
3. Add PostgreSQL race tests for managed writer-vs-fence paths. The test must show no post-fence managed object/export/index write is made, not merely that a relational row later cascades.
4. Add the durable erasure request/step ledger and provider-managed-domain preflight block from ADR-0012.
5. Only then expose the managed native sole-tenant erasure scope, initially with S3 adapter contract tests and a disposable provider-backed integration environment.

## Explicit non-goals at this stage

- This document does not authorize an erasure endpoint or relational organization deletion.
- A `usage_storage_marker` alone is not promoted to a generic tenant tombstone.
- No Git repository, Cloudflare hostname, route, or customer DNS record is deleted or disconnected implicitly.
