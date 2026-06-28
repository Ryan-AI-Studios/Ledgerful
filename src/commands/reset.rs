use crate::state::layout::Layout;
use camino::{Utf8Path, Utf8PathBuf};
use miette::{Result, miette};
use owo_colors::OwoColorize;
use std::env;
use std::fs;

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemovalOutcome {
    Removed,
    Absent,
    Preserved,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResetItem {
    path: Utf8PathBuf,
    outcome: RemovalOutcome,
}

pub fn execute_reset(
    remove_config: bool,
    remove_rules: bool,
    include_ledger: bool,
    remove_all: bool,
    confirm_destructive: bool,
    dry_run: bool,
) -> Result<()> {
    if (remove_all || remove_config || remove_rules || include_ledger)
        && !confirm_destructive
        && !dry_run
    {
        return Err(miette!(
            "Destructive reset options require confirmation. Re-run with '--yes'."
        ));
    }

    let current_dir =
        env::current_dir().map_err(|e| miette!("Failed to get current directory: {e}"))?;
    let root = Utf8PathBuf::from_path_buf(current_dir)
        .map_err(|path| miette!("Current directory is not valid UTF-8: {:?}", path))?;
    let layout = Layout::new(root.as_str());

    // 1. Generate and print reset plan preview
    let plan_items = reset_plan_items(
        &layout,
        remove_config,
        remove_rules,
        include_ledger,
        remove_all,
    );

    println!("Reset Operation Plan Preview:");
    for item in &plan_items {
        let label = match &item.outcome {
            RemovalOutcome::Removed => "would remove".yellow().to_string(),
            RemovalOutcome::Absent => "absent (no action)".dimmed().to_string(),
            RemovalOutcome::Preserved => "preserved".green().to_string(),
            RemovalOutcome::Failed(_) => "failed validation".red().to_string(),
        };
        println!("  {:<25} : {}", label, item.path);
    }

    if dry_run {
        println!("\nDry-run completed. No files were modified.");
        return Ok(());
    }

    // 2. Perform actual reset
    println!("\nExecuting reset plan...");
    let mut items = if remove_all {
        vec![remove_path(
            layout.state_dir.clone(),
            &layout.state_dir,
            false,
        )]
    } else {
        default_reset_items(&layout, remove_config, remove_rules, include_ledger, false)
    };
    items.sort_by(|a, b| a.path.cmp(&b.path));

    print_summary(&items);

    let failures: Vec<String> = items
        .iter()
        .filter_map(|item| match &item.outcome {
            RemovalOutcome::Failed(reason) => Some(format!("{}: {}", item.path, reason)),
            _ => None,
        })
        .collect();

    if failures.is_empty() {
        Ok(())
    } else {
        Err(miette!(
            "Reset completed with failures:\n{}",
            failures.join("\n")
        ))
    }
}

fn default_reset_items(
    layout: &Layout,
    remove_config: bool,
    remove_rules: bool,
    include_ledger: bool,
    dry_run: bool,
) -> Vec<ResetItem> {
    let mut items = vec![
        remove_path(layout.logs_dir(), &layout.state_dir, dry_run),
        remove_path(layout.tmp_dir(), &layout.state_dir, dry_run),
        remove_path(layout.reports_dir(), &layout.state_dir, dry_run),
        // Derived state files (cache, rebuildable) — always removed
        remove_path(
            layout.state_subdir().join("current-batch.json"),
            &layout.state_dir,
            dry_run,
        ),
        remove_path(
            layout.state_subdir().join("snapshots"),
            &layout.state_dir,
            dry_run,
        ),
        // Durable user data — preserved by default, removed only with --include-ledger
        maybe_preserve_or_remove(
            layout.state_subdir().join("ledger.db"),
            &layout.state_dir,
            include_ledger,
            dry_run,
        ),
        maybe_preserve_or_remove(
            layout.state_subdir().join("ledger.db-wal"),
            &layout.state_dir,
            include_ledger,
            dry_run,
        ),
        maybe_preserve_or_remove(
            layout.state_subdir().join("ledger.db-shm"),
            &layout.state_dir,
            include_ledger,
            dry_run,
        ),
    ];

    items.push(maybe_preserve_or_remove(
        layout.config_file(),
        &layout.state_dir,
        remove_config,
        dry_run,
    ));
    items.push(maybe_preserve_or_remove(
        layout.rules_file(),
        &layout.state_dir,
        remove_rules,
        dry_run,
    ));

    items
}

fn reset_plan_items(
    layout: &Layout,
    remove_config: bool,
    remove_rules: bool,
    include_ledger: bool,
    remove_all: bool,
) -> Vec<ResetItem> {
    let mut items = if remove_all {
        plan_path(layout.state_dir.clone(), &layout.state_dir)
    } else {
        default_plan_items(layout, remove_config, remove_rules, include_ledger)
    };
    items.sort_by(|a, b| a.path.cmp(&b.path));
    items
}

fn default_plan_items(
    layout: &Layout,
    remove_config: bool,
    remove_rules: bool,
    include_ledger: bool,
) -> Vec<ResetItem> {
    let mut items = Vec::new();
    items.extend(plan_path(layout.logs_dir(), &layout.state_dir));
    items.extend(plan_path(layout.tmp_dir(), &layout.state_dir));
    items.extend(plan_path(layout.reports_dir(), &layout.state_dir));
    items.extend(plan_path(
        layout.state_subdir().join("current-batch.json"),
        &layout.state_dir,
    ));
    items.extend(plan_path(
        layout.state_subdir().join("snapshots"),
        &layout.state_dir,
    ));
    items.extend(maybe_preserve_or_plan(
        layout.state_subdir().join("ledger.db"),
        &layout.state_dir,
        include_ledger,
    ));
    items.extend(maybe_preserve_or_plan(
        layout.state_subdir().join("ledger.db-wal"),
        &layout.state_dir,
        include_ledger,
    ));
    items.extend(maybe_preserve_or_plan(
        layout.state_subdir().join("ledger.db-shm"),
        &layout.state_dir,
        include_ledger,
    ));
    items.extend(maybe_preserve_or_plan(
        layout.config_file(),
        &layout.state_dir,
        remove_config,
    ));
    items.extend(maybe_preserve_or_plan(
        layout.rules_file(),
        &layout.state_dir,
        remove_rules,
    ));
    items
}

fn maybe_preserve_or_plan(
    path: Utf8PathBuf,
    state_root: &Utf8Path,
    should_remove: bool,
) -> Vec<ResetItem> {
    if should_remove {
        plan_path(path, state_root)
    } else {
        vec![ResetItem {
            path,
            outcome: RemovalOutcome::Preserved,
        }]
    }
}

fn plan_path(path: Utf8PathBuf, state_root: &Utf8Path) -> Vec<ResetItem> {
    let mut items = Vec::new();
    collect_plan_path(path, state_root, &mut items);
    items
}

fn collect_plan_path(path: Utf8PathBuf, state_root: &Utf8Path, items: &mut Vec<ResetItem>) {
    let outcome = match validate_target(&path, state_root) {
        Err(err) => RemovalOutcome::Failed(err),
        Ok(()) => match fs::symlink_metadata(&path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => RemovalOutcome::Absent,
            Err(err) => RemovalOutcome::Failed(err.to_string()),
            Ok(metadata) => {
                if metadata.is_dir() && !metadata.file_type().is_symlink() {
                    match fs::read_dir(&path) {
                        Ok(entries) => {
                            for entry in entries {
                                match entry {
                                    Ok(entry) => match Utf8PathBuf::from_path_buf(entry.path()) {
                                        Ok(child) => collect_plan_path(child, state_root, items),
                                        Err(path) => items.push(ResetItem {
                                            path: path.to_string_lossy().into_owned().into(),
                                            outcome: RemovalOutcome::Failed(
                                                "encountered non-UTF-8 path during reset preview"
                                                    .to_string(),
                                            ),
                                        }),
                                    },
                                    Err(err) => items.push(ResetItem {
                                        path: path.clone(),
                                        outcome: RemovalOutcome::Failed(err.to_string()),
                                    }),
                                }
                            }
                            RemovalOutcome::Removed
                        }
                        Err(err) => RemovalOutcome::Failed(err.to_string()),
                    }
                } else {
                    RemovalOutcome::Removed
                }
            }
        },
    };

    items.push(ResetItem { path, outcome });
}

fn maybe_preserve_or_remove(
    path: Utf8PathBuf,
    state_root: &Utf8Path,
    should_remove: bool,
    dry_run: bool,
) -> ResetItem {
    if should_remove {
        remove_path(path, state_root, dry_run)
    } else {
        ResetItem {
            path,
            outcome: RemovalOutcome::Preserved,
        }
    }
}

fn remove_path(path: Utf8PathBuf, state_root: &Utf8Path, dry_run: bool) -> ResetItem {
    let outcome = match validate_target(&path, state_root) {
        Err(err) => RemovalOutcome::Failed(err),
        Ok(()) => match fs::symlink_metadata(&path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => RemovalOutcome::Absent,
            Err(err) => RemovalOutcome::Failed(err.to_string()),
            Ok(metadata) => {
                if dry_run {
                    RemovalOutcome::Removed
                } else if let Err(err) = clear_readonly_recursive(&path) {
                    RemovalOutcome::Failed(err.to_string())
                } else {
                    let result = if metadata.file_type().is_dir() {
                        fs::remove_dir_all(&path)
                    } else {
                        fs::remove_file(&path)
                    };

                    match result {
                        Ok(()) => RemovalOutcome::Removed,
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                            RemovalOutcome::Absent
                        }
                        Err(err) => RemovalOutcome::Failed(err.to_string()),
                    }
                }
            }
        },
    };

    ResetItem { path, outcome }
}

