use camino::Utf8Path;
use ledgerful::commands::index::{IndexArgs, execute_index};
use std::fs;
use tempfile::tempdir;

use crate::common::{DirGuard, git_add_and_commit, git_cmd, run_cli, setup_git_repo};

/// DoD-1: healthy `index --check` prints non-empty human stdout and exits 0.
/// Uses subprocess because the live path may call process::exit on other branches.
#[test]
fn test_index_check_healthy_prints_status() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "pub fn check_target() {}").unwrap();

    let _guard = DirGuard::from_utf8(root);
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .unwrap();

    let index_result = execute_index(IndexArgs {
        ..Default::default()
    });
    assert!(index_result.is_ok(), "index must succeed: {index_result:?}");

    // Live path: `index --check` (not the deleted execute_index_check).
    let (stdout, _stderr, code) = run_cli(tmp.path(), &["index", "--check"]);
    assert_eq!(code, 0, "healthy check must exit 0; stdout={stdout}");
    assert!(
        !stdout.trim().is_empty(),
        "DoD-1: healthy check must print non-empty stdout, got empty"
    );
    assert!(
        stdout.contains("Index")
            || stdout.contains("up to date")
            || stdout.contains("Files indexed"),
        "expected human status text, got: {stdout}"
    );
}

/// DoD-3: `index --check --json` entire stdout parses as a single JSON document.
#[test]
fn test_index_check_json_is_pure() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn check_json_target() {}",
    )
    .unwrap();

    let _guard = DirGuard::from_utf8(root);
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .unwrap();

    let index_result = execute_index(IndexArgs {
        ..Default::default()
    });
    assert!(index_result.is_ok(), "index must succeed: {index_result:?}");

    let (stdout, stderr, code) = run_cli(tmp.path(), &["index", "--check", "--json"]);
    assert_eq!(code, 0, "check --json must exit 0; stdout={stdout}");
    // Assert by parsing the *entire* stdout — absence greps pass on empty output.
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("DoD-3: entire stdout must parse as JSON: {e}; stdout={stdout}");
    });
    assert!(parsed.is_object(), "expected JSON object, got {parsed}");
    assert_eq!(
        parsed["schemaVersion"], 1,
        "check JSON schemaVersion: {parsed}"
    );
    assert_eq!(parsed["kind"], "indexCheck", "check JSON kind: {parsed}");
    assert!(
        parsed.get("totalFiles").is_some(),
        "check JSON must expose camelCase totalFiles: {parsed}"
    );
    assert!(
        parsed.get("total_files").is_none(),
        "check JSON must not expose snake total_files: {parsed}"
    );
    // 0149 DoD-4: success path must not print human Info on stderr.
    assert!(
        stderr.trim().is_empty(),
        "index --check --json success must have empty stderr, got: {stderr:?}"
    );
}

/// DoD-2: `index --check --strict` on a stale index prints reason on stderr and exits 1.
///
/// Must use `run_cli` (subprocess): the live path calls `process::exit(1)`, which
/// would kill an in-process test binary. Do not "simplify" this back in-process.
#[test]
fn test_index_check_strict_stale_exits_with_reason() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "pub fn strict_target() {}").unwrap();

    let _guard = DirGuard::from_utf8(root);
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .unwrap();

    let index_result = execute_index(IndexArgs {
        ..Default::default()
    });
    assert!(index_result.is_ok(), "index must succeed: {index_result:?}");

    // Make index stale by editing a source file after indexing.
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn strict_target() {}\npub fn extra() {}",
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli(tmp.path(), &["index", "--check", "--strict"]);
    assert_eq!(
        code, 1,
        "DoD-2: strict stale must exit 1; stdout={stdout}; stderr={stderr}"
    );
    assert!(
        stdout.trim().is_empty(),
        "strict failure reason belongs on stderr, not stdout: {stdout}"
    );
    assert!(
        !stderr.trim().is_empty(),
        "DoD-2: strict stale must print reason on stderr"
    );
    let lower = stderr.to_lowercase();
    assert!(
        lower.contains("stale") || lower.contains("strict"),
        "stderr must name staleness/strict: {stderr}"
    );
}

