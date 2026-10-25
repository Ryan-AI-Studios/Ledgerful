//! 0549 — `--scope fast` must not return a fmt/clippy-only green when a
//! changed non-code file (document, template, fixture) is embedded in or read
//! by tests in a third-party repo. Hermetic temp git repos, dry-run JSON only
//! (default-suite plan-shape; real cargo evidence is `cli_verify_embedded_qualification`).

use crate::common::{git_add_and_commit, setup_git_repo};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::tempdir;

fn spawn_verify(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args(args)
        .current_dir(cwd)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env_remove("LEDGERFUL_STATE_DIR")
        .output()
        .expect("spawn ledgerful")
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
    fs::write(p, body).expect("write");
}

const ACCESSOR: &str = r#"
pub fn contract() -> &'static str { include_str!("../docs/contract.md") }
pub fn message() -> &'static str { include_str!("../templates/message.txt") }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contracts_document() { assert!(contract().contains("v1")); }
    #[test]
    fn contracts_template() { assert!(message().contains("hi")); }
}
"#;

/// Benchmark-shaped fixture: production accessor with `include_str!`, a
/// SAME_FILE inline test, and a separate-file downstream test (0550-owned).
fn init_fixture(root: &Path) {
    setup_git_repo(root);
    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(root, "src/lib.rs", "pub mod contracts;\n");
    write(root, "src/contracts.rs", ACCESSOR);
    write(root, "docs/contract.md", "v1\n");
    write(root, "docs/guide.md", "guide\n");
    write(root, "templates/message.txt", "hi\n");
    write(
        root,
        "tests/service.rs",
        "#[test]\nfn service_contract() { assert!(fx::contracts::contract().contains(\"v1\")); }\n",
    );
    git_add_and_commit(root, "init");
}

fn dry_run_json(cwd: &Path, extra: &[&str]) -> serde_json::Value {
    let mut args = vec!["verify", "--scope", "fast", "--json", "--dry-run"];
    args.extend_from_slice(extra);
    let out = spawn_verify(cwd, &args);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "dry-run JSON: {e}; stdout={stdout:?} stderr={:?}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn dry_run_refusal_json(cwd: &Path, extra: &[&str]) -> serde_json::Value {
    let mut args = vec!["verify", "--scope", "fast", "--json", "--dry-run"];
    args.extend_from_slice(extra);
    let out = spawn_verify(cwd, &args);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(1), "{value}");
    assert_eq!(value["refused"], true, "{value}");
    assert_eq!(value["scopeExecuted"], "refused", "{value}");
    assert!(
        value["fallbackReason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "{value}"
    );
    value
}

fn nextest_command(v: &serde_json::Value) -> Option<String> {
    v["steps"]
        .as_array()?
        .iter()
        .filter_map(|s| s["command"].as_str())
        .find(|c| c.contains("nextest"))
        .map(str::to_string)
}

#[test]
fn dirty_txt_template_only_is_not_a_clean_tree() {
    // H0: `.txt` used to be filtered out before planning (LiveEmpty).
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_fixture(root);
    write(root, "templates/message.txt", "bye\n");

    let v = dry_run_json(root, &[]);
    assert_eq!(v["cleanTree"], false, "{v}");
    assert_eq!(v["scopeExecuted"], "fast", "{v}");
    assert_eq!(v["refused"], false, "{v}");
    let cmd = nextest_command(&v).expect("template change must schedule nextest");
    assert!(cmd.contains("test(contracts_template)"), "{cmd}");
    assert!(
        !cmd.contains("test(service_"),
        "service_* is 0550-owned, must not appear accidentally: {cmd}"
    );
}

#[test]
fn dirty_embedded_doc_schedules_includer_tests() {
    // H1: docs/** used to be NonCodeCheap (fmt+clippy only).
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_fixture(root);
    write(root, "docs/contract.md", "v2\n");

    let v = dry_run_json(root, &[]);
    assert_eq!(v["scopeExecuted"], "fast", "{v}");
    assert!(v.get("fallbackReason").is_none(), "{v}");
    let cmd = nextest_command(&v).expect("embedded doc must schedule nextest");
    assert!(cmd.contains("test(contracts_document)"), "{cmd}");
}

