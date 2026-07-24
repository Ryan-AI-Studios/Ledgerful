//! Content-Security-Policy construction for the daemon-served SPA.
//!
//! # Vendored hash manifest
//!
//! The embedded default CSP is built from `csp_script_hashes.json` in this
//! directory — a vendored copy of `ledgerful-frontend/.csp/csp-script-hashes.json`.
//! Keep them in sync when the frontend build (track 0081) lands new inline
//! scripts; engine CI must not depend on a sibling frontend checkout.
//!
//! # `--spa-dir` sidecar
//!
//! When an operator supplies `--spa-dir`, the daemon looks for
//! `{spa_dir.parent()}/.csp/csp-script-hashes.json`. Present + valid → strict
//! hash-based `script-src`. Missing/invalid → `tracing::warn!` and
//! `script-src 'self' 'unsafe-inline'` for that instance only.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use camino::Utf8Path;
use serde_json::Value;

/// Vendored frontend CSP script-hash manifest (routes + union).
const VENDORED_CSP_MANIFEST: &str = include_str!("csp_script_hashes.json");

/// Permissions-Policy deny-by-default list (OWASP Secure Headers baseline).
pub const PERMISSIONS_POLICY: &str = "camera=(), microphone=(), geolocation=(), payment=(), \
usb=(), display-capture=(), accelerometer=(), gyroscope=(), magnetometer=(), \
browsing-topics=()";

/// Build a full daemon CSP header value from `script-src` tokens (without quotes
/// around hashes; each hash is `sha256-...` or a keyword like `'self'`).
///
/// Daemon `connect-src` stays `'self'` (SPA and `/api/*` share origin).
pub fn build_csp_header(script_src_tokens: &[String]) -> String {
    let script_src = if script_src_tokens.is_empty() {
        "'self'".to_string()
    } else {
        script_src_tokens.join(" ")
    };
    format!(
        "default-src 'self'; connect-src 'self'; img-src 'self' data:; \
         style-src 'self' 'unsafe-inline'; script-src {script_src}; \
         object-src 'none'; base-uri 'self'; frame-ancestors 'none'"
    )
}

/// Build CSP with strict hash tokens: `script-src 'self' <hashes...>`.
pub fn build_hash_csp(hashes: &[String]) -> String {
    let mut tokens = Vec::with_capacity(hashes.len() + 1);
    tokens.push("'self'".to_string());
    for h in hashes {
        // Accept either bare `sha256-...` or already-quoted `'sha256-...'`.
        let token = if h.starts_with('\'') {
            h.clone()
        } else {
            format!("'{h}'")
        };
        tokens.push(token);
    }
    build_csp_header(&tokens)
}

/// Build the documented `--spa-dir` fallback CSP (includes `'unsafe-inline'`).
pub fn build_fallback_csp() -> String {
    build_csp_header(&["'self'".to_string(), "'unsafe-inline'".to_string()])
}

/// Parse a CSP hash manifest JSON into sorted unique `sha256-...` strings
/// (without surrounding quotes).
///
/// Accepts:
/// - Preferred: `{ "routes": { "/": ["sha256-..."], ... }, "union": ["sha256-..."] }`
/// - Flat forward-compat: `{ "/": ["sha256-..."], ... }` (union computed)
pub fn parse_csp_manifest(json: &str) -> Result<Vec<String>, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|e| format!("invalid CSP manifest JSON: {e}"))?;

    let mut set = BTreeSet::new();

    match &value {
        Value::Object(map) => {
            // Preferred shape with top-level `union`.
            if let Some(union) = map.get("union") {
                collect_hash_array(union, &mut set)?;
            } else if let Some(routes) = map.get("routes") {
                // routes object only — walk all arrays
                collect_hashes_from_routes_object(routes, &mut set)?;
            } else {
                // Flat Record<route, hashes[]> — every value that is an array of strings
                for (key, v) in map {
                    // Skip non-route metadata keys if any future ones appear
                    if key == "version" || key == "generatedAt" {
                        continue;
                    }
                    if v.is_array() {
                        collect_hash_array(v, &mut set)?;
                    }
                }
            }
            // If both union and routes exist, union already populated; if only
            // routes under preferred shape without union, handle routes above.
            // When union exists but is empty and routes has data, also merge routes.
            if set.is_empty()
                && let Some(routes) = map.get("routes")
            {
                collect_hashes_from_routes_object(routes, &mut set)?;
            }
        }
        _ => return Err("CSP manifest root must be a JSON object".to_string()),
    }

    Ok(set.into_iter().collect())
}

