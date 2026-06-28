//! Regression test for Codex finding #2 (docs/codex-findings1.md): `dependencies list`
//! misclassified real third-party crates (gix, rusqlite, cozo) as "Local Dependencies"
//! because stale `package` nodes from a previous Cargo.lock version lingered in the
//! graph after a version bump, sitting alongside the correct, sourced, current-version
//! node under a different URN (URNs are keyed by name+version[+source]).
//!
//! The fix lives in `phase_cargo_dependencies` (src/index/graph_loader.rs): on each
//! re-index, any `package` node whose URN is not part of the current Cargo.lock's
//! package set is now pruned (along with its outgoing DependsOn edges) before/around
//! inserting the fresh node set.

use crate::common::setup_git_repo;
use camino::Utf8PathBuf;
use ledgerful::config::model::Config;
use ledgerful::index::graph_loader::build_native_graph;
use ledgerful::index::orchestrator::ProjectIndexer;
use ledgerful::state::storage::StorageManager;
use std::fs;

struct TestHarness {
    _tmp: tempfile::TempDir,
    pub root: Utf8PathBuf,
    pub indexer: ProjectIndexer,
}

impl TestHarness {
    fn new_with_lock(lock_content: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8 path");

        setup_git_repo(root.as_std_path());
        fs::create_dir_all(root.join("src")).expect("src dir");
        fs::write(root.join("src").join("lib.rs"), "pub fn noop() {}").expect("lib.rs");
        fs::write(root.join("Cargo.lock"), lock_content).expect("Cargo.lock");

        let db_dir = root.join(".ledgerful").join("state");
        fs::create_dir_all(&db_dir).expect("state dir");
        let storage =
            StorageManager::init(db_dir.join("ledger.db").as_std_path()).expect("storage init");

        let config = Config::default();
        let mut indexer = ProjectIndexer::new(storage, root.clone(), config);
        indexer.full_index().expect("full_index");
        indexer.build_call_graph().expect("call_graph");

        Self {
            _tmp: tmp,
            root,
            indexer,
        }
    }

    fn build_graph(&self) {
        let cozo = self.indexer.cozo().expect("cozo available");
        let config = Config::default();
        build_native_graph(self.indexer.storage(), cozo, "full", &config)
            .expect("build_native_graph");
    }

    fn cozo(&self) -> &ledgerful::state::storage_cozo::CozoStorage {
        self.indexer.cozo().expect("cozo")
    }

    fn write_lock(&self, lock_content: &str) {
        fs::write(self.root.join("Cargo.lock"), lock_content).expect("rewrite Cargo.lock");
    }

