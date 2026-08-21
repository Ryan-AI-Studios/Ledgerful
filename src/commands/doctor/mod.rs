mod binary_currency;
mod checks;
mod finding;
mod remediation;

pub use binary_currency::{
    BINARY_BEHIND_TREE_CODE, BINARY_BEHIND_TREE_REMEDIATION, BinaryCurrencyLag,
    build_binary_behind_tree_finding, classify_binary_currency, compose_binary_currency_message,
    is_ledgerful_engine_worktree, probe_binary_currency, sha_prefix_equal, shorten_sha_for_display,
    worktree_package_version,
};
pub use finding::{
    DoctorCategory, DoctorFinding, DoctorSeverity, DoctorSummary, dashboard_failures,
    ready_for_publish, summarize,
};
pub(crate) use finding::{is_action_critical, is_hygiene, split_doctor_warns};
pub use remediation::{
    ContentHashDriftInputs, GraphAgeInputs, GraphIndexHealth, SearchDocsClassification,
    build_graph_content_stale_finding, build_graph_drift_check_failed_finding,
    build_search_empty_finding, build_sig_pin_finding, build_sig_version_finding,
    build_surfaces_gated_finding, classify_graph_index_health, classify_search_document_count,
    graph_content_stale_index_health_line, graph_current_empty_cozo_index_health_line,
    graph_current_populated_index_health_line, graph_drift_check_failed_index_health_line,
    search_empty_index_health_line, search_ok_index_health_line,
};

use crate::output::human::print_doctor_report;
use crate::platform::{PathKind, check_tools, classify_path, current_platform, detect_shell};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use chrono::Utc;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};
use serde_json::json;
use std::env;

/// Soft-pin warning when `intent.trusted_public_keys` is empty (0072 / 0100 DoD-3).
/// Shares vocabulary with [`crate::ledger::crypto::SignatureTrustStatus::ValidUnknownKey`]:
/// "unknown key", pin, trusted / trusted_public_keys.
/// Message text only (no severity prefix) — severity lives on [`DoctorFinding`].
pub const SIG_PIN_WARNING: &str = "no intent.trusted_public_keys pinned; crypto-valid signatures report VALID (unknown key). Pin keys after init or re-sign.";

#[cfg(test)]
pub(crate) use checks::lifecycle::collect_legacy_migration_findings;
pub(crate) use checks::lifecycle::split_brain_ledger_finding;
#[cfg(test)]
pub(crate) use checks::lifecycle::split_brain_ledger_warning;
#[cfg(test)]
pub(crate) use checks::llm::is_transient_error;
pub(crate) use checks::llm::{
    BackendAvailabilityReport, ProbeResult, embedding_finding, format_active_ask_backend,
    format_embedding_backend_availability, probe_with_retry,
};
#[cfg(test)]
pub(crate) use checks::llm::{
    format_active_ask_backend_with, parse_url_host, probe_with_retry_budgeted,
};
#[cfg(test)]
pub(crate) use checks::optional::{chain_checkpoint_practice_finding, collect_scip_findings};

