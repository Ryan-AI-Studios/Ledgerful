use crate::output::human::print_doctor_report;
use crate::platform::env::ExecutableStatus;
use crate::platform::{check_tools, classify_path, current_platform, detect_shell};
use crate::state::layout::Layout;
use chrono::Utc;
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use serde_json::json;
use std::env;

use crate::state::reports::write_clean_tree_tombstone;
use crate::state::storage::StorageManager;

pub fn execute_doctor() -> Result<()> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());

    let platform = current_platform();
    let shell = detect_shell();
    let tools = check_tools();

    layout.ensure_state_dir()?;
    let storage_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(storage_path.as_std_path())?;

    let platform_str = format!("{:?}", platform);
    let shell_str = format!("{:?}", shell);
    let path_kind_str = format!("{:?}", classify_path(&current_dir));

    let mut report = crate::output::human::DoctorReport {
        platform: &platform_str,
        shell: &shell_str,
        tools: &tools,
        path_display: &current_dir.to_string_lossy(),
        path_kind: &path_kind_str,
        is_wsl_mounted: false,
        embedding_model_status: "checking...".to_string(),
        completion_model_status: "checking...".to_string(),
        native_graph_status: "checking...".to_string(),
        active_ask_backend: "checking...".to_string(),
        index_health: Vec::new(),
        target_triple: env!("TARGET"),
    };

    // --- Intelligence Probes ---
    let config = crate::config::load::load_config(&layout)?;
    let mut model_config = config.local_model.clone();
    model_config.timeout_secs = 2;

    report.active_ask_backend = format_active_ask_backend(&config);

    report
        .index_health
        .push(format_gate_mode_status(&layout, &config));

    if config.local_model.embedding_model.is_empty() {
        report.embedding_model_status = "Not configured".yellow().to_string();
    } else {
        match probe_with_retry(|| crate::embed::client::check_local_model(&model_config)) {
            ProbeResult::Healthy(dims) => {
                report.embedding_model_status = format!(
                    "{} ({} dims) @ {}",
                    config.local_model.embedding_model,
                    dims.dimensions,
                    config
                        .local_model
                        .embedding_url
                        .as_deref()
                        .unwrap_or(&config.local_model.base_url)
                );
            }
            ProbeResult::ReachableAfterRetry { val: dims, retries } => {
                report.embedding_model_status = format!(
                    "{} ({} dims) @ {} (reachable after retry: flaky/transient - {})",
                    config.local_model.embedding_model,
                    dims.dimensions,
                    config
                        .local_model
                        .embedding_url
                        .as_deref()
                        .unwrap_or(&config.local_model.base_url),
                    format!(
                        "{} {}",
                        retries,
                        if retries == 1 { "retry" } else { "retries" }
                    )
                    .green()
                );
            }
            ProbeResult::Unreachable { err, retries } => {
                let retry_suffix = if retries > 0 {
                    format!(" after {} retries", retries)
                } else {
                    "".to_string()
                };
                let truncated: String = err.chars().take(80).collect();
                let detail_hint = if err.chars().count() > 80 {
                    " [set RUST_LOG=debug for details]"
                } else {
                    ""
                };
                report.embedding_model_status = format!(
                    "unreachable ({}{}){}",
                    truncated.yellow(),
                    retry_suffix,
                    detail_hint
                );
                tracing::debug!("Full embedding model error: {}", err);
            }
        }
    }

    if config.local_model.generation_model.is_empty() {
        report.completion_model_status = "Not configured".yellow().to_string();
    } else {
        match probe_with_retry(|| crate::local_model::client::ping_completions(&model_config)) {
            ProbeResult::Healthy(model) => {
                report.completion_model_status = format!(
                    "{} @ {}",
                    model,
                    config
                        .local_model
                        .generation_url
                        .as_deref()
                        .unwrap_or(&config.local_model.base_url)
                );
            }
            ProbeResult::ReachableAfterRetry {
                val: model,
                retries,
            } => {
                report.completion_model_status = format!(
                    "{} @ {} (reachable after retry: flaky/transient - {})",
                    model,
                    config
                        .local_model
                        .generation_url
                        .as_deref()
                        .unwrap_or(&config.local_model.base_url),
                    format!(
                        "{} {}",
                        retries,
                        if retries == 1 { "retry" } else { "retries" }
                    )
                    .green()
                );
            }
            ProbeResult::Unreachable { err, retries } => {
                let retry_suffix = if retries > 0 {
                    format!(" after {} retries", retries)
                } else {
                    "".to_string()
                };
                let truncated: String = err.chars().take(80).collect();
                let detail_hint = if err.chars().count() > 80 {
                    " [set RUST_LOG=debug for details]"
                } else {
                    ""
                };
                report.completion_model_status = format!(
                    "unreachable ({}{}){}",
                    truncated.yellow(),
                    retry_suffix,
                    detail_hint
                );
                tracing::debug!("Full completion model error: {}", err);
            }
        }
    }

    let mut total_nodes = 0;
    let mut total_edges = 0;

    // --- Graph Probe ---
    if let Some(cozo) = &storage.cozo {
        match cozo.run_script("?[count(n)] := *node{id: n}") {
            Ok(res) => {
                let node_count = res
                    .rows
                    .first()
                    .and_then(|r| r.first())
                    .and_then(|v| match v {
                        cozo::DataValue::Num(cozo::Num::Int(i)) => Some(*i),
                        _ => None,
                    })
                    .unwrap_or(0);

                let edge_res = cozo.run_script("?[count(s)] := *edge{source: s}");
                let edge_count = edge_res
                    .ok()
                    .and_then(|res| res.rows.first().cloned())
                    .and_then(|r| r.first().cloned())
                    .and_then(|v| match v {
                        cozo::DataValue::Num(cozo::Num::Int(i)) => Some(i),
                        _ => None,
                    })
                    .unwrap_or(0);

                total_nodes = node_count;
                total_edges = edge_count;

                report.native_graph_status = format!(
                    "Ready (CozoDB active, {} nodes, {} edges)",
                    node_count, edge_count
                );
            }
            Err(e) => report.native_graph_status = format!("Error ({})", e.red()),
        }
    } else {
        report.native_graph_status = "Not initialized".to_string();
    }

    // --- Index Health Probes ---
    // 1. Tantivy Search Index
    let index_path = layout.search_index_dir();
    if !index_path.exists() {
        report
            .index_health
            .push("Search index: Missing (run 'ledgerful index')".to_string());
    } else {
        let engine = crate::search::tantivy_engine::TantivySearchEngine::open_or_create(
            index_path.as_std_path(),
        );
        match engine {
            Ok(e) => {
                if let Err(err) = e.verify_index_integrity(index_path.as_std_path()) {
                    report.index_health.push(format!(
                        "Search index: Corrupt ({}) - run 'ledgerful index --full'",
                        err.red()
                    ));
                } else {
                    let docs = e.document_count();
                    report
                        .index_health
                        .push(format!("Search index: OK ({} documents)", docs));
                }
            }
            Err(e) => report
                .index_health
                .push(format!("Search index: Load failed ({})", e.red())),
        }
    }

    // 2. Knowledge Graph Staleness
    if let Some(stale_res) =
        crate::index::staleness::check_index_staleness(&storage, config.index.stale_threshold_days)
    {
        if stale_res.is_missing {
            report
                .index_health
                .push("Graph state: Empty (never indexed)".yellow().to_string());
        } else {
            report.index_health.push(
                format!(
                    "Graph state: STALE ({} files affected) - run 'ledgerful index'",
                    stale_res.stale_files
                )
                .yellow()
                .to_string(),
            );
        }
    } else {
        if total_nodes == 0 && total_edges == 0 {
            report.index_health.push("Graph state: Current (run 'ledgerful index --analyze-graph' to populate the knowledge graph)".to_string());
        } else {
            report.index_health.push("Graph state: Current".to_string());
        }
    }

    // 3. Impact Report Freshness
    if let Ok(repo) = crate::git::repo::open_repo(&current_dir)
        && let Ok((head_hash, branch_name)) = crate::git::repo::get_head_info(&repo)
    {
        let changes = crate::git::status::get_repo_status(&repo).unwrap_or_default();
        // Filter ignored changes like scan does
        let filtered = crate::git::ignore::filter_ignored_changes(
            changes,
            &config.watch.ignore_patterns,
            true,
        )
        .unwrap_or_default();

        let snapshot = crate::git::RepoSnapshot {
            head_hash,
            branch_name,
            is_clean: filtered.is_empty(),
            changes: filtered,
        };

        let freshness = crate::state::reports::check_impact_freshness(&layout, &snapshot);
        match freshness {
            crate::state::reports::ImpactFreshness::Missing => {
                report.index_health.push(
                    "Impact report: None (run 'ledgerful scan --impact')"
                        .yellow()
                        .to_string(),
                );
            }
            crate::state::reports::ImpactFreshness::CurrentClean => {
                report
                    .index_health
                    .push("Impact report: Current (Clean tree)".to_string());
            }
            crate::state::reports::ImpactFreshness::CurrentDirty => {
                report
                    .index_health
                    .push("Impact report: Current (Dirty tree packet)".to_string());
            }
            crate::state::reports::ImpactFreshness::Stale { reason } => {
                if snapshot.is_clean {
                    tracing::debug!(
                        "Auto-refreshing stale clean-tree impact report for HEAD {:?}",
                        snapshot.head_hash
                    );
                    match write_clean_tree_tombstone(
                        &layout,
                        snapshot.head_hash.clone(),
                        snapshot.branch_name.clone(),
                    ) {
                        Ok(()) => {
                            tracing::debug!("Auto-refreshed impact report successfully");
                            report
                                .index_health
                                .push("Impact report: Current (Clean tree)".to_string());
                        }
                        Err(e) => {
                            tracing::debug!("Failed to auto-refresh impact report: {e}");
                            report.index_health.push(
                                format!(
                                    "Impact report: STALE ({}) — run 'ledgerful impact' or 'ledgerful scan --impact' to refresh",
                                    reason
                                )
                                .yellow()
                                .to_string(),
                            );
                        }
                    }
                } else {
                    report.index_health.push(
                        format!(
                            "Impact report: STALE ({}) — run 'ledgerful impact' or 'ledgerful scan --impact' to refresh",
                            reason
                        )
                        .yellow()
                        .to_string(),
                    );
                }
            }
            crate::state::reports::ImpactFreshness::Corrupt { reason } => {
                report.index_health.push(
                    format!("Impact report: Corrupt ({})", reason)
                        .red()
                        .to_string(),
                );
            }
        }
    }

    print_doctor_report(&report);
    print_vram_section();

    // Persist a summary so the web dashboard's `health_score` formula
    // (see `compute_health_score` in `src/commands/web/server.rs`) can
    // apply the `doctor_failures` penalty. The file is intentionally
    // minimal — a single `failures: u64` count and a timestamp — and
    // lives in `state_subdir()` (the same location as `ledger.db`) so
    // the writer and reader agree on the path. The M8 DoD at
    // `conductor/trackM8/spec.md:90` requires this signal to be
    // derivable; without this write the `doctor_failures` term in the
    // health-score formula would always be 0 and a failing doctor
    // run would not move the dashboard off "healthy".
    if let Err(e) = write_doctor_results(&layout, &report) {
        tracing::warn!("Failed to write doctor-results.json: {}", e);
    }

    Ok(())
}

