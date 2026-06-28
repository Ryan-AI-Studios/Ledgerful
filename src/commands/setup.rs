use crate::commands::doctor::execute_doctor;
use crate::commands::index::{IndexArgs, execute_index};
use crate::commands::scan::execute_scan;
use crate::state::layout::Layout;
use crate::state::reports::LATEST_IMPACT_REPORT;
use camino::{Utf8Path, Utf8PathBuf};
use miette::{IntoDiagnostic, Result, miette};
use owo_colors::OwoColorize;
use std::env;
use std::io::Write;
use std::path::PathBuf;

/// Run the guided onboarding wizard.
///
/// Flow: welcome → init (if needed) → doctor → first scan → success screen.
/// When `yes` is true, all prompts are skipped and defaults are accepted.
/// When `skip_scan` is true, the first-scan step is omitted.
/// If no git repository is detected, the first-scan step is skipped with a warning.
///
/// The wizard's project root is resolved via `gix::discover(".")` (mirroring
/// `init.rs:192-210`) so the wizard's own bookkeeping (state_exists check,
/// init call, no-git scan-skip, success-screen report path) agrees with
/// what `init`/`index` actually do. A `CwdGuard` additionally redirects the
/// process cwd to the discovered root for the duration of the wizard so
/// that `execute_doctor`/`execute_scan` — which read `env::current_dir()`
/// unconditionally — write to the same `.ledgerful` tree.
///
/// # Concurrency
///
/// This function mutates the process-wide current directory via `CwdGuard`.
/// It is safe for the wizard's normal CLI invocation (single-threaded,
/// one-shot) but should NOT be invoked from a multi-threaded context (MCP
/// tool handler, web-server request) where concurrent relative-path
/// resolution by other threads would race. See [`CwdGuard`] for the full
/// caveat and remediation guidance.
pub fn execute_setup(yes: bool, skip_scan: bool) -> Result<()> {
    // ── 1. Welcome ──────────────────────────────────────────────────────────
    if !yes {
        let mut stdout = std::io::stdout().lock();
        welcome_message(&mut stdout).into_diagnostic()?;
    }

    // ── 2. Resolve git-discovered root ──────────────────────────────────────
    // H1: mirror the gix::discover(".") pattern from init.rs:192-210 so the
    // wizard's own bookkeeping agrees with init/index (which both use the
    // git-discovered workdir) instead of with doctor/scan (which both use
    // raw cwd). Without this, running the wizard from a subdirectory of a
    // git work tree splits the .ledgerful tree across two locations and
    // silently hides state from later commands.
    let root = match gix::discover(".") {
        Ok(repo) => {
            let path = repo
                .workdir()
                .ok_or(crate::commands::CommandError::RepoDiscoveryFailed)?
                .to_path_buf();
            // `gix::discover(".")` from a subdirectory returns a workdir
            // expressed as a path *from the starting point* (e.g.
            // `<git-root>/sub/..`), which still contains a `..` component.
            // Canonicalize so downstream `gix::discover` checks and the
            // scan-step's `open_repo` see the resolved git root. The
            // canonicalize is best-effort: if it fails (e.g. the workdir
            // was deleted between discovery and now), we fall back to the
            // raw workdir path.
            dunce::canonicalize(&path)
                .ok()
                .and_then(|p| Utf8PathBuf::from_path_buf(p).ok())
                .unwrap_or_else(|| {
                    Utf8PathBuf::from_path_buf(path).unwrap_or_else(|_| Utf8PathBuf::from("."))
                })
        }
        Err(_) => Utf8PathBuf::from_path_buf(env::current_dir().into_diagnostic()?)
            .map_err(|_| miette!("Current directory is not valid UTF-8"))?,
    };
    let layout = Layout::new(&root);
    let state_exists = layout.state_dir.exists();

    // H1: redirect cwd to the discovered root so execute_doctor and
    // execute_scan (which both use raw cwd) target the same .ledgerful
    // tree that execute_init and execute_index just wrote to. The guard
    // restores the original cwd on drop, so the wizard doesn't leave the
    // caller's shell in an unexpected directory.
    let _cwd_guard = CwdGuard::enter(&root)?;

    if state_exists {
        if !yes && crate::util::term::is_interactive() {
            // M2 note: this `reconfigure == true` branch (re-running
            // `execute_init` on top of existing state) is unreachable
            // from the integration test suite because `cwd_lock()` in
            // `tests/integration/common/mod.rs` sets
            // `LEDGERFUL_NON_INTERACTIVE=1` for every test, which makes
            // `is_interactive() == false` and skips this `if` entirely.
            // Exercising the affirmative-Confirm path would require a
            // dedicated test seam (e.g. an injectable confirm result),
            // which is intentionally out of scope for this track.
            use inquire::Confirm;
            let reconfigure = Confirm::new("Reconfigure existing setup?")
                .with_default(false)
                .with_help_message("This will re-run init but preserve existing config files")
                .prompt()
                .unwrap_or(false);
            if reconfigure {
                crate::commands::init::execute_init(false)?;
            } else {
                println!("{} Using existing setup.", "✓".green());
            }
        } else {
            println!("{} Using existing setup.", "✓".green());
        }
    } else {
        println!("{} Initializing Ledgerful in {}", "→".cyan(), layout.root);
        crate::commands::init::execute_init(false)?;
        println!("{} Initialization complete.", "✓".green());
    }

    // ── 3. Doctor step ──────────────────────────────────────────────────────
    println!("\n{} Running system health check...", "→".cyan());
    execute_doctor()?;
    println!("{} Health check complete.", "✓".green());

    // ── 4. First scan step (skipped when no git repo) ───────────────────────
    if !skip_scan {
        // LOW-3: CwdGuard above has already redirected cwd to `root`, so
        // discovering from "." is equivalent to discovering from
        // `root.as_std_path()` and matches the discover-at-cwd idiom used
        // elsewhere in the file.
        if gix::discover(".").is_err() {
            println!(
                "{} Skipping first scan: no git repository detected in this directory.",
                "⚠".yellow()
            );
            println!(
                "  Run {} after {} to enable impact analysis.",
                "git init".cyan(),
                "ledgerful index".cyan()
            );
        } else {
            println!("\n{} Running first index and scan...", "→".cyan());
            // Run incremental index first
            execute_index(IndexArgs {
                incremental: true,
                ..Default::default()
            })?;

            // Equivalent to: ledgerful scan --impact
            execute_scan(true, false, false, None, None)?;
            println!("{} First scan complete.", "✓".green());
        }
    }

    // ── 5. Usage-metrics opt-in prompt (gated on feature) ───────────────────
    #[cfg(feature = "usage-metrics")]
    {
        if !yes && crate::util::term::is_interactive() {
            // M2 note: this `opt_in == true` branch (which calls
            // `execute_usage_enable` and exercises the `if opt_in && let
            // Err(e) = ...` error-reporting let-chain) is also unreachable
            // from the integration test suite for the same reason as the
            // reconfigure branch above: `cwd_lock()` forces
            // `LEDGERFUL_NON_INTERACTIVE=1` globally, so
            // `is_interactive()` is always `false` in tests.
            use inquire::Confirm;
            let opt_in = Confirm::new(
                "Opt in to anonymous usage metrics? (helps us understand how Ledgerful is used)",
            )
            .with_default(false)
            .with_help_message(
                "Only command names and feature flags are collected. No repo names, file paths, or content. \
                 Reversible via 'ledgerful usage disable' (or review with 'ledgerful usage show-payload').",
            )
            .prompt()
            .unwrap_or(false);
            if opt_in && let Err(e) = crate::commands::usage::execute_usage_enable() {
                eprintln!("{} Failed to enable usage metrics: {}", "✗".red(), e);
            }
        }
    }

    // ── 6. Success screen ───────────────────────────────────────────────────
    {
        let mut stdout = std::io::stdout().lock();
        success_screen(&mut stdout, &root).into_diagnostic()?;
    }

    Ok(())
}

