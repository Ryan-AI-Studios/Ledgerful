//! Web dashboard authentication.

use crate::commands::web::error::WebError;
use axum::http::header;
use axum::http::request::Parts;
use miette::{Result, miette};
use rand::RngCore;
use subtle::ConstantTimeEq;

/// Minimum length for an operator-supplied session token (after trim).
/// Auto-generated tokens are 64 hex chars (256-bit).
pub const MIN_USER_TOKEN_LEN: usize = 32;

/// How a session token was obtained at process start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenResolution {
    /// Freshly generated 256-bit hex token.
    Generated(String),
    /// Operator-supplied token (CLI `--token` or `LEDGERFUL_WEB_TOKEN`).
    Provided(String),
}

impl TokenResolution {
    /// Borrow the resolved token string.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Generated(t) | Self::Provided(t) => t,
        }
    }

    /// Consume into the owned token string.
    pub fn into_token(self) -> String {
        match self {
            Self::Generated(t) | Self::Provided(t) => t,
        }
    }
}

/// Resolve the session token from optional CLI and env values.
///
/// Priority: CLI `--token` over `LEDGERFUL_WEB_TOKEN` over auto-generate.
///
/// - Absent CLI and absent env → generate.
/// - Explicit empty/whitespace CLI or env → refuse (fail closed).
/// - Explicit short (< [`MIN_USER_TOKEN_LEN`] after trim) → refuse.
/// - Never yields an empty expected token.
pub fn resolve_session_token(
    cli_token: Option<String>,
    env_token: Option<String>,
) -> Result<TokenResolution> {
    if let Some(raw) = cli_token {
        return validate_user_supplied_token(raw, "CLI --token").map(TokenResolution::Provided);
    }
    if let Some(raw) = env_token {
        return validate_user_supplied_token(raw, "LEDGERFUL_WEB_TOKEN")
            .map(TokenResolution::Provided);
    }
    Ok(TokenResolution::Generated(generate_token()))
}

fn validate_user_supplied_token(raw: String, source: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(miette!(
            "Empty or whitespace-only session token from {source}. \
             Omit the flag/env to auto-generate a 256-bit token, or supply a token \
             of at least {MIN_USER_TOKEN_LEN} characters."
        ));
    }
    if trimmed.len() < MIN_USER_TOKEN_LEN {
        return Err(miette!(
            "Session token from {source} is too short ({} chars after trim; \
             minimum is {MIN_USER_TOKEN_LEN}). Use the auto-generated token \
             (omit --token / LEDGERFUL_WEB_TOKEN) or paste a ≥{MIN_USER_TOKEN_LEN}-character secret.",
            trimmed.len()
        ));
    }
    Ok(trimmed.to_string())
}

