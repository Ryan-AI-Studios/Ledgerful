use crate::output::human::print_verify_plan;
use crate::output::verification::VerificationReporter;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use crate::verify::engine::{VerificationContext, VerifyEngine};
use crate::verify::plan::{VerificationStep, build_plan_from_config, build_plan_scoped};
use crate::verify::predictor::OutcomePredictor;
use crate::verify::suggestions::{generate_suggestions, query_ledger_status};
use crate::verify::timeouts::manual_timeout;
use miette::Result;
use owo_colors::OwoColorize;
use std::env;
use tracing::{info, warn};

pub fn verify_ledger_signatures(layout: &Layout) -> Result<()> {
    let db_path = layout.state_subdir().join("ledger.db");
    let mut storage = StorageManager::init(db_path.as_std_path())?;
    let db = crate::ledger::db::LedgerDb::new(storage.get_connection_mut());

    // Load config to determine whether signing is required.
    let config = crate::config::load::load_config(layout).unwrap_or_default();
    let signing_required = config.intent.require_signing;

    let entries = db
        .get_all_committed_ledger_entries()
        .map_err(|e| miette::miette!("Failed to read ledger entries: {}", e))?;

    if entries.is_empty() {
        eprintln!("Ledger is empty. No signatures to verify.");
        return Ok(());
    }

    tracing::info!(
        target: "cli_summary",
        "Verifying signatures for {} ledger entries (require_signing={})...",
        entries.len(),
        signing_required
    );
    let invalid = enumerate_invalid_ledger_entries(&entries, signing_required);
    let invalid_count = invalid.len();
    let all_valid = invalid_count == 0;

    let invalid_tx_ids: std::collections::HashSet<&str> =
        invalid.iter().map(|(tx_id, _, _)| tx_id.as_str()).collect();
    let mut valid_count = 0usize;
    let mut skipped_count = 0usize;

    for entry in &entries {
        match (&entry.signature, &entry.public_key) {
            (Some(_sig), Some(pub_key)) => {
                if invalid_tx_ids.contains(entry.tx_id.as_str()) {
                    eprintln!(
                        "  [{}] TX {} signature verification FAILED!",
                        "INVALID".red(),
                        &entry.tx_id[..8]
                    );
                } else {
                    tracing::info!(
                        target: "cli_summary",
                        "  [{}] TX {} signed by {}",
                        "VALID".green(),
                        &entry.tx_id[..8],
                        &pub_key[..8]
                    );
                    valid_count += 1;
                }
            }
            _ => {
                if signing_required {
                    eprintln!(
                        "  [{}] TX {} has no signature — treating as verification failure.",
                        "UNSIGNED".yellow(),
                        &entry.tx_id[..8]
                    );
                } else {
                    tracing::info!(
                        target: "cli_summary",
                        "  [{}] TX {} has no signature (signing not required, skipping).",
                        "SKIP".yellow(),
                        &entry.tx_id[..8]
                    );
                    skipped_count += 1;
                }
            }
        }
    }

    tracing::info!(
        target: "cli_summary",
        "\nSignature verification summary: {} valid, {} invalid, {} skipped.",
        valid_count.green(),
        if invalid_count > 0 {
            invalid_count.red().to_string()
        } else {
            invalid_count.to_string()
        },
        skipped_count.yellow()
    );

    if all_valid {
        tracing::info!(
            target: "cli_summary",
            "{}",
            "All signature validations passed successfully!"
                .green()
                .bold()
        );
        Ok(())
    } else {
        Err(miette::miette!(
            "Ledger signature verification failed: {} entries have invalid or missing signatures.",
            invalid_count
        ))
    }
}

pub fn enumerate_invalid_ledger_entries(
    entries: &[crate::ledger::types::LedgerEntry],
    signing_required: bool,
) -> Vec<(String, String, String)> {
    let mut invalid = Vec::new();
    for entry in entries {
        match (&entry.signature, &entry.public_key) {
            (Some(sig), Some(pub_key)) => {
                let valid = crate::ledger::crypto::verify_signature(
                    &entry.tx_id,
                    &entry.category.to_string(),
                    &entry.summary,
                    &entry.reason,
                    &entry.committed_at,
                    sig,
                    pub_key,
                );
                if !valid {
                    invalid.push((entry.tx_id.clone(), sig.clone(), pub_key.clone()));
                }
            }
            _ => {
                if signing_required {
                    // Missing signatures are treated as invalid when signing is required,
                    // but we have no old signature/key fingerprint to record. We still include
                    // them with empty placeholders so callers can decide how to surface them.
                    invalid.push((entry.tx_id.clone(), String::new(), String::new()));
                }
            }
        }
    }
    invalid
}

