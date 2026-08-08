//! 0161 DoD-6 / B7: hermetic CLI smoke for semantic index progress, modes, JSON purity.
//!
//! Coverage:
//! 1. Non-TTY progress lines (mode / candidates / complete) under LEDGERFUL_NON_INTERACTIVE
//! 2. Warm second run → up-to-date incremental (0 files changed)
//! 3. Content delta → only changed file processed
//! 4. `--json` whole-stdout purity (schemaVersion, mode, reason, files*, upToDate)
//!
//! Hermetic: tempfile git repo + ledgerful init + httpmock OpenAI embeddings.
//! Embedding mock batch size is swapped between phases via mock.delete().

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use httpmock::prelude::*;
use serial_test::serial;

use crate::common::setup_git_repo;

fn ledgerful_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ledgerful")
}

fn combined(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn run_in(root: &Path, args: &[&str]) -> Output {
    Command::new(ledgerful_bin())
        .args(args)
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn ledgerful {:?}: {e}", args))
}

fn write_source_files(root: &Path) {
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    // One function each → one AST chunk each (matches fixed mock batch sizes).
    fs::write(
        src.join("alpha.rs"),
        "pub fn alpha_helper() { let _x = 1; }\n",
    )
    .unwrap();
    fs::write(
        src.join("beta.rs"),
        "pub fn beta_helper() { let _y = 2; }\n",
    )
    .unwrap();
}

fn hermetic_repo(root: &Path) {
    setup_git_repo(root);
    write_source_files(root);
    let init = run_in(root, &["init"]);
    assert!(
        init.status.success(),
        "ledgerful init failed: {}",
        combined(&init)
    );
}

fn write_embed_config(root: &Path, base_url: &str) {
    let config_path = root.join(".ledgerful").join("config.toml");
    let existing = fs::read_to_string(&config_path).unwrap_or_default();
    let mut out = String::new();
    let mut in_local = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_local = trimmed == "[local_model]";
            if in_local {
                continue;
            }
        }
        if in_local {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !out.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&format!(
        r#"[local_model]
base_url = "{base_url}"
embedding_model = "test-embed-0161"
dimensions = 3
timeout_secs = 10
disable_hnsw = true
"#
    ));
    fs::write(config_path, out).unwrap();
}

/// OpenAI-style embedding response with exactly `n` vectors of dim 3.
fn mock_embed_batch(server: &MockServer, n: usize) -> httpmock::Mock<'_> {
    let data: Vec<serde_json::Value> = (0..n)
        .map(|i| {
            serde_json::json!({
                "embedding": [1.0, (i as f64) * 0.01, 0.0]
            })
        })
        .collect();
    server.mock(|when, then| {
        when.method(POST).path("/v1/embeddings");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({ "data": data }));
    })
}

/// B7-1: non-TTY progress smoke (cold full run over tiny fixture).
#[test]
#[serial(test)]
fn test_index_semantic_non_tty_progress_smoke() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    hermetic_repo(root);

    let server = MockServer::start();
    let _mock = mock_embed_batch(&server, 2);
    write_embed_config(root, &server.base_url());

    let output = run_in(root, &["index", "--semantic"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let text = combined(&output);

    assert!(
        output.status.success(),
        "index --semantic must exit 0 on cold path:\n{text}"
    );

    assert!(
        stdout.contains("Semantic indexing: mode="),
        "expected mode progress line, got stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("mode=full") && stdout.contains("reason=cold-store"),
        "cold store should report mode=full reason=cold-store:\n{stdout}"
    );

    let has_mid = stdout.contains("candidates=")
        || stdout.contains("to_process=")
        || stdout.contains("Semantic index: parsing")
        || stdout.contains("Semantic index: embedding");
    assert!(
        has_mid,
        "expected candidates/to_process or parse/embed progress line:\n{stdout}"
    );

    assert!(
        stdout.contains("Semantic indexing complete") || stdout.contains("Semantic index complete"),
        "expected complete line:\n{stdout}"
    );
}

/// B7-2: warm second run with no content changes → up-to-date incremental.
#[test]
#[serial(test)]
fn test_index_semantic_up_to_date_warm_second_run() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    hermetic_repo(root);

    let server = MockServer::start();
    let mut mock = mock_embed_batch(&server, 2);
    write_embed_config(root, &server.base_url());

    let first = run_in(root, &["index", "--semantic"]);
    assert!(
        first.status.success(),
        "first index failed:\n{}",
        combined(&first)
    );

    // Warm path should not call embed; remove mock so any hit fails loudly.
    mock.delete();

    let second = run_in(root, &["index", "--semantic"]);
    let stdout = String::from_utf8_lossy(&second.stdout);
    let text = combined(&second);

    assert!(
        second.status.success(),
        "warm index --semantic must exit 0:\n{text}"
    );
    assert!(
        stdout.contains("mode=incremental"),
        "warm store should use incremental mode:\n{stdout}"
    );
    assert!(
        stdout.contains("up to date")
            || stdout.contains("0 files changed")
            || stdout.contains("to_process=0"),
        "expected up-to-date / 0 changed / to_process=0:\n{stdout}"
    );
}