fn collect_hashes_from_routes_object(
    routes: &Value,
    set: &mut BTreeSet<String>,
) -> Result<(), String> {
    let obj = routes
        .as_object()
        .ok_or_else(|| "CSP manifest `routes` must be an object".to_string())?;
    for (_route, hashes) in obj {
        collect_hash_array(hashes, set)?;
    }
    Ok(())
}

fn collect_hash_array(value: &Value, set: &mut BTreeSet<String>) -> Result<(), String> {
    let arr = value
        .as_array()
        .ok_or_else(|| "CSP hash list must be a JSON array".to_string())?;
    for item in arr {
        let s = item
            .as_str()
            .ok_or_else(|| "CSP hash entry must be a string".to_string())?;
        let normalized = normalize_hash_token(s)?;
        set.insert(normalized);
    }
    Ok(())
}

fn normalize_hash_token(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_matches('\'');
    if !trimmed.starts_with("sha256-") {
        return Err(format!(
            "CSP hash token must start with sha256- (got {raw:?})"
        ));
    }
    // Strict: payload must be standard base64 that decodes to exactly 32 bytes
    // (SHA-256 digest length). Rejects alphabet-only placeholders that are not
    // real digests so a malformed --spa-dir sidecar falls back honestly.
    let b64 = &trimmed["sha256-".len()..];
    if b64.is_empty() {
        return Err(format!("CSP hash token has empty base64 payload: {raw:?}"));
    }
    let decoded = BASE64_STANDARD
        .decode(b64)
        .map_err(|e| format!("CSP hash token base64 decode failed ({e}); token={raw:?}"))?;
    if decoded.len() != 32 {
        return Err(format!(
            "CSP hash token must decode to 32-byte SHA-256 digest (got {} bytes): {raw:?}",
            decoded.len()
        ));
    }
    Ok(trimmed.to_string())
}

/// Result of resolving CSP for an operator-supplied `--spa-dir`.
///
/// When [`Self::fallback_reason`] is `Some`, production code must log a warning
/// and use the fallback CSP (DoD-3 honesty contract).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaDirCspResolve {
    pub csp: String,
    /// Human-readable reason the strict hash path was not used (testable).
    pub fallback_reason: Option<String>,
}

/// Resolve CSP for `--spa-dir` without logging (pure for unit tests).
///
/// Looks for `{spa_dir.parent()}/.csp/csp-script-hashes.json`. On success
/// returns a strict hash CSP; on missing/invalid returns the `'unsafe-inline'`
/// fallback with a non-empty [`SpaDirCspResolve::fallback_reason`].
pub fn resolve_csp_for_spa_dir_detailed(spa_dir: &Utf8Path) -> SpaDirCspResolve {
    let sidecar = spa_dir
        .parent()
        .map(|p| p.join(".csp").join("csp-script-hashes.json"));

    let Some(path) = sidecar else {
        return SpaDirCspResolve {
            csp: build_fallback_csp(),
            fallback_reason: Some(
                "CSP hash manifest unavailable (--spa-dir has no parent); \
                 falling back to script-src 'self' 'unsafe-inline' for this instance"
                    .to_string(),
            ),
        };
    };

    if !path.is_file() {
        return SpaDirCspResolve {
            csp: build_fallback_csp(),
            fallback_reason: Some(format!(
                "CSP hash manifest missing for --spa-dir at {path}; \
                 falling back to script-src 'self' 'unsafe-inline' for this instance. \
                 Place a valid .csp/csp-script-hashes.json next to the SPA root to enable strict hashes."
            )),
        };
    }

    match std::fs::read_to_string(path.as_std_path()) {
        Ok(json) => match parse_csp_manifest(&json) {
            Ok(hashes) if !hashes.is_empty() => SpaDirCspResolve {
                csp: build_hash_csp(&hashes),
                fallback_reason: None,
            },
            Ok(_) => SpaDirCspResolve {
                csp: build_fallback_csp(),
                fallback_reason: Some(format!(
                    "CSP hash manifest is empty at {path}; \
                     falling back to script-src 'self' 'unsafe-inline'"
                )),
            },
            Err(e) => SpaDirCspResolve {
                csp: build_fallback_csp(),
                fallback_reason: Some(format!(
                    "CSP hash manifest invalid at {path}: {e}; \
                     falling back to script-src 'self' 'unsafe-inline' for this instance"
                )),
            },
        },
        Err(e) => SpaDirCspResolve {
            csp: build_fallback_csp(),
            fallback_reason: Some(format!(
                "Failed to read CSP hash manifest at {path}: {e}; \
                 falling back to script-src 'self' 'unsafe-inline' for this instance"
            )),
        },
    }
}

