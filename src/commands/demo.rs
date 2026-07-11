//! `ledgerful demo` — build a synthetic invoice-service repo, run it through
//! the real Ledgerful hook flow, and produce a self-identifying DEMO SOC2
//! evidence export.
//!
//! The command never touches the user's production `~/.ledgerful/keys/`. All
//! keys live inside the demo repo's own `.ledgerful/keys/` directory by
//! redirecting `HOME`/`USERPROFILE` to the demo directory for the duration of
//! the run.

use miette::{IntoDiagnostic, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_DEMO_DIR: &str = "ledgerful-demo";
const DEMO_EVIDENCE_FILE: &str = "ledgerful-DEMO-evidence.zip";

/// One scripted commit cycle.
struct DemoCycle {
    conventional_subject: &'static str,
    conventional_body: &'static str,
    modify: fn(&Path) -> Result<()>,
}

fn build_cycles() -> Vec<DemoCycle> {
    vec![
        DemoCycle {
            conventional_subject: "feat(invoice): [DEMO] add invoice calculation logic",
            conventional_body: "Add line-item totals and tax computation to the invoice module.\n\n[DEMO] This commit is part of the Ledgerful synthetic demo flow.",
            modify: |root| {
                let path = root.join("src/invoice.rs");
                let content = "use rust_decimal::Decimal;\n\n/// Calculate the total for a line item including tax.\npub fn line_total(unit_price: Decimal, quantity: u32, tax_rate: Decimal) -> Decimal {\n    let subtotal = unit_price * Decimal::from(quantity);\n    subtotal * (Decimal::ONE + tax_rate)\n}\n";
                std::fs::write(&path, content).into_diagnostic()
            },
        },
        DemoCycle {
            conventional_subject: "refactor(invoice): [DEMO] extract tax rate into config",
            conventional_body: "Move the hard-coded tax rate into a configurable `TaxConfig` struct.\n\n[DEMO] This commit is part of the Ledgerful synthetic demo flow.",
            modify: |root| {
                let path = root.join("src/invoice.rs");
                let original = std::fs::read_to_string(&path).into_diagnostic()?;
                let config = "\n/// Configurable tax rate.\npub struct TaxConfig {\n    pub rate: Decimal,\n}\n";
                std::fs::write(&path, original + config).into_diagnostic()
            },
        },
        DemoCycle {
            conventional_subject: "feat(cli): [DEMO] add CLI argument parsing",
            conventional_body: "Parse --input and --output flags for the invoice generator.\n\n[DEMO] This commit is part of the Ledgerful synthetic demo flow.",
            modify: |root| {
                let path = root.join("src/main.rs");
                let original = std::fs::read_to_string(&path).into_diagnostic()?;
                let addition = "\n#[allow(dead_code)]\nfn parse_args() -> (String, String) {\n    (\"invoices.csv\".to_string(), \"output.csv\".to_string())\n}\n";
                std::fs::write(&path, original + addition).into_diagnostic()
            },
        },
        DemoCycle {
            conventional_subject: "chore(release): [DEMO] bump version to 0.2.0",
            conventional_body: "Advance the package version to 0.2.0 ahead of the first release.\n\n[DEMO] This commit is part of the Ledgerful synthetic demo flow.",
            modify: |root| {
                let path = root.join("Cargo.toml");
                let original = std::fs::read_to_string(&path).into_diagnostic()?;
                let updated = original.replace("version = \"0.1.0\"", "version = \"0.2.0\"");
                std::fs::write(&path, updated).into_diagnostic()
            },
        },
        DemoCycle {
            conventional_subject: "fix(invoice): [DEMO] fix rounding error in tax calculation",
            conventional_body: "Switch tax calculation from f64 to Decimal to eliminate floating-point rounding.\n\n[DEMO] This commit is part of the Ledgerful synthetic demo flow.",
            modify: |root| {
                let path = root.join("src/invoice.rs");
                let original = std::fs::read_to_string(&path).into_diagnostic()?;
                let addition = "\n// BUGFIX: explicit rounding to two decimal places\npub fn round_money(value: Decimal) -> Decimal {\n    value.round_dp(2)\n}\n";
                std::fs::write(&path, original + addition).into_diagnostic()
            },
        },
    ]
}

fn create_initial_files(root: &Path) -> Result<()> {
    let src = root.join("src");
    std::fs::create_dir_all(&src).into_diagnostic()?;

    std::fs::write(
        src.join("main.rs"),
        "fn main() {\n    println!(\"Invoice service starting...\");\n}\n",
    )
    .into_diagnostic()?;

    std::fs::write(
        src.join("invoice.rs"),
        "//! Invoice calculation logic for the demo service.\n",
    )
    .into_diagnostic()?;

    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"invoice-service\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\n",
    )
    .into_diagnostic()?;

    std::fs::write(
        root.join("README.md"),
        "# invoice-service\n\nSynthetic demo repository generated by `ledgerful demo`.\n",
    )
    .into_diagnostic()?;

    Ok(())
}

