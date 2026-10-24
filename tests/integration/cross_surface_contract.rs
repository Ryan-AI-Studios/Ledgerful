//! CLI, MCP, and loopback HTTP agree on pending and unaudited counts.
//!
//! SSE is read with the same HTTP/1.1 `TcpStream` pattern as `web_api.rs`.
//! Reqwest here has no `stream` feature, so `GET /api/events` is not `.text().await`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use ledgerful::commands::web::auth::generate_token;
use ledgerful::commands::web::server::{make_connect_info_service, router};
use ledgerful::commands::web::state::AppState;
use ledgerful::config::model::Config;
use ledgerful::ledger::drift::DriftManager;
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HOST, ORIGIN};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream as TokioTcpStream};

use crate::common::{git_add_and_commit, nested_repo_root, setup_git_repo};
use crate::mcp_server::tests::structured_harness::{assert_structured_success, spawn_session};

const SEARCH_SUMMARY: &str = "0560 search hit";
const WRONG_BEARER: &str = "0123456789abcdef0123456789abcdef";

fn repo() -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = nested_repo_root(tmp.path());
    setup_git_repo(&root);
    (tmp, root)
}

fn ledgerful(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .current_dir(root)
        .arg("-C")
        .arg(root)
        .args(args)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .expect("spawn ledgerful")
}

fn assert_ok(output: &std::process::Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(root: &Path) {
    let output = ledgerful(root, &["init"]);
    assert_ok(&output, "ledgerful init");
}

fn stdout_json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON ({err}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// Two drift events on one connection, then drop that connection before `ledger start`.
fn seed_drift_and_pending(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("src");
    std::fs::create_dir_all(root.join("notes")).expect("notes");
    std::fs::write(root.join("src/pending.rs"), "pub fn pending() {}\n").expect("pending.rs");
    std::fs::write(root.join("notes/unaudited.txt"), "note\n").expect("note");
    git_add_and_commit(root, "fixture: count files");
    init_repo(root);

    let db_path = root.join(".ledgerful").join("state").join("ledger.db");
    {
        let mut storage = StorageManager::init(&db_path).expect("open ledger");
        {
            let mut drift = DriftManager::new(
                storage.get_connection_mut(),
                root.to_path_buf(),
                Config::default(),
            );
            drift
                .process_event("notes/unaudited.txt")
                .expect("first drift");
            drift
                .process_event("notes/unaudited.txt")
                .expect("second drift");
        }
        storage.shutdown().expect("drop ledger connection");
    }

    let start = ledgerful(
        root,
        &[
            "ledger",
            "start",
            "src/pending.rs",
            "--category",
            "CHORE",
            "--message",
            "0560 count fixture",
        ],
    );
    assert_ok(&start, "ledger start");
}

fn commit_search_hit(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("src");
    std::fs::write(root.join("src/search.rs"), "pub fn search_hit() {}\n").expect("search.rs");
    git_add_and_commit(root, "fixture: search file");
    init_repo(root);
    let start = ledgerful(
        root,
        &[
            "ledger",
            "start",
            "src/search.rs",
            "--category",
            "CHORE",
            "--message",
            "0560 search seed",
        ],
    );
    assert_ok(&start, "ledger start");
    let status = ledgerful(root, &["ledger", "status", "--json"]);
    assert_ok(&status, "ledger status");
    let tx_id = stdout_json(&status)["pendingTxIds"][0]
        .as_str()
        .expect("pending tx id")
        .to_string();
    let commit = ledgerful(
        root,
        &[
            "ledger",
            "commit",
            &tx_id,
            "--summary",
            SEARCH_SUMMARY,
            "--reason",
            "qualification",
        ],
    );
    assert_ok(&commit, "ledger commit");
}

async fn spawn_server(root: &Path) -> (String, String, tokio::task::JoinHandle<()>) {
    let layout = Layout::new(Utf8Path::from_path(root).expect("utf8 repo root"));
    let token = generate_token();
    let state = Arc::new(AppState::new(
        layout,
        token.clone(),
        None::<Utf8PathBuf>,
        None,
        None,
    ));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let app = router(state);
    let serve = axum::serve(listener, make_connect_info_service(app));
    let handle = tokio::spawn(async move {
        let _ = serve.await;
    });
    (format!("http://{addr}"), token, handle)
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn authed_json(url: &str, token: &str) -> Value {
    let response = client()
        .get(url)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await
        .expect("http get");
    assert_eq!(response.status().as_u16(), 200, "expected 200");
    let body = response.text().await.expect("body");
    serde_json::from_str(&body).unwrap_or_else(|err| panic!("body is not JSON ({err}): {body}"))
}

/// Connect via raw TCP HTTP/1.1 (reqwest is built without the `stream` feature).
/// Returns (stream, status, headers, body_prefix_already_read).
async fn sse_handshake(
    base_url: &str,
    token: Option<&str>,
) -> (TokioTcpStream, u16, String, String) {
    let hostport = base_url.trim_start_matches("http://");
    let mut stream = TokioTcpStream::connect(hostport)
        .await
        .expect("connect to test server");
    let auth = match token {
        Some(t) => format!("Authorization: Bearer {t}\r\n"),
        None => String::new(),
    };
    let req = format!(
        "GET /api/events HTTP/1.1\r\nHost: {hostport}\r\nAccept: text/event-stream\r\nConnection: keep-alive\r\n{auth}\r\n"
    );
    stream
        .write_all(req.as_bytes())
        .await
        .expect("write sse request");

    let mut buf = vec![0u8; 8192];
    let mut collected = Vec::new();
    let header_end = loop {
        let n = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut buf))
            .await
            .expect("header read timed out")
            .expect("header read failed");
        assert!(n > 0, "connection closed before headers");
        collected.extend_from_slice(&buf[..n]);
        if let Some(pos) = collected.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        assert!(collected.len() < 64 * 1024, "headers too large");
    };
    let headers = String::from_utf8_lossy(&collected[..header_end]).to_string();
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    let body_prefix = String::from_utf8_lossy(&collected[header_end..]).to_string();
    (stream, status, headers, body_prefix)
}

/// Read SSE body chunks until `predicate` matches or timeout.
async fn read_sse_body_until(
    stream: &mut TokioTcpStream,
    initial: &str,
    predicate: impl Fn(&str) -> bool,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut body = initial.to_string();
    if predicate(&body) {
        return body;
    }
    let mut buf = vec![0u8; 4096];
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, stream.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                body.push_str(&String::from_utf8_lossy(&buf[..n]));
                if predicate(&body) {
                    return body;
                }
            }
            Ok(Err(_)) | Err(_) => break,
        }
    }
    body
}

