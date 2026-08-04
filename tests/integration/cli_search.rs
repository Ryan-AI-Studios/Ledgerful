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

    // 1.5 JSON envelope: fuzzy hits appear as results[].kind
    let output_json = Command::new(ledgerful_bin)
        .args(["search", "excute", "--index", "--json"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout_json = String::from_utf8_lossy(&output_json.stdout);
    let env: serde_json::Value = serde_json::from_str(stdout_json.trim()).expect("envelope parse");
    assert_eq!(env["schemaVersion"], 1);
    let kinds: Vec<&str> = env["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter_map(|r| r["kind"].as_str())
        .collect();
    assert!(
        kinds.contains(&"fuzzy_match"),
        "Expected fuzzy_match hit in envelope: {}",
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
/// Colour is gated via `if_supports_color` (0131). Pipes/non-TTY and `NO_COLOR`
/// suppress ANSI; this test still strips escapes if present so HTML-entity
/// checks remain robust under force-on CI TTYs.
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
    // Positive: highlighting survived. Strip ANSI first — gated owo_colors may
    // still insert escapes under force-on/TTY, so a raw contains() on the plain
    // identifier can fail against a correct emphasized rendering.
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

/// DoD-5 / 0136: `search <identifier> --json` — whole stdout is one envelope;
/// results[].content is plain (no ANSI / HTML entities).
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
    assert!(!stdout.trim().is_empty(), "expected envelope JSON");

    let env: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("DoD-5/0136: whole stdout must parse as one JSON object: {e}; stdout={stdout}");
    });
    assert_eq!(env["schemaVersion"], 1);
    let results = env["results"].as_array().expect("results array");
    assert!(!results.is_empty(), "expected at least one hit: {stdout}");
    for hit in results {
        let content = hit["content"].as_str().unwrap_or("");
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

/// 0126 / 0136: empty index + search --json emits searchIndexStatus on the envelope.
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

    let env: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("expected single envelope JSON");
    assert_eq!(env["schemaVersion"], 1);
    assert_eq!(env["resultCount"], 0);
    assert_eq!(env["results"], serde_json::json!([]));
    let status = env
        .get("searchIndexStatus")
        .expect("searchIndexStatus must be present on empty-index path");
    let state = status["state"].as_str().unwrap_or("");
    assert!(
        state == "was_empty" || state == "empty_after_rebuild",
        "unexpected state: {status}"
    );
    assert!(status["documentCount"].is_number());
    if state == "empty_after_rebuild" {
        let rem = status["remediation"].as_str().unwrap_or("");
        assert!(
            rem.contains("Rebuild") || rem.contains("indexable") || rem.contains("ignore"),
            "empty_after_rebuild needs B2 honesty: {rem}"
        );
    }
}

/// 0126 / 0136: populated index + zero hits must not claim was_empty / empty_after_rebuild.
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

    // Query something that won't match; pre_count > 0 so no searchIndexStatus.
    let (stdout, stderr, code) =
        run_cli(root, &["search", "zzzz_nonexistent_token_0126", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let env: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("empty-hit envelope must parse");
    assert_eq!(env["schemaVersion"], 1);
    assert_eq!(env["resultCount"], 0);
    assert!(
        env.get("searchIndexStatus").is_none(),
        "populated index zero-hit must not claim empty index: {stdout}"
    );
}

/// 0126 / 0136: empty Tantivy + indexable sources → auto-rebuild yields
/// `searchIndexStatus.state == "was_empty"` with `documentCount >= 1`.
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

    let env: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("expected single envelope");
    assert_eq!(env["schemaVersion"], 1);
    let status = env
        .get("searchIndexStatus")
        .expect("searchIndexStatus required on was_empty path");
    assert_eq!(
        status["state"].as_str().unwrap_or(""),
        "was_empty",
        "successful rebuild must emit was_empty not empty_after_rebuild: {status}"
    );
    let docs = status["documentCount"]
        .as_u64()
        .expect("documentCount number");
    assert!(
        docs >= 1,
        "was_empty path requires documentCount >= 1: {status}"
    );
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

    let env: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("envelope parse after auto-index");
    assert_eq!(env["schemaVersion"], 1);
    let has_hit = env["results"]
        .as_array()
        .expect("results")
        .iter()
        .any(|hit| {
            let kind = hit["kind"].as_str().unwrap_or("");
            let content = hit["content"].as_str().unwrap_or("");
            let path = hit["path"].as_str().unwrap_or("");
            matches!(
                kind,
                "bm25_match" | "fuzzy_match" | "regex_match" | "insight"
            ) && (content.contains(token) || path.contains(token) || content.contains("lib"))
        });
    assert!(
        has_hit,
        "0128: auto-index + FTS rebuild must surface new token; stdout={stdout}; stderr={stderr}"
    );
}

/// 0136: multi-hit `--json` is one envelope (whole-stdout parse).
#[test]
fn search_json_multi_hit_envelope() {
    use crate::common::git_add_and_commit;

    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    fs::write(root.join("a.rs"), "pub fn multi_hit_alpha() {}\n").unwrap();
    fs::write(root.join("b.rs"), "pub fn multi_hit_beta() {}\n").unwrap();
    fs::write(root.join("c.rs"), "pub fn multi_hit_gamma() {}\n").unwrap();
    git_add_and_commit(root, "a.rs b.rs c.rs");

    let (stdout, stderr, code) = run_cli(
        root,
        &["search", "multi_hit", "--index", "--json", "--limit", "5"],
    );
    assert_eq!(code, 0, "stderr={stderr}; stdout={stdout}");

    // Whole stdout must be one object — not NDJSON multi-document.
    let env: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("0136 multi-hit whole-stdout parse failed: {e}; stdout={stdout}");
    });
    assert_eq!(env["schemaVersion"], 1);
    let count = env["resultCount"].as_u64().expect("resultCount");
    let results = env["results"].as_array().expect("results");
    assert_eq!(count as usize, results.len());
    assert!(count >= 2, "expected multi-hit: {stdout}");
    assert!(env.get("bridge_version").is_none());
    assert!(env.get("record_kind").is_none());
}