fn run_git(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .into_diagnostic()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(miette::miette!(
            "git command failed in demo repo (args: {:?}): {}",
            args,
            stderr
        ));
    }
    Ok(output)
}

fn set_repo_local_git_config(root: &Path) -> Result<()> {
    run_git(root, &["config", "user.name", "Ledgerful Demo"])?;
    run_git(root, &["config", "user.email", "demo@ledgerful.local"])?;
    // Force repo-local hooks path so the host's global core.hooksPath cannot
    // bypass Ledgerful's commit-msg/post-commit hooks.
    run_git(root, &["config", "core.hooksPath", ".git/hooks"])?;
    Ok(())
}

fn ledgerful_binary() -> String {
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| crate::BINARY_NAME.to_string())
}

fn run_ledger_verify_fast(root: &Path) -> Result<()> {
    use owo_colors::OwoColorize;
    let output = Command::new(ledgerful_binary())
        .args(["verify", "--scope", "fast"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .into_diagnostic()?;
    // Print the verify output with a [DEMO] prefix so the verification
    // surface self-identifies as synthetic.
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if !line.is_empty() {
            println!("{} {}", "[DEMO]".cyan().bold(), line);
        }
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stderr.lines() {
        if !line.is_empty() {
            eprintln!("{} {}", "[DEMO]".cyan().bold(), line);
        }
    }
    if output.status.code().is_none() {
        return Err(miette::miette!("ledger verify --scope fast was killed"));
    }
    // The synthetic demo repo won't pass all verification steps (e.g. cargo
    // clippy on dummy Rust files), but the run is still recorded in
    // verification_history.csv — which is the spec's goal ("so
    // verification_history.csv is non-empty"). We accept any non-killed exit
    // status so the demo can proceed to export.
    Ok(())
}

fn run_export(root: &Path, out_path: &Path) -> Result<()> {
    use crate::export::soc2::generate_soc2_export_with_options;
    use crate::state::layout::Layout;
    use camino::Utf8PathBuf;

    let root_utf8 = Utf8PathBuf::from_path_buf(root.to_path_buf())
        .map_err(|_| miette::miette!("demo root path is not valid UTF-8"))?;
    let layout = Layout::new(root_utf8);
    let keys_dir = root.join(".ledgerful").join("keys");
    let zip_bytes = generate_soc2_export_with_options(&layout, true, Some(&keys_dir))?;

    std::fs::write(out_path, &zip_bytes).into_diagnostic()?;
    Ok(())
}

fn run_commit(root: &Path, subject: &str, body: &str) -> Result<()> {
    run_git(root, &["add", "-A"])?;
    let msg = format!("{}\n\n{}", subject, body);
    run_git(root, &["commit", "-m", &msg])?;
    Ok(())
}

/// Determine whether a directory is non-empty. Any entry (including
/// dotfiles like `.git`, `.gitignore`, `.env`) counts as non-empty so the
/// demo never silently destroys a hidden repository or config.
fn dir_is_non_empty(path: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    entries.filter_map(|e| e.ok()).any(|e| {
        let name = e.file_name();
        let name_str = name.to_string_lossy();
        name_str != "." && name_str != ".."
    })
}

fn resolve_demo_dir(output: Option<PathBuf>, force: bool, caller_cwd: &Path) -> Result<PathBuf> {
    let dir = match output {
        Some(p) => {
            if p.is_absolute() {
                p
            } else {
                caller_cwd.join(p)
            }
        }
        None => caller_cwd.join(DEFAULT_DEMO_DIR),
    };

    if dir.exists() && dir_is_non_empty(&dir) && !force {
        return Err(miette::miette!(
            "{} already exists and is not empty; use --force to overwrite",
            dir.display()
        ));
    }

    if dir.exists() {
        std::fs::remove_dir_all(&dir).into_diagnostic()?;
    }
    std::fs::create_dir_all(&dir).into_diagnostic()?;

    Ok(dir)
}

/// RAII guard that redirects the process home directory for the demo run.
struct DemoHomeGuard {
    #[allow(dead_code)]
    demo_dir: PathBuf,
    original_home: Option<std::ffi::OsString>,
    original_userprofile: Option<std::ffi::OsString>,
    original_cwd: PathBuf,
}

impl DemoHomeGuard {
    fn enter(demo_dir: PathBuf) -> Result<Self> {
        let original_home = std::env::var_os("HOME");
        let original_userprofile = std::env::var_os("USERPROFILE");
        let original_cwd = std::env::current_dir().into_diagnostic()?;

        // SAFETY: this function is only called from the single-threaded demo
        // command path before any subprocesses are spawned. The env mutation
        // is intentionally visible to spawned subprocesses (git, ledgerful)
        // so they resolve keys/config relative to the demo directory. The
        // guard restores state on drop.
        unsafe {
            std::env::set_var("HOME", &demo_dir);
            std::env::set_var("USERPROFILE", &demo_dir);
        }
        std::env::set_current_dir(&demo_dir).into_diagnostic()?;

        Ok(Self {
            demo_dir,
            original_home,
            original_userprofile,
            original_cwd,
        })
    }
}

impl Drop for DemoHomeGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original_cwd);
        match &self.original_home {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        match &self.original_userprofile {
            Some(v) => unsafe { std::env::set_var("USERPROFILE", v) },
            None => unsafe { std::env::remove_var("USERPROFILE") },
        }
    }
}

