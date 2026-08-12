//! Integration tests for `ledgerful symbols` (track 0163).
//!
//! Hermetic fixtures seed `project_files` / `project_symbols` without
//! `execute_init` (avoids re-migration races). Subprocess CLI uses `run_cli`
//! with an explicit root — no process-wide `DirGuard` (parallel-safe).

use crate::common::{git_add_and_commit, git_cmd, run_cli, setup_git_repo};
use camino::Utf8Path;
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn seed_symbols_fixture(root: &Path) {
    let root_utf8 = Utf8Path::from_path(root).expect("utf8 root");
    let layout = Layout::new(root_utf8);
    layout.ensure_state_dir().unwrap();

    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let conn = storage.get_connection();

    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES \
         (1, 'src/commands/foo.rs', '2026-01-01T00:00:00Z'), \
         (2, 'src/commands/bar.rs', '2026-01-01T00:00:00Z'), \
         (3, 'src/cli/args.rs', '2026-01-01T00:00:00Z'), \
         (4, 'tests/integration/cli.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    let rows = [
        (1i64, 1i64, "alpha", "Function", Some(10i64), 1i64, "alpha"),
        (2, 1, "beta", "Function", Some(20), 0, "beta"),
        (3, 2, "gamma", "Struct", Some(5), 1, "gamma"),
        (4, 2, "delta", "Function", Some(15), 1, "delta"),
        (5, 1, "epsilon", "Module", None, 1, "epsilon"),
        (6, 3, "cli_main", "Function", Some(1), 1, "cli_main"),
        (7, 4, "test_a", "Function", Some(1), 0, "test_a"),
        (8, 1, "twin", "Function", Some(99), 1, "a::twin"),
        (9, 1, "twin", "Function", Some(99), 1, "b::twin"),
    ];
    for (id, file_id, name, kind, line, is_pub, qn) in rows {
        conn.execute(
            "INSERT INTO project_symbols \
             (id, file_id, qualified_name, symbol_name, symbol_kind, is_public, \
              line_start, last_indexed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '2026-01-01T00:00:00Z')",
            rusqlite::params![id, file_id, qn, name, kind, is_pub, line],
        )
        .unwrap();
    }

    storage.shutdown().unwrap();
}

fn setup_seeded_repo() -> tempfile::TempDir {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join("src/commands")).unwrap();
    fs::write(root.join("src/commands/foo.rs"), "fn alpha() {}").unwrap();
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");
    seed_symbols_fixture(root);
    tmp
}

#[test]
fn symbols_json_schema_version_1_path_prefix() {
    let tmp = setup_seeded_repo();
    let root = tmp.path();

    let (stdout, stderr, code) = run_cli(
        root,
        &[
            "symbols",
            "--path",
            "src/commands",
            "--json",
            "--limit",
            "50",
        ],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    // F4: pure --json must not emit product banners on stderr (stale WARN gated).
    assert!(
        stderr.trim().is_empty(),
        "JSON mode stderr must be empty/whitespace; stderr={stderr:?}"
    );
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("pure JSON expected: {e}; stdout={stdout}");
    });
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["scope"]["path"], "src/commands");
    assert_eq!(v["scope"]["changed"], false);
    assert_eq!(v["scope"]["kind"], serde_json::Value::Null);
    assert_eq!(v["scope"]["pubOnly"], false);
    assert_eq!(v["limit"], 50);
    assert_eq!(v["totalMatching"], 7);
    assert_eq!(v["resultCount"], 7);
    assert_eq!(v["truncated"], false);
    assert!(v.get("indexStatus").is_none());
    let symbols = v["symbols"].as_array().unwrap();
    assert_eq!(symbols.len(), 7);
    assert!(
        symbols
            .iter()
            .all(|s| s["path"].as_str().unwrap().starts_with("src/commands"))
    );
}