fn first_daemon_event(body: &str) -> Value {
    let start = body
        .find("event: daemon")
        .or_else(|| body.find("event:daemon"))
        .expect("first daemon event");
    let data = body[start..]
        .lines()
        .find_map(|line| line.trim().strip_prefix("data:"))
        .expect("daemon data line")
        .trim();
    serde_json::from_str(data).unwrap_or_else(|err| panic!("daemon json ({err}): {data}"))
}

async fn first_sse_counts(base_url: &str, token: &str) -> (u64, u64) {
    let (mut stream, status, _headers, prefix) = sse_handshake(base_url, Some(token)).await;
    assert_eq!(status, 200, "expected 200");
    let body = read_sse_body_until(
        &mut stream,
        &prefix,
        |text| text.contains("event: daemon") || text.contains("event:daemon"),
        Duration::from_secs(3),
    )
    .await;
    let event = first_daemon_event(&body);
    let pending = event["pendingTransactions"].as_u64().expect("pending");
    let unaudited = event["unauditedDrift"].as_u64().expect("unaudited");
    (pending, unaudited)
}

fn cli_counts(root: &Path) -> Value {
    let output = ledgerful(root, &["ledger", "status", "--json"]);
    assert_ok(&output, "ledger status");
    stdout_json(&output)
}

fn mcp_status(root: &Path) -> Value {
    let mut session = spawn_session(root);
    session.initialize();
    assert_structured_success(&session.call_tool("ledger_status", json!({})))
}

fn assert_http_counts(body: &Value, pending: u64, unaudited: u64) {
    assert_eq!(body["pending_transactions"].as_u64(), Some(pending));
    assert_eq!(body["unaudited_drift"].as_u64(), Some(unaudited));
}

async fn assert_refused(response: reqwest::Response, token: &str) {
    assert_eq!(response.status().as_u16(), 403, "expected 403");
    let body = response.text().await.expect("body");
    assert!(!body.contains(token), "response leaked the session token");
}

#[tokio::test]
async fn cross_surface__fresh_repo__counts_are_zero() {
    let (_tmp, root) = repo();
    init_repo(&root);
    let cli = cli_counts(&root);
    assert_eq!(cli["pendingCount"], 0);
    assert_eq!(cli["unauditedCount"], 0);

    let (url, token, handle) = spawn_server(&root).await;
    let status = authed_json(&format!("{url}/api/status"), &token).await;
    let snapshot = authed_json(&format!("{url}/api/snapshot"), &token).await;
    assert_http_counts(&status, 0, 0);
    assert_http_counts(&snapshot, 0, 0);
    let (pending, unaudited) = first_sse_counts(&url, &token).await;
    assert_eq!((pending, unaudited), (0, 0));
    handle.abort();

    let mcp = mcp_status(&root);
    assert_eq!(mcp["pending"], 0);
    assert_eq!(mcp["unaudited_drift"], 0);
}

