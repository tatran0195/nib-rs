use axum::{
    Json,
    extract::{FromRequest, Request},
};
use serde::de::DeserializeOwned;

use crate::AppError;

/// JSON extractor that maps malformed bodies, wrong content types, and Serde
/// schema rejections into the platform's RFC 9457 error contract.
///
/// Feature request types still use `#[serde(deny_unknown_fields)]`; this wrapper
/// centralizes the transport-level rejection instead of letting Axum return a
/// second, framework-specific error shape.
#[derive(Debug)]
pub struct StrictJson<T>(pub T);

impl<S, T> FromRequest<S> for StrictJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|_| {
                AppError::validation(
                    "request body must be valid JSON matching this endpoint's schema",
                )
            })
    }
}
