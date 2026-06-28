use crate::common::DirGuard;
use ledgerful::commands::doctor::execute_doctor;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn doctor_reports_system_health() {
    let tmp = tempdir().unwrap();

    // Initialize a mock git repository
    Command::new("git")
        .arg("init")
        .current_dir(tmp.path())
        .output()
        .expect("Failed to run git init");

    let _guard = DirGuard::new(tmp.path());

    // execute_doctor prints to stdout, so we just check if it returns Ok
    let result = execute_doctor();

    assert!(result.is_ok());
}