/// Count the number of failed checks in a doctor report.
///
/// Heuristic, by design: a check is "failed" if any of these are true:
/// - a required tool is `ExecutableStatus::NotFound`,
/// - the embedding model is "Not configured" or "unreachable",
/// - the completion model is "Not configured" or "unreachable",
/// - the native graph is "Not initialized" or starts with "Error",
/// - any `index_health` line contains the markers "Corrupt", "Missing",
///   "STALE", or "Load failed" (case-sensitive substring match against
///   the prefix labels produced by `execute_doctor` above).
///
/// The exact markers are kept in sync with the string literals
/// produced in `execute_doctor`; the test
/// `test_doctor_results_count_failures` asserts the mapping.
fn count_doctor_failures(report: &crate::output::human::DoctorReport) -> u64 {
    let mut failures: u64 = 0;

    for (_name, status) in report.tools {
        if matches!(status, ExecutableStatus::NotFound) {
            failures += 1;
        }
    }

    let lower = |s: &String| s.to_ascii_lowercase();
    if report.embedding_model_status.starts_with("Not configured")
        || lower(&report.embedding_model_status).contains("unreachable")
    {
        failures += 1;
    }
    if report.completion_model_status.starts_with("Not configured")
        || lower(&report.completion_model_status).contains("unreachable")
    {
        failures += 1;
    }
    if report.native_graph_status == "Not initialized"
        || report.native_graph_status.starts_with("Error")
    {
        failures += 1;
    }

    for line in &report.index_health {
        if line.contains("Corrupt")
            || line.contains("Missing")
            || line.contains("STALE")
            || line.contains("Load failed")
        {
            failures += 1;
        }
    }

    failures
}

