-- Durable native identity OTP email delivery.
--
-- Identity begins before a user or organization exists, so the reusable outbox
-- supports explicitly platform-scoped events. Tenant-scoped feature events keep
-- their existing foreign key and always write a non-null tenant ID.

BEGIN;

ALTER TABLE outbox_event
    ALTER COLUMN tenant_id DROP NOT NULL;

CREATE TABLE rust_identity_otp_delivery (
    id UUID PRIMARY KEY,
    challenge_id UUID NOT NULL UNIQUE REFERENCES rust_identity_otp_challenge(id) ON DELETE CASCADE,
    ciphertext BYTEA NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',
    provider TEXT,
    provider_message_id TEXT,
    delivered_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT rust_identity_otp_delivery_ciphertext_present CHECK (
        (state = 'pending' AND octet_length(ciphertext) > 0)
        OR (state IN ('delivered', 'cancelled') AND octet_length(ciphertext) = 0)
    ),
    CONSTRAINT rust_identity_otp_delivery_state CHECK (state IN ('pending', 'delivered', 'cancelled')),
    CONSTRAINT rust_identity_otp_delivery_receipt_facts CHECK (
        (state IN ('pending', 'cancelled') AND provider IS NULL AND provider_message_id IS NULL AND delivered_at IS NULL)
        OR (state = 'delivered' AND provider IN ('postmark', 'smtp')
            AND char_length(provider_message_id) BETWEEN 1 AND 1024 AND delivered_at IS NOT NULL)
    )
);

CREATE INDEX rust_identity_otp_delivery_pending_idx
    ON rust_identity_otp_delivery (created_at ASC)
    WHERE state = 'pending';

COMMIT;
