#![forbid(unsafe_code)]

//! Disposable PostgreSQL proof for native session and OTP state transitions.

use std::{env, io};

use nibleaf_core::{Actor, OrganizationId, UserId};
use nibleaf_identity::{
    AccountDeletionError, AuthenticatedSession, EncryptionKey, IdentityProfileError,
    IdentitySessionError, IssuedSession, OtpChallengeError, OtpPurpose, authenticate_session,
    confirm_email_change, consume_otp_challenge, current_profile, decrypt_secret,
    delete_direct_account_preserving_shared_workspaces, issue_otp_challenge, issue_session,
    queue_otp_challenge_delivery, request_email_change, resolve_or_create_otp_account,
    revoke_session, select_active_organization, update_profile_name, verify_csrf,
    verify_current_email_change,
};
use sqlx::PgPool;
use time::Duration;

#[tokio::test]
async fn native_session_and_otp_lifecycles_are_fenced_in_postgres()
-> Result<(), Box<dyn std::error::Error>> {
    if env::var("NIBLEAF_IDENTITY_E2E").is_err() {
        return Ok(());
    }
    let database_url = env::var("NIBLEAF_TEST_DATABASE_URL")?;
    let pool = PgPool::connect(&database_url).await?;
    let user_id = "identity-e2e-user";
    let email = "identity-e2e@example.test";
    sqlx::query(
        "INSERT INTO \"user\" (id, name, email, \"emailVerified\", role, banned, \"createdAt\", \"updatedAt\") VALUES ($1,'Identity E2E',$2,TRUE,'user',FALSE,NOW(),NOW()) ON CONFLICT (id) DO UPDATE SET banned=FALSE, \"suspendedAt\"=NULL",
    )
    .bind(user_id)
    .bind(email)
    .execute(&pool)
    .await?;
    let user_id = UserId::try_from(user_id.to_owned())?;
    let (issued, authenticated) = assert_profile_and_active_workspace(&pool, &user_id).await?;
    assert_read_only_identity_and_originating_support_session(&pool).await?;
    verify_csrf(&authenticated, &issued.csrf_secret)?;
    assert!(matches!(
        verify_csrf(&authenticated, "not-a-csrf-secret"),
        Err(IdentitySessionError::Unavailable)
    ));
    assert!(revoke_session(&pool, issued.session_id, &user_id, "sign_out").await?);
    assert!(matches!(
        authenticate_session(&pool, &issued.session_token).await,
        Err(IdentitySessionError::Unavailable)
    ));

    assert_encrypted_delivery_and_account_intent(&pool).await?;
    assert_two_stage_email_change(&pool).await?;
    assert_account_deletion_fences(&pool).await?;
    assert_provisioner_membership_race_is_fenced(&pool).await?;
    assert_shared_workspace_membership_race_is_fenced(&pool).await?;
    assert_concurrent_shared_member_deletions_preserve_tenant(&pool).await?;

    let consumed = issue_otp_challenge(&pool, email, OtpPurpose::SignIn).await?;
    let consumed_facts =
        consume_otp_challenge(&pool, consumed.challenge_id, &consumed.code).await?;
    assert_eq!(consumed_facts.normalized_email, email);
    assert_eq!(consumed_facts.purpose, OtpPurpose::SignIn);
    assert!(matches!(
        consume_otp_challenge(&pool, consumed.challenge_id, &consumed.code).await,
        Err(OtpChallengeError::Rejected)
    ));

    let locked =
        issue_otp_challenge(&pool, "identity-lock@example.test", OtpPurpose::SignIn).await?;
    for _ in 0..3 {
        assert!(matches!(
            consume_otp_challenge(&pool, locked.challenge_id, "nototp").await,
            Err(OtpChallengeError::Rejected)
        ));
    }
    let state: String =
        sqlx::query_scalar("SELECT state FROM rust_identity_otp_challenge WHERE id=$1")
            .bind(locked.challenge_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(state, "locked");

    for _ in 0..3 {
        issue_otp_challenge(&pool, "identity-rate@example.test", OtpPurpose::VerifyEmail).await?;
    }
    assert!(matches!(
        issue_otp_challenge(&pool, "identity-rate@example.test", OtpPurpose::VerifyEmail).await,
        Err(OtpChallengeError::RateLimited)
    ));
    Ok(())
}

async fn assert_profile_and_active_workspace(
    pool: &PgPool,
    user_id: &UserId,
) -> Result<(IssuedSession, AuthenticatedSession), Box<dyn std::error::Error>> {
    let first_organization_id =
        OrganizationId::try_from("identity-e2e-organization-one".to_owned())?;
    let second_organization_id =
        OrganizationId::try_from("identity-e2e-organization-two".to_owned())?;
    sqlx::query(
        "INSERT INTO organization (id, name, slug) VALUES ($1, 'Identity E2E One', 'identity-e2e-one'), ($2, 'Identity E2E Two', 'identity-e2e-two')",
    )
    .bind(first_organization_id.as_str())
    .bind(second_organization_id.as_str())
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO member (id, \"organizationId\", \"userId\", role) VALUES ('identity-e2e-member-one', $1, $2, 'owner'), ('identity-e2e-member-two', $3, $2, 'member')",
    )
    .bind(first_organization_id.as_str())
    .bind(user_id.as_str())
    .bind(second_organization_id.as_str())
    .execute(pool)
    .await?;

    let issued = issue_session(pool, user_id, None, None, Duration::hours(1)).await?;
    let authenticated = authenticate_session(pool, &issued.session_token).await?;
    // The session begins with no stored preference, as sign-in can precede the
    // async first-workspace worker. Profile/session reads nevertheless return a
    // deterministic first live membership, exactly like the legacy middleware.
    let initial_profile = current_profile(pool, &authenticated).await?;
    assert_eq!(initial_profile.name, "Identity E2E");
    assert_eq!(
        initial_profile.active_organization_id.as_deref(),
        Some(first_organization_id.as_str())
    );
    let renamed_profile = update_profile_name(pool, &authenticated, "  Native Identity  ").await?;
    assert_eq!(renamed_profile.name, "Native Identity");
    assert!(matches!(
        update_profile_name(pool, &authenticated, "\u{0000}").await,
        Err(IdentityProfileError::InvalidName)
    ));
    assert!(matches!(
        select_active_organization(
            pool,
            &authenticated,
            &OrganizationId::try_from("identity-e2e-not-a-member".to_owned())?,
        )
        .await,
        Err(IdentitySessionError::InvalidActiveOrganization)
    ));
    let selected_profile =
        select_active_organization(pool, &authenticated, &second_organization_id).await?;
    assert_eq!(
        selected_profile.active_organization_id.as_deref(),
        Some(second_organization_id.as_str())
    );
    Ok((issued, authenticated))
}

async fn assert_encrypted_delivery_and_account_intent(
    pool: &PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let delivery_key = EncryptionKey::new(1, &[42; 32])?;
    let queued = queue_otp_challenge_delivery(
        pool,
        "identity-delivery@example.test",
        OtpPurpose::SignIn,
        &delivery_key,
    )
    .await?;
    let pending_delivery: (String, i32) = sqlx::query_as(
        "SELECT d.state, octet_length(d.ciphertext) FROM rust_identity_otp_delivery AS d WHERE d.challenge_id=$1",
    )
    .bind(queued.challenge_id)
    .fetch_one(pool)
    .await?;
    assert_eq!(pending_delivery.0, "pending");
    assert!(pending_delivery.1 > 6);
    let queued_event: (Option<String>, serde_json::Value) = sqlx::query_as(
        "SELECT tenant_id, payload FROM outbox_event WHERE aggregate_id=$1 AND event_type='identity.otp.delivery.requested'",
    )
    .bind(queued.challenge_id.to_string())
    .fetch_one(pool)
    .await?;
    assert_eq!(queued_event.0, None);
    assert!(queued_event.1.get("delivery_id").is_some());
    assert_eq!(
        queued_event.1.as_object().map(serde_json::Map::len),
        Some(1)
    );

    let resolved = resolve_or_create_otp_account(
        pool,
        "identity-new-account@example.test",
        Some("New Account"),
        false,
    )
    .await?;
    assert_eq!(
        resolved,
        resolve_or_create_otp_account(
            pool,
            "identity-new-account@example.test",
            Some("Ignored Later Name"),
            false,
        )
        .await?
    );
    let provision_event: (Option<String>, serde_json::Value) = sqlx::query_as(
        "SELECT tenant_id, payload FROM outbox_event WHERE aggregate_id=$1 AND event_type='identity.account.provision_requested'",
    )
    .bind(resolved.as_str())
    .fetch_one(pool)
    .await?;
    assert_eq!(provision_event.0, None);
    assert_eq!(provision_event.1["user_id"], resolved.as_str());
    Ok(())
}

async fn assert_account_deletion_fences(pool: &PgPool) -> Result<(), Box<dyn std::error::Error>> {
    assert_zero_membership_deletion(pool).await?;
    assert_deletion_rejections(pool).await
}

async fn assert_zero_membership_deletion(pool: &PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let deletable_user = create_deletion_test_user(
        pool,
        "identity-delete-zero",
        "identity-delete-zero@example.test",
    )
    .await?;
    let deletable_session =
        issue_session(pool, &deletable_user, None, None, Duration::hours(1)).await?;
    let pending_event_id = uuid::Uuid::now_v7();
    sqlx::query(
        r"INSERT INTO outbox_event (
              id, tenant_id, aggregate_type, aggregate_id, event_type, schema_version,
              payload, correlation_id
           ) VALUES ($1, NULL, 'identity_account', $2,
                     'identity.account.provision_requested', 1, $3, $4)",
    )
    .bind(pending_event_id)
    .bind(deletable_user.as_str())
    .bind(serde_json::json!({"user_id": deletable_user.as_str()}))
    .bind(uuid::Uuid::now_v7())
    .execute(pool)
    .await?;
    delete_direct_account_preserving_shared_workspaces(
        pool,
        &deletion_actor(&deletable_user, deletable_session.session_id),
    )
    .await?;
    let deleted: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM \"user\" WHERE id=$1)")
        .bind(deletable_user.as_str())
        .fetch_one(pool)
        .await?;
    assert!(!deleted);
    let audit: (String, String) = sqlx::query_as(
        "SELECT deleted_user_id, action FROM rust_identity_account_deletion_audit WHERE actor_session_id=$1",
    )
    .bind(deletable_session.session_id)
    .fetch_one(pool)
    .await?;
    assert_eq!(audit.0, deletable_user.as_str());
    assert_eq!(audit.1, "self_deleted_without_memberships");
    let cancelled: (String, String) =
        sqlx::query_as("SELECT state, last_error FROM outbox_event WHERE id=$1")
            .bind(pending_event_id)
            .fetch_one(pool)
            .await?;
    assert_eq!(cancelled.0, "dead_letter");
    assert_eq!(
        cancelled.1,
        "identity account deleted before workspace provisioning"
    );
    Ok(())
}

