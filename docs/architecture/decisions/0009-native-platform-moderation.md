# ADR-0009: Native platform moderation is atomic, native-session-only, and auditable

**Status:** Accepted, 2026-10-15

## Context

Legacy platform actions in `apps/server/src/actions/admin.ts` suspend a regular user by setting legacy moderation fields and deleting Better Auth session rows. They prevent suspension of a platform administrator, but the action implementation does not append a dedicated immutable operator audit fact. Project takedown/restoration writes `project.takedownAt` and `project.takedownReason`; publication already rejects a taken-down project.

The native rollout has two concurrent session stores during the bounded bridge: legacy Better Auth `session` rows and `rust_identity_session`. A moderation implementation that revoked only one of them would leave a viable authentication path. A tenant-scoped authorization check would also be wrong: platform moderation targets global account/project records, not a workspace membership.

## Decision

Add a native moderation vertical slice behind `NIBLEAF_NATIVE_IDENTITY_ENABLED`:

- Only a currently active platform user whose legacy-compatible `user.role` is `admin` may invoke native moderation routes. The operator row is locked and its role is re-read inside the command transaction; a stale native cookie is not sufficient, and a concurrent demotion/suspension serializes before or after the command.
- The routes accept only the native identity cookie. API-wide native admission applies exact trusted-Origin and CSRF checks to every mutation; a Better Auth bridge credential cannot call this administration surface.
- Suspending a regular user atomically sets legacy-compatible `suspendedAt`/ban fields, locks and terminalizes every active native support context whose target is that user with a moderator-attributed impersonation `revoked` fact, revokes every open `rust_identity_session`, deletes all Better Auth `session` rows, and appends one moderation audit fact. Platform administrators cannot be suspended; this preserves the legacy protection against eliminating the administrative operating path.
- Unsuspension never restores a prior session. A user must authenticate again.
- Taking down/restoring a project changes only the existing moderation fields. Publication remains fenced by the existing taken-down-project check. Takedown reasons are optional but normalized, control-character-free, and at most 500 Unicode scalar values.
- Every accepted command, including an idempotent no-op, appends `rust_identity_moderation_audit`. A database trigger rejects ordinary row updates/deletes. Each fact records operator user and session identifiers, constrained action, one target kind, optional reason, whether the requested state changed, and creation time. The table intentionally has no cascading target foreign keys so account/project deletion cannot erase an operator fact.

## Consequences

- This is a native API slice, not dashboard migration. Existing `/admin` pages and Better Auth admin-plugin actions remain authoritative until separately migrated and compatibility-tested.
- The audit table provides durable moderation attribution only for native paths. Historical legacy actions are not backfilled or relabeled as native audit facts.
- Account deletion remains blocked on the retained ClickHouse/search erasure workflow; the audit schema deliberately preserves references as opaque identifiers if deletion is eventually authorized.
- Support impersonation remains a separate high-risk slice. It must not reuse moderation audit rows as a substitute for auditable impersonation start/end/revocation semantics.
