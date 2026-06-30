use camino::Utf8Path;
use ledgerful::commands::web::auth::generate_token;
use ledgerful::commands::web::server::router;
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
    let token = generate_token();
    let state = Arc::new(AppState::new(layout, token.clone(), None));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let app = router(state);
    let serve = axum::serve(listener, app);
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

#[tokio::test]
async fn host_validation_rejects_non_loopback_host() {
    let guard = temp_layout();
    let (url, _token, handle) = spawn_server(guard.layout()).await;

    let status = client()
        .get(format!("{}/health", url))
        .header(HOST, "evil.com")
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();

    assert_eq!(status, 403);
    handle.abort();
}

#[tokio::test]
async fn host_validation_accepts_localhost() {
    let guard = temp_layout();
    let (url, _token, handle) = spawn_server(guard.layout()).await;
    let host = url.trim_start_matches("http://").to_string();

    let status = client()
        .get(format!("{}/health", url))
        .header(HOST, &host)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();

    assert_eq!(status, 200);
    handle.abort();
}

#[tokio::test]
async fn host_validation_accepts_ipv4_loopback() {
    let guard = temp_layout();
    let (url, _token, handle) = spawn_server(guard.layout()).await;
    let port = port_from_url(&url);

    let status = client()
        .get(format!("{}/health", url))
        .header(HOST, format!("127.0.0.1:{}", port))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();

    assert_eq!(status, 200);
    handle.abort();
}

#[tokio::test]
async fn host_validation_accepts_ipv6_bracketed() {
    let guard = temp_layout();
    let (url, _token, handle) = spawn_server(guard.layout()).await;
    let port = port_from_url(&url);

    let status = client()
        .get(format!("{}/health", url))
        .header(HOST, format!("[::1]:{}", port))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();

    assert_eq!(status, 200);
    handle.abort();
}

#[tokio::test]
async fn host_validation_accepts_ipv6_expanded() {
    let guard = temp_layout();
    let (url, _token, handle) = spawn_server(guard.layout()).await;
    let port = port_from_url(&url);

    let status = client()
        .get(format!("{}/health", url))
        .header(HOST, format!("[0:0:0:0:0:0:0:1]:{}", port))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();

    assert_eq!(status, 200);
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
