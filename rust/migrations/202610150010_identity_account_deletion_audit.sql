-- Immutable evidence for the deliberately narrow first native account-deletion
-- path. It records only opaque identifiers and has no foreign keys so deleting
-- the subject account cannot erase the deletion fact itself.
--
-- The API initially permits physical deletion only for a direct native session
-- whose user has no workspace memberships. Any account with a membership must
-- continue through the future retained-store erasure workflow; this table must
-- not be interpreted as evidence that that broader workflow is implemented.

CREATE TABLE rust_identity_account_deletion_audit (
    id UUID PRIMARY KEY,
    deleted_user_id TEXT NOT NULL,
    actor_session_id UUID NOT NULL,
    action TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT rust_identity_account_deletion_audit_user_id_bounded CHECK (
        char_length(deleted_user_id) BETWEEN 1 AND 191
        AND deleted_user_id = btrim(deleted_user_id)
    ),
    CONSTRAINT rust_identity_account_deletion_audit_action CHECK (
        action = 'self_deleted_without_memberships'
    )
);
CREATE INDEX rust_identity_account_deletion_audit_user_created_idx
    ON rust_identity_account_deletion_audit (deleted_user_id, created_at DESC);

CREATE OR REPLACE FUNCTION rust_identity_account_deletion_audit_reject_mutation()
RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'rust_identity_account_deletion_audit facts are immutable';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER rust_identity_account_deletion_audit_immutable
BEFORE UPDATE OR DELETE ON rust_identity_account_deletion_audit
FOR EACH ROW EXECUTE FUNCTION rust_identity_account_deletion_audit_reject_mutation();
