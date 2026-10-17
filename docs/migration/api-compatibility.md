# API compatibility and breaking-change policy

> **Current staged-search delta — ADR-0017 (2026-10-16):** the distinct opt-in native public transport, in-budget native/transition read-only identity, selected UTF-16/literal contract, scoped BM25/stable pre-limit ties, bounded owned JSON, aggregate-only metrics, offline rebuild and local load/network/HTTP proofs are implemented. It is **disabled by default**, with no production routing, UI/caller selection or legacy removal. Earlier statements below that transport/scoring are unimplemented describe the prior milestone and are superseded by the [current candidate contract and release boundary](../operations/public-search-candidate.md). Selected 200-UTF-16 native validation differs from installed Zod 4.5.4 scalar counting; no exact Unicode parity claim.

## Current position

Rust endpoints are a new versioned API at `/v1`; they do not silently replace the existing Hono `/app/*` or `/public/*` routes. A dashboard/proxy adapter is required during coexistence.

## Intentional differences in implemented slices

| Legacy behavior | Rust behavior | Reason / migration strategy |
|---|---|---|
| Hono app routes under `/app/projects...` | `/v1/projects...` | Avoid accidental partial route replacement. Adapter maps only accepted slices. |
| Custom `{ error: { code, message } }` errors | RFC 9457 `application/problem+json` | Consistent standard classification without internal detail. Client adapter converts during transition if required. |
| Page updates do not require a dedicated revision field | `PATCH /v1/projects/{project_id}/pages/{page_id}` requires `expected_revision` | Prevent silent lost writes. Client must read revision then retry after 409. |
| Project creation has rich TypeScript configuration/add-on behavior | Rust create takes `name`, optional `description`; creates secure minimum tenant/project defaults | Do not claim theme/add-on/starter parity. Route only eligible onboarding use case until expanded. |
| Publish creates a BullMQ job | Rust publishes via transactional PostgreSQL outbox | Preserves user intent through Redis/provider outage; external indexing/rendering parity is not yet complete. |
| Better Auth issues cookies | Native email-OTP issues opaque HttpOnly sessions with origin/CSRF fences behind its rollout gate; the bounded transition bridge accepts `nibleaf_session` or one bearer credential and rejects ambiguity | [Identity parity inventory](../architecture/identity-parity-inventory.md) and ADR-0008 govern controlled dashboard/authority cutover, not indefinite Better Auth coexistence. Google is intentionally disabled at first native cutover; no OAuth parity claim. The public-search optional actor transport is still unwired. |
| Axum's default JSON rejection response | Shared `StrictJson` maps malformed JSON, wrong content type and request-schema rejection to RFC 9457 `application/problem+json` | Clients have one documented failure envelope for every implemented JSON body. |
| Framework-generated empty 404/405/413/timeout/panic responses | Outer API normalizer returns RFC 9457 `application/problem+json`; preserves `X-Request-Id` and `Allow` on `405` | Routing and admission failures have the same contract as feature errors. |
| Legacy member routes include invitation creation/acceptance and mail delivery | Rust additionally exposes additive invitation create/cancel/accept routes under `/v1`; legacy Better Auth invitation rows and links remain authoritative | Rust invitation lifecycle has Postmark/SMTP contracts and PostgreSQL API/worker E2E. Do not route dashboard UX until the [adapter/coexistence cutover](rust-invitation-cutover.md) is approved. Owner is never directly assignable; transfer requires an existing admin. |
| Native management search initially used a minimal format-1 corpus and tokenizer-family language hint | Native snapshot/source format 2 and current runtime-bound manifest 3; exact locale, optional exact version, eligible-page corpus and additive rendering/version result fields | Explicit [source expansion](native-search-format2.md) and [runtime upgrade](native-search-runtime-upgrade.md); snapshot 1 and manifest-1/2 generations are refused, not relabeled. A new reviewed publication is required; current native snapshot 2 is still valid input. This remains member-only management search, not the legacy public replacement. |
| Legacy public search overlays live caps/enabled flags and forces the published default locale to serve | User-selected published ceilings plus live restrictions; exact latest-READY selection and one-shot consuming non-HTTP execution | [ADR-0015 policy/migration](native-public-search-scope.md) documents stricter eligibility/cap/default/malformed-control behavior and promotion-gap selection. No public HTTP contract or caller was switched; transport, response/cache/privacy/relevance, hard-resource/load operations and legacy removal remain gated. ADR-0016 now supplies supervised preparation/execution for member HTTP and a non-HTTP published facade. |

| Member management previously awaited a per-request blocking handle | Shared query admission, tracked SQL and driver-owned joined execution; optional API-only data limits/timeout | [ADR-0016 / query operations](../operations/search-query-runtime.md): oversized valid generations and capacity/deadline/stopping use existing 503 `dependency-unavailable`; DTO/private-no-store success and authorization-before-invalid-query remain. No hard drain/CPU/I/O/RSS guarantee or public transport decision. |

## Compatibility rules

1. No public endpoint is removed without an inventory disposition, consumer discovery and deprecation window.
2. New JSON input is typed and rejects unknown keys unless an extension field is explicitly documented.
3. Error schemas are stable within a major API version.
4. Cursor/page size contracts must be explicit; the Rust page list caps `limit` at 200.
5. An endpoint can move only after contract tests compare legacy and Rust observable behavior for the selected capability.

See [`../api/openapi.v1.yaml`](../api/openapi.v1.yaml) for the currently implemented contract.
