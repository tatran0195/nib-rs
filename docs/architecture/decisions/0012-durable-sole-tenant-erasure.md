# ADR-0012: durable, fail-closed sole-tenant erasure

**Status:** Accepted design; implementation deliberately not exposed  
**Date:** 2026-10-15  
**Decision owners:** Nibleaf platform / identity

## Context

ADR-0011 now permits native account deletion when the account has no workspace memberships or can leave every shared workspace safely. The transactional departure path preserves each shared workspace and, where needed, transfers the final owner role. It deliberately rejects an account that is the **sole member** of a workspace.

**Scope change — 2026-10-16:** at the user's direction, ClickHouse, Qdrant, Git synchronization, and BullMQ are excluded from this native retained-store workflow. This decision no longer requires their fencing, deletion, or provider lifecycle, and a future operation must not claim erasure of data in those excluded systems. ADR-0013 separately replaces the application search paths with project-scoped Tantivy/Lindera; it does not make historical Qdrant data part of this workflow.

A sole-member workspace is a tenant-erasure operation, not a relational `DELETE`. The legacy JavaScript implementation in `packages/auth/src/tenant-erasure.ts` provides useful behavior, but it is not a durable cross-store state machine and cannot be copied as proof of complete erasure:

- it calls `beginUsageDeletion` to fence new usage ingestion and waits for pending usage receipts;
- it synchronously writes ClickHouse tombstones and mutations for analytics and usage;
- it enqueues a BullMQ Qdrant project-delete job, then immediately deletes the organization relationally;
- database cascade removes relational rows, but it does not provide durable step receipts/recovery for search, object storage, export artifacts, or all provider-managed resources.

Source inspection establishes the following retained/external scopes:

| Scope | Evidence | Required treatment |
|---|---|---|
| ClickHouse analytics and usage | `packages/clickhouse/src`, `packages/database/src/index.ts` | **Excluded by current scope.** No native operation may claim those retained facts are erased. |
| Qdrant search vectors | `apps/worker/src/processors/search.ts` | **Excluded by current scope.** ADR-0013 replaces future application search with Tantivy/Lindera, but does not claim deletion of historical Qdrant data. |
| Native Tantivy indexes | `rust/crates/search/src/{lib.rs,registry.rs}`; format-2 stores readable source/result fields and retains superseded/staging files | Newly concrete retained store under ADR-0013, not excluded Qdrant. Before sole-tenant erasure can ship, fence/quiesce native build/promotion/readers, prove no file recreation and receipt exact project-directory purge; include backups in the retention policy. No implementation/erasure claim yet. |
| S3-compatible object storage | `packages/storage/src/index.ts`, `apps/server/src/actions/assets.ts`, `apps/worker/src/processors/export.ts` | Delete all project-prefix objects and exact asset/export artifact keys with a paginated, retry-safe process; record a durable completion receipt. |
| Export jobs/artifacts | Prisma `ExportJob`/`ExportArtifact` models and `apps/worker/src/processors/export.ts` | Stop/cancel pending work before deleting artifact objects; do not rely on relational cascade to remove stored bytes. |
| Git connection/repository credentials and webhooks | Prisma `GitConnection` family and `apps/server/src/actions/git/*` | **Excluded by current scope.** The native workflow does not delete, disconnect, or claim erasure of Git-synchronized resources. |
| externally managed custom hostname | Prisma `Domain` and `apps/server/src/actions/domains.ts` | **Block before any fence, external deletion, or relational mutation** when a domain is provider-managed (`provider != 'INGRESS'`, currently `CLOUDFLARE_SAAS`). No implicit hostname/route disconnect or deletion is allowed. |
| relational project/tenant rows | Prisma schema: `Project.organizationId` is unique; `Organization` may nevertheless be projectless historically | Delete only after every required retained-store step has a durable success receipt. A projectless organization needs an explicit relational-only preflight rather than an assumed project. |

The current production-facing dashboard/worker remains JavaScript during the native cutover. A native workflow cannot claim safe erasure until all writers that can create retained data after a cleanup step are fenced or made to consult one shared deletion state. `tenant-erasure-write-fence-inventory.md` names the inspected legacy and Rust writers plus their required integration points.