/// Run doctor health checks.
///
/// When `json` is true, stdout is pure schema-v1 JSON only (no human banners,
/// sccache/SCIP/VRAM printers). Exit code is 1 iff any **block** finding.
/// `full` / `quiet` are ignored for JSON content (schema v1 full findings).
///
/// `--apply-hook-refresh` rewrites only known Ledgerful marker-bounded product
/// templates (0121). Cannot be combined with `--json`.
///
/// Human profile (0174): `full` expands hygiene (optional/info); `quiet`
/// suppresses multi-line remediations and the VRAM footer.
pub fn execute_doctor(
    json: bool,
    apply_hook_refresh: bool,
    dry_run: bool,
    full: bool,
    quiet: bool,
) -> Result<()> {
    if json && apply_hook_refresh {
        return Err(miette::miette!(
            "doctor --json cannot be combined with --apply-hook-refresh"
        ));
    }

    let doctor_started = std::time::Instant::now();
    let current_dir = env::current_dir().into_diagnostic()?;
    let layout = crate::commands::helpers::get_layout_or_cwd_if_not_git()?;

    if apply_hook_refresh {
        let root = layout.root.as_path();
        let refresh = crate::commands::hook_template::refresh_product_templates_at(root, dry_run)?;
        crate::commands::hook_template::print_refresh_report(&refresh);
    }

    let platform = current_platform();
    let shell = detect_shell();
    let tools = check_tools();
    layout.ensure_state_dir()?;
    let storage = StorageManager::init_with_layout(&layout)?;

    let platform_str = format!("{:?}", platform);
    let shell_str = format!("{:?}", shell);
    let path_kind = classify_path(&current_dir);
    let path_kind_str = format!("{:?}", path_kind);
    let work_root_str = layout.root.to_string();
    let state_dir_str = layout.state_dir.to_string();
    let path_display = current_dir.to_string_lossy().into_owned();

    let mut findings: Vec<DoctorFinding> = Vec::new();
    if let Some(finding) = probe_binary_currency(
        layout.root.as_std_path(),
        &current_dir,
        env!("CARGO_PKG_VERSION"),
        env!("LEDGERFUL_GIT_SHA"),
    ) {
        findings.push(finding);
    }
    findings.extend(checks::tools::collect_tool_findings(&tools));

    let mut report = crate::output::human::DoctorReport {
        platform: &platform_str,
        shell: &shell_str,
        tools: &tools,
        path_display: &path_display,
        path_kind: &path_kind_str,
        work_root: &work_root_str,
        state_dir: &state_dir_str,
        is_wsl_mounted: path_kind == PathKind::WslMounted,
        embedding_model_status: "checking...".to_string(),
        embedding_model_failed: false,
        completion_model_status: "checking...".to_string(),
        native_graph_status: "checking...".to_string(),
        active_ask_backend: "checking...".to_string(),
        index_health: Vec::new(),
        target_triple: env!("TARGET"),
    };

    if let Some(f) = split_brain_ledger_finding(&layout) {
        findings.push(f);
    }

    let config = match crate::config::load::load_config(&layout) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("doctor config load failed (continuing with defaults): {e}");
            crate::config::model::Config::default()
        }
    };
    let mut model_config = config.local_model.clone();
    model_config.timeout_secs = 2;
    report.active_ask_backend = format_active_ask_backend(&config);
    checks::lifecycle::apply_gate_mode(&layout, &config, &mut report, &mut findings);

    let model_cfg_embed = model_config.clone();
    let local_model_for_embed = config.local_model.clone();
    let generation_configured = !config.local_model.generation_model.is_empty();
    let generation_endpoint = config
        .local_model
        .generation_url
        .as_deref()
        .unwrap_or(&config.local_model.base_url)
        .to_string();

    let embed_handle = std::thread::spawn(move || {
        format_embedding_backend_availability(&local_model_for_embed, &model_cfg_embed)
    });
    let completion_handle = if generation_configured {
        let cfg = model_config.clone();
        Some(std::thread::spawn(move || {
            probe_with_retry(move || crate::local_model::client::ping_completions(&cfg))
        }))
    } else {
        None
    };

    findings.extend(checks::index::collect_index_findings(
        &storage,
        &layout,
        &config,
        &current_dir,
        &mut report,
    )?);
    findings.extend(checks::lifecycle::collect_lifecycle_findings(
        &layout, &config, &storage,
    )?);
    findings.extend(checks::optional::collect_optional_findings(
        &config, &layout, &storage,
    ));

    apply_joined_network_probes(
        embed_handle,
        completion_handle,
        &generation_endpoint,
        &config,
        &mut report,
        &mut findings,
    );

    findings.sort_by(|a, b| {
        a.code
            .cmp(&b.code)
            .then(a.message.cmp(&b.message))
            .then(a.severity.as_str().cmp(b.severity.as_str()))
    });

    let counts = summarize(&findings);
    let split = split_doctor_warns(&findings);
    debug_assert_eq!(split.total, counts.warn);
    let summary = crate::output::human::DoctorSummaryCounts {
        block: counts.block,
        warn: counts.warn,
        info: counts.info,
    };
    let ready = ready_for_publish(&findings);

    if let Err(e) = write_doctor_results(&layout, &findings) {
        tracing::warn!("Failed to write doctor-results.json: {}", e);
    }

    if json {
        let duration_ms = doctor_started.elapsed().as_millis() as u64;
        let body = json!({
            "schemaVersion": 1u32,
            "readyForPublish": ready,
            "summary": {
                "block": counts.block,
                "warn": counts.warn,
                "warnAction": split.action,
                "warnOptional": split.optional,
                "info": counts.info,
            },
            "findings": findings,
            "environment": {
                "platform": platform_str,
                "shell": shell_str,
                "workRoot": work_root_str,
                "stateDir": state_dir_str,
                "pathDisplay": path_display,
                "targetTriple": env!("TARGET"),
                "binaryVersion": env!("CARGO_PKG_VERSION"),
                "buildSha": env!("LEDGERFUL_GIT_SHA"),
            },
            "durationMs": duration_ms,
        });
        let pretty = serde_json::to_string_pretty(&body).into_diagnostic()?;
        println!("{pretty}");
    } else {
        use crate::output::human::DoctorHumanProfile;
        print_doctor_report(
            &report,
            &summary,
            &findings,
            DoctorHumanProfile { full, quiet },
        );
        if !quiet {
            print_vram_section();
        }
    }

    drop(storage);

    if counts.block > 0 {
        if !json {
            eprintln!(
                "\n{} {} block finding(s). Exit 1.",
                "Doctor:".if_supports_color(Stream::Stderr, |s| s.style(Style::new().red().bold())),
                counts.block
            );
        } else {
            eprintln!("Doctor: {} block finding(s). Exit 1.", counts.block);
        }
        std::process::exit(1);
    }

    Ok(())
}

