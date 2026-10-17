# ADR-0008: Native identity cutover uses email OTP; Google sign-in is disabled

**Status:** Accepted, 2026-10-15

## Context

The current Better Auth configuration is passwordless email OTP with optional Google OAuth. The user selected native identity work rather than retaining Better Auth indefinitely. A real Google OAuth authorization-code/PKCE/provider smoke test requires a non-production Google client and approved callback configuration; none is available to this workspace.

## Decision

The first native identity cutover will implement and verify the existing email-OTP contract. Google sign-in is intentionally disabled at cutover rather than retaining a Better Auth-only Google route. Password sign-in/reset remains absent because it is already disabled in the deployed product.

This is an intentional user-facing capability break, not parity. The dashboard must disclose it before enabling native cutover, and rollback must restore the Better Auth adapter before re-enabling Google.

## Consequences

- `NIBLEAF_NATIVE_IDENTITY_ENABLED` defaults to `false`. Before it is explicitly enabled, native routes are absent and native cookies cannot authorize Rust feature routes. The flag is a rollout gate, not a declaration that cutover acceptance gates have passed.
- Native acceptance gates cover OTP issuance/delivery, sign-in/sign-up policy, session/CSRF/cookie handling, email verification/change, moderation, impersonation, deletion, organization hooks, and browser compatibility. The native sign-in OTP request and two-stage email-change transport return opaque UUID continuations on both real and ordinary-rejected request/proof paths; this is required so a browser can progress through the flow without database-only identifiers while response shape does not enumerate a target email. Every non-rate-limited request for a live account also sends the current-mailbox OTP even for a malformed, unchanged, or taken target; only an eligible target receives a durable change state and later new-mailbox OTP. This closes the otherwise observable delivery-arrival oracle. The email-change backend is an additive two-stage current-mailbox/new-mailbox OTP state machine behind the same native-cookie rollout gate. Native moderation, bounded native support sessions, and their worker expiry reconciliation are implemented under ADR-0009/0010, but dashboard migration and browser compatibility evidence remain required. Native signup now has the rich starter content/automatic first-publication behavior needed for that specific parity gate.
- Native configuration and public capability metadata must expose Google as unavailable; no OAuth route is advertised.
- Better Auth may not be removed from deployment until Google credentials are disabled and the maximum legacy session/OTP windows have elapsed, unless a later Google-native implementation passes its own provider test matrix.
- Existing account rows associated with Google are retained; no provider credentials/tokens are copied into Rust tables.
