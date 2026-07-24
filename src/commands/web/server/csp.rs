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
    // Basic sanity: base64 alphabet after prefix
    let b64 = &trimmed["sha256-".len()..];
    if b64.is_empty()
        || !b64
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
    {
        return Err(format!(
            "CSP hash token has invalid base64 payload: {raw:?}"
        ));
    }
    Ok(trimmed.to_string())
}

/// Resolve CSP for an operator-supplied `--spa-dir`.
///
/// Looks for `{spa_dir.parent()}/.csp/csp-script-hashes.json`. On success
/// returns a strict hash CSP; on missing/invalid logs a warning and returns
/// the `'unsafe-inline'` fallback.
pub fn resolve_csp_for_spa_dir(spa_dir: &Utf8Path) -> String {
    let sidecar = spa_dir
        .parent()
        .map(|p| p.join(".csp").join("csp-script-hashes.json"));

    let Some(path) = sidecar else {
        tracing::warn!(
            spa_dir = %spa_dir,
            "CSP hash manifest unavailable (--spa-dir has no parent); \
             falling back to script-src 'self' 'unsafe-inline' for this instance"
        );
        return build_fallback_csp();
    };

    if !path.is_file() {
        tracing::warn!(
            path = %path,
            spa_dir = %spa_dir,
            "CSP hash manifest missing for --spa-dir; \
             falling back to script-src 'self' 'unsafe-inline' for this instance. \
             Place a valid .csp/csp-script-hashes.json next to the SPA root to enable strict hashes."
        );
        return build_fallback_csp();
    }

    match std::fs::read_to_string(path.as_std_path()) {
        Ok(json) => match parse_csp_manifest(&json) {
            Ok(hashes) if !hashes.is_empty() => {
                tracing::info!(
                    path = %path,
                    hash_count = hashes.len(),
                    "Loaded CSP script hashes from --spa-dir sidecar manifest"
                );
                build_hash_csp(&hashes)
            }
            Ok(_) => {
                tracing::warn!(
                    path = %path,
                    "CSP hash manifest is empty; falling back to script-src 'self' 'unsafe-inline'"
                );
                build_fallback_csp()
            }
            Err(e) => {
                tracing::warn!(
                    path = %path,
                    error = %e,
                    "CSP hash manifest invalid; falling back to script-src 'self' 'unsafe-inline' for this instance"
                );
                build_fallback_csp()
            }
        },
        Err(e) => {
            tracing::warn!(
                path = %path,
                error = %e,
                "Failed to read CSP hash manifest; falling back to script-src 'self' 'unsafe-inline' for this instance"
            );
            build_fallback_csp()
        }
    }
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

    #[test]
    fn parse_preferred_manifest_with_union() {
        let json = r#"{
            "routes": {
                "/": ["sha256-abc123=", "sha256-def456="],
                "/other": ["sha256-def456=", "sha256-ghi789="]
            },
            "union": ["sha256-abc123=", "sha256-def456=", "sha256-ghi789="]
        }"#;
        let hashes = parse_csp_manifest(json).unwrap();
        assert_eq!(
            hashes,
            vec![
                "sha256-abc123=".to_string(),
                "sha256-def456=".to_string(),
                "sha256-ghi789=".to_string(),
            ]
        );
    }

    #[test]
    fn parse_flat_manifest_computes_union() {
        let json = r#"{
            "/": ["sha256-zzz=", "sha256-aaa="],
            "/x": ["sha256-aaa="]
        }"#;
        let hashes = parse_csp_manifest(json).unwrap();
        assert_eq!(
            hashes,
            vec!["sha256-aaa=".to_string(), "sha256-zzz=".to_string()]
        );
    }

    #[test]
    fn parse_routes_only_without_union() {
        let json = r#"{
            "routes": {
                "/": ["sha256-one="],
                "/two": ["sha256-two=", "sha256-one="]
            }
        }"#;
        let hashes = parse_csp_manifest(json).unwrap();
        assert_eq!(
            hashes,
            vec!["sha256-one=".to_string(), "sha256-two=".to_string()]
        );
    }

    #[test]
    fn build_hash_csp_includes_hashes_without_unsafe_inline() {
        let hashes = vec!["sha256-abc=".to_string(), "sha256-def=".to_string()];
        let csp = build_hash_csp(&hashes);
        assert!(csp.contains("script-src 'self' 'sha256-abc=' 'sha256-def='"));
        assert!(!csp.contains("script-src 'self' 'unsafe-inline'"));
        assert!(
            !csp.contains("'unsafe-inline'; script-src")
                || csp.contains("style-src 'self' 'unsafe-inline'")
        );
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

    #[test]
    fn resolve_spa_dir_missing_manifest_falls_back() {
        let tmp = tempdir().unwrap();
        let spa = Utf8Path::from_path(tmp.path()).unwrap().join("out");
        std::fs::create_dir_all(spa.as_std_path()).unwrap();
        let csp = resolve_csp_for_spa_dir(&spa);
        let script_part = csp
            .split("script-src ")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        assert!(script_part.contains("unsafe-inline"));
    }

    #[test]
    fn resolve_spa_dir_valid_sidecar_uses_hashes() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let spa = root.join("out");
        let csp_dir = root.join(".csp");
        std::fs::create_dir_all(spa.as_std_path()).unwrap();
        std::fs::create_dir_all(csp_dir.as_std_path()).unwrap();
        let manifest = r#"{
            "routes": { "/": ["sha256-TestHashValue1234567890abcd="] },
            "union": ["sha256-TestHashValue1234567890abcd="]
        }"#;
        std::fs::write(
            csp_dir.join("csp-script-hashes.json").as_std_path(),
            manifest,
        )
        .unwrap();

        let csp = resolve_csp_for_spa_dir(&spa);
        assert!(csp.contains("'sha256-TestHashValue1234567890abcd='"));
        let script_part = csp
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
        let csp = resolve_csp_for_spa_dir(&spa);
        assert!(csp.contains("unsafe-inline"));
    }

    #[test]
    fn parse_rejects_non_sha256_token() {
        let json = r#"{ "union": ["md5-nope"] }"#;
        assert!(parse_csp_manifest(json).is_err());
    }
}
