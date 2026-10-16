//! Hermetic Rust and TypeScript change-packet qualification (0514).
//!
//! Ceilings and the child env live here. `common::run_cli` is unchanged.

use crate::common::{
    git_add_and_commit_no_verify, git_cmd, nested_repo_root, run_cli_env, setup_git_repo,
};
use camino::Utf8Path;
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use rstest::rstest;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;
use std::time::Instant;
use tempfile::TempDir;

const MAX_MS: u128 = 20_000;
const MAX_STDOUT_BYTES: usize = 262_144;
const DOCS_COMMITS: u32 = 2;
/// `TemporalEngine` returns `InsufficientHistory` until 10 kept commits.
/// These three commits touch only `notes/pad.txt`, which stays under
/// `min_revisions`, so the handler/downstream score is unchanged.
const PAD_COMMITS: u32 = 3;

#[derive(Clone, Copy, Debug)]
enum Lang {
    Rust,
    Ts,
}

impl Lang {
    fn dir(self) -> &'static str {
        match self {
            Self::Rust => "rust-svc",
            Self::Ts => "ts-svc",
        }
    }

    fn handler(self) -> &'static str {
        match self {
            Self::Rust => "src/handler.rs",
            Self::Ts => "src/handler.ts",
        }
    }

    fn downstream(self) -> &'static str {
        match self {
            Self::Rust => "src/downstream.rs",
            Self::Ts => "src/downstream.ts",
        }
    }

    fn route(self) -> &'static str {
        match self {
            Self::Rust => "src/routes.rs",
            Self::Ts => "src/server.ts",
        }
    }

    fn service(self) -> &'static str {
        match self {
            Self::Rust => "rust-svc",
            Self::Ts => "ts-svc",
        }
    }
}

struct Repo {
    _tmp: TempDir,
    root: PathBuf,
    lang: Lang,
}

fn fixture_src(lang: Lang) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/qualification")
        .join(lang.dir())
}

fn copy_tree(src: &Path, dst: &Path) {
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let dest_name = if name == "Cargo.toml.fixture" {
            std::ffi::OsString::from("Cargo.toml")
        } else {
            name
        };
        let dest = dst.join(dest_name);
        let file_type = entry.file_type().unwrap();
        if file_type.is_dir() {
            fs::create_dir_all(&dest).unwrap();
            copy_tree(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

fn append_line(path: &Path, line: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut text = if path.exists() {
        fs::read_to_string(path).unwrap()
    } else {
        String::new()
    };
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(line);
    text.push('\n');
    fs::write(path, text).unwrap();
}

fn insert_rev(path: &Path, rev: u32) {
    let text = fs::read_to_string(path).unwrap();
    let marker = "// slot";
    let idx = text
        .find(marker)
        .unwrap_or_else(|| panic!("missing // slot in {}", path.display()));
    let at = idx + marker.len();
    let mut next = String::new();
    next.push_str(&text[..at]);
    next.push_str(&format!(" {rev}"));
    next.push_str(&text[at..]);
    fs::write(path, next).unwrap();
}

fn replace_once(path: &Path, from: &str, to: &str) {
    let text = fs::read_to_string(path).unwrap();
    assert!(text.contains(from), "missing {from} in {}", path.display());
    fs::write(path, text.replacen(from, to, 1)).unwrap();
}

/// `git add -A` + `git commit` with a fixed author date.
///
/// Dead-code confidence blends reachability with git inactivity (`90` days).
/// A same-day history keeps an uncalled `#[cfg(test)]` helper under the `0.75`
/// threshold, so `--include-tests` would not name it. The date is history
/// metadata. It does not change the co-change counts.
fn git_add_and_commit_dated(dir: &Path, msg: &str) {
    let added = Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        added.status.success(),
        "git add failed: {}",
        String::from_utf8_lossy(&added.stderr)
    );
    let committed = Command::new("git")
        .args(["commit", "-m", msg])
        .current_dir(dir)
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00+00:00")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00+00:00")
        .output()
        .unwrap();
    assert!(
        committed.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&committed.stderr)
    );
}

