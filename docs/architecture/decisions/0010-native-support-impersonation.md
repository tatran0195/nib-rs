# ADR-0010: Native support impersonation uses bounded target sessions and separate immutable audit facts

**Status:** Accepted, 2026-10-15

## Context

The legacy Better Auth admin plugin is configured for a one-hour impersonation session. It writes `session.impersonatedBy`; the customer app shows a support banner and exits via `stopImpersonating`. Legacy `databaseHooks` append best-effort `platform_event` rows only when the Better Auth session is created/deleted. The stored session identifies the operator but does not bind it to the exact initiating operator session, and the generic platform-event shape is not an immutable native support audit trail.

During the bounded native migration, browser support access must not make an operator token ambient to customer feature endpoints. Emergency revocation also must not leave an operator trapped in a revoked customer context with no safe way back to their own still-valid session.

## Decision

The gated native implementation introduces a distinct one-hour `rust_identity_session` for the target with `impersonated_by` set to the operator user ID, plus two PostgreSQL records:

- `rust_identity_impersonation` is a mutable lifecycle fence binding exact operator session, target user, support session, expiry, and terminal state. A partial unique index permits at most one active support context for one operator session.
- `rust_identity_impersonation_audit` is append-only. It records `started`, `ended`, and `revoked` facts with the exact human actor session, plus `expired` facts with target, support-session, and time. Expiry facts are attributed to the named system actor `system:identity-impersonation-expirer` and deliberately carry no fabricated user session. A trigger rejects ordinary update/delete operations.

Only a direct, live platform administrator with a native session can start or emergency-revoke support access. An impersonated session cannot use platform moderation or begin/revoke further impersonation. Self-impersonation is rejected. Target availability and an optional target membership `active_organization_id` are checked in the same transaction that issues the support session and appends its `started` fact. If an impersonated target session uses the generic native sign-out endpoint, the support lifecycle is terminalized with an `ended` fact in that same sign-out transaction rather than silently leaving stale support state.

Starting support replaces the normal `nibleaf_identity` browser cookie with the target session and returns that session's CSRF secret once. The pre-existing operator token is held only in `nibleaf_identity_resume`, an `HttpOnly`, SameSite-Lax cookie scoped to `POST /v1/identity/admin/impersonations/stop`; it is not ambient feature authentication. The stop route requires an explicit trusted Origin and authenticates this resume cookie directly. That narrow admission deliberately remains available even if the target support cookie is expired or has been emergency-revoked, so it can clear the support context and restore the operator's still-valid session. No raw token is returned in JSON.

Native-session authentication additionally checks that the operator of an impersonated session remains a live platform administrator. A demoted, banned, or suspended operator therefore invalidates their support context on the next request.

## Consequences

- The implementation is additive behind `NIBLEAF_NATIVE_IDENTITY_ENABLED`; existing Better Auth impersonation and dashboard routes are not cut over.
- Session expiry is a hard one-hour capability boundary. Expired active lifecycle rows are terminalized with an `expired` audit fact when encountered by a new start, explicit stop, or emergency revoke, and by the worker's periodic reconciler. The reconciler locks a bounded expiry-ordered batch with PostgreSQL `FOR UPDATE SKIP LOCKED`, records `actor_kind='system'`, uses `system:identity-impersonation-expirer`, and leaves the actor-session fields null. It is at-least-once scheduling with an idempotent `terminated_at IS NULL` fence; it does not claim exactly-once execution.
- Moderator suspension is atomic across the target account state, native/bridge session fencing, and any active native support context for that target. It terminalizes the support lifecycle with a moderator-attributed `revoked` fact before the broad target-session revocation, so suspension cannot leave a logically active support record. A dashboard audit-query UI remains follow-up work.
- Account deletion remains fail-closed on the retained ClickHouse/search erasure dependency. Both lifecycle and audit records preserve opaque IDs rather than cascading user deletion through support evidence.
