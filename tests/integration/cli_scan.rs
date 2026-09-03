use ledgerful::cli::args::ScanImpactMode;
use ledgerful::commands::scan::{execute_scan, execute_scan_with_opts};
use ledgerful::state::layout::Layout;
use ledgerful::state::reports::{LATEST_IMPACT_REPORT, write_impact_report};
use std::fs;
use std::process::Command;
use std::time::Duration;
use tempfile::tempdir;

use crate::common::{DirGuard, setup_git_repo};

fn git_cmd(dir: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn scan_clean_tree_reports_no_changes() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    fs::write(root.join("initial.txt"), "hello").unwrap();
    git_cmd(root, &["add", "initial.txt"]);
    git_cmd(root, &["commit", "-m", "initial commit"]);

    let _guard = DirGuard::new(root);

    let result = execute_scan(false, false, false, None, None, None, None);
    assert!(result.is_ok());

    let layout = Layout::new(root.to_string_lossy().as_ref());
    let report = fs::read_to_string(layout.reports_dir().join("latest-scan.json")).unwrap();
    assert!(report.contains("\"isClean\": true"));
}

#[test]
fn scan_dirty_tree_reports_changed_files() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    fs::write(root.join("initial.txt"), "hello").unwrap();
    git_cmd(root, &["add", "initial.txt"]);
    git_cmd(root, &["commit", "-m", "initial commit"]);

    // Add untracked file
    fs::write(root.join("untracked.txt"), "new").unwrap();

    // Modify existing file
    fs::write(root.join("initial.txt"), "modified").unwrap();

    // Stage a change
    fs::write(root.join("staged.txt"), "staged").unwrap();
    git_cmd(root, &["add", "staged.txt"]);

    let _guard = DirGuard::new(root);

    let result = execute_scan(false, false, false, None, None, None, None);
    assert!(result.is_ok());

    let layout = Layout::new(root.to_string_lossy().as_ref());
    let report = fs::read_to_string(layout.reports_dir().join("latest-scan.json")).unwrap();
    assert!(report.contains("initial.txt"));
    assert!(report.contains("untracked.txt"));
}

#[test]
fn scan_detached_head_reports_detached_state() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    fs::write(root.join("initial.txt"), "hello").unwrap();
    git_cmd(root, &["add", "initial.txt"]);
    git_cmd(root, &["commit", "-m", "initial commit"]);

    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .unwrap();
    let head_sha = String::from_utf8(output.stdout).unwrap().trim().to_string();

    git_cmd(root, &["checkout", &head_sha]);

    let _guard = DirGuard::new(root);

    let result = execute_scan(false, false, false, None, None, None, None);
    assert!(result.is_ok());
}

#[test]
fn test_scan_impact_out_writes_json_without_json_flag() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    fs::write(root.join("initial.txt"), "hello").unwrap();
    git_cmd(root, &["add", "initial.txt"]);
    git_cmd(root, &["commit", "-m", "initial commit"]);

    fs::write(root.join("initial.txt"), "modified").unwrap();

    let out_path = root.join("impact.json");
    let _guard = DirGuard::new(root);

    execute_scan(true, false, false, Some(out_path.clone()), None, None, None).unwrap();

    let content = fs::read_to_string(out_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["schemaVersion"], "v1");
    assert!(
        parsed.get("kind").is_none(),
        "impact packet must not set kind gitScan, got {parsed}"
    );
    assert!(parsed["changes"].is_array());
    // Impact-shaped: at least one of the escalate-only keys is present.
    assert!(
        parsed.get("riskLevel").is_some()
            || parsed.get("agentSummary").is_some()
            || parsed.get("blastRadius").is_some()
            || parsed.get("testCoverage").is_some(),
        "impact packet should include impact-only fields, got keys: {:?}",
        parsed.as_object().map(|o| o.keys().collect::<Vec<_>>())
    );
}

#[test]
fn test_scan_out_emits_git_scan_without_impact() {
    // 0180-B: --out without --impact writes gitScan file (not require-impact error).
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    let out_path = root.join("scan-out.json");
    let _guard = DirGuard::new(root);
    execute_scan(
        false,
        false,
        false,
        Some(out_path.clone()),
        None,
        None,
        None,
    )
    .expect("scan --out without --impact should emit gitScan");

    let content = fs::read_to_string(&out_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["schemaVersion"], 1);
    assert_eq!(parsed["kind"], "gitScan");
    assert!(parsed["isClean"].as_bool().unwrap());
}