#[test]
fn symbols_trailing_slash_path_matches() {
    let tmp = setup_seeded_repo();
    let root = tmp.path();

    let (stdout, stderr, code) = run_cli(root, &["symbols", "--path", "src/commands/", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["scope"]["path"], "src/commands");
    assert_eq!(v["totalMatching"], 7);
}

#[test]
fn symbols_kind_alias_canonical_scope() {
    let tmp = setup_seeded_repo();
    let root = tmp.path();

    let (stdout, stderr, code) = run_cli(
        root,
        &[
            "symbols",
            "--path",
            "src/commands",
            "--kind",
            "fn",
            "--json",
        ],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["scope"]["kind"], "Function");
    let symbols = v["symbols"].as_array().unwrap();
    assert!(!symbols.is_empty());
    assert!(symbols.iter().all(|s| s["kind"] == "Function"));
}

#[test]
fn symbols_unknown_kind_rejected() {
    let tmp = setup_seeded_repo();
    let root = tmp.path();

    let (_stdout, stderr, code) = run_cli(root, &["symbols", "--kind", "notakind", "--json"]);
    assert_ne!(code, 0, "unknown kind must fail");
    assert!(
        stderr.to_lowercase().contains("unknown") || stderr.to_lowercase().contains("kind"),
        "stderr={stderr}"
    );
}

#[test]
fn symbols_pub_and_limit_truncate_true_count() {
    let tmp = setup_seeded_repo();
    let root = tmp.path();

    let (stdout, stderr, code) = run_cli(
        root,
        &[
            "symbols",
            "--path",
            "src/commands",
            "--pub",
            "--limit",
            "2",
            "--json",
        ],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["scope"]["pubOnly"], true);
    assert_eq!(v["limit"], 2);
    assert_eq!(v["resultCount"], 2);
    // public under src/commands: alpha, gamma, delta, epsilon, twin×2 = 6 (beta private)
    assert_eq!(v["totalMatching"], 6);
    assert_eq!(v["truncated"], true);
    let symbols = v["symbols"].as_array().unwrap();
    assert_eq!(symbols.len(), 2);
    assert!(symbols.iter().all(|s| s["isPublic"] == true));
}

#[test]
fn symbols_sort_tiebreak_qualified_name() {
    let tmp = setup_seeded_repo();
    let root = tmp.path();

    let (stdout, stderr, code) = run_cli(
        root,
        &[
            "symbols",
            "--path",
            "src/commands/foo.rs",
            "--kind",
            "Function",
            "--json",
        ],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let twins: Vec<_> = v["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["name"] == "twin")
        .collect();
    assert_eq!(twins.len(), 2);
    assert_eq!(twins[0]["qualifiedName"], "a::twin");
    assert_eq!(twins[1]["qualifiedName"], "b::twin");
}

#[test]
fn symbols_changed_clean_empty() {
    let tmp = setup_seeded_repo();
    let root = tmp.path();

    let (stdout, stderr, code) = run_cli(root, &["symbols", "--changed", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["scope"]["changed"], true);
    assert_eq!(v["totalMatching"], 0);
    assert_eq!(v["resultCount"], 0);
    assert!(v["symbols"].as_array().unwrap().is_empty());
}

#[test]
fn symbols_changed_dirty_and_path_intersection() {
    let tmp = setup_seeded_repo();
    let root = tmp.path();

    fs::write(
        root.join("src/commands/foo.rs"),
        "fn alpha() { /* dirty */ }",
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli(root, &["symbols", "--changed", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["scope"]["changed"], true);
    assert!(
        v["totalMatching"].as_u64().unwrap() >= 1,
        "dirty file should surface symbols; envelope={v}"
    );
    assert!(
        v["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["path"].as_str().unwrap().contains("foo.rs")),
        "changed-only should not include unrelated paths; envelope={v}"
    );

    let (stdout2, stderr2, code2) = run_cli(
        root,
        &["symbols", "--changed", "--path", "src/cli", "--json"],
    );
    assert_eq!(code2, 0, "stderr={stderr2}");
    let v2: serde_json::Value = serde_json::from_str(stdout2.trim()).unwrap();
    assert_eq!(v2["totalMatching"], 0);
    assert!(v2["symbols"].as_array().unwrap().is_empty());
}

/// Codex R2 P2: rename keeps old indexed path in --changed membership until re-index.
#[test]
fn symbols_changed_rename_includes_old_path() {
    let tmp = setup_seeded_repo();
    let root = tmp.path();

    // Prefer git mv so status can surface Renamed { old_path }.
    git_cmd(
        root,
        &["mv", "src/commands/foo.rs", "src/commands/foo_renamed.rs"],
    );

    let (stdout, stderr, code) = run_cli(root, &["symbols", "--changed", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(
        v["totalMatching"].as_u64().unwrap() >= 1,
        "rename must still match symbols under pre-index path; envelope={v}"
    );
    let paths: Vec<&str> = v["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["path"].as_str().unwrap())
        .collect();
    assert!(
        paths.iter().any(|p| p.contains("foo.rs")),
        "expected old indexed path foo.rs in results; paths={paths:?}; envelope={v}"
    );
}

#[test]
fn symbols_missing_db_empty_index_status() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");
    // No ledger.db

    let (stdout, stderr, code) = run_cli(root, &["symbols", "--json"]);
    assert_eq!(code, 0, "H1: missing DB must exit 0; stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("pure JSON envelope: {e}; stdout={stdout}");
    });
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["totalMatching"], 0);
    assert!(v["symbols"].as_array().unwrap().is_empty());
    assert_eq!(v["indexStatus"]["state"], "missing");
    assert!(
        v["indexStatus"]["remediation"]
            .as_str()
            .unwrap()
            .contains("index")
    );
}

#[test]
fn symbols_limit_over_max_rejected() {
    let tmp = setup_seeded_repo();
    let root = tmp.path();

    let (_stdout, stderr, code) = run_cli(root, &["symbols", "--limit", "5001", "--json"]);
    assert_ne!(code, 0, "limit > 5000 must be rejected; stderr={stderr}");
}

#[test]
fn symbols_line_null_omitted() {
    let tmp = setup_seeded_repo();
    let root = tmp.path();

    let (stdout, stderr, code) = run_cli(
        root,
        &[
            "symbols",
            "--path",
            "src/commands",
            "--kind",
            "Module",
            "--json",
        ],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let symbols = v["symbols"].as_array().unwrap();
    assert!(!symbols.is_empty());
    for s in symbols {
        assert!(
            s.get("line").is_none() || s["line"].is_number(),
            "line must be omitted or number, never null: {s}"
        );
        if s["name"] == "epsilon" {
            assert!(s.get("line").is_none(), "epsilon has null line_start");
        }
    }
}

/// F2: `--changed` outside a git work tree must fail closed with a clear error.
#[test]
fn symbols_changed_non_git_errors() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    // No git init — hermetic non-repo root. Seed index so we fail on git, not H1.
    seed_symbols_fixture(root);

    let (_stdout, stderr, code) = run_cli(root, &["symbols", "--changed", "--json"]);
    assert_ne!(
        code, 0,
        "non-git --changed must be non-zero; stderr={stderr}"
    );
    let lower = stderr.to_lowercase();
    assert!(
        lower.contains("git") || lower.contains("changed"),
        "error should mention git/changed; stderr={stderr}"
    );
}

/// Codex P2-2: `--changed` without git must fail *before* H1 missing-DB envelope.
/// Non-git root + no ledger.db must be non-zero with git/changed error — not
/// exit 0 with `indexStatus: missing`.
#[test]
fn symbols_changed_non_git_no_db_errors_before_h1() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    // No git init, no ledger.db.

    let (stdout, stderr, code) = run_cli(root, &["symbols", "--changed", "--json"]);
    assert_ne!(
        code, 0,
        "non-git --changed without DB must be non-zero; stderr={stderr}; stdout={stdout}"
    );
    let lower = stderr.to_lowercase();
    assert!(
        lower.contains("git") || lower.contains("changed"),
        "error should mention git/changed; stderr={stderr}"
    );
    // Must not emit the H1 empty indexStatus success envelope on stdout.
    let trimmed = stdout.trim();
    if !trimmed.is_empty()
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed)
    {
        let is_h1_missing = v.get("schemaVersion") == Some(&serde_json::json!(1))
            && v.get("indexStatus")
                .is_some_and(|s| s.get("state") == Some(&serde_json::json!("missing")));
        assert!(
            !is_h1_missing,
            "must not emit H1 missing-index envelope for --changed outside git; stdout={stdout}"
        );
    }
}

/// 0183: file-form `pkg.rs` with only `pkg/mod.rs` indexed → non-empty symbols + pathResolve.
#[test]
fn symbols_path_file_form_resolves_to_mod_rs() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join("src/pkg")).unwrap();
    fs::write(root.join("src/pkg/mod.rs"), "pub fn mod_fn() {}").unwrap();
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let root_utf8 = Utf8Path::from_path(root).expect("utf8 root");
    let layout = Layout::new(root_utf8);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let conn = storage.get_connection();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) \
         VALUES (1, 'src/pkg/mod.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_symbols \
         (id, file_id, qualified_name, symbol_name, symbol_kind, is_public, \
          line_start, last_indexed_at) \
         VALUES (1, 1, 'mod_fn', 'mod_fn', 'Function', 1, 1, '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    storage.shutdown().unwrap();

    let (stdout, stderr, code) = run_cli(
        root,
        &["symbols", "--path", "src/pkg.rs", "--json", "--limit", "20"],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("JSON expected: {e}; stdout={stdout}");
    });
    assert!(
        v["totalMatching"].as_u64().unwrap() >= 1,
        "file-form path must resolve to mod.rs symbols; envelope={v}"
    );
    assert_eq!(v["pathResolve"]["status"], "resolved");
    assert_eq!(v["pathResolve"]["resolvedPath"], "src/pkg/mod.rs");
    let symbols = v["symbols"].as_array().unwrap();
    assert!(
        symbols
            .iter()
            .any(|s| s["path"].as_str() == Some("src/pkg/mod.rs")),
        "symbols should list mod.rs path; envelope={v}"
    );

    // Dir prefix still inventories children without forcing file resolve.
    let (stdout2, stderr2, code2) = run_cli(
        root,
        &["symbols", "--path", "src/pkg", "--json", "--limit", "20"],
    );
    assert_eq!(code2, 0, "stderr={stderr2}");
    let v2: serde_json::Value = serde_json::from_str(stdout2.trim()).unwrap();
    assert!(v2["totalMatching"].as_u64().unwrap() >= 1);
    assert!(
        v2.get("pathResolve").is_none(),
        "successful dir prefix must not set pathResolve; envelope={v2}"
    );
}

/// 0183: multi-match suffix on --path refuses with pathResolve ambiguous.
#[test]
fn symbols_path_ambiguous_suffix_refuses() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join("src/a")).unwrap();
    fs::create_dir_all(root.join("src/b")).unwrap();
    fs::write(root.join("src/a/mod.rs"), "pub fn a() {}").unwrap();
    fs::write(root.join("src/b/mod.rs"), "pub fn b() {}").unwrap();
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let root_utf8 = Utf8Path::from_path(root).expect("utf8 root");
    let layout = Layout::new(root_utf8);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let conn = storage.get_connection();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES \
         (1, 'src/a/mod.rs', '2026-01-01T00:00:00Z'), \
         (2, 'src/b/mod.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_symbols \
         (id, file_id, qualified_name, symbol_name, symbol_kind, is_public, last_indexed_at) \
         VALUES (1, 1, 'a', 'a', 'Function', 1, '2026-01-01T00:00:00Z'), \
                (2, 2, 'b', 'b', 'Function', 1, '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    storage.shutdown().unwrap();

    let (stdout, stderr, code) = run_cli(root, &["symbols", "--path", "mod.rs", "--json"]);
    assert_eq!(
        code, 0,
        "ambiguous is honest empty, not hard fail; stderr={stderr}"
    );
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["totalMatching"], 0);
    assert_eq!(v["pathResolve"]["status"], "ambiguous");
    let cands = v["pathResolve"]["candidates"].as_array().unwrap();
    assert_eq!(cands.len(), 2);
}

