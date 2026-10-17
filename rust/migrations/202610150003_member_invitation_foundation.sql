-- Additive foundation for ADR-0007. This deliberately does not repurpose the
-- legacy Better Auth `invitation` table: legacy IDs/links remain authoritative
-- until the complete Rust invitation lifecycle and dashboard adapter ship.
--
-- No endpoint uses these tables in this migration. The subsequent slice must
-- write its aggregate, delivery receipt and outbox event in one transaction.
CREATE TABLE member_invitation (
    id UUID PRIMARY KEY,
    project_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    recipient_email TEXT NOT NULL,
    requested_role TEXT NOT NULL,
    delivery_mode TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    expires_at TIMESTAMPTZ NOT NULL,
    inviter_id TEXT NOT NULL,
    idempotency_key_hash BYTEA NOT NULL,
    request_fingerprint BYTEA NOT NULL,
    accepted_at TIMESTAMPTZ,
    accepted_member_id TEXT,
    cancelled_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT member_invitation_project_tenant_fkey
        FOREIGN KEY (project_id, organization_id)
        REFERENCES project (id, "organizationId") ON DELETE CASCADE,
    CONSTRAINT member_invitation_inviter_fkey
        FOREIGN KEY (inviter_id) REFERENCES "user" (id) ON DELETE RESTRICT,
    CONSTRAINT member_invitation_accepted_member_fkey
        FOREIGN KEY (accepted_member_id) REFERENCES member (id) ON DELETE RESTRICT,
    CONSTRAINT member_invitation_email_normalized_check
        CHECK (
            recipient_email = btrim(recipient_email)
            AND recipient_email = lower(recipient_email)
            AND char_length(recipient_email) BETWEEN 3 AND 320
        ),
    CONSTRAINT member_invitation_role_check
        CHECK (requested_role IN ('admin', 'member')),
    CONSTRAINT member_invitation_delivery_mode_check
        CHECK (delivery_mode IN ('email', 'manual')),
    CONSTRAINT member_invitation_status_check
        CHECK (status IN ('pending', 'accepted', 'cancelled', 'expired')),
    CONSTRAINT member_invitation_expiry_check
        CHECK (expires_at > created_at),
    CONSTRAINT member_invitation_idempotency_hash_check
        CHECK (octet_length(idempotency_key_hash) = 32),
    CONSTRAINT member_invitation_fingerprint_check
        CHECK (octet_length(request_fingerprint) = 32),
    CONSTRAINT member_invitation_terminal_facts_check
        CHECK (
            (status = 'pending' AND accepted_at IS NULL AND accepted_member_id IS NULL AND cancelled_at IS NULL)
            OR (status = 'accepted' AND accepted_at IS NOT NULL AND accepted_member_id IS NOT NULL AND cancelled_at IS NULL)
            OR (status = 'cancelled' AND cancelled_at IS NOT NULL AND accepted_at IS NULL AND accepted_member_id IS NULL)
            OR (status = 'expired' AND accepted_at IS NULL AND accepted_member_id IS NULL AND cancelled_at IS NULL)
        )
);

CREATE UNIQUE INDEX member_invitation_organization_idempotency_key_key
    ON member_invitation (organization_id, idempotency_key_hash);
-- Expired pending invitations must be transitioned to `expired` under the
-- project advisory lock before creating a replacement. A partial unique index
-- cannot safely use NOW() in its predicate.
CREATE UNIQUE INDEX member_invitation_one_pending_recipient_key
    ON member_invitation (organization_id, recipient_email)
    WHERE status = 'pending';
CREATE INDEX member_invitation_project_status_expires_idx
    ON member_invitation (project_id, status, expires_at ASC);

CREATE TABLE member_invitation_delivery (
    invitation_id UUID PRIMARY KEY REFERENCES member_invitation (id) ON DELETE CASCADE,
    state TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0,
    available_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    leased_until TIMESTAMPTZ,
    lease_token UUID,
    provider TEXT,
    provider_message_id TEXT,
    last_error TEXT,
    sent_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT member_invitation_delivery_state_check
        CHECK (state IN ('pending', 'processing', 'sent', 'dead_letter')),
    CONSTRAINT member_invitation_delivery_attempts_check CHECK (attempts >= 0),
    CONSTRAINT member_invitation_delivery_lease_check
        CHECK (
            (state = 'processing' AND leased_until IS NOT NULL AND lease_token IS NOT NULL)
            OR state <> 'processing'
        )
);
CREATE INDEX member_invitation_delivery_claim_idx
    ON member_invitation_delivery (available_at ASC, invitation_id)
    WHERE state IN ('pending', 'processing');