/// B7-3: edit one source file → only the delta is reprocessed.
#[test]
#[serial(test)]
fn test_index_semantic_incremental_delta() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    hermetic_repo(root);

    let server = MockServer::start();
    let mut mock = mock_embed_batch(&server, 2);
    write_embed_config(root, &server.base_url());

    let first = run_in(root, &["index", "--semantic"]);
    assert!(
        first.status.success(),
        "first index failed:\n{}",
        combined(&first)
    );
    mock.delete();

    // Confirm warm is idle before delta.
    let warm = run_in(root, &["index", "--semantic"]);
    assert!(
        warm.status.success(),
        "warm index failed:\n{}",
        combined(&warm)
    );
    let warm_out = String::from_utf8_lossy(&warm.stdout);
    assert!(
        warm_out.contains("to_process=0") || warm_out.contains("up to date"),
        "pre-delta warm should be idle:\n{warm_out}"
    );

    // Change only alpha.rs content → hash miss → to_process=1.
    fs::write(
        root.join("src").join("alpha.rs"),
        "pub fn alpha_helper() { let _x = 42; /* delta */ }\n",
    )
    .unwrap();

    let _delta_mock = mock_embed_batch(&server, 1);
    let delta = run_in(root, &["index", "--semantic"]);
    let stdout = String::from_utf8_lossy(&delta.stdout);
    let text = combined(&delta);

    assert!(
        delta.status.success(),
        "delta index --semantic must exit 0:\n{text}"
    );
    assert!(
        stdout.contains("mode=incremental"),
        "delta path should stay incremental:\n{stdout}"
    );

    // Prefer explicit to_process=1; also accept complete 1/N incremental form.
    let delta_ok = stdout.contains("to_process=1")
        || stdout.contains("Semantic indexing complete: 1/1 files")
        || (stdout.contains("files produced embeddings (incremental)")
            && stdout.contains("complete: 1/"));
    assert!(
        delta_ok,
        "expected only one file processed on delta (to_process=1 or complete 1/N):\n{stdout}"
    );

    // candidates should still be both sources when to_process is reported.
    if stdout.contains("to_process=1") {
        assert!(
            stdout.contains("candidates=2") || stdout.contains("candidates=1"),
            // candidates=1 only if walk saw one file (unlikely); allow but prefer 2
            "unexpected candidates line with to_process=1:\n{stdout}"
        );
        // Stronger: candidates must be >= to_process and ideally 2.
        assert!(
            stdout.contains("candidates=2"),
            "expected candidates=2 to_process=1 after editing one of two files:\n{stdout}"
        );
    }
}

/// B7-4: `--json` emits a pure single JSON object (no human mode prefixes).
#[test]
#[serial(test)]
fn test_index_semantic_json_purity() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    hermetic_repo(root);

    let server = MockServer::start();
    let mut mock = mock_embed_batch(&server, 2);
    write_embed_config(root, &server.base_url());

    // Cold --json
    let cold = run_in(root, &["index", "--semantic", "--json"]);
    let cold_stdout = String::from_utf8_lossy(&cold.stdout);
    let cold_text = combined(&cold);
    assert!(
        cold.status.success(),
        "cold index --semantic --json must exit 0:\n{cold_text}"
    );

    assert!(
        !cold_stdout.contains("Semantic indexing: mode="),
        "JSON mode must not emit human mode line:\n{cold_stdout}"
    );
    assert!(
        !cold_stdout
            .lines()
            .any(|l| l.trim_start().starts_with("Semantic indexing:")),
        "JSON mode must not mix human Semantic indexing: lines:\n{cold_stdout}"
    );

    let cold_json: serde_json::Value =
        serde_json::from_str(cold_stdout.trim()).unwrap_or_else(|e| {
            panic!("cold --json stdout must be a single JSON object ({e}):\n{cold_stdout}")
        });
    assert_eq!(cold_json["schemaVersion"], 1);
    assert_eq!(cold_json["mode"], "full");
    assert_eq!(cold_json["reason"], "cold-store");
    assert_eq!(cold_json["upToDate"], false);
    assert!(
        cold_json["filesProcessed"].as_u64().unwrap_or(0) >= 1,
        "cold should process files: {cold_json}"
    );
    assert_eq!(cold_json["filesCandidates"], 2);

    mock.delete();

    // Warm --json (up-to-date)
    let warm = run_in(root, &["index", "--semantic", "--json"]);
    let warm_stdout = String::from_utf8_lossy(&warm.stdout);
    let warm_text = combined(&warm);
    assert!(
        warm.status.success(),
        "warm index --semantic --json must exit 0:\n{warm_text}"
    );
    assert!(
        !warm_stdout.contains("Semantic indexing: mode="),
        "warm JSON must not emit human mode line:\n{warm_stdout}"
    );

    let warm_json: serde_json::Value =
        serde_json::from_str(warm_stdout.trim()).unwrap_or_else(|e| {
            panic!("warm --json stdout must be a single JSON object ({e}):\n{warm_stdout}")
        });
    assert_eq!(warm_json["schemaVersion"], 1);
    assert_eq!(warm_json["mode"], "incremental");
    assert!(
        warm_json["reason"] == "auto-incremental" || warm_json["reason"] == "explicit-incremental",
        "warm reason should be auto/explicit incremental: {warm_json}"
    );
    assert_eq!(warm_json["upToDate"], true);
    assert_eq!(warm_json["filesProcessed"], 0);
    assert_eq!(warm_json["filesCandidates"], 2);
}
