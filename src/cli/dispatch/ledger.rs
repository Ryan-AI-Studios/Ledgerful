use crate::cli::args::{LedgerCommands, RegisterCommands};
use crate::commands::ledger::LedgerStatusOpts;
use miette::Result;

use super::export::strip_verbatim_prefix;
use super::load_user_config;

pub(super) fn dispatch_ledger(command: LedgerCommands) -> Result<()> {
    match command {
        LedgerCommands::Start {
            entity,
            category,
            message,
            force,
        } => crate::commands::ledger::execute_ledger_start(
            entity,
            &category.to_string(),
            &message,
            force,
        ),
        LedgerCommands::Commit {
            tx_id,
            summary,
            reason,
            breaking,
            force,
            with_git,
            git_message,
            no_signoff,
            dry_run,
        } => crate::commands::ledger::execute_ledger_commit(
            tx_id,
            &summary,
            &reason,
            breaking,
            force,
            crate::commands::ledger::LedgerCommitGitOptions {
                with_git,
                git_message,
                signoff: !no_signoff,
                dry_run,
            },
        ),
        LedgerCommands::Rollback { tx_id, reason } => {
            crate::commands::ledger::execute_ledger_rollback(tx_id, reason)
        }
        LedgerCommands::Atomic {
            entity,
            category,
            summary,
            reason,
            force,
        } => crate::commands::ledger::execute_ledger_atomic(
            &entity,
            &category.to_string(),
            &summary,
            &reason,
            force,
        ),
        LedgerCommands::Status {
            all,
            entity,
            compact,
            exit_code,
            strict_observe_signal,
            verify_signatures,
            json,
            global,
            repo,
            reindex,
            opt_out,
            opt_in,
        } => {
            if opt_out {
                crate::state::rollup::set_global_rollup_enabled(false)?;
            }
            if opt_in {
                crate::state::rollup::set_global_rollup_enabled(true)?;
            }

            if global {
                let user_config = load_user_config()?;
                crate::state::rollup::execute_ledger_status_global(
                    &user_config.global_rollup,
                    repo.as_deref(),
                    reindex,
                    json,
                )
            } else {
                crate::commands::ledger::execute_ledger_status(LedgerStatusOpts {
                    entity_filter: entity,
                    compact,
                    exit_code,
                    verify_signatures,
                    json,
                    all,
                    strict_observe_signal,
                })
            }
        }
        LedgerCommands::RecoverOrphan {
            promote,
            abandon,
            reason,
        } => crate::commands::ledger::execute_ledger_recover_orphan(promote, abandon, reason),
        LedgerCommands::Register { command } => match command {
            RegisterCommands::Rule {
                term,
                category,
                reason,
            } => crate::commands::ledger::execute_ledger_register_rule(
                &term,
                &category.to_string(),
                &reason,
            ),
            RegisterCommands::Validator {
                name,
                command,
                category,
                timeout,
            } => crate::commands::ledger::execute_ledger_register_validator(
                &name, &command, &category, timeout,
            ),
        },
        LedgerCommands::Stack { category } => {
            crate::commands::ledger_stack::execute_ledger_stack(category.map(|c| c.to_string()))
        }
        LedgerCommands::Adr { command } => crate::commands::ledger_adr::execute_ledger_adr(command),
        LedgerCommands::Validator { command } => {
            crate::commands::ledger_register::execute_validator_lifecycle(command)
        }
        LedgerCommands::Graph(args) => crate::commands::ledger_graph::execute_ledger_graph(args),
        LedgerCommands::Search {
            query,
            category,
            days,
            breaking,
            limit,
            offset,
            json,
            include_rollback,
        } => crate::commands::ledger_search::execute_ledger_search(
            query,
            category,
            days,
            breaking,
            limit,
            offset,
            json,
            include_rollback,
        ),
        LedgerCommands::Reconcile {
            tx_id,
            pattern,
            all,
            reason,
        } => crate::commands::ledger::execute_ledger_reconcile(tx_id, pattern, all, reason),
        LedgerCommands::Adopt {
            pattern,
            all,
            category,
            summary,
            reason,
        } => crate::commands::ledger::execute_ledger_adopt(
            pattern,
            all,
            &category.to_string(),
            &summary,
            &reason,
        ),
        LedgerCommands::Audit {
            entity,
            pos_entity,
            include_unaudited,
            limit,
            offset,
            json,
        } => crate::commands::ledger_audit::execute_ledger_audit(
            entity.or(pos_entity),
            include_unaudited,
            limit,
            offset,
            json,
        ),
        LedgerCommands::Note {
            entity,
            note,
            message,
        } => crate::commands::ledger::execute_ledger_note(&entity, note, message),
        LedgerCommands::ReSign {
            tx,
            all_invalid,
            all,
            dry_run,
            yes,
        } => crate::commands::ledger_re_sign::execute_ledger_re_sign(
            tx,
            all_invalid,
            all,
            dry_run,
            yes,
        ),
        LedgerCommands::Gc {
            stale,
            orphans,
            ttl_hours,
            force,
            dry_run,
        } => crate::commands::ledger::execute_ledger_gc(stale, orphans, ttl_hours, force, dry_run),
        LedgerCommands::Resume { tx_id } => crate::commands::ledger::execute_ledger_resume(tx_id),
        LedgerCommands::ExportProvenance { out_path, force } => {
            dispatch_ledger_export_provenance(out_path, force)
        }
        LedgerCommands::ExportPublic { output, sign, key } => {
            dispatch_ledger_export_public(output, sign, key)
        }
        LedgerCommands::HookRepair { force } => {
            crate::commands::ledger::execute_ledger_hook_repair(force)
        }
    }
}