#[test]
fn dirty_unreferenced_doc_keeps_cheap_fast_path() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_fixture(root);
    write(root, "docs/guide.md", "guide v2\n");

    let v = dry_run_json(root, &[]);
    assert_eq!(v["scopeExecuted"], "fast", "{v}");
    assert_eq!(v["refused"], false, "{v}");
    assert!(nextest_command(&v).is_none(), "{v}");
}

#[test]
fn embedded_plan_is_byte_identical_across_runs_and_from_subdirectory() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_fixture(root);
    write(root, "docs/contract.md", "v2\n");
    write(root, "templates/message.txt", "bye\n");

    let a = nextest_command(&dry_run_json(root, &[])).expect("nextest");
    let b = nextest_command(&dry_run_json(root, &[])).expect("nextest");
    assert_eq!(a, b);
    assert!(
        a.contains("test(contracts_document) + test(contracts_template)"),
        "{a}"
    );
    // `-C` is a global flag placed before the subcommand; cwd is a subdirectory.
    let out = spawn_verify(
        &root.join("src"),
        &[
            "-C",
            root.to_str().unwrap(),
            "verify",
            "--scope",
            "fast",
            "--json",
            "--dry-run",
        ],
    );
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("JSON");
    assert_eq!(a, nextest_command(&v).expect("nextest from subdirectory"));
}

#[test]
fn unreferenced_asset_with_dynamic_test_reads_refuses() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_fixture(root);
    write(
        root,
        "tests/dyn.rs",
        "#[test]\nfn d() { let p = std::env::var(\"X\").unwrap(); let _ = std::fs::read(p); }\n",
    );
    git_add_and_commit(root, "dyn");
    write(root, "assets/logo.png", "png\n");

    let v = dry_run_refusal_json(root, &[]);
    assert_eq!(v["scopeExecuted"], "refused", "{v}");
    assert_eq!(v["refused"], true, "{v}");
    assert!(
        v["fallbackReason"]
            .as_str()
            .unwrap_or("")
            .contains("tests/dyn.rs"),
        "{v}"
    );
}

#[test]
fn embedded_untestable_include_refuses_with_reason() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_fixture(root);
    write(
        root,
        "tests/common/mod.rs",
        "pub fn d() -> &'static str { include_str!(\"../../docs/shared.md\") }\n",
    );
    write(root, "docs/shared.md", "one\n");
    git_add_and_commit(root, "shared");
    write(root, "docs/shared.md", "two\n");

    let v = dry_run_refusal_json(root, &[]);
    assert_eq!(v["scopeExecuted"], "refused", "{v}");
    assert!(
        v["fallbackReason"]
            .as_str()
            .unwrap_or("")
            .contains("tests/common/mod.rs"),
        "{v}"
    );
}

/// Index the fixture so mixed (source + embedded) plans have a fresh mapping.
fn index_fixture(root: &Path) {
    let init = spawn_verify(root, &["init"]);
    assert!(
        init.status.success(),
        "{:?}",
        String::from_utf8_lossy(&init.stderr)
    );
    let idx = spawn_verify(root, &["index", "--incremental"]);
    assert!(
        idx.status.success(),
        "{:?}",
        String::from_utf8_lossy(&idx.stderr)
    );
}

fn init_mixed_fixture(root: &Path) {
    init_fixture(root);
    write(
        root,
        "src/lib.rs",
        "pub mod contracts;
pub mod alpha;
pub mod render;
",
    );
    write(
        root,
        "src/alpha.rs",
        "pub fn a() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    #[test]
    fn alpha_ok() {
        assert_eq!(super::a(), 1);
    }
}
",
    );
    write(
        root,
        "src/render.rs",
        "pub fn r() -> &'static str {
    include_str!(\"../templates/render.txt\")
}
",
    );
    write(
        root,
        "templates/render.txt",
        "r
",
    );
    git_add_and_commit(root, "mixed");
    index_fixture(root);
}