fn welcome_message<W: Write>(out: &mut W) -> std::io::Result<()> {
    writeln!(
        out,
        "{}
{}
{}
{}",
        "╭──────────────────────────────────────────────────────╮".cyan(),
        "│                                                      │".cyan(),
        "│            Welcome to Ledgerful!                     │".cyan(),
        "│                                                      │".cyan(),
    )?;
    writeln!(
        out,
        "{}",
        "│  Ledgerful is a local-first change intelligence      │".cyan()
    )?;
    writeln!(
        out,
        "{}",
        "│  engine for your code. It provides impact analysis,  │".cyan()
    )?;
    writeln!(
        out,
        "{}",
        "│  hotspot detection, verification planning, and a    │".cyan()
    )?;
    writeln!(
        out,
        "{}",
        "│  cryptographic ledger for every change you make.     │".cyan()
    )?;
    writeln!(
        out,
        "{}",
        "│                                                      │".cyan()
    )?;
    writeln!(
        out,
        "{}",
        "│  This wizard will:                                   │".cyan()
    )?;
    writeln!(
        out,
        "{}",
        "│  1. Initialize Ledgerful in this repository          │".cyan()
    )?;
    writeln!(
        out,
        "{}",
        "│  2. Run a system health check                       │".cyan()
    )?;
    writeln!(
        out,
        "{}",
        "│  3. Perform your first impact scan                  │".cyan()
    )?;
    writeln!(
        out,
        "{}",
        "│  4. Show you what to do next                        │".cyan()
    )?;
    writeln!(
        out,
        "{}",
        "╰──────────────────────────────────────────────────────╯".cyan()
    )?;
    writeln!(out)?;
    Ok(())
}