/// 0421: content-hash clean + typed surface head mismatch → `--strict` exit 1.
///
/// Must commit before `execute_index` (unborn HEAD cannot stale a surface).
/// Seed `test_mapping` with migrated columns + live parent ids when empty.
#[test]
fn test_index_check_strict_surface_stale_exits() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn surface_stale_target() {}",
    )
    .unwrap();
    git_add_and_commit(tmp.path(), "initial");

    let _guard = DirGuard::from_utf8(root);
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .unwrap();

    let index_result = execute_index(IndexArgs {
        ..Default::default()
    });
    assert!(index_result.is_ok(), "index must succeed: {index_result:?}");

    let db = root.join(".ledgerful").join("state").join("ledger.db");
    let conn = rusqlite::Connection::open(db.as_std_path()).expect("open ledger.db");
    let mapping_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM test_mapping", [], |r| r.get(0))
        .expect("count test_mapping");
    if mapping_count == 0 {
        let file_id: i64 = conn
            .query_row("SELECT id FROM project_files LIMIT 1", [], |r| r.get(0))
            .expect("project_files row after index");
        let symbol_id: i64 = conn
            .query_row("SELECT id FROM project_symbols LIMIT 1", [], |r| r.get(0))
            .expect("project_symbols row after index");
        conn.execute(
            "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id, mapping_kind, last_indexed_at) \
             VALUES (?1, ?2, ?1, ?2, 'IMPORT', '2026-01-01T00:00:00Z')",
            (symbol_id, file_id),
        )
        .expect("seed test_mapping");
    }
    drop(conn);

    git_cmd(tmp.path(), &["commit", "--allow-empty", "-m", "bump"]);

    let (stdout, stderr, code) = run_cli(tmp.path(), &["index", "--check", "--json"]);
    assert_eq!(
        code, 0,
        "json check must exit 0; stdout={stdout}; stderr={stderr}"
    );
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("check --json must parse: {e}; stdout={stdout}");
    });
    assert_eq!(parsed["schemaVersion"], 1);
    assert_eq!(parsed["kind"], "indexCheck");
    assert_eq!(
        parsed["staleFiles"], 0,
        "fixture must stay file-hash clean: {parsed}"
    );
    assert_eq!(
        parsed["assessment"]["state"], "FreshPopulated",
        "assessment must stay file-hash: {parsed}"
    );
    let stale = parsed["surfaces"]
        .as_array()
        .expect("surfaces")
        .iter()
        .any(|s| s["status"] == "stale");
    assert!(
        stale,
        "expected a stale surface after empty commit: {parsed}"
    );

    let (stdout, stderr, code) = run_cli(tmp.path(), &["index", "--check", "--strict"]);
    assert_eq!(
        code, 1,
        "strict surface stale must exit 1; stdout={stdout}; stderr={stderr}"
    );
    assert!(
        stdout.trim().is_empty(),
        "strict failure reason belongs on stderr, not stdout: {stdout}"
    );
    let lower = stderr.to_lowercase();
    assert!(
        lower.contains("stale") || lower.contains("strict"),
        "stderr must name staleness/strict: {stderr}"
    );

    let (stdout, stderr, code) = run_cli(tmp.path(), &["index", "--check", "--json", "--strict"]);
    assert_eq!(
        code, 1,
        "json+strict must exit 1; stdout={stdout}; stderr={stderr}"
    );
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("json+strict stdout must parse: {e}; stdout={stdout}");
    });
    assert_eq!(parsed["schemaVersion"], 1);
    assert_eq!(parsed["kind"], "indexCheck");
    let lower = stderr.to_lowercase();
    assert!(
        lower.contains("strict") || lower.contains("stale"),
        "json+strict stderr must name --strict/stale: {stderr}"
    );

    let (stdout, _stderr, code) = run_cli(tmp.path(), &["index", "--check"]);
    assert_eq!(
        code, 0,
        "non-strict mixed fixture must exit 0; stdout={stdout}"
    );
    assert!(
        stdout.contains("Surface "),
        "non-strict must print a surface lag line: {stdout}"
    );
}

/// 0128: content-dirty age-fresh index must never report FreshPopulated with
/// positive staleFiles; assessment.staleFiles == top-level staleFiles.
#[test]
fn test_index_check_json_content_stale_not_fresh_populated() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn content_stale_target() {}",
    )
    .unwrap();

    let _guard = DirGuard::from_utf8(root);
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .unwrap();

    let index_result = execute_index(IndexArgs {
        ..Default::default()
    });
    assert!(index_result.is_ok(), "index must succeed: {index_result:?}");

    // Edit after index → content-hash drift while last_indexed_at is today.
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn content_stale_target() {}\npub fn after_edit() {}",
    )
    .unwrap();

    let (stdout, _stderr, code) = run_cli(tmp.path(), &["index", "--check", "--json"]);
    assert_eq!(
        code, 0,
        "check --json (non-strict) must exit 0; stdout={stdout}"
    );
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("entire stdout must parse as JSON: {e}; stdout={stdout}");
    });

    let top_stale = parsed["staleFiles"].as_u64().expect("top-level staleFiles");
    assert!(
        top_stale > 0,
        "edited file must produce content drift: {parsed}"
    );

    let assessment = parsed["assessment"]
        .as_object()
        .expect("assessment object present");
    let state = assessment["state"].as_str().unwrap_or("");
    assert_ne!(
        state, "FreshPopulated",
        "DoD-1: never FreshPopulated with staleFiles > 0: {parsed}"
    );
    assert!(
        state == "ContentStalePopulated" || state.contains("Stale") || state.contains("Content"),
        "expected ContentStalePopulated (or stale variant), got {state}: {parsed}"
    );
    let assess_stale = assessment["staleFiles"]
        .as_u64()
        .expect("assessment.staleFiles");
    assert_eq!(
        assess_stale, top_stale,
        "DoD-2: assessment.staleFiles must equal top-level staleFiles"
    );
}

/// 0128: clean age-fresh index reports up to date with staleFiles 0.
#[test]
fn test_index_check_json_clean_is_fresh_populated_zero_stale() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "pub fn clean_target() {}").unwrap();

    let _guard = DirGuard::from_utf8(root);
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .unwrap();

    let index_result = execute_index(IndexArgs {
        ..Default::default()
    });
    assert!(index_result.is_ok(), "index must succeed: {index_result:?}");

    let (stdout, _stderr, code) = run_cli(tmp.path(), &["index", "--check", "--json"]);
    assert_eq!(code, 0, "check --json must exit 0; stdout={stdout}");
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("entire stdout must parse as JSON: {e}; stdout={stdout}");
    });

    assert_eq!(
        parsed["staleFiles"].as_u64().unwrap_or(999),
        0,
        "clean tree must have top-level staleFiles 0: {parsed}"
    );
    let assessment = &parsed["assessment"];
    assert_eq!(
        assessment["state"].as_str().unwrap_or(""),
        "FreshPopulated",
        "clean age-fresh must be FreshPopulated: {parsed}"
    );
    assert_eq!(
        assessment["staleFiles"].as_u64().unwrap_or(999),
        0,
        "assessment.staleFiles must be 0 when clean: {parsed}"
    );
}

/// Semantic dry-run on a fresh repo should succeed and print a report.
#[test]
fn test_index_semantic_dry_run_smoke() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "fn main() {}").unwrap();

    let _guard = DirGuard::from_utf8(root);

    let result = execute_index(IndexArgs {
        semantic_dry_run: Some(None),
        ..Default::default()
    });
    assert!(result.is_ok());
}

/// Docs mode with no config should gracefully skip.
#[test]
fn test_index_docs_no_config_skips() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);

    // ensure state dir exists so StorageManager::init can create ledger.db
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .unwrap();

    let result = execute_index(IndexArgs {
        docs: true,
        ..Default::default()
    });
    assert!(result.is_ok());
}

