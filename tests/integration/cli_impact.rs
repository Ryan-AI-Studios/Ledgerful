use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};
use ledgerful::commands::impact::execute_impact;
use ledgerful::state::layout::Layout;
use std::fs;

#[test]
fn test_impact_warns_on_rules_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    setup_git_repo(dir);

    // Create a file and commit
    fs::create_dir_all(dir.join("src")).unwrap_or(());
    fs::write(dir.join("src").join("main.rs"), "fn main() {}").unwrap();
    git_add_and_commit(dir, "initial");

    // Modify the file
    fs::write(
        dir.join("src").join("main.rs"),
        "fn main() { /* modified */ }",
    )
    .unwrap();

    // Init ledgerful
    let _guard = DirGuard::new(dir);
    let layout = Layout::new(dir.to_string_lossy().as_ref());
    layout.ensure_state_dir().unwrap();

    // Write invalid rules.toml
    let rules_path = layout.rules_file();
    let rules_std = rules_path.as_std_path();
    fs::write(rules_std, "this is not valid toml [[[[").unwrap();

    // Impact should still succeed but warn about rules
    let result = execute_impact(false, false, false, false, false, None);
    // The impact command should succeed even with bad rules
    // (it warns but doesn't fail)
    assert!(
        result.is_ok(),
        "Impact should succeed even with invalid rules"
    );
}

#[test]
fn test_impact_succeeds_without_rules_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    setup_git_repo(dir);

    fs::create_dir_all(dir.join("src")).unwrap_or(());
    fs::write(dir.join("src").join("main.rs"), "fn main() {}").unwrap();
    git_add_and_commit(dir, "initial");

    fs::write(
        dir.join("src").join("main.rs"),
        "fn main() { /* modified */ }",
    )
    .unwrap();

    let _guard = DirGuard::new(dir);
    let layout = Layout::new(dir.to_string_lossy().as_ref());
    layout.ensure_state_dir().unwrap();

    // No rules file at all — should use defaults
    let result = execute_impact(false, false, false, false, false, None);
    assert!(result.is_ok(), "Impact should succeed with no rules file");
}

#[test]
fn test_impact_creates_report_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    setup_git_repo(dir);

    fs::create_dir_all(dir.join("src")).unwrap_or(());
    fs::write(dir.join("src").join("main.rs"), "fn main() {}").unwrap();
    git_add_and_commit(dir, "initial");

    fs::write(
        dir.join("src").join("main.rs"),
        "fn main() { /* modified */ }",
    )
    .unwrap();

    let _guard = DirGuard::new(dir);
    let layout = Layout::new(dir.to_string_lossy().as_ref());
    layout.ensure_state_dir().unwrap();

    let result = execute_impact(false, false, false, false, false, None);
    assert!(result.is_ok());

    let report_path = layout.reports_dir().join("latest-impact.json");
    assert!(report_path.exists(), "Impact report should be written");
}

#[test]
fn test_impact_records_unsupported_analysis_in_report() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    setup_git_repo(dir);

    fs::write(dir.join("notes.txt"), "hello").unwrap();
    git_add_and_commit(dir, "initial");
    fs::write(dir.join("notes.txt"), "updated").unwrap();

    let _guard = DirGuard::new(dir);
    let layout = Layout::new(dir.to_string_lossy().as_ref());
    layout.ensure_state_dir().unwrap();

    execute_impact(false, false, false, false, false, None).unwrap();

    let report = fs::read_to_string(layout.reports_dir().join("latest-impact.json")).unwrap();
    assert!(report.contains("\"analysisStatus\""));
    assert!(report.contains("\"unsupported\""));
    assert!(report.contains("\"analysisWarnings\""));
}