pub(super) fn dispatch_ledger_export_provenance(
    out_path: Option<std::path::PathBuf>,
    force: bool,
) -> Result<()> {
    use camino::Utf8PathBuf;

    let Some(path) = out_path else {
        return crate::commands::ledger::execute_ledger_export_provenance(None);
    };

    let clean = validate_export_path(&path, force)?;
    let utf8 = Utf8PathBuf::from_path_buf(clean)
        .map_err(|_| miette::miette!("export path is not valid UTF-8"))?;
    crate::commands::ledger::execute_ledger_export_provenance(Some(utf8.to_string()))
}

pub(super) fn dispatch_ledger_export_public(
    output: std::path::PathBuf,
    sign: bool,
    key: Option<std::path::PathBuf>,
) -> Result<()> {
    let clean = validate_export_public_path(&output)?;
    let options = crate::ledger::ExportOptions {
        output: &clean,
        sign,
        key: key.as_deref(),
    };
    crate::commands::ledger::execute_ledger_export_public(options)
}

pub(super) fn validate_export_public_path(
    path: &std::path::Path,
) -> miette::Result<std::path::PathBuf> {
    let repo_root = crate::commands::helpers::get_repo_root()
        .map_err(|e| miette::miette!("failed to determine repository root: {e}"))?;

    let absolute = std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|e| miette::miette!("failed to determine current directory: {e}"))?;
    let cleaned = path_clean::PathClean::clean(&absolute);

    // For directory output, the target itself must have a directory name component.
    let file_name_os = cleaned.file_name();
    let file_name_valid = file_name_os
        .and_then(|n| n.to_str().map(|s| !s.is_empty()))
        .unwrap_or(false);
    if !file_name_valid {
        return Err(miette::miette!(
            "invalid output directory: no directory name component"
        ));
    }

    let canonical = if cleaned.exists() {
        let meta = std::fs::metadata(&cleaned)
            .map_err(|e| miette::miette!("failed to inspect path: {e}"))?;
        if !meta.is_dir() {
            return Err(miette::miette!(
                "output path exists and is not a directory: {}",
                cleaned.display()
            ));
        }
        std::fs::canonicalize(&cleaned)
            .map_err(|e| miette::miette!("failed to resolve path: {e}"))?
    } else {
        match cleaned.parent() {
            Some(parent) => {
                let base = std::fs::canonicalize(parent)
                    .map_err(|e| miette::miette!("failed to resolve parent directory: {e}"))?;
                base.join(file_name_os.unwrap_or_default())
            }
            None => cleaned.clone(),
        }
    };

    let canonical = strip_verbatim_prefix(&canonical);

    let canonical_repo_root = std::fs::canonicalize(repo_root.as_std_path())
        .map_err(|e| miette::miette!("failed to resolve repo root: {e}"))?;
    let canonical_repo_root = strip_verbatim_prefix(&canonical_repo_root);

    let state_dir = strip_verbatim_prefix(&canonical_repo_root.join(".ledgerful").join("state"));
    if canonical.starts_with(&state_dir) {
        return Err(miette::miette!(
            "refusing to write public ledger bundle inside .ledgerful/state/"
        ));
    }

    let src_dir = strip_verbatim_prefix(&canonical_repo_root.join("src"));
    if canonical.starts_with(&src_dir) {
        return Err(miette::miette!(
            "refusing to write public ledger bundle inside src/"
        ));
    }

    if !canonical.exists() {
        std::fs::create_dir_all(&canonical)
            .map_err(|e| miette::miette!("failed to create output directory: {e}"))?;
    }

    Ok(canonical)
}

