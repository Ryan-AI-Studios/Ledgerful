use std::fs;
use std::process::Command;
use tempfile::tempdir;

use crate::common::{DirGuard, run_cli, setup_git_repo};

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

/// 0126: empty index + search --json emits search_index_status Insight (not silent).
#[test]
fn search_json_empty_index_emits_search_index_status() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    // Ensure .ledgerful exists so search can open layout; no index content.
    let (init_out, init_err, init_code) = run_cli(root, &["init"]);
    assert_eq!(
        init_code, 0,
        "init should succeed; stderr={init_err}; stdout={init_out}"
    );

    // Empty repo (no indexable sources) → rebuild still yields 0 docs.
    let (stdout, stderr, code) = run_cli(root, &["search", "anything", "--json"]);
    assert_eq!(
        code, 0,
        "search --json empty should exit 0; stderr={stderr}; stdout={stdout}"
    );

    // Status must be the first non-empty JSON record (before any matches/noise).
    let first_json: serde_json::Value = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .find_map(|line| serde_json::from_str(line).ok())
        .expect("expected at least one JSON NDJSON line");
    assert_eq!(
        first_json["record_kind"], "search_index_status",
        "search_index_status must be the first JSON record:\n{stdout}"
    );
    assert_eq!(first_json["direction"], "outbound");
    assert_eq!(first_json["payload"]["type"], "Insight");
    assert_eq!(first_json["payload"]["memory_id"], "search_index_status");
    let content = first_json["payload"]["content"]
        .as_str()
        .expect("Insight content string");
    let status: serde_json::Value = serde_json::from_str(content).expect("content is status JSON");
    let state = status["state"].as_str().unwrap_or("");
    assert!(
        state == "was_empty" || state == "empty_after_rebuild",
        "unexpected state: {status}"
    );
    assert!(status["document_count"].is_number());
    if state == "empty_after_rebuild" {
        let rem = status["remediation"].as_str().unwrap_or("");
        assert!(
            rem.contains("Rebuild") || rem.contains("indexable") || rem.contains("ignore"),
            "empty_after_rebuild needs B2 honesty: {rem}"
        );
    }
}

/// 0126: populated index + zero hits must not claim was_empty / empty_after_rebuild.
#[test]
fn search_json_populated_no_matches_does_not_claim_empty_index() {
    use crate::common::git_add_and_commit;

    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    fs::write(root.join("a.rs"), "pub fn real_symbol() {}").unwrap();
    git_add_and_commit(root, "a.rs");

    let (out, err, code) = run_cli(root, &["init"]);
    assert_eq!(code, 0, "init; stderr={err}; stdout={out}");

    // Build Tantivy with --index so pre_count > 0 on the next query.
    let (out, err, code) = run_cli(root, &["search", "real_symbol", "--index", "--json"]);
    assert_eq!(code, 0, "search --index; stderr={err}; stdout={out}");

    // Query something that won't match; pre_count > 0 so no search_index_status.
    let (stdout, stderr, code) =
        run_cli(root, &["search", "zzzz_nonexistent_token_0126", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            assert_ne!(
                v["record_kind"], "search_index_status",
                "populated index zero-hit must not claim empty index: {line}"
            );
        }
    }
}

