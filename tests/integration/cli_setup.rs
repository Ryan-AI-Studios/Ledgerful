use crate::common::{DirGuard, setup_git_repo};
use ledgerful::commands::setup::execute_setup;
use std::fs;
use tempfile::tempdir;

#[test]
fn setup_yes_creates_full_layout() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);

    let result = execute_setup(true, false);
    assert!(result.is_ok());

    let cg_dir = root.join(".ledgerful");
    assert!(cg_dir.exists());
    assert!(cg_dir.join("config.toml").exists());
    assert!(cg_dir.join("rules.toml").exists());
    assert!(cg_dir.join("logs").exists());

    // Ledger DB should exist
    assert!(cg_dir.join("state").join("ledger.db").exists());
}

#[test]
#[allow(non_snake_case)]
fn setup_yes_is_idempotent__slow() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);

    // First run
    let result = execute_setup(true, false);
    assert!(result.is_ok());

    // Second run — should not error
    let result = execute_setup(true, false);
    assert!(result.is_ok());

    // Hooks should not be duplicated
    let pre_commit = fs::read_to_string(root.join(".git").join("hooks").join("pre-commit"))
        .expect("pre-commit hook should be installed");
    let pre_push = fs::read_to_string(root.join(".git").join("hooks").join("pre-push"))
        .expect("pre-push hook should be installed");

    assert_eq!(pre_commit.matches("# ledgerful-ledger-gate").count(), 1);
    assert_eq!(pre_push.matches("# ledgerful-ledger-gate").count(), 1);
}

#[test]
fn setup_yes_skip_scan_skips_report() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);

    // First run creates the .ledgerful/ layout (without an impact report,
    // because --skip-scan is on).
    let result = execute_setup(true, true);
    assert!(result.is_ok());

    let cg_dir = root.join(".ledgerful");
    assert!(cg_dir.join("config.toml").exists());

    // Pre-seed a stale impact report. A second run with --skip-scan must NOT
    // touch this file. This is a stronger test than the original (which
    // merely checked that no report was created) because the wizard actually
    // has to choose to skip the scan rather than relying on the absence of
    // a pre-existing report.
    fs::create_dir_all(cg_dir.join("reports")).unwrap();
    let report = cg_dir.join("reports").join("latest-impact.json");
    let sentinel = r#"{"stale":true,"produced_by":"skip-scan test"}"#;
    fs::write(&report, sentinel).unwrap();
    let mtime_before = fs::metadata(&report).unwrap().modified().unwrap();

    let result = execute_setup(true, true);
    assert!(result.is_ok());

    // The pre-existing report must still exist and be byte-identical.
    assert!(
        report.exists(),
        "skip-scan must not delete a pre-existing report"
    );
    let contents = fs::read_to_string(&report).unwrap();
    assert_eq!(
        contents, sentinel,
        "skip-scan must not rewrite the report contents"
    );
    let mtime_after = fs::metadata(&report).unwrap().modified().unwrap();
    assert_eq!(
        mtime_before, mtime_after,
        "skip-scan must not touch the report file"
    );
}

#[test]
#[allow(non_snake_case)]
fn setup_existing_state_yes_prints_using_existing__slow() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);

    // First run — initializes fresh layout
    let result = execute_setup(true, false);
    assert!(result.is_ok());

    let cg_dir = root.join(".ledgerful");
    assert!(cg_dir.exists());
    assert!(cg_dir.join("config.toml").exists());

    // Second run with yes=true on existing state — should not error
    // and should leave the existing layout intact (H3 fix exercised here).
    let result = execute_setup(true, false);
    assert!(
        result.is_ok(),
        "second setup run on existing state should succeed"
    );

    // Layout must still be there and hooks must remain unduplicated.
    assert!(cg_dir.join("config.toml").exists());
    let pre_commit = fs::read_to_string(root.join(".git").join("hooks").join("pre-commit"))
        .expect("pre-commit hook should be installed");
    let pre_push = fs::read_to_string(root.join(".git").join("hooks").join("pre-push"))
        .expect("pre-push hook should be installed");

    assert_eq!(pre_commit.matches("# ledgerful-ledger-gate").count(), 1);
    assert_eq!(pre_push.matches("# ledgerful-ledger-gate").count(), 1);
}