pub fn execute_demo(keep: bool, output: Option<PathBuf>, force: bool) -> Result<()> {
    use owo_colors::OwoColorize;

    let caller_cwd = std::env::current_dir().into_diagnostic()?;
    let demo_dir = resolve_demo_dir(output, force, &caller_cwd)?;

    println!(
        "{} Building demo repository at {}",
        "[DEMO]".cyan().bold(),
        demo_dir.display()
    );

    // Redirect HOME and CWD into the demo directory so Ledgerful's own code
    // (which resolves keys via `dirs::home_dir()` / `$HOME`) treats the demo
    // repo as the production environment. The guard restores state on drop.
    let _home_guard = DemoHomeGuard::enter(demo_dir.clone())?;

    run_git(&demo_dir, &["init"])?;
    set_repo_local_git_config(&demo_dir)?;
    create_initial_files(&demo_dir)?;

    // Install Ledgerful in the synthetic repo. `execute_init` uses the current
    // directory and the current HOME, which now point at the demo directory.
    // Use observe mode (the 0050 default) so the demo shows the real
    // observe-first onboarding users actually receive.
    crate::commands::init::execute_init(false, false)?;

    // Write a demo marker file so the web UI (if opened with --keep) can
    // detect and self-identify the repo as synthetic. The marker is a simple
    // JSON file in .ledgerful/state/ that the API can check.
    let demo_marker = demo_dir.join(".ledgerful").join("DEMO_MARKER");
    std::fs::write(
        &demo_marker,
        r#"{"demo": true, "source": "ledgerful demo", "notice": "This is a synthetic demo repository. All entries are disposable."}"#,
    )
    .into_diagnostic()?;

    // Commit initial files so they exist in git; the first real cycle must have
    // something to modify. This first commit also exercises the hook flow in
    // an empty repo, creating the initial ledger entry.
    run_git(&demo_dir, &["add", "-A"])?;
    run_git(
        &demo_dir,
        &[
            "commit",
            "-m",
            "chore(demo): scaffold invoice-service repository\n\nInitial synthetic files for the Ledgerful demo flow.\n\n[DEMO]",
        ],
    )?;

    let cycles = build_cycles();

    for (idx, cycle) in cycles.iter().enumerate() {
        println!(
            "{} Commit cycle {}/{}: {}",
            "[DEMO]".cyan().bold(),
            idx + 1,
            cycles.len(),
            cycle.conventional_subject
        );
        (cycle.modify)(&demo_dir)?;
        // In observe mode the commit-msg hook auto-drafts the ledger entry
        // from the conventional commit message and creates a pending sidecar.
        // The post-commit hook promotes it. This is the real onboarding flow
        // users experience: just `git commit` — Ledgerful handles the rest.
        run_commit(
            &demo_dir,
            cycle.conventional_subject,
            cycle.conventional_body,
        )?;
    }

    println!(
        "{} Running ledgerful verify --scope fast",
        "[DEMO]".cyan().bold()
    );
    run_ledger_verify_fast(&demo_dir)?;

    let export_path = demo_dir.join(DEMO_EVIDENCE_FILE);
    println!(
        "{} Exporting DEMO evidence to {}",
        "[DEMO]".cyan().bold(),
        export_path.display()
    );
    run_export(&demo_dir, &export_path)?;

    let export_path_str = export_path.to_string_lossy().to_string();

    // Restore original HOME/CWD before optional cleanup so cleanup uses the
    // caller's filesystem perspective.
    drop(_home_guard);

    if keep {
        println!(
            "\n{} Demo evidence export ready: {}",
            "SUCCESS:".green().bold(),
            export_path_str
        );
        println!(
            "{} Verify it offline with the public key in the export: {}",
            "Verifier:".cyan().bold(),
            format!(
                "cd {} && ledgerful verify --signatures --against-export {}",
                demo_dir.display(),
                export_path_str
            )
            .cyan()
        );
        println!(
            "{} Open the demo repo in the dashboard: {}",
            "Dashboard:".cyan().bold(),
            format!("cd {} && ledgerful web start", demo_dir.display()).cyan()
        );
    } else {
        println!(
            "\n{} Demo completed. Export was generated and cleaned up.",
            "SUCCESS:".green().bold()
        );
        println!(
            "{} Re-run with {} to inspect or verify the export.",
            "[DEMO]".cyan().bold(),
            "--keep".yellow().bold()
        );
    }
    println!(
        "{} Gate mode for this demo: {} (observe mode warns only; enforce would block).",
        "Notice:".yellow().bold(),
        "observe".yellow().bold()
    );

    if keep {
        println!(
            "{} Demo repo kept at: {}",
            "[DEMO]".cyan().bold(),
            demo_dir.display()
        );
    } else {
        println!(
            "{} Cleaning up demo repo at {}",
            "[DEMO]".cyan().bold(),
            demo_dir.display()
        );
        std::fs::remove_dir_all(&demo_dir).into_diagnostic()?;
        println!(
            "{} Demo repo removed. Re-run with --keep to inspect it.",
            "[DEMO]".cyan().bold()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_cycles_has_five_steps() {
        assert_eq!(build_cycles().len(), 5);
    }

    #[test]
    fn resolve_demo_dir_creates_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("demo-test");
        let resolved = resolve_demo_dir(Some(dir.clone()), false, tmp.path()).unwrap();
        assert_eq!(resolved, dir);
        assert!(dir.exists());
    }

    #[test]
    fn resolve_demo_dir_refuses_non_empty_without_force() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("demo-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("file.txt"), "x").unwrap();
        let err = resolve_demo_dir(Some(dir.clone()), false, tmp.path())
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"));
    }

    #[test]
    fn resolve_demo_dir_overwrites_non_empty_with_force() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("demo-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("file.txt"), "x").unwrap();
        let resolved = resolve_demo_dir(Some(dir.clone()), true, tmp.path()).unwrap();
        assert_eq!(resolved, dir);
        assert!(!dir.join("file.txt").exists());
    }
}