/// 0183: --changed + file-form --path still intersects resolved mod.rs.
#[test]
fn symbols_changed_path_file_form_resolves() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join("src/pkg")).unwrap();
    fs::write(root.join("src/pkg/mod.rs"), "pub fn mod_fn() {}").unwrap();
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let root_utf8 = Utf8Path::from_path(root).expect("utf8 root");
    let layout = Layout::new(root_utf8);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let conn = storage.get_connection();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) \
         VALUES (1, 'src/pkg/mod.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_symbols \
         (id, file_id, qualified_name, symbol_name, symbol_kind, is_public, \
          line_start, last_indexed_at) \
         VALUES (1, 1, 'mod_fn', 'mod_fn', 'Function', 1, 1, '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    storage.shutdown().unwrap();

    // Dirty the mod.rs path so it appears in --changed.
    fs::write(
        root.join("src/pkg/mod.rs"),
        "pub fn mod_fn() { let _x = 1; }",
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli(
        root,
        &[
            "symbols",
            "--changed",
            "--path",
            "src/pkg.rs",
            "--json",
            "--limit",
            "20",
        ],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("JSON expected: {e}; stdout={stdout}");
    });
    assert!(
        v["totalMatching"].as_u64().unwrap() >= 1,
        "changed+file-form path must resolve to dirty mod.rs; envelope={v}"
    );
    assert_eq!(v["pathResolve"]["status"], "resolved");
    assert_eq!(v["pathResolve"]["resolvedPath"], "src/pkg/mod.rs");
}

