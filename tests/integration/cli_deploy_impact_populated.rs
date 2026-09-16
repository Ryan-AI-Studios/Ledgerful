//! 0358: hermetic Dockerfile + compose detect; no impact orchestrator.

use crate::common::{git_cmd, setup_git_repo};
use serde_json::Value;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn ledgerful_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ledgerful")
}

fn write_temp_config(root: &std::path::Path, coverage_enabled: bool, deploy_enabled: bool) {
    let cg_dir = root.join(".ledgerful");
    fs::create_dir_all(&cg_dir).unwrap();
    let content = format!(
        "[coverage]\nenabled = {coverage_enabled}\n\n[coverage.deploy]\nenabled = \
         {deploy_enabled}\n"
    );
    fs::write(cg_dir.join("config.toml"), content).unwrap();
}

fn run_deploy_impact(root: &std::path::Path, json: bool) -> (bool, String, String) {
    let mut cmd = Command::new(ledgerful_bin());
    cmd.arg("deploy").arg("impact");
    if json {
        cmd.arg("--json");
    }
    let output = cmd.current_dir(root).output().expect("binary should run");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn parse_object(stdout: &str, label: &str) -> Value {
    let v: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("{label} stdout must parse as JSON: {e}\n{stdout}"));
    assert!(
        v.is_object(),
        "{label} must be an object envelope, got: {stdout}"
    );
    v
}

fn write_dockerfile_and_compose(root: &std::path::Path) {
    fs::write(
        root.join("Dockerfile"),
        "FROM alpine:3.20\nCOPY src/ ./src/\n",
    )
    .unwrap();
    fs::write(
        root.join("docker-compose.yml"),
        "services:\n  app:\n    image: nginx\n",
    )
    .unwrap();
}