#[tokio::test]
async fn cross_surface__seeded_drift_and_pending__counts_agree() {
    let (_tmp, root) = repo();
    seed_drift_and_pending(&root);

    let cli = cli_counts(&root);
    assert_eq!(cli["pendingCount"], 1);
    assert_eq!(cli["unauditedCount"], 1);
    assert_eq!(cli["unauditedFileCount"], 2);

    let (url, token, handle) = spawn_server(&root).await;
    let status = authed_json(&format!("{url}/api/status"), &token).await;
    let snapshot = authed_json(&format!("{url}/api/snapshot"), &token).await;
    assert_http_counts(&status, 1, 1);
    assert_http_counts(&snapshot, 1, 1);
    let (pending, unaudited) = first_sse_counts(&url, &token).await;
    assert_eq!((pending, unaudited), (1, 1));
    handle.abort();

    let mcp = mcp_status(&root);
    assert_eq!(mcp["pending"], 1);
    assert_eq!(mcp["unaudited_drift"], 1);
    assert_eq!(mcp["unaudited_file_count"], 2);
}

#[tokio::test]
async fn cross_surface__ledger_search__cli_is_bare_array() {
    let (_tmp, root) = repo();
    commit_search_hit(&root);

    let cli = ledgerful(&root, &["ledger", "search", "--json", SEARCH_SUMMARY]);
    assert_ok(&cli, "ledger search");
    let cli_body = stdout_json(&cli);
    assert!(cli_body.is_array(), "CLI ledger search is a bare array");
    assert!(cli_body.get("schemaVersion").is_none());
    assert!(
        cli_body.to_string().contains(SEARCH_SUMMARY),
        "CLI array must contain the summary"
    );

    let (url, token, handle) = spawn_server(&root).await;
    let http = authed_json(
        &format!(
            "{url}/api/ledger/search?q={}",
            urlencoding_query(SEARCH_SUMMARY)
        ),
        &token,
    )
    .await;
    assert!(http.is_array(), "HTTP ledger search is an array");
    assert!(
        http.to_string().contains(SEARCH_SUMMARY),
        "HTTP array must contain the summary"
    );
    handle.abort();

    let mut session = spawn_session(&root);
    session.initialize();
    let mcp = assert_structured_success(
        &session.call_tool("ledger_search", json!({ "query": SEARCH_SUMMARY })),
    );
    assert!(mcp.is_array(), "MCP ledger_search inner value is an array");
    assert!(
        mcp.to_string().contains(SEARCH_SUMMARY),
        "MCP array must contain the summary"
    );
}

fn urlencoding_query(query: &str) -> String {
    query.replace(' ', "%20")
}

#[test]
fn cross_surface__empty_search__exit_zero_envelope() {
    let (_tmp, root) = repo();
    std::fs::write(root.join("README.md"), "nothing to find\n").expect("readme");
    git_add_and_commit(&root, "fixture: empty search");
    init_repo(&root);
    let output = ledgerful(&root, &["search", "--json", "--", "zz_no_such_token_0560"]);
    assert_eq!(output.status.code(), Some(0), "expected exit 0");
    let body = stdout_json(&output);
    assert_eq!(body["schemaVersion"], 1);
    assert_eq!(body["resultCount"], 0);
    assert_eq!(body["results"], json!([]));
}

/// The same four refusals already live in `tests/integration/web_security.rs`
/// (`test_host_validation`, the missing-`Authorization` case,
/// `session_exchange_rejects_non_loopback_origin`, and the body that must not
/// echo the token). This test locks them on the contract file. It does not
/// replace those tests.
#[tokio::test]
async fn cross_surface__missing_bearer_foreign_host_and_origin__403() {
    let (_tmp, root) = repo();
    init_repo(&root);
    let (url, token, handle) = spawn_server(&root).await;
    assert_eq!(WRONG_BEARER.len(), 32);

    let missing = client()
        .get(format!("{url}/api/status"))
        .send()
        .await
        .expect("missing bearer");
    assert_refused(missing, &token).await;

    let foreign = client()
        .get(format!("{url}/api/status"))
        .header(AUTHORIZATION, format!("Bearer {WRONG_BEARER}"))
        .send()
        .await
        .expect("foreign bearer");
    assert_refused(foreign, &token).await;

    let host = client()
        .get(format!("{url}/health"))
        .header(HOST, "evil.example")
        .send()
        .await
        .expect("foreign host");
    assert_refused(host, &token).await;

    let origin = client()
        .post(format!("{url}/api/session/exchange"))
        .header(CONTENT_TYPE, "application/json")
        .header(ORIGIN, "https://evil.example")
        .body(r#"{"code":"00000000000000000000000000000000"}"#)
        .send()
        .await
        .expect("foreign origin");
    assert_refused(origin, &token).await;

    handle.abort();
}