/// Join embed/completion handles after local probes (0143). Lives on the
/// doctor orchestrator — not in `checks/llm.rs`.
fn apply_joined_network_probes(
    embed_handle: std::thread::JoinHandle<BackendAvailabilityReport>,
    completion_handle: Option<std::thread::JoinHandle<ProbeResult<String>>>,
    generation_endpoint: &str,
    config: &crate::config::model::Config,
    report: &mut crate::output::human::DoctorReport<'_>,
    findings: &mut Vec<DoctorFinding>,
) {
    let avail = match embed_handle.join() {
        Ok(v) => v,
        Err(payload) => std::panic::resume_unwind(payload),
    };
    report.embedding_model_status = avail.display.clone();
    report.embedding_model_failed = avail.is_failure;
    if let Some(detail) = &avail.debug_detail {
        tracing::debug!("Full embedding model error: {}", detail);
    }
    if let Some(f) = embedding_finding(&config.local_model, &avail) {
        findings.push(f);
    }

    match completion_handle {
        None => {
            report.completion_model_status = "Not configured"
                .if_supports_color(Stream::Stdout, |s| s.yellow())
                .to_string();
            findings.push(DoctorFinding::info(
                "completion-not-configured",
                DoctorCategory::Optional,
                "Completion model not configured",
            ));
        }
        Some(handle) => {
            let completion_probe = match handle.join() {
                Ok(v) => v,
                Err(payload) => std::panic::resume_unwind(payload),
            };
            match completion_probe {
                ProbeResult::Healthy(model) => {
                    report.completion_model_status = format!("{model} @ {generation_endpoint}");
                }
                ProbeResult::ReachableAfterRetry {
                    val: model,
                    retries,
                } => {
                    report.completion_model_status = format!(
                        "{} @ {} (reachable after retry: flaky/transient - {})",
                        model,
                        generation_endpoint,
                        format!(
                            "{} {}",
                            retries,
                            if retries == 1 { "retry" } else { "retries" }
                        )
                        .if_supports_color(Stream::Stdout, |s| s.green())
                    );
                }
                ProbeResult::Unreachable { err, retries } => {
                    let retry_suffix = if retries > 0 {
                        format!(" after {retries} retries")
                    } else {
                        String::new()
                    };
                    let truncated: String = err.chars().take(80).collect();
                    let detail_hint = if err.chars().count() > 80 {
                        " [set RUST_LOG=debug for details]"
                    } else {
                        ""
                    };
                    report.completion_model_status = format!(
                        "unreachable ({}{}){}",
                        truncated.if_supports_color(Stream::Stdout, |s| s.yellow()),
                        retry_suffix,
                        detail_hint
                    );
                    tracing::debug!("Full completion model error: {}", err);
                    findings.push(DoctorFinding::warn(
                        "completion-unreachable",
                        DoctorCategory::Optional,
                        format!(
                            "Completion model unreachable ({truncated}{retry_suffix}){detail_hint}"
                        ),
                    ));
                }
            }
        }
    }
}

