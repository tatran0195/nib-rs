-- Two-stage native email change: prove the current mailbox, then the new one.
-- Both OTP references are identity-owned and code hashes/ciphertexts remain in
-- their dedicated challenge/delivery tables.

BEGIN;

CREATE TABLE rust_identity_email_change (
    id UUID PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    old_normalized_email TEXT NOT NULL,
    new_normalized_email TEXT NOT NULL,
    current_challenge_id UUID NOT NULL UNIQUE REFERENCES rust_identity_otp_challenge(id) ON DELETE RESTRICT,
    new_challenge_id UUID UNIQUE REFERENCES rust_identity_otp_challenge(id) ON DELETE RESTRICT,
    state TEXT NOT NULL DEFAULT 'pending_current',
    expires_at TIMESTAMPTZ NOT NULL,
    current_verified_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT rust_identity_email_change_emails_normalized CHECK (
        old_normalized_email = lower(btrim(old_normalized_email))
        AND new_normalized_email = lower(btrim(new_normalized_email))
        AND old_normalized_email <> new_normalized_email
        AND char_length(old_normalized_email) BETWEEN 3 AND 320
        AND char_length(new_normalized_email) BETWEEN 3 AND 320
    ),
    CONSTRAINT rust_identity_email_change_state CHECK (
        state IN ('pending_current', 'pending_new', 'completed', 'cancelled')
    ),
    CONSTRAINT rust_identity_email_change_facts CHECK (
        (state = 'pending_current' AND new_challenge_id IS NULL AND current_verified_at IS NULL AND completed_at IS NULL)
        OR (state = 'pending_new' AND new_challenge_id IS NOT NULL AND current_verified_at IS NOT NULL AND completed_at IS NULL)
        OR (state = 'completed' AND new_challenge_id IS NOT NULL AND current_verified_at IS NOT NULL AND completed_at IS NOT NULL)
        OR (state = 'cancelled' AND completed_at IS NULL)
    )
);
CREATE UNIQUE INDEX rust_identity_email_change_one_active_per_user
    ON rust_identity_email_change (user_id)
    WHERE state IN ('pending_current', 'pending_new');
CREATE INDEX rust_identity_email_change_pending_new_idx
    ON rust_identity_email_change (new_challenge_id)
    WHERE state = 'pending_new';

COMMIT;