#[test]
fn mixed_source_plus_embedded_template_unions_both_stem_sets() {
    // H2: the template used to be dropped when a mapped source file also changed.
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_mixed_fixture(root);
    write(
        root,
        "src/alpha.rs",
        "pub fn a() -> u32 {
    2
}

#[cfg(test)]
mod tests {
    #[test]
    fn alpha_ok() {
        assert_eq!(super::a(), 2);
    }
}
",
    );
    write(
        root,
        "templates/message.txt",
        "bye
",
    );

    let v = dry_run_json(root, &["--auto-index"]);
    assert_eq!(v["refused"], false, "{v}");
    let cmd = nextest_command(&v).expect("nextest");
    assert!(cmd.contains("test(alpha_ok)"), "{cmd}");
    assert!(cmd.contains("test(contracts_template)"), "{cmd}");
}

#[test]
fn embedded_stems_do_not_mask_an_unmapped_source_change() {
    // P1-1: new unmapped source + embedded template must still refuse.
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_mixed_fixture(root);
    write(
        root,
        "src/new_mod.rs",
        "pub fn n() -> u32 {
    7
}
",
    );
    write(
        root,
        "templates/message.txt",
        "bye
",
    );

    let v = dry_run_refusal_json(root, &["--auto-index"]);
    assert_eq!(v["refused"], true, "{v}");
    assert!(
        v["fallbackReason"]
            .as_str()
            .unwrap_or("")
            .contains("test_mapping"),
        "{v}"
    );
}

#[test]
fn includer_without_mapped_tests_is_not_replaced_by_sibling_stems() {
    // P1-2: render.rs includes render.txt but has no tests; alpha is mapped.
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_mixed_fixture(root);
    write(
        root,
        "src/alpha.rs",
        "pub fn a() -> u32 {
    3
}

#[cfg(test)]
mod tests {
    #[test]
    fn alpha_ok() {
        assert_eq!(super::a(), 3);
    }
}
",
    );
    write(
        root,
        "templates/render.txt",
        "changed
",
    );

    let v = dry_run_refusal_json(root, &["--auto-index"]);
    assert_eq!(v["refused"], true, "{v}");
    assert!(
        v["fallbackReason"]
            .as_str()
            .unwrap_or("")
            .contains("render.rs"),
        "{v}"
    );
}

#[test]
fn non_rust_repo_keeps_legacy_material_definition() {
    // A dirty notes.txt next to app.py is material for the non-Rust scan.
    // Nothing names the file, so the plan stays zero-step and is not a refusal.
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write(
        root,
        "app.py",
        "print(1)
",
    );
    git_add_and_commit(root, "init");
    write(
        root,
        "notes.txt",
        "n
",
    );

    let v = dry_run_json(root, &[]);
    assert_eq!(v["cleanTree"], false, "{v}");
    assert_eq!(v["refused"], false, "{v}");
    assert!(
        v["steps"].as_array().is_some_and(|steps| steps.is_empty()),
        "{v}"
    );
}

#[rstest::rstest]
#[case("malformed_root")]
#[case("oversized_nested")]
#[case("bounded_walk")]
fn rust_detection_inconclusive_never_turns_dirty_material_into_green(#[case] kind: &str) {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    if kind == "malformed_root" {
        write(root, "Cargo.toml", "[package\nnot valid TOML\n");
    } else if kind == "oversized_nested" {
        write(root, "backend/Cargo.toml", &" ".repeat(600 * 1024));
    } else {
        for n in 0..2501 {
            write(root, &format!("aaa/{n:04}.rs"), "");
        }
        write(
            root,
            "zzz/Cargo.toml",
            "[package]\nname='fx'\nversion='0.1.0'\n",
        );
    }
    git_add_and_commit(root, "inconclusive detection baseline");
    write(root, "templates/message.txt", "dirty\n");
    let out = spawn_verify(root, &["verify", "--scope", "fast", "--dry-run", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(1), "{kind}: {value}");
    assert_eq!(value["cleanTree"], false, "{kind}: {value}");
    assert_eq!(value["refused"], true, "{kind}: {value}");
    assert!(
        value["fallbackReason"]
            .as_str()
            .unwrap()
            .contains("Rust detection inconclusive"),
        "{kind}: {value}"
    );
}