fn success_screen<W: Write>(out: &mut W, layout_root: &Utf8Path) -> std::io::Result<()> {
    writeln!(
        out,
        "\n{}",
        "╭──────────────────────────────────────────────────────╮".green()
    )?;
    writeln!(
        out,
        "{}",
        "│            Setup Complete!                           │".green()
    )?;
    writeln!(
        out,
        "{}",
        "╰──────────────────────────────────────────────────────╯".green()
    )?;

    // M1: derive the canonical impact-report path from the Layout helper
    // and the `LATEST_IMPACT_REPORT` constant instead of hand-rolling the
    // path segments. This keeps the success screen in sync if `STATE_DIR`
    // or `REPORTS_DIR` ever changes.
    let report_path = Layout::new(layout_root)
        .reports_dir()
        .join(LATEST_IMPACT_REPORT);
    if report_path.exists() {
        writeln!(
            out,
            "{} Impact report: {}",
            "→".cyan(),
            report_path.as_str().dimmed()
        )?;
    }

    writeln!(out)?;
    writeln!(out, "{} Suggested next steps:", "→".cyan())?;
    writeln!(
        out,
        "  {} ledgerful ask \"<question>\"  — Ask questions about your codebase",
        "•".yellow()
    )?;
    writeln!(
        out,
        "  {} ledgerful hotspots          — View hotspot rankings",
        "•".yellow()
    )?;
    writeln!(
        out,
        "  {} ledgerful ledger start      — Start tracking a change",
        "•".yellow()
    )?;
    writeln!(
        out,
        "  {} ledgerful ledger status     — Check provenance state",
        "•".yellow()
    )?;

    #[cfg(feature = "web")]
    {
        writeln!(
            out,
            "  {} ledgerful web start        — Launch the local dashboard",
            "•".yellow()
        )?;
    }

    #[cfg(feature = "mcp")]
    {
        writeln!(
            out,
            "  {} ledgerful mcp               — Run the MCP server (stdio)",
            "•".yellow()
        )?;
    }

    #[cfg(feature = "viz-server")]
    {
        writeln!(
            out,
            "  {} ledgerful viz-server        — Launch the live architecture view",
            "•".yellow()
        )?;
    }

    writeln!(out)?;
    writeln!(
        out,
        "{} Run {} anytime to re-run this wizard.",
        "→".cyan(),
        "ledgerful setup".cyan()
    )?;
    Ok(())
}

/// RAII guard that redirects the process cwd for the duration of its
/// lifetime and restores the original cwd on drop.
///
/// Used by `execute_setup` to make `execute_doctor` and `execute_scan`
/// (which both read `env::current_dir()` unconditionally) target the same
/// `.ledgerful` tree that `execute_init` and `execute_index` (which both
/// use `gix::discover`) just wrote to. The Drop is best-effort: if the
/// original directory has since been removed, the restore is silently
/// skipped (the same fallback `DirGuard` uses in `tests/common`).
///
/// # Concurrency caveat
///
/// SAFETY NOTE: This guard mutates the **process-wide** current directory
/// via `env::set_current_dir`. It is safe for the wizard's normal CLI
/// invocation (single-threaded, one-shot) but should NOT be invoked from a
/// multi-threaded context (MCP tool handler, web-server request) where
/// concurrent relative-path resolution by other threads would race. If M6
/// is ever wired into such contexts, thread the resolved `root` explicitly
/// into `execute_doctor` / `execute_index` / `execute_scan` instead of
/// relying on process cwd.
struct CwdGuard {
    original: PathBuf,
}

