//! Web dashboard error types — RFC 7807 problem-detail responses.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[cfg(any(test, feature = "openapi", feature = "web"))]
use utoipa::ToSchema;

/// Errors returned by the Ledgerful web dashboard HTTP layer.
#[derive(Debug)]
pub enum WebError {
    NotFound,
    BadRequest(String),
    Internal(String),
    Forbidden,
    TooManyRequests,
    NotImplemented(String),
}

/// RFC 7807 problem-detail object. Additional members are allowed; this shape
/// supplies the core fields required by the track contract.
///
/// Track 0013: `ToSchema` + `pub(crate)` so the OpenAPI can document error
/// response bodies (e.g. the 501 from `/api/sync/status` when built without
/// the `sync` feature).
#[derive(Debug, Serialize)]
#[cfg_attr(any(test, feature = "openapi", feature = "web"), derive(ToSchema))]
pub(crate) struct ProblemDetail {
    #[serde(rename = "type")]
    type_uri: &'static str,
    title: &'static str,
    status: u16,
    detail: String,
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let (status, type_uri, title, detail) = match self {
            WebError::NotFound => (
                StatusCode::NOT_FOUND,
                "urn:ledgerful:problem:not-found",
                "Not Found",
                "The requested resource does not exist.".to_string(),
            ),
            WebError::BadRequest(msg) => (
                StatusCode::BAD_REQUEST,
                "urn:ledgerful:problem:bad-request",
                "Bad Request",
                msg,
            ),
            WebError::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "urn:ledgerful:problem:internal",
                "Internal Server Error",
                msg,
            ),
            WebError::Forbidden => (
                StatusCode::FORBIDDEN,
                "urn:ledgerful:problem:forbidden",
                "Forbidden",
                "A valid session token is required to access this resource.".to_string(),
            ),
            WebError::TooManyRequests => (
                StatusCode::TOO_MANY_REQUESTS,
                "urn:ledgerful:problem:too-many-requests",
                "Too Many Requests",
                "Rate limit exceeded; retry after a short cooldown.".to_string(),
            ),
            WebError::NotImplemented(msg) => (
                StatusCode::NOT_IMPLEMENTED,
                "urn:ledgerful:problem:not-implemented",
                "Not Implemented",
                msg,
            ),
        };

        let body = Json(ProblemDetail {
            type_uri,
            title,
            status: status.as_u16(),
            detail,
        });

        let mut response = (status, body).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        response
    }
}
