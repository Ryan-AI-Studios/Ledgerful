//! Router-level bound verification evidence (0556).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use camino::Utf8Path;
use ledgerful::commands::web::auth::generate_token;
use ledgerful::commands::web::server::router;
use ledgerful::commands::web::state::AppState;
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use ledgerful::verify::results::VERIFY_HISTORY;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Fixture {
    _tmp: tempfile::TempDir,
    layout: Layout,
    tx_id: String,
}

fn fixture(tx_id: &str) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let layout = Layout::new(Utf8Path::from_path(tmp.path()).expect("utf8 path"));
    layout.ensure_state_dir().expect("state dir");
    seed_ledger(&layout, tx_id);
    Fixture {
        _tmp: tmp,
        layout,
        tx_id: tx_id.to_string(),
    }
}

fn with_db(layout: &Layout, body: impl FnOnce(&Connection)) {
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(db_path.as_std_path()).expect("init ledger");
    body(storage.get_connection());
}

fn seed_ledger(layout: &Layout, tx_id: &str) {
    with_db(layout, |conn| {
        conn.execute(
            "INSERT INTO transactions (
                tx_id, status, category, entity, entity_normalized,
                session_id, source, started_at
             ) VALUES (?1, 'COMMITTED', 'FEATURE', 'entity', 'entity', 'test', 'test', ?2)",
            params![tx_id, "2026-10-23T00:00:00Z"],
        )
        .expect("transaction");
        conn.execute(
            "INSERT INTO ledger_entries (
                tx_id, category, entry_type, entity, entity_normalized, change_type,
                summary, reason, is_breaking, committed_at, origin, author,
                verification_status, verification_basis, signature, public_key, sig_version
             ) VALUES (
                ?1, 'FEATURE', 'IMPLEMENTATION', 'entity', 'entity', 'MODIFY',
                'seed', 'proof', 0, '2026-10-23T00:00:00Z', 'LOCAL', 'test',
                NULL, NULL, 'sig-fixed', 'pk-fixed', 2
             )",
            params![tx_id],
        )
        .expect("ledger entry");
    });
}

fn insert_run(
    conn: &Connection,
    timestamp: &str,
    plan_json: Option<&str>,
    overall_pass: i64,
    tx_id: Option<&str>,
) -> i64 {
    conn.execute(
        "INSERT INTO verification_runs (timestamp, plan_json, overall_pass, tx_id)
         VALUES (?1, ?2, ?3, ?4)",
        params![timestamp, plan_json, overall_pass, tx_id],
    )
    .expect("verification run");
    conn.last_insert_rowid()
}

fn insert_result(
    conn: &Connection,
    run_id: i64,
    command: &str,
    exit_code: i64,
    tx_id: Option<&str>,
) {
    conn.execute(
        "INSERT INTO verification_results (run_id, command, exit_code, duration_ms, truncated, tx_id)
         VALUES (?1, ?2, ?3, 10, 0, ?4)",
        params![run_id, command, exit_code, tx_id],
    )
    .expect("verification result");
}

fn plan_with_steps(commands: &[&str]) -> String {
    let steps: Vec<Value> = commands
        .iter()
        .map(|command| {
            json!({
                "command": command,
                "timeoutSecs": 30,
                "description": "step"
            })
        })
        .collect();
    json!({ "steps": steps }).to_string()
}

fn write_history(layout: &Layout, body: &str) {
    let dir = layout.reports_dir();
    std::fs::create_dir_all(&dir).expect("reports dir");
    std::fs::write(dir.join(VERIFY_HISTORY), body).expect("history");
}

fn history_bytes(layout: &Layout) -> Vec<u8> {
    std::fs::read(layout.reports_dir().join(VERIFY_HISTORY)).expect("read history")
}

async fn request_json(layout: Layout, path: &str) -> (StatusCode, Value, String) {
    let token = generate_token();
    let state = Arc::new(AppState::new(layout, token.clone(), None, None, None));
    let app = router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri(path)
                .header("host", "127.0.0.1")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1_000_000)
        .await
        .expect("body");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_str(&text).unwrap_or(Value::Null);
    (status, json, text)
}

async fn detail(layout: Layout, tx_id: &str) -> (StatusCode, Value, String) {
    request_json(layout, &format!("/api/ledger/{tx_id}")).await
}

