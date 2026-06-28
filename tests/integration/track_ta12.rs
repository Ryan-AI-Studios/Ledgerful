use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};
use ledgerful::commands::scan::execute_scan;
use ledgerful::state::layout::Layout;
use ledgerful::state::reports::{LatestImpactReport, read_latest_impact_report};
use serial_test::serial;
use std::fs;
use tempfile::tempdir;

#[test]
#[serial(cwd)]
fn test_scan_clean_tree_writes_tombstone() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("file.txt"), "hello").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);

    execute_scan(false, false, false, None, None).unwrap();

    let layout = Layout::new(root.to_str().unwrap());
    let report = read_latest_impact_report(&layout).unwrap();
    assert!(
        report.is_some(),
        "scan on clean tree should write a clean-tree tombstone"
    );

    match report.unwrap() {
        LatestImpactReport::CleanTree(tombstone) => {
            assert_eq!(tombstone.status, "clean_tree");
            assert!(tombstone.tree_clean);
            assert!(!tombstone.timestamp_utc.is_empty());
        }
        LatestImpactReport::Packet(_) => {
            panic!("expected clean-tree tombstone, got full packet")
        }
    }
}

#[test]
#[serial(cwd)]
fn test_scan_dirty_tree_does_not_write_tombstone() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("file.txt"), "hello").unwrap();
    git_add_and_commit(root, "initial");

    let _guard = DirGuard::new(root);
    let layout = Layout::new(root.to_str().unwrap());

    // Pre-populate with a full packet by making the tree dirty and running
    // scan --impact.
    fs::write(root.join("file.txt"), "modified").unwrap();
    execute_scan(true, false, false, None, None).unwrap();
    let original_report = read_latest_impact_report(&layout)
        .unwrap()
        .expect("impact report should exist");
    match &original_report {
        LatestImpactReport::Packet(_) => {}
        LatestImpactReport::CleanTree(_) => {
            panic!("original report should be a full packet for a dirty tree scan")
        }
    }

    // Now dirty the tree again and run scan without --impact. The existing
    // impact report must be left unchanged.
    fs::write(root.join("file.txt"), "modified again").unwrap();
    execute_scan(false, false, false, None, None).unwrap();

    let new_report = read_latest_impact_report(&layout).unwrap().unwrap();
    match new_report {
        LatestImpactReport::Packet(_) => {}
        LatestImpactReport::CleanTree(_) => {
            panic!(
                "dirty-tree scan without --impact should not overwrite with a clean-tree tombstone"
            )
        }
    }
}