#[allow(clippy::too_many_arguments)]
pub fn execute_verify(
    command_str: Option<String>,
    timeout_secs: u64,
    no_predict: bool,
    explain: bool,
    entity: Option<String>,
    health: bool,
    dry_run: bool,
    scope: crate::verify::plan::VerifyScope,
) -> Result<()> {
    let current_dir = env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    let manual_requested = command_str.is_some();

    // 1. Initialize Context
    let config = crate::config::load::load_config(&layout).unwrap_or_else(|e| {
        warn!("Config load failed: {e}. Using defaults.");
        crate::config::model::Config::default()
    });

    let mut ctx = VerificationContext::new(
        layout.clone(),
        current_dir.clone(),
        config.clone(),
        no_predict,
        explain,
        health,
    );

    // 2. Load Storage and Packet
    ctx.storage = match StorageManager::open_read_only(&layout.root) {
        Ok(storage) => Some(storage),
        Err(err) => {
            if !no_predict {
                let warning =
                    format!("Prediction disabled: failed to initialize SQLite storage: {err}");
                warn!("{warning}");
                ctx.add_warning(warning);
            }
            None
        }
    };

    if let Some(storage) = &ctx.storage {
        ctx.packet = match storage.get_latest_packet() {
            Ok(packet) => packet,
            Err(err) => {
                if !no_predict {
                    let warning =
                        format!("Prediction disabled: failed to load latest packet: {err}");
                    warn!("{warning}");
                    ctx.add_warning(warning);
                }
                None
            }
        };
    }

    // CG-F35 review fix: there are actually three plan-building paths, not
    // two. Besides the manual-command path (`command_str` is `Some`), a
    // config-defined plan (`[[verify.steps]]` present) takes priority over
    // `OutcomePredictor::predict` below and, like the manual path, never
    // consults `ctx.packet` at all -- `build_plan_from_config` just maps the
    // configured steps verbatim. Compute it once, here, so we can both gate
    // the staleness warning on whether prediction will actually run *and*
    // reuse this same value in the plan-building match below instead of
    // calling `build_plan_from_config` a second time.
    let config_plan = build_plan_from_config(&config.verify);

    // CG-F35 (requirement #1, #6): the packet just loaded above feeds
    // `OutcomePredictor` and the plan-reordering heuristics below. If it's
    // stale or corrupt relative to the current HEAD/working tree, those
    // predictions are quietly built on outdated data. Reuse the same
    // `ctx.add_warning` path the storage-init failure above already uses so
    // this surfaces through `VerificationReporter::report`'s warnings
    // section rather than being silent.
    //
    // Gated on `command_str.is_none() && config_plan.is_none()`: those are
    // exactly the conditions under which the plan-building match below falls
    // through to `OutcomePredictor::predict`. Both the manual-command branch
    // (`command_str` is `Some`) and the config-defined-plan branch
    // (`config_plan` is `Some`) build their plan without consulting
    // `ctx.packet` at all, so warning about stale predictions in either of
    // those paths would be inaccurate, since no prediction happens there.
    if command_str.is_none()
        && config_plan.is_none()
        && ctx.packet.is_some()
        && let Some(reason) = crate::state::reports::warn_if_impact_stale(&layout, &config)
    {
        ctx.add_warning(format!(
            "Verification predictions are based on data where the {reason} — plan ordering may not reflect the current working tree."
        ));
    }

    // Health mode early exit — skip OutcomePredictor::predict and full plan building
    if health {
        return execute_verify_health(&layout, &config);
    }

    // 3. Build Plan
    let (plan, steps) = match command_str {
        Some(ref cmd) => (
            None,
            vec![manual_step(cmd.clone(), manual_timeout(timeout_secs))],
        ),
        None => {
            if let Some(config_plan) = config_plan {
                print_verify_plan(&config_plan);
                (Some(config_plan.clone()), config_plan.steps)
            } else {
                let prediction = OutcomePredictor::predict(&mut ctx)?;
                let rules = crate::policy::load::load_rules(&layout)?;

                let mut plan = match &ctx.packet {
                    Some(packet) => {
                        let conn = ctx.storage.as_ref().map(|s| s.get_connection());
                        let profile = crate::platform::repository::detect_repository(
                            layout.root.as_std_path(),
                        );
                        build_plan_scoped(
                            packet,
                            &rules,
                            &prediction.files,
                            &config.verify,
                            &profile,
                            scope,
                            conn,
                            layout.root.as_std_path(),
                        )
                    }
                    None => {
                        let profile = crate::platform::repository::detect_repository(
                            layout.root.as_std_path(),
                        );
                        let empty_packet = crate::impact::packet::ImpactPacket::default();
                        crate::verify::plan::build_plan(
                            &empty_packet,
                            &rules,
                            &[],
                            &config.verify,
                            &profile,
                            layout.root.as_std_path(),
                        )
                    }
                };

                // Apply probabilistic ordering if storage is available
                if let Some(stg) = &ctx.storage
                    && let Ok(dataset) =
                        crate::verify::probability::extract_dataset(stg.get_connection())
                {
                    let probs = crate::verify::probability::calculate_probabilities(&dataset);
                    plan.apply_probability_ordering(&probs);
                    info!(
                        "Probabilistic verification ordering applied ({} active models).",
                        probs.len()
                    );
                }

                print_verify_plan(&plan);
                let steps = plan.steps.clone();
                (Some(plan), steps)
            }
        }
    };

    // Entity-scoped explanation: show tests mapped to the entity and relevant steps.
    if explain && entity.is_some() {
        let target = entity.as_deref().unwrap_or("");
        println!(
            "\n{}",
            format!("Verification explanation for entity: {}", target)
                .bold()
                .cyan()
        );

        if let Some(storage) = &ctx.storage {
            let conn = storage.get_connection();
            let normalized_entity =
                crate::util::path::normalize_relative_path(layout.root.as_std_path(), target)
                    .unwrap_or_else(|_| target.to_string());

            match explain_test_mappings(conn, &normalized_entity) {
                TestMappingState::TableMissing => {
                    println!(
                        "  Test-mapping table is not present in the index. Run `ledgerful index --incremental` to build it."
                    );
                }
                TestMappingState::TableEmpty => {
                    println!(
                        "  No test mappings have been indexed yet. Run `ledgerful index --incremental` to populate them."
                    );
                }
                TestMappingState::EntityNotIndexed => {
                    println!(
                        "  '{}' is not a recognized indexed file path or symbol name.",
                        target
                    );
                    println!(
                        "  Run `ledgerful index --incremental` if it was added or renamed recently, or confirm the path with `ledgerful search \"{}\"`.",
                        target
                    );
                }
                TestMappingState::NoMappingsForEntity => {
                    println!(
                        "  '{}' is indexed, but no tests currently map to it.",
                        normalized_entity
                    );
                    println!(
                        "  This may be accurate (no covering tests yet) -- use `ledgerful search \"{}\"` to confirm test coverage manually.",
                        normalized_entity
                    );
                }
                TestMappingState::Mapped(tests) => {
                    println!("  Mapped tests ({}):", tests.len());
                    for t in &tests {
                        println!("    • {}", t);
                    }
                }
            }
        }

        let relevant: Vec<_> = steps
            .iter()
            .filter(|s| {
                let cmd = s.command.to_lowercase();
                let t = target.to_lowercase();
                cmd.contains(&t) || cmd.contains("test") || cmd.contains("check")
            })
            .collect();
        println!(
            "\n  Verification steps relevant to this entity ({}):",
            relevant.len()
        );
        for s in &relevant {
            println!("    • {} (timeout: {}s)", s.command, s.timeout_secs);
        }
        println!();
    }

    // Dry Run early exit with compressed output
    if dry_run {
        // For manual commands, print the steps derived from the CLI arg
        if manual_requested {
            println!("{}", "Verification Plan".bold().green());
            println!(
                "  • {} (timeout: {}s)",
                command_str.as_deref().unwrap_or(""),
                timeout_secs
            );
            println!();
        }

        // Group predicted impacts by source for compressed output
        let verbose = std::env::var("VERBOSE_DRY_RUN").is_ok();
        let predicted: Vec<&VerificationStep> = steps
            .iter()
            .filter(|s| s.description.starts_with("Predicted impact"))
            .collect();
        let other: Vec<&VerificationStep> = steps
            .iter()
            .filter(|s| !s.description.starts_with("Predicted impact"))
            .collect();

        // Print non-predicted steps (rules, config)
        if !other.is_empty() {
            println!("{}", "Verification Steps:".bold().cyan());
            for step in &other {
                println!("  • {} (timeout: {}s)", step.command, step.timeout_secs);
            }
        }

        // Print compressed predicted impacts
        if !predicted.is_empty() {
            println!(
                "\n{}",
                "Predicted Impacts (grouped by source):".bold().cyan()
            );
            let mut groups: std::collections::BTreeMap<String, Vec<String>> =
                std::collections::BTreeMap::new();
            for step in &predicted {
                // Extract group name from "Predicted impact (GroupName) on path"
                let desc = &step.description;
                if let Some(start) = desc.find('(')
                    && let Some(end) = desc.find(')')
                {
                    let group = desc[start + 1..end].to_string();
                    let path = desc[end + 5..].to_string(); // ") on " = 5 chars
                    groups.entry(group).or_default().push(path);
                }
            }

            for (source, paths) in &groups {
                println!(
                    "  {}",
                    format!("Source: {} — {} items", source, paths.len()).bold()
                );
                let show = if verbose {
                    paths.len()
                } else {
                    std::cmp::min(5, paths.len())
                };
                for path in paths.iter().take(show) {
                    println!("    • {}", path);
                }
                if !verbose && paths.len() > 5 {
                    println!(
                        "    ... and {} more (set VERBOSE_DRY_RUN=1 for full list)",
                        paths.len() - 5
                    );
                }
            }
        }

        println!(
            "\n{}",
            "Dry run mode: verification plan displayed above. No commands were executed.".yellow()
        );
        return Ok(());
    }

    // 4. Execute
    // Explicitly release the database connection and close locks before running verification commands.
    // This prevents deadlock/lock contention when cargo test runs child Ledgerful commands.
    if let Some(storage) = ctx.storage.take() {
        let _ = storage.shutdown();
    }

    // Show progress indicator before verification execution
    if !ctx.no_predict {
        let num_steps = steps.len();
        if num_steps > 0 {
            tracing::info!(target: "cli_summary", "Running {} verification step(s)...", num_steps);
        }
    }

    let mut report = VerifyEngine::execute(&mut ctx, plan, &steps, manual_requested)?;

    // 5. Generate Suggestions
    let ledger_status = query_ledger_status(&layout);
    let suggestions = generate_suggestions(&report, &ledger_status);

    report = report.with_suggested_actions(suggestions);

    // 6. Final Reporting & IPC
    VerificationReporter::report(&ctx, &report);

    // Push results to AI-Brains
    let bridge_outcomes = report
        .results
        .iter()
        .map(|res| crate::bridge::model::BridgeVerifyOutcome {
            success: res.exit_code == 0,
            command: res.command.clone(),
            error_snippet: if res.exit_code != 0 {
                let err = if !res.stderr_summary.is_empty() {
                    &res.stderr_summary
                } else {
                    &res.stdout_summary
                };
                Some(err.chars().take(200).collect::<String>())
            } else {
                None
            },
        })
        .collect();
    crate::bridge::notify::push_verify_results(bridge_outcomes);

    if report.overall_pass {
        Ok(())
    } else {
        Err(miette::miette!("Verification failed"))
    }
}

