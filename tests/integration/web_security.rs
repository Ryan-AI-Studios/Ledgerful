use camino::{Utf8Path, Utf8PathBuf};
use ledgerful::commands::web::auth::generate_token;
use ledgerful::commands::web::server::{make_connect_info_service, router};
use ledgerful::commands::web::state::{AppState, HANDOFF_TTL, HandoffCode};
use ledgerful::state::layout::Layout;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HOST, ORIGIN};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;

struct LayoutGuard {
    _tmp: tempfile::TempDir,
    layout: Layout,
}

impl LayoutGuard {
    fn layout(&self) -> Layout {
        self.layout.clone()
    }
}

fn temp_layout() -> LayoutGuard {
    let tmp = tempfile::tempdir().unwrap();
    let layout = Layout::new(Utf8Path::from_path(tmp.path()).unwrap());
    LayoutGuard { _tmp: tmp, layout }
}

async fn spawn_server(layout: Layout) -> (String, String, tokio::task::JoinHandle<()>) {
    spawn_server_with_spa(layout, None).await
}

async fn spawn_server_with_spa(
    layout: Layout,
    spa_dir: Option<Utf8PathBuf>,
) -> (String, String, tokio::task::JoinHandle<()>) {
    spawn_server_with_options(layout, spa_dir, None).await
}

async fn spawn_server_with_handoff(
    layout: Layout,
    handoff: Option<HandoffCode>,
) -> (String, String, tokio::task::JoinHandle<()>) {
    spawn_server_with_options(layout, None, handoff).await
}

async fn spawn_server_with_options(
    layout: Layout,
    spa_dir: Option<Utf8PathBuf>,
    handoff: Option<HandoffCode>,
) -> (String, String, tokio::task::JoinHandle<()>) {
    let token = generate_token();
    let state = Arc::new(AppState::new(layout, token.clone(), spa_dir, None, handoff));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let app = router(state);
    let serve = axum::serve(listener, make_connect_info_service(app));
    let handle = tokio::spawn(async move {
        let _ = serve.await;
    });

    let url = format!("http://{}", addr);
    (url, token, handle)
}