/// Persist a minimal `doctor-results.json` summary to the state's
/// subdir so the web dashboard's health score can read it.
///
/// Schema (per `conductor/trackM8/spec.md` DoD + the M8 review H1
/// recommendation in `output/m8-opencode-1.md`):
/// ```json
/// { "failures": N, "timestamp": "RFC3339" }
/// ```
///
/// Returns `Err` on I/O failure; the caller logs a warning and
/// continues (we never want to abort a successful doctor run because
/// the dashboard-cache write failed).
fn write_doctor_results(
    layout: &Layout,
    report: &crate::output::human::DoctorReport,
) -> Result<()> {
    let failures = count_doctor_failures(report);
    let body = json!({
        "failures": failures,
        "timestamp": Utc::now().to_rfc3339(),
    });
    let path = layout.state_subdir().join("doctor-results.json");
    std::fs::write(
        path.as_std_path(),
        serde_json::to_vec_pretty(&body).into_diagnostic()?,
    )
    .into_diagnostic()?;
    Ok(())
}

#[derive(Debug)]
enum ProbeResult<T> {
    Healthy(T),
    ReachableAfterRetry { val: T, retries: u32 },
    Unreachable { err: String, retries: u32 },
}

fn is_transient_error(err: &str) -> bool {
    let err_lower = err.to_lowercase();
    if err_lower.contains("unreachable")
        || err_lower.contains("timed out")
        || err_lower.contains("timeout")
    {
        return true;
    }
    if err_lower.contains("502") || err_lower.contains("503") || err_lower.contains("504") {
        return true;
    }
    false
}