async fn assert_deletion_rejections(pool: &PgPool) -> Result<(), Box<dyn std::error::Error>> {
    assert_membership_deletion_rejection(pool).await?;
    assert_invalid_ownership_deletion_rejection(pool).await?;
    assert_shared_membership_deletion(pool).await?;
    assert_mixed_membership_deletion_is_atomic(pool).await?;
    assert_processing_and_stale_deletion_rejections(pool).await
}

async fn assert_membership_deletion_rejection(
    pool: &PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let member_user = create_deletion_test_user(
        pool,
        "identity-delete-member",
        "identity-delete-member@example.test",
    )
    .await?;
    let member_session = issue_session(pool, &member_user, None, None, Duration::hours(1)).await?;
    sqlx::query("INSERT INTO organization (id, name, slug) VALUES ('identity-delete-organization', 'Deletion Fence', 'deletion-fence')")
        .execute(pool)
        .await?;
    sqlx::query(
        r#"INSERT INTO member (id, "organizationId", "userId", role)
           VALUES ('identity-delete-membership', 'identity-delete-organization', $1, 'owner')"#,
    )
    .bind(member_user.as_str())
    .execute(pool)
    .await?;
    assert!(matches!(
        delete_direct_account_preserving_shared_workspaces(
            pool,
            &deletion_actor(&member_user, member_session.session_id),
        )
        .await,
        Err(AccountDeletionError::RetainedScopeRequired)
    ));
    let member_still_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM \"user\" WHERE id=$1)")
            .bind(member_user.as_str())
            .fetch_one(pool)
            .await?;
    assert!(member_still_exists);
    Ok(())
}