#[test]
fn test_scan_json_emits_git_scan_without_impact() {
    // 0180-B: bare --json emits gitScan (not require-impact error).
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    let _guard = DirGuard::new(root);
    // Capture via --out to avoid relying on process stdout from library call;
    // execute_scan prints JSON to stdout when out is None — use out for assertion.
    let out_path = root.join("scan-json.json");
    execute_scan(false, false, true, Some(out_path.clone()), None, None, None)
        .expect("scan --json without --impact should emit gitScan");

    let content = fs::read_to_string(&out_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["schemaVersion"], 1);
    assert_eq!(parsed["kind"], "gitScan");
    assert!(parsed["changes"].is_array());
}

#[test]
fn test_scan_summary_requires_impact() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    let _guard = DirGuard::new(root);
    let error = execute_scan(false, true, false, None, None, None, None).unwrap_err();
    let msg = error.to_string();
    assert!(
        msg.contains("--impact"),
        "expected impact requirement error, got {error:?}"
    );
    assert!(
        msg.contains("--summary"),
        "expected --summary in message, got {msg}"
    );
    assert!(
        !msg.contains("--format json") && !msg.contains("scan --pr"),
        "summary reject must not tip PR format, got {msg}"
    );
}

#[test]
fn test_scan_json_dirty_tree_emits_changes() {
    // Plan matrix: dirty tree → isClean false + non-empty changes.
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("dirty.txt"), "v1").unwrap();
    git_cmd(root, &["add", "dirty.txt"]);
    git_cmd(root, &["commit", "-m", "seed"]);
    fs::write(root.join("dirty.txt"), "v2").unwrap();

    let out_path = root.join("dirty-scan.json");
    let _guard = DirGuard::new(root);
    execute_scan(false, false, true, Some(out_path.clone()), None, None, None)
        .expect("dirty scan --json should emit gitScan");

    let content = fs::read_to_string(&out_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["kind"], "gitScan");
    assert_eq!(parsed["isClean"], false);
    let changes = parsed["changes"].as_array().expect("changes array");
    assert!(
        !changes.is_empty(),
        "dirty tree should list changes, got {parsed}"
    );
    let paths: Vec<&str> = changes.iter().filter_map(|c| c["path"].as_str()).collect();
    assert!(
        paths.iter().any(|p| p.contains("dirty.txt")),
        "expected dirty.txt in changes, got {paths:?}"
    );
}

#[test]
fn test_scan_json_base_ref_emits_git_scan() {
    // 0180-G: --json --base-ref → gitScan; diffSummaries may be empty.
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::write(root.join("tracked.txt"), "v1").unwrap();
    git_cmd(root, &["add", "tracked.txt"]);
    git_cmd(root, &["commit", "-m", "base"]);
    fs::write(root.join("tracked.txt"), "v2").unwrap();
    git_cmd(root, &["add", "tracked.txt"]);
    git_cmd(root, &["commit", "-m", "tip"]);

    let out_path = root.join("base-ref-scan.json");
    let _guard = DirGuard::new(root);
    execute_scan(
        false,
        false,
        true,
        Some(out_path.clone()),
        Some("HEAD~1".into()),
        None,
        None,
    )
    .expect("scan --json --base-ref should emit gitScan");

    let content = fs::read_to_string(&out_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["kind"], "gitScan");
    assert_eq!(parsed["schemaVersion"], 1);
    assert!(parsed["diffSummaries"].is_array());
}

#[test]
fn test_scan_json_paths_still_requires_impact() {
    // AI1 P3-2: --json --paths still requires --impact.
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    let _guard = DirGuard::new(root);
    let error = execute_scan_with_opts(
        false,
        false,
        true,
        None,
        None,
        None,
        None,
        None,
        vec!["src/foo.rs".into()],
        false,
        None,
        false,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("--impact"),
        "expected --paths requires --impact, got {error:?}"
    );
}