#[test]
fn deploy_impact_dockerfile_and_compose_from_root_and_subdir() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "# temp\n").unwrap();
    git_cmd(root, &["add", "-A"]);
    git_cmd(root, &["commit", "-m", "initial"]);
    write_temp_config(root, true, true);
    write_dockerfile_and_compose(root);

    let (ok, stdout, stderr) = run_deploy_impact(root, true);
    assert!(ok, "root --json failed stderr={stderr}");
    let v = parse_object(&stdout, "root");
    assert!(v.get("emptyReason").is_none(), "{v}");
    assert!(v.get("completeness").is_none(), "{v}");
    let patterns = v["defaultPatterns"]
        .as_array()
        .expect("defaultPatterns")
        .iter()
        .filter_map(|x| x.as_str())
        .collect::<Vec<_>>();
    assert!(patterns.contains(&"**/Dockerfile*"), "{patterns:?}");
    assert!(patterns.contains(&"**/docker-compose*.yml"), "{patterns:?}");
    let classifiers = v["classifiers"]
        .as_array()
        .expect("classifiers")
        .iter()
        .filter_map(|x| x.as_str())
        .collect::<Vec<_>>();
    assert!(classifiers.contains(&"Dockerfile"), "{classifiers:?}");
    assert!(classifiers.contains(&"Helm"), "{classifiers:?}");
    let results = v["results"].as_array().expect("results");
    assert!(results.len() >= 2, "{v}");
    assert!(
        results
            .iter()
            .any(|r| r["type"] == "Dockerfile" && r["path"] == "Dockerfile"),
        "{v}"
    );
    assert!(
        results
            .iter()
            .any(|r| { r["type"] == "DockerCompose" && r["path"] == "docker-compose.yml" }),
        "{v}"
    );

    let subdir = root.join("subdir");
    fs::create_dir_all(&subdir).unwrap();
    let output = Command::new(ledgerful_bin())
        .args(["deploy", "impact", "--json"])
        .current_dir(&subdir)
        .output()
        .expect("subdir run");
    assert!(
        output.status.success(),
        "subdir failed {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sub = parse_object(&String::from_utf8_lossy(&output.stdout), "subdir");
    let sub_results = sub["results"].as_array().expect("subdir results");
    assert!(
        sub_results
            .iter()
            .any(|r| r["path"] == "Dockerfile" && r["type"] == "Dockerfile"),
        "{sub}"
    );
    assert!(
        sub_results
            .iter()
            .any(|r| r["path"] == "docker-compose.yml" && r["type"] == "DockerCompose"),
        "{sub}"
    );

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("app.rs"), "pub fn f() {}\n").unwrap();
    let (ok, stdout, stderr) = run_deploy_impact(root, true);
    assert!(ok, "coupled --json failed stderr={stderr}");
    let coupled = parse_object(&stdout, "coupled");
    let docker = coupled["results"]
        .as_array()
        .expect("results")
        .iter()
        .find(|r| r["type"] == "Dockerfile")
        .expect("dockerfile row");
    let files = docker["coupledFiles"]
        .as_array()
        .expect("coupledFiles")
        .iter()
        .filter_map(|x| x.as_str())
        .collect::<Vec<_>>();
    assert!(
        files.iter().all(|p| !p.contains('\\')),
        "coupledFiles must be slash-normalized: {files:?}"
    );
    assert!(
        files.iter().any(|p| *p == "src/app.rs" || *p == "src/"),
        "{docker}"
    );
    assert!(docker["risk_tier"].as_u64().unwrap_or(0) >= 2, "{docker}");

    let reports = root
        .join(".ledgerful")
        .join("reports")
        .join("latest-impact.json");
    let wrong = root.join(".ledgerful").join("latest-impact.json");
    assert!(
        !reports.exists(),
        "reports latest-impact.json must be absent"
    );
    assert!(
        !wrong.exists(),
        "wrong-path latest-impact.json must be absent"
    );
    let state = root.join(".ledgerful").join("state");
    assert!(
        !state.join("ledger.db").exists(),
        "enabled populated must not create ledger.db"
    );
    assert!(
        !state.join("ledger.cozo").exists(),
        "enabled populated must not create ledger.cozo"
    );
    assert!(
        !state.join("cli-session.json").exists(),
        "enabled populated must not write cli-session.json"
    );
}

#[test]
fn deploy_impact_gated_skips_sqlite_growth() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("README.md"), "# temp\n").unwrap();
    git_cmd(root, &["add", "-A"]);
    git_cmd(root, &["commit", "-m", "initial"]);
    write_temp_config(root, false, true);
    fs::write(root.join("Dockerfile"), "FROM alpine:3.20\n").unwrap();

    let db = root.join(".ledgerful").join("state").join("ledger.db");
    let cozo = root.join(".ledgerful").join("state").join("ledger.cozo");

    let (ok, stdout, stderr) = run_deploy_impact(root, true);
    assert!(ok, "gated --json failed stderr={stderr}");
    assert!(stdout.contains("disabledByConfig"), "{stdout}");
    assert!(
        !stdout.contains("\"message\": \"No deployment impact detected"),
        "{stdout}"
    );
    assert!(!db.exists(), "gated skip must not create ledger.db");
    assert!(!cozo.exists(), "gated skip must not create ledger.cozo");
}

#[test]
fn deploy_impact_enabled_non_git_is_no_matches() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    write_temp_config(root, true, true);
    let (ok, stdout, stderr) = run_deploy_impact(root, true);
    assert!(ok, "enabled non-git should exit 0; stderr={stderr}");
    let v = parse_object(&stdout, "enabled-nongit");
    assert_eq!(v["emptyReason"], "noMatches");
    assert!(v.get("completeness").is_none(), "{v}");

    let (ok, stdout, stderr) = run_deploy_impact(root, false);
    assert!(ok, "enabled non-git human should exit 0; stderr={stderr}");
    assert!(
        stdout.contains("No deployment impact detected for current changes."),
        "{stdout}"
    );
}
