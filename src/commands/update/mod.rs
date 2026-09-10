use crate::git::ignore::add_to_gitignore;
use miette::Result;
use owo_colors::{OwoColorize, Stream, Style};

mod binary;

const NO_FLAG_HINT: &str = "\
Specify what to update:
  --binary         Latest published archive, or cargo from this engine tree
  --migrate        Full re-index + schema upgrade
  --repair-hooks   Rewrite retired hook commands to ledgerful
Preview: add --dry-run or --check (alias; e.g. update --binary --check)";

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
            "{} {NO_FLAG_HINT}",
            "HINT:".if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
        );
        return Ok(());
    }

    if migrate {
        execute_migration(fast, dry_run)?;
    }

    if binary {
        binary::execute_binary_update(force, force_unlock, dry_run)?;
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
            "DRY-RUN".if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
        );
        return Ok(());
    }

    println!(
        "{} Migrating repository state...",
        "INIT".if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold()))
    );

    crate::commands::index::execute_index(crate::commands::index::IndexArgs {
        incremental: false,
        full: false,
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

    println!(
        "{} Migration complete.",
        "DONE".if_supports_color(Stream::Stdout, |s| s.style(Style::new().green().bold()))
    );
    Ok(())
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
    use super::*;
    use std::fs;

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
        let result = execute_update(false, false, false, false, false, false, false);
        assert!(result.is_ok(), "no-flag invocation should succeed");
    }

    #[test]
    fn update_binary_hint_not_source_tree_only() {
        assert!(
            !NO_FLAG_HINT.contains("from this source tree"),
            "no-flag hint must not claim source-tree-only: {NO_FLAG_HINT}"
        );
        assert!(
            NO_FLAG_HINT.contains("Latest published archive, or cargo from this engine tree"),
            "no-flag hint must name Latest + engine cargo: {NO_FLAG_HINT}"
        );
    }
}
