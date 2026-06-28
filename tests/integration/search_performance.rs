use std::fs;
use std::process::Command;
use std::time::Instant;

use crate::common::setup_git_repo;

#[test]
fn test_search_performance_gate() {
    let binary_path = env!("CARGO_BIN_EXE_ledgerful");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        src_dir.join("lib.rs"),
        "pub struct TantivySearchEngine;\npub fn search_gate() {}\n",
    )
    .unwrap();

    // 1. Initial indexing
    let start = Instant::now();
    let status = Command::new(binary_path)
        .args(["search", "struct", "--index", "--limit", "1"])
        .current_dir(root)
        .status()
        .expect("Failed to execute ledgerful search");
    assert!(status.success());
    let indexing_duration = start.elapsed();
    println!("Indexing took: {:?}", indexing_duration);

    // 2. Ranked search performance
    let start = Instant::now();
    let output = Command::new(binary_path)
        .args(["search", "TantivySearchEngine", "--limit", "10"])
        .current_dir(root)
        .output()
        .expect("Failed to execute ledgerful search");
    assert!(output.status.success());
    let ranked_duration = start.elapsed();
    println!("Ranked search took: {:?}", ranked_duration);
    assert!(
        ranked_duration.as_millis() < 500,
        "Ranked search too slow: {:?}",
        ranked_duration
    );

    // 3. Regex search performance
    let start = Instant::now();
    let output = Command::new(binary_path)
        .args(["search", "pub fn.*\\{", "--regex", "--limit", "10"])
        .current_dir(root)
        .output()
        .expect("Failed to execute ledgerful search");
    assert!(output.status.success());
    let regex_duration = start.elapsed();
    println!("Regex search took: {:?}", regex_duration);
    assert!(
        regex_duration.as_millis() < 500,
        "Regex search too slow: {:?}",
        regex_duration
    );
}
