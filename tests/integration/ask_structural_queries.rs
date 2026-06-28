use ledgerful::commands::ask::execute_ask;
use ledgerful::gemini::modes::GeminiMode;
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use serial_test::serial;
use std::process::Command;
use tempfile::tempdir;

use crate::common::{DirGuard, TempEnv, non_interactive, setup_git_repo};

fn seeded_storage(layout: &Layout) -> StorageManager {
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    {
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES \
             (1, 'src/main.rs', '2026-01-01T00:00:00Z'), \
             (2, 'src/cli/mod.rs', '2026-01-01T00:00:00Z'), \
             (3, 'src/routes.rs', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO project_symbols (id, file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at) VALUES \
             (1, 1, 'main', 'main', 'Function', '2026-01-01T00:00:00Z'), \
             (2, 2, 'run_with', 'run_with', 'Function', '2026-01-01T00:00:00Z'), \
             (3, 3, 'my_handler', 'my_handler', 'Function', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO structural_edges (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id) \
             VALUES (1, 1, 2, 2)",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO api_routes (method, path_pattern, handler_symbol_name, handler_file_id, framework, last_indexed_at) \
             VALUES ('GET', '/api/test', 'my_handler', 3, 'axum', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    }
    storage
}

#[test]
#[serial(env, cwd)]
fn test_ask_what_calls_run_with() {
    let _env_non_interactive = non_interactive();
    let _env_gemini = TempEnv::remove("GEMINI_API_KEY");

    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);
    std::process::Command::new("git")
        .arg("init")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage = seeded_storage(&layout);
    storage.shutdown().unwrap();

    let result = execute_ask(
        Some("what calls run_with".into()),
        false,
        10,
        GeminiMode::Analyze,
        false,
        None,
        false,
        15,
        false, // no_kg_fallback
        false, // auto_scan
    );
    assert!(
        result.is_ok(),
        "query must resolve without an LLM backend: {:?}",
        result
    );
}

#[test]
#[serial(env, cwd)]
fn test_ask_show_callers_of_run_with() {
    let _env_non_interactive = non_interactive();
    let _env_gemini = TempEnv::remove("GEMINI_API_KEY");

    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);
    std::process::Command::new("git")
        .arg("init")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage = seeded_storage(&layout);
    storage.shutdown().unwrap();

    let result = execute_ask(
        Some("show callers of run_with".into()),
        false,
        10,
        GeminiMode::Analyze,
        false,
        None,
        false,
        15,
        false, // no_kg_fallback
        false, // auto_scan
    );
    assert!(
        result.is_ok(),
        "query must resolve without an LLM backend: {:?}",
        result
    );
}

#[test]
#[serial(env, cwd)]
fn test_ask_find_all_axum_route_handlers() {
    let _env_non_interactive = non_interactive();
    let _env_gemini = TempEnv::remove("GEMINI_API_KEY");

    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);
    std::process::Command::new("git")
        .arg("init")
        .current_dir(tmp.path())
        .output()
        .unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage = seeded_storage(&layout);
    storage.shutdown().unwrap();

    let result = execute_ask(
        Some("find all Axum route handlers".into()),
        false,
        10,
        GeminiMode::Analyze,
        false,
        None,
        false,
        15,
        false, // no_kg_fallback
        false, // auto_scan
    );
    assert!(
        result.is_ok(),
        "query must resolve without an LLM backend: {:?}",
        result
    );
}

// --- CG-F31: command-discovery / repo-health routing ---
//
// These tests spawn the actual compiled `ledgerful` binary (rather than
// calling `execute_ask` in-process) so we can assert on the printed stdout
// *content* -- specifically, that the deterministic command-discovery path
// actually names the right commands, and that it short-circuits before any
// LLM-backend chatter (`Using Gemini...`/`Contacting LLM...`) would ever be
// reached. `GEMINI_API_KEY` is explicitly removed from the spawned process's
// environment so a fallback-to-LLM path would visibly differ (a hard error,
// since `execute_ask` requires a configured backend) if routing didn't
// actually short-circuit.

