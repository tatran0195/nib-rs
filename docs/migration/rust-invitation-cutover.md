# Rust member-invitation coexistence and cutover

## Current authority

The Rust invitation aggregate is additive and routeable at `/v1`; the legacy Better Auth `invitation` table, legacy acceptance links, dashboard flows, and notification preference behavior remain authoritative. Neither system reads or mutates the other's invitation rows.

Do **not** switch the dashboard's existing member-invite action to Rust solely because the Rust endpoint exists. That would strand legacy links and change notification-preference behavior without a reviewed migration.

## Rust API contract

All endpoints use the current opaque legacy session bridge during transition.

| Operation | Route | Core behavior |
|---|---|---|
| Create | `POST /v1/projects/{project_id}/member-invitations` | Project admin/owner only. Requires `Idempotency-Key` and `recipient_email`, `role` (`admin`/`member`), and `delivery_mode` (`manual`/`email`). |
| Cancel | `POST /v1/projects/{project_id}/member-invitations/{invitation_id}/cancel` | Project admin/owner only. Pending unexpired invitation becomes cancelled. |
| Accept | `POST /v1/member-invitations/{token}/accept` | Authenticated recipient only. Invalid, expired, cancelled, mismatched, or unauthorized tokens return `404`. |

A manual creation response includes `acceptance_url`; email creation intentionally does not return a raw token. Email delivery uses a durable outbox intent and provider receipt state. SMTP is at-least-once: acknowledgement loss can result in a duplicate send.

## Dashboard adapter requirements

Before enabling a Rust-backed dashboard action, implement an adapter that:

1. Uses the Rust API only for invitations created after the feature flag is enabled.
2. Displays the returned manual URL only to the authorized inviter and never persists it in browser analytics, error tracking, or logs.
3. Retains the legacy invitation list/acceptance route for every legacy invitation ID/link.
4. Treats `404` from acceptance as a generic unavailable link, not as recipient/address/status information.
5. Surfaces email delivery as asynchronous intent, not success; show a copyable manual fallback only when policy permits.
6. Sends one idempotency key per user intent and retries the same key only for the identical request body.

## Controlled cutover

1. **Preflight:** deploy additive migration, API, and worker; configure `NIBLEAF_APP_URL`, sender, signing key, and at least one provider. Confirm metrics and dead-letter runbook access.
2. **Shadow/allowlist:** enable Rust invitation creation only for internal test organizations. Keep legacy creation default for all other organizations.
3. **Provider smoke test:** from an approved non-production Postmark account and SMTP relay, verify receipt persistence, link delivery, recipient mismatch `404`, cancellation, and acceptance.
4. **Dashboard rollout:** enable the adapter per organization. Do not alter old links or the legacy table.
5. **Observation period:** monitor delivery dead letters, retry/reclaim rate, unauthorized acceptance `404` rate, and membership creation anomalies for at least the maximum invitation TTL plus operational review time.
6. **Default switch:** make Rust creation the default only after the observation gate is approved. Legacy acceptance stays enabled until every legacy invitation has expired or been cancelled.
7. **Retirement:** after the maximum legacy invitation TTL and a database/reporting audit, remove the legacy creation path in a separate release. Preserve a rollback playbook; never bulk-convert tokens.

## Rollback

Disable only the dashboard adapter/feature flag. Existing Rust invitations remain valid through their expiry and continue through the Rust worker. Do not delete Rust aggregate, delivery, or outbox rows and do not rewrite Better Auth rows. If a provider outage occurs, preserve delivery receipts and use the normal dead-letter/replay procedure after operator review.