/// Fast health check that only probes executable availability and basic ledger
/// state, skipping OutcomePredictor::predict and full plan building entirely.
/// Returns within a bounded time (<5s on normal machines).
fn execute_verify_health(layout: &Layout, config: &crate::config::model::Config) -> Result<()> {
    println!("{}", "Verification Health Check".bold().green());
    eprintln!("Checking verification dependencies...");
    let mut all_ok = true;

    let profile = crate::platform::repository::detect_repository(layout.root.as_std_path());
    let empty_packet = crate::impact::packet::ImpactPacket::default();
    let rules = crate::policy::load::load_rules(layout).unwrap_or_default();
    let effective_plan = crate::verify::plan::build_plan(
        &empty_packet,
        &rules,
        &[],
        &config.verify,
        &profile,
        layout.root.as_std_path(),
    );

    let mut expected_tools = std::collections::HashSet::new();
    for step in &effective_plan.steps {
        let exe = extract_executable(&step.command);
        expected_tools.insert(exe.to_string());
    }

    // Always check for nextest if Rust is present and prefer_nextest is true
    let prefer_nextest = config.verify.prefer_nextest.unwrap_or(false);
    if profile.rust.is_some() && prefer_nextest {
        expected_tools.insert("cargo-nextest".to_string());
    }

    if expected_tools.is_empty() {
        println!("  [{}] No verification steps required.", "OK".green());
    } else {
        let mut sorted_tools: Vec<_> = expected_tools.into_iter().collect();
        sorted_tools.sort();
        for tool in sorted_tools {
            eprintln!("  Checking {}...", tool);
            let exists = check_executable_exists(&tool);
            if exists {
                println!("  [{}] {} is available.", "OK".green(), tool);
            } else {
                let hint = match tool.as_str() {
                    "cargo-nextest" => " (install with `cargo install cargo-nextest`)",
                    "cargo" => " (install Rust toolchain)",
                    "npm" => " (install Node.js)",
                    "pnpm" => " (install pnpm)",
                    "yarn" => " (install yarn)",
                    "bun" => " (install Bun)",
                    "deno" => " (install Deno)",
                    _ => "",
                };
                println!("  [{}] {} not found on PATH.{}", "FAILED".red(), tool, hint);
                all_ok = false;
            }
        }
    }

    // Check ledger health (bounded query)
    eprintln!("  Checking ledger state...");
    let ledger_status = query_ledger_status(layout);
    if ledger_status.unaudited_count > 0 || ledger_status.has_stale_pending {
        println!(
            "  [{}] Ledger: {} unaudited, stale pending: {}",
            "NOTE".yellow(),
            ledger_status.unaudited_count,
            ledger_status.has_stale_pending
        );
    } else if ledger_status.no_impact_report {
        println!(
            "  [{}] No impact report found. Run 'ledgerful scan --impact' after making changes.",
            "NOTE".yellow()
        );
    } else {
        println!("  [{}] Ledger is clean.", "OK".green());
    }

    // Show runner selection info
    let has_nextest = check_executable_exists("cargo-nextest");
    let prefer_nextest = has_nextest && config.verify.prefer_nextest.unwrap_or(false);
    println!(
        "  [{}] Runner: {} (nextest {})",
        "OK".green(),
        if prefer_nextest {
            "cargo nextest"
        } else {
            "cargo test"
        },
        if has_nextest {
            "available"
        } else {
            "not available"
        }
    );

    if all_ok {
        println!(
            "\n{}",
            "All verification dependencies are available.".green()
        );
        Ok(())
    } else {
        Err(miette::miette!(
            "Verification health check failed: some executables are missing."
        ))
    }
}

