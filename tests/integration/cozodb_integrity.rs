use std::fs;
use std::process::Command;

use httpmock::prelude::*;
use serial_test::serial;

use crate::common::setup_git_repo;

#[test]
#[serial(test)]
fn test_cozodb_hard_migration_integrity() {
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

    let init_output = Command::new(binary_path)
        .args(["init"])
        .current_dir(root)
        .output()
        .expect("Failed to execute ledgerful init");

    if !init_output.status.success() {
        panic!(
            "init failed: {}",
            String::from_utf8_lossy(&init_output.stderr)
        );
    }

    let server = MockServer::start();
    let _embedding_mock = server.mock(|when, then| {
        when.method(POST).path("/v1/embeddings");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "data": [
                    { "embedding": [1.0, 0.0, 0.0] }
                ]
            }));
    });

    let config_path = root.join(".ledgerful").join("config.toml");
    let config = format!(
        r#"[local_model]
base_url = "{}"
embedding_model = "test-model"
dimensions = 3
timeout_secs = 5
"#,
        server.base_url()
    );
    fs::write(config_path, config).unwrap();

    // Loop 10 times: index -> hard migrate -> semantic index
    // This exercises the `robust_remove_dir` and checks for "Invalid neighbor degree" errors.
    for i in 0..10 {
        println!("Migration Loop Iteration: {}", i + 1);

        // 1. Semantic index (populates Cozo HNSW)
        let output = Command::new(binary_path)
            .args(["index", "--semantic"])
            .current_dir(root)
            .output()
            .expect("Failed to execute ledgerful index --semantic");

        if !output.status.success() {
            panic!(
                "index --semantic failed on iteration {}: {}",
                i + 1,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // 2. Search to ensure HNSW is readable
        let output = Command::new(binary_path)
            .args(["search", "--semantic", "main"])
            .current_dir(root)
            .output()
            .expect("Failed to execute ledgerful search --semantic");

        if !output.status.success() {
            panic!(
                "search --semantic failed on iteration {}: {}",
                i + 1,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // 3. Hard migrate (should safely shutdown and robustly remove directories)
        let output = Command::new(binary_path)
            .args(["update", "--migrate", "--force"])
            .current_dir(root)
            .output()
            .expect("Failed to execute ledgerful update --migrate --force");

        if !output.status.success() {
            panic!(
                "update --migrate --force failed on iteration {}: {}",
                i + 1,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