/// Historical projectless organizations can lack the project-owner constraint.
/// Preserve such a workspace rather than treating a non-owner departure as safe.
async fn assert_invalid_ownership_deletion_rejection(
    pool: &PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let departing_user = create_deletion_test_user(
        pool,
        "identity-delete-invalid-owner-departing",
        "identity-delete-invalid-owner-departing@example.test",
    )
    .await?;
    let other_user = create_deletion_test_user(
        pool,
        "identity-delete-invalid-owner-other",
        "identity-delete-invalid-owner-other@example.test",
    )
    .await?;
    let session = issue_session(pool, &departing_user, None, None, Duration::hours(1)).await?;
    sqlx::query(
        "INSERT INTO organization (id, name, slug) VALUES ('identity-delete-invalid-owner-organization', 'Invalid Owner Deletion', 'invalid-owner-deletion')",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"INSERT INTO member (id, "organizationId", "userId", role) VALUES
             ('identity-delete-invalid-owner-departing-member', 'identity-delete-invalid-owner-organization', $1, 'member'),
             ('identity-delete-invalid-owner-other-member', 'identity-delete-invalid-owner-organization', $2, 'admin')"#,
    )
    .bind(departing_user.as_str())
    .bind(other_user.as_str())
    .execute(pool)
    .await?;
    assert!(matches!(
        delete_direct_account_preserving_shared_workspaces(
            pool,
            &deletion_actor(&departing_user, session.session_id),
        )
        .await,
        Err(AccountDeletionError::TenantInvariant)
    ));
    let remaining_members: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM member WHERE \"organizationId\"='identity-delete-invalid-owner-organization'",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(remaining_members, 2);
    let audit_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM rust_identity_account_deletion_audit WHERE actor_session_id=$1",
    )
    .bind(session.session_id)
    .fetch_one(pool)
    .await?;
    assert_eq!(audit_count, 0);
    Ok(())
}