/// Persist `doctor-results.json` for the web dashboard health score and
/// change-context `doctor.topFindings` (0129).
///
/// Schema (0109 + 0129 additive):
/// ```json
/// {
///   "failures": N,
///   "timestamp": "RFC3339",
///   "readyForPublish": bool,
///   "block": u64,
///   "warn": u64,
///   "info": u64,
///   "findings": [
///     { "code", "severity", "message", "remediation"? }
///   ]
/// }
/// ```
///
/// **`failures`** = [`dashboard_failures`] — `count(block) + count(warn where
/// category != optional)`. Optional backends never contribute. Dashboard/health
/// readers still use only `failures` / counts; unknown `findings` is ignored.
///
/// **`findings`** (0129 + 0138): top-N **action-critical** findings for agent
/// packets — block always; warn when category != Optional; info never.
/// Eligibility is shared with [`dashboard_failures`] via [`is_action_critical`].
/// Optional-category warns are **excluded** (they remain on full `doctor --json`
/// `findings[]`, which includes `category`). Severity-first re-sort (block
/// before warn, then code, then message) before cap 5. Optional
/// `remediation` when present (never null).
///
/// Legacy `results: [{passed}]` array shape is accepted on read only (writers
/// no longer emit it).
///
/// Returns `Err` on I/O failure; the caller logs a warning and continues.
fn write_doctor_results(layout: &Layout, findings: &[DoctorFinding]) -> Result<()> {
    let counts = summarize(findings);
    let failures = dashboard_failures(findings);
    let top = select_sidecar_top_findings(findings);
    let findings_json: Vec<serde_json::Value> = top
        .into_iter()
        .map(|f| {
            let mut obj = serde_json::Map::new();
            obj.insert("code".into(), json!(f.code));
            obj.insert("severity".into(), json!(f.severity.as_str()));
            obj.insert("message".into(), json!(f.message));
            if let Some(ref rem) = f.remediation {
                obj.insert("remediation".into(), json!(rem));
            }
            serde_json::Value::Object(obj)
        })
        .collect();
    let body = json!({
        "failures": failures,
        "timestamp": Utc::now().to_rfc3339(),
        "readyForPublish": ready_for_publish(findings),
        "block": counts.block,
        "warn": counts.warn,
        "info": counts.info,
        "findings": findings_json,
    });
    let path = layout.state_subdir().join("doctor-results.json");
    std::fs::write(
        path.as_std_path(),
        serde_json::to_vec_pretty(&body).into_diagnostic()?,
    )
    .into_diagnostic()?;
    Ok(())
}

/// Select top-N action-critical findings for the doctor sidecar (0129 + 0138).
///
/// Filter: [`is_action_critical`] — block always; warn when category != Optional;
/// info never. Optional-category warns are excluded (full `doctor --json` remains
/// complete). Sort: block before warn, then code, then message — **before** take(5).
/// Eligibility shared with [`dashboard_failures`]; list vs count remain separate.
fn select_sidecar_top_findings(findings: &[DoctorFinding]) -> Vec<&DoctorFinding> {
    let mut selected: Vec<&DoctorFinding> =
        findings.iter().filter(|f| is_action_critical(f)).collect();
    selected.sort_by(|a, b| {
        sidecar_severity_rank(a.severity)
            .cmp(&sidecar_severity_rank(b.severity))
            .then_with(|| a.code.cmp(&b.code))
            .then_with(|| a.message.cmp(&b.message))
    });
    selected.into_iter().take(5).collect()
}

fn sidecar_severity_rank(severity: DoctorSeverity) -> u8 {
    match severity {
        DoctorSeverity::Block => 0,
        DoctorSeverity::Warn => 1,
        DoctorSeverity::Info => 2,
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
                        .if_supports_color(Stream::Stdout, |s| s.yellow())
                        .to_string()
                } else {
                    "".to_string()
                };

                let usage_str = format!("{:.1}", usage_gb);
                let color_usage = match pressure {
                    VramPressure::Ok => usage_str
                        .if_supports_color(Stream::Stdout, |s| s.white())
                        .to_string(),
                    VramPressure::High => usage_str
                        .if_supports_color(Stream::Stdout, |s| {
                            s.style(Style::new().yellow().bold())
                        })
                        .to_string(),
                    VramPressure::Critical => usage_str
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold()))
                        .to_string(),
                };
                println!(
                    "{:<20} {} GB / {:.1} GB{}",
                    "GPU VRAM:".if_supports_color(Stream::Stdout, |s| s.bold()),
                    color_usage,
                    budget_gb,
                    note
                );
            }
            Err(e) => println!(
                "{:<20} unavailable ({})",
                "GPU VRAM:".if_supports_color(Stream::Stdout, |s| s.bold()),
                e.if_supports_color(Stream::Stdout, |s| s.yellow())
            ),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        println!("{:<20} n/a (Windows-only monitoring)", "GPU VRAM:");
    }
}

#[cfg(test)]
mod tests;