fn extract_executable(command: &str) -> &str {
    // Skip leading `KEY=value` tokens to reach the actual executable.
    // e.g. `CARGO_TERM_COLOR=always cargo test` -> `cargo`
    let exe_token = command
        .split_whitespace()
        .find(|tok| !tok.contains('='))
        .unwrap_or("");
    // Strip surrounding quotes from the token if present.
    exe_token
        .trim_start_matches(['\"', '\''])
        .trim_end_matches(['\"', '\''])
}

fn check_executable_exists(name: &str) -> bool {
    let path = std::path::Path::new(name);
    if path.is_absolute() || path.components().count() > 1 {
        return path.exists();
    }
    if let Ok(path_env) = std::env::var("PATH") {
        let paths = std::env::split_paths(&path_env);
        for p in paths {
            let exe_path = p.join(name);
            #[cfg(target_os = "windows")]
            {
                for ext in &["", ".exe", ".cmd", ".bat"] {
                    let full_path = if ext.is_empty() {
                        exe_path.clone()
                    } else {
                        let mut s = exe_path.to_string_lossy().to_string();
                        s.push_str(ext);
                        std::path::PathBuf::from(s)
                    };
                    if full_path.is_file() {
                        return true;
                    }
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                if exe_path.is_file() {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(metadata) = std::fs::metadata(&exe_path)
                        && metadata.permissions().mode() & 0o111 != 0
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn manual_step(command: String, timeout_secs: u64) -> VerificationStep {
    VerificationStep {
        description: "Manually requested verification command".to_string(),
        command,
        timeout_secs,
        shell: true,
    }
}

/// Distinct absence/presence states for `verify --explain --entity`, so the
/// CLI can tell "feature is empty here" apart from "feature is broken".
#[derive(Debug, PartialEq, Eq)]
pub enum TestMappingState {
    /// The `test_mapping` table itself doesn't exist (pre-migration DB).
    TableMissing,
    /// The table exists but has never been populated by an index run.
    TableEmpty,
    /// The entity didn't resolve to an indexed file path or a known symbol name.
    EntityNotIndexed,
    /// The entity is indexed, but no test currently maps to it.
    NoMappingsForEntity,
    /// Mapped tests, formatted as `"<test file path>::<test symbol name>"`.
    Mapped(Vec<String>),
}

const MAPPED_TESTS_QUERY_BY_FILE: &str = "SELECT DISTINCT pf_test.file_path || '::' || ps_test.symbol_name \
     FROM test_mapping tm \
     JOIN project_symbols ps_test ON tm.test_symbol_id = ps_test.id \
     JOIN project_files pf_test ON tm.test_file_id = pf_test.id \
     WHERE tm.tested_file_id = ?1 \
     ORDER BY 1";

const MAPPED_TESTS_QUERY_BY_SYMBOL: &str = "SELECT DISTINCT pf_test.file_path || '::' || ps_test.symbol_name \
     FROM test_mapping tm \
     JOIN project_symbols ps_test ON tm.test_symbol_id = ps_test.id \
     JOIN project_files pf_test ON tm.test_file_id = pf_test.id \
     JOIN project_symbols ps_tested ON tm.tested_symbol_id = ps_tested.id \
     WHERE ps_tested.symbol_name = ?1 \
     ORDER BY 1";

/// Resolves test-mapping coverage for an entity against the real
/// `test_mapping` schema (`test_symbol_id`/`test_file_id`/`tested_symbol_id`/
/// `tested_file_id`), trying an exact indexed file path first and falling
/// back to a symbol-name match if the entity isn't a file path.
pub fn explain_test_mappings(
    conn: &rusqlite::Connection,
    normalized_entity: &str,
) -> TestMappingState {
    use rusqlite::OptionalExtension;

    let total: i64 = match conn.query_row("SELECT count(*) FROM test_mapping", [], |row| row.get(0))
    {
        Ok(c) => c,
        Err(_) => return TestMappingState::TableMissing,
    };
    if total == 0 {
        return TestMappingState::TableEmpty;
    }

    let file_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM project_files WHERE file_path = ?1",
            [normalized_entity],
            |row| row.get(0),
        )
        .optional()
        .unwrap_or(None);

    let mapped = if let Some(fid) = file_id {
        conn.prepare(MAPPED_TESTS_QUERY_BY_FILE)
            .and_then(|mut s| {
                s.query_map([fid], |row| row.get(0))
                    .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<String>>())
            })
            .unwrap_or_default()
    } else {
        let symbol_exists: bool = conn
            .query_row(
                "SELECT 1 FROM project_symbols WHERE symbol_name = ?1 LIMIT 1",
                [normalized_entity],
                |_| Ok(true),
            )
            .optional()
            .unwrap_or(None)
            .unwrap_or(false);

        if !symbol_exists {
            return TestMappingState::EntityNotIndexed;
        }

        conn.prepare(MAPPED_TESTS_QUERY_BY_SYMBOL)
            .and_then(|mut s| {
                s.query_map([normalized_entity], |row| row.get(0))
                    .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<String>>())
            })
            .unwrap_or_default()
    };

    if mapped.is_empty() {
        TestMappingState::NoMappingsForEntity
    } else {
        TestMappingState::Mapped(mapped)
    }
}