#[test]
fn test_scan_impact_excludes_tracked_ignored() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    fs::create_dir_all(root.join(".ledgerful")).unwrap();
    fs::write(
        root.join(".ledgerful/config.toml"),
        "[watch]\nignore_patterns = [\"ignored.rs\"]\n",
    )
    .unwrap();

    fs::write(root.join("ignored.rs"), "// ignored content").unwrap();
    git_cmd(root, &["add", "ignored.rs"]);
    git_cmd(root, &["commit", "-m", "add ignored"]);
    fs::write(root.join("ignored.rs"), "// modified ignored content").unwrap();

    fs::write(root.join("normal.rs"), "// normal content").unwrap();
    git_cmd(root, &["add", "normal.rs"]);
    git_cmd(root, &["commit", "-m", "add normal"]);
    fs::write(root.join("normal.rs"), "// modified normal content").unwrap();

    let _guard = DirGuard::new(root);

    let result = execute_scan(true, false, false, None, None, None, None);
    assert!(result.is_ok());

    let layout = Layout::new(root.to_string_lossy().as_ref());
    let report = fs::read_to_string(layout.reports_dir().join("latest-scan.json")).unwrap();
    assert!(
        !report.contains("ignored.rs"),
        "Report should not contain ignored.rs under impact"
    );
    assert!(
        report.contains("normal.rs"),
        "Report should contain normal.rs"
    );
}

#[test]
fn test_scan_impact_proactive_guidance_clean_tree() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    let _guard = DirGuard::new(root);

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["scan", "--impact"])
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Working tree is clean"),
        "Expected output to indicate clean tree, got: {}",
        stdout
    );
    assert!(
        !stdout.contains("ledgerful ledger status"),
        "Clean-tree scan should not suggest ledger status, got: {}",
        stdout
    );
}

#[test]
fn scan_with_base_ref_emits_changed_files() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    // Create initial commit so HEAD~1 exists
    fs::write(root.join("base.txt"), "base content").unwrap();
    git_cmd(root, &["add", "base.txt"]);
    git_cmd(root, &["commit", "-m", "base commit"]);

    // Commit the file we want to detect
    fs::write(root.join("tracked.txt"), "tracked content").unwrap();
    git_cmd(root, &["add", "tracked.txt"]);
    git_cmd(root, &["commit", "-m", "add tracked file"]);

    let out_path = root.join("impact.json");
    let _guard = DirGuard::new(root);

    execute_scan(
        true,
        false,
        false,
        Some(out_path.clone()),
        Some("HEAD~1".to_string()),
        None,
        None,
    )
    .unwrap();

    let content = fs::read_to_string(out_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    let changes = parsed["changes"].as_array().unwrap();
    let paths: Vec<&str> = changes.iter().filter_map(|c| c["path"].as_str()).collect();
    assert!(
        paths.iter().any(|p| p.contains("tracked.txt")),
        "expected tracked.txt in changed_files, got: {:?}",
        paths
    );
}

#[test]
fn scan_with_base_ref_empty_when_no_diff() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    fs::write(root.join("initial.txt"), "hello").unwrap();
    git_cmd(root, &["add", "initial.txt"]);
    git_cmd(root, &["commit", "-m", "initial commit"]);

    let out_path = root.join("impact.json");
    let _guard = DirGuard::new(root);

    // HEAD...HEAD produces no diff
    execute_scan(
        true,
        false,
        false,
        Some(out_path.clone()),
        Some("HEAD".to_string()),
        None,
        None,
    )
    .unwrap();

    let content = fs::read_to_string(out_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(
        parsed["changes"].as_array().unwrap().is_empty(),
        "expected changes to be empty for HEAD...HEAD diff, got: {:?}",
        parsed["changes"]
    );
}

