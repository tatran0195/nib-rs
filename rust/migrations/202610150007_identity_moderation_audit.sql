-- Native moderation actions are platform-scoped but must remain attributable
-- after either actor or target account deletion. The audit facts therefore use
-- validated text identifiers rather than cascading user/project foreign keys.

BEGIN;

CREATE TABLE rust_identity_moderation_audit (
    id UUID PRIMARY KEY,
    actor_user_id TEXT NOT NULL,
    actor_session_id TEXT NOT NULL,
    action TEXT NOT NULL,
    target_user_id TEXT,
    target_project_id TEXT,
    reason TEXT,
    changed BOOLEAN NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT rust_identity_moderation_audit_action_check CHECK (
        action IN (
            'user_suspended',
            'user_unsuspended',
            'project_taken_down',
            'project_restored'
        )
    ),
    CONSTRAINT rust_identity_moderation_audit_target_check CHECK (
        (
            action IN ('user_suspended', 'user_unsuspended')
            AND target_user_id IS NOT NULL
            AND target_project_id IS NULL
        )
        OR (
            action IN ('project_taken_down', 'project_restored')
            AND target_user_id IS NULL
            AND target_project_id IS NOT NULL
        )
    ),
    CONSTRAINT rust_identity_moderation_audit_reason_check CHECK (
        reason IS NULL OR (char_length(reason) BETWEEN 1 AND 500 AND reason !~ '[[:cntrl:]]')
    ),
    CONSTRAINT rust_identity_moderation_audit_actor_session_check CHECK (
        char_length(actor_session_id) BETWEEN 1 AND 191
    )
);

CREATE INDEX rust_identity_moderation_audit_actor_created_idx
    ON rust_identity_moderation_audit (actor_user_id, created_at DESC);
CREATE INDEX rust_identity_moderation_audit_target_user_created_idx
    ON rust_identity_moderation_audit (target_user_id, created_at DESC)
    WHERE target_user_id IS NOT NULL;
CREATE INDEX rust_identity_moderation_audit_target_project_created_idx
    ON rust_identity_moderation_audit (target_project_id, created_at DESC)
    WHERE target_project_id IS NOT NULL;

-- Audit facts are application-append-only, including for a privileged process
-- that accidentally uses a broad ORM update/delete operation. Schema migration
-- and retention policy changes remain explicit operational decisions.
CREATE FUNCTION rust_identity_moderation_audit_reject_mutation()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'rust_identity_moderation_audit facts are immutable';
END;
$$;

CREATE TRIGGER rust_identity_moderation_audit_no_mutation
    BEFORE UPDATE OR DELETE ON rust_identity_moderation_audit
    FOR EACH ROW EXECUTE FUNCTION rust_identity_moderation_audit_reject_mutation();

COMMIT;