/// Commit without the init-installed post-commit hook.
///
/// `--no-verify` skips pre-commit only. The post-commit hook runs
/// `index` and would move `index_metadata.head_hash` onto the new commit,
/// so a route row could not show `stale`.
fn commit_without_hooks(dir: &Path, msg: &str) {
    git_cmd(dir, &["add", "-A"]);
    fs::create_dir_all(dir.join("empty-hooks")).unwrap();
    git_cmd(
        dir,
        &[
            "-c",
            "core.hooksPath=empty-hooks",
            "commit",
            "--no-verify",
            "-m",
            msg,
        ],
    );
}

fn prepare(lang: Lang, docs: u32, pads: u32) -> Repo {
    let tmp = tempfile::tempdir().unwrap();
    let root = nested_repo_root(tmp.path());
    copy_tree(&fixture_src(lang), &root);
    setup_git_repo(&root);
    git_cmd(&root, &["config", "core.autocrlf", "false"]);
    git_add_and_commit_dated(&root, "initial");
    for rev in 2..=5 {
        insert_rev(&root.join(lang.handler()), rev);
        insert_rev(&root.join(lang.downstream()), rev);
        git_add_and_commit_dated(&root, &format!("pair {rev}"));
    }
    for n in 1..=docs {
        append_line(&root.join("docs/guide.md"), &format!("History note {n}."));
        git_add_and_commit_dated(&root, &format!("docs {n}"));
    }
    for n in 1..=pads {
        append_line(&root.join("notes/pad.txt"), &format!("pad {n}"));
        git_add_and_commit_dated(&root, &format!("pad {n}"));
    }
    Repo {
        _tmp: tmp,
        root,
        lang,
    }
}

fn history(lang: Lang) -> Repo {
    prepare(lang, DOCS_COMMITS, PAD_COMMITS)
}

fn run_raw(dir: &Path, args: &[&str]) -> (String, String, i32) {
    static BIN: Once = Once::new();
    BIN.call_once(|| {
        eprintln!("QUALIFY binary={}", env!("CARGO_BIN_EXE_ledgerful"));
    });
    let started = Instant::now();
    let (stdout, stderr, code) = run_cli_env(
        dir,
        args,
        &[("LEDGERFUL_NON_INTERACTIVE", "1"), ("NO_COLOR", "1")],
    );
    let ms = started.elapsed().as_millis();
    let bytes = stdout.len();
    eprintln!(
        "QUALIFY args={} exit={code} bytes={bytes} ms={ms}",
        args.join(" ")
    );
    assert!(
        ms < MAX_MS,
        "qualification exceeded {MAX_MS} ms: args={args:?} exit={code} bytes={bytes} ms={ms}\nstderr={stderr}"
    );
    assert!(
        bytes <= MAX_STDOUT_BYTES,
        "qualification stdout exceeded {MAX_STDOUT_BYTES} bytes: args={args:?} exit={code} bytes={bytes} ms={ms}"
    );
    (stdout, stderr, code)
}

fn assert_ok(code: i32, stdout: &str, stderr: &str, args: &[&str]) {
    assert_eq!(code, 0, "args={args:?}\nstdout={stdout}\nstderr={stderr}");
}

fn run_ok(dir: &Path, args: &[&str]) -> String {
    let (stdout, stderr, code) = run_raw(dir, args);
    assert_ok(code, &stdout, &stderr, args);
    stdout
}

fn run_json(dir: &Path, args: &[&str]) -> Value {
    let stdout = run_ok(dir, args);
    serde_json::from_str(&stdout)
        .unwrap_or_else(|err| panic!("json parse {err} args={args:?}\n{stdout}"))
}

fn slash(value: &Value) -> String {
    value.as_str().unwrap_or_default().replace('\\', "/")
}