fn port_from_url(url: &str) -> u16 {
    let bracket_end = url.rfind(']');
    if let Some(end) = bracket_end {
        url[end + 2..].parse().unwrap()
    } else {
        url.rsplit(':').next().unwrap().parse().unwrap()
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

#[rstest::rstest]
#[case::rejects_non_loopback("evil.com", 403)]
#[case::accepts_localhost("{host}", 200)]
#[case::accepts_ipv4("127.0.0.1:{port}", 200)]
#[case::accepts_ipv6_bracketed("[::1]:{port}", 200)]
#[case::accepts_ipv6_expanded("[0:0:0:0:0:0:0:1]:{port}", 200)]
#[tokio::test]
async fn test_host_validation(#[case] host_template: &str, #[case] expected_status: u16) {
    let guard = temp_layout();
    let (url, _token, handle) = spawn_server(guard.layout()).await;
    let port = port_from_url(&url);
    let host = host_template
        .replace("{host}", url.trim_start_matches("http://"))
        .replace("{port}", &port.to_string());

    let status = client()
        .get(format!("{}/health", url))
        .header(HOST, &host)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();

    assert_eq!(status, expected_status);
    handle.abort();
}

#[tokio::test]
async fn cors_rejects_lookalike_origin() {
    let guard = temp_layout();
    let (url, _token, handle) = spawn_server(guard.layout()).await;

    let acao = client()
        .get(format!("{}/health", url))
        .header(ORIGIN, "http://localhost.evil.com")
        .send()
        .await
        .unwrap()
        .headers()
        .get("Access-Control-Allow-Origin")
        .cloned();

    assert!(
        acao.is_none(),
        "lookalike origin must not receive ACAO header"
    );
    handle.abort();
}

#[tokio::test]
async fn cors_accepts_loopback_origin() {
    let guard = temp_layout();
    let (url, _token, handle) = spawn_server(guard.layout()).await;

    let acao = client()
        .get(format!("{}/health", url))
        .header(ORIGIN, "http://localhost:3001")
        .send()
        .await
        .unwrap()
        .headers()
        .get("Access-Control-Allow-Origin")
        .cloned();

    assert_eq!(
        acao.and_then(|h| h.to_str().ok().map(|s| s.to_string())),
        Some("http://localhost:3001".to_string())
    );
    handle.abort();
}

#[tokio::test]
async fn token_required_via_authorization_header() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let no_auth = client()
        .get(format!("{}/api/snapshot", url))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();

    let with_auth = client()
        .get(format!("{}/api/snapshot", url))
        .header(AUTHORIZATION, format!("Bearer {}", token))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();

    assert_eq!(no_auth, 403);
    assert_eq!(with_auth, 200);
    handle.abort();
}

#[tokio::test]
async fn blank_bearer_token_returns_403() {
    let guard = temp_layout();
    let (url, _token, handle) = spawn_server(guard.layout()).await;

    let blank = client()
        .get(format!("{}/api/snapshot", url))
        .header(AUTHORIZATION, "Bearer ")
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();

    let whitespace = client()
        .get(format!("{}/api/status", url))
        .header(AUTHORIZATION, "Bearer    ")
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();

    assert_eq!(blank, 403, "blank Bearer must not authenticate");
    assert_eq!(whitespace, 403, "whitespace Bearer must not authenticate");
    handle.abort();
}

#[tokio::test]
async fn empty_expected_token_never_authenticates() {
    // Defense-in-depth: even if AppState were constructed with an empty token
    // (bypassing resolve_session_token), no Authorization / blank Bearer → 403.
    use ledgerful::commands::web::auth::validate_token;
    assert!(validate_token(None, "").is_err());
    assert!(validate_token(Some(String::new()), "").is_err());
    assert!(validate_token(Some("   ".into()), "   ").is_err());

    let guard = temp_layout();
    let state = Arc::new(AppState::new(
        guard.layout(),
        String::new(),
        None,
        None,
        None,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state);
    let serve = axum::serve(listener, make_connect_info_service(app));
    let handle = tokio::spawn(async move {
        let _ = serve.await;
    });
    let url = format!("http://{}", addr);

    let status = client()
        .get(format!("{}/api/snapshot", url))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, 403);
    handle.abort();
}

#[tokio::test]
async fn rate_limit_engages_on_auth_failures() {
    let guard = temp_layout();
    let (url, _token, handle) = spawn_server(guard.layout()).await;

    let mut limited = (false, 0);
    for i in 0..=70 {
        let status = client()
            .get(format!("{}/api/snapshot", url))
            .header(AUTHORIZATION, "Bearer bad-token")
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        if status == 429 {
            limited = (true, i);
            break;
        }
    }

    assert!(
        limited.0,
        "expected 429 after burst of bad auth, last request number {}",
        limited.1
    );
    handle.abort();
}

#[tokio::test]
async fn token_not_in_query_string() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let resp = client()
        .get(format!("{}/api/snapshot?token={}", url, token))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap();

    assert_eq!(status, 403, "query-string token must not authenticate");
    assert!(
        !body.contains(&token),
        "response body must not echo the token"
    );
    handle.abort();
}

/// Expected Permissions-Policy value (must match engine csp::PERMISSIONS_POLICY).
const EXPECTED_PERMISSIONS_POLICY: &str = "camera=(), microphone=(), geolocation=(), payment=(), \
usb=(), display-capture=(), accelerometer=(), gyroscope=(), magnetometer=(), \
browsing-topics=()";

fn assert_security_headers(headers: &reqwest::header::HeaderMap) {
    assert_eq!(
        headers
            .get("x-content-type-options")
            .and_then(|v| v.to_str().ok()),
        Some("nosniff")
    );
    assert_eq!(
        headers.get("x-frame-options").and_then(|v| v.to_str().ok()),
        Some("DENY")
    );
    assert_eq!(
        headers.get("referrer-policy").and_then(|v| v.to_str().ok()),
        Some("strict-origin-when-cross-origin")
    );
    assert_eq!(
        headers
            .get("permissions-policy")
            .and_then(|v| v.to_str().ok()),
        Some(EXPECTED_PERMISSIONS_POLICY)
    );
    assert_eq!(
        headers
            .get("cross-origin-opener-policy")
            .and_then(|v| v.to_str().ok()),
        Some("same-origin")
    );
    assert!(
        headers.get("strict-transport-security").is_none(),
        "daemon must never set Strict-Transport-Security"
    );
    let csp = headers
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .expect("CSP header required");
    assert!(csp.contains("default-src 'self'"));
    assert!(csp.contains("connect-src 'self'"));
    assert!(csp.contains("object-src 'none'"));
    assert!(csp.contains("base-uri 'self'"));
    assert!(csp.contains("frame-ancestors 'none'"));
    assert!(csp.contains("style-src 'self' 'unsafe-inline'"));
}

