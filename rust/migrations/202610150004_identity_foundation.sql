-- Additive foundation for the native Rust identity replacement.
--
-- These tables deliberately coexist with Better Auth's user/session/account/
-- verification rows. No route reads or writes this foundation until a complete
-- security slice has cookie, CSRF, OTP, OAuth, moderation, deletion, and
-- compatibility coverage; do not import raw Better Auth tokens or credentials.

CREATE TABLE rust_identity_session (
    id UUID PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    token_hash BYTEA NOT NULL UNIQUE,
    csrf_secret_hash BYTEA NOT NULL,
    active_organization_id TEXT REFERENCES organization(id) ON DELETE SET NULL,
    impersonated_by TEXT REFERENCES "user"(id) ON DELETE SET NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    revoke_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT rust_identity_session_token_hash_length CHECK (octet_length(token_hash) = 32),
    CONSTRAINT rust_identity_session_csrf_hash_length CHECK (octet_length(csrf_secret_hash) = 32),
    CONSTRAINT rust_identity_session_revocation_facts CHECK (
        (revoked_at IS NULL AND revoke_reason IS NULL)
        OR (revoked_at IS NOT NULL AND revoke_reason IS NOT NULL AND char_length(revoke_reason) BETWEEN 1 AND 120)
    )
);
CREATE INDEX rust_identity_session_user_open_idx
    ON rust_identity_session (user_id, expires_at ASC)
    WHERE revoked_at IS NULL;

CREATE TABLE rust_identity_otp_challenge (
    id UUID PRIMARY KEY,
    normalized_email TEXT NOT NULL,
    purpose TEXT NOT NULL,
    code_hash BYTEA NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    attempts SMALLINT NOT NULL DEFAULT 0,
    max_attempts SMALLINT NOT NULL DEFAULT 3,
    sent_count SMALLINT NOT NULL DEFAULT 1,
    state TEXT NOT NULL DEFAULT 'pending',
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT rust_identity_otp_email_normalized CHECK (
        normalized_email = btrim(normalized_email)
        AND normalized_email = lower(normalized_email)
        AND char_length(normalized_email) BETWEEN 3 AND 320
    ),
    CONSTRAINT rust_identity_otp_purpose CHECK (purpose IN ('sign_in', 'verify_email', 'change_email')),
    CONSTRAINT rust_identity_otp_code_hash_length CHECK (octet_length(code_hash) = 32),
    CONSTRAINT rust_identity_otp_attempts CHECK (attempts >= 0 AND max_attempts BETWEEN 1 AND 10 AND attempts <= max_attempts),
    CONSTRAINT rust_identity_otp_send_count CHECK (sent_count BETWEEN 1 AND 10),
    CONSTRAINT rust_identity_otp_state CHECK (state IN ('pending', 'consumed', 'expired', 'locked')),
    CONSTRAINT rust_identity_otp_terminal_facts CHECK (
        (state = 'consumed' AND consumed_at IS NOT NULL)
        OR (state <> 'consumed' AND consumed_at IS NULL)
    )
);
CREATE INDEX rust_identity_otp_lookup_idx
    ON rust_identity_otp_challenge (normalized_email, purpose, created_at DESC);

CREATE TABLE rust_identity_oauth_transaction (
    id UUID PRIMARY KEY,
    provider TEXT NOT NULL,
    state_hash BYTEA NOT NULL UNIQUE,
    pkce_verifier_ciphertext BYTEA NOT NULL,
    pkce_key_version SMALLINT NOT NULL,
    redirect_uri TEXT NOT NULL,
    intended_user_id TEXT REFERENCES "user"(id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT rust_identity_oauth_provider CHECK (provider IN ('google')),
    CONSTRAINT rust_identity_oauth_state_hash_length CHECK (octet_length(state_hash) = 32),
    CONSTRAINT rust_identity_oauth_pkce_ciphertext_not_empty CHECK (octet_length(pkce_verifier_ciphertext) > 0),
    CONSTRAINT rust_identity_oauth_key_version CHECK (pkce_key_version > 0),
    CONSTRAINT rust_identity_oauth_redirect_uri_bounded CHECK (char_length(redirect_uri) BETWEEN 1 AND 2048)
);
CREATE INDEX rust_identity_oauth_unconsumed_expiry_idx
    ON rust_identity_oauth_transaction (expires_at ASC)
    WHERE consumed_at IS NULL;

CREATE TABLE rust_identity_external_account (
    id UUID PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    provider_account_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT rust_identity_external_account_provider CHECK (provider IN ('google')),
    CONSTRAINT rust_identity_external_account_subject_bounded CHECK (char_length(provider_account_id) BETWEEN 1 AND 512),
    CONSTRAINT rust_identity_external_account_provider_subject_unique UNIQUE (provider, provider_account_id),
    CONSTRAINT rust_identity_external_account_user_provider_unique UNIQUE (user_id, provider)
);