pub(super) fn validate_export_path(
    path: &std::path::Path,
    force: bool,
) -> miette::Result<std::path::PathBuf> {
    let repo_root = crate::commands::helpers::get_repo_root()
        .map_err(|e| miette::miette!("failed to determine repository root: {e}"))?;

    let absolute = std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|e| miette::miette!("failed to determine current directory: {e}"))?;
    let cleaned = path_clean::PathClean::clean(&absolute);

    // Reject paths that escape the repository root (e.g. "../foo.json").
    let repo_root_std = repo_root.as_std_path();
    if cleaned != repo_root_std && !cleaned.starts_with(repo_root_std) {
        return Err(miette::miette!(
            "export path must be inside the repository ({})",
            repo_root_std.display()
        ));
    }

    // Reject directory-only targets (e.g. "src/..") and paths with no file component.
    let file_name_os = cleaned.file_name();
    let file_name_valid = file_name_os
        .and_then(|n| n.to_str().map(|s| !s.is_empty()))
        .unwrap_or(false);
    if cleaned.is_dir() || !file_name_valid {
        return Err(miette::miette!(
            "invalid path: no file component; must specify a file, not a directory"
        ));
    }

    // Resolve symlinks/junctions for the safety-boundary comparison.
    let canonical = if cleaned.exists() {
        std::fs::canonicalize(&cleaned)
            .map_err(|e| miette::miette!("failed to resolve path: {e}"))?
    } else {
        match cleaned.parent() {
            Some(parent) => {
                let base = std::fs::canonicalize(parent)
                    .map_err(|e| miette::miette!("failed to resolve parent directory: {e}"))?;
                base.join(file_name_os.unwrap_or_default())
            }
            None => cleaned.clone(),
        }
    };

    let canonical = strip_verbatim_prefix(&canonical);

    // Re-check the repository boundary after canonicalization so a repo-local
    // symlink/junction cannot resolve outside the repository.
    let canonical_repo_root = std::fs::canonicalize(repo_root_std)
        .map_err(|e| miette::miette!("failed to resolve repo root: {e}"))?;
    let canonical_repo_root = strip_verbatim_prefix(&canonical_repo_root);
    if canonical != canonical_repo_root && !canonical.starts_with(&canonical_repo_root) {
        return Err(miette::miette!(
            "export path resolves outside the repository after symlink resolution"
        ));
    }

    let cargo_toml_path = strip_verbatim_prefix(&canonical_repo_root.join("Cargo.toml"));
    if canonical == cargo_toml_path {
        return Err(miette::miette!("refusing to write to Cargo.toml"));
    }

    let src_dir = strip_verbatim_prefix(&canonical_repo_root.join("src"));
    if canonical.starts_with(&src_dir) {
        return Err(miette::miette!("refusing to write inside src/"));
    }

    let state_dir = strip_verbatim_prefix(&canonical_repo_root.join(".ledgerful").join("state"));
    if canonical.starts_with(&state_dir) {
        return Err(miette::miette!(
            "refusing to write inside .ledgerful/state/"
        ));
    }

    if canonical.exists() && !force {
        return Err(miette::miette!(
            "{} already exists; use --force to overwrite",
            canonical.display()
        ));
    }

    Ok(canonical)
}

