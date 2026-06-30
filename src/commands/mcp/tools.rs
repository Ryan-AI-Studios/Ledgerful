use serde_json::Value;
use std::process::{Command, Stdio};
use std::time::Duration;

const MCP_TOOL_TIMEOUT_SECS: u64 = 120;

fn get_ledgerful_exe() -> std::path::PathBuf {
    std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("ledgerful"))
}

fn run_ledgerful_tool<I, S>(args: I) -> Result<std::process::Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let exe = get_ledgerful_exe();
    let exe_str = exe.to_string_lossy().to_string();

    crate::platform::process_policy::check_policy(
        &exe_str,
        &crate::platform::process_policy::ProcessPolicy {
            allowed_commands: vec![exe_str.clone()],
            denied_commands: Vec::new(),
            default_timeout_secs: MCP_TOOL_TIMEOUT_SECS,
            strict: true,
        },
    )
    .map_err(|e| format!("Process policy denied ledgerful self-spawn: {}", e))?;

    let mut child = Command::new(&exe)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn ledgerful tool: {}", e))?;

    let timeout = Duration::from_secs(MCP_TOOL_TIMEOUT_SECS);
    let status = match wait_timeout::ChildExt::wait_timeout(&mut child, timeout)
        .map_err(|e| format!("Error waiting for ledgerful tool: {}", e))?
    {
        Some(status) => status,
        None => {
            let _ = child.kill();
            return Err(format!(
                "ledgerful tool timed out after {} seconds",
                MCP_TOOL_TIMEOUT_SECS
            ));
        }
    };

    child
        .wait_with_output()
        .map(|mut output| {
            output.status = status;
            output
        })
        .map_err(|e| format!("Failed to read ledgerful tool output: {}", e))
}

pub fn dispatch_tool(name: &str, params: Value) -> Value {
    match name {
        "ledger_status" => handle_ledger_status(params),
        "hotspots" => handle_hotspots(params),
        "scan" => handle_scan(params),
        "search" => handle_search(params),
        "ledger_search" => handle_ledger_search(params),
        "ask" => handle_ask(params),
        "endpoints_changed" => handle_endpoints_changed(params),
        "security_boundaries" => handle_security_boundaries(params),
        "dead_code" => handle_dead_code(params),
        "verify_plan" => handle_verify_plan(params),
        _ => error_response(&format!("Tool {} not implemented yet.", name)),
    }
}

fn handle_ledger_status(_params: Value) -> Value {
    let layout = match crate::commands::helpers::get_layout() {
        Ok(l) => l,
        Err(e) => return error_response(&format!("Failed to get layout: {}", e)),
    };
    let mut storage =
        match crate::state::storage::StorageManager::open_read_only_sqlite_only(&layout.root) {
            Ok(s) => s,
            Err(e) => return error_response(&format!("Failed to open storage: {}", e)),
        };
    let config = match crate::commands::helpers::load_ledger_config(&layout) {
        Ok(c) => c,
        Err(e) => return error_response(&format!("Failed to load ledger config: {}", e)),
    };

    let tx_mgr = crate::ledger::TransactionManager::new(&mut storage, layout.root.into(), config);
    let pending = tx_mgr.get_all_pending().unwrap_or_default();
    let unaudited = tx_mgr.get_all_unaudited().unwrap_or_default();

    let status = serde_json::json!({
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

    json_response(&status)
}

fn handle_hotspots(params: Value) -> Value {
    let limit = params["limit"].as_u64().unwrap_or(10) as usize;
    let layout = match crate::commands::helpers::get_layout() {
        Ok(l) => l,
        Err(e) => return error_response(&format!("Failed to get layout: {}", e)),
    };
    let config = crate::config::load_config(&layout).unwrap_or_default();
    let current_dir = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => return error_response(&format!("Failed to get current dir: {}", e)),
    };
    let repo = match crate::git::repo::open_repo(&current_dir) {
        Ok(r) => r,
        Err(e) => return error_response(&format!("Failed to open repo: {}", e)),
    };
    let storage =
        match crate::state::storage::StorageManager::open_read_only_sqlite_only(&layout.root) {
            Ok(s) => s,
            Err(e) => return error_response(&format!("Failed to open storage: {}", e)),
        };

    let history_provider = crate::impact::temporal::GixHistoryProvider::new(&repo);
    let query = crate::impact::hotspots::HotspotQuery {
        limit,
        commits: config.hotspots.max_commits,
        decay_half_life: config.hotspots.decay_half_life,
        ..Default::default()
    };

    let hotspots = crate::impact::hotspots::calculate_hotspots(&storage, &history_provider, &query)
        .unwrap_or_default();
    json_response(&hotspots)
}

fn handle_scan(_params: Value) -> Value {
    let out = match run_ledgerful_tool(["scan", "--impact", "--json"]) {
        Ok(o) => o,
        Err(e) => return error_response(&e),
    };

    if !out.status.success() {
        return error_response(&String::from_utf8_lossy(&out.stderr));
    }

    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text_response(&text)
}

fn handle_search(params: Value) -> Value {
    let query = params["query"].as_str().unwrap_or_default();
    let limit = params["limit"].as_u64().unwrap_or(50).to_string();

    let out = match run_ledgerful_tool(["search", "--json", "--limit", &limit, query]) {
        Ok(o) => o,
        Err(e) => return error_response(&e),
    };

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return error_response(&format!("Search failed: {}\n{}", stdout, stderr));
    }
    text_response(&stdout)
}