fn read_reason<'a>(packet: &'a Value, path: &str) -> Option<&'a str> {
    packet["readSet"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|entry| {
            if slash(&entry["path"]) == path {
                entry["reason"].as_str()
            } else {
                None
            }
        })
}

fn assert_no_budget(value: &Value) {
    let stop = value
        .pointer("/completeness/stop")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert_ne!(stop, "budget", "completeness.stop budget: {value}");
}

fn init_repo(root: &Path) {
    run_ok(root, &["init"]);
    // `init` writes an untracked `.gitignore`. A dirty tree is not the idle
    // policy target (`verification_must_pass` becomes a violation).
    if root.join(".gitignore").exists() {
        git_add_and_commit_no_verify(root, "ignore ledgerful state");
    }
}

fn index_incremental(root: &Path) {
    run_ok(root, &["index", "--incremental"]);
}

fn indexed(lang: Lang) -> Repo {
    let repo = history(lang);
    init_repo(&repo.root);
    index_incremental(&repo.root);
    repo
}

/// Idle applies when the tree is clean and no bound verification run exists.
/// A dirty tree with no bound run is a `verification_must_pass` violation
/// (`status` `fail`), which is not the false-safe `pass`.
fn assert_policy_idle(root: &Path) {
    let report = run_json(root, &["policy", "check", "--format", "json"]);
    assert_eq!(report["schemaVersion"], 1, "{report}");
    assert_eq!(report["status"], "idle", "{report}");
}

fn assert_policy_not_pass(root: &Path) {
    let report = run_json(root, &["policy", "check", "--format", "json"]);
    assert_ne!(report["status"], "pass", "{report}");
}

fn dirty_handler(repo: &Repo) {
    insert_rev(&repo.root.join(repo.lang.handler()), 99);
}

fn dirty_docs(root: &Path) {
    append_line(root.join("docs/guide.md").as_path(), "Task note.");
}

fn dirty_route_and_cedar(repo: &Repo) {
    replace_once(
        &repo.root.join("policies/access.cedar"),
        "Feature::\"orders\"",
        "Feature::\"orders-v2\"",
    );
    replace_once(
        &repo.root.join(repo.lang.route()),
        "\"/orders\"",
        "\"/orders-v2\"",
    );
}

fn restore_route_and_cedar(repo: &Repo) {
    git_cmd(
        &repo.root,
        &["checkout", "--", "policies/access.cedar", repo.lang.route()],
    );
}

fn coupling_names_pair(couplings: &[Value], left: &str, right: &str) -> bool {
    couplings.iter().any(|coupling| {
        let a = slash(&coupling["fileA"]);
        let b = slash(&coupling["fileB"]);
        (a == left && b == right) || (a == right && b == left)
    })
}

fn symbol_names(report: &Value) -> Vec<String> {
    report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|finding| finding["symbolName"].as_str().map(str::to_string))
        .collect()
}

fn hotspot_history_count(root: &Path) -> i64 {
    let repo_root = Utf8Path::from_path(root).unwrap();
    let storage = StorageManager::open_read_only_sqlite_only(&Layout::new(repo_root)).unwrap();
    let conn = storage.get_connection();
    conn.query_row("SELECT COUNT(*) FROM hotspot_history", [], |row| row.get(0))
        .unwrap()
}

fn find_uuid(text: &str) -> Option<String> {
    for token in text.split_whitespace() {
        let token = token.trim_matches(|ch: char| matches!(ch, ',' | '.' | ':' | '"' | '\''));
        let bytes = token.as_bytes();
        if bytes.len() == 36
            && bytes.iter().enumerate().all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => *byte == b'-',
                _ => byte.is_ascii_hexdigit(),
            })
        {
            return Some(token.to_string());
        }
    }
    None
}

