//! Track 0073 Codex R1: process-level MCP cloud-egress hard-fail assertions.
//!
//! Spawns the real `ledgerful` binary with `mcp_tool_spawn_env()` (Forbidden
//! child env, no host ALLOW_CLOUD), exercises `ask --backend local` under
//! hermetic malicious/cloud-only config, and proves:
//! - child fails
//! - stdout/stderr name `cloud_policy_forbidden` + `LEDGERFUL_MCP_ALLOW_CLOUD_EGRESS`
//! - mock cloud server receives zero HTTP hits

use ledgerful::local_model::cloud_policy::{
    CLOUD_POLICY_ENV, CLOUD_POLICY_FORBIDDEN_CODE, MCP_ALLOW_CLOUD_EGRESS_ENV, mcp_tool_spawn_env,
    mcp_tool_spawn_env_removes,
};
use ledgerful::state::storage::StorageManager;
use serial_test::serial;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

use crate::common::TempEnv;

/// Process-level keystone: MCP spawn env → real ask child → Forbidden cloud-only
/// config with local-down + cloud credentials/mocks → structured fail + zero hits.
#[test]
#[serial(env, cwd)]
fn mcp_spawn_env_ask_child_forbidden_cloud_only_zero_http() {
    let cloud = httpmock::MockServer::start();
    let mock = cloud.mock(|when, then| {
        when.any_request();
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "choices": [{ "message": { "content": "should never see this" } }]
            }));
    });

    let tmp = tempdir().unwrap();
    let root = tmp.path();
    crate::common::setup_git_repo(root);

    let state_dir = root.join(".ledgerful/state");
    fs::create_dir_all(&state_dir).unwrap();
    let storage = StorageManager::init(&state_dir.join("ledger.db")).unwrap();
    storage.shutdown().unwrap();

    // Cloud-only local_model (omit base_url → empty default) + ollama cloud at mock.
    // Under Forbidden, is_configured ignores cloud → structured policy error.
    // No priority list: legacy path hits the is_configured gate before complete.
    let config = format!(
        "[local_model]\n\
         generation_model = \"test-model\"\n\
         prefer_local = true\n\
         timeout_secs = 3\n\
         ollama_cloud_url = \"{cloud_url}\"\n\
         ollama_cloud_api_key = \"ollama-key-not-real\"\n\
         ollama_cloud_model = \"model:cloud\"\n",
        cloud_url = cloud.base_url()
    );
    fs::write(root.join(".ledgerful/config.toml"), &config).unwrap();

    // Parent MCP host: no allow-cloud opt-in → spawn env includes Forbidden.
    let _allow = TempEnv::remove(MCP_ALLOW_CLOUD_EGRESS_ENV);
    let spawn_env = mcp_tool_spawn_env();
    assert!(
        spawn_env
            .iter()
            .any(|(k, v)| k == CLOUD_POLICY_ENV && v == "forbidden"),
        "MCP spawn must set Forbidden marker"
    );
    assert!(
        mcp_tool_spawn_env_removes().is_empty(),
        "Forbidden path must not remove CLOUD_POLICY"
    );

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let mut cmd = Command::new(ledgerful_bin);
    cmd.args([
        "ask",
        "--backend",
        "local",
        "--",
        "what does this codebase do?",
    ])
    .current_dir(root)
    .env_remove(MCP_ALLOW_CLOUD_EGRESS_ENV)
    .env("OPENROUTER_API_KEY", "sk-or-v1-test-not-real")
    .env("OPENROUTER_BASE_URL", cloud.base_url())
    .env("GEMINI_API_KEY", "test-gemini-key-not-real");

    // Apply the same env pairs run_ledgerful_tool would set on the child.
    for (key, value) in &spawn_env {
        cmd.env(key, value);
    }
    for key in mcp_tool_spawn_env_removes() {
        cmd.env_remove(key);
    }

    let output = cmd.output().expect("failed to spawn ledgerful ask child");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}\n{stderr}");

    assert!(
        !output.status.success(),
        "MCP Forbidden cloud-only ask child must fail, got status {:?}\nstdout: {stdout}\nstderr: {stderr}",
        output.status
    );
    assert!(
        combined.contains(CLOUD_POLICY_FORBIDDEN_CODE),
        "child output must contain cloud_policy_forbidden, got:\n{combined}"
    );
    assert!(
        combined.contains(MCP_ALLOW_CLOUD_EGRESS_ENV),
        "child output must name LEDGERFUL_MCP_ALLOW_CLOUD_EGRESS, got:\n{combined}"
    );
    mock.assert_hits(0);
}

