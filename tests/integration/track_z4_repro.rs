use crate::common::setup_git_repo;
use camino::Utf8Path;
use ledgerful::state::storage::StorageManager;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_cargo_lock_ingestion() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let root_utf8 = Utf8Path::from_path(root).unwrap();

    setup_git_repo(root);
    fs::write(
        root.join("Cargo.lock"),
        r#"
[[package]]
name = "serde"
version = "1.0.152"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "bb132488d2348f7a79a296f187a7412ee291e0a24f0a0d9223011400e955f134"
"#,
    )
    .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    // Init and Index with --analyze-graph
    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    Command::new(ledgerful_bin)
        .args(["index", "--analyze-graph"])
        .current_dir(root)
        .output()
        .unwrap();

    // Check CozoDB
    let storage = StorageManager::open_read_only(root_utf8).unwrap();
    let cozo = storage.cozo.as_ref().expect("CozoDB should be initialized");

    let res = cozo
        .run_script("?[id, label] := *node{id, label, category: 'package'}")
        .unwrap();
    assert!(!res.rows.is_empty(), "CozoDB should have package nodes");

    let found_serde = res.rows.iter().any(|row| {
        row.first()
            .map(|v| v.to_string().contains("serde"))
            .unwrap_or(false)
    });
    assert!(
        found_serde,
        "Should have found serde package with correct URN"
    );

    // Verify edges
    let _edge_res = cozo
        .run_script("?[src, tgt] := *edge{source: src, target: tgt, relation: 'depends_on'}")
        .unwrap();
    // In our mock lock, serde has no deps, but we can add another package with deps to verify edges.
}

#[test]
fn test_cargo_lock_edges() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let root_utf8 = Utf8Path::from_path(root).unwrap();

    setup_git_repo(root);
    fs::write(
        root.join("Cargo.lock"),
        r#"
[[package]]
name = "anyhow"
version = "1.0.0"

[[package]]
name = "thiserror"
version = "1.0.0"
dependencies = [
 "anyhow",
]
"#,
    )
    .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    Command::new(ledgerful_bin)
        .args(["index", "--analyze-graph"])
        .current_dir(root)
        .output()
        .unwrap();

    let storage = StorageManager::open_read_only(root_utf8).unwrap();
    let cozo = storage.cozo.as_ref().expect("CozoDB should be initialized");

    let edge_res = cozo
        .run_script("?[src, tgt] := *edge{source: src, target: tgt, relation: 'depends_on'}")
        .unwrap();
    assert!(!edge_res.rows.is_empty(), "Should have depends_on edges");

    let found_edge = edge_res.rows.iter().any(|row| {
        let src = row.first().unwrap();
        let tgt = row.get(1).unwrap();
        if let (cozo::DataValue::Str(s_str), cozo::DataValue::Str(t_str)) = (src, tgt) {
            s_str.as_str() == "urn:ledgerful:package:thiserror:1.0.0"
                && t_str.as_str() == "urn:ledgerful:package:anyhow:1.0.0"
        } else {
            false
        }
    });
    assert!(found_edge, "Should have edge from thiserror to anyhow");
}

#[test]
fn test_cargo_lock_version_disambiguation() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let root_utf8 = Utf8Path::from_path(root).unwrap();

    setup_git_repo(root);
    fs::write(
        root.join("Cargo.lock"),
        r#"
[[package]]
name = "regex"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "regex"
version = "2.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "consumer"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "regex",
]
"#,
    )
    .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    Command::new(ledgerful_bin)
        .args(["index", "--analyze-graph"])
        .current_dir(root)
        .output()
        .unwrap();

    let storage = StorageManager::open_read_only(root_utf8).unwrap();
    let cozo = storage.cozo.as_ref().expect("CozoDB should be initialized");

    // Query for DependsOn edges from consumer. Join with node table on stable label.
    let res = cozo
        .run_script(
            r#"
            ?[src, tgt] := *edge{source: src, target: tgt, relation: 'depends_on'},
                           *node{id: src, label: 'consumer@0.1.0'}
            "#,
        )
        .unwrap();

    // Resolve bare dependency: it should pick EXACTLY ONE edge (fulfilling Z-R1 requirement).
    assert_eq!(
        res.rows.len(),
        1,
        "Should have exactly ONE DependsOn edge from consumer (Z-R1 requirement)"
    );
}

#[test]
fn test_cargo_lock_git_and_path_deps() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let root_utf8 = Utf8Path::from_path(root).unwrap();

    setup_git_repo(root);
    fs::write(
        root.join("Cargo.lock"),
        r#"
[[package]]
name = "git-dep"
version = "0.1.0"
source = "git+https://github.com/example/repo#sha"

[[package]]
name = "path-dep"
version = "0.1.0"

[[package]]
name = "root"
version = "0.1.0"
dependencies = [
 "git-dep",
 "path-dep",
]
"#,
    )
    .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    Command::new(ledgerful_bin)
        .args(["index", "--analyze-graph"])
        .current_dir(root)
        .output()
        .unwrap();

    let storage = StorageManager::open_read_only(root_utf8).unwrap();
    let cozo = storage.cozo.as_ref().expect("CozoDB should be initialized");

    let res = cozo
        .run_script(
            r#"
            ?[id, meta] := *node{id: id, metadata: meta, category: 'package'}
            "#,
        )
        .unwrap();

    let mut found_git = false;
    let mut found_path = false;

    for row in res.rows {
        let id = row[0].to_string();
        let meta: serde_json::Value = serde_json::from_str(&row[1].to_string()).unwrap();

        if id.contains("git-dep") {
            found_git = true;
            assert!(
                meta.get("source").is_some(),
                "git-dep should have source in metadata"
            );
        }
        if id.contains("path-dep") {
            found_path = true;
            assert_eq!(
                meta.get("name").unwrap().as_str().unwrap(),
                "path-dep",
                "path-dep should have correct name in metadata"
            );
        }
    }

    assert!(found_git, "git-dep node not found");
    assert!(found_path, "path-dep node not found");
}

#[test]
fn test_cargo_lock_weak_fallback() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let root_utf8 = Utf8Path::from_path(root).unwrap();

    setup_git_repo(root);
    // Malformed TOML: version is missing in one package.
    // This should fail typed parsing but work with weak parsing (defaulting to 0.0.0).
    fs::write(
        root.join("Cargo.lock"),
        r#"
[[package]]
name = "malformed"
# version is missing

[[package]]
name = "dependent"
version = "0.1.0"
dependencies = [
 "malformed",
]
"#,
    )
    .unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    Command::new(ledgerful_bin)
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    Command::new(ledgerful_bin)
        .args(["index", "--analyze-graph"])
        .current_dir(root)
        .output()
        .unwrap();

    let storage = StorageManager::open_read_only(root_utf8).unwrap();
    let cozo = storage.cozo.as_ref().expect("CozoDB should be initialized");

    let res = cozo
        .run_script("?[id] := *node{id: id, category: 'package'}")
        .unwrap();

    let found_malformed = res.rows.iter().any(|row| {
        if let cozo::DataValue::Str(s) = &row[0] {
            s.as_str() == "urn:ledgerful:package:malformed:0.0.0"
        } else {
            false
        }
    });
    assert!(
        found_malformed,
        "Should have found malformed package via weak fallback"
    );

    let edge_res = cozo
        .run_script("?[src, tgt] := *edge{source: src, target: tgt, relation: 'depends_on'}")
        .unwrap();
    assert!(
        !edge_res.rows.is_empty(),
        "Should have edges even with weak fallback"
    );
}