async fn assert_shared_membership_deletion(
    pool: &PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let departing_user = create_deletion_test_user(
        pool,
        "identity-delete-shared-owner",
        "identity-delete-shared-owner@example.test",
    )
    .await?;
    let successor_user = create_deletion_test_user(
        pool,
        "identity-delete-shared-successor",
        "identity-delete-shared-successor@example.test",
    )
    .await?;
    let session = issue_session(pool, &departing_user, None, None, Duration::hours(1)).await?;
    sqlx::query(
        "INSERT INTO organization (id, name, slug) VALUES ('identity-delete-shared-organization', 'Shared Deletion', 'shared-deletion')",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO organization (id, name, slug) VALUES ('identity-delete-shared-member-organization', 'Shared Member Deletion', 'shared-member-deletion')",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"INSERT INTO member (id, "organizationId", "userId", role)
           VALUES
             ('identity-delete-shared-owner-member', 'identity-delete-shared-organization', $1, 'owner'),
             ('identity-delete-shared-successor-member', 'identity-delete-shared-organization', $2, 'member'),
             ('identity-delete-shared-departing-member', 'identity-delete-shared-member-organization', $1, 'member'),
             ('identity-delete-shared-existing-owner', 'identity-delete-shared-member-organization', $2, 'owner')"#,
    )
    .bind(departing_user.as_str())
    .bind(successor_user.as_str())
    .execute(pool)
    .await?;
    delete_direct_account_preserving_shared_workspaces(
        pool,
        &deletion_actor(&departing_user, session.session_id),
    )
    .await?;

    let successor_role: String = sqlx::query_scalar(
        "SELECT role FROM member WHERE \"organizationId\"='identity-delete-shared-organization' AND \"userId\"=$1",
    )
    .bind(successor_user.as_str())
    .fetch_one(pool)
    .await?;
    assert_eq!(successor_role, "owner");
    let retained_members: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM member WHERE \"organizationId\"='identity-delete-shared-organization'",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(retained_members, 1);
    let retained_shared_member_workspace: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM member WHERE \"organizationId\"='identity-delete-shared-member-organization' AND \"userId\"=$1 AND role='owner'",
    )
    .bind(successor_user.as_str())
    .fetch_one(pool)
    .await?;
    assert_eq!(retained_shared_member_workspace, 1);
    let action: String = sqlx::query_scalar(
        "SELECT action FROM rust_identity_account_deletion_audit WHERE actor_session_id=$1",
    )
    .bind(session.session_id)
    .fetch_one(pool)
    .await?;
    assert_eq!(action, "self_deleted_after_membership_departure");
    Ok(())
}

/// A deletion that encounters one sole-member tenant must not depart an earlier
/// shared tenant: planning completes before any role/member mutation.
async fn assert_mixed_membership_deletion_is_atomic(
    pool: &PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let departing_user = create_deletion_test_user(
        pool,
        "identity-delete-mixed-departing",
        "identity-delete-mixed-departing@example.test",
    )
    .await?;
    let remaining_user = create_deletion_test_user(
        pool,
        "identity-delete-mixed-remaining",
        "identity-delete-mixed-remaining@example.test",
    )
    .await?;
    let session = issue_session(pool, &departing_user, None, None, Duration::hours(1)).await?;
    sqlx::query(
        r"INSERT INTO organization (id, name, slug) VALUES
            ('identity-delete-mixed-shared', 'Mixed Shared Deletion', 'mixed-shared-deletion'),
            ('identity-delete-mixed-sole', 'Mixed Sole Deletion', 'mixed-sole-deletion')",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"INSERT INTO member (id, "organizationId", "userId", role) VALUES
             ('identity-delete-mixed-shared-departing', 'identity-delete-mixed-shared', $1, 'owner'),
             ('identity-delete-mixed-shared-remaining', 'identity-delete-mixed-shared', $2, 'member'),
             ('identity-delete-mixed-sole-departing', 'identity-delete-mixed-sole', $1, 'owner')"#,
    )
    .bind(departing_user.as_str())
    .bind(remaining_user.as_str())
    .execute(pool)
    .await?;

    assert!(matches!(
        delete_direct_account_preserving_shared_workspaces(
            pool,
            &deletion_actor(&departing_user, session.session_id),
        )
        .await,
        Err(AccountDeletionError::RetainedScopeRequired)
    ));
    let shared_roles: Vec<(String, String)> = sqlx::query_as(
        r#"SELECT "userId", role FROM member
           WHERE "organizationId"='identity-delete-mixed-shared'
           ORDER BY "userId" ASC"#,
    )
    .fetch_all(pool)
    .await?;
    assert_eq!(
        shared_roles,
        vec![
            (departing_user.as_str().to_owned(), "owner".to_owned()),
            (remaining_user.as_str().to_owned(), "member".to_owned()),
        ]
    );
    let user_still_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM \"user\" WHERE id=$1)")
            .bind(departing_user.as_str())
            .fetch_one(pool)
            .await?;
    assert!(user_still_exists);
    let audit_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM rust_identity_account_deletion_audit WHERE actor_session_id=$1",
    )
    .bind(session.session_id)
    .fetch_one(pool)
    .await?;
    assert_eq!(audit_count, 0);
    Ok(())
}