/// Total wall-clock time `probe_with_retry` is allowed to spend sleeping
/// between retries, per probe. `doctor` is a session-start health check
/// (see `conductor/trackCG-F32/spec.md` requirement #4: "Keep doctor
/// read-only and concise"), so this is intentionally small: 1.5s is
/// enough for a couple of quick retries to catch a genuine flap (a
/// service that comes back up after one or two blips) without letting a
/// fully-down endpoint turn a "fast health check" into a multi-second
/// stall. This budget bounds only the *sleep* time between retries, not
/// the per-attempt network timeout (`model_config.timeout_secs`).
const RETRY_BUDGET: std::time::Duration = std::time::Duration::from_millis(1500);

/// Delay between retry attempts. Kept short relative to `RETRY_BUDGET` so
/// multiple retries can still fit inside the budget.
const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

fn probe_with_retry<T, F>(probe_fn: F) -> ProbeResult<T>
where
    T: std::marker::Send + 'static,
    F: FnMut() -> Result<T, String> + std::marker::Send,
{
    probe_with_retry_budgeted(probe_fn, RETRY_BUDGET, RETRY_DELAY)
}

/// Core retry loop, parameterized by retry budget and inter-retry delay
/// so tests can exercise the deadline logic with tiny durations instead
/// of waiting through the real (small but nonzero) production budget.
///
/// Retries on transient errors (per `is_transient_error`) continue only
/// while the elapsed wall-clock time spent in this call is still under
/// `budget`; once the budget is exhausted, the probe returns
/// `Unreachable` immediately with however many retries were actually
/// attempted, rather than sleeping/retrying further. Non-transient
/// ("semantic") errors always fail immediately with zero retries.
fn probe_with_retry_budgeted<T, F>(
    mut probe_fn: F,
    budget: std::time::Duration,
    delay: std::time::Duration,
) -> ProbeResult<T>
where
    T: std::marker::Send + 'static,
    F: FnMut() -> Result<T, String> + std::marker::Send,
{
    let start = std::time::Instant::now();
    let mut retries = 0;
    // TA15 R4: Per-attempt hard deadline so DNS-level hangs cannot stall
    // doctor indefinitely. The inner ureq timeouts (timeout_connect +
    // timeout_read) fire first when possible; this thread-based deadline
    // covers the entire request lifecycle including DNS resolution.
    let per_attempt_deadline = std::time::Duration::from_secs(10);

    loop {
        // Wrap the probe call in a thread + recv_timeout so a hung DNS
        // resolution or TCP connect cannot stall doctor indefinitely.
        let (tx, rx) = std::sync::mpsc::channel::<Result<T, String>>();
        std::thread::scope(|s| {
            s.spawn(|| {
                let _ = tx.send(probe_fn());
            });
        });

        let probe_result = match rx.recv_timeout(per_attempt_deadline) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!(
                "probe timed out after {}s",
                per_attempt_deadline.as_secs()
            )),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err("probe thread panicked".to_string())
            }
        };

        match probe_result {
            Ok(val) => {
                if retries > 0 {
                    return ProbeResult::ReachableAfterRetry { val, retries };
                } else {
                    return ProbeResult::Healthy(val);
                }
            }
            Err(err) => {
                let elapsed = start.elapsed();
                if is_transient_error(&err) && elapsed + delay <= budget {
                    retries += 1;
                    std::thread::sleep(delay);
                    continue;
                }
                return ProbeResult::Unreachable { err, retries };
            }
        }
    }
}

