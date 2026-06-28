use crate::git::ignore::add_to_gitignore;
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use std::env;
use std::fs;
use std::process::Command;
use tracing::info;

pub fn execute_update(
    migrate: bool,
    binary: bool,
    force: bool,
    force_unlock: bool,
    fast: bool,
    dry_run: bool,
    repair_hooks: bool,
) -> Result<()> {
    if !migrate && !binary && !repair_hooks {
        println!(
            "{} Specify what to update (e.g. --migrate, --binary, or --repair-hooks)",
            "HINT:".yellow().bold()
        );
        return Ok(());
    }

    if migrate {
        execute_migration(fast, dry_run)?;
    }

    if binary {
        execute_binary_update(force, force_unlock, dry_run)?;
    }

    if repair_hooks {
        crate::commands::hook_repair::execute_hook_repair(dry_run)?;
    }

    Ok(())
}

fn execute_migration(_fast: bool, dry_run: bool) -> Result<()> {
    if dry_run {
        println!(
            "{} Would migrate repository state (perform full re-indexing and schema migration).",
            "DRY-RUN".yellow().bold()
        );
        return Ok(());
    }

    println!("{} Migrating repository state...", "INIT".cyan().bold());

    // 1. Re-index
    crate::commands::index::execute_index(crate::commands::index::IndexArgs {
        incremental: false,
        analyze_graph: true,
        docs: true,
        contracts: true,
        semantic: false,
        scip: None,
        auto_scip: false,
        export_docs: false,

        doc_type: None,
        check: false,
        json: false,
        strict: false,
        concurrency: None,
        semantic_dry_run: None,
        fast: false,
        repair_metadata: false,
        dry_run: false,
        yes: false,
    })?;

    println!("{} Migration complete.", "DONE".green().bold());
    Ok(())
}

fn execute_binary_update(force: bool, force_unlock: bool, dry_run: bool) -> Result<()> {
    let bin_path = env::current_exe().unwrap_or_else(|e| {
        tracing::warn!(
            "Failed to get current executable path: {}. Falling back to 'ledgerful'.",
            e
        );
        std::path::PathBuf::from(if cfg!(windows) {
            "ledgerful.exe"
        } else {
            "ledgerful"
        })
    });
    let display_path = bin_path.display().to_string();

    if dry_run {
        println!(
            "{} Would replace binary at {} with current source build (cargo install --path .).",
            "DRY-RUN".yellow().bold(),
            display_path.cyan()
        );
        return Ok(());
    }

    if force_unlock {
        crate::platform::process_policy::force_unlock_processes()?;
    }

    let is_local_repo = fs::metadata("Cargo.toml").is_ok();
    if !is_local_repo {
        return Err(miette::miette!(
            "Binary update currently only supported from within the Ledgerful source repository."
        ));
    }

    println!(
        "Replacing {} with current source build...",
        display_path.cyan()
    );
    info!("Running 'cargo install --path .'");

    // Check if the target binary is locked before starting the build
    if std::fs::OpenOptions::new()
        .write(true)
        .open(&bin_path)
        .is_err()
    {
        println!(
            "{}",
            "Warning: Ledgerful binary is currently locked by another process.".yellow()
        );
        println!("Please close any other running instances or daemon processes before continuing.");
        println!("(Attempting shadow-copy anyway...)");
    }

    // --- H4: Windows shadow-copy ---
    // On Windows, we cannot overwrite a running executable. Even if
    // `cargo install` builds a new one, it will fail to copy it to the
    // destination if the destination executable is locked by the OS. We rename it to
    // `<name>.old` so that the file handle is still valid (Windows allows
    // renaming locked files) but the path is free for the new binary.
    let old_path_opt = shadow_copy_current_exe();

    let mut cmd = cargo_install_command();
    cmd.args(["install", "--path", "."]);

    if force {
        cmd.arg("--force");
    }

    let status = cmd.status().into_diagnostic()?;

    if status.success() {
        println!("{} Ledgerful updated successfully.", "DONE".green().bold());
        if let Some(old_path) = old_path_opt {
            info!("Stale binary moved to: {}", old_path.display());
            println!(
                "{} Stale binary will be cleaned up on next startup.",
                "INFO:".blue().bold()
            );
        }
    } else {
        // If update failed, try to restore the old binary name if we moved it
        if let Some(old_path) = old_path_opt {
            let _ = fs::rename(old_path, bin_path);
        }
        return Err(miette::miette!("Update failed. See above for errors."));
    }

    Ok(())
}

fn cargo_install_command() -> Command {
    #[cfg(debug_assertions)]
    if let Some(path) = env::var_os("LEDGERFUL_TEST_CARGO_COMMAND") {
        return Command::new(path);
    }
    Command::new("cargo")
}

fn shadow_copy_current_exe() -> Option<std::path::PathBuf> {
    if let Ok(bin_path) = env::current_exe() {
        let mut old_path = bin_path.clone();
        let stem = bin_path.file_stem()?.to_string_lossy();
        let extension = bin_path.extension()?.to_string_lossy();
        old_path.set_file_name(format!("{}.old.{}", stem, extension));

        if fs::rename(&bin_path, &old_path).is_ok() {
            return Some(old_path);
        }
    }
    None
}

pub fn validate_gitignore(root: &camino::Utf8Path) -> Result<()> {
    let patterns = [
        ".ledgerful/tmp",
        ".ledgerful/logs",
        ".ledgerful/state/ledger.db-shm",
        ".ledgerful/state/ledger.db-wal",
        "output/",
    ];

    for pattern in patterns {
        add_to_gitignore(root, pattern)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    struct TempDirCanon {
        _dir: tempfile::TempDir,
        path: std::path::PathBuf,
    }
    impl TempDirCanon {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dunce::canonicalize(dir.path()).unwrap();
            Self { _dir: dir, path }
        }
        fn path(&self) -> &std::path::Path {
            &self.path
        }
    }

    use super::*;

    #[test]
    fn test_validate_gitignore_adds_missing() {
        let tmp = TempDirCanon::new();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();

        validate_gitignore(root).unwrap();

        let ignore_path = root.join(".gitignore");
        let content = fs::read_to_string(ignore_path).unwrap();
        assert!(content.contains(".ledgerful/tmp"));
        assert!(content.contains("output/"));
    }

    #[test]
    fn test_validate_gitignore_idempotent() {
        let tmp = TempDirCanon::new();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let ignore_path = root.join(".gitignore");
        fs::write(&ignore_path, ".ledgerful/tmp\n").unwrap();

        validate_gitignore(root).unwrap();

        let content = fs::read_to_string(&ignore_path).unwrap();
        let count = content.matches(".ledgerful/tmp").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_execute_update_no_flags_prints_hint() {
        // Without any flag, execute_update should print the hint and not error.
        let result = execute_update(false, false, false, false, false, false, false);
        assert!(result.is_ok(), "no-flag invocation should succeed");
    }
}