fn validate_target(path: &Utf8Path, state_root: &Utf8Path) -> std::result::Result<(), String> {
    if path == state_root || path.starts_with(state_root) {
        Ok(())
    } else {
        Err(format!("refusing to remove path outside {}", state_root))
    }
}

fn clear_readonly_recursive(path: &Utf8Path) -> std::io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };

    if metadata.file_type().is_symlink() {
        return Ok(());
    }

    let mut permissions = metadata.permissions();
    if permissions.readonly() {
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions)?;
    }

    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let child = Utf8PathBuf::from_path_buf(entry.path())
                .map_err(|_| std::io::Error::other("encountered non-UTF-8 path during reset"))?;
            clear_readonly_recursive(&child)?;
        }
    }

    Ok(())
}

fn print_summary(items: &[ResetItem]) {
    for item in items {
        let label = match &item.outcome {
            RemovalOutcome::Removed => "removed",
            RemovalOutcome::Absent => "absent",
            RemovalOutcome::Preserved => "preserved",
            RemovalOutcome::Failed(_) => "failed",
        };
        println!("{label}: {}", item.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_target_rejects_outside_path() {
        let state_root = Utf8Path::new("repo/.ledgerful");
        let other = Utf8Path::new("repo/src");
        assert!(validate_target(other, state_root).is_err());
    }

    #[test]
    fn reset_all_plan_lists_nested_files_before_removal() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let layout = Layout::new(&root);
        layout.ensure_state_dir().unwrap();
        fs::write(layout.config_file(), "config").unwrap();
        fs::write(layout.state_subdir().join("ledger.db"), "ledger").unwrap();
        fs::write(layout.logs_dir().join("watch.log"), "log").unwrap();

        let plan = reset_plan_items(&layout, false, false, false, true);
        let paths = plan
            .iter()
            .map(|item| item.path.as_str().replace('\\', "/"))
            .collect::<Vec<_>>();

        assert!(
            paths
                .iter()
                .any(|path| path.ends_with(".ledgerful/config.toml"))
        );
        assert!(
            paths
                .iter()
                .any(|path| path.ends_with(".ledgerful/state/ledger.db"))
        );
        assert!(
            paths
                .iter()
                .any(|path| path.ends_with(".ledgerful/logs/watch.log"))
        );
    }
}
