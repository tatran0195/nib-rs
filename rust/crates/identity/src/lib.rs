#![forbid(unsafe_code)]

//! Native Rust identity primitives.
//!
//! This crate intentionally owns only transport-neutral secrets and verification
//! rules. HTTP cookie attributes, rate limiting, transactions, email delivery,
//! and OAuth provider calls stay in their respective identity vertical slices.

mod account;
mod account_deletion;
mod cookie;
mod encryption;
mod http;
mod impersonation;
mod moderation;
mod otp;
mod otp_challenge;
mod otp_delivery;
mod profile;
mod secret;

pub use account::{AccountResolutionError, resolve_or_create_otp_account};
pub use account_deletion::{
    AccountDeletionError, delete_direct_account_preserving_shared_workspaces,
};
pub use cookie::{
    CookiePolicyError, RUST_IDENTITY_COOKIE, RUST_IDENTITY_RESUME_COOKIE, SessionCookiePolicy,
    cleared_resume_session_cookie, cleared_session_cookie, require_trusted_origin,
    resume_session_set_cookie, session_set_cookie,
};
pub use encryption::{EncryptionError, EncryptionKey, decrypt_secret, encrypt_secret};
pub use http::{native_session_cookie, router};
pub use impersonation::{
    ImpersonationError, ImpersonationOutcome, MAX_EXPIRY_RECONCILIATION_BATCH_SIZE,
    StartedImpersonation, end_impersonation, end_impersonation_from_support_session,
    expire_due_impersonations, revoke_impersonation, start_impersonation,
};
pub use moderation::{
    ModerationError, ModerationOutcome, restore_project, suspend_user, takedown_project,
    unsuspend_user,
};
pub use otp::{IssuedOtpCode, OtpError, issue_otp_code, verify_otp_code};
pub use otp_challenge::{
    ConsumedOtpChallenge, EmailChangeChallenge, EmailChangeError, IssuedOtpChallenge,
    OtpChallengeError, OtpPurpose, QueuedOtpChallenge, confirm_email_change, consume_otp_challenge,
    issue_otp_challenge, queue_otp_challenge_delivery, request_email_change,
    verify_current_email_change,
};
pub use otp_delivery::deliver_otp_email_event;
pub use profile::{
    IdentityProfile, IdentityProfileError, current_profile, select_active_organization,
    update_profile_name,
};
pub use secret::{
    IdentitySecretError, IssuedIdentitySecret, hash_identity_secret, issue_identity_secret,
    verify_identity_secret,
};

mod session;

pub use session::{
    AuthenticatedSession, IdentitySessionError, IssuedSession,
    authenticate_read_only_actor_on_connection, authenticate_session, issue_session,
    revoke_session, rotate_csrf_secret, verify_csrf,
};