fn format_active_ask_backend(config: &crate::config::model::Config) -> String {
    format_active_ask_backend_with(config, &|name| std::env::var(name).ok(), &|name| {
        crate::config::model::read_env_key(name)
    })
}

fn format_active_ask_backend_with(
    config: &crate::config::model::Config,
    env_reader: &dyn Fn(&str) -> Option<String>,
    dotenv_reader: &dyn Fn(&str) -> Option<String>,
) -> String {
    // If user configured a provider priority list, show the full chain
    // with model names (TA14 R6). Uses resolve_provider_entries so env var
    // overrides (LEDGERFUL_ASK_MODEL_N) are reflected in the display.
    if !config.ask.providers.priority.is_empty()
        && let Ok(entries) = crate::commands::ask::resolve_provider_entries(config, None)
        && !entries.is_empty()
    {
        let names: Vec<String> = entries
            .iter()
            .map(|e| {
                let model = e.model.as_deref().unwrap_or("");
                if model.is_empty() {
                    e.backend.display_name().to_string()
                } else {
                    format!("{} ({})", e.backend.display_name(), model)
                }
            })
            .collect();
        return names.join(" → ");
    }

    // Legacy display when no provider priority list is configured.
    use crate::commands::ask::{Backend, resolve_backend_with};
    let resolved = resolve_backend_with(config, None, env_reader, dotenv_reader);
    match resolved {
        Backend::Gemini => "Gemini (Cloud)".to_string(),
        Backend::Local | Backend::OllamaCloud | Backend::OpenRouter => {
            let base_url = config
                .local_model
                .generation_url
                .as_deref()
                .unwrap_or(&config.local_model.base_url);
            if base_url.is_empty() {
                "Local (127.0.0.1)".to_string()
            } else {
                let host = parse_url_host(base_url).unwrap_or_else(|| "127.0.0.1".to_string());
                format!("Local ({})", host)
            }
        }
    }
}

fn format_gate_mode_status(
    layout: &crate::state::layout::Layout,
    config: &crate::config::model::Config,
) -> String {
    let effective_mode = config.gate.mode.clone();
    let ledger_mode = crate::ledger::mode_history::current_mode_from_ledger(layout);

    match ledger_mode {
        Some(ledger_mode) if ledger_mode == effective_mode => {
            format!("Gate mode: {} (matches ledger history)", effective_mode)
        }
        Some(ledger_mode) => format!(
            "Gate mode: {} (WARNING: ledger history shows {}; run `ledgerful gate mode {}`)",
            effective_mode, ledger_mode, ledger_mode
        )
        .yellow()
        .to_string(),
        None => format!(
            "Gate mode: {} (no ledger transition history yet)",
            effective_mode
        ),
    }
}