/// Mode-combination matrix: --semantic without --analyze-graph should
/// take the semantic standalone path (not the main pipeline).
#[test]
fn test_index_semantic_standalone_mode() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "fn main() {}").unwrap();

    let _guard = DirGuard::from_utf8(root);

    // With no CozoDB storage this will fail at "CozoDB storage not initialized",
    // but that's expected and proves it entered the semantic path, not the main path.
    let result = execute_index(IndexArgs {
        semantic: true,
        analyze_graph: false,
        ..Default::default()
    });
    assert!(result.is_err());
}

/// Mode-combination matrix: --semantic --analyze-graph should fall through
/// to the main pipeline (not early-return from semantic standalone).
#[test]
fn test_index_semantic_with_analyze_graph_falls_through() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "fn main() {}").unwrap();

    let _guard = DirGuard::from_utf8(root);

    // Ensure state dir exists so the main path gets past StorageManager::init.
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .unwrap();

    // analyze_graph falls through to main path and completes successfully on
    // a minimal repo (unlike the semantic standalone path which needs CozoDB).
    let result = execute_index(IndexArgs {
        semantic: true,
        analyze_graph: true,
        ..Default::default()
    });
    assert!(result.is_ok(), "Main path should complete on minimal repo");
}

/// --auto-scip should gracefully fall back to native indexing if no toolchain
/// is found, instead of failing the entire command.
#[test]
#[allow(non_snake_case)]
fn test_index_auto_scip_graceful_fallback__slow() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "fn main() {}").unwrap();
    // Add a Cargo.toml so detection triggers for Rust
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"test\"\nversion = \"0.1.0\"",
    )
    .unwrap();

    let _guard = DirGuard::from_utf8(root);

    // Ensure state dir exists so the main path gets past StorageManager::init.
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .unwrap();

    // Even if rust-analyzer is missing, this should succeed by falling back.
    let result = execute_index(IndexArgs {
        auto_scip: true,
        ..Default::default()
    });
    assert!(
        result.is_ok(),
        "Auto-SCIP should fall back to native if binary is missing or generation fails"
    );
}

fn setup_call_fixture(root: &Utf8Path) {
    setup_git_repo(root.as_std_path());
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src").join("lib.rs"),
        "fn helper() {}\nfn caller() { helper(); }\n",
    )
    .unwrap();
    ledgerful::state::layout::Layout::new(root)
        .ensure_state_dir()
        .unwrap();
}

fn count_structural_edges(root: &Utf8Path) -> i64 {
    let db = root.join(".ledgerful").join("state").join("ledger.db");
    let conn = rusqlite::Connection::open(db.as_std_path()).expect("open ledger.db");
    conn.query_row("SELECT COUNT(*) FROM structural_edges", [], |r| r.get(0))
        .expect("COUNT(*) structural_edges")
}

/// DoD-2(a): `--full --analyze-graph` must store one builder pass, not 2×.
#[test]
#[allow(non_snake_case)]
fn analyze_graph_does_not_double_native_structural_edges__slow() {
    let tmp_plain = tempdir().unwrap();
    let root_plain = Utf8Path::from_path(tmp_plain.path()).unwrap();
    setup_call_fixture(root_plain);
    {
        let _guard = DirGuard::from_utf8(root_plain);
        execute_index(IndexArgs {
            full: true,
            ..Default::default()
        })
        .expect("index --full");
    }
    let n = count_structural_edges(root_plain);
    assert!(n > 0, "fixture must produce native edges, got {n}");

    let tmp_analyze = tempdir().unwrap();
    let root_analyze = Utf8Path::from_path(tmp_analyze.path()).unwrap();
    setup_call_fixture(root_analyze);
    {
        let _guard = DirGuard::from_utf8(root_analyze);
        execute_index(IndexArgs {
            full: true,
            analyze_graph: true,
            ..Default::default()
        })
        .expect("index --full --analyze-graph");
    }
    let n_analyze = count_structural_edges(root_analyze);
    assert_eq!(
        n_analyze, n,
        "analyze-graph must not insert another copy of the native pass"
    );
}

/// DoD-2(b): `--full` then `--incremental` twice with no edits must not stack.
#[test]
#[allow(non_snake_case)]
fn repeat_incremental_does_not_stack_native_structural_edges__slow() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_call_fixture(root);

    let _guard = DirGuard::from_utf8(root);
    execute_index(IndexArgs {
        full: true,
        ..Default::default()
    })
    .expect("index --full");
    let n = count_structural_edges(root);
    assert!(n > 0, "fixture must produce native edges, got {n}");

    execute_index(IndexArgs {
        incremental: true,
        ..Default::default()
    })
    .expect("index --incremental #1");
    assert_eq!(
        count_structural_edges(root),
        n,
        "first no-change incremental must not stack native edges"
    );

    execute_index(IndexArgs {
        incremental: true,
        ..Default::default()
    })
    .expect("index --incremental #2");
    assert_eq!(
        count_structural_edges(root),
        n,
        "second no-change incremental must not stack native edges"
    );
}

/// DoD-1: `index --full --analyze-graph` emits exactly one CG complete line.
#[test]
#[allow(non_snake_case)]
fn analyze_graph_emits_one_call_graph_build_complete__slow() {
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone)]
    struct BufWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for BufWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for BufWriter {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_call_fixture(root);
    let _guard = DirGuard::from_utf8(root);

    let buf = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(BufWriter(buf.clone()))
        .with_ansi(false)
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        execute_index(IndexArgs {
            full: true,
            analyze_graph: true,
            ..Default::default()
        })
        .expect("index --full --analyze-graph");
    });

    let logs = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    let hits = logs.matches("Call graph build complete").count();
    assert_eq!(
        hits, 1,
        "expected exactly one Call graph build complete line, got {hits}: {logs}"
    );
}