#[tokio::test]
async fn health_response_has_security_headers_and_no_hsts() {
    let guard = temp_layout();
    let (url, _token, handle) = spawn_server(guard.layout()).await;

    let resp = client()
        .get(format!("{}/health", url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_security_headers(resp.headers());

    // Embedded path uses vendored hash CSP (no script-src unsafe-inline).
    let csp = resp
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap();
    let script_part = csp
        .split("script-src ")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert!(
        !script_part.contains("unsafe-inline"),
        "embedded CSP script-src must not include unsafe-inline: {script_part}"
    );
    assert!(
        script_part.contains("sha256-"),
        "embedded CSP must include hash tokens from the vendored manifest: {script_part}"
    );

    handle.abort();
}

#[tokio::test]
async fn api_response_has_security_headers_and_no_hsts() {
    let guard = temp_layout();
    let (url, token, handle) = spawn_server(guard.layout()).await;

    let resp = client()
        .get(format!("{}/api/snapshot", url))
        .header(AUTHORIZATION, format!("Bearer {}", token))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_security_headers(resp.headers());
    handle.abort();
}

/// Valid `sha256-` token: base64 of 32 zero bytes (real digest length).
fn integration_sha256_token() -> String {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    format!("sha256-{}", STANDARD.encode([0u8; 32]))
}

#[tokio::test]
async fn spa_dir_with_sidecar_manifest_uses_hash_csp() {
    let guard = temp_layout();
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    let spa = root.join("out");
    let csp_dir = root.join(".csp");
    std::fs::create_dir_all(spa.as_std_path()).unwrap();
    std::fs::create_dir_all(csp_dir.as_std_path()).unwrap();
    // Minimal SPA marker for ServeDir (index.html)
    std::fs::write(
        spa.join("index.html").as_std_path(),
        "<!doctype html><html><body>spa-sidecar</body></html>",
    )
    .unwrap();
    let token = integration_sha256_token();
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

    let (url, _token, handle) = spawn_server_with_spa(guard.layout(), Some(spa)).await;

    // SPA document path (not just /health) must carry security headers + hash CSP.
    let resp = client().get(format!("{}/", url)).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_security_headers(resp.headers());
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("spa-sidecar"),
        "expected SPA index body, got: {body}"
    );

    // Re-fetch for headers after body consume — headers already checked above;
    // assert CSP hash on a second SPA request.
    let resp2 = client()
        .get(format!("{}/index.html", url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status().as_u16(), 200);
    assert_security_headers(resp2.headers());
    let csp = resp2
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap();
    assert!(csp.contains(&format!("'{token}'")));
    let script_part = csp
        .split("script-src ")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert!(!script_part.contains("unsafe-inline"));
    handle.abort();
}

#[tokio::test]
async fn spa_dir_without_sidecar_falls_back_to_unsafe_inline() {
    let guard = temp_layout();
    let tmp = tempfile::tempdir().unwrap();
    let spa = Utf8Path::from_path(tmp.path()).unwrap().join("out");
    std::fs::create_dir_all(spa.as_std_path()).unwrap();
    std::fs::write(
        spa.join("index.html").as_std_path(),
        "<!doctype html><html><body>spa-fallback</body></html>",
    )
    .unwrap();

    let (url, _token, handle) = spawn_server_with_spa(guard.layout(), Some(spa)).await;
    // Prove headers on the SPA document path (ServeDir), not only /health.
    let resp = client().get(format!("{}/", url)).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_security_headers(resp.headers());
    let body = resp.text().await.unwrap();
    assert!(body.contains("spa-fallback"), "expected SPA body: {body}");

    let resp2 = client().get(format!("{}/", url)).send().await.unwrap();
    let csp = resp2
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap();
    let script_part = csp
        .split("script-src ")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert!(
        script_part.contains("unsafe-inline"),
        "missing sidecar must fall back to unsafe-inline: {script_part}"
    );
    handle.abort();
}

#[tokio::test]
async fn embedded_fallback_response_has_security_headers_and_no_hsts() {
    // Debug builds serve a stub for embedded SPA (no --spa-dir). Middleware
    // still wraps that fallback — prove headers + hash CSP + no HSTS on the
    // SPA route itself (not only /health).
    let guard = temp_layout();
    let (url, _token, handle) = spawn_server(guard.layout()).await;
    let resp = client().get(format!("{}/", url)).send().await.unwrap();
    // Debug: 404 stub; release: 200 SPA. Either way headers must apply.
    assert_security_headers(resp.headers());
    let csp = resp
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap();
    let script_part = csp
        .split("script-src ")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert!(
        !script_part.contains("unsafe-inline"),
        "embedded path must not use script-src unsafe-inline: {script_part}"
    );
    assert!(
        script_part.contains("sha256-"),
        "embedded path must ship vendored hashes: {script_part}"
    );
    handle.abort();
}

// ---------------------------------------------------------------------------
// Track 0090 — single-use session handoff (DoD-2/3/4/5)
// ---------------------------------------------------------------------------

/// DoD-4: exchange is reachable without Authorization AND protected routes still
/// require a token. Both directions in one test so neither can be dropped later.
#[tokio::test]
async fn session_exchange_public_but_protected_routes_still_gated() {
    let code = generate_token();
    let guard = temp_layout();
    let (url, token, handle) = spawn_server_with_handoff(
        guard.layout(),
        Some(HandoffCode {
            code: code.clone(),
            expires_at: Instant::now() + HANDOFF_TTL,
        }),
    )
    .await;

    // Public: exchange succeeds without Authorization when handoff is valid.
    let exchange = client()
        .post(format!("{}/api/session/exchange", url))
        .header(CONTENT_TYPE, "application/json")
        .body(format!(r#"{{"code":"{code}"}}"#))
        .send()
        .await
        .unwrap();
    assert_eq!(
        exchange.status().as_u16(),
        200,
        "exchange must succeed without Authorization"
    );
    let body: serde_json::Value =
        serde_json::from_str(&exchange.text().await.unwrap()).expect("exchange json");
    assert_eq!(body["token"].as_str(), Some(token.as_str()));

    // Protected: these must still 403 without Authorization (layer not dropped).
    for path in ["/api/snapshot", "/api/status", "/api/events"] {
        let status = client()
            .get(format!("{url}{path}"))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        assert_eq!(
            status, 403,
            "{path} must still require Authorization after public merge"
        );
    }
    handle.abort();
}

#[tokio::test]
async fn session_exchange_rejects_non_loopback_origin() {
    let code = generate_token();
    let guard = temp_layout();
    let (url, token, handle) = spawn_server_with_handoff(
        guard.layout(),
        Some(HandoffCode {
            code: code.clone(),
            expires_at: Instant::now() + HANDOFF_TTL,
        }),
    )
    .await;

    let evil = client()
        .post(format!("{}/api/session/exchange", url))
        .header(ORIGIN, "https://evil.example")
        .header(CONTENT_TYPE, "application/json")
        .body(format!(r#"{{"code":"{code}"}}"#))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(evil, 403, "present non-loopback Origin must 403");

    // Missing Origin still succeeds; Origin reject must not burn the code.
    let ok = client()
        .post(format!("{}/api/session/exchange", url))
        .header(CONTENT_TYPE, "application/json")
        .body(format!(r#"{{"code":"{code}"}}"#))
        .send()
        .await
        .unwrap();
    assert_eq!(
        ok.status().as_u16(),
        200,
        "missing Origin must still succeed on a valid handoff"
    );
    let body: serde_json::Value =
        serde_json::from_str(&ok.text().await.unwrap()).expect("exchange json");
    assert_eq!(body["token"].as_str(), Some(token.as_str()));
    handle.abort();
}

#[tokio::test]
async fn session_exchange_rejects_non_loopback_host() {
    let code = generate_token();
    let guard = temp_layout();
    let (url, _token, handle) = spawn_server_with_handoff(
        guard.layout(),
        Some(HandoffCode {
            code: code.clone(),
            expires_at: Instant::now() + HANDOFF_TTL,
        }),
    )
    .await;

    let status = client()
        .post(format!("{}/api/session/exchange", url))
        .header(HOST, "evil.com")
        .header(CONTENT_TYPE, "application/json")
        .body(format!(r#"{{"code":"{code}"}}"#))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(
        status, 403,
        "outer host_validation_layer must wrap the exchange path"
    );
    handle.abort();
}

#[tokio::test]
async fn session_exchange_happy_path_and_replay_forbidden() {
    let code = generate_token();
    let guard = temp_layout();
    let (url, token, handle) = spawn_server_with_handoff(
        guard.layout(),
        Some(HandoffCode {
            code: code.clone(),
            expires_at: Instant::now() + HANDOFF_TTL,
        }),
    )
    .await;

    let first = client()
        .post(format!("{}/api/session/exchange", url))
        .header(CONTENT_TYPE, "application/json")
        .body(format!(r#"{{"code":"{code}"}}"#))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status().as_u16(), 200);
    let body: serde_json::Value =
        serde_json::from_str(&first.text().await.unwrap()).expect("exchange json");
    assert_eq!(body["token"].as_str(), Some(token.as_str()));

    // Replay must fail (consume-on-match).
    let second = client()
        .post(format!("{}/api/session/exchange", url))
        .header(CONTENT_TYPE, "application/json")
        .body(format!(r#"{{"code":"{code}"}}"#))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(second, 403, "replay after consume must 403");
    handle.abort();
}

#[tokio::test]
async fn session_exchange_mismatch_does_not_burn_code() {
    let code = generate_token();
    let guard = temp_layout();
    let (url, token, handle) = spawn_server_with_handoff(
        guard.layout(),
        Some(HandoffCode {
            code: code.clone(),
            expires_at: Instant::now() + HANDOFF_TTL,
        }),
    )
    .await;

    let wrong = "0".repeat(64);
    let mismatch = client()
        .post(format!("{}/api/session/exchange", url))
        .header(CONTENT_TYPE, "application/json")
        .body(format!(r#"{{"code":"{wrong}"}}"#))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(mismatch, 403, "wrong guess must 403");

    // Correct code still works afterwards (not burned).
    let ok = client()
        .post(format!("{}/api/session/exchange", url))
        .header(CONTENT_TYPE, "application/json")
        .body(format!(r#"{{"code":"{code}"}}"#))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status().as_u16(), 200);
    let body: serde_json::Value =
        serde_json::from_str(&ok.text().await.unwrap()).expect("exchange json");
    assert_eq!(body["token"].as_str(), Some(token.as_str()));
    handle.abort();
}

#[tokio::test]
async fn session_exchange_expired_and_absent_forbid() {
    let code = generate_token();
    let guard = temp_layout();
    let (url, _token, handle) = spawn_server_with_handoff(
        guard.layout(),
        Some(HandoffCode {
            code: code.clone(),
            expires_at: Instant::now() - Duration::from_secs(1),
        }),
    )
    .await;

    let expired = client()
        .post(format!("{}/api/session/exchange", url))
        .header(CONTENT_TYPE, "application/json")
        .body(format!(r#"{{"code":"{code}"}}"#))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(expired, 403, "expired handoff must 403");
    handle.abort();

    // Absent handoff (no --open).
    let guard2 = temp_layout();
    let (url2, _token2, handle2) = spawn_server(guard2.layout()).await;
    let absent = client()
        .post(format!("{}/api/session/exchange", url2))
        .header(CONTENT_TYPE, "application/json")
        .body(format!(r#"{{"code":"{}"}}"#, generate_token()))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(absent, 403, "absent handoff must 403");
    handle2.abort();
}
