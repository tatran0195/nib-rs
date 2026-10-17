//! Native email-OTP HTTP routes. They remain additive until cutover gates pass.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::IntoResponse,
    routing::{get, patch, post, put},
};
use nibleaf_core::{
    Actor, AppError, AppState, Config, OrganizationId, ProjectId, StrictJson, UserId,
};
use serde::{Deserialize, Serialize};
use time::Duration;
use uuid::Uuid;

use crate::{
    AccountDeletionError, AccountResolutionError, EmailChangeError, EncryptionKey, IdentityProfile,
    IdentityProfileError, IdentitySessionError, ImpersonationError, ModerationError,
    OtpChallengeError, OtpPurpose, RUST_IDENTITY_COOKIE, RUST_IDENTITY_RESUME_COOKIE,
    SessionCookiePolicy, authenticate_session, cleared_resume_session_cookie,
    cleared_session_cookie, confirm_email_change, consume_otp_challenge, current_profile,
    delete_direct_account_preserving_shared_workspaces, end_impersonation,
    end_impersonation_from_support_session, issue_session, queue_otp_challenge_delivery,
    request_email_change, require_trusted_origin, resolve_or_create_otp_account, restore_project,
    resume_session_set_cookie, revoke_impersonation, revoke_session, rotate_csrf_secret,
    select_active_organization, session_set_cookie, start_impersonation, suspend_user,
    takedown_project, unsuspend_user, update_profile_name, verify_csrf,
    verify_current_email_change,
};

