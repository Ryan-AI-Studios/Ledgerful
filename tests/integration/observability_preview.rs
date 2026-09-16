//! 0360 — observability `--preview` + versioned diff envelope.

use crate::common::{git_add_and_commit, setup_git_repo};
use ledgerful::commands::index::{IndexArgs, execute_index};
use ledgerful::commands::init::execute_init;
use serde_json::Value;
use serial_test::serial;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_slo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("observability")
        .join("dogfood_slo.yaml")
}

fn new_git_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "fn main() {}\n").unwrap();
    git_add_and_commit(root, "initial");
    tmp
}

fn copy_openslo_fixture(root: &Path) {
    let src = fixture_slo();
    assert!(src.is_file(), "dogfood SLO fixture must exist at {src:?}");
    let dest_dir = root.join("observability");
    fs::create_dir_all(&dest_dir).unwrap();
    fs::copy(&src, dest_dir.join("dogfood_slo.yaml")).unwrap();
}

fn run_cli(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let output = Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args(args)
        .current_dir(dir)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .expect("failed to run ledgerful");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

fn parse_json(stdout: &str) -> Value {
    serde_json::from_str(stdout).unwrap_or_else(|e| panic!("stdout was not JSON: {e}\n{stdout}"))
}

fn object_keys(v: &Value) -> Vec<String> {
    v.as_object().expect("object").keys().cloned().collect()
}

#[test]
fn coverage_preview_without_init_populates_dogfood() {
    let tmp = new_git_repo();
    let root = tmp.path();
    copy_openslo_fixture(root);
    assert!(
        !root.join(".ledgerful").exists(),
        "preview must not require init"
    );

    let (stdout, stderr, code) =
        run_cli(root, &["observability", "coverage", "--preview", "--json"]);
    assert_eq!(
        code, 0,
        "preview coverage failed stderr={stderr} stdout={stdout}"
    );
    let v = parse_json(&stdout);
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["preview"], true);
    assert_eq!(v["results"][0]["service"], "Service: dogfood-service");
    assert_eq!(v["results"][0]["slo_count"], 1);
    assert_eq!(v["results"][0]["metric_count"], 1);
    assert_eq!(v["results"][0]["health"], "covered");
    assert_eq!(v["notWired"], serde_json::json!(["endpoints"]));
    let kinds = v["inputs"]["kinds"]
        .as_array()
        .expect("kinds")
        .iter()
        .map(|x| x.as_str().unwrap_or(""))
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            "AlertCondition",
            "AlertNotificationTarget",
            "AlertPolicy",
            "DataSource",
            "SLI",
            "SLO",
            "Service"
        ]
    );
    let keys = object_keys(&v);
    assert_eq!(keys.first().map(String::as_str), Some("schemaVersion"));
    assert!(
        keys.windows(2)
            .any(|w| w[0] == "inputs" && w[1] == "notWired"),
        "inputs must precede notWired, got {keys:?}"
    );
    assert!(
        !root.join(".ledgerful").exists(),
        "preview must not write state"
    );
}

#[test]
#[serial(cwd)]
fn persist_without_ingest_is_no_indexed_data_while_preview_populated() {
    let tmp = new_git_repo();
    let root = tmp.path();
    let _guard = crate::common::DirGuard::new(root);
    execute_init(false, false).unwrap();
    copy_openslo_fixture(root);

    let (persist, stderr, code) = run_cli(root, &["observability", "coverage", "--json"]);
    assert_eq!(code, 0, "persist coverage failed stderr={stderr}");
    let persist = parse_json(&persist);
    assert_eq!(persist["emptyReason"], "noIndexedData");
    assert!(
        persist["message"]
            .as_str()
            .unwrap_or("")
            .contains("index --analyze-graph"),
        "persist empty must keep 0215 ingest next, got {}",
        persist["message"]
    );

    let (preview, stderr, code) =
        run_cli(root, &["observability", "coverage", "--preview", "--json"]);
    assert_eq!(code, 0, "preview coverage failed stderr={stderr}");
    let preview = parse_json(&preview);
    assert_eq!(preview["results"][0]["service"], "Service: dogfood-service");
    assert_eq!(preview["preview"], true);
}

#[test]
fn preview_empty_without_yaml_does_not_mention_analyze_graph() {
    let tmp = new_git_repo();
    let root = tmp.path();
    let (stdout, stderr, code) =
        run_cli(root, &["observability", "coverage", "--preview", "--json"]);
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    let v = parse_json(&stdout);
    assert_eq!(v["emptyReason"], "noMatches");
    let message = v["message"].as_str().unwrap_or("");
    assert!(
        !message.contains("index --analyze-graph"),
        "preview empty must not send users to analyze-graph, got {message}"
    );
    assert!(message.contains("observability/"), "got {message}");

    let (human, _, code) = run_cli(root, &["observability", "coverage", "--preview"]);
    assert_eq!(code, 0);
    assert!(human.contains("(preview)"), "got {human}");
    assert!(!human.contains("index --analyze-graph"), "got {human}");
    assert!(
        !human.to_lowercase().contains("generate a base openslo"),
        "preview must not call DX1, got {human}"
    );
    assert!(
        !root.join(".ledgerful").join("cli-session.json").exists(),
        "preview must not write the session cookie"
    );
}

#[test]
fn coverage_preview_service_only_is_health_missing() {
    let tmp = new_git_repo();
    let root = tmp.path();
    fs::create_dir_all(root.join("observability")).unwrap();
    fs::write(
        root.join("observability").join("svc.yaml"),
        "apiVersion: openslo/v1\nkind: Service\nmetadata:\n  name: solo\nspec:\n  description: x\n",
    )
    .unwrap();
    let (stdout, stderr, code) =
        run_cli(root, &["observability", "coverage", "--preview", "--json"]);
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    let v = parse_json(&stdout);
    assert_eq!(v["results"][0]["service"], "Service: solo");
    assert_eq!(v["results"][0]["slo_count"], 0);
    assert_eq!(v["results"][0]["health"], "missing");
}