async fn assert_processing_and_stale_deletion_rejections(
    pool: &PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let processing_user = create_deletion_test_user(
        pool,
        "identity-delete-processing",
        "identity-delete-processing@example.test",
    )
    .await?;
    let processing_session =
        issue_session(pool, &processing_user, None, None, Duration::hours(1)).await?;
    sqlx::query(
        r"INSERT INTO outbox_event (
              id, tenant_id, aggregate_type, aggregate_id, event_type, schema_version, payload,
              correlation_id, state, lease_token, leased_until
           ) VALUES ($1, NULL, 'identity_account', $2,
                     'identity.account.provision_requested', 1, $3, $4,
                     'processing', $5, NOW() + interval '5 minutes')",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(processing_user.as_str())
    .bind(serde_json::json!({"user_id": processing_user.as_str()}))
    .bind(uuid::Uuid::now_v7())
    .bind(uuid::Uuid::now_v7())
    .execute(pool)
    .await?;
    assert!(matches!(
        delete_direct_account_preserving_shared_workspaces(
            pool,
            &deletion_actor(&processing_user, processing_session.session_id),
        )
        .await,
        Err(AccountDeletionError::ProvisioningInProgress)
    ));

    let stale_user = create_deletion_test_user(
        pool,
        "identity-delete-stale",
        "identity-delete-stale@example.test",
    )
    .await?;
    let stale_session = issue_session(pool, &stale_user, None, None, Duration::hours(1)).await?;
    assert!(
        revoke_session(
            pool,
            stale_session.session_id,
            &stale_user,
            "deletion_stale_test",
        )
        .await?
    );
    assert!(matches!(
        delete_direct_account_preserving_shared_workspaces(
            pool,
            &deletion_actor(&stale_user, stale_session.session_id)
        )
        .await,
        Err(AccountDeletionError::Unavailable)
    ));
    assert!(matches!(
        delete_direct_account_preserving_shared_workspaces(
            pool,
            &Actor {
                impersonated_by: Some("support-operator".to_owned()),
                ..deletion_actor(&stale_user, stale_session.session_id)
            },
        )
        .await,
        Err(AccountDeletionError::Forbidden)
    ));
    Ok(())
}

async fn create_deletion_test_user(
    pool: &PgPool,
    id: &str,
    email: &str,
) -> Result<UserId, Box<dyn std::error::Error>> {
    sqlx::query(
        r#"INSERT INTO "user" (id, name, email, "emailVerified", role, banned, "createdAt", "updatedAt")
           VALUES ($1, 'Deletion E2E', $2, TRUE, 'user', FALSE, NOW(), NOW())"#,
    )
    .bind(id)
    .bind(email)
    .execute(pool)
    .await?;
    Ok(UserId::try_from(id.to_owned())?)
}

fn deletion_actor(user_id: &UserId, session_id: uuid::Uuid) -> Actor {
    Actor {
        user_id: user_id.clone(),
        session_id: session_id.to_string(),
        impersonated_by: None,
    }
}

/// Concurrent departures of the final owner and another member must leave an
/// owner and a live account rather than deleting a tenant whose final member remains.
async fn assert_concurrent_shared_member_deletions_preserve_tenant(
    pool: &PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    const FIRST_USER_ID: &str = "identity-delete-concurrent-owner-first";
    const SECOND_USER_ID: &str = "identity-delete-concurrent-owner-second";
    const ORGANIZATION_ID: &str = "identity-delete-concurrent-owner-organization";
    let first = create_deletion_test_user(
        pool,
        FIRST_USER_ID,
        "identity-delete-concurrent-owner-first@example.test",
    )
    .await?;
    let second = create_deletion_test_user(
        pool,
        SECOND_USER_ID,
        "identity-delete-concurrent-owner-second@example.test",
    )
    .await?;
    let first_session = issue_session(pool, &first, None, None, Duration::hours(1)).await?;
    let second_session = issue_session(pool, &second, None, None, Duration::hours(1)).await?;
    sqlx::query(
        "INSERT INTO organization (id, name, slug) VALUES ($1, 'Concurrent Owner Deletion', 'concurrent-owner-deletion')",
    )
    .bind(ORGANIZATION_ID)
    .execute(pool)
    .await?;
    sqlx::query(
        r#"INSERT INTO member (id, "organizationId", "userId", role) VALUES
             ('identity-delete-concurrent-owner-first-member', $1, $2, 'owner'),
             ('identity-delete-concurrent-owner-second-member', $1, $3, 'admin')"#,
    )
    .bind(ORGANIZATION_ID)
    .bind(first.as_str())
    .bind(second.as_str())
    .execute(pool)
    .await?;

    let first_pool = pool.clone();
    let first_actor = deletion_actor(&first, first_session.session_id);
    let first_deletion = tokio::spawn(async move {
        delete_direct_account_preserving_shared_workspaces(&first_pool, &first_actor).await
    });
    let second_pool = pool.clone();
    let second_actor = deletion_actor(&second, second_session.session_id);
    let second_deletion = tokio::spawn(async move {
        delete_direct_account_preserving_shared_workspaces(&second_pool, &second_actor).await
    });
    let (first_result, second_result) = tokio::join!(first_deletion, second_deletion);
    let outcomes = [first_result?, second_result?];
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(
        outcomes
            .iter()
            .any(|result| matches!(result, Err(AccountDeletionError::RetainedScopeRequired))),
        "unexpected concurrent deletion outcomes: {outcomes:?}"
    );
    let surviving_members: Vec<(String, String)> =
        sqlx::query_as(r#"SELECT "userId", role FROM member WHERE "organizationId"=$1"#)
            .bind(ORGANIZATION_ID)
            .fetch_all(pool)
            .await?;
    assert_eq!(surviving_members.len(), 1);
    assert_eq!(surviving_members[0].1, "owner");
    let surviving_user: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM \"user\" WHERE id=$1)")
            .bind(&surviving_members[0].0)
            .fetch_one(pool)
            .await?;
    assert!(surviving_user);
    Ok(())
}

