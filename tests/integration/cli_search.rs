use std::fs;
use std::process::Command;
use tempfile::tempdir;

use crate::common::{DirGuard, setup_git_repo};

#[test]
fn test_search_fuzzy_fallback_and_hint() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    use crate::common::git_add_and_commit;
    fs::write(root.join("test_file.rs"), "pub fn execute_scan_impact() {}").unwrap();
    git_add_and_commit(root, "test_file.rs");

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    // 1. Fuzzy match success
    let output = Command::new(ledgerful_bin)
        .args(["search", "excute", "--index"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Falling back to fuzzy search"),
        "Expected fallback to fuzzy search: {}",
        stdout
    );
    assert!(stdout.contains("Fuzzy Search Results:"));
    assert!(stdout.contains("test_file.rs"));

    // 1.5 JSON Output test
    let output_json = Command::new(ledgerful_bin)
        .args(["search", "excute", "--index", "--json"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout_json = String::from_utf8_lossy(&output_json.stdout);
    assert!(
        stdout_json.contains(r#"record_kind":"fuzzy_match"#),
        "Expected JSON fallback record: {}",
        stdout_json
    );

    // 2. Semantic Handoff Hint
    let output2 = Command::new(ledgerful_bin)
        .args(["search", "nonexistent_symbol_12345"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout2 = String::from_utf8_lossy(&output2.stdout);
    assert!(stdout2.contains("Alternatively, try semantic search instead:"));
}

#[test]
fn test_search_ranking_identifier_vs_prose() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    use crate::common::git_add_and_commit;

    // 1. Function definition (very long file to dilute term frequency)
    let filler = "other code here ".repeat(200);
    fs::write(
        root.join("def.rs"),
        format!("pub fn my_target_func() {{}}\n{}", filler),
    )
    .unwrap();

    // 2. Caller
    fs::write(
        root.join("caller.rs"),
        format!("fn main() {{ my_target_func(); }}\n{}", filler),
    )
    .unwrap();

    // 3. Re-export
    fs::write(
        root.join("export.rs"),
        format!("pub use def::my_target_func;\n{}", filler),
    )
    .unwrap();

    // 4. Markdown mention (lots of occurrences so BM25 ranks it high)
    let doc_content = "my_target_func ".repeat(20);
    fs::write(
        root.join("doc.md"),
        format!("{}\nAnd here is some doc.", doc_content),
    )
    .unwrap();

    git_add_and_commit(root, "def.rs caller.rs export.rs doc.md");

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    // Index first
    Command::new(ledgerful_bin)
        .args(["index"])
        .current_dir(root)
        .output()
        .unwrap();

    // Search for identifier
    let output = Command::new(ledgerful_bin)
        .args(["search", "my_target_func"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|l| {
            l.contains("def.rs")
                || l.contains("caller.rs")
                || l.contains("export.rs")
                || l.contains("doc.md")
        })
        .collect();

    // In Phase 1, doc.md probably ranks first because of term frequency.
    // We want def.rs or caller.rs or export.rs to rank ABOVE doc.md for this identifier query.
    println!("LINES:\n{}", lines.join("\n"));
    let doc_pos = lines
        .iter()
        .position(|l| l.contains("doc.md"))
        .unwrap_or(999);
    let def_pos = lines
        .iter()
        .position(|l| l.contains("def.rs"))
        .unwrap_or(999);

    assert!(
        def_pos < doc_pos,
        "Expected definition to rank above markdown doc, but got:\n{}",
        lines.join("\n")
    );
}

#[test]
fn test_search_ranking_prose_query() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    use crate::common::git_add_and_commit;

    // 1. Source code with a brief mention
    let filler = "other code here ".repeat(200);
    fs::write(
        root.join("def.rs"),
        format!(
            "pub fn my_target_func() {{ // a great function }}\n{}",
            filler
        ),
    )
    .unwrap();

    // 2. Markdown doc with extensive discussion
    fs::write(root.join("doc.md"), "a great function!").unwrap();

    git_add_and_commit(root, "def.rs doc.md");

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    // Index first
    Command::new(ledgerful_bin)
        .args(["index"])
        .current_dir(root)
        .output()
        .unwrap();

    // Search for prose
    let output = Command::new(ledgerful_bin)
        .args(["search", "a great function"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("def.rs") || l.contains("doc.md"))
        .collect();

    let doc_pos = lines
        .iter()
        .position(|l| l.contains("doc.md"))
        .unwrap_or(999);
    let def_pos = lines
        .iter()
        .position(|l| l.contains("def.rs"))
        .unwrap_or(999);

    assert!(
        doc_pos <= def_pos,
        "Expected doc.md to rank well for prose query"
    );
}
