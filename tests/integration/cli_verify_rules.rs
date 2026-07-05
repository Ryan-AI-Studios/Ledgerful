use camino::Utf8Path;
use ledgerful::commands::verify::execute_verify;
use ledgerful::state::layout::Layout;
use ledgerful::verify::plan::VerifyScope;
use std::fs;
use tempfile::tempdir;

use crate::common::DirGuard;

#[test]
fn test_verify_invalid_rules_fail_visibly() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    let _guard = DirGuard::from_utf8(root);

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    fs::write(
        layout.rules_file(),
        "[global]\nmode = \"analyze\"\n\n[[overrides]]\npattern = \"[\"\n",
    )
    .unwrap();

    let err = execute_verify(
        None,
        None,
        5,
        false,
        false,
        None,
        false,
        false,
        VerifyScope::Full,
    )
    .unwrap_err();
    assert!(format!("{err:?}").contains("Invalid glob pattern"));
}