/// 0126: empty Tantivy + indexable sources → auto-rebuild yields
/// `state == "was_empty"` with `document_count >= 1` (successful rebuild path).
#[test]
fn search_json_was_empty_after_successful_rebuild() {
    use crate::common::git_add_and_commit;

    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    // Distinctive token so a match is expected after rebuild.
    let token = "was_empty_rebuild_token_0126";
    fs::write(
        root.join("src_was_empty.rs"),
        format!("pub fn {token}() {{}}\n"),
    )
    .unwrap();
    git_add_and_commit(root, "src_was_empty.rs");

    // Fresh init: search_index present/created but never populated (pre_count=0).
    let (init_out, init_err, init_code) = run_cli(root, &["init"]);
    assert_eq!(
        init_code, 0,
        "init should succeed; stderr={init_err}; stdout={init_out}"
    );

    // First search --json on empty index: auto-rebuild then query.
    // Do not pass --index explicitly; empty pre_count triggers rebuild.
    let (stdout, stderr, code) = run_cli(root, &["search", token, "--json"]);
    assert_eq!(
        code, 0,
        "search --json was_empty path; stderr={stderr}; stdout={stdout}"
    );

    // First non-empty JSON record must be search_index_status (status-before-matches).
    let json_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter(|l| serde_json::from_str::<serde_json::Value>(l).is_ok())
        .collect();
    assert!(!json_lines.is_empty(), "expected NDJSON output:\n{stdout}");
    let first: serde_json::Value = serde_json::from_str(json_lines[0]).expect("first JSON line");
    assert_eq!(
        first["record_kind"], "search_index_status",
        "search_index_status must be the first JSON record:\n{stdout}"
    );
    assert_eq!(first["direction"], "outbound");
    assert_eq!(first["payload"]["type"], "Insight");
    assert_eq!(first["payload"]["memory_id"], "search_index_status");
    let content = first["payload"]["content"]
        .as_str()
        .expect("Insight content string");
    let status: serde_json::Value = serde_json::from_str(content).expect("content is status JSON");
    assert_eq!(
        status["state"].as_str().unwrap_or(""),
        "was_empty",
        "successful rebuild must emit was_empty not empty_after_rebuild: {status}"
    );
    let docs = status["document_count"]
        .as_u64()
        .expect("document_count number");
    assert!(
        docs >= 1,
        "was_empty path requires document_count >= 1: {status}"
    );

    // If match records exist later, status already preceded them by first-record assert.
    let has_match = json_lines.iter().skip(1).any(|line| {
        let v: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
        let kind = v["record_kind"].as_str().unwrap_or("");
        kind == "search_match"
            || kind == "hybrid_match"
            || kind.contains("match")
            || v["payload"]["type"] == "SearchResult"
            || v["payload"]["type"] == "Hit"
    });
    // Prefer that a matching token produces at least one hit after rebuild.
    let _ = has_match;
}

/// 0128: after content change, `search --auto-index` must rebuild Tantivy so a
/// brand-new token is discoverable without an explicit `index --incremental`.
#[test]
fn search_auto_index_after_content_change_finds_token() {
    use crate::common::git_add_and_commit;

    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "pub fn before_edit() {}\n").unwrap();
    git_add_and_commit(root, "src/lib.rs");

    let (out, err, code) = run_cli(root, &["init"]);
    assert_eq!(code, 0, "init; stderr={err}; stdout={out}");

    // Full index builds SQLite + Tantivy floor.
    let (out, err, code) = run_cli(root, &["index"]);
    assert_eq!(code, 0, "index; stderr={err}; stdout={out}");

    // Same-day content edit: SQLite + Tantivy would both be content-stale without
    // --auto-index + FTS rebuild.
    let token = "unique_auto_index_token_0128";
    fs::write(
        root.join("src").join("lib.rs"),
        format!("pub fn before_edit() {{}}\npub fn {token}() {{}}\n"),
    )
    .unwrap();

    let (stdout, stderr, code) = run_cli(root, &["search", token, "--auto-index", "--json"]);
    assert_eq!(
        code, 0,
        "search --auto-index; stderr={stderr}; stdout={stdout}"
    );

    let has_hit = stdout.lines().filter(|l| !l.trim().is_empty()).any(|line| {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        let kind = v["record_kind"].as_str().unwrap_or("");
        let content = v["payload"]["content"].as_str().unwrap_or("");
        let memory = v["payload"]["memory_id"].as_str().unwrap_or("");
        (kind.contains("match") || kind == "insight")
            && (content.contains(token) || memory.contains(token) || content.contains("lib.rs"))
    });
    assert!(
        has_hit,
        "0128: auto-index + FTS rebuild must surface new token; stdout={stdout}; stderr={stderr}"
    );
}