#[test]
fn scan_with_base_ref_detects_deleted_file() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);

    // Create initial commit with the file to be deleted
    fs::write(root.join("to_delete.rs"), "fn main() {}").unwrap();
    git_cmd(root, &["add", "to_delete.rs"]);
    git_cmd(root, &["commit", "-m", "initial"]);

    // Delete the file and commit
    fs::remove_file(root.join("to_delete.rs")).unwrap();
    git_cmd(root, &["rm", "to_delete.rs"]);
    git_cmd(root, &["commit", "-m", "delete file"]);

    let out_path = root.join("out.json");
    let _guard = DirGuard::new(root);

    execute_scan(
        true,
        false,
        true,
        Some(out_path.clone()),
        Some("HEAD~1".to_string()),
        None,
        None,
    )
    .unwrap();

    let content = fs::read_to_string(&out_path).unwrap();
    let packet: serde_json::Value = serde_json::from_str(&content).unwrap();
    let changes = packet["changes"].as_array().unwrap();
    assert!(!changes.is_empty(), "expected at least one changed file");
    let deleted = changes.iter().find(|c| {
        c["path"]
            .as_str()
            .map(|p| p.contains("to_delete"))
            .unwrap_or(false)
    });
    assert!(deleted.is_some(), "expected to_delete.rs in changes");
    let status = deleted.unwrap()["status"].as_str().unwrap_or("");
    assert_eq!(status, "Deleted");
}

fn seed_latest_impact(
    root: &std::path::Path,
    marker: &str,
) -> (Layout, String, std::time::SystemTime) {
    let layout = Layout::new(root.to_string_lossy().as_ref());
    layout.ensure_state_dir().unwrap();
    let seed = ledgerful::impact::packet::ImpactPacket {
        schema_version: "v1".to_string(),
        head_hash: Some(marker.to_string()),
        risk_reasons: vec!["seed-docs-do-not-clobber".to_string()],
        ..Default::default()
    };
    write_impact_report(&layout, &seed).unwrap();
    let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
    let before_meta = fs::metadata(report_path.as_std_path()).unwrap();
    let before_mtime = before_meta.modified().unwrap();
    let before = fs::read_to_string(report_path.as_std_path()).unwrap();
    assert!(before.contains(marker));
    (layout, before, before_mtime)
}

fn assert_latest_impact_unclobbered(
    layout: &Layout,
    before: &str,
    before_mtime: std::time::SystemTime,
) {
    let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
    let after_meta = fs::metadata(report_path.as_std_path()).unwrap();
    let after = fs::read_to_string(report_path.as_std_path()).unwrap();
    assert_eq!(
        before, after,
        "docs mode must not rewrite latest-impact.json contents"
    );
    assert_eq!(
        before_mtime,
        after_meta.modified().unwrap(),
        "docs mode must not change latest-impact.json mtime"
    );
}

/// H-0227-1: `--mode docs` on a **source** dirty tree (not `--paths`) must skip
/// `execute_impact_silent*` write. Deleting `|| docs_mode` would clobber the seed.
#[test]
fn scan_docs_mode_does_not_clobber_latest_impact_mtime() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "fn a() {}").unwrap();
    git_cmd(root, &["add", "src/lib.rs"]);
    git_cmd(root, &["commit", "-m", "src"]);
    fs::write(root.join("src/lib.rs"), "fn a() { /* dirty */ }").unwrap();

    let (layout, before, before_mtime) = seed_latest_impact(root, "SEED_MARKER_0227_DOCS");
    std::thread::sleep(Duration::from_millis(100));

    let out_path = root.join("docs-mode-impact.json");
    let _guard = DirGuard::new(root);
    execute_scan_with_opts(
        true,
        false,
        true,
        Some(out_path.clone()),
        None,
        None,
        None,
        None,
        Vec::new(),
        false,
        Some(ScanImpactMode::Docs),
        false,
    )
    .expect("scan --impact --mode docs on source dirty tree");

    let packet: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out_path).unwrap()).unwrap();
    assert_eq!(packet["schemaVersion"], "v1");
    assert!(
        packet.get("glossary").is_some(),
        "explicit --mode docs must apply presentation: {packet}"
    );
    assert_latest_impact_unclobbered(&layout, &before, before_mtime);
}