#[test]
fn setup_no_git_repo_succeeds() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();

    // Note: deliberately NOT calling setup_git_repo. The wizard must still
    // create the .ledgerful/ layout because init falls back to cwd when
    // gix::discover fails (see src/commands/init.rs:202-209). The first-scan
    // step is gated on a fresh gix::discover check, so it gracefully
    // skips with a "Skipping first scan" warning rather than erroring.
    let _guard = DirGuard::from_utf8(root);

    let result = execute_setup(true, false);
    assert!(
        result.is_ok(),
        "setup should succeed in a no-git tempdir (first scan must be skipped, not errored)"
    );

    let cg_dir = root.join(".ledgerful");
    assert!(cg_dir.exists(), ".ledgerful/ must be created without git");
    assert!(cg_dir.join("config.toml").exists());
    assert!(cg_dir.join("rules.toml").exists());
    assert!(cg_dir.join("logs").exists());
}

/// H1 regression test: running `ledgerful setup` from a *subdirectory* of
/// an existing git work tree must still place the `.ledgerful/` tree at
/// the git root — not at the cwd the wizard was invoked from. This guards
/// the split-state bug the M6 cross-model review identified: without the
/// `gix::discover(".")`-based root resolution (and the matching `CwdGuard`
/// that redirects `execute_doctor`/`execute_scan` to the same root), the
/// wizard's bookkeeping would silently diverge from `init`/`index` and
/// leave two disconnected `.ledgerful/` trees.
#[test]
fn setup_run_from_subdir_uses_git_root() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();

    // Git root is the tempdir itself; the wizard will be invoked from `sub`.
    setup_git_repo(tmp.path());

    let sub_dir = root.join("sub");
    std::fs::create_dir_all(sub_dir.as_std_path()).unwrap();

    // Run the wizard from inside the subdirectory — not the git root.
    let _guard = DirGuard::from_utf8(&sub_dir);

    let result = execute_setup(true, false);
    assert!(
        result.is_ok(),
        "setup should succeed when run from a subdirectory of a git work tree"
    );

    // MED-1: pin the CwdGuard's drop-time cwd-restoration contract. After
    // `execute_setup` returns, the wizard's internal `CwdGuard` has dropped
    // and must have restored cwd back to the subdirectory the test started
    // from. This assertion runs *before* the outer `DirGuard` (declared
    // above) drops at end-of-test, so a regression that silently leaked
    // the cwd change (e.g. by removing the `Drop` impl) would fail here.
    //
    // Compare canonicalized paths: on macOS the temp dir lives behind a
    // symlink (/var -> /private/var), so `current_dir()` returns the
    // resolved /private/var/... form while `sub_dir` keeps the /var/... form
    // — they'd differ even when CWD was correctly restored. Canonicalizing
    // both sides makes the assertion stable across platforms.
    let current = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    let expected = std::fs::canonicalize(sub_dir.as_std_path()).unwrap();
    assert_eq!(
        current, expected,
        "CwdGuard must restore the original cwd on drop; the wizard's \
         internal guard entered at the git root and should have switched \
         back to the subdir the caller invoked it from"
    );

    // The .ledgerful tree must live at the git root (where init/index
    // resolve to via gix::discover), NOT in the subdirectory the wizard
    // was invoked from. Without the H1 fix, doctor/scan would each
    // create a parallel tree under the cwd.
    let git_root_cg = root.join(".ledgerful");
    let sub_cg = sub_dir.join(".ledgerful");

    assert!(
        git_root_cg.exists(),
        ".ledgerful must be created at the git root ({}); the wizard's \
         bookkeeping diverged from init/index without the H1 fix",
        git_root_cg,
    );
    assert!(
        !sub_cg.exists(),
        ".ledgerful must NOT be created at the cwd ({}) — that means the \
         wizard's bookkeeping diverged from init/index and a second, empty \
         .ledgerful tree was created",
        sub_cg,
    );
    assert!(git_root_cg.join("config.toml").exists());
    assert!(git_root_cg.join("rules.toml").exists());
    assert!(git_root_cg.join("state").join("ledger.db").exists());

    // The first-scan step (when not --skip-scan) must write its impact
    // report to the same git-rooted .ledgerful tree. Without the
    // CwdGuard, execute_scan (which uses raw cwd) would write the report
    // to <sub>/.ledgerful/reports/... and the success screen — which
    // checks the git-rooted path — would not find it.
    let git_root_report = git_root_cg.join("reports").join("latest-impact.json");
    let sub_report = sub_cg.join("reports").join("latest-impact.json");

    assert!(
        git_root_report.exists(),
        "first-scan impact report must be written under the git root ({}); \
         a cwd-based report would mean scan diverged from init/index",
        git_root_report,
    );
    assert!(
        !sub_report.exists(),
        "first-scan must not write the report to a cwd-based .ledgerful tree ({})",
        sub_report,
    );
}