impl CwdGuard {
    fn enter(dir: &Utf8Path) -> Result<Self> {
        let original = env::current_dir().into_diagnostic()?;
        if original.as_path() != dir.as_std_path() {
            env::set_current_dir(dir.as_std_path()).into_diagnostic()?;
        }
        Ok(Self { original })
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = env::set_current_dir(&self.original);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_message_contains_key_sections() {
        let mut buf = Vec::new();
        welcome_message(&mut buf).expect("welcome_message should not fail on Vec writer");

        let output = String::from_utf8(buf).expect("welcome_message output must be UTF-8");

        assert!(
            output.contains("Welcome to Ledgerful!"),
            "welcome banner should announce Ledgerful"
        );
        assert!(
            output.contains("This wizard will:"),
            "welcome banner should describe the wizard steps"
        );
        assert!(
            output.contains("1. Initialize Ledgerful in this repository"),
            "step 1 should describe init"
        );
        assert!(
            output.contains("2. Run a system health check"),
            "step 2 should describe doctor"
        );
        assert!(
            output.contains("3. Perform your first impact scan"),
            "step 3 should describe first scan"
        );
        assert!(
            output.contains("4. Show you what to do next"),
            "step 4 should describe the success screen"
        );
        // Box drawing characters indicate the banner frame is emitted
        assert!(
            output.contains('╭') && output.contains('╰'),
            "banner should be enclosed in box-drawing characters"
        );
    }

    #[test]
    fn success_screen_contains_suggested_next_steps() {
        let tmp = tempfile::tempdir().expect("tempdir must succeed");
        let root = Utf8Path::from_path(tmp.path()).expect("tempdir path must be UTF-8");

        // Pre-create a fake impact report so success_screen renders the path.
        std::fs::create_dir_all(root.join(".ledgerful").join("reports"))
            .expect("reports dir must be creatable");
        std::fs::write(
            root.join(".ledgerful")
                .join("reports")
                .join("latest-impact.json"),
            r#"{"sentinel":"success_screen test"}"#,
        )
        .expect("sentinel report must be writable");

        let mut buf = Vec::new();
        success_screen(&mut buf, root).expect("success_screen should not fail on Vec writer");

        let output = String::from_utf8(buf).expect("success_screen output must be UTF-8");

        assert!(
            output.contains("Setup Complete!"),
            "success banner should announce completion"
        );
        assert!(
            output.contains("Impact report:"),
            "success screen should reference the impact report when present"
        );
        assert!(
            output.contains("latest-impact.json"),
            "success screen should reference the canonical report filename"
        );
        assert!(
            output.contains("Suggested next steps:"),
            "success screen should enumerate next steps"
        );
        assert!(
            output.contains("ledgerful ask"),
            "next steps should suggest the `ask` command"
        );
        assert!(
            output.contains("ledgerful hotspots"),
            "next steps should suggest the `hotspots` command"
        );
        assert!(
            output.contains("ledgerful ledger start"),
            "next steps should suggest starting a ledger entry"
        );
        assert!(
            output.contains("ledgerful ledger status"),
            "next steps should suggest checking ledger status"
        );
        assert!(
            output.contains("Run ") && output.contains("anytime"),
            "success screen should advertise re-running the wizard"
        );
        assert!(
            output.contains("re-run this wizard"),
            "success screen should describe the re-run target"
        );
        // Box drawing characters indicate the success frame is emitted
        assert!(
            output.contains('╭') && output.contains('╰'),
            "success screen should be enclosed in box-drawing characters"
        );

        // Feature-gated bullets: only assert present when the matching feature is on.
        #[cfg(feature = "web")]
        assert!(
            output.contains("ledgerful web start"),
            "web feature should advertise the dashboard bullet"
        );
        #[cfg(feature = "mcp")]
        assert!(
            output.contains("ledgerful mcp"),
            "mcp feature should advertise the MCP server bullet"
        );
        #[cfg(feature = "viz-server")]
        assert!(
            output.contains("ledgerful viz-server"),
            "viz-server feature should advertise the live architecture bullet"
        );
    }
}