/// H-0227-1 auto-detect: docs-only working tree, no `--mode`, no `--paths`.
#[test]
fn scan_docs_auto_detect_does_not_clobber_latest_impact_mtime() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(root.join("docs/note.md"), "v1").unwrap();
    git_cmd(root, &["add", "docs/note.md"]);
    git_cmd(root, &["commit", "-m", "docs"]);
    fs::write(root.join("docs/note.md"), "v2 dirty").unwrap();

    let (layout, before, before_mtime) = seed_latest_impact(root, "SEED_MARKER_0227_AUTO");
    std::thread::sleep(Duration::from_millis(100));

    let out_path = root.join("auto-docs-impact.json");
    let _guard = DirGuard::new(root);
    execute_scan_with_opts(
        true,
        false,
        true,
        Some(out_path.clone()),
        None,
        None,
        None,
        None,
        Vec::new(),
        false,
        None,
        false,
    )
    .expect("scan --impact auto-detect docs-only dirty tree");

    let packet: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out_path).unwrap()).unwrap();
    assert_eq!(packet["schemaVersion"], "v1");
    assert!(
        packet.get("glossary").is_some(),
        "docs-only dirty tree must auto-detect: {packet}"
    );
    assert_latest_impact_unclobbered(&layout, &before, before_mtime);
}

/// M-0227-1: `--mode docs` without `--impact` errors before `open_repo`.
#[test]
fn execute_scan_mode_docs_without_impact_errors_before_open_repo() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    // No git repo on purpose.
    let _guard = DirGuard::new(root);
    let error = execute_scan_with_opts(
        false,
        false,
        false,
        None,
        None,
        None,
        None,
        None,
        Vec::new(),
        false,
        Some(ScanImpactMode::Docs),
        false,
    )
    .unwrap_err();
    let msg = error.to_string();
    assert!(
        msg.contains("--impact"),
        "expected --mode requires --impact, got {msg}"
    );
    assert!(
        !msg.contains("Failed to discover git repository"),
        "reject must fire before open_repo, got {msg}"
    );
}

/// M-0227-2: `--paths` docs-only vs mixed execute fixtures (plan Phase 0/3).
#[test]
fn scan_impact_paths_docs_only_emits_glossary_mixed_does_not() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);

    fs::create_dir_all(root.join("docs")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("docs/agent-output-contract.md"), "docs").unwrap();
    fs::write(root.join("docs/installation.md"), "install").unwrap();
    fs::write(root.join("src/lib.rs"), "fn x() {}").unwrap();
    git_cmd(root, &["add", "-A"]);
    git_cmd(root, &["commit", "-m", "seed"]);

    let _guard = DirGuard::new(root);

    let docs_out = root.join("paths-docs.json");
    execute_scan_with_opts(
        true,
        false,
        true,
        Some(docs_out.clone()),
        None,
        None,
        None,
        None,
        vec!["docs/agent-output-contract.md".into()],
        false,
        None,
        false,
    )
    .expect("scan --impact --json --paths docs-only");
    let docs_packet: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&docs_out).unwrap()).unwrap();
    assert_eq!(docs_packet["schemaVersion"], "v1");
    assert!(
        docs_packet.get("glossary").is_some(),
        "docs-only --paths must enter docs mode: {docs_packet}"
    );
    let glossary = docs_packet["glossary"]
        .as_object()
        .expect("glossary object");
    assert!(glossary.contains_key("no_source_seeds"));
    assert!(glossary.contains_key("mapped=0"));
    if let Some(lead) = docs_packet["actionableLead"].as_array() {
        for item in lead {
            let a = item["fileA"].as_str().unwrap_or("");
            let b = item["fileB"].as_str().unwrap_or("");
            let pair_trivia = (a.contains("docs/") || a.ends_with(".md"))
                && (b.contains("src/") || b.ends_with(".rs"));
            assert!(
                !pair_trivia,
                "docs↔crate trivia must not be in actionableLead: {item}"
            );
        }
    }

    let mixed_out = root.join("paths-mixed.json");
    execute_scan_with_opts(
        true,
        false,
        true,
        Some(mixed_out.clone()),
        None,
        None,
        None,
        None,
        vec!["src/lib.rs".into(), "docs/installation.md".into()],
        false,
        None,
        false,
    )
    .expect("scan --impact --json --paths mixed");
    let mixed_packet: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&mixed_out).unwrap()).unwrap();
    assert_eq!(mixed_packet["schemaVersion"], "v1");
    assert!(
        mixed_packet.get("glossary").is_none(),
        "mixed --paths must not auto-detect docs mode: {mixed_packet}"
    );
}