/// Resolve CSP for an operator-supplied `--spa-dir`.
///
/// Looks for `{spa_dir.parent()}/.csp/csp-script-hashes.json`. On success
/// returns a strict hash CSP; on missing/invalid logs a warning and returns
/// the `'unsafe-inline'` fallback.
pub fn resolve_csp_for_spa_dir(spa_dir: &Utf8Path) -> String {
    let resolved = resolve_csp_for_spa_dir_detailed(spa_dir);
    if let Some(reason) = &resolved.fallback_reason {
        tracing::warn!(spa_dir = %spa_dir, "{reason}");
    } else {
        tracing::info!(
            spa_dir = %spa_dir,
            "Loaded CSP script hashes from --spa-dir sidecar manifest"
        );
    }
    resolved.csp
}

/// Embedded (default) CSP header string from the vendored manifest.
///
/// Parsed once via [`OnceLock`]. On parse failure (corrupt vendored file) logs
/// an error and uses `script-src 'self'` only — tests assert the committed
/// vendored JSON parses cleanly so this path is defensive only.
pub fn embedded_csp() -> &'static str {
    static CSP: OnceLock<String> = OnceLock::new();
    CSP.get_or_init(|| match parse_csp_manifest(VENDORED_CSP_MANIFEST) {
        Ok(hashes) if !hashes.is_empty() => build_hash_csp(&hashes),
        Ok(_) => {
            tracing::error!("Vendored CSP manifest has no hashes; using script-src 'self' only");
            build_csp_header(&["'self'".to_string()])
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                "Vendored CSP manifest failed to parse; using script-src 'self' only"
            );
            build_csp_header(&["'self'".to_string()])
        }
    })
    .as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Token for a synthetic 32-byte digest (`byte` repeated).
    fn digest_token(byte: u8) -> String {
        format!("sha256-{}", BASE64_STANDARD.encode([byte; 32]))
    }

    #[test]
    fn parse_preferred_manifest_with_union() {
        let a = digest_token(1);
        let b = digest_token(2);
        let c = digest_token(3);
        let json = format!(
            r#"{{
            "routes": {{
                "/": ["{a}", "{b}"],
                "/other": ["{b}", "{c}"]
            }},
            "union": ["{a}", "{b}", "{c}"]
        }}"#
        );
        let hashes = parse_csp_manifest(&json).unwrap();
        assert_eq!(hashes, vec![a, b, c]);
    }

    #[test]
    fn parse_flat_manifest_computes_union() {
        let aaa = digest_token(10);
        let zzz = digest_token(20);
        let json = format!(
            r#"{{
            "/": ["{zzz}", "{aaa}"],
            "/x": ["{aaa}"]
        }}"#
        );
        let hashes = parse_csp_manifest(&json).unwrap();
        // BTreeSet sorts lexicographically by token string
        let mut expected = vec![aaa, zzz];
        expected.sort();
        assert_eq!(hashes, expected);
    }

    #[test]
    fn parse_routes_only_without_union() {
        let one = digest_token(30);
        let two = digest_token(40);
        let json = format!(
            r#"{{
            "routes": {{
                "/": ["{one}"],
                "/two": ["{two}", "{one}"]
            }}
        }}"#
        );
        let hashes = parse_csp_manifest(&json).unwrap();
        let mut expected = vec![one, two];
        expected.sort();
        assert_eq!(hashes, expected);
    }

    #[test]
    fn build_hash_csp_includes_hashes_without_unsafe_inline() {
        let abc = digest_token(50);
        let def = digest_token(60);
        let hashes = vec![abc.clone(), def.clone()];
        let csp = build_hash_csp(&hashes);
        assert!(csp.contains(&format!("script-src 'self' '{abc}' '{def}'")));
        assert!(csp.contains("style-src 'self' 'unsafe-inline'"));
        // script-src must not include unsafe-inline
        let script_part = csp
            .split("script-src ")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        assert!(
            !script_part.contains("unsafe-inline"),
            "script-src must not include unsafe-inline: {script_part}"
        );
        assert!(csp.contains("object-src 'none'"));
        assert!(csp.contains("base-uri 'self'"));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(csp.contains("connect-src 'self'"));
    }

    #[test]
    fn embedded_csp_uses_vendored_hashes_not_unsafe_inline() {
        let csp = embedded_csp();
        assert!(
            csp.contains("sha256-"),
            "embedded CSP must include vendored script hashes"
        );
        let script_part = csp
            .split("script-src ")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        assert!(
            !script_part.contains("unsafe-inline"),
            "embedded script-src must not use unsafe-inline: {script_part}"
        );
    }

    #[test]
    fn build_fallback_includes_unsafe_inline_for_scripts() {
        let csp = build_fallback_csp();
        let script_part = csp
            .split("script-src ")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        assert!(
            script_part.contains("unsafe-inline"),
            "fallback must allow unsafe-inline scripts: {script_part}"
        );
    }

    #[test]
    fn vendored_manifest_parses_and_has_hashes() {
        let hashes = parse_csp_manifest(VENDORED_CSP_MANIFEST).expect("vendored manifest");
        assert!(
            !hashes.is_empty(),
            "vendored CSP manifest must contain at least one hash"
        );
        for h in &hashes {
            assert!(h.starts_with("sha256-"), "bad hash token: {h}");
        }
        let csp = build_hash_csp(&hashes);
        let script_part = csp
            .split("script-src ")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        assert!(!script_part.contains("unsafe-inline"));
    }

    /// Real base64 of 32 zero bytes (valid SHA-256 digest length).
    fn sample_sha256_token() -> String {
        let digest = [0u8; 32];
        format!("sha256-{}", BASE64_STANDARD.encode(digest))
    }

    #[test]
    fn resolve_spa_dir_missing_manifest_falls_back() {
        let tmp = tempdir().unwrap();
        let spa = Utf8Path::from_path(tmp.path()).unwrap().join("out");
        std::fs::create_dir_all(spa.as_std_path()).unwrap();
        let resolved = resolve_csp_for_spa_dir_detailed(&spa);
        let script_part = resolved
            .csp
            .split("script-src ")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        assert!(script_part.contains("unsafe-inline"));
        let reason = resolved
            .fallback_reason
            .expect("missing sidecar must surface a fallback reason for warn");
        assert!(
            reason.contains("missing") || reason.contains("unavailable"),
            "reason should explain missing sidecar: {reason}"
        );
    }

    #[test]
    fn resolve_spa_dir_valid_sidecar_uses_hashes() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let spa = root.join("out");
        let csp_dir = root.join(".csp");
        std::fs::create_dir_all(spa.as_std_path()).unwrap();
        std::fs::create_dir_all(csp_dir.as_std_path()).unwrap();
        let token = sample_sha256_token();
        let manifest = format!(
            r#"{{
            "routes": {{ "/": ["{token}"] }},
            "union": ["{token}"]
        }}"#
        );
        std::fs::write(
            csp_dir.join("csp-script-hashes.json").as_std_path(),
            manifest,
        )
        .unwrap();

        let resolved = resolve_csp_for_spa_dir_detailed(&spa);
        assert!(
            resolved.fallback_reason.is_none(),
            "valid sidecar must not fall back: {:?}",
            resolved.fallback_reason
        );
        assert!(resolved.csp.contains(&format!("'{token}'")));
        let script_part = resolved
            .csp
            .split("script-src ")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        assert!(!script_part.contains("unsafe-inline"));
    }

    #[test]
    fn resolve_spa_dir_invalid_json_falls_back() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let spa = root.join("out");
        let csp_dir = root.join(".csp");
        std::fs::create_dir_all(spa.as_std_path()).unwrap();
        std::fs::create_dir_all(csp_dir.as_std_path()).unwrap();
        std::fs::write(
            csp_dir.join("csp-script-hashes.json").as_std_path(),
            "not-json{{",
        )
        .unwrap();
        let resolved = resolve_csp_for_spa_dir_detailed(&spa);
        assert!(resolved.csp.contains("unsafe-inline"));
        let reason = resolved
            .fallback_reason
            .expect("invalid JSON must surface a fallback reason");
        assert!(
            reason.contains("invalid"),
            "reason should note invalid manifest: {reason}"
        );
    }

    #[test]
    fn resolve_spa_dir_placeholder_hash_falls_back() {
        // Alphabet-looking but not a real 32-byte digest — must not be treated
        // as a valid strict sidecar (DoD-3 honesty).
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let spa = root.join("out");
        let csp_dir = root.join(".csp");
        std::fs::create_dir_all(spa.as_std_path()).unwrap();
        std::fs::create_dir_all(csp_dir.as_std_path()).unwrap();
        let manifest = r#"{
            "union": ["sha256-TestHashValue1234567890abcd="]
        }"#;
        std::fs::write(
            csp_dir.join("csp-script-hashes.json").as_std_path(),
            manifest,
        )
        .unwrap();
        let resolved = resolve_csp_for_spa_dir_detailed(&spa);
        assert!(
            resolved.fallback_reason.is_some(),
            "placeholder hash must fall back"
        );
        assert!(resolved.csp.contains("unsafe-inline"));
    }

    #[test]
    fn parse_rejects_non_sha256_token() {
        let json = r#"{ "union": ["md5-nope"] }"#;
        assert!(parse_csp_manifest(json).is_err());
    }

    #[test]
    fn parse_rejects_non_digest_length_base64() {
        // Valid base64 of 1 byte, not 32.
        let json = r#"{ "union": ["sha256-AQ=="] }"#;
        assert!(parse_csp_manifest(json).is_err());
    }
}