fn parse_url_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let without_scheme = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))?;
    let authority = without_scheme.split('/').next()?;
    let host = authority.split(':').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

fn print_vram_section() {
    #[cfg(target_os = "windows")]
    {
        use crate::platform::gpu::{VramPressure, classify, query_vram_usage};
        match query_vram_usage() {
            Ok(info) => {
                let usage_gb = info.current_usage as f64 / 1_073_741_824.0;
                let budget_gb = info.budget_bytes as f64 / 1_073_741_824.0;
                let pressure = classify(&info);

                let is_arc = info.adapter_name.to_lowercase().contains("arc");
                let note = if is_arc && info.current_usage == 0 {
                    " (Driver limitation: zero-usage reporting on Intel Arc)"
                        .yellow()
                        .to_string()
                } else {
                    "".to_string()
                };

                let usage_str = format!("{:.1}", usage_gb);
                let color_usage = match pressure {
                    VramPressure::Ok => usage_str.white().to_string(),
                    VramPressure::High => usage_str.yellow().bold().to_string(),
                    VramPressure::Critical => usage_str.red().bold().to_string(),
                };
                println!(
                    "{:<20} {} GB / {:.1} GB{}",
                    "GPU VRAM:".bold(),
                    color_usage,
                    budget_gb,
                    note
                );
            }
            Err(e) => println!("{:<20} unavailable ({})", "GPU VRAM:".bold(), e.yellow()),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        println!("{:<20} n/a (Windows-only monitoring)", "GPU VRAM:");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::human::DoctorReport;
    use crate::platform::env::ExecutableStatus;
    use camino::Utf8Path;
    use std::path::PathBuf;

    fn sample_report<'a>(tools: &'a Vec<(String, ExecutableStatus)>) -> DoctorReport<'a> {
        DoctorReport {
            platform: "test",
            shell: "test",
            tools,
            path_display: "test",
            path_kind: "test",
            is_wsl_mounted: false,
            embedding_model_status: "OK".to_string(),
            completion_model_status: "OK".to_string(),
            native_graph_status: "Ready (CozoDB active)".to_string(),
            active_ask_backend: "Gemini (Cloud)".to_string(),
            index_health: vec!["Search index: OK (0 documents)".to_string()],
            target_triple: "test",
        }
    }

    #[test]
    fn test_doctor_results_count_failures_clean() {
        let tools = vec![(
            "git".to_string(),
            ExecutableStatus::Found(PathBuf::from("git")),
        )];
        let report = sample_report(&tools);
        assert_eq!(count_doctor_failures(&report), 0);
    }

    #[test]
    fn test_doctor_results_count_failures_reachable_after_retry() {
        let tools = vec![(
            "git".to_string(),
            ExecutableStatus::Found(PathBuf::from("git")),
        )];
        let report = DoctorReport {
            embedding_model_status: "nomic-embed-text (768 dims) @ http://127.0.0.1:8083 (reachable after retry: flaky/transient - 2 retries)".to_string(),
            completion_model_status: "gemma-4-E4B-it-Q6_K.gguf @ http://127.0.0.1:8081 (reachable after retry: flaky/transient - 1 retry)".to_string(),
            ..sample_report(&tools)
        };
        // Reachable after retry should not count as a failure
        assert_eq!(count_doctor_failures(&report), 0);
    }

    #[test]
    fn test_doctor_results_count_failures_dirty() {
        let tools = vec![
            (
                "git".to_string(),
                ExecutableStatus::Found(PathBuf::from("git")),
            ),
            ("cargo".to_string(), ExecutableStatus::NotFound),
        ];
        let report = DoctorReport {
            embedding_model_status: "Not configured".to_string(),
            completion_model_status: "unreachable (connection refused)".to_string(),
            native_graph_status: "Not initialized".to_string(),
            index_health: vec![
                "Search index: Missing (run 'ledgerful index')".to_string(),
                "Graph state: STALE (5 files affected) - run 'ledgerful index'".to_string(),
                "Search index: Corrupt (bad segment) - run 'ledgerful index --full'".to_string(),
            ],
            ..sample_report(&tools)
        };
        // 1 missing tool + 1 unconfigured embedding + 1 unreachable completion +
        // 1 not-initialized graph + 3 index_health lines = 7
        assert_eq!(count_doctor_failures(&report), 7);
    }

    #[test]
    fn test_doctor_results_count_failures_graph_qualifier_does_not_fail() {
        let tools = vec![(
            "git".to_string(),
            ExecutableStatus::Found(PathBuf::from("git")),
        )];
        let report = DoctorReport {
            index_health: vec!["Graph state: Current (run 'ledgerful index --analyze-graph' to populate the knowledge graph)".to_string()],
            ..sample_report(&tools)
        };
        // The qualifier shouldn't count as a failure
        assert_eq!(count_doctor_failures(&report), 0);
    }

    #[test]
    fn format_active_ask_backend_prefers_gemini_when_configured() {
        let mut config = crate::config::model::Config::default();
        config.gemini.api_key = Some("AIzaTestKey".to_string());
        config.local_model.base_url = "http://127.0.0.1:8081".to_string();
        // Hermetic readers: Gemini wins via explicit api_key regardless of env.
        assert_eq!(
            format_active_ask_backend_with(&config, &|_| None, &|_| None),
            "Gemini (Cloud)"
        );
    }

    #[test]
    fn format_active_ask_backend_prefers_local_when_configured() {
        let mut config = crate::config::model::Config::default();
        config.local_model.base_url = "http://127.0.0.1:8081".to_string();
        config.local_model.generation_model = "test-model".to_string();
        // Hermetic readers returning None so no ambient GEMINI_API_KEY leaks in.
        assert_eq!(
            format_active_ask_backend_with(&config, &|_| None, &|_| None),
            "Local (127.0.0.1)"
        );
    }

    #[test]
    fn format_active_ask_backend_uses_generation_url_host() {
        let mut config = crate::config::model::Config::default();
        config.local_model.generation_url = Some("https://example.com:8080/v1".to_string());
        config.local_model.generation_model = "test-model".to_string();
        assert_eq!(
            format_active_ask_backend_with(&config, &|_| None, &|_| None),
            "Local (example.com)"
        );
    }

    #[test]
    fn parse_url_host_extracts_host_from_http_and_https() {
        assert_eq!(
            parse_url_host("http://127.0.0.1:8081/v1"),
            Some("127.0.0.1".to_string())
        );
        assert_eq!(
            parse_url_host("https://example.com:8080/path"),
            Some("example.com".to_string())
        );
        assert_eq!(parse_url_host("not-a-url"), None);
        assert_eq!(parse_url_host(""), None);
    }

    #[test]
    fn test_write_doctor_results_writes_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8Path::from_path(tmp.path()).expect("utf8 path");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("ensure_state_dir");

        let tools = vec![("git".to_string(), ExecutableStatus::NotFound)];
        let report = DoctorReport {
            embedding_model_status: "Not configured".to_string(),
            ..sample_report(&tools)
        };
        write_doctor_results(&layout, &report).expect("write_doctor_results");

        let path = layout.state_subdir().join("doctor-results.json");
        let body = std::fs::read_to_string(path.as_std_path()).expect("read back");
        let json: serde_json::Value = serde_json::from_str(&body).expect("parse");
        assert_eq!(json["failures"].as_u64(), Some(2));
        assert!(json["timestamp"].as_str().is_some());
    }

    #[test]
    fn test_is_transient_error() {
        assert!(is_transient_error("unreachable (connection refused)"));
        assert!(is_transient_error("timed out after 2s"));
        assert!(is_transient_error("503 server error (Service Unavailable)"));
        assert!(is_transient_error("502 Bad Gateway"));
        assert!(is_transient_error("504 Gateway Timeout"));

        // Semantic errors should not be transient
        assert!(!is_transient_error("400 server error (pooling type none)"));
        assert!(!is_transient_error("401 server error (Unauthorized)"));
        assert!(!is_transient_error("404 server error (Not Found)"));
        assert!(!is_transient_error("some custom error"));
    }

    #[test]
    fn test_probe_with_retry_healthy() {
        let mut count = 0;
        let res = probe_with_retry(|| {
            count += 1;
            Ok("success")
        });
        assert!(matches!(res, ProbeResult::Healthy("success")));
        assert_eq!(count, 1);
    }

    #[test]
    fn test_probe_with_retry_flaky_success() {
        // Tiny budget, but generous enough relative to the tiny test delay
        // for 2 quick retries to land before the budget is exhausted.
        let budget = std::time::Duration::from_millis(50);
        let delay = std::time::Duration::from_millis(1);
        let mut count = 0;
        let res = probe_with_retry_budgeted(
            || {
                count += 1;
                if count < 3 {
                    Err("unreachable (connection refused)".to_string())
                } else {
                    Ok("success")
                }
            },
            budget,
            delay,
        );
        assert!(matches!(
            res,
            ProbeResult::ReachableAfterRetry {
                val: "success",
                retries: 2
            }
        ));
        assert_eq!(count, 3);
    }

    #[test]
    fn test_probe_with_retry_hard_unreachable() {
        // A probe that always fails transiently must eventually stop
        // retrying once the (tiny, test-only) budget is exhausted, rather
        // than retrying forever. We don't assert an exact retry count
        // since that's now a function of timing, not a fixed counter;
        // instead assert the qualitative bound: at least one attempt, a
        // small number of retries, and the error is preserved verbatim.
        let budget = std::time::Duration::from_millis(20);
        let delay = std::time::Duration::from_millis(5);
        let mut count = 0;
        let res: ProbeResult<()> = probe_with_retry_budgeted(
            || {
                count += 1;
                Err("unreachable (connection refused)".to_string())
            },
            budget,
            delay,
        );
        match res {
            ProbeResult::Unreachable { err, retries } => {
                assert_eq!(err, "unreachable (connection refused)");
                // Budget is small relative to delay, so retries must be bounded.
                assert!(retries <= 10, "retries should stay small: {retries}");
                assert_eq!(
                    count,
                    retries + 1,
                    "count is always retries + 1 initial attempt"
                );
            }
            other => panic!("expected Unreachable, got {other:?}"),
        }
    }

    #[test]
    fn test_probe_with_retry_budget_exhausted_stops_retrying() {
        // With a zero retry budget, a transient failure must return
        // Unreachable after exactly the first attempt with zero retries -
        // i.e. the budget check itself (not just is_transient_error) gates
        // whether a retry happens at all.
        let budget = std::time::Duration::from_millis(0);
        let delay = std::time::Duration::from_millis(1);
        let mut count = 0;
        let res: ProbeResult<()> = probe_with_retry_budgeted(
            || {
                count += 1;
                Err("unreachable (connection refused)".to_string())
            },
            budget,
            delay,
        );
        assert!(
            matches!(res, ProbeResult::Unreachable { ref err, retries: 0 } if err == "unreachable (connection refused)")
        );
        assert_eq!(count, 1);
    }

    #[test]
    fn test_probe_with_retry_wall_clock_bounded() {
        // Regression test for the latency-regression finding: a probe that
        // always fails transiently must not cause probe_with_retry to
        // spend more than a small, bounded amount of wall-clock time
        // sleeping between retries. Uses the tiny test budget (not the
        // real RETRY_BUDGET) so the test itself stays fast; the ceiling
        // is generous relative to that budget to avoid flakiness on a
        // loaded CI machine, while still catching an unbounded-retry
        // regression (which would blow well past it).
        let budget = std::time::Duration::from_millis(50);
        let delay = std::time::Duration::from_millis(5);
        let start = std::time::Instant::now();
        let res: ProbeResult<()> = probe_with_retry_budgeted(
            || Err("unreachable (connection refused)".to_string()),
            budget,
            delay,
        );
        let elapsed = start.elapsed();
        assert!(matches!(res, ProbeResult::Unreachable { .. }));
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "probe_with_retry_budgeted took {elapsed:?}, expected well under 500ms for a {budget:?} budget"
        );
    }

    #[test]
    fn test_probe_with_retry_semantic_fail_no_retry() {
        let mut count = 0;
        let res: ProbeResult<()> = probe_with_retry(|| {
            count += 1;
            Err("401 server error (Unauthorized)".to_string())
        });
        assert!(
            matches!(res, ProbeResult::Unreachable { ref err, retries: 0 } if err == "401 server error (Unauthorized)")
        );
        assert_eq!(count, 1); // Fail immediately, no retry
    }
}
