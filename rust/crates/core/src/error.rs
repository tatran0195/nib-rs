use axum::{
    Json,
    response::{IntoResponse, Response},
};
use http::{
    HeaderValue, StatusCode,
    header::{CONTENT_TYPE, WWW_AUTHENTICATE},
};
use serde::Serialize;
use tracing::error;

/// RFC 9457-compatible error document. `detail` deliberately contains only
/// client-safe text; database messages and other implementation details are
/// logged, never returned.
#[derive(Debug, Serialize)]
pub struct Problem {
    #[serde(rename = "type")]
    pub type_url: String,
    pub title: &'static str,
    pub status: u16,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("authentication required")]
    Unauthenticated,
    #[error("permission denied")]
    Forbidden,
    #[error("method not allowed")]
    MethodNotAllowed,
    #[error("{resource} not found")]
    NotFound { resource: &'static str },
    #[error("request body is too large")]
    PayloadTooLarge,
    #[error("request timed out")]
    RequestTimeout,
    #[error("conflict: {detail}")]
    Conflict { detail: String },
    #[error("validation failed: {detail}")]
    Validation { detail: String },
    #[error("dependency unavailable")]
    Unavailable,
    #[error("database operation failed")]
    Database(#[source] sqlx::Error),
    #[error("internal error")]
    Internal,
}

impl AppError {
    #[must_use]
    pub fn validation(detail: impl Into<String>) -> Self {
        Self::Validation {
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::Conflict {
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn not_found(resource: &'static str) -> Self {
        Self::NotFound { resource }
    }

    #[must_use]
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::NotFound { .. } => StatusCode::NOT_FOUND,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::RequestTimeout => StatusCode::REQUEST_TIMEOUT,
            Self::Conflict { .. } => StatusCode::CONFLICT,
            Self::Validation { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::Database(_) | Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn problem(&self) -> Problem {
        let status = self.status();
        let (kind, title, detail) = match self {
            Self::Unauthenticated => (
                "unauthenticated",
                "Authentication required",
                "A valid session is required.".to_owned(),
            ),
            Self::Forbidden => (
                "forbidden",
                "Permission denied",
                "You do not have permission to perform this action.".to_owned(),
            ),
            Self::MethodNotAllowed => (
                "method-not-allowed",
                "Method not allowed",
                "This HTTP method is not allowed for the requested resource.".to_owned(),
            ),
            // A project that an actor cannot access intentionally maps to NotFound
            // at the data access boundary; callers must not infer tenant existence.
            Self::NotFound { resource } => (
                "not-found",
                "Resource not found",
                format!("The requested {resource} was not found."),
            ),
            Self::PayloadTooLarge => (
                "payload-too-large",
                "Request body too large",
                "The request body exceeds this endpoint's size limit.".to_owned(),
            ),
            Self::RequestTimeout => (
                "request-timeout",
                "Request timed out",
                "The request did not complete before the server timeout.".to_owned(),
            ),
            Self::Conflict { detail } => ("conflict", "Conflict", detail.clone()),
            Self::Validation { detail } => ("validation", "Validation failed", detail.clone()),
            Self::Unavailable => (
                "dependency-unavailable",
                "Service temporarily unavailable",
                "Please retry later.".to_owned(),
            ),
            Self::Database(_) | Self::Internal => (
                "internal",
                "Internal server error",
                "An unexpected error occurred.".to_owned(),
            ),
        };

        Problem {
            type_url: format!("https://nibleaf.dev/problems/{kind}"),
            title,
            status: status.as_u16(),
            detail,
            instance: None,
        }
    }
}

impl From<sqlx::Error> for AppError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if matches!(self, Self::Database(_) | Self::Internal) {
            error!(error = ?self, "request failed");
        }
        let status = self.status();
        let mut response = (status, Json(self.problem())).into_response();
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        if matches!(self, Self::Unauthenticated) {
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}