fn docs_projection(packet: &Value) -> Value {
    let mut read_set: Vec<Value> = packet["readSet"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            json!({
                "path": slash(&entry["path"]),
                "reason": entry["reason"].as_str().unwrap_or(""),
            })
        })
        .collect();
    read_set.sort_by(|left, right| {
        left["path"]
            .as_str()
            .unwrap_or("")
            .cmp(right["path"].as_str().unwrap_or(""))
            .then_with(|| {
                left["reason"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(right["reason"].as_str().unwrap_or(""))
            })
    });
    let notes = packet
        .pointer("/blast/honestyNotes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut freshness: Vec<Value> = packet["freshness"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|row| {
            json!({
                "id": row["id"].as_str().unwrap_or(""),
                "status": row["status"].as_str().unwrap_or(""),
            })
        })
        .collect();
    freshness.sort_by(|left, right| {
        left["id"]
            .as_str()
            .unwrap_or("")
            .cmp(right["id"].as_str().unwrap_or(""))
    });
    json!({
        "schemaVersion": packet["schemaVersion"],
        "status": packet["status"],
        "changed": packet["agentSummary"]["changed"],
        "pathMode": packet["agentSummary"]["pathMode"],
        "readSet": read_set,
        "honestyNotes": notes,
        "freshness": freshness,
    })
}

fn absent_or_null(value: &Value, key: &str) -> bool {
    match value.get(key) {
        None => true,
        Some(item) => item.is_null(),
    }
}

#[rstest]
#[case::rust(Lang::Rust)]
#[case::ts(Lang::Ts)]
fn qualify_uninitialized_is_not_ready(#[case] lang: Lang) {
    let repo = history(lang);
    let packet = run_json(&repo.root, &["change-context", "--json"]);
    assert_eq!(packet["schemaVersion"], 1, "{packet}");
    assert_eq!(packet["status"], "not_ready", "{packet}");
    assert!(absent_or_null(&packet, "riskLevel"), "{packet}");
    assert!(absent_or_null(&packet, "blast"), "{packet}");
}

#[rstest]
#[case::rust(Lang::Rust)]
#[case::ts(Lang::Ts)]
fn qualify_docs_only_counts_no_code(#[case] lang: Lang) {
    let repo = indexed(lang);
    assert_policy_idle(&repo.root);
    dirty_docs(&repo.root);
    let packet = run_json(&repo.root, &["change-context", "--json"]);
    assert_no_budget(&packet);
    assert_eq!(packet["schemaVersion"], 1, "{packet}");
    assert_eq!(packet["status"], "ready", "{packet}");
    assert_eq!(packet["agentSummary"]["changed"]["code"], 0, "{packet}");
    assert!(
        packet["agentSummary"]["changed"]["governance"]
            .as_u64()
            .unwrap()
            >= 1,
        "{packet}"
    );
    assert_eq!(packet["agentSummary"]["pathMode"], "code", "{packet}");
    assert_eq!(
        read_reason(&packet, "docs/guide.md"),
        Some("changed"),
        "{packet}"
    );
    assert!(
        read_reason(&packet, lang.downstream()).is_none(),
        "{packet}"
    );
    assert_policy_not_pass(&repo.root);
}

#[rstest]
#[case::rust(Lang::Rust)]
#[case::ts(Lang::Ts)]
fn qualify_handler_names_temporal_partner(#[case] lang: Lang) {
    let repo = indexed(lang);
    assert_policy_idle(&repo.root);
    dirty_handler(&repo);
    let packet = run_json(&repo.root, &["change-context", "--json"]);
    assert_no_budget(&packet);
    assert!(packet.get("completeness").is_none(), "{packet}");
    assert_eq!(
        read_reason(&packet, lang.handler()),
        Some("changed"),
        "{packet}"
    );
    assert_eq!(
        read_reason(&packet, lang.downstream()),
        Some("temporal"),
        "missing temporal partner with completeness absent: {packet}"
    );
    assert_ne!(
        read_reason(&packet, "docs/guide.md"),
        Some("temporal"),
        "{packet}"
    );

    let impact = run_json(&repo.root, &["impact", "--json"]);
    assert_no_budget(&impact);
    assert!(impact.get("completeness").is_none(), "{impact}");
    assert_eq!(impact["schemaVersion"], "v1", "{impact}");
    let couplings = impact["temporalCouplings"].as_array().unwrap();
    assert!(
        coupling_names_pair(couplings, lang.handler(), lang.downstream()),
        "missing temporal pair with completeness absent: {impact}"
    );
    assert!(
        !coupling_names_pair(couplings, lang.handler(), "docs/guide.md"),
        "{impact}"
    );
    assert_policy_not_pass(&repo.root);
}

