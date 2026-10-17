//! Narrow, fail-closed native self-account deletion.
//!
//! This is intentionally **not** a replacement for Nibleaf's retained-store
//! organization erasure. It deletes a direct native-session account only when
//! it has no memberships or when every membership belongs to a shared workspace
//! that can retain its tenant after a safe owner transfer/departure. A sole
//! member workspace is rejected before relational deletion because `ClickHouse`,
//! search, storage, exports, and Git-retention erasure still require their own
//! verified durable workflow.

use nibleaf_core::Actor;
use sqlx::{PgPool, Postgres, Transaction};
use thiserror::Error;
use uuid::Uuid;

const ACCOUNT_PROVISION_EVENT_TYPE: &str = "identity.account.provision_requested";
const ZERO_MEMBERSHIP_DELETION_ACTION: &str = "self_deleted_without_memberships";
const SHARED_MEMBERSHIP_DELETION_ACTION: &str = "self_deleted_after_membership_departure";
const CANCELLED_PROVISIONING_ERROR: &str = "identity account deleted before workspace provisioning";

#[derive(sqlx::FromRow)]
struct MembershipRow {
    organization_id: String,
}

#[derive(sqlx::FromRow)]
struct OrganizationMemberRow {
    id: String,
    user_id: String,
    role: String,
}

struct SharedMembershipDeparture {
    organization_id: String,
    actor_was_sole_owner: bool,
    successor_member_id: Option<String>,
}

