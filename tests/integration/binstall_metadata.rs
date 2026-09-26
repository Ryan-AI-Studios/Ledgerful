//! Regression guard for cargo-binstall metadata (track 0051 DoD-4b / 0439).
//!
//! Live smoke (prebuilt download + `--version`) is recorded in the track review
//! log; this test keeps the committed metadata shape aligned with release assets
//! so a silent template drift fails CI.
//!
//! 0439: `[workspace.package]` already contains `repository = "https://…"`, so a
//! whole-file `contains` is a false pass. Root `[package]` must inherit the
//! four keys, and `cargo metadata` for `name == "ledgerful"` must be non-empty.

use serde_json::Value as JsonValue;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use toml::Value as TomlValue;

fn cargo_toml_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")
}

fn cargo_toml() -> String {
    let path = cargo_toml_path();
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read Cargo.toml: {e}"))
}

fn inherit_workspace_true(package: &TomlValue, key: &str) {
    let workspace = package
        .get(key)
        .and_then(|v| v.get("workspace"))
        .and_then(TomlValue::as_bool);
    assert_eq!(
        workspace,
        Some(true),
        "root [package].{key} must be `{key}.workspace = true` (0439); got {workspace:?}"
    );
}

fn non_empty_meta(pkg: &JsonValue, key: &str) {
    let value = pkg.get(key).and_then(JsonValue::as_str).unwrap_or("");
    assert!(
        !value.trim().is_empty(),
        "cargo metadata package ledgerful.{key} must be a non-empty string; got {value:?}"
    );
}

#[test]
#[allow(non_snake_case)]
fn binstall_metadata__present_with_expected_url_templates() {
    let toml = cargo_toml();

    assert!(
        toml.contains("[package.metadata.binstall]"),
        "missing [package.metadata.binstall] block"
    );
    assert!(
        toml.contains("repository = \"https://github.com/Ryan-AI-Studios/Ledgerful\""),
        "repository URL required for binstall {{ repo }} template"
    );

    // Unix default: nested tar.gz matching release.yml Package Unix
    assert!(
        toml.contains(
            "pkg-url = \"{ repo }/releases/download/v{ version }/{ name }-{ target }.tar.gz\""
        ),
        "unix pkg-url must match ledgerful-{{target}}.tar.gz release assets"
    );
    assert!(
        toml.contains("bin-dir = \"{ name }-{ target }/{ bin }{ binary-ext }\""),
        "unix bin-dir must be nested ledgerful-{{target}}/ledgerful"
    );
    assert!(
        toml.contains("pkg-fmt = \"tgz\""),
        "unix pkg-fmt must be tgz for .tar.gz"
    );
    assert!(
        toml.contains("disabled-strategies = [\"quick-install\"]"),
        "quick-install mirrors must stay disabled"
    );

    // Windows override: portable zip with binary at archive root
    assert!(
        toml.contains("[package.metadata.binstall.overrides.x86_64-pc-windows-msvc]"),
        "missing Windows binstall override"
    );
    assert!(
        toml.contains(
            "pkg-url = \"{ repo }/releases/download/v{ version }/{ name }-{ target }.zip\""
        ),
        "windows pkg-url must match ledgerful-{{target}}.zip release assets"
    );
    assert!(
        toml.contains("pkg-fmt = \"zip\""),
        "windows pkg-fmt must be zip"
    );
    // Root bin path (Compress-Archive of dist/asset/*)
    assert!(
        toml.lines()
            .any(|l| l.trim() == "bin-dir = \"{ bin }{ binary-ext }\""),
        "windows bin-dir must place binary at archive root"
    );
}

#[test]
#[allow(non_snake_case)]
fn root_package_inherits_workspace_identity_keys() {
    let parsed: TomlValue =
        toml::from_str(&cargo_toml()).unwrap_or_else(|e| panic!("parse Cargo.toml: {e}"));
    let package = parsed
        .get("package")
        .unwrap_or_else(|| panic!("missing [package]"));

    assert_eq!(
        package.get("name").and_then(TomlValue::as_str),
        Some("ledgerful"),
        "root package name is the engine fingerprint"
    );
    assert!(
        package.get("version").and_then(TomlValue::as_str).is_some(),
        "root [package].version must stay a literal string (worktree_package_version)"
    );
    assert!(
        package.get("edition").and_then(TomlValue::as_str).is_some(),
        "root [package].edition must stay a literal string"
    );

    inherit_workspace_true(package, "description");
    inherit_workspace_true(package, "homepage");
    inherit_workspace_true(package, "repository");
    inherit_workspace_true(package, "readme");
}

#[test]
#[allow(non_snake_case)]
fn cargo_metadata_root_ledgerful_identity_is_non_empty() {
    let manifest = cargo_toml_path();
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output()
        .unwrap_or_else(|e| panic!("spawn cargo metadata: {e}"));
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let meta: JsonValue = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("parse cargo metadata JSON: {e}"));
    let packages = meta
        .get("packages")
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("cargo metadata missing packages[]"));
    let pkg = packages
        .iter()
        .find(|p| p.get("name").and_then(JsonValue::as_str) == Some("ledgerful"))
        .unwrap_or_else(|| panic!("cargo metadata missing package name=ledgerful"));

    non_empty_meta(pkg, "description");
    non_empty_meta(pkg, "homepage");
    non_empty_meta(pkg, "repository");
    non_empty_meta(pkg, "readme");
}
