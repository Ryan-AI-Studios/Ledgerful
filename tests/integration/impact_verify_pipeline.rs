use ledgerful::commands::impact::execute_impact;
use ledgerful::commands::verify::execute_verify;
use ledgerful::impact::packet::ImpactPacket;
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use ledgerful::verify::plan::VerifyScope;
use std::fs;

use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};

#[test]
fn test_impact_packet_is_loadable_by_verify_after_scan() {
    // Regression: NaN hotspot scores serialized as JSON null caused
    // storage.get_latest_packet() to fail with a type error, which silently
    // disabled all predictive verification suggestions.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    setup_git_repo(dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 { a + b }",
    )
    .unwrap();
    git_add_and_commit(dir, "initial");

    // Dirty the working tree so impact has something to analyse
    fs::write(
        dir.join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 { a + b + 0 }",
    )
    .unwrap();

    let _guard = DirGuard::new(dir);
    let layout = Layout::new(dir.to_string_lossy().as_ref());
    layout.ensure_state_dir().unwrap();

    // Step 1: impact writes the packet to SQLite
    execute_impact(false, false, false, false, false, None).expect("execute_impact should succeed");

    // Step 2: verify should be able to load and deserialize that packet
    // (using prediction mode, no manual command override)
    let result = execute_verify(
        Some("echo ok".into()),
        None,
        10,
        false,
        false,
        None,
        false,
        false,
        VerifyScope::Full,
    );
    assert!(
        result.is_ok(),
        "verify should succeed after impact: {:?}",
        result.err()
    );
}

#[test]
fn test_impact_packet_stored_in_sqlite_has_finite_hotspot_scores() {
    // Directly inspects the SQLite round-trip: hotspot scores in the stored
    // packet must be finite (0.0 for zero-complexity files, not NaN/null).
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    setup_git_repo(dir);
    // Use a markdown file — zero complexity, the exact case that triggered NaN
    fs::write(dir.join("README.md"), "# Hello").unwrap();
    git_add_and_commit(dir, "initial");
    fs::write(dir.join("README.md"), "# Hello World").unwrap();

    let _guard = DirGuard::new(dir);
    let layout = Layout::new(dir.to_string_lossy().as_ref());
    layout.ensure_state_dir().unwrap();

    execute_impact(false, false, false, false, false, None).expect("execute_impact should succeed");

    // Read back from SQLite and check all hotspot scores
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(db_path.as_std_path()).unwrap();
    let packet: ImpactPacket = storage
        .get_latest_packet()
        .expect("get_latest_packet should not fail")
        .expect("packet should exist after impact");

    for hotspot in &packet.hotspots {
        assert!(
            hotspot.score.is_finite(),
            "hotspot '{}' has non-finite score: {}",
            hotspot.path.display(),
            hotspot.score
        );
    }
}