/// Validate a provided token string against the expected session token using
/// constant-time comparison.
///
/// Fail-closed rules:
/// - Deny when `expected` is empty/whitespace (defense-in-depth if resolution is buggy).
/// - Deny when no token is provided or the provided value is empty/whitespace after trim.
/// - Trim the provided token once (after `Bearer ` strip upstream); never shorten `expected`.
pub fn validate_token(provided: Option<String>, expected: &str) -> Result<(), WebError> {
    if expected.is_empty() || expected.chars().all(char::is_whitespace) {
        return Err(WebError::Forbidden);
    }

    let Some(raw) = provided else {
        return Err(WebError::Forbidden);
    };
    let provided = raw.trim();
    if provided.is_empty() {
        return Err(WebError::Forbidden);
    }

    let provided_bytes = provided.as_bytes();
    let expected_bytes = expected.as_bytes();

    // `ct_eq` returns 0 when lengths differ; never pad/shorten `expected`.
    if provided_bytes.ct_eq(expected_bytes).unwrap_u8() == 1 {
        Ok(())
    } else {
        Err(WebError::Forbidden)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn long_token() -> String {
        "a".repeat(MIN_USER_TOKEN_LEN)
    }

    #[test]
    fn resolve_missing_both_generates() {
        let r = resolve_session_token(None, None).unwrap();
        match r {
            TokenResolution::Generated(t) => {
                assert_eq!(t.len(), 64);
                assert!(!t.trim().is_empty());
            }
            TokenResolution::Provided(_) => panic!("expected Generated"),
        }
    }

    #[test]
    fn resolve_cli_empty_refuses() {
        let err = resolve_session_token(Some(String::new()), None).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("Empty") || msg.contains("empty"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn resolve_cli_whitespace_refuses() {
        let err = resolve_session_token(Some("   \t  ".into()), None).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("Empty") || msg.contains("empty") || msg.contains("whitespace"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn resolve_env_empty_refuses() {
        let err = resolve_session_token(None, Some(String::new())).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("LEDGERFUL_WEB_TOKEN") || msg.contains("Empty"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn resolve_env_whitespace_refuses() {
        let err = resolve_session_token(None, Some(" \n ".into())).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("Empty") || msg.contains("whitespace") || msg.contains("LEDGERFUL"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn resolve_cli_short_refuses() {
        let err = resolve_session_token(Some("password123".into()), None).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("too short") || msg.contains("minimum"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn resolve_env_short_refuses() {
        let err = resolve_session_token(None, Some("short".into())).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("too short") || msg.contains("minimum"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn resolve_cli_takes_precedence_over_env() {
        let cli = long_token();
        let env = "b".repeat(MIN_USER_TOKEN_LEN);
        let r = resolve_session_token(Some(cli.clone()), Some(env)).unwrap();
        assert_eq!(r, TokenResolution::Provided(cli));
    }

    #[test]
    fn resolve_cli_valid_provided() {
        let t = long_token();
        let r = resolve_session_token(Some(format!("  {t}  ")), None).unwrap();
        assert_eq!(r, TokenResolution::Provided(t));
    }

    #[test]
    fn resolve_env_valid_when_cli_absent() {
        let t = long_token();
        let r = resolve_session_token(None, Some(t.clone())).unwrap();
        assert_eq!(r, TokenResolution::Provided(t));
    }

    #[test]
    fn resolve_never_yields_empty() {
        for case in [
            resolve_session_token(None, None),
            resolve_session_token(Some(long_token()), None),
            resolve_session_token(None, Some(long_token())),
        ] {
            let token = case.unwrap().into_token();
            assert!(!token.is_empty());
            assert!(!token.trim().is_empty());
            assert!(token.len() >= MIN_USER_TOKEN_LEN);
        }
    }

    #[test]
    fn validate_denies_empty_expected() {
        assert!(validate_token(Some(long_token()), "").is_err());
        assert!(validate_token(Some(long_token()), "   ").is_err());
    }

    #[test]
    fn validate_denies_missing_provided() {
        let expected = long_token();
        assert!(validate_token(None, &expected).is_err());
    }

    #[test]
    fn validate_denies_blank_provided() {
        let expected = long_token();
        assert!(validate_token(Some(String::new()), &expected).is_err());
        assert!(validate_token(Some("  \t".into()), &expected).is_err());
    }

    #[test]
    fn validate_accepts_exact_match() {
        let expected = long_token();
        assert!(validate_token(Some(expected.clone()), &expected).is_ok());
    }

    #[test]
    fn validate_trims_provided_once() {
        let expected = long_token();
        assert!(validate_token(Some(format!("  {expected}  ")), &expected).is_ok());
    }

    #[test]
    fn validate_does_not_trim_expected() {
        // Expected with intentional trailing space must not match trimmed-only provided.
        let base = long_token();
        let expected_with_space = format!("{base} ");
        assert!(validate_token(Some(base), &expected_with_space).is_err());
    }

    #[test]
    fn validate_rejects_wrong_token() {
        let expected = long_token();
        let wrong = "b".repeat(MIN_USER_TOKEN_LEN);
        assert!(validate_token(Some(wrong), &expected).is_err());
    }

    #[test]
    fn blank_expected_plus_no_auth_is_denied() {
        // RT-W0 regression: empty expected + missing Authorization must never Ok.
        assert!(validate_token(None, "").is_err());
        assert!(validate_token(Some(String::new()), "").is_err());
    }
}