#[rstest]
#[case::rust(Lang::Rust)]
#[case::ts(Lang::Ts)]
fn qualify_route_policy_without_graph_is_not_clean(#[case] lang: Lang) {
    let repo = indexed(lang);
    assert_policy_idle(&repo.root);
    dirty_route_and_cedar(&repo);
    let report = run_json(&repo.root, &["security", "impact", "--json"]);
    assert_eq!(report["schemaVersion"], 1, "{report}");
    assert_eq!(report["emptyReason"], "noIndexedData", "{report}");
    assert_ne!(report["emptyReason"], "cleanDiff", "{report}");
    let message = report["message"].as_str().unwrap_or("");
    assert!(
        message.contains("`ledgerful index --analyze-graph`"),
        "{report}"
    );
    assert_policy_not_pass(&repo.root);
}

#[rstest]
#[case::rust(Lang::Rust)]
#[case::ts(Lang::Ts)]
fn qualify_route_policy_cedar_change_is_not_clean(#[case] lang: Lang) {
    let repo = indexed(lang);
    assert_policy_idle(&repo.root);
    run_ok(&repo.root, &["index", "--incremental", "--analyze-graph"]);
    dirty_route_and_cedar(&repo);
    let changed = run_json(&repo.root, &["security", "impact", "--json"]);
    assert_eq!(changed["schemaVersion"], 1, "{changed}");
    assert!(changed.get("emptyReason").is_none(), "{changed}");
    let impacted = changed["impacted"].as_array().unwrap();
    assert!(!impacted.is_empty(), "{changed}");
    assert!(
        impacted.iter().any(|row| row["is_changed"] == true),
        "{changed}"
    );

    restore_route_and_cedar(&repo);
    dirty_docs(&repo.root);
    let docs = run_json(&repo.root, &["security", "impact", "--json"]);
    assert_eq!(docs["emptyReason"], "cleanDiff", "{docs}");
    assert_eq!(
        docs["message"], "No changed policies found in the current diff.",
        "{docs}"
    );
}

#[rstest]
#[case::rust(Lang::Rust)]
#[case::ts(Lang::Ts)]
fn qualify_stale_head_marks_routes(#[case] lang: Lang) {
    let repo = indexed(lang);
    insert_rev(&repo.root.join(lang.handler()), 50);
    commit_without_hooks(&repo.root, "move head");
    let packet = run_json(&repo.root, &["change-context", "--json"]);
    assert_no_budget(&packet);
    let routes = packet["freshness"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "routes")
        .unwrap_or_else(|| panic!("missing routes freshness: {packet}"));
    assert_eq!(routes["status"], "stale", "{packet}");
}

#[rstest]
#[case::rust(Lang::Rust)]
#[case::ts(Lang::Ts)]
fn qualify_policy_check_idle_without_policy_toml(#[case] lang: Lang) {
    let repo = history(lang);
    init_repo(&repo.root);
    assert!(
        !repo.root.join(".ledgerful/policy.toml").exists(),
        "fixture grew a policy.toml"
    );
    assert_policy_idle(&repo.root);
}