    fn package_node_ids(&self) -> Vec<String> {
        self.cozo()
            .run_script("?[id] := *node{id, category: 'package'}")
            .map(|r| {
                r.rows
                    .into_iter()
                    .filter_map(|row| match row.into_iter().next() {
                        Some(cozo::DataValue::Str(s)) => Some(s.to_string()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

const LOCK_V1: &str = r#"
[[package]]
name = "gix"
version = "0.83.0"

[[package]]
name = "rusqlite"
version = "0.39.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;

const LOCK_V2: &str = r#"
[[package]]
name = "gix"
version = "0.84.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "rusqlite"
version = "0.40.1"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;

#[test]
fn test_stale_package_version_pruned_on_lockfile_bump() {
    let harness = TestHarness::new_with_lock(LOCK_V1);

    // First build: only the v1 package nodes should exist.
    harness.build_graph();
    let ids_v1 = harness.package_node_ids();
    assert!(
        ids_v1
            .iter()
            .any(|id| id == "urn:ledgerful:package:gix:0.83.0"),
        "expected stale-version gix node to exist after first build, got: {ids_v1:?}"
    );
    assert_eq!(
        ids_v1.len(),
        2,
        "expected exactly 2 package nodes after first build, got: {ids_v1:?}"
    );

    // Act: bump the lockfile to newer versions (simulating `cargo update`) and re-run
    // the same graph-build phase that ingests Cargo.lock.
    harness.write_lock(LOCK_V2);
    harness.build_graph();

    let ids_v2 = harness.package_node_ids();

    // The old versions must be gone -- this is the orphan-cleanup fix under test.
    assert!(
        !ids_v2
            .iter()
            .any(|id| id == "urn:ledgerful:package:gix:0.83.0"),
        "stale gix@0.83.0 node should have been pruned, still present in: {ids_v2:?}"
    );
    assert!(
        !ids_v2.iter().any(|id| id.contains("rusqlite:0.39.0")),
        "stale rusqlite@0.39.0 node should have been pruned, still present in: {ids_v2:?}"
    );

    // The new, correctly-sourced versions must be present.
    assert!(
        ids_v2.iter().any(|id| id.contains("gix:0.84.0")),
        "expected current gix@0.84.0 node to be present, got: {ids_v2:?}"
    );
    assert!(
        ids_v2.iter().any(|id| id.contains("rusqlite:0.40.1")),
        "expected current rusqlite@0.40.1 node to be present, got: {ids_v2:?}"
    );

    assert_eq!(
        ids_v2.len(),
        2,
        "expected exactly 2 package nodes after re-index (old versions pruned), got: {ids_v2:?}"
    );
}

#[test]
fn test_dependencies_list_no_longer_shows_stale_version_as_local() {
    let harness = TestHarness::new_with_lock(LOCK_V1);
    harness.build_graph();

    harness.write_lock(LOCK_V2);
    harness.build_graph();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = std::process::Command::new(ledgerful_bin)
        .args(["dependencies", "list", "--json"])
        .current_dir(&harness.root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json_val: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let arr = json_val.as_array().expect("expected a JSON array");

    // gix and rusqlite must each appear exactly once now, sourced, and not local --
    // the stale sourceless old-version duplicate must be gone.
    let gix_entries: Vec<&serde_json::Value> = arr
        .iter()
        .filter(|e| e["name"].as_str() == Some("gix"))
        .collect();
    assert_eq!(
        gix_entries.len(),
        1,
        "expected exactly one gix entry, got: {gix_entries:?}"
    );
    assert_eq!(gix_entries[0]["version"].as_str().unwrap(), "0.84.0");
    assert!(!gix_entries[0]["is_local"].as_bool().unwrap());

    let rusqlite_entries: Vec<&serde_json::Value> = arr
        .iter()
        .filter(|e| e["name"].as_str() == Some("rusqlite"))
        .collect();
    assert_eq!(
        rusqlite_entries.len(),
        1,
        "expected exactly one rusqlite entry, got: {rusqlite_entries:?}"
    );
    assert_eq!(rusqlite_entries[0]["version"].as_str().unwrap(), "0.40.1");
    assert!(!rusqlite_entries[0]["is_local"].as_bool().unwrap());
}

/// Regression test: `phase_cargo_dependencies`'s orphan-cleanup logic (added to fix
/// the stale-version-pruning bug above) must never delete `package` nodes created by
/// the *separate* OSV advisory-import path (`OsvImporter::populate_kg`,
/// src/index/advisories.rs, used by `dependencies audit`).
///
/// That importer uses a different URN scheme (`urn:ledgerful:package:{name}`, no
/// version/source) which never matches `phase_cargo_dependencies`'s
/// `build_pkg_urn`-produced URNs. Without scoping the cleanup query to only nodes this
/// phase itself created (tagged `metadata.manifest == "Cargo.lock"`), every OSV-imported
/// package node looks "stale" on the very next `index --analyze-graph` run and gets
/// silently deleted, destroying security advisory data.
///
/// Sequence: `dependencies audit <osv.json>` (creates an OSV package node) -> `index
/// --analyze-graph` (re-index, the "next index run" that triggers the cleanup) -> assert
/// the OSV-imported package node is still present.
#[test]
fn test_osv_imported_package_node_survives_reindex() {
    let harness = TestHarness::new_with_lock(LOCK_V1);

    let osv_json = r#"{
        "results": [
            {
                "source": {
                    "path": "Cargo.lock",
                    "type": "lockfile"
                },
                "packages": [
                    {
                        "package": {
                            "name": "totally-unique-osv-only-package",
                            "version": "9.9.9",
                            "ecosystem": "crates.io"
                        },
                        "vulnerabilities": [
                            {
                                "id": "GHSA-osv-test-0001",
                                "summary": "Test advisory for regression coverage",
                                "details": "Synthetic vulnerability used only by this test.",
                                "modified": "2026-01-01T00:00:00Z",
                                "published": "2026-01-01T00:00:00Z"
                            }
                        ]
                    }
                ]
            }
        ]
    }"#;
    let osv_path = harness.root.join("osv-report.json");
    fs::write(osv_path.as_std_path(), osv_json).expect("write osv fixture");

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    // 1. `dependencies audit` creates the OSV-imported package node via
    //    `OsvImporter::populate_kg`, using the bare-name URN scheme.
    let audit_output = std::process::Command::new(ledgerful_bin)
        .args(["dependencies", "audit", "--input", "osv-report.json"])
        .current_dir(&harness.root)
        .output()
        .unwrap();
    assert!(
        audit_output.status.success(),
        "dependencies audit failed: {}",
        String::from_utf8_lossy(&audit_output.stderr)
    );

    let osv_urn = "urn:ledgerful:package:totally-unique-osv-only-package";
    let ids_after_audit = harness.package_node_ids();
    assert!(
        ids_after_audit.iter().any(|id| id == osv_urn),
        "expected OSV-imported package node to exist after `dependencies audit`, got: {ids_after_audit:?}"
    );

    // 2. `index --analyze-graph` re-runs `phase_cargo_dependencies`, which is where the
    //    orphan-cleanup bug lived: it must not treat the OSV node as stale just because
    //    it's outside the current Cargo.lock URN set.
    let index_output = std::process::Command::new(ledgerful_bin)
        .args(["index", "--analyze-graph"])
        .current_dir(&harness.root)
        .output()
        .unwrap();
    assert!(
        index_output.status.success(),
        "index --analyze-graph failed: {}",
        String::from_utf8_lossy(&index_output.stderr)
    );

    let ids_after_reindex = harness.package_node_ids();
    assert!(
        ids_after_reindex.iter().any(|id| id == osv_urn),
        "OSV-imported package node must survive `index --analyze-graph`, but it was \
         deleted by the Cargo.lock orphan-cleanup logic. Present ids: {ids_after_reindex:?}"
    );

    // Also confirm it's still visible via the user-facing `dependencies list --json`
    // surface, not just present at the raw graph-storage level.
    let list_output = std::process::Command::new(ledgerful_bin)
        .args(["dependencies", "list", "--json"])
        .current_dir(&harness.root)
        .output()
        .unwrap();
    assert!(
        list_output.status.success(),
        "dependencies list failed: {}",
        String::from_utf8_lossy(&list_output.stderr)
    );
    let list_stdout = String::from_utf8_lossy(&list_output.stdout);
    let list_json: serde_json::Value = serde_json::from_str(&list_stdout).unwrap();
    let list_arr = list_json.as_array().expect("expected a JSON array");
    assert!(
        list_arr
            .iter()
            .any(|e| e["name"].as_str() == Some("totally-unique-osv-only-package")),
        "expected OSV-imported package to still appear in `dependencies list --json` \
         after re-index, got: {list_arr:?}"
    );
}
