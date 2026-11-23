use axum::{Json, http::StatusCode, response::{IntoResponse, Response}};
use serde_json::json;

/// Returned (inside an anyhow::Error) when a user has no tokens left.
#[derive(Debug, thiserror::Error)]
#[error("token quota exceeded")]
pub struct QuotaExceeded;

pub struct AppError(pub StatusCode, pub String);

impl AppError {
    pub fn bad_request(msg: impl Into<String>) -> Self { Self(StatusCode::BAD_REQUEST, msg.into()) }
    pub fn unauthorized(msg: impl Into<String>) -> Self { Self(StatusCode::UNAUTHORIZED, msg.into()) }
    pub fn too_many(msg: impl Into<String>) -> Self { Self(StatusCode::TOO_MANY_REQUESTS, msg.into()) }
    pub fn not_found(msg: impl Into<String>) -> Self { Self(StatusCode::NOT_FOUND, msg.into()) }
}

// Any error type that converts into anyhow::Error works with `?` in handlers.
impl<E> From<E> for AppError where E: Into<anyhow::Error> {
    fn from(e: E) -> Self {
        let e: anyhow::Error = e.into();
        if e.downcast_ref::<QuotaExceeded>().is_some() {
            return Self(StatusCode::PAYMENT_REQUIRED, "You have used all your tokens for this period. Upgrade your plan to continue.".into());
        }
        tracing::error!("request failed: {e:#}");
        Self(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong. Please try again.".into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let code = match self.0 {
            StatusCode::PAYMENT_REQUIRED => "quota_exceeded",
            StatusCode::UNAUTHORIZED => "unauthorized",
            StatusCode::TOO_MANY_REQUESTS => "rate_limited",
            _ => "error",
        };
        (self.0, Json(json!({ "error": self.1, "code": code }))).into_response()
    }
}
