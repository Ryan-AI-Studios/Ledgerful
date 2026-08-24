use crate::common::{TempEnv, run_cli, setup_git_repo};
use std::fs;
use tempfile::tempdir;

/// T0: consumer worktree → skipped, exit 2, no engine fingerprint.
#[test]
fn release_pins_non_engine_skipped_exit_2() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();

    let (stdout, stderr, code) = run_cli(root, &["release", "pins", "--json"]);
    assert_eq!(
        code, 2,
        "consumer skip must exit 2; stderr={stderr} stdout={stdout}"
    );
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be pure JSON");
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["kind"], "releasePins");
    assert_eq!(v["status"], "skipped");
    assert!(v.get("latest").is_none());
    assert_eq!(v["surfaces"].as_array().map(Vec::len), Some(0));
    assert!(v.get("advisory").is_none());
}

/// T1 / DoD-4: engine + LEDGERFUL_NO_NETWORK → unverified, exit 2 (no sockets).
#[test]
#[serial_test::serial(env)]
fn release_pins_no_network_unverified_exit_2() {
    let _g = TempEnv::set("LEDGERFUL_NO_NETWORK", "1");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args(["release", "pins", "--json"])
        .current_dir(root)
        .env("LEDGERFUL_NO_NETWORK", "1")
        .output()
        .expect("failed to run ledgerful");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = output.status.code().unwrap_or(-1);
    assert_eq!(
        code, 2,
        "NO_NETWORK must exit 2; stderr={stderr} stdout={stdout}"
    );
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be pure JSON");
    assert_eq!(v["kind"], "releasePins");
    assert_eq!(v["status"], "unverified");
    assert!(v.get("latest").is_none());
    let surfaces = v["surfaces"].as_array().expect("surfaces");
    assert_eq!(surfaces.len(), 6);
    for s in surfaces {
        assert_eq!(s["status"], "unverified");
    }
}
