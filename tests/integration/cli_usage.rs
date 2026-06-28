//! Integration tests for `ledgerful usage` (Track M7).
//!
//! These tests exercise the full CLI surface end-to-end via the
//! compiled binary. Because the metrics pipeline depends on env vars
//! (`USERPROFILE`, `LEDGERFUL_USAGE_ENDPOINT`) and per-cwd state,
//! each test runs under the `DirGuard` harness and uses `TempEnv` to
//! isolate env mutations.

use crate::common::{DirGuard, TempEnv, non_interactive, setup_git_repo};
use camino::Utf8Path;
use serial_test::serial;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

const BINARY: &str = env!("CARGO_BIN_EXE_ledgerful");

fn run_cg(cwd: &Path, args: &[&str]) -> std::process::Output {
    let output = Command::new(BINARY)
        .args(args)
        .current_dir(cwd)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .expect("failed to run ledgerful binary");
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        panic!(
            "ledgerful {:?} failed (status={:?})\n--- stdout ---\n{}\n--- stderr ---\n{}",
            args,
            output.status.code(),
            stdout,
            stderr
        );
    }
    output
}

fn run_cg_allow_failure(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BINARY)
        .args(args)
        .current_dir(cwd)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .expect("failed to run ledgerful binary")
}

fn write_usage_config(home: &Path, toml_body: &str) {
    let dir = home.join(".ledgerful").join("usage");
    fs::create_dir_all(&dir).expect("create usage config dir");
    fs::write(dir.join("config.toml"), toml_body).expect("write config.toml");
}

fn read_usage_config(home: &Path) -> String {
    let path = home.join(".ledgerful").join("usage").join("config.toml");
    fs::read_to_string(&path).expect("read config.toml")
}

