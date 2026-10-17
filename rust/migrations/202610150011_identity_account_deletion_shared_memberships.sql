-- Expand immutable account-deletion evidence for the safe shared-workspace
-- case. A tenant is never deleted by this migration or route: an account may
-- depart a shared workspace, transferring the last owner role transactionally
-- when needed, while a sole-member workspace remains fail-closed.

ALTER TABLE rust_identity_account_deletion_audit
    DROP CONSTRAINT rust_identity_account_deletion_audit_action;

ALTER TABLE rust_identity_account_deletion_audit
    ADD CONSTRAINT rust_identity_account_deletion_audit_action CHECK (
        action IN (
            'self_deleted_without_memberships',
            'self_deleted_after_membership_departure'
        )
    );
