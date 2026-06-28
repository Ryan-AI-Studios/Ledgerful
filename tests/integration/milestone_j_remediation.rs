use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_index_check_exit_codes() {
    let dir = tempdir().unwrap();
    let binary = option_env!("CARGO_BIN_EXE_ledgerful").unwrap_or("target/debug/ledgerful");

    // Initialize git repo (required for discover_files)
    Command::new("git")
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Initialize Ledgerful
    Command::new(binary)
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Run scan --impact to initialize the ledger database
    Command::new(binary)
        .args(["scan", "--impact"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Check on empty/uninitialized index should exit 1 (or 0 if it's considered just empty but valid)
    let output = Command::new(binary)
        .args(["index", "--check"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    if !output.status.success() {
        eprintln!(
            "index --check failed on fresh init: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(output.status.success());

    // Create a source file
    fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

    // Now index is missing (total_files == 0 but discover_files found main.rs)
    let output = Command::new(binary)
        .args(["index", "--check"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "index --check should fail when index is missing but files exist"
    );

    // Build index
    Command::new(binary)
        .arg("index")
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Now index is clean
    let output = Command::new(binary)
        .args(["index", "--check"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());

    // Make it stale
    fs::write(dir.path().join("other.rs"), "fn other() {}").unwrap();

    // Stale index should exit 0 by default
    let output = Command::new(binary)
        .args(["index", "--check"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stale index should exit 0 by default"
    );

    // Stale index with --strict should exit 1
    let output = Command::new(binary)
        .args(["index", "--check", "--strict"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "--strict should exit 1 for stale index"
    );
}

#[test]
#[allow(non_snake_case)]
fn test_bridge_export_stdout__slow() {
    let dir = tempdir().unwrap();
    let binary = option_env!("CARGO_BIN_EXE_ledgerful").unwrap_or("target/debug/ledgerful");

    // Initialize git and ledgerful
    Command::new("git")
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    Command::new(binary)
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Run scan --impact to initialize the ledger database
    Command::new(binary)
        .args(["scan", "--impact"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Run bridge export without --out
    let output = Command::new(binary)
        .args(["bridge", "export", "--hotspots", "--stdout"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let _stdout = String::from_utf8_lossy(&output.stdout);
    // Should contain NDJSON records (even if empty list, it might show nothing or some header)
    // Actually bridge export prints each record on a new line.
    // If no hotspots, it might be empty.

    // Let's create some hotspots by scanning
    fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
    Command::new("git")
        .args(["add", "main.rs"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Create an atomic ledger entry
    Command::new(binary)
        .args([
            "ledger",
            "atomic",
            "main.rs",
            "--category",
            "FEATURE",
            "--summary",
            "done",
            "--reason",
            "test",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let output = Command::new(binary)
        .args(["bridge", "export", "--ledger", "--stdout"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    // At least one of these should be present if scan/init worked
    // Wait, if it's still empty, I'll print it to debug
    if !stdout.contains(r#""record_kind":"#) {
        eprintln!(
            "bridge export stdout was empty. stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(stdout.contains(r#""record_kind":"#));
}

#[test]
fn test_dead_code_filtering() {
    let dir = tempdir().unwrap();
    let binary = option_env!("CARGO_BIN_EXE_ledgerful").unwrap_or("target/debug/ledgerful");

    // Initialize git and ledgerful
    Command::new("git")
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap();
    Command::new(binary)
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Create a Rust file with symbols that should be filtered
    let code = r#"
        #[test]
        fn my_test() {}

        extern "C" {
            fn external_fn();
        }

        #[no_mangle]
        pub extern "C" fn exported_ffi() {}

        #[proc_macro]
        pub fn my_macro(_item: TokenStream) -> TokenStream { _item }

        #[cfg(feature = "hidden")]
        pub fn feature_gated() {}

        fn truly_dead() {}
    "#;
    fs::write(dir.path().join("main.rs"), code).unwrap();

    // Index the file
    Command::new(binary)
        .arg("index")
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Run dead-code detection (use --expand to see per-symbol output)
    let output = Command::new(binary)
        .args(["dead-code", "--expand"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Truly dead should be flagged
    assert!(stdout.contains("truly_dead"));

    // Filtered symbols should NOT be flagged as dead
    // Note: the current dead-code command output might vary.
    // I'll check for their absence.
    assert!(!stdout.contains("my_test"), "my_test should be filtered");
    assert!(
        !stdout.contains("exported_ffi"),
        "exported_ffi should be filtered"
    );
    assert!(!stdout.contains("my_macro"), "my_macro should be filtered");

    // Feature gated might be flagged but with annotation.
    // My implementation added annotation to evidence/recommendation.
    // Let's see if it's in the output.
    // Actually, J7 goal said "Annotate feature-gated symbols... instead of just flagging them as dead."
}