/// DoD-5: `--analyze-graph --export-docs` must not claim KG unavailable when Cozo is up.
#[test]
#[allow(non_snake_case)]
fn analyze_graph_export_docs_does_not_report_kg_unavailable__slow() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_call_fixture(root);

    let (stdout, stderr, code) = run_cli(
        tmp.path(),
        &["index", "--full", "--analyze-graph", "--export-docs"],
    );
    assert_eq!(
        code, 0,
        "export-docs must exit 0; stdout={stdout}; stderr={stderr}"
    );
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        !combined.contains("Knowledge Graph unavailable"),
        "Cozo is available on this fixture; dummy storage swap must not hide it: {combined}"
    );
    let docs_dir = root.join(".ledgerful").join("docs");
    assert!(
        combined.contains("Doc:") || docs_dir.exists(),
        "export-docs must print a Doc: line or write docs from this CLI run; stdout={stdout}; stderr={stderr}"
    );
}
mod dependency_evidence {
    use rstest::rstest;
    use serde_json::{Value, json};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/dependency_evidence"
    );
    const REPORT: &[&str] = &["index", "--check", "--dependency-evidence", "--json"];

    struct Repo {
        dir: tempfile::TempDir,
        home: PathBuf,
    }

    impl Repo {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join(".test-config");
            fs::create_dir(&home).unwrap();
            fs::write(home.join("gitconfig"), "").unwrap();
            let repo = Self { dir, home };
            repo.git(&["init", "-q"]);
            repo
        }

        fn root(&self) -> &Path {
            self.dir.path()
        }

        fn db(&self) -> PathBuf {
            self.root().join(".ledgerful/state/ledger.db")
        }

        fn git(&self, args: &[&str]) {
            let output = Command::new("git")
                .args([
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "user.name=fixture",
                ])
                .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
                .current_dir(self.root())
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}: {output:?}");
        }

        fn command(&self) -> Command {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_ledgerful"));
            cmd.env_remove("LEDGERFUL_STATE_DIR")
                .env("LEDGERFUL_CONFIG_HOME", &self.home)
                .env("XDG_CONFIG_HOME", &self.home)
                .env(
                    "LEDGERFUL_DEFAULT_CONFIG",
                    self.home.join("no-default.toml"),
                )
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
                .env("PATH", &self.home)
                .env_remove("GEMINI_API_KEY")
                .env_remove("OPENAI_API_KEY")
                .env_remove("OLLAMA_CLOUD_API_KEY")
                .current_dir(self.root());
            cmd
        }

        fn call(&self, args: &[&str]) -> Output {
            self.command().args(args).output().unwrap()
        }

        fn packet(&self, args: &[&str]) -> Value {
            json_output(self.call(args))
        }

        fn report(&self) -> Value {
            self.packet(REPORT)
        }

        fn checkpoint(&self, message: &str) {
            self.git(&["add", "--all"]);
            self.git(&["commit", "-qm", message]);
        }

        fn index(&self, extra: &[&str]) -> Value {
            let args = ["index", "--analyze-graph", "--json"];
            self.packet(&[&args[..], extra].concat())
        }
    }

    fn json_output(output: Output) -> Value {
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| panic!("{error}: {output:?}"))
    }

    #[rstest]
    #[case(&["--incremental"])]
    #[case(&["-i"])]
    #[case(&["--full"])]
    #[case(&["-f"])]
    #[case(&["--analyze-graph"])]
    #[case(&["--docs"])]
    #[case(&["--contracts"])]
    #[case(&["--semantic"])]
    #[case(&["--scip", "unused.scip"])]
    #[case(&["--auto-scip"])]
    #[case(&["--export-docs"])]
    #[case(&["--doc-type", "markdown"])]
    #[case(&["--strict"])]
    #[case(&["--concurrency", "1"])]
    #[case(&["-j", "1"])]
    #[case(&["--semantic-dry-run"])]
    #[case(&["--semantic-dry-run", "must-not-write.json"])]
    #[case(&["--fast"])]
    #[case(&["--repair-metadata"])]
    #[case(&["--dry-run"])]
    #[case(&["--yes"])]
    fn dependency_evidence_conflicting_flags_fail_before_writes(#[case] conflict: &[&str]) {
        let repo = Repo::new();
        let args = [REPORT, conflict].concat();
        let rejected = repo.call(&args);
        assert_eq!(rejected.status.code(), Some(2), "{rejected:?}");
        assert!(rejected.stdout.is_empty());
        assert!(!repo.root().join(".ledgerful").exists());
        assert!(!repo.root().join("must-not-write.json").exists());
    }

    #[rstest]
    #[case(&["index", "--dependency-evidence", "--json"])]
    #[case(&["index", "--check", "--dependency-scip-file", "missing.scip"])]
    #[case(&["index", "--strict", "--dependency-evidence", "--check"])]
    fn dependency_evidence_incomplete_and_reordered_invocations_fail_without_migrating(
        #[case] args: &[&str],
    ) {
        let repo = Repo::new();
        let legacy = repo.root().join(concat!(".change", "guard"));
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join("marker"), "legacy").unwrap();
        let result = repo.call(args);
        assert_eq!(result.status.code(), Some(2), "{result:?}");
        assert!(!repo.root().join(".ledgerful").exists());
        assert_eq!(fs::read(legacy.join("marker")).unwrap(), b"legacy");
    }

    #[test]
    fn dependency_evidence_missing_database_preserves_legacy_install_and_packets() {
        let repo = Repo::new();
        let legacy = repo.root().join(concat!(".change", "guard"));
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join("config.toml"), "[verify]\n").unwrap();
        let first = repo.call(REPORT);
        let repeat = repo.call(REPORT);
        assert_eq!(first.stdout, repeat.stdout);
        let report = json_output(first);
        assert_eq!(report["schemaVersion"], 1);
        assert_eq!(report["kind"], "dependencyEvidence");
        assert_eq!(report["native"]["state"], "unknown");
        assert_eq!(
            report["relationships"],
            json!({
                "state":"unknown","reasons":["databaseMissing"],"totalRows":null,
                "byCallerLanguage":null,"byProvenanceAndResolution":null,"totalWitnesses":null,
                "truncated":null,"witnesses":[]
            })
        );
        assert_eq!(
            report["scipArtifact"],
            json!({
                "state":"notChecked","sourceFreshness":"unknown","reasons":[]
            })
        );
        assert_eq!(
            report["limitations"],
            json!([
                "crossLanguageUnqualified",
                "ignoredOrUnseenFiles",
                "importsAliasesReExportsUnqualified",
                "retainedScipRowsUnattributed"
            ])
        );
        let human = repo.call(&REPORT[..3]);
        assert!(human.status.success(), "{human:?}");
        let human = String::from_utf8(human.stdout).unwrap();
        assert!(human.contains("databaseMissing"));
        assert!(human.contains("retainedScipRowsUnattributed"));
        assert_eq!(fs::read(legacy.join("config.toml")).unwrap(), b"[verify]\n");
        assert!(!repo.root().join(".ledgerful").exists());
    }

    #[cfg(windows)]
    #[test]
    fn dependency_evidence_preserves_project_local_old_binary() {
        let repo = Repo::new();
        let binary = repo.root().join("ledgerful.exe");
        let old = repo.root().join("ledgerful.old.fixture.exe");
        fs::copy(env!("CARGO_BIN_EXE_ledgerful"), &binary).unwrap();
        fs::write(&old, "old executable witness").unwrap();
        let output = Command::new(binary)
            .current_dir(repo.root())
            .args(REPORT)
            .output()
            .unwrap();
        assert_eq!(json_output(output)["kind"], "dependencyEvidence");
        assert_eq!(fs::read(old).unwrap(), b"old executable witness");
        assert!(!repo.root().join(".ledgerful").exists());
    }

    #[rstest]
    #[case("zero-byte", "databaseEmpty")]
    #[case("corrupt", "databaseCorrupt")]
    #[case("old", "databaseSchemaIncompatible")]
    #[case("new", "databaseSchemaIncompatible")]
    fn dependency_evidence_bad_database_is_reported_without_migration(
        #[case] state: &str,
        #[case] reason: &str,
    ) {
        let repo = Repo::new();
        let path = repo.db();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        match state {
            "zero-byte" => fs::write(&path, []).unwrap(),
            "corrupt" => fs::write(&path, b"this is not SQLite").unwrap(),
            _ => {
                let conn = rusqlite::Connection::open(&path).unwrap();
                let version = ledgerful::state::migrations::get_migrations_count() as i64;
                conn.pragma_update(
                    None,
                    "user_version",
                    if state == "old" {
                        version - 1
                    } else {
                        version + 1
                    },
                )
                .unwrap();
            }
        }
        let before = fs::read(&path).unwrap();
        let report = repo.report();
        assert_eq!(report["native"]["state"], "unknown");
        assert_eq!(report["relationships"]["state"], "unknown");
        assert_eq!(report["relationships"]["reasons"], json!([reason]));
        assert_eq!(report["languages"][0]["indexedFiles"], Value::Null);
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn dependency_evidence_busy_reader_exits_zero_and_preserves_database() {
        let repo = Repo::new();
        fs::create_dir_all(repo.db().parent().unwrap()).unwrap();
        let conn = rusqlite::Connection::open(repo.db()).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=DELETE; CREATE TABLE fixture(x TEXT); BEGIN EXCLUSIVE;",
        )
        .unwrap();
        let report = repo.report();
        assert_eq!(report["relationships"]["state"], "unknown");
        assert_eq!(report["relationships"]["reasons"], json!(["databaseBusy"]));
        conn.execute_batch("ROLLBACK;").unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM fixture", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    fn fixture() -> Repo {
        use sha2::{Digest, Sha256};
        let repo = Repo::new();
        let dir = Path::new(FIXTURE);
        let manifest: Value =
            serde_json::from_slice(&fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        for (bucket, source) in [
            ("sources", dir.join("source")),
            ("artifacts", dir.to_owned()),
        ] {
            for (name, digest) in manifest[bucket].as_object().unwrap() {
                let bytes = fs::read(source.join(name)).unwrap();
                assert_eq!(
                    hex::encode(Sha256::digest(&bytes)),
                    digest.as_str().unwrap(),
                    "{name}"
                );
                fs::write(repo.root().join(name), bytes).unwrap();
            }
        }
        fs::write(repo.root().join(".gitignore"), ".ledgerful/\n*.scip\n").unwrap();
        fs::write(
            repo.root().join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::write(repo.root().join("tsconfig.json"), "{}").unwrap();
        fs::write(repo.root().join("requirements.txt"), "").unwrap();
        repo.checkpoint("fixture");
        fs::create_dir_all(repo.db().parent().unwrap()).unwrap();
        repo
    }

    fn assess_artifact(repo: &Repo, path: &Path) -> Value {
        repo.packet(&[REPORT, &["--dependency-scip-file", path.to_str().unwrap()]].concat())["scipArtifact"].clone()
    }

    #[test]
    #[cfg(any(target_os = "linux", windows))]
    fn dependency_evidence_replays_native_and_frozen_scip_oracle() {
        let repo = fixture();
        let ordinary = repo.index(&[]);
        assert_eq!(ordinary["scip"]["status"], "did_not_run");
        assert!(ordinary["scip"].get("selection").is_none());
        let report = repo.report();
        assert_eq!(report["native"]["state"], "indexInputsMatch");
        assert_eq!(report["relationships"]["state"], "partial");
        assert_eq!(report["relationships"]["reasons"], json!(["unresolved"]));
        let witness = |file: &str, caller: &str, callee: Option<&str>, provenance: &str| {
            json!({
                "callerFile":file,"callerSymbol":caller,
                "calleeFile":callee.map(|_| file),"calleeSymbol":callee,
                "kind":"direct","resolution":if callee.is_some() { "resolved" } else { "unresolved" },
                "provenance":provenance,"equivalentRows":1
            })
        };
        let mut expected = vec![
            witness("main.py", "alias_py_caller", None, "nativeCall"),
            witness("main.py", "py_caller", Some("py_target"), "nativeCall"),
            witness("main.rs", "alias_rust_caller", None, "nativeCall"),
            witness("main.rs", "rust_caller", Some("rust_target"), "nativeCall"),
            witness("main.ts", "aliasTsCaller", None, "nativeCall"),
            witness("main.ts", "tsCaller", Some("tsTarget"), "nativeCall"),
        ];
        // Exact inventory excludes import-only, re-export-only and bridge mentions.
        assert_eq!(report["relationships"]["witnesses"], json!(expected));
        let languages = report["languages"].as_array().unwrap();
        assert_eq!(
            languages
                .iter()
                .map(|row| row["language"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "cpp",
                "go",
                "java",
                "markdown",
                "other",
                "python",
                "rust",
                "typescript"
            ]
        );
        assert_eq!(
            languages[1],
            json!({
                "language":"go","nativeParser":"source","indexedFiles":1,"scipToolchain":null,
                "rootMarkers":[],"generationEligibility":"notWired","capability":"notApplicable","setupHint":null
            })
        );
        assert_eq!(languages[3]["nativeParser"], "documentation");
        assert_eq!(languages[4]["nativeParser"], "unsupported");
        assert_eq!(
            languages[6],
            json!({
                "language":"rust","nativeParser":"source","indexedFiles":2,"scipToolchain":"rust-analyzer",
                "rootMarkers":["Cargo.toml"],"generationEligibility":"candidate","capability":"notProbed",
                "setupHint":"rustup component add rust-analyzer"
            })
        );

        let artifact = dunce::canonicalize(repo.root().join("fixture.scip")).unwrap();
        assert_eq!(assess_artifact(&repo, &artifact)["state"], "unregistered");
        let result = repo.index(&["--auto-scip", "--scip", artifact.to_str().unwrap()]);
        let scip = &result["scip"];
        assert_eq!(scip["status"], "success");
        assert_eq!(
            scip["selection"],
            json!({"mode":"imported","selected_toolchain":null,"rows":[]})
        );
        assert_eq!(scip["edges_added"], 0);
        assert_eq!(scip["edges_updated"], 1);
        assert_eq!(scip["definitions_mapped"], 2);
        assert_eq!(scip["references_seen"], 3);
        assert_eq!(scip["edges_skipped_unmapped"], 1);
        assert_eq!(scip["edges_skipped_invalid_occ_range"], 1);
        expected[3]["provenance"] = json!("scipReference");
        assert_eq!(repo.report()["relationships"]["witnesses"], json!(expected));
        assert_eq!(
            assess_artifact(&repo, &artifact),
            json!({
                "state":"matchesRegisteredBytes","sourceFreshness":"unknown","reasons":[]
            })
        );

        let db = rusqlite::Connection::open(repo.db()).unwrap();
        db.execute(
            "DELETE FROM structural_edges WHERE evidence = 'scip:ref'",
            [],
        )
        .unwrap();
        assert_eq!(
            assess_artifact(&repo, &artifact)["state"],
            "matchesRegisteredBytes"
        );
        fs::write(&artifact, "changed artifact").unwrap();
        assert_eq!(
            assess_artifact(&repo, &artifact)["state"],
            "changedSinceRegistration"
        );
        fs::remove_file(&artifact).unwrap();
        assert_eq!(assess_artifact(&repo, &artifact)["state"], "unknown");
        assert_eq!(
            assess_artifact(&repo, Path::new("fixture.scip"))["reasons"],
            json!(["artifactSpellingUnverified"])
        );

        let malformed = repo.root().join("malformed.scip");
        let failed = repo.index(&["--scip", malformed.to_str().unwrap()]);
        assert_eq!(failed["scip"]["status"], "failed");
        assert_eq!(failed["scip"]["selection"]["mode"], "imported");
        assert_eq!(repo.report()["relationships"]["totalRows"], 6);
    }
    #[rstest]
    #[case(100, false)]
    #[case(101, true)]
    fn dependency_evidence_groups_and_caps_in_sql(#[case] n: u64, #[case] truncated: bool) {
        let repo = fixture();
        repo.index(&[]);
        let mut db = rusqlite::Connection::open(repo.db()).unwrap();
        let tx = db.transaction().unwrap();
        tx.execute("DELETE FROM structural_edges", []).unwrap();
        let file = tx
            .query_row(
                "SELECT id FROM project_files WHERE file_path='main.rs'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        let target = tx
            .query_row(
                "SELECT id FROM project_symbols WHERE symbol_name='rust_target'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        for i in (0..n).rev() {
            let name = format!("caller_{i:03}");
            tx.execute("INSERT INTO project_symbols(file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
                VALUES (?1, ?2, ?2, 'Function', '')", rusqlite::params![file, name]).unwrap();
            let caller = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO structural_edges(caller_symbol_id, caller_file_id, callee_symbol_id,
                callee_file_id, call_kind, resolution_status, evidence) VALUES
                (?1, ?2, ?3, ?2, 'DIRECT', 'RESOLVED', 'call_expr:private source text'),
                (?1, ?2, ?3, ?2, 'DIRECT', 'RESOLVED', 'call_expr:distinct private text')",
                rusqlite::params![caller, file, target],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let a = repo.call(REPORT);
        let b = repo.call(REPORT);
        assert_eq!(a.stdout, b.stdout);
        let relation = json_output(a)["relationships"].clone();
        assert_eq!(relation["state"], "observed");
        assert_eq!(relation["totalRows"], n * 2);
        assert_eq!(relation["totalWitnesses"], n);
        assert_eq!(relation["truncated"], truncated);
        assert_eq!(
            relation["byCallerLanguage"],
            json!([{"language":"rust","rows":n*2}])
        );
        assert_eq!(
            relation["byProvenanceAndResolution"],
            json!([{"provenance":"nativeCall","resolution":"resolved","rows":n*2}])
        );
        let rows = relation["witnesses"].as_array().unwrap();
        assert_eq!(rows.len(), 100);
        assert_eq!(rows[0]["callerSymbol"], "caller_000");
        assert_eq!(rows[99]["callerSymbol"], "caller_099");
        assert_eq!(rows[0]["equivalentRows"], 2);
        assert!(!relation.to_string().contains("private source text"));
        assert!(
            !String::from_utf8(repo.call(&REPORT[..3]).stdout)
                .unwrap()
                .contains("distinct private text")
        );
    }

    #[test]
    fn dependency_evidence_unknown_dangling_and_foreign_witnesses_are_private() {
        let repo = fixture();
        repo.index(&[]);
        let db = rusqlite::Connection::open(repo.db()).unwrap();
        db.execute_batch(
            r"PRAGMA foreign_keys=OFF;
            UPDATE project_files SET file_path = 'nested\go.go' WHERE file_path='unwired.go';
            UPDATE project_files SET file_path = 'C:\FOREIGN_SECRET\main.ts' WHERE file_path='main.ts';
            UPDATE project_files SET file_path = 'nested\main.py' WHERE file_path='main.py';
            UPDATE structural_edges SET evidence = 'PRIVATE_SECRET_EXPRESSION', unresolved_callee = 'PRIVATE_SECRET_CALL'
               WHERE caller_symbol_id = (SELECT id FROM project_symbols WHERE symbol_name='rust_caller');
            UPDATE structural_edges SET callee_symbol_id = 999999999
               WHERE caller_symbol_id = (SELECT id FROM project_symbols WHERE symbol_name='py_caller');"
        ).unwrap();
        let machine = repo.call(REPORT);
        let human = repo.call(&REPORT[..3]);
        for out in [&machine, &human] {
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(!text.contains("FOREIGN_SECRET"));
            assert!(!text.contains("PRIVATE_SECRET"));
            assert!(text.contains("nested/main.py"));
        }
        let report = json_output(machine);
        let relation = &report["relationships"];
        assert_eq!(relation["state"], "partial");
        for reason in ["danglingReference", "unknownEvidence", "unsafePath"] {
            assert!(
                relation["reasons"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(reason))
            );
        }
        assert!(
            relation["witnesses"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["provenance"] == "unknown")
                .all(|row| row["calleeFile"].is_null() && row["calleeSymbol"].is_null())
        );
    }

    #[test]
    fn dependency_evidence_sql_surface_failure_preserves_independent_facts() {
        let repo = fixture();
        repo.index(&[]);
        let db = rusqlite::Connection::open(repo.db()).unwrap();
        db.execute_batch("DROP TABLE structural_edges;").unwrap();
        let report = repo.report();
        assert_eq!(
            report["relationships"]["reasons"],
            json!(["relationshipReadFailed"])
        );
        #[cfg(any(target_os = "linux", windows))]
        assert_eq!(report["native"]["state"], "indexInputsMatch");
        #[cfg(not(any(target_os = "linux", windows)))]
        assert!(
            report["native"]["reasons"]
                .as_array()
                .unwrap()
                .contains(&json!("discoveryUnavailable"))
        );
        let rust = report["languages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["language"] == "rust")
            .unwrap();
        assert_eq!(rust["indexedFiles"], 2);
        assert_eq!(report["scipArtifact"]["state"], "notChecked");
    }

    #[test]
    #[cfg(any(target_os = "linux", windows))]
    fn dependency_evidence_freshness_distinguishes_same_metadata_content_and_head() {
        let repo = fixture();
        repo.index(&[]);
        let original = repo.root().join("main.rs");
        let time =
            filetime::FileTime::from_last_modification_time(&fs::metadata(&original).unwrap());
        let input = fs::read_to_string(&original).unwrap();
        fs::write(&original, input.replacen("rust_target", "rust_differ", 1)).unwrap();
        filetime::set_file_mtime(&original, time).unwrap();
        let report = repo.report();
        assert_eq!(report["native"]["state"], "stale");
        assert_eq!(
            report["native"]["indexedHead"],
            report["native"]["assessedHead"]
        );
        assert_eq!(report["native"]["reasons"], json!(["changedSource"]));
        repo.checkpoint("changed HEAD");
        assert_eq!(
            repo.report()["native"]["reasons"],
            json!(["changedSource", "headMismatch"])
        );
    }

    #[test]
    fn dependency_evidence_artifact_never_aliases_a_registered_key() {
        let repo = fixture();
        repo.index(&[]);
        let db = rusqlite::Connection::open(repo.db()).unwrap();
        let artifact = dunce::canonicalize(repo.root().join("fixture.scip")).unwrap();
        let alias = artifact.with_file_name("Fixture.scip");
        db.execute(
            "INSERT INTO scip_indices(index_path, blake3_hash, indexed_at) VALUES (?1, 'fake', '')",
            [alias.to_str().unwrap()],
        )
        .unwrap();
        assert_eq!(assess_artifact(&repo, &artifact)["state"], "unregistered");
        db.execute("INSERT INTO scip_indices(index_path, blake3_hash, indexed_at) VALUES ('fixture.scip', 'fake', '')", []).unwrap();
        assert_eq!(
            assess_artifact(&repo, &artifact)["reasons"],
            json!(["relativeRegistration"])
        );
    }
    fn install_fake_tool(repo: &Repo, name: &str, action: &str) {
        #[cfg(windows)]
        {
            let generation = match action {
                "failed" => "exit /b 13",
                "invalid" => "echo not-a-protobuf>ledgerful.temp.scip\r\nexit /b 0",
                _ => "exit /b 0",
            };
            let script = format!(
                "@echo off\r\nif \"%1\"==\"--version\" (\r\n  echo {name}-probe>>.test-config\\trace\r\n  exit /b 0\r\n)\r\necho {name}-generation>>.test-config\\trace\r\n{generation}\r\n"
            );
            fs::write(repo.home.join(format!("{name}.cmd")), script).unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let generation = match action {
                "failed" => "exit 13",
                "invalid" => "printf 'not-a-protobuf' > ledgerful.temp.scip\nexit 0",
                _ => "exit 0",
            };
            let script = format!(
                "#!/bin/sh\nif [ \"$1\" = --version ]; then\n  echo {name}-probe >> .test-config/trace\n  exit 0\nfi\necho {name}-generation >> .test-config/trace\n{generation}\n"
            );
            let path = repo.home.join(name);
            fs::write(&path, script).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[rstest]
    #[case(false, "failed", "generation failed")]
    #[case(false, "missing", "generation failed")]
    #[case(true, "failed", "generation failed")]
    #[case(true, "invalid", "ingestion failed")]
    #[cfg(any(windows, unix))]
    fn dependency_evidence_scip_selection_runs_one_fake_tool_and_keeps_native_floor(
        #[case] graph: bool,
        #[case] action: &str,
        #[case] stage: &str,
    ) {
        let repo = fixture();
        install_fake_tool(&repo, "rust-analyzer", action);
        install_fake_tool(&repo, "scip-typescript", "failed");
        install_fake_tool(&repo, "scip-python", "failed");
        let trace = repo.home.join("trace");
        assert_eq!(repo.report()["kind"], "dependencyEvidence");
        assert!(!trace.exists(), "passive mode must not start a probe");
        let mut args = vec!["index", "--auto-scip", "--json"];
        if graph {
            args.push("--analyze-graph");
        }
        let result = repo.packet(&args);
        let scip = &result["scip"];
        assert_eq!(scip["status"], "failed");
        assert!(
            scip["message"].as_str().unwrap().starts_with(stage),
            "{scip}"
        );
        assert_eq!(
            scip["selection"],
            json!({
                "mode":"automatic","selected_toolchain":"rust-analyzer","rows":[
                    {"toolchain":"rust-analyzer","root_markers":["Cargo.toml"],"status":"selected"},
                    {"toolchain":"scip-typescript","root_markers":["tsconfig.json"],"status":"not_probed_after_selection"},
                    {"toolchain":"scip-python","root_markers":["requirements.txt"],"status":"not_probed_after_selection"}
                ]
            })
        );
        let attempts = fs::read_to_string(trace).unwrap();
        assert_eq!(
            attempts.lines().collect::<Vec<_>>(),
            ["rust-analyzer-probe", "rust-analyzer-generation"]
        );
        assert_eq!(repo.report()["relationships"]["totalRows"], 6);
        assert!(!repo.root().join("ledgerful.temp.scip").exists());
    }

    #[test]
    #[cfg(any(windows, unix))]
    fn dependency_evidence_import_precedence_skips_fake_tools() {
        let repo = fixture();
        install_fake_tool(&repo, "rust-analyzer", "failed");
        let input = repo.root().join("fixture.scip");
        let result = repo.packet(&[
            "index",
            "--auto-scip",
            "--scip",
            input.to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(result["scip"]["status"], "success");
        assert_eq!(
            result["scip"]["selection"],
            json!({
                "mode":"imported","selected_toolchain":null,"rows":[]
            })
        );
        assert!(!repo.home.join("trace").exists());
    }
    #[test]
    fn dependency_evidence_initialized_rejection_and_report_leave_rows_unchanged() {
        let repo = fixture();
        repo.index(&[]);
        let db = repo.db();
        let before = fs::read(&db).unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        let count = |sql| conn.query_row(sql, [], |row| row.get::<_, i64>(0)).unwrap();
        let before_version = count("PRAGMA user_version");
        let before_edges = count("SELECT COUNT(*) FROM structural_edges");
        let before_timings = count("SELECT COUNT(*) FROM command_timings");
        let mut args = REPORT.to_vec();
        args.extend(["--semantic-dry-run", "must-not-write.json"]);
        let rejected = repo.call(&args);
        assert_eq!(rejected.status.code(), Some(2), "{rejected:?}");
        assert_eq!(repo.report()["kind"], "dependencyEvidence");
        assert!(!repo.root().join("must-not-write.json").exists());
        assert_eq!(count("PRAGMA user_version"), before_version);
        assert_eq!(count("SELECT COUNT(*) FROM structural_edges"), before_edges);
        assert_eq!(
            count("SELECT COUNT(*) FROM command_timings"),
            before_timings
        );
        assert_eq!(fs::read(&db).unwrap(), before);
    }
    #[test]
    #[cfg(any(target_os = "linux", windows))]
    fn dependency_evidence_matches_native_ignore_scope_and_global_precedence() {
        let repo = fixture();
        let local = repo.root().join(".test-config/local.ignore");
        fs::write(&local, "added.rs\n").unwrap();
        repo.git(&["config", "core.excludesFile", local.to_str().unwrap()]);
        repo.index(&[]);
        fs::write(repo.root().join("added.rs"), "fn newly_visible() {}").unwrap();
        let before = repo.report();
        assert_eq!(before["native"]["state"], "stale");
        assert_eq!(before["native"]["reasons"], json!(["unindexedSource"]));

        repo.index(&[]);
        let conn = rusqlite::Connection::open(repo.db()).unwrap();
        let count_added = || {
            conn.query_row(
            "SELECT COUNT(*) FROM project_files WHERE file_path = 'added.rs' AND parse_status != 'DELETED'", [],
            |row| row.get::<_, i64>(0),
        ).unwrap()
        };
        assert_eq!(
            count_added(),
            1,
            "ordinary discovery sees the repo-locally excluded file"
        );
        assert_eq!(repo.report()["native"]["state"], "indexInputsMatch");

        fs::write(repo.root().join("global-fixture.ignore"), "added.rs\n").unwrap();
        fs::write(
            repo.home.join("gitconfig"),
            "[core]\nexcludesfile = global-fixture.ignore\n",
        )
        .unwrap();
        let now_hidden = repo.report();
        assert_eq!(now_hidden["native"]["state"], "unknown");
        assert_eq!(
            now_hidden["native"]["reasons"],
            json!(["inventoryUncertain"])
        );
        repo.index(&[]);
        assert_eq!(
            count_added(),
            0,
            "ordinary discovery honors the global exclusion"
        );
        assert_eq!(repo.report()["native"]["state"], "indexInputsMatch");
    }
}
