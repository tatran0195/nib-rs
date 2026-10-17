//! App composition between identity and search; neither feature imports the other.
use axum::http::{HeaderMap, header::AUTHORIZATION};
use nibleaf_core::{AppError, authz::optional_transition_actor_on_connection};
use nibleaf_identity::{
    IdentitySessionError, authenticate_read_only_actor_on_connection, native_session_cookie,
};
use nibleaf_search::{ActorResolution, PublishedActorResolver};
use sqlx::PgConnection;

#[derive(Debug)]
pub(super) struct OptionalWorkspaceIdentity {
    pub native_enabled: bool,
    pub authority: nibleaf_core::AuthenticationAuthority,
}
impl PublishedActorResolver for OptionalWorkspaceIdentity {
    fn resolve<'a>(
        &'a self,
        connection: &'a mut PgConnection,
        headers: &'a HeaderMap,
    ) -> ActorResolution<'a> {
        Box::pin(async move {
            let native_present = super::cookie_contains_name(headers, "nibleaf_identity")?;
            let bridge_present = headers.contains_key(AUTHORIZATION)
                || super::cookie_contains_name(headers, "nibleaf_session")?;
            if native_present {
                if !self.native_enabled || bridge_present {
                    return Err(AppError::Unauthenticated);
                }
                let token = native_session_cookie(headers)?.ok_or(AppError::Unauthenticated)?;
                if token.len() > 512 {
                    return Err(AppError::Unauthenticated);
                }
                let actor = authenticate_read_only_actor_on_connection(connection, &token)
                    .await
                    .map_err(|error| match error {
                        IdentitySessionError::Database(error) => AppError::Database(error),
                        _ => AppError::Unauthenticated,
                    })?;
                return Ok(Some(actor));
            }
            if self.authority == nibleaf_core::AuthenticationAuthority::Native {
                return if bridge_present {
                    Err(AppError::Unauthenticated)
                } else {
                    Ok(None)
                };
            }
            optional_transition_actor_on_connection(connection, headers).await
        })
    }
}
