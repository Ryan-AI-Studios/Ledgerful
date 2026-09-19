//! 0357: hermetic FromRow extract + `--changed` `is_changed` (no INSERT).

use crate::common::{git_add_and_commit, run_cli_env, setup_git_repo};
use serde_json::Value;
use std::fs;
use tempfile::tempdir;

const BILLING_SRC: &str = r#"#[derive(sqlx::FromRow)]
pub struct BillingAccount {
    pub id: i64,
}
"#;

const LEDGER_MAPPER_SRC: &str = r#"pub struct LedgerEntry {
    pub id: i64,
}

fn map_ledger_entry(row: &rusqlite::Row) -> rusqlite::Result<LedgerEntry> {
    Ok(LedgerEntry { id: row.get(0)? })
}
"#;

fn parse_object(stdout: &str, label: &str) -> Value {
    let v: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("{label} stdout must parse as JSON: {e}\n{stdout}"));
    assert!(
        v.is_object(),
        "{label} must be an object envelope, got: {stdout}"
    );
    v
}

fn run_ni(root: &std::path::Path, args: &[&str]) -> (String, String, i32) {
    run_cli_env(root, args, &[("LEDGERFUL_NON_INTERACTIVE", "1")])
}

#[test]
#[allow(non_snake_case)]
fn data_models_fromrow_list_and_changed_is_changed__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("billing.rs"), BILLING_SRC).unwrap();
    git_add_and_commit(root, "initial");

    let (stdout, stderr, code) = run_ni(root, &["init"]);
    assert_eq!(code, 0, "init failed stdout={stdout} stderr={stderr}");
    git_add_and_commit(root, "post-init clean");

    let (stdout, stderr, code) = run_ni(root, &["index", "--incremental"]);
    assert_eq!(
        code, 0,
        "index --incremental failed stdout={stdout} stderr={stderr}"
    );

    let (stdout, stderr, code) = run_ni(root, &["data-models", "list", "--json"]);
    assert_eq!(
        code, 0,
        "list --json failed stdout={stdout} stderr={stderr}"
    );
    let list = parse_object(&stdout, "list --json");
    assert!(
        list["resultCount"].as_u64().unwrap_or(0) >= 1,
        "expected extracted BillingAccount: {list}"
    );
    let names: Vec<&str> = list["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["name"].as_str())
        .collect();
    assert!(
        names.contains(&"BillingAccount"),
        "list must extract BillingAccount, got {names:?} stdout={stdout}"
    );
    let account = list["models"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["name"] == "BillingAccount")
        .expect("BillingAccount row");
    assert_eq!(account["kind"], "SCHEMA");
    assert_eq!(account["fieldImpact"], "unsupported");
    assert_eq!(account["file_path"], "src/billing.rs");
    assert!(
        list["supportedExtractors"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|v| v == "rustPersistenceDerive"),
        "supportedExtractors: {}",
        list["supportedExtractors"]
    );
    assert!(
        list["notWired"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|v| v == "sqlMigrations"),
        "notWired: {}",
        list["notWired"]
    );

    let (stdout, stderr, code) = run_ni(root, &["data-models", "impact", "--changed", "--json"]);
    assert_eq!(
        code, 0,
        "pre-dirty --changed failed stdout={stdout} stderr={stderr}"
    );
    let clean = parse_object(&stdout, "pre-dirty --changed");
    assert_eq!(clean["emptyReason"], "cleanDiff", "{clean}");
    assert_eq!(clean["resultCount"], 0, "{clean}");
    assert!(
        clean["impacted"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(false),
        "pre-dirty impacted must be empty: {clean}"
    );

    fs::write(
        root.join("src").join("billing.rs"),
        r#"#[derive(sqlx::FromRow)]
pub struct BillingAccount {
    pub id: i64,
    pub name: String,
}
"#,
    )
    .unwrap();

    let (stdout, stderr, code) = run_ni(root, &["data-models", "impact", "--changed", "--json"]);
    assert_eq!(
        code, 0,
        "dirty --changed failed stdout={stdout} stderr={stderr}"
    );
    let dirty = parse_object(&stdout, "dirty --changed");
    assert_eq!(dirty["resultCount"], 1, "{dirty}");
    let row = &dirty["impacted"][0];
    assert_eq!(row["name"], "BillingAccount");
    assert_eq!(row["is_changed"], true);
    assert_eq!(row["file_path"], "src/billing.rs");
    assert_eq!(row["fieldImpact"], "unsupported");

    assert!(
        !root
            .join(".ledgerful")
            .join("reports")
            .join("latest-impact.json")
            .exists(),
        "must not write reports/latest-impact.json"
    );
    assert!(
        !root.join(".ledgerful").join("latest-impact.json").exists(),
        "must not write .ledgerful/latest-impact.json"
    );
}

#[test]
#[allow(non_snake_case)]
fn data_models_row_mapper_list_and_changed_is_changed__slow() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("ledger.rs"), LEDGER_MAPPER_SRC).unwrap();
    git_add_and_commit(root, "initial");

    let (stdout, stderr, code) = run_ni(root, &["init"]);
    assert_eq!(code, 0, "init failed stdout={stdout} stderr={stderr}");
    git_add_and_commit(root, "post-init clean");

    let (stdout, stderr, code) = run_ni(root, &["index", "--incremental"]);
    assert_eq!(
        code, 0,
        "index --incremental failed stdout={stdout} stderr={stderr}"
    );

    let (stdout, stderr, code) = run_ni(root, &["data-models", "list", "--json"]);
    assert_eq!(
        code, 0,
        "list --json failed stdout={stdout} stderr={stderr}"
    );
    let list = parse_object(&stdout, "list --json");
    assert!(
        list["resultCount"].as_u64().unwrap_or(0) >= 1,
        "expected extracted LedgerEntry: {list}"
    );
    let names: Vec<&str> = list["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["name"].as_str())
        .collect();
    assert!(
        names.contains(&"LedgerEntry"),
        "list must extract LedgerEntry, got {names:?} stdout={stdout}"
    );
    let entry = list["models"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["name"] == "LedgerEntry")
        .expect("LedgerEntry row");
    assert_eq!(entry["kind"], "SCHEMA");
    assert_eq!(entry["fieldImpact"], "unsupported");
    assert_eq!(entry["file_path"], "src/ledger.rs");
    assert!(
        list["notWired"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|v| v == "sqlMigrations"),
        "notWired: {}",
        list["notWired"]
    );

    let (stdout, stderr, code) = run_ni(root, &["data-models", "impact", "--changed", "--json"]);
    assert_eq!(
        code, 0,
        "pre-dirty --changed failed stdout={stdout} stderr={stderr}"
    );
    let clean = parse_object(&stdout, "pre-dirty --changed");
    assert_eq!(clean["emptyReason"], "cleanDiff", "{clean}");
    assert_eq!(clean["resultCount"], 0, "{clean}");

    fs::write(
        root.join("src").join("ledger.rs"),
        r#"pub struct LedgerEntry {
    pub id: i64,
    pub name: String,
}

fn map_ledger_entry(row: &rusqlite::Row) -> rusqlite::Result<LedgerEntry> {
    Ok(LedgerEntry {
        id: row.get(0)?,
        name: row.get(1)?,
    })
}
"#,
    )
    .unwrap();

    let (stdout, stderr, code) = run_ni(root, &["data-models", "impact", "--changed", "--json"]);
    assert_eq!(
        code, 0,
        "dirty --changed failed stdout={stdout} stderr={stderr}"
    );
    let dirty = parse_object(&stdout, "dirty --changed");
    assert!(
        dirty["resultCount"].as_u64().unwrap_or(0) >= 1,
        "dirty --changed must include LedgerEntry: {dirty}"
    );
    let row = dirty["impacted"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["name"] == "LedgerEntry")
        .expect("LedgerEntry impacted row");
    assert_eq!(row["is_changed"], true);
    assert_eq!(row["file_path"], "src/ledger.rs");
    assert_eq!(row["fieldImpact"], "unsupported");

    assert!(
        !root
            .join(".ledgerful")
            .join("reports")
            .join("latest-impact.json")
            .exists(),
        "must not write reports/latest-impact.json"
    );
    assert!(
        !root.join(".ledgerful").join("latest-impact.json").exists(),
        "must not write .ledgerful/latest-impact.json"
    );
}

#[test]
fn data_models_include_fixtures_json_omits_include_next() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "x\n").unwrap();
    git_add_and_commit(root, "initial");
    let (stdout, stderr, code) = run_ni(root, &["init"]);
    assert_eq!(code, 0, "init failed stdout={stdout} stderr={stderr}");

    let (stdout, stderr, code) = run_ni(
        root,
        &["data-models", "list", "--include-fixtures", "--json"],
    );
    assert_eq!(
        code, 0,
        "include-fixtures list failed stdout={stdout} stderr={stderr}"
    );
    let v = parse_object(&stdout, "include-fixtures list");
    assert_eq!(v["includeFixtures"], true);
    assert!(
        v.get("next").is_none()
            || v["next"]
                .as_array()
                .map(|a| !a.iter().any(|s| {
                    s.as_str()
                        .is_some_and(|t| t.contains("data-models list --include-fixtures"))
                }))
                .unwrap_or(true),
        "must not suggest include-fixtures next when the flag is on: {v}"
    );
}
