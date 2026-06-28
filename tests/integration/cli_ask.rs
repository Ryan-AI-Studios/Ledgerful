use ledgerful::commands::ask::execute_ask;
use ledgerful::gemini::modes::GeminiMode;
use ledgerful::impact::packet::ImpactPacket;
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use serial_test::serial;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

use crate::common::{DirGuard, TempEnv, non_interactive};

#[test]
#[serial(env, cwd)]
fn test_ask_command_no_packet() {
    let _env_non_interactive = non_interactive();
    let _env_gemini = TempEnv::remove("GEMINI_API_KEY");

    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);

    // Initialize a minimal git repo
    std::process::Command::new("git")
        .arg("init")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();

    // We must init the storage so it can be queried, even if there's no packet.
    // If it's totally missing, execute_ask will try to create it.
    // We expect it to succeed now (fallback to global mode) instead of erroring out.
    // However, it will fail when trying to connect to Gemini/Local if no config is set.
    // We'll write a dummy config to trigger a specific error later in the chain,
    // proving it got past the "No impact report" check.

    fs::write(layout.config_file(), "[gemini]\nfast_model = \"dummy\"\n").unwrap();

    let result = execute_ask(
        Some("What's up?".into()),
        false, // semantic
        10,    // limit
        GeminiMode::Analyze,
        false, // narrative
        None,  // backend
        false, // auto_index
        15,    // timeout_secs
        false, // no_kg_fallback
        false, // auto_scan
    );

    // It should NOT fail with "No impact report found" anymore.
    // Depending on the test environment, it might fail to reach Gemini or the local model.
    if let Err(e) = result {
        let err_str = e.to_string();
        assert!(
            !err_str.contains("No impact report found"),
            "Should fallback to global mode"
        );
    }
}

/// Regression for CG-F20: an exact structural query ("what calls X") must be
/// answered directly from the graph/index, with no LLM backend required at
/// all. No `GEMINI_API_KEY` and no local-model config are present here; if
/// the routing fix regresses and the query falls through to the LLM path,
/// `execute_ask` would fail with a "backend not configured" error instead
/// of succeeding.
#[test]
#[serial(env, cwd)]
fn test_ask_resolves_exact_caller_query_without_llm_backend() {
    let _env_non_interactive = non_interactive();
    let _env_gemini = TempEnv::remove("GEMINI_API_KEY");

    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);

    std::process::Command::new("git")
        .arg("init")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();

    // No config.toml at all: neither Gemini nor a local model is configured.
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    {
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES \
             (1, 'src/state/storage_cozo.rs', '2026-01-01T00:00:00Z'), \
             (2, 'src/index/incremental.rs', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project_symbols (id, file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at) VALUES \
             (1, 1, 'remove_snippets_for_files', 'remove_snippets_for_files', 'Function', '2026-01-01T00:00:00Z'), \
             (2, 2, 'reindex_file', 'reindex_file', 'Function', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO structural_edges (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id) \
             VALUES (2, 2, 1, 1)",
            [],
        )
        .unwrap();
    }
    storage.shutdown().unwrap();

    let result = execute_ask(
        Some("what calls remove_snippets_for_files".into()),
        false, // semantic
        10,    // limit
        GeminiMode::Analyze,
        false, // narrative
        None,  // backend
        false, // auto_index
        15,    // timeout_secs
        false, // no_kg_fallback
        false, // auto_scan
    );

    assert!(
        result.is_ok(),
        "exact caller query must resolve without an LLM backend: {result:?}"
    );
}

#[test]
#[serial(cwd)]
fn test_ask_invalid_config_fails_before_query_execution() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);

    // Initialize a minimal git repo
    std::process::Command::new("git")
        .arg("init")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    fs::write(layout.config_file(), "[watch]\ndebounce_ms = 0\n").unwrap();

    {
        let storage =
            StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
        storage.save_packet(&ImpactPacket::default()).unwrap();
        // storage is dropped here, releasing the CozoDB lock
    }

    let err = execute_ask(
        Some("What's up?".into()),
        false, // semantic
        10,    // limit
        GeminiMode::Analyze,
        false, // narrative
        None,  // backend
        false, // auto_index
        15,    // timeout_secs
        false, // no_kg_fallback
        false, // auto_scan
    )
    .unwrap_err();
    assert!(format!("{err:?}").contains("debounce_ms"));
}