const SESSION_LIFETIME: Duration = Duration::days(7);
const CSRF_HEADER: HeaderName = HeaderName::from_static("x-csrf-token");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestOtpBody {
    email: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyOtpBody {
    challenge_id: String,
    code: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmailChangeRequestBody {
    new_email: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmailChangeVerifyBody {
    challenge_id: String,
    code: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateProfileBody {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectActiveOrganizationBody {
    organization_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TakedownProjectBody {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartImpersonationBody {
    target_user_id: String,
    #[serde(default)]
    active_organization_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct StartImpersonationResponse {
    impersonation_id: Uuid,
    user_id: String,
    csrf_token: String,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: time::OffsetDateTime,
}

#[derive(Debug, Serialize)]
struct ImpersonationOutcomeResponse {
    changed: bool,
}

#[derive(Debug, Serialize)]
struct ModerationResponse {
    changed: bool,
}

#[derive(Debug, Serialize)]
struct Data<T> {
    data: T,
}

#[derive(Debug, Serialize)]
struct VerifyOtpResponse {
    csrf_token: String,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: time::OffsetDateTime,
}

/// Opaque non-secret continuation handle for a native sign-in OTP. Invalid or
/// rate-limited requests receive the same UUID-shaped response as a queued
/// challenge, so the pre-authentication request path remains non-enumerating.
#[derive(Debug, Serialize)]
struct OtpChallengeResponse {
    challenge_id: Uuid,
}

/// A newly rotated CSRF secret returned only to a same-origin browser with a
/// valid native identity session. It is never stored in a readable cookie.
#[derive(Debug, Serialize)]
struct CsrfRecoveryResponse {
    csrf_token: String,
}

/// Opaque continuation handle for a two-stage email-change request. It is not
/// a secret: the server still requires the native session user and a one-time
/// mailbox code. Ordinary request/proof paths return an indistinguishable fresh
/// handle so response shape cannot enumerate another account's email.
#[derive(Debug, Serialize)]
struct EmailChangeChallengeResponse {
    challenge_id: Uuid,
}

/// Additive native identity routes. No legacy endpoint or cookie is replaced by
/// these paths until the documented cutover is enabled.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/identity/otp/request", post(request_otp))
        .route("/identity/otp/verify", post(verify_otp))
        .route(
            "/identity/email-change/request",
            post(request_email_change_route),
        )
        .route(
            "/identity/email-change/verify-current",
            post(verify_current_email_change_route),
        )
        .route(
            "/identity/email-change/confirm",
            post(confirm_email_change_route),
        )
        .route("/identity/session", get(session))
        .route("/identity/csrf", post(rotate_csrf_route))
        .route("/identity/account-deletion", post(delete_account_route))
        .route("/identity/profile", patch(update_profile_route))
        .route(
            "/identity/session/active-organization",
            put(select_active_organization_route),
        )
        .route("/identity/sign-out", post(sign_out))
        .route(
            "/identity/admin/users/{user_id}/suspend",
            post(suspend_user_route),
        )
        .route(
            "/identity/admin/users/{user_id}/unsuspend",
            post(unsuspend_user_route),
        )
        .route(
            "/identity/admin/projects/{project_id}/takedown",
            post(takedown_project_route),
        )
        .route(
            "/identity/admin/projects/{project_id}/restore",
            post(restore_project_route),
        )
        .route(
            "/identity/admin/impersonations",
            post(start_impersonation_route),
        )
        .route(
            "/identity/admin/impersonations/stop",
            post(stop_impersonation_route),
        )
        .route(
            "/identity/admin/impersonations/{impersonation_id}/revoke",
            post(revoke_impersonation_route),
        )
}

async fn request_otp(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictJson(request): StrictJson<RequestOtpBody>,
) -> Result<impl IntoResponse, AppError> {
    require_origin(&headers, &state)?;
    // Required-auth delivery must not acknowledge a code request when no
    // provider can ever consume the durable intent.
    nibleaf_email::configured_provider(&state.config.invitation_email)
        .map_err(|_| AppError::Unavailable)?;
    let key = current_delivery_key(&state.config)?;
    let challenge_id =
        match queue_otp_challenge_delivery(&state.pool, &request.email, OtpPurpose::SignIn, &key)
            .await
        {
            Ok(challenge) => challenge.challenge_id,
            // The fake UUID is intentionally indistinguishable from a real
            // opaque handle. Verification still requires a stored one-time
            // code, so it cannot authenticate a caller.
            Err(OtpChallengeError::InvalidEmail | OtpChallengeError::RateLimited) => Uuid::now_v7(),
            Err(OtpChallengeError::Database(_) | OtpChallengeError::Outbox(_)) => {
                return Err(AppError::Unavailable);
            }
            Err(_) => return Err(AppError::Internal),
        };
    Ok(otp_challenge_response(challenge_id))
}

async fn verify_otp(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictJson(request): StrictJson<VerifyOtpBody>,
) -> Result<impl IntoResponse, AppError> {
    require_origin(&headers, &state)?;
    let challenge_id =
        Uuid::parse_str(&request.challenge_id).map_err(|_| AppError::Unauthenticated)?;
    let facts = consume_otp_challenge(&state.pool, challenge_id, &request.code)
        .await
        .map_err(|error| otp_verify_error(&error))?;
    if facts.purpose != OtpPurpose::SignIn {
        return Err(AppError::Unauthenticated);
    }
    let user_id = resolve_or_create_otp_account(
        &state.pool,
        &facts.normalized_email,
        request.name.as_deref(),
        state.config.disable_signup,
    )
    .await
    .map_err(|error| account_error(&error))?;
    let issued = issue_session(&state.pool, &user_id, None, None, SESSION_LIFETIME)
        .await
        .map_err(|_| AppError::Unavailable)?;
    let secure = state
        .config
        .invitation_email
        .application_url
        .starts_with("https://");
    let cookie = session_set_cookie(
        &issued.session_token,
        SessionCookiePolicy {
            secure,
            max_age: SESSION_LIFETIME,
        },
    )
    .map_err(|_| AppError::Internal)?;
    let mut response = (
        StatusCode::OK,
        Json(Data {
            data: VerifyOtpResponse {
                csrf_token: issued.csrf_secret,
                expires_at: issued.expires_at,
            },
        }),
    )
        .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| AppError::Internal)?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn request_email_change_route(
    State(state): State<AppState>,
    actor: Actor,
    headers: HeaderMap,
    StrictJson(request): StrictJson<EmailChangeRequestBody>,
) -> Result<impl IntoResponse, AppError> {
    require_native_identity_cookie(&headers)?;
    nibleaf_email::configured_provider(&state.config.invitation_email)
        .map_err(|_| AppError::Unavailable)?;
    let key = current_delivery_key(&state.config)?;
    let challenge_id =
        match request_email_change(&state.pool, &actor.user_id, &request.new_email, &key).await {
            Ok(challenge) => challenge.challenge_id,
            // Return an opaque random continuation on every ordinary rejection. A
            // caller with a real current-mailbox code receives the same 202 shape
            // whether the requested target is valid, taken, unchanged, malformed,
            // or currently rate-limited.
            Err(
                EmailChangeError::Rejected
                | EmailChangeError::Otp(
                    OtpChallengeError::InvalidEmail | OtpChallengeError::RateLimited,
                ),
            ) => Uuid::now_v7(),
            Err(
                EmailChangeError::Database(_)
                | EmailChangeError::Otp(
                    OtpChallengeError::Database(_) | OtpChallengeError::Outbox(_),
                ),
            ) => return Err(AppError::Unavailable),
            Err(_) => return Err(AppError::Internal),
        };
    Ok(email_change_challenge_response(challenge_id))
}

async fn verify_current_email_change_route(
    State(state): State<AppState>,
    actor: Actor,
    headers: HeaderMap,
    StrictJson(request): StrictJson<EmailChangeVerifyBody>,
) -> Result<impl IntoResponse, AppError> {
    require_native_identity_cookie(&headers)?;
    nibleaf_email::configured_provider(&state.config.invitation_email)
        .map_err(|_| AppError::Unavailable)?;
    let key = current_delivery_key(&state.config)?;
    let challenge_id = match Uuid::parse_str(&request.challenge_id) {
        Ok(current_challenge_id) => match verify_current_email_change(
            &state.pool,
            &actor.user_id,
            current_challenge_id,
            &request.code,
            &key,
        )
        .await
        {
            Ok(challenge) => challenge.challenge_id,
            // A fake request handle, expired/foreign flow, target race, and a
            // wrong current-mailbox code all produce a fresh indistinguishable
            // continuation. The next-email code is sent only for a real flow.
            Err(
                EmailChangeError::Rejected | EmailChangeError::Otp(OtpChallengeError::Rejected),
            ) => Uuid::now_v7(),
            Err(
                EmailChangeError::Database(_)
                | EmailChangeError::Otp(
                    OtpChallengeError::Database(_) | OtpChallengeError::Outbox(_),
                ),
            ) => return Err(AppError::Unavailable),
            Err(_) => return Err(AppError::Internal),
        },
        // Preserve the same generic continuation response even when a caller
        // supplies a syntactically invalid opaque handle.
        Err(_) => Uuid::now_v7(),
    };
    Ok(email_change_challenge_response(challenge_id))
}

async fn confirm_email_change_route(
    State(state): State<AppState>,
    actor: Actor,
    headers: HeaderMap,
    StrictJson(request): StrictJson<EmailChangeVerifyBody>,
) -> Result<Json<Data<serde_json::Value>>, AppError> {
    require_native_identity_cookie(&headers)?;
    let challenge_id =
        Uuid::parse_str(&request.challenge_id).map_err(|_| AppError::Unauthenticated)?;
    confirm_email_change(&state.pool, &actor.user_id, challenge_id, &request.code)
        .await
        .map_err(|error| email_change_error(&error))?;
    Ok(Json(Data {
        data: serde_json::json!({}),
    }))
}

async fn suspend_user_route(
    State(state): State<AppState>,
    actor: Actor,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<Json<Data<ModerationResponse>>, AppError> {
    require_native_identity_cookie(&headers)?;
    let user_id = UserId::try_from(user_id)?;
    let outcome = suspend_user(&state.pool, &actor, &user_id)
        .await
        .map_err(moderation_error)?;
    Ok(Json(Data {
        data: ModerationResponse {
            changed: outcome.changed,
        },
    }))
}

async fn unsuspend_user_route(
    State(state): State<AppState>,
    actor: Actor,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<Json<Data<ModerationResponse>>, AppError> {
    require_native_identity_cookie(&headers)?;
    let user_id = UserId::try_from(user_id)?;
    let outcome = unsuspend_user(&state.pool, &actor, &user_id)
        .await
        .map_err(moderation_error)?;
    Ok(Json(Data {
        data: ModerationResponse {
            changed: outcome.changed,
        },
    }))
}

async fn takedown_project_route(
    State(state): State<AppState>,
    actor: Actor,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    StrictJson(request): StrictJson<TakedownProjectBody>,
) -> Result<Json<Data<ModerationResponse>>, AppError> {
    require_native_identity_cookie(&headers)?;
    let project_id = ProjectId::try_from(project_id)?;
    let outcome = takedown_project(&state.pool, &actor, &project_id, request.reason.as_deref())
        .await
        .map_err(moderation_error)?;
    Ok(Json(Data {
        data: ModerationResponse {
            changed: outcome.changed,
        },
    }))
}

async fn restore_project_route(
    State(state): State<AppState>,
    actor: Actor,
    headers: HeaderMap,
    Path(project_id): Path<String>,
) -> Result<Json<Data<ModerationResponse>>, AppError> {
    require_native_identity_cookie(&headers)?;
    let project_id = ProjectId::try_from(project_id)?;
    let outcome = restore_project(&state.pool, &actor, &project_id)
        .await
        .map_err(moderation_error)?;
    Ok(Json(Data {
        data: ModerationResponse {
            changed: outcome.changed,
        },
    }))
}

async fn start_impersonation_route(
    State(state): State<AppState>,
    actor: Actor,
    headers: HeaderMap,
    StrictJson(request): StrictJson<StartImpersonationBody>,
) -> Result<impl IntoResponse, AppError> {
    require_native_identity_cookie(&headers)?;
    let target_user_id = UserId::try_from(request.target_user_id)?;
    let active_organization_id = request
        .active_organization_id
        .map(OrganizationId::try_from)
        .transpose()?;
    let operator_token = native_cookie(&headers)?;
    let started = start_impersonation(
        &state.pool,
        &actor,
        &target_user_id,
        active_organization_id.as_ref(),
    )
    .await
    .map_err(impersonation_error)?;
    let secure = native_cookie_is_secure(&state.config);
    let policy = SessionCookiePolicy {
        secure,
        max_age: Duration::hours(1),
    };
    let impersonation_cookie = session_set_cookie(&started.issued_session.session_token, policy)
        .map_err(|_| AppError::Internal)?;
    let resume_cookie =
        resume_session_set_cookie(&operator_token, policy).map_err(|_| AppError::Internal)?;
    let mut response = (
        StatusCode::OK,
        Json(Data {
            data: StartImpersonationResponse {
                impersonation_id: started.impersonation_id,
                user_id: target_user_id.to_string(),
                csrf_token: started.issued_session.csrf_secret,
                expires_at: started.issued_session.expires_at,
            },
        }),
    )
        .into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&impersonation_cookie).map_err(|_| AppError::Internal)?,
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&resume_cookie).map_err(|_| AppError::Internal)?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// This route is deliberately admitted with the path-scoped resume cookie, not
/// the potentially expired/revoked customer support cookie. See API middleware:
/// an emergency revocation must not strand the operator in an unusable browser
/// session. It still requires an explicit trusted browser Origin.
async fn stop_impersonation_route(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    require_explicit_origin(&headers, &state)?;
    let resume_token = resume_cookie(&headers)?;
    let operator_session = authenticate_session(&state.pool, &resume_token)
        .await
        .map_err(|_| AppError::Unauthenticated)?;
    let outcome = end_impersonation(&state.pool, &operator_session)
        .await
        .map_err(impersonation_error)?;
    let max_age = (operator_session.expires_at - time::OffsetDateTime::now_utc()).whole_seconds();
    if max_age <= 0 {
        return Err(AppError::Unauthenticated);
    }
    let secure = native_cookie_is_secure(&state.config);
    let operator_cookie = session_set_cookie(
        &resume_token,
        SessionCookiePolicy {
            secure,
            max_age: Duration::seconds(max_age),
        },
    )
    .map_err(|_| AppError::Unauthenticated)?;
    let mut response = Json(Data {
        data: ImpersonationOutcomeResponse {
            changed: outcome.changed,
        },
    })
    .into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&operator_cookie).map_err(|_| AppError::Internal)?,
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&cleared_resume_session_cookie(secure))
            .map_err(|_| AppError::Internal)?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn revoke_impersonation_route(
    State(state): State<AppState>,
    actor: Actor,
    headers: HeaderMap,
    Path(impersonation_id): Path<String>,
) -> Result<Json<Data<ImpersonationOutcomeResponse>>, AppError> {
    require_native_identity_cookie(&headers)?;
    let impersonation_id = Uuid::parse_str(&impersonation_id)
        .map_err(|_| AppError::validation("invalid impersonation id"))?;
    let outcome = revoke_impersonation(&state.pool, &actor, impersonation_id)
        .await
        .map_err(impersonation_error)?;
    Ok(Json(Data {
        data: ImpersonationOutcomeResponse {
            changed: outcome.changed,
        },
    }))
}

/// Recovers a CSRF secret for a still-live native session. The route itself is
/// intentionally exempt from the API-wide CSRF check because it is how a new
/// browser tab recovers that proof; it replaces that requirement with an
/// explicit exact trusted Origin before rotating the server-side secret.
async fn rotate_csrf_route(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    require_native_identity_cookie(&headers)?;
    require_explicit_origin(&headers, &state)?;
    let token = native_cookie(&headers)?;
    let authenticated = authenticate_session(&state.pool, &token)
        .await
        .map_err(|_| AppError::Unauthenticated)?;
    let csrf_token = rotate_csrf_secret(&state.pool, &authenticated)
        .await
        .map_err(identity_session_error)?;
    let mut response = Json(Data {
        data: CsrfRecoveryResponse { csrf_token },
    })
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// Updates only the authenticated user's display name. API-wide native-cookie
/// admission has already checked exact Origin and CSRF before this handler; the
/// explicit cookie/session re-read keeps the feature safe when called without
/// that middleware in a future composition.
async fn update_profile_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictJson(request): StrictJson<UpdateProfileBody>,
) -> Result<impl IntoResponse, AppError> {
    require_native_identity_cookie(&headers)?;
    let token = native_cookie(&headers)?;
    let authenticated = authenticate_session(&state.pool, &token)
        .await
        .map_err(|_| AppError::Unauthenticated)?;
    let profile = update_profile_name(&state.pool, &authenticated, &request.name)
        .await
        .map_err(profile_error)?;
    Ok(profile_response(profile))
}

/// Selects a presentation-only active workspace after proving that it belongs
/// to the current session user. Feature authorization never derives permission
/// from this preference.
async fn select_active_organization_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    StrictJson(request): StrictJson<SelectActiveOrganizationBody>,
) -> Result<impl IntoResponse, AppError> {
    require_native_identity_cookie(&headers)?;
    let organization_id = OrganizationId::try_from(request.organization_id)?;
    let token = native_cookie(&headers)?;
    let authenticated = authenticate_session(&state.pool, &token)
        .await
        .map_err(|_| AppError::Unauthenticated)?;
    let profile = select_active_organization(&state.pool, &authenticated, &organization_id)
        .await
        .map_err(identity_session_error)?;
    Ok(profile_response(profile))
}

async fn session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let token = native_cookie(&headers)?;
    let authenticated = authenticate_session(&state.pool, &token)
        .await
        .map_err(|_| AppError::Unauthenticated)?;
    let profile = current_profile(&state.pool, &authenticated)
        .await
        .map_err(profile_error)?;
    let mut response = Json(Data {
        data: SessionResponse {
            user_id: authenticated.user_id.to_string(),
            user: ProfileUserResponse::from(&profile),
            active_organization_id: profile.active_organization_id,
            impersonated_by: authenticated.impersonated_by,
            expires_at: authenticated.expires_at,
        },
    })
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[derive(Debug, Serialize)]
struct ProfileUserResponse {
    id: String,
    name: String,
    email: String,
    email_verified: bool,
}

impl From<&IdentityProfile> for ProfileUserResponse {
    fn from(profile: &IdentityProfile) -> Self {
        Self {
            id: profile.user_id.clone(),
            name: profile.name.clone(),
            email: profile.email.clone(),
            email_verified: profile.email_verified,
        }
    }
}

#[derive(Debug, Serialize)]
struct ProfileResponse {
    user: ProfileUserResponse,
    active_organization_id: Option<String>,
}

impl From<IdentityProfile> for ProfileResponse {
    fn from(profile: IdentityProfile) -> Self {
        Self {
            user: ProfileUserResponse::from(&profile),
            active_organization_id: profile.active_organization_id,
        }
    }
}

fn profile_response(profile: IdentityProfile) -> axum::response::Response {
    let mut response = Json(Data {
        data: ProfileResponse::from(profile),
    })
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[derive(Debug, Serialize)]
struct SessionResponse {
    user_id: String,
    user: ProfileUserResponse,
    active_organization_id: Option<String>,
    impersonated_by: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: time::OffsetDateTime,
}

/// Permanently removes the current direct native account while preserving every
/// shared workspace. When it is the sole owner, ownership is transferred to the
/// earliest other member atomically before departure. A sole-member workspace
/// still receives a conflict rather than a partial relational deletion because
/// retained-store erasure is not implemented on this route.
///
/// API-wide native admission has already required an exact trusted Origin and
/// the live session's CSRF proof. The feature repeats native-cookie admission
/// defensively, and its transaction re-locks/revalidates the exact session and
/// user before it writes audit evidence or deletes anything.
async fn delete_account_route(
    State(state): State<AppState>,
    actor: Actor,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    require_native_identity_cookie(&headers)?;
    // Unlike generic unsafe feature routes, deletion refuses an originless
    // request too: this irreversible browser operation has an exact-Origin
    // contract in addition to API-wide CSRF verification.
    require_explicit_origin(&headers, &state)?;
    delete_direct_account_preserving_shared_workspaces(&state.pool, &actor)
        .await
        .map_err(|error| account_deletion_error(&error))?;
    let secure = native_cookie_is_secure(&state.config);
    let mut response = StatusCode::NO_CONTENT.into_response();
    // Set this only after the transaction commits; an account that could not
    // be erased must retain its browser session so it can complete the future
    // sole-tenant retained-store erasure workflow.
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cleared_session_cookie(secure)).map_err(|_| AppError::Internal)?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn sign_out(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    require_origin(&headers, &state)?;
    let token = native_cookie(&headers)?;
    let authenticated = authenticate_session(&state.pool, &token)
        .await
        .map_err(|_| AppError::Unauthenticated)?;
    let csrf = headers
        .get(CSRF_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(AppError::Unauthenticated)?;
    verify_csrf(&authenticated, csrf).map_err(|_| AppError::Unauthenticated)?;
    if authenticated.impersonated_by.is_some() {
        end_impersonation_from_support_session(&state.pool, &authenticated)
            .await
            .map_err(impersonation_error)?;
    } else {
        revoke_session(
            &state.pool,
            authenticated.session_id,
            &authenticated.user_id,
            "sign_out",
        )
        .await
        .map_err(|_| AppError::Unavailable)?;
    }
    let secure = native_cookie_is_secure(&state.config);
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cleared_session_cookie(secure)).map_err(|_| AppError::Internal)?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

fn otp_challenge_response(challenge_id: Uuid) -> axum::response::Response {
    let mut response = (
        StatusCode::ACCEPTED,
        Json(Data {
            data: OtpChallengeResponse { challenge_id },
        }),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn email_change_challenge_response(challenge_id: Uuid) -> axum::response::Response {
    let mut response = (
        StatusCode::ACCEPTED,
        Json(Data {
            data: EmailChangeChallengeResponse { challenge_id },
        }),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn current_delivery_key(config: &Config) -> Result<EncryptionKey, AppError> {
    EncryptionKey::new(
        config.identity_delivery_encryption_key_version,
        &config.identity_delivery_encryption_key,
    )
    .map_err(|_| AppError::Internal)
}

fn require_native_identity_cookie(headers: &HeaderMap) -> Result<(), AppError> {
    if native_session_cookie(headers)?.is_some() {
        Ok(())
    } else {
        Err(AppError::Unauthenticated)
    }
}

fn require_explicit_origin(headers: &HeaderMap, state: &AppState) -> Result<(), AppError> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or(AppError::Forbidden)?;
    require_trusted_origin(Some(origin), &state.config.trusted_origins)
        .map_err(|_| AppError::Forbidden)
}

fn native_cookie_is_secure(config: &Config) -> bool {
    config
        .invitation_email
        .application_url
        .starts_with("https://")
}

fn require_origin(headers: &HeaderMap, state: &AppState) -> Result<(), AppError> {
    require_trusted_origin(
        headers
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok()),
        &state.config.trusted_origins,
    )
    .map_err(|_| AppError::Forbidden)
}

/// Extracts at most one native session cookie without choosing between
/// ambiguous values. API-wide identity admission uses this before feature
/// extractors so native sessions can authorize the existing Rust slices.
///
/// # Errors
/// Returns `Unauthenticated` for malformed cookie headers or duplicate native
/// cookie values.
pub fn native_session_cookie(headers: &HeaderMap) -> Result<Option<String>, AppError> {
    cookie_value(headers, RUST_IDENTITY_COOKIE)
}

fn resume_cookie(headers: &HeaderMap) -> Result<String, AppError> {
    cookie_value(headers, RUST_IDENTITY_RESUME_COOKIE)?.ok_or(AppError::Unauthenticated)
}

fn cookie_value(headers: &HeaderMap, expected_name: &str) -> Result<Option<String>, AppError> {
    let mut token = None;
    for header_value in headers.get_all(header::COOKIE) {
        let cookie = header_value
            .to_str()
            .map_err(|_| AppError::Unauthenticated)?;
        for pair in cookie.split(';') {
            let (name, value) = pair.trim().split_once('=').unwrap_or_default();
            if name == expected_name
                && !value.is_empty()
                && token.replace(value.to_owned()).is_some()
            {
                return Err(AppError::Unauthenticated);
            }
        }
    }
    Ok(token)
}

fn native_cookie(headers: &HeaderMap) -> Result<String, AppError> {
    native_session_cookie(headers)?.ok_or(AppError::Unauthenticated)
}

fn otp_verify_error(error: &OtpChallengeError) -> AppError {
    match error {
        OtpChallengeError::Rejected
        | OtpChallengeError::InvalidEmail
        | OtpChallengeError::RateLimited => AppError::Unauthenticated,
        OtpChallengeError::Database(_) => AppError::Unavailable,
        OtpChallengeError::Otp(_)
        | OtpChallengeError::Encryption(_)
        | OtpChallengeError::Outbox(_) => AppError::Internal,
    }
}

fn email_change_error(error: &EmailChangeError) -> AppError {
    match error {
        EmailChangeError::Rejected | EmailChangeError::Otp(OtpChallengeError::Rejected) => {
            AppError::Unauthenticated
        }
        EmailChangeError::Database(_)
        | EmailChangeError::Otp(OtpChallengeError::Database(_) | OtpChallengeError::Outbox(_)) => {
            AppError::Unavailable
        }
        EmailChangeError::Otp(_) => AppError::Internal,
    }
}

fn profile_error(error: IdentityProfileError) -> AppError {
    match error {
        IdentityProfileError::InvalidName => AppError::validation("name is invalid"),
        IdentityProfileError::Unavailable => AppError::Unauthenticated,
        IdentityProfileError::Database(error) => AppError::Database(error),
    }
}

fn identity_session_error(error: IdentitySessionError) -> AppError {
    match error {
        IdentitySessionError::InvalidActiveOrganization => {
            AppError::validation("active organization is not a membership of this user")
        }
        IdentitySessionError::AccountUnavailable | IdentitySessionError::Unavailable => {
            AppError::Unauthenticated
        }
        IdentitySessionError::Database(error) => AppError::Database(error),
        IdentitySessionError::Secret(_) | IdentitySessionError::InvalidLifetime => {
            AppError::Internal
        }
    }
}

fn impersonation_error(error: ImpersonationError) -> AppError {
    match error {
        ImpersonationError::Forbidden => AppError::Forbidden,
        ImpersonationError::NotFound => AppError::not_found("support impersonation target"),
        ImpersonationError::SelfImpersonation => {
            AppError::validation("a user cannot impersonate themself")
        }
        ImpersonationError::InvalidActiveOrganization => {
            AppError::validation("active organization is not a target membership")
        }
        ImpersonationError::Database(error) => AppError::Database(error),
        ImpersonationError::Session(_) => AppError::Unavailable,
        ImpersonationError::InvalidExpiryReconciliationBatchSize => AppError::Internal,
    }
}

fn moderation_error(error: ModerationError) -> AppError {
    match error {
        ModerationError::Forbidden => AppError::Forbidden,
        ModerationError::NotFound => AppError::not_found("moderation target"),
        ModerationError::AdministratorProtected => {
            AppError::conflict("Platform administrators cannot be suspended.")
        }
        ModerationError::InvalidReason => AppError::validation("moderation reason is invalid"),
        ModerationError::Database(error) => AppError::Database(error),
    }
}

fn account_error(error: &AccountResolutionError) -> AppError {
    match error {
        AccountResolutionError::SignupDisabled => AppError::Unauthenticated,
        AccountResolutionError::Database(_) | AccountResolutionError::Outbox(_) => {
            AppError::Unavailable
        }
        AccountResolutionError::InvalidName => AppError::Validation {
            detail: "name is invalid".to_owned(),
        },
    }
}

fn account_deletion_error(error: &AccountDeletionError) -> AppError {
    match error {
        // Do not distinguish revoked/expired/missing session state on a
        // destructive endpoint. The caller must authenticate again.
        AccountDeletionError::Unavailable => AppError::Unauthenticated,
        AccountDeletionError::Forbidden => AppError::Forbidden,
        AccountDeletionError::RetainedScopeRequired => AppError::conflict(
            "account deletion requires retained-store erasure for a sole-member workspace",
        ),
        AccountDeletionError::TenantInvariant => AppError::conflict(
            "workspace ownership is inconsistent; account deletion is unavailable",
        ),
        AccountDeletionError::ProvisioningInProgress => AppError::conflict(
            "account workspace provisioning is in progress; retry after it completes",
        ),
        AccountDeletionError::Database(_) => AppError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::native_cookie;
    use axum::http::{HeaderMap, HeaderValue, header};
    use nibleaf_core::AppError;

    #[test]
    fn identity_wire_dates_are_rfc3339_not_time_tuples() -> Result<(), serde_json::Error> {
        let issued = super::VerifyOtpResponse {
            csrf_token: "test-only".to_owned(),
            expires_at: time::OffsetDateTime::UNIX_EPOCH,
        };
        let value = serde_json::to_value(issued)?;
        assert_eq!(value["expires_at"].as_str(), Some("1970-01-01T00:00:00Z"));
        let session = super::SessionResponse {
            user_id: "user".to_owned(),
            user: super::ProfileUserResponse {
                id: "user".to_owned(),
                name: "name".to_owned(),
                email: "test@example.test".to_owned(),
                email_verified: true,
            },
            active_organization_id: None,
            impersonated_by: None,
            expires_at: time::OffsetDateTime::UNIX_EPOCH,
        };
        assert_eq!(
            serde_json::to_value(session)?["expires_at"].as_str(),
            Some("1970-01-01T00:00:00Z")
        );
        Ok(())
    }

    #[test]
    fn native_cookie_parser_rejects_duplicate_values() {
        let mut headers = HeaderMap::new();
        headers.append(
            header::COOKIE,
            HeaderValue::from_static("nibleaf_identity=one"),
        );
        headers.append(
            header::COOKIE,
            HeaderValue::from_static("nibleaf_identity=two"),
        );
        assert!(matches!(
            native_cookie(&headers),
            Err(AppError::Unauthenticated)
        ));
    }
}