fn assert_detail_ok(status: StatusCode, body: &str) {
    assert_eq!(
        status,
        StatusCode::OK,
        "detail must be 200 before the body is read: {body}"
    );
}

type LedgerProofColumns = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
);

fn proof_columns(layout: &Layout, tx_id: &str) -> LedgerProofColumns {
    let db_path = layout.state_subdir().join("ledger.db");
    let conn = Connection::open(db_path.as_std_path()).expect("open");
    conn.query_row(
        "SELECT verification_status, verification_basis, signature, public_key, sig_version
         FROM ledger_entries WHERE tx_id = ?1",
        params![tx_id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )
    .expect("proof columns")
}

#[tokio::test]
async fn bound_git_version_without_head_keeps_signatures() {
    let fix = fixture("tx-git");
    let timestamp = chrono::Utc::now().to_rfc3339();
    with_db(&fix.layout, |conn| {
        let run = insert_run(conn, &timestamp, None, 1, Some(&fix.tx_id));
        insert_result(conn, run, "git --version", 0, Some(&fix.tx_id));
    });
    write_history(
        &fix.layout,
        &json!([{
            "timestamp": timestamp,
            "passed": true,
            "duration_secs": 0,
            "tx_id": fix.tx_id
        }])
        .to_string(),
    );
    let before = proof_columns(&fix.layout, &fix.tx_id);
    let (status, json, body) = detail(fix.layout.clone(), &fix.tx_id).await;
    assert_detail_ok(status, &body);
    assert_eq!(json["tests_run"], 1);
    assert_eq!(json["flakes"], 0);
    assert_eq!(json["verification_status"], Value::Null);
    assert_eq!(json["verification_basis"], Value::Null);
    assert_eq!(
        json["bound_runs"][0]["commands"][0]["command"],
        "git --version"
    );
    assert_eq!(json["bound_runs"][0]["overall_pass"], true);
    assert_eq!(json["bound_runs"][0]["partial"], false);
    assert_eq!(json["bound_snapshot"]["head"], Value::Null);
    assert_eq!(json["bound_snapshot"]["head_status"], "unknown");
    assert_eq!(proof_columns(&fix.layout, &fix.tx_id), before);
}

#[tokio::test]
async fn no_verification_rows_returns_empty_bound_runs() {
    let fix = fixture("tx-empty");
    let (status, json, body) = detail(fix.layout, &fix.tx_id).await;
    assert_detail_ok(status, &body);
    assert_eq!(json["bound_runs"], json!([]));
    assert_eq!(json["bound_snapshot"], Value::Null);
    assert_eq!(json["tests_run"], 0);
    assert_eq!(json["flakes"], 0);
}

#[tokio::test]
async fn failed_run_counts_one_flake() {
    let fix = fixture("tx-flake");
    let timestamp = chrono::Utc::now().to_rfc3339();
    with_db(&fix.layout, |conn| {
        let run = insert_run(conn, &timestamp, None, 0, Some(&fix.tx_id));
        insert_result(conn, run, "cargo test", 1, Some(&fix.tx_id));
        insert_result(conn, run, "cargo build", 0, Some(&fix.tx_id));
    });
    let (status, json, body) = detail(fix.layout, &fix.tx_id).await;
    assert_detail_ok(status, &body);
    assert_eq!(json["tests_run"], 2);
    assert_eq!(json["flakes"], 1);
    assert_eq!(json["bound_runs"][0]["overall_pass"], false);
}

#[tokio::test]
async fn longer_plan_is_partial_true() {
    let fix = fixture("tx-partial");
    let timestamp = chrono::Utc::now().to_rfc3339();
    let plan = plan_with_steps(&["cargo fmt", "cargo clippy"]);
    with_db(&fix.layout, |conn| {
        let run = insert_run(conn, &timestamp, Some(&plan), 1, Some(&fix.tx_id));
        insert_result(conn, run, "cargo fmt", 0, Some(&fix.tx_id));
    });
    let (status, json, body) = detail(fix.layout, &fix.tx_id).await;
    assert_detail_ok(status, &body);
    assert_eq!(json["bound_runs"][0]["partial"], true);
}

#[tokio::test]
async fn unreadable_plan_json_is_partial_null() {
    let fix = fixture("tx-bad-plan");
    let timestamp = chrono::Utc::now().to_rfc3339();
    with_db(&fix.layout, |conn| {
        let run = insert_run(conn, &timestamp, Some("{"), 1, Some(&fix.tx_id));
        insert_result(conn, run, "cargo fmt", 0, Some(&fix.tx_id));
    });
    let (status, json, body) = detail(fix.layout, &fix.tx_id).await;
    assert_detail_ok(status, &body);
    assert_eq!(json["bound_runs"][0]["partial"], Value::Null);
}

#[tokio::test]
async fn fewer_plan_steps_than_results_is_partial_null() {
    let fix = fixture("tx-fewer");
    let timestamp = chrono::Utc::now().to_rfc3339();
    let plan = plan_with_steps(&["cargo fmt"]);
    with_db(&fix.layout, |conn| {
        let run = insert_run(conn, &timestamp, Some(&plan), 1, Some(&fix.tx_id));
        insert_result(conn, run, "cargo build", 0, Some(&fix.tx_id));
        insert_result(conn, run, "cargo fmt", 0, Some(&fix.tx_id));
    });
    let (status, json, body) = detail(fix.layout, &fix.tx_id).await;
    assert_detail_ok(status, &body);
    assert_eq!(json["bound_runs"][0]["partial"], Value::Null);
}

#[tokio::test]
async fn null_plan_json_is_partial_false() {
    let fix = fixture("tx-null-plan");
    let timestamp = chrono::Utc::now().to_rfc3339();
    with_db(&fix.layout, |conn| {
        let run = insert_run(conn, &timestamp, None, 1, Some(&fix.tx_id));
        insert_result(conn, run, "git --version", 0, Some(&fix.tx_id));
    });
    let (status, json, body) = detail(fix.layout, &fix.tx_id).await;
    assert_detail_ok(status, &body);
    assert_eq!(json["bound_runs"][0]["partial"], false);
}

#[tokio::test]
async fn equal_plan_and_result_count_is_partial_false() {
    let fix = fixture("tx-equal");
    let timestamp = chrono::Utc::now().to_rfc3339();
    let plan = plan_with_steps(&["cargo fmt"]);
    with_db(&fix.layout, |conn| {
        let run = insert_run(conn, &timestamp, Some(&plan), 1, Some(&fix.tx_id));
        insert_result(conn, run, "cargo fmt", 0, Some(&fix.tx_id));
    });
    let (status, json, body) = detail(fix.layout, &fix.tx_id).await;
    assert_detail_ok(status, &body);
    assert_eq!(json["bound_runs"][0]["partial"], false);
}

#[tokio::test]
async fn older_than_seven_days_is_stale() {
    let fix = fixture("tx-stale");
    with_db(&fix.layout, |conn| {
        let run = insert_run(conn, "2020-01-01T00:00:00Z", None, 0, Some(&fix.tx_id));
        insert_result(conn, run, "cargo test", 1, Some(&fix.tx_id));
    });
    let (status, json, body) = detail(fix.layout, &fix.tx_id).await;
    assert_detail_ok(status, &body);
    assert_eq!(json["bound_runs"][0]["stale"], true);
    assert_eq!(json["bound_runs"][0]["overall_pass"], false);
}

#[tokio::test]
async fn two_runs_ordered_by_run_id() {
    let fix = fixture("tx-order");
    with_db(&fix.layout, |conn| {
        insert_run(conn, "2026-10-23T00:00:02Z", None, 1, Some(&fix.tx_id));
        insert_run(conn, "2020-01-01T00:00:00Z", None, 1, Some(&fix.tx_id));
    });
    let (status, json, body) = detail(fix.layout, &fix.tx_id).await;
    assert_detail_ok(status, &body);
    let runs = json["bound_runs"].as_array().expect("runs");
    assert_eq!(runs.len(), 2);
    assert!(runs[0]["run_id"].as_i64().expect("id") < runs[1]["run_id"].as_i64().expect("id"));
    assert_eq!(runs[0]["run_timestamp"], "2026-10-23T00:00:02Z");
    assert_eq!(runs[1]["run_timestamp"], "2020-01-01T00:00:00Z");
}

#[tokio::test]
async fn run_with_no_results_has_empty_commands() {
    let fix = fixture("tx-nocmd");
    let timestamp = chrono::Utc::now().to_rfc3339();
    with_db(&fix.layout, |conn| {
        insert_run(conn, &timestamp, None, 1, Some(&fix.tx_id));
    });
    let (status, json, body) = detail(fix.layout, &fix.tx_id).await;
    assert_detail_ok(status, &body);
    assert_eq!(json["bound_runs"][0]["commands"], json!([]));
}

#[tokio::test]
async fn matching_history_head_is_recorded() {
    let fix = fixture("tx-head");
    let timestamp = chrono::Utc::now().to_rfc3339();
    with_db(&fix.layout, |conn| {
        let run = insert_run(conn, &timestamp, None, 1, Some(&fix.tx_id));
        insert_result(conn, run, "git --version", 0, Some(&fix.tx_id));
    });
    write_history(
        &fix.layout,
        &json!([
            {
                "timestamp": timestamp,
                "passed": true,
                "duration_secs": 0,
                "tx_id": "someone-else",
                "head": "nope"
            },
            {
                "timestamp": timestamp,
                "passed": true,
                "duration_secs": 0,
                "tx_id": fix.tx_id,
                "head": "abc123"
            }
        ])
        .to_string(),
    );
    let (status, json, body) = detail(fix.layout, &fix.tx_id).await;
    assert_detail_ok(status, &body);
    assert_eq!(json["bound_snapshot"]["head"], "abc123");
    assert_eq!(json["bound_snapshot"]["head_status"], "recorded");
}

#[tokio::test]
async fn last_history_record_in_file_order_supplies_head() {
    let fix = fixture("tx-last");
    let timestamp = "2026-10-23T01:02:03Z";
    with_db(&fix.layout, |conn| {
        insert_run(conn, timestamp, None, 1, Some(&fix.tx_id));
    });
    write_history(
        &fix.layout,
        &json!([
            {
                "timestamp": timestamp,
                "passed": true,
                "duration_secs": 1,
                "tx_id": fix.tx_id,
                "head": "first-hash"
            },
            {
                "timestamp": timestamp,
                "passed": true,
                "duration_secs": 1,
                "tx_id": fix.tx_id,
                "head": "last-hash"
            }
        ])
        .to_string(),
    );
    let (status, json, body) = detail(fix.layout, &fix.tx_id).await;
    assert_detail_ok(status, &body);
    assert_eq!(json["bound_snapshot"]["head"], "last-hash");
    assert_ne!(json["bound_snapshot"]["head"], "first-hash");
    assert_eq!(json["bound_snapshot"]["head_status"], "recorded");
}

#[tokio::test]
async fn newest_run_snapshot_ignores_older_history() {
    let fix = fixture("tx-newest");
    let older = "2026-10-20T00:00:00Z";
    let newer = chrono::Utc::now().to_rfc3339();
    with_db(&fix.layout, |conn| {
        insert_run(conn, older, None, 1, Some(&fix.tx_id));
        insert_run(conn, &newer, None, 1, Some(&fix.tx_id));
    });
    write_history(
        &fix.layout,
        &json!([{
            "timestamp": older,
            "passed": true,
            "duration_secs": 1,
            "tx_id": fix.tx_id,
            "head": "older-hash"
        }])
        .to_string(),
    );
    let (status, json, body) = detail(fix.layout, &fix.tx_id).await;
    assert_detail_ok(status, &body);
    assert_eq!(json["bound_snapshot"]["run_timestamp"], newer);
    assert_eq!(json["bound_snapshot"]["head"], Value::Null);
    assert_eq!(json["bound_snapshot"]["head_status"], "unknown");
    assert_ne!(json["bound_snapshot"]["head"], "older-hash");
}

#[tokio::test]
async fn unbound_result_row_is_excluded() {
    let fix = fixture("tx-unbound");
    let timestamp = chrono::Utc::now().to_rfc3339();
    with_db(&fix.layout, |conn| {
        let other = insert_run(conn, &timestamp, None, 1, None);
        insert_result(conn, other, "cargo test", 0, Some(&fix.tx_id));
    });
    let (status, json, body) = detail(fix.layout, &fix.tx_id).await;
    assert_detail_ok(status, &body);
    assert_eq!(json["bound_runs"], json!([]));
    assert_eq!(json["bound_snapshot"], Value::Null);
    assert_eq!(json["tests_run"], 1);
}

#[tokio::test]
async fn malformed_history_bytes_stay_and_head_is_unknown() {
    let fix = fixture("tx-malformed");
    let timestamp = chrono::Utc::now().to_rfc3339();
    with_db(&fix.layout, |conn| {
        insert_run(conn, &timestamp, None, 1, Some(&fix.tx_id));
    });
    let corrupt = "{not-valid-verify-history";
    write_history(&fix.layout, corrupt);
    let (status, json, body) = detail(fix.layout.clone(), &fix.tx_id).await;
    assert_detail_ok(status, &body);
    assert_eq!(json["bound_snapshot"]["head_status"], "unknown");
    assert_eq!(json["bound_snapshot"]["head"], Value::Null);
    assert_eq!(history_bytes(&fix.layout), corrupt.as_bytes());
}

#[tokio::test]
async fn verification_table_read_error_returns_500() {
    let fix = fixture("tx-500");
    with_db(&fix.layout, |conn| {
        conn.execute("DROP TABLE verification_results", [])
            .expect("drop results");
    });
    let (status, json, body) = detail(fix.layout, &fix.tx_id).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    let looks_like_empty_detail = json["tests_run"] == 0 && json["bound_runs"] == json!([]);
    assert!(
        !looks_like_empty_detail,
        "a verification read error must not answer with zeros and an empty bound_runs: {body}"
    );
}

#[tokio::test]
async fn healthy_git_version_sets_cheap_message() {
    let fix = fixture("tx-healthy-cheap");
    let timestamp = chrono::Utc::now().to_rfc3339();
    with_db(&fix.layout, |conn| {
        let run = insert_run(conn, &timestamp, None, 1, Some("other"));
        insert_result(conn, run, "git --version", 0, Some("other"));
    });
    let (status, json, body) = request_json(fix.layout, "/api/verify/health").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(json["status"], "HEALTHY");
    assert_eq!(
        json["message"],
        "Cheap command recorded: git --version. Not a project-suite pass."
    );
}

#[tokio::test]
async fn stale_git_version_keeps_stale_sentence() {
    let fix = fixture("tx-stale-cheap");
    with_db(&fix.layout, |conn| {
        let run = insert_run(conn, "2020-01-01T00:00:00Z", None, 1, Some("other"));
        insert_result(conn, run, "git --version", 0, Some("other"));
    });
    let (status, json, body) = request_json(fix.layout, "/api/verify/health").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(json["status"], "DEGRADED");
    assert_eq!(
        json["message"],
        "Last verification run is stale (older than 7 days)"
    );
}

#[tokio::test]
async fn failing_git_version_sets_cheap_failed_message() {
    let fix = fixture("tx-fail-cheap");
    let timestamp = chrono::Utc::now().to_rfc3339();
    with_db(&fix.layout, |conn| {
        let run = insert_run(conn, &timestamp, None, 0, Some("other"));
        insert_result(conn, run, "git --version", 1, Some("other"));
    });
    let (status, json, body) = request_json(fix.layout, "/api/verify/health").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(json["status"], "FAILING");
    assert_eq!(
        json["message"],
        "Cheap command failed: git --version. Not a project-suite result."
    );
}

#[tokio::test]
async fn failing_other_command_omits_cheap_sentence() {
    let fix = fixture("tx-fail-other");
    let timestamp = chrono::Utc::now().to_rfc3339();
    with_db(&fix.layout, |conn| {
        let run = insert_run(conn, &timestamp, None, 0, Some("other"));
        insert_result(conn, run, "cargo test", 1, Some("other"));
    });
    let (status, json, body) = request_json(fix.layout, "/api/verify/health").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(json["status"], "FAILING");
    assert!(json.get("message").is_none(), "{body}");
    assert!(!body.contains("Cheap command failed"), "{body}");
}

#[tokio::test]
async fn healthy_other_command_omits_message() {
    let fix = fixture("tx-healthy-other");
    let timestamp = chrono::Utc::now().to_rfc3339();
    with_db(&fix.layout, |conn| {
        let run = insert_run(conn, &timestamp, None, 1, Some("other"));
        insert_result(conn, run, "cargo test", 0, Some("other"));
    });
    let (status, json, body) = request_json(fix.layout, "/api/verify/health").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(json["status"], "HEALTHY");
    assert!(json.get("message").is_none(), "{body}");
}
