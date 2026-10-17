-- Native support impersonation is a bounded, session-scoped operator workflow.
-- Its mutable lifecycle record is distinct from the immutable audit facts so a
-- safe end/revocation can be fenced atomically without rewriting evidence.

BEGIN;

CREATE TABLE rust_identity_impersonation (
    id UUID PRIMARY KEY,
    operator_user_id TEXT NOT NULL,
    operator_session_id UUID NOT NULL,
    target_user_id TEXT NOT NULL,
    impersonation_session_id UUID NOT NULL UNIQUE,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL,
    terminated_at TIMESTAMPTZ,
    terminated_by_user_id TEXT,
    terminated_by_session_id UUID,
    termination_reason TEXT,
    CONSTRAINT rust_identity_impersonation_distinct_users CHECK (
        operator_user_id <> target_user_id
    ),
    CONSTRAINT rust_identity_impersonation_expiry_check CHECK (
        expires_at > started_at
    ),
    CONSTRAINT rust_identity_impersonation_termination_facts CHECK (
        (terminated_at IS NULL AND terminated_by_user_id IS NULL
         AND terminated_by_session_id IS NULL AND termination_reason IS NULL)
        OR
        (terminated_at IS NOT NULL AND terminated_by_user_id IS NOT NULL
         AND terminated_by_session_id IS NOT NULL
         AND termination_reason IN ('ended', 'revoked', 'expired'))
    )
);

-- A browser/operator session can enter at most one active support context. The
-- unique index is the final fence against concurrent start requests racing the
-- cookie replacement response.
CREATE UNIQUE INDEX rust_identity_impersonation_one_active_operator_session_key
    ON rust_identity_impersonation (operator_session_id)
    WHERE terminated_at IS NULL;
CREATE INDEX rust_identity_impersonation_target_active_idx
    ON rust_identity_impersonation (target_user_id, expires_at ASC)
    WHERE terminated_at IS NULL;

CREATE TABLE rust_identity_impersonation_audit (
    id UUID PRIMARY KEY,
    impersonation_id UUID NOT NULL,
    action TEXT NOT NULL,
    actor_user_id TEXT NOT NULL,
    actor_session_id UUID NOT NULL,
    target_user_id TEXT NOT NULL,
    impersonation_session_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT rust_identity_impersonation_audit_action_check CHECK (
        action IN ('started', 'ended', 'revoked', 'expired')
    )
);
CREATE INDEX rust_identity_impersonation_audit_impersonation_created_idx
    ON rust_identity_impersonation_audit (impersonation_id, created_at ASC);
CREATE INDEX rust_identity_impersonation_audit_target_created_idx
    ON rust_identity_impersonation_audit (target_user_id, created_at DESC);

-- The lifecycle row changes only to record an end/revocation. Audit rows are
-- security evidence and must remain append-only even if an application path
-- accidentally uses a broad ORM mutation.
CREATE FUNCTION rust_identity_impersonation_audit_reject_mutation()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'rust_identity_impersonation_audit facts are immutable';
END;
$$;

CREATE TRIGGER rust_identity_impersonation_audit_no_mutation
    BEFORE UPDATE OR DELETE ON rust_identity_impersonation_audit
    FOR EACH ROW EXECUTE FUNCTION rust_identity_impersonation_audit_reject_mutation();

COMMIT;
