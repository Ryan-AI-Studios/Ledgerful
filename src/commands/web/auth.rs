//! Web dashboard authentication.

use crate::commands::web::error::WebError;
use axum::http::header;
use axum::http::request::Parts;
use rand::RngCore;
use serde::Deserialize;
use subtle::ConstantTimeEq;

/// Query parameter used to pass the session token.
#[derive(Debug, Deserialize)]
pub struct TokenQuery {
    token: String,
}

/// Validate a provided token string against the expected session token using
/// constant-time comparison.
pub fn validate_token(provided: Option<String>, expected: &str) -> Result<(), WebError> {
    let provided = provided.unwrap_or_default();
    let provided_bytes = provided.as_bytes();
    let expected_bytes = expected.as_bytes();

    if provided_bytes.ct_eq(expected_bytes).unwrap_u8() == 1 {
        Ok(())
    } else {
        Err(WebError::Forbidden)
    }
}

/// Extract the token from the request query string, if present.
pub fn extract_token_query(parts: &Parts) -> Option<String> {
    parts.uri.query().and_then(|q| {
        serde_urlencoded::from_str::<TokenQuery>(q)
            .ok()
            .map(|t| t.token)
    })
}

/// Extract the token from the `Authorization: Bearer ...` header, if present.
pub fn extract_token_header(parts: &Parts) -> Option<String> {
    parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

/// Generate a new 64-character hex session token.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}