#[derive(Debug, Error)]
pub enum AccountDeletionError {
    #[error("account-deletion persistence failed")]
    Database(#[from] sqlx::Error),
    #[error("account deletion requires a direct native user session")]
    Forbidden,
    #[error("identity session is unavailable")]
    Unavailable,
    #[error("account has a sole-member workspace requiring retained-store erasure")]
    RetainedScopeRequired,
    #[error("workspace ownership is invalid and account deletion is fail-closed")]
    TenantInvariant,
    #[error("account workspace provisioning is currently in progress")]
    ProvisioningInProgress,
}

/// Deletes a direct native-session user while preserving every shared tenant.
///
/// The transaction locks the exact session/user and every membership/organization
/// before planning any change. A shared workspace retains its tenant; if the
/// departing account is its only owner, the earliest other member becomes owner
/// before the departing membership is removed. A sole-member workspace returns
/// `RetainedScopeRequired` without changing anything because it needs retained-
/// store erasure. The transaction also refuses a claimed provisioning event,
/// cancels pending provisioning, writes immutable deletion evidence, and finally
/// deletes the user. Native/legacy sessions and credentials cascade through
/// existing foreign keys.
///
/// # Errors
/// Returns `RetainedScopeRequired` only when the user is the sole member of at
/// least one workspace. The caller must not work around that error with a
/// relational delete: tenant-owned retained-store erasure remains a separate
/// fail-closed workflow. Returns `ProvisioningInProgress` when a worker already
/// owns the starter-workspace intent.
pub async fn delete_direct_account_preserving_shared_workspaces(
    pool: &PgPool,
    actor: &Actor,
) -> Result<(), AccountDeletionError> {
    if actor.impersonated_by.is_some() {
        return Err(AccountDeletionError::Forbidden);
    }
    let session_id =
        Uuid::parse_str(&actor.session_id).map_err(|_| AccountDeletionError::Unavailable)?;
    let mut transaction = pool.begin().await?;
    lock_live_direct_account(&mut transaction, actor, session_id).await?;

    let provisioning_states: Vec<String> = sqlx::query_scalar(
        r"SELECT state
           FROM outbox_event
           WHERE aggregate_type = 'identity_account'
             AND aggregate_id = $1
             AND event_type = $2
           FOR UPDATE",
    )
    .bind(actor.user_id.as_str())
    .bind(ACCOUNT_PROVISION_EVENT_TYPE)
    .fetch_all(&mut *transaction)
    .await?;
    if provisioning_states
        .iter()
        .any(|state| state == "processing")
    {
        return Err(AccountDeletionError::ProvisioningInProgress);
    }

    let departures = lock_and_plan_shared_membership_departures(&mut transaction, actor).await?;
    for departure in &departures {
        if departure.actor_was_sole_owner {
            // The partial unique owner index requires this order. The account
            // membership is removed in the same transaction immediately after
            // every successor is promoted, so no externally visible state can
            // observe the temporary non-owner actor row.
            sqlx::query(
                "UPDATE member SET role='admin' WHERE \"organizationId\"=$1 AND \"userId\"=$2",
            )
            .bind(&departure.organization_id)
            .bind(actor.user_id.as_str())
            .execute(&mut *transaction)
            .await?;
            let successor = departure
                .successor_member_id
                .as_deref()
                .ok_or(AccountDeletionError::Unavailable)?;
            let promoted = sqlx::query("UPDATE member SET role='owner' WHERE id=$1")
                .bind(successor)
                .execute(&mut *transaction)
                .await?;
            if promoted.rows_affected() != 1 {
                return Err(AccountDeletionError::Unavailable);
            }
        }
    }
    if !departures.is_empty() {
        sqlx::query("DELETE FROM member WHERE \"userId\"=$1")
            .bind(actor.user_id.as_str())
            .execute(&mut *transaction)
            .await?;
    }

    // A pending event is locked above, so a worker claim cannot race this state
    // transition. Delivered/dead-letter historical evidence is retained.
    sqlx::query(
        r"UPDATE outbox_event
           SET state = 'dead_letter',
               lease_token = NULL,
               leased_until = NULL,
               worker_id = NULL,
               last_error = $3
           WHERE aggregate_type = 'identity_account'
             AND aggregate_id = $1
             AND event_type = $2
             AND state = 'pending'",
    )
    .bind(actor.user_id.as_str())
    .bind(ACCOUNT_PROVISION_EVENT_TYPE)
    .bind(CANCELLED_PROVISIONING_ERROR)
    .execute(&mut *transaction)
    .await?;

    sqlx::query(
        r"INSERT INTO rust_identity_account_deletion_audit (
              id, deleted_user_id, actor_session_id, action
           ) VALUES ($1, $2, $3, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(actor.user_id.as_str())
    .bind(session_id)
    .bind(if departures.is_empty() {
        ZERO_MEMBERSHIP_DELETION_ACTION
    } else {
        SHARED_MEMBERSHIP_DELETION_ACTION
    })
    .execute(&mut *transaction)
    .await?;

    let deleted = sqlx::query("DELETE FROM \"user\" WHERE id = $1")
        .bind(actor.user_id.as_str())
        .execute(&mut *transaction)
        .await?;
    if deleted.rows_affected() != 1 {
        return Err(AccountDeletionError::Unavailable);
    }
    transaction.commit().await?;
    Ok(())
}

/// Locks every current membership and its organization before deciding whether
/// account deletion can preserve a tenant. A locked organization prevents a
/// concurrent member insert from changing a sole/shared classification through
/// its foreign-key lock while this transaction is in progress.
async fn lock_and_plan_shared_membership_departures(
    transaction: &mut Transaction<'_, Postgres>,
    actor: &Actor,
) -> Result<Vec<SharedMembershipDeparture>, AccountDeletionError> {
    // First serialize all concurrent account departures on organization rows
    // only. Locking each actor's membership before the shared organization can
    // deadlock: two departing members can each hold their own row and then
    // wait while the winner tries to lock every member. The organization-first
    // order makes the later per-member locks single-file for a workspace.
    let memberships = sqlx::query_as::<_, MembershipRow>(
        r#"SELECT membership."organizationId" AS organization_id
           FROM member AS membership
           INNER JOIN organization AS organization
             ON organization.id = membership."organizationId"
           WHERE membership."userId" = $1
           ORDER BY membership."organizationId" ASC
           FOR UPDATE OF organization"#,
    )
    .bind(actor.user_id.as_str())
    .fetch_all(&mut **transaction)
    .await?;
    let mut departures = Vec::with_capacity(memberships.len());
    for membership in memberships {
        let membership_role: Option<String> = sqlx::query_scalar(
            r#"SELECT role
               FROM member
               WHERE "organizationId" = $1 AND "userId" = $2
               FOR UPDATE"#,
        )
        .bind(&membership.organization_id)
        .bind(actor.user_id.as_str())
        .fetch_optional(&mut **transaction)
        .await?;
        let Some(membership_role) = membership_role else {
            // A non-deletion membership mutation raced after organization
            // locking. Do not make a plan from a stale membership snapshot.
            return Err(AccountDeletionError::Unavailable);
        };
        let members = sqlx::query_as::<_, OrganizationMemberRow>(
            r#"SELECT id, "userId" AS user_id, role
               FROM member
               WHERE "organizationId" = $1
               ORDER BY "createdAt" ASC, id ASC
               FOR UPDATE"#,
        )
        .bind(&membership.organization_id)
        .fetch_all(&mut **transaction)
        .await?;
        let others: Vec<&OrganizationMemberRow> = members
            .iter()
            .filter(|member| member.user_id != actor.user_id.as_str())
            .collect();
        let Some(first_other) = others.first() else {
            // This is a tenant-bearing account. It needs the durable retained
            // store workflow; changing memberships or relational rows here
            // would falsely claim that external data was erased.
            return Err(AccountDeletionError::RetainedScopeRequired);
        };
        let other_owner_exists = others.iter().any(|member| member.role == "owner");
        if membership_role != "owner" && !other_owner_exists {
            return Err(AccountDeletionError::TenantInvariant);
        }
        let actor_was_sole_owner = membership_role == "owner" && !other_owner_exists;
        departures.push(SharedMembershipDeparture {
            organization_id: membership.organization_id,
            actor_was_sole_owner,
            successor_member_id: actor_was_sole_owner.then(|| first_other.id.clone()),
        });
    }
    Ok(departures)
}

async fn lock_live_direct_account(
    transaction: &mut Transaction<'_, Postgres>,
    actor: &Actor,
    session_id: Uuid,
) -> Result<(), AccountDeletionError> {
    let locked_session: Option<Uuid> = sqlx::query_scalar(
        r#"SELECT session.id
           FROM rust_identity_session AS session
           INNER JOIN "user" AS user_account ON user_account.id = session.user_id
           WHERE session.id = $1
             AND session.user_id = $2
             AND session.impersonated_by IS NULL
             AND session.revoked_at IS NULL
             AND session.expires_at > NOW()
             AND user_account."suspendedAt" IS NULL
             AND user_account.banned = FALSE
           FOR UPDATE OF session, user_account"#,
    )
    .bind(session_id)
    .bind(actor.user_id.as_str())
    .fetch_optional(&mut **transaction)
    .await?;
    if locked_session.is_some() {
        Ok(())
    } else {
        Err(AccountDeletionError::Unavailable)
    }
}