/// Sets up a bare git repo with an initialized (but otherwise empty)
/// `.ledgerful/state/ledger.db`, ready for the `ledgerful` binary to be
/// invoked against it via `current_dir`. No `ledgerful init`/index is
/// required: the CG-F31 routing path resolves entirely from the live CLI
/// metadata and returns before any index-dependent logic in `execute_ask`.
fn setup_bare_repo_for_ask() -> tempfile::TempDir {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    let state_dir = root.join(".ledgerful/state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let storage = StorageManager::init(&state_dir.join("ledger.db")).unwrap();
    storage.shutdown().unwrap();

    tmp
}

#[test]
#[serial(env, cwd)]
fn test_ask_what_commands_show_repo_health_resolves_deterministically() {
    let tmp = setup_bare_repo_for_ask();
    let root = tmp.path();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["ask", "what commands show repo health?"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env_remove("GEMINI_API_KEY")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "CLI command failed: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("doctor"), "expected `doctor` in: {stdout}");
    assert!(stdout.contains("status"), "expected `status` in: {stdout}");
    assert!(stdout.contains("audit"), "expected `audit` in: {stdout}");
    assert!(
        !stdout.contains("Using Gemini"),
        "deterministic path must not print backend chatter, got: {stdout}"
    );
    assert!(
        !stdout.contains("Contacting LLM"),
        "deterministic path must not print backend chatter, got: {stdout}"
    );
}

#[test]
#[serial(env, cwd)]
fn test_ask_generic_command_discovery_for_hotspots_resolves_deterministically() {
    let tmp = setup_bare_repo_for_ask();
    let root = tmp.path();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["ask", "which command shows hotspots"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env_remove("GEMINI_API_KEY")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "CLI command failed: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("hotspots"),
        "expected the `hotspots` command to be named in: {stdout}"
    );
    assert!(
        stdout.contains("Command-discovery query resolved via live CLI metadata"),
        "expected the generic command-discovery path to be used, got: {stdout}"
    );
    assert!(
        !stdout.contains("Using Gemini"),
        "deterministic path must not print backend chatter, got: {stdout}"
    );
    assert!(
        !stdout.contains("Contacting LLM"),
        "deterministic path must not print backend chatter, got: {stdout}"
    );
}

#[test]
#[serial(env, cwd)]
fn test_ask_implementation_question_does_not_use_command_discovery_path() {
    let tmp = setup_bare_repo_for_ask();
    let root = tmp.path();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["ask", "how does calculate_hotspots compute scores"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env_remove("GEMINI_API_KEY")
        .output()
        .unwrap();

    // This query is implementation-flavored and contains no "command(s)"
    // wording, so it must not be claimed by the new CG-F31 routing. It is
    // not a recognized CG-F20 structural phrasing either, so with no LLM
    // backend configured in this environment, `execute_ask` is expected to
    // fail validation further down -- the negative control here is about
    // which path was *not* taken, not about overall command success.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Command-discovery query resolved via live CLI metadata"),
        "implementation-flavored question must not be claimed by the command-discovery path, got: {stdout}"
    );
}

#[test]
#[serial(env, cwd)]
fn test_ask_implementation_question_with_command_name_does_not_use_command_discovery_path() {
    let tmp = setup_bare_repo_for_ask();
    let root = tmp.path();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["ask", "how does the hotspots command compute scores"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env_remove("GEMINI_API_KEY")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Command-discovery query resolved via live CLI metadata"),
        "implementation-flavored question with 'command' must not be claimed by command-discovery, got: {stdout}"
    );
}

// --- CG-F35: cache freshness warning on `ask` ---

/// CG-F35 (requirement #1, #6): when the cached impact packet `ask` is about
// to use as LLM context is stale relative to current HEAD, `ask` must warn
// rather than silently feeding stale context. The warning is diagnostic
// chatter, not answer content, so it must land on stderr, never stdout.
#[test]
#[serial(env, cwd)]
fn test_ask_warns_on_stale_cached_impact_packet() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    std::fs::write(root.join("a.txt"), "v1").unwrap();
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(root)
        .output()
        .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    // Make a tracked change so `scan --impact` records a real (non-empty,
    // non-tombstone) packet, then snapshot it via `scan --impact`.
    std::fs::write(root.join("a.txt"), "v2").unwrap();
    let scan_out = Command::new(ledgerful_bin)
        .args(["scan", "--impact"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        scan_out.status.success(),
        "scan --impact failed: {}",
        String::from_utf8_lossy(&scan_out.stderr)
    );

    // Commit the change, advancing HEAD past what the packet above recorded
    // -- this is what `check_impact_freshness` calls "stale".
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "advance head past the cached packet"])
        .current_dir(root)
        .output()
        .unwrap();

    // A query that is not claimed by CG-F20/CG-F31 deterministic routing,
    // so execution reaches the packet-freshness check. No LLM backend is
    // configured in this environment, so the command is expected to fail
    // further down -- the warning must still have been printed to stderr
    // before that failure, since the freshness check runs ahead of backend
    // validation.
    let output = Command::new(ledgerful_bin)
        .args(["ask", "tell me about the architecture of this codebase"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env_remove("GEMINI_API_KEY")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.to_lowercase().contains("stale"),
        "expected a staleness warning on stderr, got stderr: {stderr}"
    );
    assert!(
        !stdout.to_lowercase().contains("stale cached impact"),
        "staleness chatter must not leak onto stdout, got stdout: {stdout}"
    );
}

/// CG-F35 review fix: a stale *clean-tree tombstone* must also warn, not just
// a stale non-empty packet. `ask` flips into global-mode whenever the
// latest packet has empty `changes` (a clean-tree tombstone is exactly
// that), and the original freshness-warning gate (`!is_global`) skipped the
// check entirely once that flip happened -- regardless of whether the
// tombstone itself was stale. `check_impact_freshness` (src/state/reports.rs)
// classifies a clean-tree tombstone as `Stale` when `HEAD` has moved since
// the clean scan, which is exactly what this test constructs: `scan
// --impact` runs against a clean tree (writing a `CleanTreeTombstone` with
// the HEAD hash at that point), then a further commit advances HEAD past
// it, so by the time `ask` reads the cached packet, `tombstone.head_hash !=
// current HEAD` and `check_impact_freshness` returns `Stale`. This is
// exactly the "clean scan, then make changes, then run `ask`" workflow the
// track exists to fix.
#[test]
#[serial(env, cwd)]
fn test_ask_warns_on_stale_clean_tree_tombstone() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    std::fs::write(root.join("a.txt"), "v1").unwrap();
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(root)
        .output()
        .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();

    // Run `scan --impact` against a clean working tree (no uncommitted
    // changes) so the cached packet written is a `CleanTreeTombstone`
    // (`tree_clean: true`, `changes: []`) rather than a real packet.
    let scan_out = Command::new(ledgerful_bin)
        .args(["scan", "--impact"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        scan_out.status.success(),
        "scan --impact failed: {}",
        String::from_utf8_lossy(&scan_out.stderr)
    );

    // Advance HEAD past the tombstone's recorded hash -- this is what
    // `check_impact_freshness`'s `CleanTree` branch calls "stale" (`HEAD has
    // changed since clean impact scan`).
    std::fs::write(root.join("b.txt"), "v1").unwrap();
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "advance head past the clean-tree tombstone"])
        .current_dir(root)
        .output()
        .unwrap();

    // A query that is not claimed by CG-F20/CG-F31 deterministic routing, so
    // execution reaches the packet-freshness check. No LLM backend is
    // configured in this environment, so the command is expected to fail
    // further down -- the warning must still have been printed to stderr
    // before that failure.
    let output = Command::new(ledgerful_bin)
        .args(["ask", "tell me about the architecture of this codebase"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env_remove("GEMINI_API_KEY")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.to_lowercase().contains("stale"),
        "expected a staleness warning on stderr for a stale clean-tree tombstone, got stderr: {stderr}"
    );
    assert!(
        !stdout.to_lowercase().contains("stale cached impact"),
        "staleness chatter must not leak onto stdout, got stdout: {stdout}"
    );
}