/// U22: end-to-end timeout test. The local model completion client must
/// respect the per-call `timeout_secs_override` parameter, return an
/// error, and abort well before the server's mocked response delay.
///
/// Server delay is intentionally small (3s) so that the httpmock
/// listener thread exits promptly after the assertion fires — a 15s
/// delay held the test binary open for an extra 13s, producing false
/// "test running for over 60 seconds" reports.
#[test]
#[serial(env, cwd)]
fn test_ask_respects_cli_timeout_override() {
    use ledgerful::config::model::LocalModelConfig;
    use ledgerful::local_model::client::{ChatMessage, CompletionOptions, complete};
    use std::time::Instant;

    let server = httpmock::MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/v1/chat/completions");
        then.status(200)
            .delay(std::time::Duration::from_secs(3))
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "choices": [{"message": {"content": "too late"}}]
            }));
    });

    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);

    let _env_gemini = TempEnv::remove("GEMINI_API_KEY");
    let _env_openrouter = TempEnv::remove("OPENROUTER_API_KEY");

    let config = LocalModelConfig {
        base_url: server.base_url(),
        embedding_url: None,
        generation_url: None,
        ollama_cloud_url: None,
        ollama_cloud_api_key: None,
        ollama_cloud_model: None,
        embedding_model: String::new(),
        generation_model: "test-model".to_string(),
        rerank_model: String::new(),
        dimensions: 0,
        context_window: 38000,
        timeout_secs: 60, // not used — override takes precedence
        prefer_local: true,
        chunk_top_k: 10,
        chunk_min_similarity: 0.3,
        chunk_dedup_threshold: 0.95,
        disable_hnsw: false,
        concurrency: None,
    };

    let messages = vec![ChatMessage {
        role: "user".to_string(),
        content: "hello".to_string(),
    }];

    let start = Instant::now();
    let result = complete(&config, &messages, &CompletionOptions::default(), Some(1));
    let elapsed = start.elapsed();

    assert!(result.is_err(), "expected timeout error, got: {result:?}");
    let err = result.unwrap_err();
    assert!(
        err.contains("timed out"),
        "expected 'timed out' in error, got: {err}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "expected <2s, got {elapsed:?}"
    );
    assert_eq!(mock.hits(), 1, "the mock should have been hit exactly once");
}

/// Track DX2: when the configured local completion model is unreachable and no
/// cloud fallback is available, `ledgerful ask` must gracefully degrade —
/// exit successfully, emit the spec warning to stderr, and render the retrieved
/// context header + body to stdout — instead of hard-failing.
///
/// This spawns the compiled `ledgerful` binary (matching the pattern in
/// `ask_structural_queries.rs`) so we can assert on the *actual* stdout/stderr
/// observables the spec requires, not just `Ok(())`. A regression that
/// silently drops the warning OR the context render fails here.
///
/// The config points the local model at a port nothing listens on
/// (`http://127.0.0.1:1`), no Gemini/OpenRouter keys are present, and the
/// session is non-interactive so the optional cloud-switch prompt never fires.
/// No real LLM or network is required: the unreachable probe short-circuits
/// before any completion request, and the storage has no indexed content so
/// retrieval yields the empty-context marker.
#[test]
#[serial(env, cwd)]
fn test_ask_degrades_gracefully_when_local_model_unreachable() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    crate::common::setup_git_repo(root);

    let state_dir = root.join(".ledgerful/state");
    fs::create_dir_all(&state_dir).unwrap();
    let storage = StorageManager::init(&state_dir.join("ledger.db")).unwrap();
    storage.shutdown().unwrap();

    // Configure ONLY a local model, pointed at an unreachable port. No Gemini
    // key, no Ollama Cloud, no OpenRouter — so degrade_to_context must fire.
    fs::write(
        root.join(".ledgerful/config.toml"),
        "[local_model]\nbase_url = \"http://127.0.0.1:1\"\ngeneration_model = \"test-model\"\nprefer_local = true\ntimeout_secs = 2\n",
    )
    .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["ask", "What does this codebase do?"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env_remove("GEMINI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .output()
        .unwrap();

    // is_ok() equivalent: the degrade path returns Ok(()), so exit is success.
    assert!(
        output.status.success(),
        "ask must degrade to Ok(()) on unreachable local model, got: {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Spec-pinned warning on stderr (URL substituted from config). The warning
    // is colored via `.yellow()`, so ANSI escapes wrap it — the plain text
    // remains a substring, which is what we assert on.
    assert!(
        stderr.contains("Warning: Local completion model at http://127.0.0.1:1 is unreachable. Falling back to graph/semantic search."),
        "expected spec warning on stderr, got: {stderr}"
    );
    // Spec-pinned context header on stdout — proves the context render fired
    // end-to-end, not just that the command exited successfully.
    assert!(
        stdout.contains("Retrieved context (local model unavailable, skipping synthesis):"),
        "expected spec context header on stdout, got: {stdout}"
    );
}

/// Track DX2: a 429 (rate limit) from the local completion endpoint must NOT
/// degrade — it must keep the existing hard-fail behavior. The probe
/// `ping_completions` surfaces the 429 as a status error (no "unreachable" /
/// "timeout" signature), so `is_degradable_error` returns false and
/// `execute_ask` returns `Err` instead of falling back to context render.
#[test]
#[serial(env, cwd)]
fn test_ask_does_not_degrade_on_rate_limit() {
    let _env_non_interactive = non_interactive();
    let _env_gemini = TempEnv::remove("GEMINI_API_KEY");
    let _env_openrouter = TempEnv::remove("OPENROUTER_API_KEY");

    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/v1/chat/completions");
        then.status(429).body("Too Many Requests");
    });

    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);

    std::process::Command::new("git")
        .arg("init")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();

    // Point the local model at the mocked 429 endpoint. No cloud fallback keys.
    fs::write(
        layout.config_file(),
        format!(
            "[local_model]\nbase_url = \"{}\"\ngeneration_model = \"test-model\"\nprefer_local = true\ntimeout_secs = 5\n",
            server.base_url()
        ),
    )
    .unwrap();

    let result = execute_ask(
        Some("What does this codebase do?".into()),
        false, // semantic
        10,    // limit
        GeminiMode::Analyze,
        false, // narrative
        None,  // backend
        false, // auto_index
        5,     // timeout_secs
        false, // no_kg_fallback
        false, // auto_scan
    );

    assert!(
        result.is_err(),
        "rate-limit must NOT degrade — expected hard-fail Err, got: {result:?}"
    );
    let err = format!("{result:?}");
    // The probe surfaces the 429 status code in its error string; the wrapper
    // message says "probe failed" (not "unreachable", which would mislabel a
    // rate-limit as a transport outage). Assert the rate-limit signal
    // specifically — do NOT rely on the circular "unreachable" substring.
    assert!(
        err.contains("429") || err.contains("rate limit"),
        "expected 429/rate-limit signal in error, got: {err}"
    );
}