/// A departing owner holds the organization lock while its successor is
/// promoted. A concurrent membership insert must not alter the sole/shared
/// classification after it has been planned.
async fn assert_shared_workspace_membership_race_is_fenced(
    pool: &PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    const DEPARTING_USER_ID: &str = "identity-delete-shared-race-departing";
    const SUCCESSOR_USER_ID: &str = "identity-delete-shared-race-successor";
    const JOINING_USER_ID: &str = "identity-delete-shared-race-joining";
    const ORGANIZATION_ID: &str = "identity-delete-shared-race-organization";
    const DEPARTING_MEMBER_ID: &str = "identity-delete-shared-race-departing-member";

    let departing = create_deletion_test_user(
        pool,
        DEPARTING_USER_ID,
        "identity-delete-shared-race-departing@example.test",
    )
    .await?;
    let successor = create_deletion_test_user(
        pool,
        SUCCESSOR_USER_ID,
        "identity-delete-shared-race-successor@example.test",
    )
    .await?;
    let joining = create_deletion_test_user(
        pool,
        JOINING_USER_ID,
        "identity-delete-shared-race-joining@example.test",
    )
    .await?;
    let session = issue_session(pool, &departing, None, None, Duration::hours(1)).await?;
    sqlx::query(
        "INSERT INTO organization (id, name, slug) VALUES ($1, 'Shared Lock Race', 'shared-lock-race')",
    )
    .bind(ORGANIZATION_ID)
    .execute(pool)
    .await?;
    sqlx::query(
        r#"INSERT INTO member (id, "organizationId", "userId", role) VALUES
             ($1, $2, $3, 'owner'),
             ('identity-delete-shared-race-successor-member', $2, $4, 'member')"#,
    )
    .bind(DEPARTING_MEMBER_ID)
    .bind(ORGANIZATION_ID)
    .bind(departing.as_str())
    .bind(successor.as_str())
    .execute(pool)
    .await?;
    install_owner_transfer_pause_trigger(pool, DEPARTING_MEMBER_ID).await?;

    let deletion_pool = pool.clone();
    let deletion_actor = deletion_actor(&departing, session.session_id);
    let deletion = tokio::spawn(async move {
        delete_direct_account_preserving_shared_workspaces(&deletion_pool, &deletion_actor).await
    });
    wait_until_organization_update_lock_is_held(pool, ORGANIZATION_ID).await?;

    let mut joining_transaction = pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '250ms'")
        .execute(&mut *joining_transaction)
        .await?;
    let insert = sqlx::query(
        r#"INSERT INTO member (id, "organizationId", "userId", role)
           VALUES ('identity-delete-shared-race-joining-member', $1, $2, 'member')"#,
    )
    .bind(ORGANIZATION_ID)
    .bind(joining.as_str())
    .execute(&mut *joining_transaction)
    .await;
    let Err(insert_error) = insert else {
        return Err(io::Error::other("concurrent member insert unexpectedly committed").into());
    };
    assert_eq!(
        insert_error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("55P03")
    );
    drop(joining_transaction);
    deletion.await??;
    remove_owner_transfer_pause_trigger(pool).await?;

    let successor_role: String =
        sqlx::query_scalar("SELECT role FROM member WHERE \"organizationId\"=$1 AND \"userId\"=$2")
            .bind(ORGANIZATION_ID)
            .bind(successor.as_str())
            .fetch_one(pool)
            .await?;
    assert_eq!(successor_role, "owner");
    let joining_membership_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM member WHERE \"organizationId\"=$1 AND \"userId\"=$2)",
    )
    .bind(ORGANIZATION_ID)
    .bind(joining.as_str())
    .fetch_one(pool)
    .await?;
    assert!(!joining_membership_exists);
    Ok(())
}

async fn install_owner_transfer_pause_trigger(
    pool: &PgPool,
    departing_member_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        r"CREATE FUNCTION rust_identity_test_pause_owner_transfer()
           RETURNS trigger AS $$
           BEGIN
             IF OLD.id = 'identity-delete-shared-race-departing-member' THEN
               PERFORM pg_sleep(0.75);
             END IF;
             RETURN NEW;
           END;
           $$ LANGUAGE plpgsql",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r"CREATE TRIGGER rust_identity_test_pause_owner_transfer
           BEFORE UPDATE OF role ON member
           FOR EACH ROW EXECUTE FUNCTION rust_identity_test_pause_owner_transfer()",
    )
    .execute(pool)
    .await?;
    // The function body uses a literal because PostgreSQL trigger definitions
    // cannot bind values. This assertion keeps the caller/test fixture coupled.
    assert_eq!(
        departing_member_id,
        "identity-delete-shared-race-departing-member"
    );
    Ok(())
}

