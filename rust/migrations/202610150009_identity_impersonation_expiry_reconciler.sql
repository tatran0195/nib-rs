-- A periodic expiry reconciler is a system actor, not a user/session. Preserve
-- pre-existing user-attributed expiry evidence while permitting new expiry facts
-- to identify the reconciler without fabricating an operator session.

BEGIN;

ALTER TABLE rust_identity_impersonation
    DROP CONSTRAINT rust_identity_impersonation_termination_facts;
ALTER TABLE rust_identity_impersonation
    ALTER COLUMN terminated_by_session_id DROP NOT NULL;
ALTER TABLE rust_identity_impersonation
    ADD CONSTRAINT rust_identity_impersonation_termination_facts CHECK (
        (terminated_at IS NULL AND terminated_by_user_id IS NULL
         AND terminated_by_session_id IS NULL AND termination_reason IS NULL)
        OR
        (
            terminated_at IS NOT NULL
            AND terminated_by_user_id IS NOT NULL
            AND termination_reason IN ('ended', 'revoked', 'expired')
            AND (
                (termination_reason IN ('ended', 'revoked') AND terminated_by_session_id IS NOT NULL)
                OR (
                    termination_reason = 'expired'
                    AND (
                        (
                            terminated_by_session_id IS NOT NULL
                            AND terminated_by_user_id <> 'system:identity-impersonation-expirer'
                        )
                        OR (
                            terminated_by_session_id IS NULL
                            AND terminated_by_user_id = 'system:identity-impersonation-expirer'
                        )
                    )
                )
            )
        )
    );

ALTER TABLE rust_identity_impersonation_audit
    ADD COLUMN actor_kind TEXT NOT NULL DEFAULT 'user';
ALTER TABLE rust_identity_impersonation_audit
    ALTER COLUMN actor_session_id DROP NOT NULL;
ALTER TABLE rust_identity_impersonation_audit
    ADD CONSTRAINT rust_identity_impersonation_audit_actor_check CHECK (
        (actor_kind = 'user' AND actor_session_id IS NOT NULL)
        OR (
            actor_kind = 'system'
            AND actor_user_id = 'system:identity-impersonation-expirer'
            AND actor_session_id IS NULL
        )
    );

-- This index has a stable predicate. In contrast, a partial predicate involving
-- NOW() would be invalid and would cease to represent moving expiry time.
CREATE INDEX rust_identity_impersonation_active_expiry_idx
    ON rust_identity_impersonation (expires_at ASC, id ASC)
    WHERE terminated_at IS NULL;

COMMIT;