/// Track DX2 review finding 1: a 503 (service unavailable) from the local
/// completion endpoint must degrade to context — 503 is a transient
/// model-unavailability (e.g. a warming model), not an auth/rate-limit
/// failure. The probe `ping_completions` surfaces the 503 as `"503 server
/// error (...)`"; `is_degradable_error` now classifies 503/502/504 (and their
/// textual forms) as degradable, so `execute_ask` returns `Ok(())` and the
/// degrade path fires. The 503 single-retry + 2s sleep inside
/// `complete_with_endpoint` happens at the client level for the *completion*
/// path; here the probe short-circuits before that, but the classification
/// fires on the final error string regardless, which is the existing behavior.
#[test]
#[serial(env, cwd)]
fn test_ask_degrades_on_503_service_unavailable() {
    let _env_non_interactive = non_interactive();
    let _env_gemini = TempEnv::remove("GEMINI_API_KEY");
    let _env_openrouter = TempEnv::remove("OPENROUTER_API_KEY");

    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/v1/chat/completions");
        then.status(503).body("Service Unavailable");
    });

    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);

    std::process::Command::new("git")
        .arg("init")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();

    // Point the local model at the mocked 503 endpoint. No cloud fallback keys.
    fs::write(
        layout.config_file(),
        format!(
            "[local_model]\nbase_url = \"{}\"\ngeneration_model = \"test-model\"\nprefer_local = true\ntimeout_secs = 5\n",
            server.base_url()
        ),
    )
    .unwrap();

    let result = execute_ask(
        Some("What does this codebase do?".into()),
        false, // semantic
        10,    // limit
        GeminiMode::Analyze,
        false, // narrative
        None,  // backend
        false, // auto_index
        5,     // timeout_secs
        false, // no_kg_fallback
        false, // auto_scan
    );

    assert!(
        result.is_ok(),
        "503 service-unavailable must degrade to Ok(()) — got: {result:?}"
    );
}

/// Track DX2 review finding 2a: a 401 (unauthorized) from the local
/// completion endpoint must NOT degrade — auth failures stay hard-fail,
/// mirroring the 429 rate-limit test. The probe surfaces the 401 status code
/// in its error string; `is_degradable_error` does not classify 401 as
/// transient, so `execute_ask` returns `Err`.
#[test]
#[serial(env, cwd)]
fn test_ask_does_not_degrade_on_401_unauthorized() {
    let _env_non_interactive = non_interactive();
    let _env_gemini = TempEnv::remove("GEMINI_API_KEY");
    let _env_openrouter = TempEnv::remove("OPENROUTER_API_KEY");

    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/v1/chat/completions");
        then.status(401).body("Unauthorized");
    });

    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);

    std::process::Command::new("git")
        .arg("init")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();

    // Point the local model at the mocked 401 endpoint. No cloud fallback keys.
    fs::write(
        layout.config_file(),
        format!(
            "[local_model]\nbase_url = \"{}\"\ngeneration_model = \"test-model\"\nprefer_local = true\ntimeout_secs = 5\n",
            server.base_url()
        ),
    )
    .unwrap();

    let result = execute_ask(
        Some("What does this codebase do?".into()),
        false, // semantic
        10,    // limit
        GeminiMode::Analyze,
        false, // narrative
        None,  // backend
        false, // auto_index
        5,     // timeout_secs
        false, // no_kg_fallback
        false, // auto_scan
    );

    assert!(
        result.is_err(),
        "401 unauthorized must NOT degrade — expected hard-fail Err, got: {result:?}"
    );
    let err = format!("{result:?}");
    assert!(
        err.contains("401") || err.contains("unauthorized"),
        "expected 401/unauthorized signal in error, got: {err}"
    );
}