#[rstest]
#[case::rust(Lang::Rust)]
#[case::ts(Lang::Ts)]
fn qualify_default_index_omits_service_map(#[case] lang: Lang) {
    let repo = indexed(lang);
    dirty_handler(&repo);
    let impact = run_json(&repo.root, &["impact", "--json"]);
    assert_no_budget(&impact);
    assert!(
        impact.get("serviceMapDelta").is_some(),
        "absent serviceMapDelta is not the default-index result: {impact}"
    );
    assert!(impact["serviceMapDelta"].is_null(), "{impact}");
}

#[rstest]
#[case::rust(Lang::Rust)]
#[case::ts(Lang::Ts)]
fn qualify_service_opt_in_names_declared_service(#[case] lang: Lang) {
    let repo = indexed(lang);
    let config_path = repo.root.join(".ledgerful/config.toml");
    let existing = fs::read_to_string(&config_path).unwrap();
    assert!(
        !existing.contains("[coverage]"),
        "init template already has [coverage]; refusing to append a second table:\n{existing}"
    );
    let block = format!(
        "\n[coverage]\nenabled = true\n\n[coverage.services]\nenabled = true\n\n[[services.definitions]]\nname = \"{}\"\nroot = \"src\"\n",
        lang.service()
    );
    fs::write(&config_path, format!("{existing}{block}")).unwrap();
    index_incremental(&repo.root);
    dirty_handler(&repo);
    let impact = run_json(&repo.root, &["impact", "--json"]);
    assert_no_budget(&impact);
    let delta = &impact["serviceMapDelta"];
    assert!(!delta.is_null(), "{impact}");
    assert!(delta.get("affectedServices").is_none(), "{delta}");
    assert!(delta.get("totalServices").is_none(), "{delta}");
    let affected: Vec<&str> = delta["affected_services"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(affected.contains(&lang.service()), "{delta}");
    assert_eq!(delta["total_services"], 1, "{delta}");
    assert!(delta.get("services").is_some(), "{delta}");
    assert!(delta.get("cross_service_edges").is_some(), "{delta}");
}

#[test]
fn qualify_rust_dead_code_omits_cfg_test() {
    let repo = indexed(Lang::Rust);
    let report = run_json(&repo.root, &["dead-code", "--json"]);
    assert_eq!(report["schemaVersion"], 1, "{report}");
    assert_eq!(report["includeTests"], false, "{report}");
    let names = symbol_names(&report);
    assert!(
        !names.iter().any(|name| name == "cfg_test_helper"),
        "{names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "uncalled_test_helper"),
        "{names:?}"
    );
    let included = run_json(&repo.root, &["dead-code", "--json", "--include-tests"]);
    let included_names = symbol_names(&included);
    assert!(
        included_names.iter().any(|name| name == "cfg_test_helper"),
        "{included_names:?}\n{included}"
    );
}

#[rstest]
#[case::rust(Lang::Rust)]
#[case::ts(Lang::Ts)]
fn qualify_review_overlaps_handler_entity(#[case] lang: Lang) {
    let repo = prepare(lang, 0, 0);
    // Review `HEAD~1..HEAD` must be the handler commit. `init_repo` commits
    // the untracked `.gitignore` and would make that range a gitignore commit.
    run_ok(&repo.root, &["init"]);
    let started = run_ok(
        &repo.root,
        &[
            "ledger",
            "start",
            lang.handler(),
            "--category",
            "FEATURE",
            "--message",
            "Qualify the handler",
        ],
    );
    let committed = run_ok(
        &repo.root,
        &[
            "ledger",
            "commit",
            "--summary",
            "Handler overlap",
            "--reason",
            "Path overlap is the rank",
        ],
    );
    let tx_id = find_uuid(&started)
        .or_else(|| find_uuid(&committed))
        .unwrap_or_else(|| panic!("no tx id in\n{started}\n{committed}"));

    let handler_review = run_json(&repo.root, &["review", "HEAD~1..HEAD", "--json"]);
    assert_eq!(handler_review["schemaVersion"], 1, "{handler_review}");
    assert_eq!(handler_review["kind"], "review", "{handler_review}");
    assert_eq!(
        handler_review["priorDecisionsRank"], "substantive-path-overlap",
        "{handler_review}"
    );
    let decision = handler_review["priorDecisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["txId"] == tx_id)
        .unwrap_or_else(|| panic!("missing {tx_id} in {handler_review}"));
    let paths: Vec<String> = decision["overlappingPaths"]
        .as_array()
        .unwrap()
        .iter()
        .map(slash)
        .collect();
    assert!(
        paths.iter().any(|path| path == lang.handler()),
        "{decision}"
    );

    append_line(&repo.root.join("docs/guide.md"), "Review docs note.");
    git_add_and_commit_no_verify(&repo.root, "docs only");
    let docs_review = run_json(&repo.root, &["review", "HEAD~1..HEAD", "--json"]);
    let docs_ids: Vec<&str> = docs_review["priorDecisions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["txId"].as_str())
        .collect();
    assert!(!docs_ids.contains(&tx_id.as_str()), "{docs_review}");
}

#[rstest]
#[case::rust(Lang::Rust)]
#[case::ts(Lang::Ts)]
fn qualify_hotspots_history_follows_complete_list(#[case] lang: Lang) {
    let repo = indexed(lang);
    let report = run_json(&repo.root, &["hotspots", "--json"]);
    assert_no_budget(&report);
    assert!(report.get("completeness").is_none(), "{report}");
    assert_eq!(report["schemaVersion"], 1, "{report}");
    let files = report["files"].as_array().unwrap();
    let mut positive = false;
    for file in files {
        let score = file["score"].as_f64().unwrap();
        assert!(
            (0.0..=1.0).contains(&score),
            "score {score} is outside 0-1: {file}"
        );
        if score > 0.0 {
            positive = true;
        }
    }
    let count = hotspot_history_count(&repo.root);
    if files.is_empty() || !positive {
        assert_eq!(count, 0, "{report}");
    } else {
        assert!(count >= 1, "count {count} report {report}");
    }
}

#[rstest]
#[case::rust(Lang::Rust)]
#[case::ts(Lang::Ts)]
fn qualify_signature_assurance_present(#[case] lang: Lang) {
    let repo = history(lang);
    init_repo(&repo.root);
    let report = run_json(&repo.root, &["verify", "--signatures", "--json"]);
    assert_eq!(report["schemaVersion"], 1, "{report}");
    let status = report["assurance"]["status"].as_str().unwrap_or("");
    assert!(
        matches!(
            status,
            "pass" | "fail" | "unpinned" | "unknownSigner" | "notEstablished" | "notRequested"
        ),
        "{report}"
    );
    assert!(report.get("assurance").is_some(), "{report}");
}

#[rstest]
#[case::rust(Lang::Rust)]
#[case::ts(Lang::Ts)]
fn qualify_repeat_docs_projection_matches(#[case] lang: Lang) {
    let repo = indexed(lang);
    dirty_docs(&repo.root);
    let first = run_json(&repo.root, &["change-context", "--json"]);
    let second = run_json(&repo.root, &["change-context", "--json"]);
    assert_no_budget(&first);
    assert_no_budget(&second);
    assert_eq!(docs_projection(&first), docs_projection(&second));
}

#[rstest]
#[case::rust(Lang::Rust)]
#[case::ts(Lang::Ts)]
fn qualify_fixture_doctor_has_no_tree_currency(#[case] lang: Lang) {
    let repo = history(lang);
    init_repo(&repo.root);
    let report = run_json(&repo.root, &["doctor", "--json"]);
    assert_eq!(report["schemaVersion"], 1, "{report}");
    let codes: Vec<&str> = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|finding| finding["code"].as_str())
        .collect();
    for forbidden in [
        "binary-behind-tree",
        "binary-ahead-of-tree",
        "binary-diverges-from-tree",
    ] {
        assert!(!codes.contains(&forbidden), "{forbidden} in {codes:?}");
    }
}
