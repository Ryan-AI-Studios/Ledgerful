use camino::{Utf8Path, Utf8PathBuf};
use ledgerful::commands::web::auth::generate_token;
use ledgerful::commands::web::server::{make_connect_info_service, router};
use ledgerful::commands::web::state::AppState;
use ledgerful::state::layout::Layout;
use reqwest::header::{AUTHORIZATION, HOST, ORIGIN};
use std::sync::Arc;
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
    let token = generate_token();
    let state = Arc::new(AppState::new(layout, token.clone(), spa_dir, None));
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
    let state = Arc::new(AppState::new(guard.layout(), String::new(), None, None));
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
    assert!(csp.contains("object-src 'none'"));
    assert!(csp.contains("frame-ancestors 'none'"));
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
        script_part.contains("sha256-") || script_part.trim() == "'self'",
        "embedded CSP should include hashes or at least 'self': {script_part}"
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
    std::fs::write(spa.join("index.html").as_std_path(), "<html></html>").unwrap();
    std::fs::write(
        csp_dir.join("csp-script-hashes.json").as_std_path(),
        r#"{
            "routes": { "/": ["sha256-IntegrationTestHashValueABCD="] },
            "union": ["sha256-IntegrationTestHashValueABCD="]
        }"#,
    )
    .unwrap();

    let (url, _token, handle) = spawn_server_with_spa(guard.layout(), Some(spa)).await;
    let resp = client()
        .get(format!("{}/health", url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_security_headers(resp.headers());
    let csp = resp
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap();
    assert!(csp.contains("'sha256-IntegrationTestHashValueABCD='"));
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
    std::fs::write(spa.join("index.html").as_std_path(), "<html></html>").unwrap();

    let (url, _token, handle) = spawn_server_with_spa(guard.layout(), Some(spa)).await;
    let resp = client()
        .get(format!("{}/health", url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
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
        script_part.contains("unsafe-inline"),
        "missing sidecar must fall back to unsafe-inline: {script_part}"
    );
    handle.abort();
}