/// 0183: rename mod.rs → pkg.rs on disk; index still has mod.rs; --changed --path pkg.rs resolves.
#[test]
fn symbols_changed_path_file_form_after_rename_to_rs() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join("src/pkg")).unwrap();
    fs::write(root.join("src/pkg/mod.rs"), "pub fn mod_fn() {}").unwrap();
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");

    let root_utf8 = Utf8Path::from_path(root).expect("utf8 root");
    let layout = Layout::new(root_utf8);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let conn = storage.get_connection();
    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) \
         VALUES (1, 'src/pkg/mod.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_symbols \
         (id, file_id, qualified_name, symbol_name, symbol_kind, is_public, \
          line_start, last_indexed_at) \
         VALUES (1, 1, 'mod_fn', 'mod_fn', 'Function', 1, 1, '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    storage.shutdown().unwrap();

    // Rename on disk: new path is file-form; index still knows mod.rs.
    git_cmd(root, &["mv", "src/pkg/mod.rs", "src/pkg.rs"]);

    let (stdout, stderr, code) = run_cli(
        root,
        &[
            "symbols",
            "--changed",
            "--path",
            "src/pkg.rs",
            "--json",
            "--limit",
            "20",
        ],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("JSON expected: {e}; stdout={stdout}");
    });
    assert!(
        v["totalMatching"].as_u64().unwrap() >= 1,
        "rename+file-form must resolve via old indexed mod.rs; envelope={v}"
    );
    assert_eq!(v["pathResolve"]["status"], "resolved");
    assert_eq!(v["pathResolve"]["resolvedPath"], "src/pkg/mod.rs");
}