fn handle_ledger_search(params: Value) -> Value {
    let layout = match crate::commands::helpers::get_layout() {
        Ok(l) => l,
        Err(e) => return error_response(&format!("Failed to get layout: {}", e)),
    };
    let query = params["query"].as_str().unwrap_or("");
    let days = params["days"].as_u64().unwrap_or(30) as u32;
    let mut storage =
        match crate::state::storage::StorageManager::open_read_only_sqlite_only(&layout.root) {
            Ok(s) => s,
            Err(e) => return error_response(&format!("Failed to open storage: {}", e)),
        };
    let db = crate::ledger::db::LedgerDb::new(storage.get_connection_mut());
    let results = match db.search_ledger(query, None, Some(days.into()), false, Some(50), 0) {
        Ok(r) => r,
        Err(e) => return error_response(&format!("Ledger search failed: {}", e)),
    };
    json_response(&results)
}

fn handle_ask(params: Value) -> Value {
    let query = params["query"].as_str().unwrap_or_default();

    let out = match run_ledgerful_tool(["ask", query]) {
        Ok(o) => o,
        Err(e) => return error_response(&e),
    };

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return error_response(&format!("Ask failed: {}\n{}", stdout, stderr));
    }
    // Remove ansi codes if present, but for now just return text
    text_response(&stdout)
}

fn handle_endpoints_changed(_params: Value) -> Value {
    let out = match run_ledgerful_tool(["endpoints", "--changed", "--json"]) {
        Ok(o) => o,
        Err(e) => return error_response(&e),
    };

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return error_response(&format!("Endpoints failed: {}\n{}", stdout, stderr));
    }
    text_response(&stdout)
}

fn handle_security_boundaries(_params: Value) -> Value {
    let out = match run_ledgerful_tool(["security", "boundaries", "--json"]) {
        Ok(o) => o,
        Err(e) => return error_response(&e),
    };

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return error_response(&format!(
            "Security boundaries failed: {}\n{}",
            stdout, stderr
        ));
    }
    text_response(&stdout)
}

fn handle_dead_code(params: Value) -> Value {
    let layout = match crate::commands::helpers::get_layout() {
        Ok(l) => l,
        Err(e) => return error_response(&format!("Failed to get layout: {}", e)),
    };
    let config = crate::config::load_config(&layout).unwrap_or_default();
    let storage = match crate::state::storage::StorageManager::open_read_only(&layout.root) {
        Ok(s) => s,
        Err(e) => return error_response(&format!("Failed to open storage: {}", e)),
    };
    let cozo = storage.cozo.as_ref();
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
        Err(e) => return error_response(&format!("Dead code scan failed: {}", e)),
    };
    json_response(&findings)
}

fn handle_verify_plan(_params: Value) -> Value {
    let layout = match crate::commands::helpers::get_layout() {
        Ok(l) => l,
        Err(e) => return error_response(&format!("Failed to get layout: {}", e)),
    };
    let config = crate::config::load_config(&layout).unwrap_or_default();
    let rules = crate::policy::load::load_rules(&layout).unwrap_or_default();

    let out = match run_ledgerful_tool(["scan", "--impact", "--json"]) {
        Ok(o) => o,
        Err(e) => return error_response(&e),
    };

    if !out.status.success() {
        return error_response(&String::from_utf8_lossy(&out.stderr));
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

fn error_response(msg: &str) -> Value {
    let mut final_msg = msg.to_string();
    if final_msg.contains("Failed to get layout")
        || final_msg.contains("Failed to discover git repository")
    {
        final_msg.push_str("\nHint: No .ledgerful directory found. Please run `ledgerful init`.");
    }
    serde_json::json!({
        "content": [{ "type": "text", "text": final_msg }],
        "isError": true
    })
}

fn text_response(text: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    })
}

fn json_response<T: serde::Serialize>(data: &T) -> Value {
    let text = serde_json::to_string_pretty(data).unwrap_or_default();
    text_response(&text)
}
