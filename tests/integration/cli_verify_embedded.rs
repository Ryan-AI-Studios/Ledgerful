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

    let v = dry_run_json(root, &[]);
    assert_eq!(v["scopeExecuted"], "refused", "{v}");
    assert_eq!(v["refused"], true, "{v}");
    assert!(
        v["fallbackReason"]
            .as_str()
            .unwrap_or("")
            .contains("unresolved reads"),
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

    let v = dry_run_json(root, &[]);
    assert_eq!(v["scopeExecuted"], "refused", "{v}");
    assert!(
        v["fallbackReason"]
            .as_str()
            .unwrap_or("")
            .contains("no #[test]"),
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

    let v = dry_run_json(root, &["--auto-index"]);
    assert_eq!(v["refused"], true, "{v}");
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

    let v = dry_run_json(root, &["--auto-index"]);
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
    // D8 applies where the lookup runs; a dirty `.gitignore`/`notes.txt` in a
    // non-Rust repo stays a clean tree (non-Rust embedding is 0570).
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
    assert_eq!(v["cleanTree"], true, "{v}");
    assert_eq!(v["refused"], false, "{v}");
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
    let v = dry_run_json(root, &[]);
    assert_eq!(v["refused"], true, "{v}");
    let reason = v["fallbackReason"].as_str().unwrap_or("");
    assert!(reason.contains("only below the repo root"), "{v}");
    assert!(
        reason.contains("root workspace Cargo.toml") && !reason.contains("-C"),
        "remediation must not point at -C: {v}"
    );
    // A real (non-dry-run) refusal exits 1 before running anything.
    let real = spawn_verify(root, &["verify", "--scope", "fast"]);
    assert_eq!(real.status.code(), Some(1));
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
fn malformed_root_manifest_cannot_produce_empty_cheap_green() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_fixture(root);
    write(root, "Cargo.toml", "not valid toml {{{");
    git_add_and_commit(root, "malformed manifest fixture");
    write(root, "docs/guide.md", "changed guide");
    let v = dry_run_json(root, &[]);
    assert!(
        v["refused"] == true
            || v["steps"].as_array().is_some_and(|s| s
                .iter()
                .any(|step| step["command"].as_str().unwrap_or("").contains("cargo"))),
        "manifest presence must not disappear into a non-Rust empty plan: {v}"
    );
}
