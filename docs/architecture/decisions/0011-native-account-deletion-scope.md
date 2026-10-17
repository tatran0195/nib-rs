# ADR-0011: Native self-account deletion preserves shared workspaces and fails closed on sole-member tenants

**Status:** Accepted, 2026-10-15

## Context

The legacy account-deletion implementation does more than delete a relational `user` row. For an owner who is the sole workspace member it runs retained-store organization erasure: it fences usage ingestion, removes ClickHouse analytics/usage, queues durable search removal, then removes the relational organization. For other owner arrangements it reassigns ownership before deleting the account. Related object storage, exports, Git-related data, and other retained external artifacts require an explicit, separately verified scope before claiming end-to-end deletion parity.

Native OTP signup also appends `identity.account.provision_requested`. A worker can later create a starter workspace and membership. Checking membership without serializing this intent can race with provisioner work and leave a newly created tenant stranded after account deletion.

## Decision

Expose a gated native `POST /v1/identity/account-deletion` route only for a **direct, live native session**. It supports a user with zero memberships and a user who can safely depart every workspace because each has another member. A sole-member workspace remains a hard conflict until tenant retained-store erasure exists.

The PostgreSQL transaction:

1. rejects an impersonated actor before any state change;
2. locks and revalidates the exact native session and user, including expiry, revocation, suspension, and ban state;
3. locks all account-provisioning outbox rows;
4. rejects a `processing` provisioning event, because a worker owns that intent;
5. locks affected organizations first in deterministic order, then locks each departing membership and every member row; this organization-first order prevents two concurrent departures from deadlocking while each holds a different member row;
6. fails without mutation if any organization has no other member; otherwise, when the departing account is its only owner, demotes it and promotes the earliest other member to owner in the same transaction;
7. removes only the departing memberships from retained shared workspaces;
8. moves any `pending` provisioning event to `dead_letter` with explicit cancellation evidence;
9. appends one immutable `rust_identity_account_deletion_audit` fact with opaque user/session identifiers; and
10. deletes the user in the same transaction, allowing existing foreign-key cascades to remove native sessions and other relational credentials.

The locked user row makes an in-flight provisioner membership insert wait on its user foreign key. The organization lock stabilizes each sole/shared-membership decision against concurrent member insertion. Deletion either observes a member and transfers/preserves the workspace, observes a sole tenant and fails closed, or deletes the user first; a raced provisioner insert then fails and its whole project transaction rolls back. The route clears `nibleaf_identity` only after the transaction commits and returns `Cache-Control: no-store`. 

API-wide native browser admission requires the exact configured `Origin` and `X-CSRF-Token` for this unsafe route. The feature also verifies that the request includes the native cookie. A missing, expired, revoked, suspended, banned, or otherwise stale session is an unauthenticated outcome; no deletion evidence is created.

## Consequences

- This route is a real, destructive self-service deletion subset, not an assertion of Better Auth deletion parity.
- An account can leave a shared workspace. The workspace remains, and an owner departure transfers its owner role to the earliest other member atomically. No retained-store deletion is needed because the tenant continues to exist.
- Any account that is the sole member of at least one workspace receives `409 Conflict` and retains its session. It must not be deleted by manually calling relational SQL or bypassing this endpoint.
- A worker-owned provisioning event also receives `409 Conflict`; operators should allow the worker to finish and then use the future tenant deletion/ownership workflow.
- Migrations `202610150010_identity_account_deletion_audit.sql` and `202610150011_identity_account_deletion_shared_memberships.sql` provide append-only evidence for zero-membership and shared-workspace departure actions. The table intentionally has no user foreign key and rejects ordinary updates/deletes, so deletion cannot erase its own fact.
- A later tenant-bearing deletion design must explicitly cover ownership transfer/reassignment, usage ingestion fencing, ClickHouse, durable search deletion, object storage/assets, exports, Git-related records, external service retention, retries/observability, and safe recovery. It may extend this decision but must not broaden the existing endpoint silently.
- **External Git and custom-domain policy:** until an explicit provider-side lifecycle exists, any tenant erasure that finds a Git connection or externally managed domain must fail closed before retained-store or relational deletion. It must not silently delete a customer-controlled repository/domain, and it must not falsely report account deletion while those resources remain. ADR-0012 records this selected policy’s durable lifecycle and cross-runtime write-fence prerequisites; no sole-tenant erasure route is exposed yet.
- The endpoint remains behind `NIBLEAF_NATIVE_IDENTITY_ENABLED` and does not migrate Better Auth dashboard account-deletion UI or authority.