/// Priority-chain network assertion under Forbidden via real child process:
/// malicious priority lists cloud backends first; local is unreachable;
/// mock cloud must see zero hits and output must name the structured policy error
/// (or Local-only failure without egress).
#[test]
#[serial(env, cwd)]
fn mcp_spawn_env_ask_child_forbidden_priority_chain_zero_http() {
    let cloud = httpmock::MockServer::start();
    let mock = cloud.mock(|when, then| {
        when.any_request();
        then.status(200)
            .header("Content-Type", "application/json")
            .json_body(serde_json::json!({
                "choices": [{ "message": { "content": "should never see this" } }]
            }));
    });

    let tmp = tempdir().unwrap();
    let root = tmp.path();
    crate::common::setup_git_repo(root);

    let state_dir = root.join(".ledgerful/state");
    fs::create_dir_all(&state_dir).unwrap();
    let storage = StorageManager::init(&state_dir.join("ledger.db")).unwrap();
    storage.shutdown().unwrap();

    // Local pointed at closed port; priority maliciously lists cloud first.
    let config = format!(
        r#"[local_model]
base_url = "http://127.0.0.1:1"
generation_model = "test-model"
prefer_local = true
timeout_secs = 2
ollama_cloud_url = "{cloud_url}"
ollama_cloud_api_key = "ollama-key-not-real"
ollama_cloud_model = "model:cloud"

[[ask.providers.priority]]
backend = "ollama_cloud"
timeout_secs = 2

[[ask.providers.priority]]
backend = "openrouter"
timeout_secs = 2

[[ask.providers.priority]]
backend = "gemini"
timeout_secs = 2

[[ask.providers.priority]]
backend = "local"
timeout_secs = 2
"#,
        cloud_url = cloud.base_url()
    );
    fs::write(root.join(".ledgerful/config.toml"), config).unwrap();

    let _allow = TempEnv::remove(MCP_ALLOW_CLOUD_EGRESS_ENV);
    let spawn_env = mcp_tool_spawn_env();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let mut cmd = Command::new(ledgerful_bin);
    cmd.args(["ask", "--backend", "local", "--", "summarize risk"])
        .current_dir(root)
        .env_remove(MCP_ALLOW_CLOUD_EGRESS_ENV)
        .env("OPENROUTER_API_KEY", "sk-or-v1-test-not-real")
        .env("OPENROUTER_BASE_URL", cloud.base_url())
        .env("GEMINI_API_KEY", "test-gemini-key-not-real");

    for (key, value) in &spawn_env {
        cmd.env(key, value);
    }
    for key in mcp_tool_spawn_env_removes() {
        cmd.env_remove(key);
    }

    let output = cmd.output().expect("failed to spawn ledgerful ask child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}\n{stderr}");

    // Zero cloud HTTP is the hard requirement. Child may fail with policy error
    // or degrade to context-only after Local-only exhaustion — either is fine
    // as long as the mock is never hit and no cloud completion path ran.
    mock.assert_hits(0);
    assert!(
        combined.contains(CLOUD_POLICY_FORBIDDEN_CODE)
            || combined.contains("Retrieved context")
            || combined.contains("All providers exhausted")
            || !output.status.success(),
        "expected Forbidden policy signal, context-only degrade, or failure; got status {:?}\n{combined}",
        output.status
    );
}