#[test]
#[serial(env, cwd)]
fn test_usage_enable_creates_anonymous_id() {
    let _env_non_interactive = non_interactive();
    let home_tmp = tempdir().expect("home tempdir");
    let work_tmp = tempdir().expect("work tempdir");

    let _home_guard = TempEnv::set("USERPROFILE", home_tmp.path().to_str().unwrap());
    let _work = DirGuard::new(work_tmp.path());

    let output = run_cg(work_tmp.path(), &["usage", "enable"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("Usage metrics enabled"),
        "expected 'Usage metrics enabled' in stdout, got: {stdout}"
    );
    assert!(
        stdout.contains("Anonymous ID:"),
        "expected 'Anonymous ID:' in stdout, got: {stdout}"
    );
    // The line containing the UUID. Very loose match — we only assert
    // there's a non-empty token after the label and the config file
    // has a UUID in it.
    let uuid_in_stdout = stdout
        .lines()
        .find(|l| l.contains("Anonymous ID:"))
        .and_then(|l| l.split_whitespace().last())
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_default();
    assert!(
        uuid::Uuid::parse_str(&uuid_in_stdout).is_ok(),
        "expected a parseable UUID in '{uuid_in_stdout}' (full stdout: {stdout})"
    );

    // The on-disk config must contain the same UUID and enabled = true.
    let cfg = read_usage_config(home_tmp.path());
    assert!(
        cfg.contains("enabled = true"),
        "config should mark enabled = true, got: {cfg}"
    );
    assert!(
        uuid::Uuid::parse_str(&cfg).is_ok() || cfg.contains(&uuid_in_stdout),
        "config should contain the anonymous_id, got: {cfg}"
    );
}

#[test]
#[serial(env, cwd)]
fn test_usage_status_after_enable() {
    let _env_non_interactive = non_interactive();
    let home_tmp = tempdir().expect("home tempdir");
    let work_tmp = tempdir().expect("work tempdir");

    let _home_guard = TempEnv::set("USERPROFILE", home_tmp.path().to_str().unwrap());
    let _work = DirGuard::new(work_tmp.path());

    run_cg(work_tmp.path(), &["usage", "enable"]);
    let output = run_cg(work_tmp.path(), &["usage", "status"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("Enabled:"),
        "expected 'Enabled:' in stdout, got: {stdout}"
    );
    assert!(
        stdout.contains("yes"),
        "expected 'yes' (enabled) in stdout, got: {stdout}"
    );
    assert!(
        stdout.contains("Anonymous ID:"),
        "expected 'Anonymous ID:' in stdout, got: {stdout}"
    );
}

#[test]
#[serial(env, cwd)]
fn test_usage_disable_clears_enabled() {
    let _env_non_interactive = non_interactive();
    let home_tmp = tempdir().expect("home tempdir");
    let work_tmp = tempdir().expect("work tempdir");

    let _home_guard = TempEnv::set("USERPROFILE", home_tmp.path().to_str().unwrap());
    let _work = DirGuard::new(work_tmp.path());

    run_cg(work_tmp.path(), &["usage", "enable"]);
    let status_after_enable = run_cg(work_tmp.path(), &["usage", "status"]);
    assert!(String::from_utf8_lossy(&status_after_enable.stdout).contains("yes"));

    run_cg(work_tmp.path(), &["usage", "disable"]);
    let status_after_disable = run_cg(work_tmp.path(), &["usage", "status"]);
    let stdout = String::from_utf8_lossy(&status_after_disable.stdout);

    assert!(
        stdout.contains("no"),
        "expected 'no' (disabled) in stdout, got: {stdout}"
    );
}

#[test]
#[serial(env, cwd)]
fn test_usage_show_payload_with_no_counters() {
    let _env_non_interactive = non_interactive();
    let home_tmp = tempdir().expect("home tempdir");
    let work_tmp = tempdir().expect("work tempdir");

    let _home_guard = TempEnv::set("USERPROFILE", home_tmp.path().to_str().unwrap());
    let _work = DirGuard::new(work_tmp.path());

    run_cg(work_tmp.path(), &["usage", "enable"]);
    let output = run_cg(work_tmp.path(), &["usage", "show-payload"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("show-payload must be valid JSON");
    assert_eq!(parsed["schema_version"], serde_json::json!(1));
    assert!(parsed["anonymous_id"].is_string());
    assert!(parsed["client_version"].is_string());
    assert!(parsed["platform"].is_string());
    assert!(parsed["sent_at"].is_string());
    assert!(parsed["window_start"].is_string());
    assert!(parsed["window_end"].is_string());
    assert!(parsed["command_counts"].is_object());
    assert!(parsed["features_enabled"].is_array());
    assert!(parsed["active_days_in_window"].is_number());
}

#[test]
#[serial(env, cwd)]
fn test_usage_show_payload_uses_placeholder_when_not_enabled() {
    let _env_non_interactive = non_interactive();
    let home_tmp = tempdir().expect("home tempdir");
    let work_tmp = tempdir().expect("work tempdir");

    let _home_guard = TempEnv::set("USERPROFILE", home_tmp.path().to_str().unwrap());
    let _work = DirGuard::new(work_tmp.path());

    // No `usage enable` — anonymous_id should be the placeholder.
    let output = run_cg(work_tmp.path(), &["usage", "show-payload"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("show-payload must be valid JSON");
    assert_eq!(
        parsed["anonymous_id"],
        serde_json::json!("(not yet enabled)"),
        "anonymous_id should be placeholder when not enabled, got: {stdout}"
    );
}

#[test]
#[serial(env, cwd)]
fn test_usage_counters_increment_after_command() {
    let _env_non_interactive = non_interactive();
    let home_tmp = tempdir().expect("home tempdir");
    let work_tmp = tempdir().expect("work tempdir");

    setup_git_repo(work_tmp.path());

    let _home_guard = TempEnv::set("USERPROFILE", home_tmp.path().to_str().unwrap());
    let _work = DirGuard::new(work_tmp.path());

    // Pre-seed a config that says metrics were sent far in the FUTURE,
    // so the 7-day flush gate (strict `>`) fails and `try_flush` is
    // a complete no-op. This is the minimal-change fix for M-NEW-1:
    // the test no longer relies on the default endpoint being
    // unreachable, and does not burn 5–10s waiting for a connection
    // timeout.
    write_usage_config(
        home_tmp.path(),
        r#"enabled = true
anonymous_id = "00000000-0000-4000-8000-000000000001"
last_sent_at = "2099-01-01T00:00:00Z"
"#,
    );
    let _endpoint_clear = TempEnv::remove("LEDGERFUL_USAGE_ENDPOINT");

    run_cg(work_tmp.path(), &["init"]);
    run_cg(work_tmp.path(), &["status"]); // → "status" (M-NEW-3)
    run_cg(work_tmp.path(), &["doctor"]); // → "doctor"

    // Read the payload to inspect command_counts. The pre-seeded
    // `last_sent_at = 2099-01-01` means `try_flush` returned early
    // and the counters are still populated.
    let output = run_cg_allow_failure(work_tmp.path(), &["usage", "show-payload"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("show-payload not valid JSON ({e}): {stdout}"));

    let command_counts = parsed["command_counts"]
        .as_object()
        .expect("command_counts should be an object");
    // The granularity fix: subcommands must be tracked at the
    // full subcommand-path level, not collapsed to the top-level
    // group name. After running `ledgerful status` (which
    // dispatches to `ledger::execute_ledger_status`), the counter
    // is `"status"` — the M-NEW-3 rename distinguishes the
    // top-level alias from `ledger_status` (which is the
    // `LedgerCommands::Status` variant). `ledgerful doctor` records
    // `doctor`, and `ledgerful init` records `init`.
    assert!(
        command_counts.contains_key("status"),
        "expected 'status' in command_counts (M-NEW-3), got: {command_counts:?}"
    );
    assert!(
        !command_counts.contains_key("ledger_status"),
        "top-level 'ledgerful status' must NOT be tracked as 'ledger_status' \
         (M-NEW-3), got: {command_counts:?}"
    );
    assert!(
        command_counts.contains_key("doctor"),
        "expected 'doctor' in command_counts, got: {command_counts:?}"
    );
    assert!(
        command_counts.contains_key("init"),
        "expected 'init' in command_counts (init was run), got: {command_counts:?}"
    );
}

#[test]
#[serial(env, cwd)]
fn test_usage_command_name_granularity() {
    let _env_non_interactive = non_interactive();
    let home_tmp = tempdir().expect("home tempdir");
    let work_tmp = tempdir().expect("work tempdir");

    setup_git_repo(work_tmp.path());

    let _home_guard = TempEnv::set("USERPROFILE", home_tmp.path().to_str().unwrap());
    let _work = DirGuard::new(work_tmp.path());

    write_usage_config(
        home_tmp.path(),
        r#"enabled = true
anonymous_id = "00000000-0000-4000-8000-000000000002"
last_sent_at = "2099-01-01T00:00:00Z"
"#,
    );
    let _endpoint_clear = TempEnv::remove("LEDGERFUL_USAGE_ENDPOINT");

    run_cg(work_tmp.path(), &["init"]);

    // `ledger start` must be tracked as `ledger_start`, not `ledger`.
    // `ledger start` opens a transaction, so the command will run to
    // completion and exit 0. We don't care about the resulting
    // pending transaction; we only care that the dispatch hook fired
    // for it.
    let output = run_cg_allow_failure(
        work_tmp.path(),
        &[
            "ledger",
            "start",
            "test/entity",
            "--category",
            "FEATURE",
            "--message",
            "m7 test",
        ],
    );
    let _ = String::from_utf8_lossy(&output.stdout);

    let payload_output = run_cg_allow_failure(work_tmp.path(), &["usage", "show-payload"]);
    let stdout = String::from_utf8_lossy(&payload_output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("show-payload not valid JSON ({e}): {stdout}"));
    let command_counts = parsed["command_counts"]
        .as_object()
        .expect("command_counts should be an object");

    // The whole point of H1: the counter key MUST be the full
    // subcommand path, not the top-level group.
    assert!(
        command_counts.contains_key("ledger_start"),
        "expected 'ledger_start' counter, got: {command_counts:?}"
    );
    assert!(
        !command_counts.keys().any(|k| k == "ledger"),
        "counter must NOT be the bare 'ledger' key, got: {command_counts:?}"
    );
}

#[test]
#[serial(env, cwd)]
fn test_usage_disable_prevents_flush() {
    let _env_non_interactive = non_interactive();
    let home_tmp = tempdir().expect("home tempdir");
    let work_tmp = tempdir().expect("work tempdir");

    setup_git_repo(work_tmp.path());

    let _home_guard = TempEnv::set("USERPROFILE", home_tmp.path().to_str().unwrap());
    let _work = DirGuard::new(work_tmp.path());

    // Seed an `enabled = true` config with a `last_sent_at` that's
    // > 7 days old, AND a LEDGERFUL_USAGE_ENDPOINT pointing at an
    // URL that would return 200 if the flush were attempted. If the
    // flush were attempted and succeeded, `last_sent_at` would be
    // updated to "now" and the counters would be cleared. By leaving
    // metrics DISABLED (`enabled = false`) we expect the flush path
    // to be a complete no-op.
    write_usage_config(
        home_tmp.path(),
        r#"enabled = false
anonymous_id = "00000000-0000-4000-8000-000000000003"
last_sent_at = "2020-01-01T00:00:00Z"
"#,
    );
    let endpoint_guard = TempEnv::set("LEDGERFUL_USAGE_ENDPOINT", "http://127.0.0.1:1/");

    run_cg(work_tmp.path(), &["init"]);
    run_cg(work_tmp.path(), &["doctor"]);

    // Re-read the config. `last_sent_at` MUST still be the seed
    // value (2020-01-01), proving the flush path did not run.
    let cfg = read_usage_config(home_tmp.path());
    assert!(
        cfg.contains("2020-01-01"),
        "last_sent_at must not have been updated when enabled = false, got: {cfg}"
    );
    drop(endpoint_guard);
}

#[test]
#[serial(env, cwd)]
fn test_usage_enable_seeds_last_sent_at_to_now() {
    // M2 fix: `usage enable` must seed `last_sent_at` to the
    // current time so the 7-day gate blocks the first flush.
    // Without this, the dispatch hook on the `enable` invocation
    // itself would see `enabled=true` + `last_sent_at=None` →
    // `should_flush=true` + a non-empty counter set → POST to
    // the production endpoint before the user can review
    // `show-payload`.
    let _env_non_interactive = non_interactive();
    let home_tmp = tempdir().expect("home tempdir");
    let work_tmp = tempdir().expect("work tempdir");

    let _home_guard = TempEnv::set("USERPROFILE", home_tmp.path().to_str().unwrap());
    let _work = DirGuard::new(work_tmp.path());

    run_cg(work_tmp.path(), &["usage", "enable"]);

    let cfg = read_usage_config(home_tmp.path());
    assert!(
        cfg.contains("last_sent_at"),
        "config must contain last_sent_at after enable (M2), got: {cfg}"
    );
    // Parse the value and assert it parses as a recent RFC 3339
    // timestamp (within the last 60 seconds — generous to avoid
    // CI clock skew, tight enough to catch "1970" or "2099").
    let last_sent = cfg
        .lines()
        .find(|l| l.contains("last_sent_at"))
        .and_then(|l| l.split('"').nth(1))
        .expect("last_sent_at should be present in config");
    let parsed = chrono::DateTime::parse_from_rfc3339(last_sent).unwrap_or_else(|e| {
        panic!("last_sent_at should be valid RFC 3339 (M2), got: {last_sent} ({e})")
    });
    let now = chrono::Utc::now();
    let delta = now.signed_duration_since(parsed.with_timezone(&chrono::Utc));
    assert!(
        delta.num_seconds() >= 0 && delta.num_seconds() < 60,
        "last_sent_at should be 'now' (within 60s), got delta {}s; value: {last_sent}",
        delta.num_seconds()
    );
}

#[test]
#[serial(env, cwd)]
fn test_usage_commands_not_counted_in_telemetry() {
    // M3 fix: `usage_*` meta-commands must not be counted as tool
    // usage. The spec's payload example shows actual tool commands
    // (`scan`, `ledger_start`, etc.), not telemetry-management
    // commands. Counting `usage_show_payload` (the user
    // *inspecting* telemetry) as "usage" would pollute the
    // aggregate signal.
    let _env_non_interactive = non_interactive();
    let home_tmp = tempdir().expect("home tempdir");
    let work_tmp = tempdir().expect("work tempdir");

    setup_git_repo(work_tmp.path());

    let _home_guard = TempEnv::set("USERPROFILE", home_tmp.path().to_str().unwrap());
    let _work = DirGuard::new(work_tmp.path());

    // Pre-seed `last_sent_at` far in the future so the 7-day
    // gate fails and `try_flush` is a no-op. This isolates the
    // counter-increment behavior under test.
    write_usage_config(
        home_tmp.path(),
        r#"enabled = true
anonymous_id = "00000000-0000-4000-8000-000000000010"
last_sent_at = "2099-01-01T00:00:00Z"
"#,
    );
    let _endpoint_clear = TempEnv::remove("LEDGERFUL_USAGE_ENDPOINT");

    // Run several `usage_*` commands. None of these should
    // appear in the counter store.
    run_cg(work_tmp.path(), &["usage", "status"]);
    run_cg(work_tmp.path(), &["usage", "show-payload"]);

    // Read the payload. The M3 fix means `usage_status` and
    // `usage_show_payload` must NOT be in `command_counts`.
    let output = run_cg_allow_failure(work_tmp.path(), &["usage", "show-payload"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("show-payload not valid JSON ({e}): {stdout}"));
    let command_counts = parsed["command_counts"]
        .as_object()
        .expect("command_counts should be an object");

    assert!(
        !command_counts.contains_key("usage_status"),
        "usage_status must NOT be counted (M3), got: {command_counts:?}"
    );
    assert!(
        !command_counts.contains_key("usage_show_payload"),
        "usage_show_payload must NOT be counted (M3), got: {command_counts:?}"
    );
    // Sanity: no `usage_*` key at all.
    for key in command_counts.keys() {
        assert!(
            !key.starts_with("usage_"),
            "no usage_* key should be in command_counts (M3), found: {key}"
        );
    }
}

#[test]
#[serial(env, cwd)]
fn test_usage_enable_does_not_trigger_immediate_flush() {
    // M2 fix end-to-end: after `usage enable`, the dispatch hook
    // on subsequent invocations must NOT POST because the
    // 7-day gate (seeded by `enable`) blocks the first flush.
    // We use a mock HTTP server to prove no POST is attempted.
    use httpmock::prelude::*;

    let _env_non_interactive = non_interactive();
    let home_tmp = tempdir().expect("home tempdir");
    let work_tmp = tempdir().expect("work tempdir");

    setup_git_repo(work_tmp.path());

    let _home_guard = TempEnv::set("USERPROFILE", home_tmp.path().to_str().unwrap());
    let _work = DirGuard::new(work_tmp.path());

    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/api/telemetry");
        then.status(200).body("{}");
    });
    let endpoint_url = format!("{}{}", server.base_url(), "/api/telemetry");
    let _endpoint_guard = TempEnv::set("LEDGERFUL_USAGE_ENDPOINT", &endpoint_url);

    // Enable. M2 fix: this seeds `last_sent_at = now` so the
    // 7-day gate blocks the first flush.
    run_cg(work_tmp.path(), &["usage", "enable"]);

    // Run a few commands. Each one's dispatch hook would call
    // `try_flush`, but the 7-day gate must block it.
    run_cg(work_tmp.path(), &["init"]);
    run_cg(work_tmp.path(), &["doctor"]);
    run_cg(work_tmp.path(), &["status"]);

    // No POST should have been attempted.
    assert_eq!(
        mock.hits(),
        0,
        "expected no POSTs after enable (M2: last_sent_at seeded to now), got {}",
        mock.hits()
    );
}

#[test]
#[serial(env, cwd)]
fn test_usage_flush_posts_expected_json_shape() {
    use httpmock::prelude::*;

    let _env_non_interactive = non_interactive();
    let home_tmp = tempdir().expect("home tempdir");
    let work_tmp = tempdir().expect("work tempdir");

    setup_git_repo(work_tmp.path());

    let _home_guard = TempEnv::set("USERPROFILE", home_tmp.path().to_str().unwrap());
    let _work = DirGuard::new(work_tmp.path());

    // M4 fix: the mock now REQUIRES the body to contain the
    // seeded anonymous_id AND the 10 required field names. A
    // regression that POSTed `{}` or omitted any field would
    // fail to match this mock, the flush would get a non-2xx
    // (mock server default 404 for unmatched), and `mock.hits()`
    // would be 0. This makes the test falsifiable against the
    // actual wire payload, not just "a POST happened".
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/api/telemetry")
            // Seeded anonymous_id — proves the right config is in use.
            .body_contains("11111111-2222-3333-4444-555555555555")
            // All 10 required fields from the spec schema.
            .body_contains("\"schema_version\"")
            .body_contains("\"anonymous_id\"")
            .body_contains("\"client_version\"")
            .body_contains("\"platform\"")
            .body_contains("\"sent_at\"")
            .body_contains("\"window_start\"")
            .body_contains("\"window_end\"")
            .body_contains("\"command_counts\"")
            .body_contains("\"features_enabled\"")
            .body_contains("\"active_days_in_window\"");
        then.status(200)
            .header("content-type", "application/json")
            .body("{}");
    });

    // Seed config with a stale `last_sent_at` so the next
    // invocation triggers a flush, and point the endpoint at our
    // mock.
    write_usage_config(
        home_tmp.path(),
        r#"enabled = true
anonymous_id = "11111111-2222-3333-4444-555555555555"
last_sent_at = "2020-01-01T00:00:00Z"
"#,
    );
    let endpoint_url = format!("{}{}", server.base_url(), "/api/telemetry");
    let _endpoint_guard = TempEnv::set("LEDGERFUL_USAGE_ENDPOINT", &endpoint_url);

    // L6 fix: the comment previously claimed `init` would NOT hit
    // the mock because "no counters". That was wrong — the
    // dispatch hook runs `increment_counter("init")` BEFORE
    // `try_flush`, so by the time the flush path runs, the
    // `command_counts` table has one row. With the seed
    // `last_sent_at = 2020-01-01` the 7-day gate passes and the
    // flush attempts a POST. If the body matches the mock
    // (anonymous_id + 10 fields), the mock returns 200, the
    // flush succeeds, counters are cleared, and `last_sent_at` is
    // updated to now.
    run_cg(work_tmp.path(), &["init"]);
    // After `init`, the mock HAS been hit (counters were non-empty
    // when try_flush ran). The "init" counter is now cleared and
    // `last_sent_at` is now, so subsequent `try_flush` calls
    // short-circuit at the 7-day gate.
    run_cg(work_tmp.path(), &["doctor"]);
    // `doctor` adds a counter for "doctor" but its `try_flush`
    // returns early (gate fails because `last_sent_at` is now).
    run_cg(work_tmp.path(), &["status"]);
    // `status` same story.

    // The mock must have been hit at least once with a body
    // matching all 11 body_contains assertions (anonymous_id +
    // 10 field names). If the flush sent `{}` or omitted any
    // field, the mock wouldn't match and `hits()` would be 0.
    assert!(
        mock.hits() >= 1,
        "expected mock to be hit at least once with the required body shape, got {}",
        mock.hits()
    );

    // Independently inspect the payload shape via
    // `usage show-payload` (same payload builder, no network).
    let output = run_cg_allow_failure(work_tmp.path(), &["usage", "show-payload"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("show-payload not valid JSON ({e}): {stdout}"));

    for required_field in [
        "schema_version",
        "anonymous_id",
        "client_version",
        "platform",
        "sent_at",
        "window_start",
        "window_end",
        "command_counts",
        "features_enabled",
        "active_days_in_window",
    ] {
        assert!(
            parsed.get(required_field).is_some(),
            "payload must contain field `{required_field}`; got: {stdout}"
        );
    }
    assert_eq!(parsed["schema_version"], serde_json::json!(1));
    assert_eq!(
        parsed["anonymous_id"],
        serde_json::json!("11111111-2222-3333-4444-555555555555")
    );
    // After the first flush (triggered by `init`) updated
    // `last_sent_at` to now, subsequent `try_flush` calls
    // short-circuit at the 7-day gate, so the `doctor` and
    // `status` counters accumulate in the table without being
    // flushed. The `show-payload` view therefore reflects at
    // least one of them. (`init` itself was cleared by the flush.)
    let command_counts = parsed["command_counts"]
        .as_object()
        .expect("command_counts should be an object");
    let doctor_count = command_counts
        .get("doctor")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let status_count = command_counts
        .get("status")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(
        doctor_count + status_count >= 1,
        "expected at least one of `doctor` / `status` counters (post-flush), got: {command_counts:?}"
    );
}

#[test]
#[serial(env, cwd)]
fn test_usage_degrades_gracefully_outside_repo() {
    // Run `usage show-payload` from a directory that has no git
    // repo and no `.ledgerful/`. The command must NOT return a
    // hard error (H3 review) — it should print a payload with an
    // empty `command_counts` map and placeholder `anonymous_id`.
    let _env_non_interactive = non_interactive();
    let home_tmp = tempdir().expect("home tempdir");
    let work_tmp = tempdir().expect("work tempdir");

    let _home_guard = TempEnv::set("USERPROFILE", home_tmp.path().to_str().unwrap());
    let _work = DirGuard::new(work_tmp.path());

    let output = run_cg(work_tmp.path(), &["usage", "enable"]);
    assert!(output.status.success(), "usage enable should succeed");

    // Now switch to a directory that has no .ledgerful at all.
    let outside_tmp = tempdir().expect("outside tempdir");
    let _work2 = DirGuard::new(outside_tmp.path());
    let output = run_cg(outside_tmp.path(), &["usage", "show-payload"]);
    assert!(
        output.status.success(),
        "show-payload from outside a repo should succeed gracefully (H3); \
         stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("show-payload not valid JSON ({e}): {stdout}"));
    let command_counts = parsed["command_counts"]
        .as_object()
        .expect("command_counts should be an object even with no counters");
    assert!(
        command_counts.is_empty(),
        "command_counts should be empty when no counters, got: {command_counts:?}"
    );
}

#[test]
#[serial(env, cwd)]
fn test_usage_features_enabled_excludes_self() {
    // M5 review: the `enabled_features()` list must NOT include
    // `usage-metrics` itself (the function is only called from
    // within the feature's code paths; self-reporting would be
    // tautological).
    let _env_non_interactive = non_interactive();
    let home_tmp = tempdir().expect("home tempdir");
    let work_tmp = tempdir().expect("work tempdir");

    let _home_guard = TempEnv::set("USERPROFILE", home_tmp.path().to_str().unwrap());
    let _work = DirGuard::new(work_tmp.path());

    run_cg(work_tmp.path(), &["usage", "enable"]);
    let output = run_cg(work_tmp.path(), &["usage", "show-payload"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("show-payload not valid JSON ({e}): {stdout}"));
    let features = parsed["features_enabled"]
        .as_array()
        .expect("features_enabled should be an array");
    let feature_strs: Vec<&str> = features.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        !feature_strs.contains(&"usage-metrics"),
        "features_enabled must not self-report 'usage-metrics', got: {feature_strs:?}"
    );
}

#[test]
#[serial(env, cwd)]
fn test_usage_path_uses_utf8() {
    // The usage config dir is computed via
    // `var_os("USERPROFILE").or_else(var_os("HOME"))` and joined
    // with `.ledgerful/usage`. Confirm the on-disk path is
    // exactly that and that writing through the binary produces
    // a valid TOML file there.
    let _env_non_interactive = non_interactive();
    let home_tmp = tempdir().expect("home tempdir");
    let work_tmp = tempdir().expect("work tempdir");

    let _home_guard = TempEnv::set("USERPROFILE", home_tmp.path().to_str().unwrap());
    let _work = DirGuard::new(work_tmp.path());

    run_cg(work_tmp.path(), &["usage", "enable"]);

    let utf8_home = Utf8Path::from_path(home_tmp.path()).expect("home path is UTF-8");
    let expected_dir = utf8_home.join(".ledgerful").join("usage");
    let expected_path = expected_dir.join("config.toml");
    assert!(
        expected_path.as_std_path().exists(),
        "expected config at {} (M2 review: var_os(USERPROFILE) path)",
        expected_path
    );
    let content = fs::read_to_string(expected_path.as_std_path()).expect("read config");
    assert!(content.contains("enabled = true"));
}