## Decision

1. **Keep the native account-deletion endpoint fail closed for every sole-member workspace.** It returns `409` without a partial membership/user change. Direct SQL deletion remains forbidden.
2. **Do not expose a “start erasure” endpoint yet.** Creating a request without a complete write fence would invite a false claim of deletion while legacy application, search, publishing, export, and usage workers can still add retained data.
3. The future implementation must use a database-backed, resumable lifecycle with an immutable request/evidence record and independently idempotent named steps. It must not use a detached best-effort task or treat an outbox acknowledgement as external deletion success.
4. Preflight is fail closed for a provider-managed custom domain (`provider != 'INGRESS'`): it blocks **before** managed retained-store work, relational deletion, or a completion claim. The operation must surface a neutral operator-facing reason and retain all tenant state. Git synchronization is excluded by the current scope; it is neither deleted nor claimed erased.
5. The lifecycle may claim only **at-least-once** external execution. Every provider call must be idempotent by stable tenant/project scope and verified or receipted before its step advances.
6. Relational organization/user deletion is the final step, in a short explicit transaction that revalidates the lifecycle, membership scope, and completion of every required retained-store step. Immutable account-deletion evidence survives without a user foreign key.

## Required lifecycle

The proposed lifecycle is intentionally sequential where ordering carries safety meaning:

1. **Preflight and request creation (one transaction).** Lock the direct native session/user, organization, sole membership, project, Git connection rows, and domain rows in stable order. Revalidate that the requester is the only member and that the organization/project identity matches the request. Reject Git/provider-managed-domain blockers without creating a destructive intent. Persist an opaque erasure request and its fixed project/resource scope.
2. **Quiesce/fence managed writers.** Publish a durable tenant-deletion fence that every managed retained-data producer reads before creating work. Quiesce or make no-op export rendering, storage upload, and legacy dashboard writers that can create managed retained data. This cross-runtime fence is a release prerequisite, not a background cleanup task.
3. **Managed retained-store steps.** Run object-storage and native index deletion outside the request transaction, after their respective writer/reader fences are proven. Each step records attempt/lease/error/receipt durably. A crash after provider acceptance but before acknowledgement repeats an idempotent delete; it never advances merely because a process restarted.
4. **Verification.** Verify the scope that can be verified (for example Qdrant count-by-filter and empty storage listing) and record provider limitations explicitly. Do not equate an unavailable provider with success.
5. **Final relational erasure (one transaction).** Re-lock tenant scope and lifecycle state, revalidate managed retained-store completion, delete relational organization/project state, delete the direct account only when it has no remaining memberships, and append immutable completion evidence. If the account still has shared memberships, it becomes a shared-workspace departure under ADR-0011 rather than deleting those tenants. This completion evidence must name excluded stores rather than implying their erasure.
6. **Reconciliation.** A cancellation-aware worker scans leased/overdue lifecycle steps, reclaims expired claims, records metrics, and makes only bounded progress. It never auto-overrides a provider/Git/domain block.

## Consequences

- Native sole-tenant deletion remains intentionally unavailable until the shared write fence, durable lifecycle schema, adapters, and integration tests exist.
- The existing shared-workspace native deletion is not delayed: it does not remove a tenant or any retained tenant data.
- Better Auth's legacy tenant-erasure path is not treated as native parity. Cutover cannot enable native sole-tenant deletion based on it.
- Operations must retain the evidence and failure records even after relational tenant rows are gone; they are required to diagnose retry and data-subject-erasure claims.

## Verification required before exposure

- PostgreSQL tests for same-tenant concurrent writer/request races, shared-versus-sole membership changes, retry/reclaim behavior, and no partial relational deletion.
- Provider-contract tests for paginated S3 deletion and unavailable-provider failure handling.
- End-to-end disposable service tests proving a post-fence legacy/native producer cannot create managed objects, exports or native index files after the fence.
- Explicit provider-managed-domain blocker tests that prove no managed retained-store or relational mutation occurs. Excluded ClickHouse, Qdrant, Git, and BullMQ systems require no native erasure test in this scope.
- An operational retry/reconciliation drill, including crash after an external provider accepts a deletion but before the worker stores its receipt.
