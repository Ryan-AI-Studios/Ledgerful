use crate::state::layout::Layout;
use crate::verify::suggestions::query_ledger_status;
use miette::Result;
use owo_colors::{OwoColorize, Stream, Style};

/// Fast health check that only probes executable availability and basic ledger
/// state, skipping OutcomePredictor::predict and full plan building entirely.
/// Returns within a bounded time (<5s on normal machines).
pub(crate) fn execute_verify_health(
    layout: &Layout,
    config: &crate::config::model::Config,
) -> Result<()> {
    println!(
        "{}",
        "Verification Health Check"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().green()))
    );
    // Product-output header (0093): keep with report body on stdout (policy §3).
    println!("Checking verification dependencies...");
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
        println!(
            "  [{}] No verification steps required.",
            "OK".if_supports_color(Stream::Stdout, |s| s.green())
        );
    } else {
        let mut sorted_tools: Vec<_> = expected_tools.into_iter().collect();
        sorted_tools.sort();
        for tool in sorted_tools {
            println!("  Checking {}...", tool);
            let exists = check_executable_exists(&tool);
            if exists {
                println!(
                    "  [{}] {} is available.",
                    "OK".if_supports_color(Stream::Stdout, |s| s.green()),
                    tool
                );
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
                println!(
                    "  [{}] {} not found on PATH.{}",
                    "FAILED".if_supports_color(Stream::Stdout, |s| s.red()),
                    tool,
                    hint
                );
                all_ok = false;
            }
        }
    }

    // Check ledger health (bounded query)
    println!("  Checking ledger state...");
    let ledger_status = query_ledger_status(layout);
    if ledger_status.unaudited_count > 0 || ledger_status.has_stale_pending {
        println!(
            "  [{}] Ledger: {} unaudited, stale pending: {}",
            "NOTE".if_supports_color(Stream::Stdout, |s| s.yellow()),
            ledger_status.unaudited_count,
            ledger_status.has_stale_pending
        );
    } else if ledger_status.no_impact_report {
        println!(
            "  [{}] No impact report found. Run 'ledgerful scan --impact' after making changes.",
            "NOTE".if_supports_color(Stream::Stdout, |s| s.yellow())
        );
    } else {
        println!(
            "  [{}] Ledger is clean.",
            "OK".if_supports_color(Stream::Stdout, |s| s.green())
        );
    }

    // Show runner selection info
    let has_nextest = check_executable_exists("cargo-nextest");
    let prefer_nextest = has_nextest && config.verify.prefer_nextest.unwrap_or(false);
    println!(
        "  [{}] Runner: {} (nextest {})",
        "OK".if_supports_color(Stream::Stdout, |s| s.green()),
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
            "All verification dependencies are available."
                .if_supports_color(Stream::Stdout, |s| s.green())
        );
        Ok(())
    } else {
        Err(miette::miette!(
            "Verification health check failed: some executables are missing."
        ))
    }
}

pub(crate) fn extract_executable(command: &str) -> &str {
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

pub(crate) fn check_executable_exists(name: &str) -> bool {
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