#[cfg(windows)]
#[test]
fn unreadable_nested_manifest_refuses_with_named_class() {
    use std::os::windows::fs::OpenOptionsExt;
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write(
        root,
        "backend/Cargo.toml",
        "[package]\nname='fx'\nversion='0.1.0'\n",
    );
    git_add_and_commit(root, "nested parent");
    let _locked = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(root.join("backend/Cargo.toml"))
        .unwrap();
    write(root, "message.txt", "dirty");
    let out = spawn_verify(root, &["verify", "--scope", "fast", "--dry-run", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(1), "{value}");
    assert_eq!(value["cleanTree"], false, "{value}");
    assert!(
        value["fallbackReason"]
            .as_str()
            .unwrap()
            .contains("unreadable"),
        "{value}"
    );
}

#[cfg(unix)]
#[test]
fn unreadable_nested_manifest_refuses_with_named_class() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write(
        root,
        "backend/Cargo.toml",
        "[package]\nname='fx'\nversion='0.1.0'\n",
    );
    git_add_and_commit(root, "nested parent");
    fs::set_permissions(
        root.join("backend/Cargo.toml"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    write(root, "message.txt", "dirty");
    let out = spawn_verify(root, &["verify", "--scope", "fast", "--dry-run", "--json"]);
    fs::set_permissions(
        root.join("backend/Cargo.toml"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(1), "{value}");
    assert_eq!(value["cleanTree"], false, "{value}");
    assert!(
        value["fallbackReason"]
            .as_str()
            .unwrap()
            .contains("unreadable"),
        "{value}"
    );
}

#[rstest::rstest]
#[case("docs/contract.md", "contracts_document")]
#[case("templates/message.txt", "contracts_template")]
fn nested_rust_repo_referenced_material_refuses_with_crate_guidance(
    #[case] path: &str,
    #[case] stem: &str,
) {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write(
        root,
        "backend/Cargo.toml",
        "[package]\nname=\"fx\"\nversion=\"0.1.0\"\n",
    );
    write(
        root,
        "backend/src/lib.rs",
        &ACCESSOR
            .replace("../docs", "../../docs")
            .replace("../templates", "../../templates"),
    );
    write(root, "docs/contract.md", "v1");
    write(root, "templates/message.txt", "hi");
    git_add_and_commit(root, "nested Rust");
    write(root, path, "mutation");
    let _ = stem;
    let out = spawn_verify(root, &["verify", "--scope", "fast", "--json", "--dry-run"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["refused"], true, "{v}");
    assert!(
        v["fallbackReason"]
            .as_str()
            .unwrap_or("")
            .contains("add a root workspace Cargo.toml"),
        "{v}"
    );
}

#[test]
fn nested_mixed_source_and_referenced_doc_refuses_instead_of_dropping_the_doc() {
    // 0549 P1 (round 6): a nested-only repo with a mapped-source sibling must
    // not silently drop the referenced document on the same route. Either the
    // includer's test is selected or the plan refuses; here the nested-only
    // policy refuses and names the remediation.
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write(
        root,
        "backend/Cargo.toml",
        "[package]\nname=\"fx\"\nversion=\"0.1.0\"\n",
    );
    write(
        root,
        "backend/src/lib.rs",
        "pub mod alpha;\npub mod render;\n",
    );
    write(
        root,
        "backend/src/render.rs",
        &ACCESSOR
            .replace("../docs", "../../docs")
            .replace("../templates", "../../templates"),
    );
    write(
        root,
        "backend/src/alpha.rs",
        "pub fn a() -> u32 { 1 }\n#[cfg(test)] mod tests { #[test] fn alpha_ok() {} }\n",
    );
    write(root, "docs/contract.md", "v1");
    write(root, "templates/message.txt", "hi");
    git_add_and_commit(root, "nested mixed");
    write(root, "backend/src/new_mod.rs", "pub fn n() -> u32 { 2 }\n");
    write(root, "docs/contract.md", "v2");
    let out = spawn_verify(root, &["verify", "--scope", "fast", "--json", "--dry-run"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["refused"], true, "{v}");
    assert_eq!(v["scopeExecuted"], "refused", "{v}");
    assert!(
        v["fallbackReason"]
            .as_str()
            .unwrap_or("")
            .contains("add a root workspace Cargo.toml"),
        "{v}"
    );
}

#[rstest::rstest]
#[case("data/x.sql")]
#[case("nested/Cargo.toml")]
fn mixed_legacy_material_refuses_and_names_path(#[case] path: &str) {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_mixed_fixture(root);
    write(
        root,
        "src/alpha.rs",
        "pub fn a() -> u32 { 2 }\n#[cfg(test)] mod tests { #[test] fn alpha_ok() {} }\n",
    );
    write(root, path, "changed");
    let out = spawn_verify(root, &["verify", "--scope", "fast", "--json", "--dry-run"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["refused"], true);
    assert_eq!(v["scopeExecuted"], "refused");
    assert!(
        v["fallbackReason"].as_str().unwrap_or("").contains(path),
        "{v}"
    );
}

#[test]
fn saved_packet_overlay_retains_template_selection() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_fixture(root);
    index_fixture(root);
    let layout = ledgerful::state::layout::Layout::new(camino::Utf8Path::from_path(root).unwrap());
    let storage = ledgerful::state::storage::StorageManager::init(
        layout.state_subdir().join("ledger.db").as_std_path(),
    )
    .unwrap();
    let packet = ledgerful::impact::packet::ImpactPacket {
        changes: vec![ledgerful::impact::packet::ChangedFile {
            path: "src/unrelated.rs".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    storage.save_packet(&packet).unwrap();
    write(root, "templates/message.txt", "mutated");
    let v = dry_run_json(root, &[]);
    assert_eq!(v["scopeExecuted"], "fast", "{v}");
    assert_eq!(v["refused"], false, "{v}");
    assert!(
        nextest_command(&v)
            .unwrap()
            .contains("test(contracts_template)")
    );
    assert_eq!(
        storage.get_latest_packet().unwrap().unwrap().changes[0]
            .path
            .to_string_lossy(),
        "src/unrelated.rs"
    );
}

#[rstest::rstest]
#[case("assets/logo.png")]
#[case("data/x.csv")]
fn benign_noncheap_exact_outcome(#[case] path: &str) {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_fixture(root);
    write(root, path, "benign");
    let out = spawn_verify(root, &["verify", "--scope", "fast", "--json", "--dry-run"]);
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["refused"], false);
    assert_eq!(v["scopeExecuted"], "fast");
    assert!(nextest_command(&v).is_none());
}

#[test]
fn polyglot_nested_rust_uses_strict_template_materiality() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write(
        root,
        "package.json",
        "{\"name\":\"polyglot\",\"version\":\"1.0.0\"}\n",
    );
    write(
        root,
        "backend/Cargo.toml",
        "[package]\nname='fx'\nversion='0.1.0'\n",
    );
    write(
        root,
        "backend/src/lib.rs",
        "#[test] fn template_value() { let _ = include_str!(\"../../templates/message.txt\"); }\n",
    );
    write(root, "templates/message.txt", "parent\n");
    git_add_and_commit(root, "polyglot parent");
    write(root, "templates/message.txt", "mutation\n");
    let out = spawn_verify(root, &["verify", "--scope", "fast", "--dry-run", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(1), "{value}");
    assert_eq!(value["cleanTree"], false, "{value}");
    assert_eq!(value["scopeExecuted"], "refused", "{value}");
    assert!(
        value["fallbackReason"]
            .as_str()
            .unwrap_or("")
            .contains("root workspace"),
        "{value}"
    );
}

#[rstest::rstest]
#[case("docs/guide.md", true)]
#[case("", false)]
fn malformed_root_manifest_refuses_nonempty_cheap_but_not_empty_tree(
    #[case] dirty_path: &str,
    #[case] refuses: bool,
) {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write(root, "Cargo.toml", "[package\nmalformed\n");
    write(root, "docs/guide.md", "parent\n");
    git_add_and_commit(root, "malformed root baseline");
    if !dirty_path.is_empty() {
        write(root, dirty_path, "mutation\n");
    }
    let out = spawn_verify(root, &["verify", "--scope", "fast", "--dry-run", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        out.status.code(),
        Some(if refuses { 1 } else { 0 }),
        "{value}"
    );
    assert_eq!(value["refused"], refuses, "{value}");
    assert_eq!(value["cleanTree"], !refuses, "{value}");
    if refuses {
        assert!(
            value["fallbackReason"]
                .as_str()
                .unwrap_or("")
                .contains("Rust detection inconclusive: malformed Cargo.toml"),
            "{value}"
        );
    }
}

#[cfg(unix)]
#[test]
fn unreadable_directory_does_not_trigger_discovery_for_cheap_or_full_scope() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    // Git ignores the inaccessible directory, but the pinned Rust walker
    // would enter it if a legacy-material doc edit incorrectly triggered it.
    write(root, ".gitignore", "blocked/\n");
    write(root, "docs/guide.md", "parent\n");
    git_add_and_commit(root, "cheap legacy baseline");
    write(
        root,
        "blocked/backend/Cargo.toml",
        "[package]\nname='fx'\nversion='0.1.0'\n",
    );
    let blocked = root.join("blocked");
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    write(root, "docs/guide.md", "mutation\n");
    // Restore permissions before assertions so failures cannot strand temp
    // directories or obscure the original command outcome during cleanup.
    let fast = spawn_verify(root, &["verify", "--scope", "fast", "--dry-run", "--json"]);
    let full = spawn_verify(root, &["verify", "--scope", "full", "--dry-run", "--json"]);
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    for (scope, out) in [("fast", fast), ("full", full)] {
        let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(out.status.code(), Some(0), "{scope}: {value}");
        assert_eq!(value["refused"], false, "{scope}: {value}");
        assert!(value.get("fallbackReason").is_none(), "{scope}: {value}");
    }
}

#[test]
fn literal_less_file_consumer_refuses_and_names_candidate_and_consumer() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_fixture(root);
    write(
        root,
        "tests/consumer.rs",
        "#[test] fn consumer() { let _ = include_dir!(directory_variable); }\n",
    );
    git_add_and_commit(root, "literal-less consumer parent");
    write(root, "assets/logo.png", "mutation\n");
    let value = dry_run_refusal_json(root, &[]);
    let reason = value["fallbackReason"].as_str().unwrap();
    assert!(reason.contains("assets/logo.png"), "{value}");
    assert!(reason.contains("include_dir"), "{value}");
    assert!(reason.contains("tests/consumer.rs"), "{value}");
}

#[rstest::rstest]
#[case("docs/example.txt")]
#[case("LICENSE")]
fn unknown_nested_manifest_keeps_actual_cheap_only_cli_plan(#[case] path: &str) {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    setup_git_repo(root);
    write(root, "backend/Cargo.toml", &" ".repeat(600 * 1024));
    write(root, path, "parent\n");
    git_add_and_commit(root, "oversized nested manifest with cheap parent");
    write(root, path, "mutation\n");
    let out = spawn_verify(root, &["verify", "--scope", "fast", "--dry-run", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "{value}");
    assert_eq!(value["cleanTree"], false, "{value}");
    assert_eq!(value["refused"], false, "{value}");
    assert_eq!(value["scopeExecuted"], "fast", "{value}");
    assert!(value.get("fallbackReason").is_none(), "{value}");
    assert!(nextest_command(&value).is_none(), "{value}");
}