#[cfg(test)]
mod export_path_tests {
    use super::super::export::validate_export_evidence_path;
    use super::*;
    use camino::Utf8Path;
    use tempfile::tempdir;

    struct CwdGuard {
        original: std::path::PathBuf,
    }

    impl CwdGuard {
        fn enter(path: &std::path::Path) -> Self {
            let original = std::env::current_dir().unwrap();
            std::env::set_current_dir(path).unwrap();
            CwdGuard { original }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original);
        }
    }

    fn temp_repo() -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap().to_path_buf();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&root)
            .output()
            .expect("git init failed");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".ledgerful").join("state")).unwrap();
        std::fs::File::create(root.join("Cargo.toml")).unwrap();
        (tmp, root)
    }

    fn temp_repo_root_only() -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap().to_path_buf();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&root)
            .output()
            .expect("git init failed");
        // No src/ and no .ledgerful/state/ so outside-repo paths have something
        // to compare against but do not accidentally hit forbidden dirs.
        (tmp, root)
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_accepts_valid_relative_file() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let path = validate_export_path(std::path::Path::new("out.json"), false).unwrap();
        // The file does not exist yet, so canonicalize its parent before
        // appending the file name. This resolves macOS /var -> /private/var
        // and Windows long-name -> 8.3 aliases the same way as production.
        let expected = strip_verbatim_prefix(&std::fs::canonicalize(root.as_std_path()).unwrap())
            .join("out.json");
        assert_eq!(path, expected);
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_accepts_existing_file_with_force() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());
        std::fs::File::create(root.join("existing.json")).unwrap();

        let path = validate_export_path(std::path::Path::new("existing.json"), true).unwrap();
        // Same 8.3 short-name workaround as above.
        let expected = std::fs::canonicalize(root.as_std_path().join("existing.json"))
            .unwrap_or_else(|_| root.as_std_path().join("existing.json"));
        let canonical_path = std::fs::canonicalize(&path).unwrap_or(path);
        assert_eq!(canonical_path, expected);
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_refuses_existing_file_without_force() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());
        std::fs::File::create(root.join("existing.json")).unwrap();

        let err = validate_export_path(std::path::Path::new("existing.json"), false)
            .unwrap_err()
            .to_string();

        assert!(err.contains("already exists"));
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_refuses_src_directory() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let err = validate_export_path(std::path::Path::new("src/foo.json"), false)
            .unwrap_err()
            .to_string();

        assert!(err.contains("inside src/"));
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_refuses_cargo_toml() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let err = validate_export_path(std::path::Path::new("Cargo.toml"), false)
            .unwrap_err()
            .to_string();

        assert!(err.contains("Cargo.toml"));
    }

    /// 0119 DoD: `export head` shares `validate_export_evidence_path` path-safety.
    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_evidence_path_refuses_cargo_toml() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let err = validate_export_evidence_path(std::path::Path::new("Cargo.toml"), false)
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("Cargo.toml"),
            "export evidence/head path safety must refuse Cargo.toml; got: {err}"
        );
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_refuses_state_directory() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let err = validate_export_path(std::path::Path::new(".ledgerful/state/foo.json"), false)
            .unwrap_err()
            .to_string();

        assert!(err.contains("inside .ledgerful/state/"));
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_refuses_dotdot_traversal() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let err = validate_export_path(std::path::Path::new("../src/foo.json"), false)
            .unwrap_err()
            .to_string();

        assert!(err.contains("inside the repository") || err.contains("inside src/"));
    }

    #[cfg(windows)]
    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_refuses_symlink_resolving_outside_repo() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        // Create a directory outside the repo and a symlink/junction inside the
        // repo that points to it. Windows requires elevated privileges for
        // directory symlinks, so we fall back to a junction when symlinking fails.
        let outside = root.as_std_path().join("../outside");
        std::fs::create_dir_all(&outside).unwrap();
        let outside_abs = std::fs::canonicalize(&outside).unwrap();
        let link_path = root.as_std_path().join("link_to_outside");

        let symlink_ok = std::os::windows::fs::symlink_dir(&outside_abs, &link_path);
        if symlink_ok.is_err() {
            let _ = std::fs::remove_dir_all(&link_path);
            // Junction fallback requires Windows-specific APIs; skip if unavailable.
            if std::process::Command::new("cmd")
                .args([
                    "/c",
                    "mklink",
                    "/J",
                    link_path.to_str().unwrap_or_default(),
                    outside_abs.to_str().unwrap_or_default(),
                ])
                .output()
                .map(|out| !out.status.success())
                .unwrap_or(true)
            {
                // Symlinks/junctions unavailable in this environment; skip.
                return;
            }
        }

        let err = validate_export_path(std::path::Path::new("link_to_outside/escaped.json"), false)
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("resolves outside the repository") || err.contains("inside src/"),
            "unexpected error: {err}"
        );
    }

    #[cfg(not(windows))]
    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_refuses_symlink_resolving_outside_repo() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let outside = root.as_std_path().join("../outside");
        std::fs::create_dir_all(&outside).unwrap();
        let outside_abs = std::fs::canonicalize(&outside).unwrap();
        let link_path = root.as_std_path().join("link_to_outside");

        if std::os::unix::fs::symlink(&outside_abs, &link_path).is_err() {
            return;
        }

        let err = validate_export_path(std::path::Path::new("link_to_outside/escaped.json"), false)
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("resolves outside the repository") || err.contains("inside src/"),
            "unexpected error: {err}"
        );
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_refuses_src_dotdot() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let err = validate_export_path(std::path::Path::new("src/.."), false)
            .unwrap_err()
            .to_string();

        assert!(err.contains("no file component") || err.contains("directory"));
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_refuses_absolute_path_in_src() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        // Match the namespace used by `get_repo_root()`. On Windows CI,
        // canonicalizing the temp root can instead produce an equivalent 8.3
        // alias such as `RUNNER~1`, which fails the earlier lexical boundary
        // check before this test reaches the protected `src/` check.
        let active_root = std::env::current_dir().unwrap();
        let err = validate_export_path(&active_root.join("src/foo.json"), false)
            .unwrap_err()
            .to_string();

        assert!(err.contains("inside src/"), "unexpected error: {err}");
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_path_rejects_directory_target() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());
        std::fs::create_dir_all(root.join("mydir")).unwrap();

        let err = validate_export_path(std::path::Path::new("mydir"), false)
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("already exists") || err.contains("directory"),
            "unexpected error: {err}"
        );
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_public_path_accepts_outside_repo() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let outside = root.as_std_path().join("..").join("public-output");
        let path = validate_export_public_path(outside.as_path()).unwrap();
        assert!(path.exists(), "output directory should be created");
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_public_path_accepts_existing_outside_directory() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let outside = root.as_std_path().join("..").join("existing-output");
        std::fs::create_dir_all(&outside).unwrap();
        let path = validate_export_public_path(outside.as_path()).unwrap();
        assert_eq!(
            path,
            strip_verbatim_prefix(&std::fs::canonicalize(&outside).unwrap())
        );
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_public_path_refuses_existing_file() {
        let (_tmp, root) = temp_repo_root_only();
        let _guard = CwdGuard::enter(root.as_std_path());

        let target = root.as_std_path().join("not-a-dir.json");
        std::fs::File::create(&target).unwrap();

        let err = validate_export_public_path(target.as_path())
            .unwrap_err()
            .to_string();

        assert!(err.contains("not a directory"), "unexpected error: {err}");
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_public_path_refuses_src_directory() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let err = validate_export_public_path(std::path::Path::new("src/bundle"))
            .unwrap_err()
            .to_string();

        assert!(err.contains("inside src/"), "unexpected error: {err}");
    }

    #[serial_test::serial(cwd)]
    #[test]
    fn validate_export_public_path_refuses_state_directory() {
        let (_tmp, root) = temp_repo();
        let _guard = CwdGuard::enter(root.as_std_path());

        let err = validate_export_public_path(std::path::Path::new(".ledgerful/state/bundle"))
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("inside .ledgerful/state/"),
            "unexpected error: {err}"
        );
    }
}