/// 0136: empty results still emit a full parseable envelope.
#[test]
fn search_json_empty_results_envelope() {
    use crate::common::git_add_and_commit;

    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    fs::write(root.join("a.rs"), "pub fn only_real_symbol() {}\n").unwrap();
    git_add_and_commit(root, "a.rs");

    let (stdout, stderr, code) = run_cli(
        root,
        &[
            "search",
            "zzzz_no_match_0136_unique",
            "--index",
            "--json",
            "--limit",
            "5",
        ],
    );
    assert_eq!(code, 0, "stderr={stderr}; stdout={stdout}");
    let env: serde_json::Value = serde_json::from_str(stdout.trim()).expect("empty envelope parse");
    assert_eq!(env["schemaVersion"], 1);
    assert_eq!(env["resultCount"], 0);
    assert_eq!(env["results"], serde_json::json!([]));
    assert_eq!(env["query"], "zzzz_no_match_0136_unique");
}

/// 0136: truncation via overfetch-by-1.
#[test]
fn search_json_truncation_flag() {
    use crate::common::git_add_and_commit;

    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    for name in ["trunc_a", "trunc_b", "trunc_c", "trunc_d"] {
        fs::write(
            root.join(format!("{name}.rs")),
            format!("pub fn trunc_shared_token_{name}() {{}}\n"),
        )
        .unwrap();
    }
    git_add_and_commit(root, "trunc_a.rs trunc_b.rs trunc_c.rs trunc_d.rs");

    let (stdout, stderr, code) = run_cli(
        root,
        &[
            "search",
            "trunc_shared_token",
            "--index",
            "--json",
            "--limit",
            "2",
        ],
    );
    assert_eq!(code, 0, "stderr={stderr}; stdout={stdout}");
    let env: serde_json::Value = serde_json::from_str(stdout.trim()).expect("envelope");
    assert_eq!(env["resultCount"], 2);
    assert_eq!(
        env["truncated"], true,
        "limit 2 with ≥3 hits must set truncated: {stdout}"
    );

    // Under-limit query → truncated false
    let (stdout2, stderr2, code2) = run_cli(
        root,
        &["search", "trunc_shared_token", "--json", "--limit", "50"],
    );
    assert_eq!(code2, 0, "stderr={stderr2}");
    let env2: serde_json::Value = serde_json::from_str(stdout2.trim()).expect("envelope");
    assert_eq!(
        env2["truncated"], false,
        "under-limit must be false: {stdout2}"
    );
    assert!(env2["resultCount"].as_u64().unwrap_or(0) >= 3);
}

