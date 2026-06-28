use std::fs;
use std::process::Command;

use crate::common::setup_git_repo;

#[test]
fn test_tantivy_index_persistence() {
    let binary_path = env!("CARGO_BIN_EXE_ledgerful");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        src_dir.join("main.rs"),
        "fn main() { println!(\"hello\"); }\n",
    )
    .unwrap();

    // 1. Run search with --index to trigger StreamIndexer
    let output = Command::new(binary_path)
        .args(["search", "main", "--index"])
        .current_dir(root)
        .output()
        .expect("Failed to execute ledgerful search");

    if !output.status.success() {
        panic!(
            "search --index failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // 2. Verify .ledgerful/search_index/ exists and contains .store files
    let index_dir = root.join(".ledgerful").join("search_index");
    assert!(index_dir.exists(), "search_index directory should exist");

    let mut store_files = 0;
    for entry in fs::read_dir(&index_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("store") {
            store_files += 1;
        }
    }

    assert!(
        store_files > 0,
        "Should have at least one .store file in the index"
    );

    // 3. Verify meta.json exists
    let meta_json = index_dir.join("meta.json");
    assert!(meta_json.exists(), "meta.json should exist");
}
