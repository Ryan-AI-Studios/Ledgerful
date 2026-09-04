use serde_json::Value;

use super::envelope::{error_response, json_response, text_response};
use super::spawn::{
    MCP_ASK_CHILD_TIMEOUT_FLAG, MCP_ASK_CHILD_TIMEOUT_SECS, MCP_TOOL_TIMEOUT_SECS,
    run_ledgerful_tool,
};

pub(super) fn handle_change_context(params: Value) -> Value {
    use crate::commands::change_context::{
        ChangeContextDetail, ChangeContextOpts, DEFAULT_MAX_FILES, build_change_context,
        open_storage_for_change_context, storage_unavailable_reason,
    };

    let detail = match params["detail"].as_str() {
        Some(s) => match ChangeContextDetail::parse(s) {
            Ok(d) => d,
            Err(e) => return error_response(format!("{e}")),
        },
        None => ChangeContextDetail::Minimal,
    };
    let max_files = params["max_files"]
        .as_u64()
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_MAX_FILES)
        .max(1);
    let base_ref = params["base_ref"].as_str().map(|s| s.to_string());
    let blast_depth = params["blast_depth"].as_u64().map(|n| n as u32);
    let paths: Vec<String> = params["paths"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let include_governance = params["include_governance"].as_bool().unwrap_or(false);

    if !paths.is_empty() && base_ref.is_some() {
        return error_response("paths and base_ref are mutually exclusive");
    }

    let opts = ChangeContextOpts {
        detail,
        max_files,
        base_ref,
        blast_depth,
        paths,
        include_governance,
    };

    let layout = match crate::commands::helpers::get_layout_or_cwd_if_not_git() {
        Ok(l) => l,
        Err(e) => return error_response(format!("Failed to get layout: {}", e)),
    };
    let config = match crate::config::load_config(&layout) {
        Ok(c) => c,
        Err(e) => return error_response(format!("Failed to load config: {e}")),
    };
    // Soft-open (B6): prefer true RO when ledger.db exists so pure-RO MCP works.
    let storage = match open_storage_for_change_context(&layout) {
        Ok(s) => s,
        Err((e, class)) => {
            return error_response(format!(
                "Failed to open storage ({class:?}): {}",
                storage_unavailable_reason(&e, class)
            ));
        }
    };

    let packet = match build_change_context(&opts, &layout, &storage, &config) {
        Ok(p) => p,
        Err(e) => {
            let _ = storage.shutdown();
            return error_response(format!("change_context failed: {e}"));
        }
    };
    let _ = storage.shutdown();
    json_response(&packet)
}

pub(super) fn handle_ledger_status(_params: Value) -> Value {
    let layout = match crate::commands::helpers::get_layout_or_cwd_if_not_git() {
        Ok(l) => l,
        Err(e) => return error_response(format!("Failed to get layout: {}", e)),
    };
    let mut storage =
        match crate::state::storage::StorageManager::open_read_only_sqlite_only(&layout) {
            Ok(s) => s,
            Err(e) => return error_response(format!("Failed to open storage: {}", e)),
        };
    let config = match crate::commands::helpers::load_ledger_config(&layout) {
        Ok(c) => c,
        Err(e) => return error_response(format!("Failed to load ledger config: {}", e)),
    };

    let tx_mgr = crate::ledger::TransactionManager::new(&mut storage, layout.root.into(), config);
    json_response(&ledger_status_object(
        tx_mgr.get_all_pending(),
        tx_mgr.get_all_unaudited(),
    ))
}

/// Inner MCP `ledger_status` object (serialized into `content[0].text`).
/// Load failures keep existing keys and add `degraded` + sorted `warnings`.
pub(super) fn ledger_status_object<E: std::fmt::Display>(
    pending: Result<Vec<crate::ledger::types::Transaction>, E>,
    unaudited: Result<Vec<crate::ledger::types::Transaction>, E>,
) -> Value {
    let mut warnings = Vec::new();
    let pending = match pending {
        Ok(v) => v,
        Err(e) => {
            warnings.push(format!("failed to load pending transactions: {e}"));
            Vec::new()
        }
    };
    let unaudited = match unaudited {
        Ok(v) => v,
        Err(e) => {
            warnings.push(format!("failed to load unaudited drift: {e}"));
            Vec::new()
        }
    };
    warnings.sort();

    let mut status = serde_json::json!({
        "pending": pending.len(),
        "unaudited_drift": unaudited.len(),
        "active_tx": pending.iter().map(|t| {
            serde_json::json!({
                "tx_id": t.tx_id,
                "entity": t.entity,
                "category": format!("{:?}", t.category),
                "started_at": t.started_at
            })
        }).collect::<Vec<_>>(),
        "unaudited_file_count": unaudited.iter().map(|u| u.drift_count as usize).sum::<usize>()
    });

    if !warnings.is_empty() {
        status["degraded"] = Value::Bool(true);
        status["warnings"] = Value::Array(warnings.into_iter().map(Value::String).collect());
    }
    status
}