#[test]
fn diff_preview_dirty_yaml_includes_source_file_on_slo_and_metric() {
    let tmp = new_git_repo();
    let root = tmp.path();
    copy_openslo_fixture(root);
    let (stdout, stderr, code) = run_cli(root, &["observability", "diff", "--preview", "--json"]);
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    let v = parse_json(&stdout);
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["kind"], "observabilityDiff");
    assert_eq!(v["preview"], true);
    assert!(v["indexedCount"].as_u64().unwrap_or(0) >= 2, "got {v}");
    assert_eq!(v["resultCount"], v["changed"].as_array().unwrap().len());
    let changed = v["changed"].as_array().cloned().unwrap_or_default();
    let slo = changed.iter().find(|x| x["category"] == "slo");
    let metric = changed.iter().find(|x| x["category"] == "metric");
    assert!(slo.is_some(), "expected changed SLO, got {v}");
    assert!(metric.is_some(), "expected changed metric, got {v}");
    assert_eq!(slo.unwrap()["sourceFile"], "observability/dogfood_slo.yaml");
    assert_eq!(
        metric.unwrap()["sourceFile"],
        "observability/dogfood_slo.yaml"
    );
}

#[test]
#[serial(cwd)]
fn persist_diff_after_analyze_graph_includes_metric_source_file() {
    let tmp = new_git_repo();
    let root = tmp.path();
    let _guard = crate::common::DirGuard::new(root);
    execute_init(false, false).unwrap();
    copy_openslo_fixture(root);
    git_add_and_commit(root, "openslo");
    execute_index(IndexArgs {
        analyze_graph: true,
        ..Default::default()
    })
    .unwrap();
    fs::write(
        root.join("observability").join("dogfood_slo.yaml"),
        format!("{}\n# dirty\n", fs::read_to_string(fixture_slo()).unwrap()),
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli(root, &["observability", "diff", "--json"]);
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    let v = parse_json(&stdout);
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["kind"], "observabilityDiff");
    assert!(v.get("preview").is_none());
    assert!(v["indexedCount"].as_u64().is_some(), "indexedCount always");
    assert_eq!(v["resultCount"], v["changed"].as_array().unwrap().len());
    let changed = v["changed"].as_array().cloned().unwrap_or_default();
    let slo = changed.iter().find(|x| x["category"] == "slo");
    let metric = changed.iter().find(|x| x["category"] == "metric");
    assert!(
        slo.is_some(),
        "persist dirty YAML must mark SLO changed, got {v}"
    );
    assert!(
        metric.is_some(),
        "persist dirty YAML must mark metric changed, got {v}"
    );
    assert_eq!(slo.unwrap()["sourceFile"], "observability/dogfood_slo.yaml");
    assert_eq!(
        metric.unwrap()["sourceFile"],
        "observability/dogfood_slo.yaml"
    );
}

#[test]
fn garbage_yaml_emits_parse_errors_and_keeps_valid_docs() {
    let tmp = new_git_repo();
    let root = tmp.path();
    copy_openslo_fixture(root);
    fs::write(root.join("observability").join("bad.yaml"), ":::: not yaml").unwrap();
    let (stdout, stderr, code) =
        run_cli(root, &["observability", "coverage", "--preview", "--json"]);
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    let v = parse_json(&stdout);
    assert_eq!(v["results"][0]["service"], "Service: dogfood-service");
    let errors = v["parseErrors"].as_array().expect("parseErrors");
    assert!(!errors.is_empty(), "got {v}");
    assert_eq!(errors[0]["path"], "observability/bad.yaml");

    let (human, herr, code) = run_cli(root, &["observability", "coverage", "--preview"]);
    assert_eq!(code, 0, "human failed {herr} {human}");
    assert!(
        herr.contains("OpenSLO parse errors") || herr.contains("bad.yaml"),
        "expected stderr banner, got {herr}"
    );

    let (dprev, _, code) = run_cli(root, &["observability", "diff", "--preview", "--json"]);
    assert_eq!(code, 0);
    let dprev = parse_json(&dprev);
    assert!(
        dprev["parseErrors"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        "diff preview parseErrors, got {dprev}"
    );
}

#[test]
#[serial(cwd)]
fn persist_coverage_json_includes_parse_errors() {
    let tmp = new_git_repo();
    let root = tmp.path();
    let _guard = crate::common::DirGuard::new(root);
    execute_init(false, false).unwrap();
    copy_openslo_fixture(root);
    fs::write(root.join("observability").join("bad.yaml"), ":::: not yaml").unwrap();
    let (stdout, stderr, code) = run_cli(root, &["observability", "coverage", "--json"]);
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    let v = parse_json(&stdout);
    let errors = v["parseErrors"].as_array().expect("parseErrors");
    assert_eq!(errors[0]["path"], "observability/bad.yaml");
}

#[test]
fn coverage_help_does_not_claim_endpoints() {
    let tmp = new_git_repo();
    let root = tmp.path();
    let (stdout, stderr, code) = run_cli(root, &["observability", "coverage", "--help"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let text = format!("{stdout}{stderr}");
    assert!(
        !text.to_lowercase().contains("endpoints"),
        "about must not claim endpoints, got {text}"
    );
}

#[test]
fn preview_does_not_create_observability_dir_when_missing() {
    let tmp = new_git_repo();
    let root = tmp.path();
    let _ = run_cli(root, &["observability", "coverage", "--preview"]);
    assert!(
        !root.join("observability").exists(),
        "preview must not create observability/"
    );
}
