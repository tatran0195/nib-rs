# ADR-0007: Member invitations are an authenticated, durable, signed-token workflow

- **Status:** Accepted for implementation; no Rust invitation route is exposed yet.
- **Date:** 2026-10-15

## Context and evidence

Source inspection found that the legacy member workflow in
`apps/server/src/actions/members.ts`:

- normalizes an invitation address with `trim().toLowerCase()`;
- grants only a non-owner role, rejects an already-present member, writes a
  legacy `invitation` row with `status = pending` and a seven-day expiry;
- returns a copyable accept link even if best-effort BullMQ email enqueue fails;
- deletes an invitation on cancellation; and
- relies on Better Auth's organization `acceptInvitation` endpoint. The web
  accept page sends the legacy invitation ID to that endpoint only after sign
  in, and its UI blocks an address mismatch.

The current legacy `invitation` table has only its opaque ID, organization,
email, nullable role, status, expiry and inviter. It has no normalized-address
constraint, acceptance/cancellation timestamps, recipient-safe delivery state,
idempotency key, or unguessable token hash. Its primary key is also the value
placed in an email link. Reusing it for a new Rust endpoint would conflate two
active authorities and make secure replay/delivery semantics impossible to
prove while Better Auth remains live.

The Rust platform does already have a transactional outbox with leased,
at-least-once processing. Its worker deliberately has no mail provider yet;
legacy email supports both Postmark and SMTP. Treating an outbox event as
"delivered" without calling one of those providers would be a false delivery
claim.

## Decision

The Rust invitation slice will use an **additive `member_invitation` aggregate**
and will not mutate or repurpose legacy `invitation` rows. Legacy invitation
links continue to be served by Better Auth until the dashboard adapter moves to
the documented Rust contract.

The slice is only complete when creation, delivery intent, cancellation,
acceptance, expiration, and idempotency are all present. There will be no
record-only Rust invite endpoint.

### Aggregate and constraints

The migration will add an organization-owned invitation aggregate with:

- an opaque text/UUID identifier and immutable organization, inviter, requested
  `admin` or `member` role, and canonical recipient email;
- `pending`, `accepted`, and `cancelled` facts with timestamps; expiry remains
  a server-clock predicate rather than a client-controlled state transition;
- a unique tenant-scoped idempotency key plus request fingerprint; replay with
  changed normalized email, role, or delivery mode is `409`;
- a partial unique constraint for one pending, unexpired-equivalent invitation
  per organization/canonical recipient, enforced with transaction-time expiry
  handling because PostgreSQL partial-index predicates cannot depend on
  `now()`;
- a recipient/delivery record that is never returned on anonymous endpoints;
  no raw accept token is stored in PostgreSQL.

The compatibility canonicalization is explicit: reject control characters,
trim leading/trailing Unicode whitespace, and apply Unicode lowercase before
validating the bounded email input. This mirrors the existing lowercasing
behavior while making the rule testable. IDN and provider-specific alias
rewrites are intentionally **not** applied: they can route mail to a different
mailbox and must not be guessed.

The aggregate is scoped by the project's organization. Creation and
cancellation acquire the existing project membership advisory transaction lock
and re-authorize the actor within the transaction. Existing-member checks,
pending-invite checks, and the outbox append occur in the same transaction.
Owners and admins may invite only `admin`/`member`; `owner` remains exclusively
an ownership-transfer role.

### Acceptance token and authorization

A mail link carries a versioned, HMAC-SHA-256-signed token containing only invitation
identity and expiration. The Rust `nibleaf-membership` token primitive now has
URL-safe round-trip, tamper, expiry and previous-key rotation unit coverage.
Both Rust binaries now require a secret-managed Base64 32-byte current key at
startup and can parse a bounded configured previous-key ring; it does not yet
have an HTTP caller. The future worker can mint it from the invitation row, so
an outbox payload and database snapshot do not retain an acceptance secret.
Key rotation mints with the current key and verifies unexpired older links only
against the configured previous keys.

`POST /v1/member-invitations/{token}/accept` requires the existing Rust `Actor`.
It locks the aggregate and current user, verifies the signature, expiration,
pending state, and canonical equality of the invite recipient and current user
email, then inserts the membership and marks acceptance in one transaction.
The database unique `(organizationId, userId)` membership constraint is the
final race backstop. Repeating an already accepted invitation by its recipient
returns the same accepted membership; if the recipient has already joined by
another safe route, acceptance records that fact without changing the existing
member role. Invalid, expired, cancelled, or recipient-mismatched tokens map
to `404` so the endpoint is not an invitation or email-address oracle.

### Email and delivery

Creation has an explicit delivery mode:

- `email` is the default. Creation writes a
  `membership.invitation.delivery.requested` outbox event atomically with the
  aggregate and returns `202`; a configured Postmark or SMTP worker adapter
  loads the pending aggregate, mints the signed link, and sends the message.
- `manual` is an explicit, documented authorized-admin workflow. It returns the
  one-time signed URL to that admin after atomically recording the invitation;
  it is not a hidden no-delivery fallback.

**Compatibility scope selected on 2026-10-15:** the first Rust invitation route
requires both verified legacy provider modes—Postmark and SMTP. It will not
route Postmark tenants to Rust while leaving SMTP tenants with a silently
weaker/different contract. Provider selection follows the legacy precedence
(Postmark when configured, otherwise SMTP); a missing provider is a
configuration/delivery failure, not successful invitation delivery.

The worker stores delivery attempts and provider message identifiers where the
provider supplies one. SMTP has no portable idempotency protocol: a crash after
SMTP accepts a message but before PostgreSQL acknowledgement can cause a
second message. This is deliberately **at-least-once**, documented and
metered; the implementation must never claim exactly-once mail delivery.
Cancellation prevents later delivery/acceptance but cannot retract a message
already handed to a provider.

## API contract to implement

The selected new `/v1` contract is intentionally separate from legacy Better
Auth routes:

- `POST /projects/{project_id}/member-invitations` — admin/owner, typed email,
  role, delivery mode, required `Idempotency-Key`; `202` for email intent and
  a documented manual-delivery response;
- `GET /projects/{project_id}/member-invitations` — member-visible pending and
  terminal operational metadata only, with no token;
- `DELETE /projects/{project_id}/member-invitations/{invitation_id}` —
  admin/owner cancellation under the membership lock;
- `POST /member-invitations/{token}/accept` — authenticated recipient only;
  no public invitation-info endpoint is required or introduced.

Every mutation has cross-tenant, role-bound, expired/cancelled, duplicate,
email-mismatch, retry/idempotency, concurrent-acceptance, and at-least-once
mail-failure coverage against PostgreSQL. The OpenAPI and migration/adapter
compatibility documents are updated only when these routes are implemented.

## Consequences

This introduces a second, explicitly versioned invitation aggregate during the
mixed-runtime period, rather than silently changing Better Auth behavior. It
costs a migration and a real mail adapter, but keeps old IDs/links working,
removes invitation-email disclosure from new public surfaces, makes token
secrecy auditable, and gives delivery a durable recovery path.