async fn remove_owner_transfer_pause_trigger(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("DROP TRIGGER rust_identity_test_pause_owner_transfer ON member")
        .execute(pool)
        .await?;
    sqlx::query("DROP FUNCTION rust_identity_test_pause_owner_transfer()")
        .execute(pool)
        .await?;
    Ok(())
}

async fn wait_until_organization_update_lock_is_held(
    pool: &PgPool,
    organization_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..20 {
        let probe = sqlx::query_scalar::<_, String>(
            "SELECT id FROM organization WHERE id=$1 FOR KEY SHARE NOWAIT",
        )
        .bind(organization_id)
        .fetch_optional(pool)
        .await;
        match probe {
            Err(error)
                if error
                    .as_database_error()
                    .and_then(sqlx::error::DatabaseError::code)
                    .is_some_and(|code| code == "55P03") =>
            {
                return Ok(());
            }
            Ok(Some(_)) => tokio::time::sleep(std::time::Duration::from_millis(25)).await,
            Ok(None) => {
                return Err(io::Error::other(
                    "race test organization disappeared before lock probe",
                )
                .into());
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(io::Error::other("deletion never acquired the organization row lock").into())
}

/// A starter-workspace provisioner can read the user before deletion starts,
/// but its later membership insert needs a conflicting FK lock. The deletion
/// transaction must either see an already-created membership or make that
/// provisioner transaction roll back; it must never leave an orphan tenant.
async fn assert_provisioner_membership_race_is_fenced(
    pool: &PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    const USER_ID: &str = "identity-delete-membership-race";
    const ORGANIZATION_ID: &str = "identity-delete-race-organization";
    let user =
        create_deletion_test_user(pool, USER_ID, "identity-delete-race@example.test").await?;
    let session = issue_session(pool, &user, None, None, Duration::hours(1)).await?;
    install_delete_pause_trigger(pool).await?;

    let deletion_pool = pool.clone();
    let deletion_actor = deletion_actor(&user, session.session_id);
    let deletion = tokio::spawn(async move {
        delete_direct_account_preserving_shared_workspaces(&deletion_pool, &deletion_actor).await
    });
    wait_until_user_delete_lock_is_held(pool, USER_ID).await?;

    // This models the provisioner's all-or-nothing foundation transaction. Its
    // organization insert precedes the membership FK insert in real code. A
    // lock timeout is expected while deletion owns the user row; dropping the
    // failed transaction must roll its foundation row back too.
    let mut provisioner = pool.begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '250ms'")
        .execute(&mut *provisioner)
        .await?;
    sqlx::query(
        "INSERT INTO organization (id, name, slug) VALUES ($1, 'Deletion Race', 'deletion-race')",
    )
    .bind(ORGANIZATION_ID)
    .execute(&mut *provisioner)
    .await?;
    let insert = sqlx::query(
        r#"INSERT INTO member (id, "organizationId", "userId", role)
           VALUES ('identity-delete-race-member', $1, $2, 'owner')"#,
    )
    .bind(ORGANIZATION_ID)
    .bind(USER_ID)
    .execute(&mut *provisioner)
    .await;
    let Err(insert_error) = insert else {
        return Err(io::Error::other("provisioner membership unexpectedly committed").into());
    };
    let code = insert_error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .map(std::borrow::Cow::into_owned);
    if !matches!(code.as_deref(), Some("55P03" | "23503")) {
        return Err(io::Error::other(format!(
            "unexpected provisioner membership error code: {code:?}"
        ))
        .into());
    }
    drop(provisioner);
    deletion.await??;
    remove_delete_pause_trigger(pool).await?;

    let user_exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM \"user\" WHERE id=$1)")
        .bind(USER_ID)
        .fetch_one(pool)
        .await?;
    let orphan_organization: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM organization WHERE id=$1)")
            .bind(ORGANIZATION_ID)
            .fetch_one(pool)
            .await?;
    assert!(!user_exists);
    assert!(!orphan_organization);
    Ok(())
}

async fn install_delete_pause_trigger(pool: &PgPool) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        r"CREATE FUNCTION rust_identity_test_pause_user_delete()
           RETURNS trigger AS $$
           BEGIN
             IF OLD.id = 'identity-delete-membership-race' THEN
               PERFORM pg_sleep(0.75);
             END IF;
             RETURN OLD;
           END;
           $$ LANGUAGE plpgsql",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"CREATE TRIGGER rust_identity_test_pause_user_delete
           BEFORE DELETE ON "user"
           FOR EACH ROW EXECUTE FUNCTION rust_identity_test_pause_user_delete()"#,
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn remove_delete_pause_trigger(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("DROP TRIGGER rust_identity_test_pause_user_delete ON \"user\"")
        .execute(pool)
        .await?;
    sqlx::query("DROP FUNCTION rust_identity_test_pause_user_delete()")
        .execute(pool)
        .await?;
    Ok(())
}

async fn wait_until_user_delete_lock_is_held(
    pool: &PgPool,
    user_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..20 {
        let probe = sqlx::query_scalar::<_, String>(
            "SELECT id FROM \"user\" WHERE id=$1 FOR KEY SHARE NOWAIT",
        )
        .bind(user_id)
        .fetch_optional(pool)
        .await;
        match probe {
            Err(error)
                if error
                    .as_database_error()
                    .and_then(sqlx::error::DatabaseError::code)
                    .is_some_and(|code| code == "55P03") =>
            {
                return Ok(());
            }
            Ok(Some(_)) => tokio::time::sleep(std::time::Duration::from_millis(25)).await,
            Ok(None) => {
                return Err(
                    io::Error::other("race test user disappeared before lock probe").into(),
                );
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(io::Error::other("deletion never acquired the user row lock").into())
}

async fn assert_two_stage_email_change(pool: &PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let user_id = UserId::try_from("identity-email-change-user".to_owned())?;
    sqlx::query(
        "INSERT INTO \"user\" (id,name,email,\"emailVerified\",role,banned,\"createdAt\",\"updatedAt\") VALUES ($1,'Email Change','email-change-old@example.test',TRUE,'user',FALSE,NOW(),NOW())",
    )
    .bind(user_id.as_str())
    .execute(pool)
    .await?;
    let key = EncryptionKey::new(1, &[91; 32])?;
    let current =
        request_email_change(pool, &user_id, "email-change-new@example.test", &key).await?;
    let current_code = delivery_code(pool, current.challenge_id, &key).await?;
    let next =
        verify_current_email_change(pool, &user_id, current.challenge_id, &current_code, &key)
            .await?;
    let next_code = delivery_code(pool, next.challenge_id, &key).await?;
    confirm_email_change(pool, &user_id, next.challenge_id, &next_code).await?;
    let email: String = sqlx::query_scalar("SELECT email FROM \"user\" WHERE id=$1")
        .bind(user_id.as_str())
        .fetch_one(pool)
        .await?;
    assert_eq!(email, "email-change-new@example.test");
    let state: String = sqlx::query_scalar(
        "SELECT state FROM rust_identity_email_change WHERE user_id=$1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(user_id.as_str())
    .fetch_one(pool)
    .await?;
    assert_eq!(state, "completed");
    assert!(
        confirm_email_change(pool, &user_id, next.challenge_id, &next_code)
            .await
            .is_err()
    );
    Ok(())
}

async fn delivery_code(
    pool: &PgPool,
    challenge_id: uuid::Uuid,
    key: &EncryptionKey,
) -> Result<String, Box<dyn std::error::Error>> {
    let ciphertext: Vec<u8> = sqlx::query_scalar(
        "SELECT ciphertext FROM rust_identity_otp_delivery WHERE challenge_id=$1",
    )
    .bind(challenge_id)
    .fetch_one(pool)
    .await?;
    Ok(String::from_utf8(decrypt_secret(
        std::slice::from_ref(key),
        &ciphertext,
        b"nibleaf.identity.otp.delivery.v1",
    )?)?)
}

async fn assert_read_only_identity_and_originating_support_session(
    pool: &PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let operator =
        create_deletion_test_user(pool, "read-only-operator", "readonly-operator@example.test")
            .await?;
    let target =
        create_deletion_test_user(pool, "read-only-target", "readonly-target@example.test").await?;
    sqlx::query("UPDATE \"user\" SET role='admin' WHERE id=$1")
        .bind(operator.as_str())
        .execute(pool)
        .await?;
    let origin = issue_session(pool, &operator, None, None, Duration::hours(1)).await?;
    let actor = Actor {
        user_id: operator.clone(),
        session_id: origin.session_id.to_string(),
        impersonated_by: None,
    };
    let support = nibleaf_identity::start_impersonation(pool, &actor, &target, None).await?;
    {
        let mut tx = pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await?;
        let resolved = nibleaf_identity::authenticate_read_only_actor_on_connection(
            &mut tx,
            &support.issued_session.session_token,
        )
        .await?;
        assert_eq!(resolved.user_id, target);
        tx.commit().await?;
    }
    let stale_support = authenticate_session(pool, &support.issued_session.session_token).await?;
    revoke_session(pool, origin.session_id, &operator, "origin_revoked_proof").await?;
    assert!(matches!(
        nibleaf_identity::rotate_csrf_secret(pool, &stale_support).await,
        Err(IdentitySessionError::Unavailable)
    ));

    assert!(matches!(
        authenticate_session(pool, &support.issued_session.session_token).await,
        Err(IdentitySessionError::Unavailable)
    ));
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut *tx)
        .await?;
    assert!(matches!(
        nibleaf_identity::authenticate_read_only_actor_on_connection(
            &mut tx,
            &support.issued_session.session_token
        )
        .await,
        Err(IdentitySessionError::Unavailable)
    ));
    tx.rollback().await?;
    Ok(())
}