pub(super) fn handle_hotspots(params: Value) -> Value {
    let limit = params["limit"].as_u64().unwrap_or(10) as usize;
    let layout = match crate::commands::helpers::get_layout_or_cwd_if_not_git() {
        Ok(l) => l,
        Err(e) => return error_response(format!("Failed to get layout: {}", e)),
    };
    let config = match crate::config::load_config(&layout) {
        Ok(c) => c,
        Err(e) => return error_response(format!("Failed to load config: {e}")),
    };
    let current_dir = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => return error_response(format!("Failed to get current dir: {}", e)),
    };
    let repo = match crate::git::repo::open_repo(&current_dir) {
        Ok(r) => r,
        Err(e) => return error_response(format!("Failed to open repo: {}", e)),
    };
    let storage = match crate::state::storage::StorageManager::open_read_only_sqlite_only(&layout) {
        Ok(s) => s,
        Err(e) => return error_response(format!("Failed to open storage: {}", e)),
    };

    let history_provider = crate::impact::temporal::GixHistoryProvider::new(&repo);
    let query = crate::impact::hotspots::HotspotQuery {
        limit,
        commits: config.hotspots.max_commits,
        decay_half_life: config.hotspots.decay_half_life,
        ..Default::default()
    };

    let hotspots = crate::impact::hotspots::calculate_hotspots(&storage, &history_provider, &query);
    hotspots_from_calc(hotspots)
}

pub(super) fn hotspots_from_calc<E: std::fmt::Display>(
    result: Result<Vec<crate::impact::packet::Hotspot>, E>,
) -> Value {
    match result {
        Ok(h) => json_response(&h),
        Err(e) => error_response(format!("Failed to calculate hotspots: {e}")),
    }
}

pub(super) fn handle_scan(_params: Value) -> Value {
    let out = match run_ledgerful_tool(["scan", "--impact", "--json"]) {
        Ok(o) => o,
        Err(e) => return error_response(&e),
    };

    if !out.status.success() {
        return error_response(String::from_utf8_lossy(&out.stderr));
    }

    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text_response(&text)
}

/// Build CLI args for MCP `search` (RT-A4: `--` before the untrusted query).
/// 0134: pass `--auto-index` so MCP search refreshes when the index is stale
/// (staleness path; empty `document_count==0` rebuild remains inside CLI).
pub(super) fn build_search_args<'a>(query: &'a str, limit: &'a str) -> Vec<&'a str> {
    vec![
        "search",
        "--json",
        "--auto-index",
        "--limit",
        limit,
        "--",
        query,
    ]
}

pub(super) fn handle_search(params: Value) -> Value {
    let query = params["query"].as_str().unwrap_or_default();
    let limit = params["limit"].as_u64().unwrap_or(50).to_string();

    // RT-A4: `--` separator prevents a query starting with `-` / `--flag`
    // from being parsed as a search CLI option (same confused-deputy class as ask).
    let out = match run_ledgerful_tool(build_search_args(query, &limit)) {
        Ok(o) => o,
        Err(e) => return error_response(&e),
    };

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return error_response(format!("Search failed: {}\n{}", stdout, stderr));
    }
    text_response(&stdout)
}

pub(super) fn handle_ledger_search(params: Value) -> Value {
    let layout = match crate::commands::helpers::get_layout_or_cwd_if_not_git() {
        Ok(l) => l,
        Err(e) => return error_response(format!("Failed to get layout: {}", e)),
    };
    let query = params["query"].as_str().unwrap_or("");
    let days = params["days"].as_u64().unwrap_or(30) as u32;
    let mut storage =
        match crate::state::storage::StorageManager::open_read_only_sqlite_only(&layout) {
            Ok(s) => s,
            Err(e) => return error_response(format!("Failed to open storage: {}", e)),
        };
    let db = crate::ledger::db::LedgerDb::new(storage.get_connection_mut());
    let include_rollback = params["include_rollback"].as_bool().unwrap_or(false);
    let results = match db.search_ledger(
        query,
        None,
        Some(days.into()),
        false,
        Some(50),
        0,
        include_rollback,
    ) {
        Ok(r) => r,
        Err(e) => return error_response(format!("Ledger search failed: {}", e)),
    };
    json_response(&results)
}

