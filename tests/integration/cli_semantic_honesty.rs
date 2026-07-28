//! 0096 DoD-4: command-level / CLI integration tests for semantic search honesty.
//!
//! Codex R2 required real-CLI string assertions for `search --semantic`,
//! `index --semantic`, and doctor partial-config — not helper/unit coverage alone.
//!
//! Hermetic: each test uses a tempfile git repo + `ledgerful init` and an
//! explicit local_model config. No external embedding server is required
//! (unconfigured / closed-port cases only).

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

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

/// Git init + ledgerful init + a tiny source file so BM25 fallback has something.
fn hermetic_repo(root: &Path) {
    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src").join("main.rs"),
        "fn main() { let _blast_radius = 1; }\n",
    )
    .unwrap();

    let init = run_in(root, &["init"]);
    assert!(
        init.status.success(),
        "ledgerful init failed: {}",
        combined(&init)
    );
}

fn write_local_model_config(root: &Path, body: &str) {
    let config_path = root.join(".ledgerful").join("config.toml");
    // Preserve any non-local_model sections from init; overwrite local_model entirely.
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
    out.push_str("[local_model]\n");
    out.push_str(body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
    fs::write(config_path, out).unwrap();
}

fn write_unconfigured_config(root: &Path) {
    write_local_model_config(
        root,
        r#"base_url = ""
embedding_model = ""
dimensions = 0
timeout_secs = 2
"#,
    );
}

/// Unconfigured `search --semantic` must say the search did not run / is not
/// configured, name base_url or dry-run, and must not sell "no matches" or the
/// Ready+empty `index --semantic` remedy as the story.
#[test]
#[serial(test)]
fn test_search_semantic_unconfigured_honesty() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    hermetic_repo(root);
    write_unconfigured_config(root);

    // BM25 index optional; fallback path is fine either way.
    let _ = run_in(root, &["index"]);

    let output = run_in(root, &["search", "--semantic", "blast radius"]);
    let text = combined(&output);

    let honesty = text.contains("not configured")
        || text.contains("Embedding backend not configured")
        || text.contains("did not run");
    assert!(
        honesty,
        "expected unconfigured / did-not-run honesty, got:\n{text}"
    );
    assert!(
        text.contains("base_url") || text.contains("semantic-dry-run"),
        "expected base_url or semantic-dry-run guidance, got:\n{text}"
    );

    // Bare Ready+empty remedy must not appear when unconfigured.
    assert!(
        !text.contains("Semantic index is empty. Run `ledgerful index --semantic` to populate."),
        "must not recommend bare index --semantic populate when unconfigured:\n{text}"
    );

    // "No relevant code snippets found semantically" is the Ready no-match line.
    // It must not be the unconfigured story without did-not-run / not-configured.
    if text.contains("No relevant code snippets found semantically") {
        assert!(
            text.contains("did not run") || text.contains("not configured"),
            "must not claim no semantic matches as primary unconfigured story:\n{text}"
        );
    }
}

/// Unconfigured `index --semantic` refuses with non-zero exit and names the backend.
#[test]
#[serial(test)]
fn test_index_semantic_unconfigured_refuses() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    hermetic_repo(root);
    write_unconfigured_config(root);

    let output = run_in(root, &["index", "--semantic"]);
    let text = combined(&output);

    assert!(
        !output.status.success(),
        "index --semantic must exit non-zero when unconfigured, got success:\n{text}"
    );

    let mentions_backend = text.contains("base_url")
        || text.contains("embedding")
        || text.contains("not configured")
        || text.contains("backend")
        || text.contains("Semantic indexing requires");
    assert!(
        mentions_backend,
        "refuse message must name backend / base_url / not configured, got:\n{text}"
    );

    // Must not look like a successful semantic indexing completion.
    assert!(
        !text.contains("Semantic indexing complete")
            && !text.contains("Semantic indexing finished"),
        "must not claim semantic indexing completed when refused:\n{text}"
    );
}

/// Configured but unreachable backend: search --semantic reports unreachable.
#[test]
#[serial(test)]
fn test_search_semantic_unreachable_backend() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    hermetic_repo(root);
    write_local_model_config(
        root,
        r#"base_url = "http://127.0.0.1:1"
embedding_model = "test-embed"
dimensions = 3
timeout_secs = 1
"#,
    );

    let _ = run_in(root, &["index"]);
    let output = run_in(root, &["search", "--semantic", "x"]);
    let text = combined(&output);

    assert!(
        text.to_ascii_lowercase().contains("unreachable")
            || (text.contains("did not run") && text.to_ascii_lowercase().contains("unreachable")),
        "expected unreachable (or did not run + unreachable), got:\n{text}"
    );
}

/// JSON mode must not silence unconfigured failure — readiness and/or semantic_error.
#[test]
#[serial(test)]
fn test_search_semantic_unconfigured_json_not_silent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    hermetic_repo(root);
    write_unconfigured_config(root);

    let output = run_in(root, &["search", "--semantic", "x", "--json"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let text = combined(&output);

    let has_not_configured_status = stdout.contains("not_configured")
        || stdout.contains("\"backend_status\":\"not_configured\"")
        || stdout.contains("NotConfigured");
    let has_semantic_error =
        stdout.contains("semantic_error") || stdout.contains("\"record_kind\":\"semantic_error\"");
    let has_honesty_in_stream = text.contains("not configured")
        || text.contains("did not run")
        || text.contains("Embedding backend not configured");

    assert!(
        has_not_configured_status || has_semantic_error || has_honesty_in_stream,
        "JSON mode must surface NotConfigured readiness and/or semantic_error, not silent empty success:\n{text}"
    );
}

/// Doctor with partial config (model name set, base_url empty) is Not configured,
/// not a healthy-looking `(0 dims) @ `.
#[test]
#[serial(test)]
fn test_doctor_partial_config_not_configured() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    hermetic_repo(root);
    write_local_model_config(
        root,
        r#"base_url = ""
embedding_model = "nomic-embed-text"
dimensions = 0
timeout_secs = 2
"#,
    );

    let output = run_in(root, &["doctor"]);
    let text = combined(&output);

    assert!(
        !text.contains("(0 dims) @"),
        "doctor must not print healthy-looking (0 dims) @ for partial config:\n{text}"
    );
    // Strip ANSI; "Not configured" may be yellow-colored.
    let plain: String = text
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\r' || *c == '\t')
        .collect();
    // owo_colors may still leave ESC sequences; also match raw substring.
    assert!(
        text.contains("Not configured") || plain.contains("Not configured"),
        "doctor must report Not configured for model-without-URL, got:\n{text}"
    );
}
