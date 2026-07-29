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

/// DoD-4: human search over a fixture with `&&` and quotes has no HTML entities.
///
/// Do NOT assert the absence of escape sequences here. Colour is ungated by design
/// (spec §2.4); a subprocess pipe is not a TTY, so a correct implementation emits
/// `\u{1b}` on this path. Escapes are covered by the source grep and the --json
/// payload assertion below.
#[test]
fn test_search_human_no_html_entities() {
    use crate::common::{git_add_and_commit, run_cli};

    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    fs::write(
        root.join("snippet_src.rs"),
        r#"
pub fn entity_probe_func() {
    if a && b {
        let _q = "quoted string";
    }
}
"#,
    )
    .unwrap();
    git_add_and_commit(root, "snippet_src.rs");

    // search --index builds the Tantivy index without a full `ledgerful index`.
    // Identifier query → hybrid path (the one that used to pollute via highlighted HTML).
    let (stdout, stderr, code) = run_cli(root, &["search", "entity_probe_func", "--index"]);
    assert_eq!(
        code, 0,
        "search must succeed; stderr={stderr}; stdout={stdout}"
    );

    for entity in ["&quot;", "&amp;", "&lt;", "&gt;", "&#39;"] {
        assert!(
            !stdout.contains(entity),
            "DoD-4: human search must not contain HTML entity {entity}; stdout={stdout}"
        );
    }
    // Positive: highlighting survived. Strip ANSI first — ungated owo_colors
    // inserts escapes around match ranges, so a raw contains() on the plain
    // identifier can fail against a correct emphasized rendering (spec §2.4).
    let stripped: String = {
        let mut out = String::new();
        let mut chars = stdout.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                // Skip CSI sequence: ESC [ ... final-byte
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for sc in chars.by_ref() {
                        if ('@'..='~').contains(&sc) {
                            break;
                        }
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    };
    assert!(
        stripped.contains("entity_probe_func"),
        "matched term must still appear after stripping colour: {stripped}"
    );
    assert!(
        stdout.contains("Hybrid") || stdout.contains("snippet_src"),
        "expected hybrid/search result header: {stdout}"
    );
}

/// DoD-5: `search <identifier> --json` — every line parses as JSON and no content
/// value contains escapes or HTML entities.
///
/// Query MUST be identifier-shaped. A spaced query silently takes the already-clean
/// BM25 path and would pass against unfixed code.
#[test]
fn test_search_json_content_plain() {
    use crate::common::{git_add_and_commit, run_cli};

    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    fs::write(
        root.join("snippet_src.rs"),
        r#"
pub fn json_probe_func() {
    if a && b {
        let _q = "quoted string";
    }
}
"#,
    )
    .unwrap();
    git_add_and_commit(root, "snippet_src.rs");

    let (stdout, stderr, code) = run_cli(root, &["search", "json_probe_func", "--index", "--json"]);
    assert_eq!(
        code, 0,
        "search --json must succeed; stderr={stderr}; stdout={stdout}"
    );
    assert!(!stdout.trim().is_empty(), "expected at least one JSON line");

    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|e| {
            panic!("DoD-5: every stdout line must parse as JSON: {e}; line={line}");
        });
        if let Some(content) = v.pointer("/payload/content").and_then(|c| c.as_str()) {
            assert!(
                !content.contains('\u{1b}'),
                "DoD-5: content must not contain escape sequences: {content}"
            );
            for entity in ["&quot;", "&amp;", "&lt;", "&gt;", "&#39;"] {
                assert!(
                    !content.contains(entity),
                    "DoD-5: content must not contain HTML entity {entity}: {content}"
                );
            }
        }
    }
}

/// DoD-6: non-ASCII (CJK + 4-byte emoji) produces a snippet without panic
/// and without a broken code point. Exercises track-owned truncation arithmetic.
#[test]
fn test_search_non_ascii_snippet_safe() {
    use crate::common::{git_add_and_commit, run_cli};

    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    // Long prefix so truncation can land near multi-byte chars.
    let prefix = "x".repeat(200);
    fs::write(
        root.join("unicode_src.rs"),
        format!(
            "// {prefix}\npub fn unicode_snippet_target() {{\n    let s = \"中文emoji😀boundary\";\n}}\n"
        ),
    )
    .unwrap();
    git_add_and_commit(root, "unicode_src.rs");

    let (stdout, stderr, code) = run_cli(root, &["search", "unicode_snippet_target", "--index"]);
    assert_eq!(
        code, 0,
        "non-ASCII search must not panic; stderr={stderr}; stdout={stdout}"
    );
    assert!(
        !stdout.contains('\u{FFFD}'),
        "snippet must not contain replacement char: {stdout}"
    );
    assert!(
        stdout.contains("unicode_snippet_target")
            || stdout.contains("中文")
            || stdout.contains("emoji")
            || stdout.contains("boundary"),
        "expected a usable snippet: {stdout}"
    );
}