pub(super) fn handle_ask(params: Value) -> Value {
    let query = params["query"].as_str().unwrap_or_default();

    // Security gate: the MCP `ask` tool forces --backend local by default so
    // an autonomous agent cannot silently route untrusted repo content to a
    // cloud provider via a configured default backend (0031 confused-deputy
    // mitigation). The gate is read from an ENVIRONMENT VARIABLE (host-level),
    // NOT from repo-local config — a malicious repo could otherwise set the
    // flag in its own .ledgerful/config.toml.
    //
    // Track 0073: --backend local alone only reorders the provider chain;
    // zero cloud is enforced by LEDGERFUL_CLOUD_POLICY=forbidden on the child
    // spawn (see run_ledgerful_tool / mcp_tool_spawn_env), not by the flag alone.
    let allow_cloud = crate::local_model::cloud_policy::mcp_allow_cloud_egress_from_env();

    let args = build_ask_args(query, allow_cloud);
    let out = match run_ledgerful_tool(args) {
        Ok(o) => o,
        Err(e) => return error_response(&e),
    };

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return error_response(format!("Ask failed: {}\n{}", stdout, stderr));
    }
    // Remove ansi codes if present, but for now just return text
    text_response(&stdout)
}

pub(super) fn build_ask_args(query: &str, allow_cloud: bool) -> Vec<&str> {
    // 0158 M4: pass child --timeout under parent MCP_TOOL_TIMEOUT_SECS so
    // ledgerful timeout messaging surfaces before the generic 120s kill.
    const {
        assert!(MCP_ASK_CHILD_TIMEOUT_SECS < MCP_TOOL_TIMEOUT_SECS);
    }
    if allow_cloud {
        vec!["ask", "--timeout", MCP_ASK_CHILD_TIMEOUT_FLAG, "--", query]
    } else {
        // Force local backend so an autonomous agent cannot silently route
        // untrusted repository content to a cloud provider via a configured
        // default backend (0031 confused-deputy mitigation). The `--` separator
        // prevents a malicious query starting with `--backend cloud` from
        // overriding the forced local backend.
        vec![
            "ask",
            "--backend",
            "local",
            "--timeout",
            MCP_ASK_CHILD_TIMEOUT_FLAG,
            "--",
            query,
        ]
    }
}

pub(super) fn handle_endpoints_changed(_params: Value) -> Value {
    let out = match run_ledgerful_tool(["endpoints", "--changed", "--json"]) {
        Ok(o) => o,
        Err(e) => return error_response(&e),
    };

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return error_response(format!("Endpoints failed: {}\n{}", stdout, stderr));
    }
    text_response(&stdout)
}

pub(super) fn handle_security_boundaries(_params: Value) -> Value {
    let out = match run_ledgerful_tool(["security", "boundaries", "--json"]) {
        Ok(o) => o,
        Err(e) => return error_response(&e),
    };

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return error_response(format!(
            "Security boundaries failed: {}\n{}",
            stdout, stderr
        ));
    }
    text_response(&stdout)
}

pub(super) fn handle_dead_code(params: Value) -> Value {
    let layout = match crate::commands::helpers::get_layout_or_cwd_if_not_git() {
        Ok(l) => l,
        Err(e) => return error_response(format!("Failed to get layout: {}", e)),
    };
    let config = crate::config::load_config(&layout).unwrap_or_default();
    let storage = match crate::state::storage::StorageManager::open_read_only(&layout) {
        Ok(s) => s,
        Err(e) => return error_response(format!("Failed to open storage: {}", e)),
    };
    let cozo = storage.cozo();
    let scorer = crate::impact::analysis::dead_code::ConfidenceScorer::new(
        cozo,
        &storage,
        &config.dead_code,
        layout.root.as_std_path(),
        false,
    );

    let limit = params["limit"].as_u64().unwrap_or(50) as usize;
    let findings = match scorer.scan_repo(limit) {
        Ok(f) => f,
        Err(e) => return error_response(format!("Dead code scan failed: {}", e)),
    };
    json_response(&findings)
}

pub(super) fn handle_verify_plan(_params: Value) -> Value {
    let layout = match crate::commands::helpers::get_layout_or_cwd_if_not_git() {
        Ok(l) => l,
        Err(e) => return error_response(format!("Failed to get layout: {}", e)),
    };
    let config = crate::config::load_config(&layout).unwrap_or_default();
    let rules = crate::policy::load::load_rules(&layout).unwrap_or_default();

    let out = match run_ledgerful_tool(["scan", "--impact", "--json"]) {
        Ok(o) => o,
        Err(e) => return error_response(&e),
    };

    if !out.status.success() {
        return error_response(String::from_utf8_lossy(&out.stderr));
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let packet: crate::impact::packet::ImpactPacket = match serde_json::from_str(&text) {
        Ok(p) => p,
        Err(_) => return error_response("Failed to parse scan output"),
    };

    let profile = crate::platform::repository::detect_repository(layout.root.as_std_path());
    let plan = crate::verify::plan::build_plan(
        &packet,
        &rules,
        &[],
        &config.verify,
        &profile,
        layout.root.as_std_path(),
    );
    json_response(&plan)
}
