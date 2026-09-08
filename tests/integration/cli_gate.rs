use crate::common::{run_cli, setup_git_repo};
use std::fs;
use tempfile::tempdir;

#[test]
fn cli_gate_mode_show_observe_prints_hint() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let (stdout, stderr, code) = run_cli(root, &["init"]);
    assert_eq!(code, 0, "init failed; stdout={stdout} stderr={stderr}");

    let (stdout, stderr, code) = run_cli(root, &["gate", "mode"]);
    assert_eq!(code, 0, "gate mode; stderr={stderr} stdout={stdout}");
    let lines: Vec<&str> = stdout.lines().filter(|line| !line.is_empty()).collect();
    assert!(
        lines.len() >= 2,
        "gate mode show must print enum + hint; stdout={stdout:?}"
    );
    assert_eq!(lines[0], "Gate mode: observe");
    assert_eq!(
        lines[1],
        "observe warns and does not block; set with `ledgerful gate mode enforce`."
    );
}

#[test]
fn cli_gate_mode_show_enforce_prints_hint() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let (stdout, stderr, code) = run_cli(root, &["init"]);
    assert_eq!(code, 0, "init failed; stdout={stdout} stderr={stderr}");

    let config_path = root.join(".ledgerful").join("config.toml");
    let config = fs::read_to_string(&config_path).expect("init writes config.toml");
    let config = config.replace("mode = \"observe\"", "mode = \"enforce\"");
    fs::write(&config_path, config).expect("rewrite gate.mode in tempdir");

    let (stdout, stderr, code) = run_cli(root, &["gate", "mode"]);
    assert_eq!(code, 0, "gate mode; stderr={stderr} stdout={stdout}");
    let lines: Vec<&str> = stdout.lines().filter(|line| !line.is_empty()).collect();
    assert!(
        lines.len() >= 2,
        "gate mode show must print enum + hint; stdout={stdout:?}"
    );
    assert_eq!(lines[0], "Gate mode: enforce");
    assert_eq!(
        lines[1],
        "enforce blocks; set with `ledgerful gate mode observe`."
    );
}