/// 0136: `--json-lines` preserves NDJSON BridgeRecord stream.
#[test]
fn search_json_lines_multi_hit_ndjson() {
    use crate::common::git_add_and_commit;

    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    fs::write(root.join("a.rs"), "pub fn lines_hit_alpha() {}\n").unwrap();
    fs::write(root.join("b.rs"), "pub fn lines_hit_beta() {}\n").unwrap();
    git_add_and_commit(root, "a.rs b.rs");

    let (stdout, stderr, code) = run_cli(
        root,
        &[
            "search",
            "lines_hit",
            "--index",
            "--json-lines",
            "--limit",
            "5",
        ],
    );
    assert_eq!(code, 0, "stderr={stderr}; stdout={stdout}");

    // Whole stdout must NOT parse as a single object with schemaVersion (NDJSON).
    let whole = serde_json::from_str::<serde_json::Value>(stdout.trim());
    if let Ok(v) = &whole {
        assert!(
            v.get("schemaVersion").is_none(),
            "json-lines must not emit envelope: {stdout}"
        );
    }

    let mut n = 0usize;
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("line parse: {e}; {line}"));
        assert!(
            v.get("record_kind").is_some(),
            "legacy BridgeRecord needs record_kind: {line}"
        );
        n += 1;
    }
    assert!(n >= 2, "expected multi-line NDJSON: {stdout}");
}

/// 0136: clap rejects both flags.
#[test]
fn search_json_and_json_lines_conflict() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    let (stdout, stderr, code) = run_cli(root, &["search", "x", "--json", "--json-lines"]);
    assert_ne!(
        code, 0,
        "conflict must be non-zero; stdout={stdout}; stderr={stderr}"
    );
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("cannot be used with")
            || combined.contains("conflict")
            || combined.contains("json-lines")
            || combined.contains("json"),
        "expected clap conflict message: {combined}"
    );
}

/// 0136 B3: fatal auto-index path emits no machine stdout under `--json`.
#[test]
fn search_json_fatal_auto_index_no_stdout() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    // init so layout exists; empty repo → try_auto_index often bails RepositoryEmpty.
    let (init_out, init_err, init_code) = run_cli(root, &["init"]);
    assert_eq!(init_code, 0, "init; stderr={init_err}; stdout={init_out}");

    let (stdout, stderr, code) = run_cli(root, &["search", "anything", "--auto-index", "--json"]);
    // Either fatal (non-zero + empty stdout) or successful empty envelope — both valid.
    // When non-zero, stdout must be empty (no partial BridgeRecord / envelope).
    if code != 0 {
        assert!(
            stdout.trim().is_empty(),
            "fatal auto-index must leave no machine stdout; stdout={stdout}; stderr={stderr}"
        );
    } else {
        // Soft success path (e.g. 0 indexable files treated as up-to-date): still one envelope.
        let env: serde_json::Value =
            serde_json::from_str(stdout.trim()).expect("success path must still be one envelope");
        assert_eq!(env["schemaVersion"], 1);
    }
}
