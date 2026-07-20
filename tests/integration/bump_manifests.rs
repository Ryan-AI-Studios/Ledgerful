//! Integration tests for scripts/bump-manifests.{ps1,sh}.
//!
//! Hermetic: uses committed fixtures under tests/fixtures/package-manifests/v0.1.8
//! (real published v0.1.8 hashes). Never downloads release archives.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_checksums() -> PathBuf {
    repo_root().join("tests/fixtures/package-manifests/v0.1.8")
}

fn packaging_dir() -> PathBuf {
    repo_root().join("packaging")
}

const HASH_WINDOWS: &str = "0f285af485ac979883ac03cb2a8fabb0d394041b2e34a8f25fe3558d858e5397";
const HASH_LINUX: &str = "0ecba8040149f351448362bad3ea3ec940a59cf9fc719b90b7d6f2ac2649341a";
const HASH_MAC_INTEL: &str = "34478cab0f4504e59083b6887dfab081df9a32607fbfc9a94352ba58b5a0300c";
const HASH_MAC_ARM: &str = "3d32d2c10ba77cc16a07fa291f2a4d5f25ba6374b13e191aec85656752269227";

/// Valid 64-hex placeholder that must be rewritten away by bump (not a real release hash).
const PLACEHOLDER_HASH: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

fn run_bump(
    out_dir: &Path,
    checksums_dir: &Path,
    packaging: &Path,
    version: &str,
) -> std::process::Output {
    if cfg!(windows) {
        Command::new("pwsh")
            .args([
                "-NoProfile",
                "-File",
                repo_root()
                    .join("scripts/bump-manifests.ps1")
                    .to_str()
                    .expect("utf-8 path"),
                "-Version",
                version,
                "-ChecksumsDir",
                checksums_dir.to_str().expect("utf-8 path"),
                "-PackagingDir",
                packaging.to_str().expect("utf-8 path"),
                "-OutDir",
                out_dir.to_str().expect("utf-8 path"),
            ])
            .output()
            .expect("pwsh should launch bump-manifests.ps1")
    } else {
        Command::new("bash")
            .args([
                repo_root()
                    .join("scripts/bump-manifests.sh")
                    .to_str()
                    .expect("utf-8 path"),
                "--version",
                version,
                "--checksums-dir",
                checksums_dir.to_str().expect("utf-8 path"),
                "--packaging-dir",
                packaging.to_str().expect("utf-8 path"),
                "--out-dir",
                out_dir.to_str().expect("utf-8 path"),
            ])
            .output()
            .expect("bash should launch bump-manifests.sh")
    }
}

fn assert_success(output: &std::process::Output, context: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{context} failed\nstatus: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status.code()
    );
}

/// Seed packaging templates that intentionally do **not** already match fixture
/// version/hashes, so bump must rewrite version, URLs, and sha256 fields.
fn seed_stale_packaging(dir: &Path, version: &str) {
    let hb = dir.join("homebrew");
    let sc = dir.join("scoop");
    fs::create_dir_all(&hb).expect("mkdir homebrew");
    fs::create_dir_all(&sc).expect("mkdir scoop");

    let formula = format!(
        r#"class Ledgerful < Formula
  desc "Local-first change intelligence CLI for impact analysis and verification"
  homepage "https://github.com/Ryan-AI-Studios/Ledgerful"
  version "{version}"
  license :cannot_represent

  on_macos do
    on_arm do
      url "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v{version}/ledgerful-aarch64-apple-darwin.tar.gz"
      sha256 "{hash}"
    end
    on_intel do
      url "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v{version}/ledgerful-x86_64-apple-darwin.tar.gz"
      sha256 "{hash}"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v{version}/ledgerful-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "{hash}"
    end
  end

  def install
    binary = Dir["ledgerful-*/ledgerful"].first
    odie "ledgerful binary not found in archive" if binary.nil?
    bin.install binary => "ledgerful"
  end
end
"#,
        version = version,
        hash = PLACEHOLDER_HASH
    );

    let scoop = format!(
        r#"{{
  "version": "{version}",
  "description": "Local-first change intelligence CLI for impact analysis and verification",
  "homepage": "https://github.com/Ryan-AI-Studios/Ledgerful",
  "architecture": {{
    "64bit": {{
      "url": "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v{version}/ledgerful-x86_64-pc-windows-msvc.zip",
      "hash": "{hash}"
    }}
  }},
  "bin": "ledgerful.exe"
}}
"#,
        version = version,
        hash = PLACEHOLDER_HASH
    );

    fs::write(hb.join("ledgerful.rb"), formula).expect("write stale formula");
    fs::write(sc.join("ledgerful.json"), scoop).expect("write stale scoop");
}

fn assert_formula_rewritten(formula: &str, version: &str) {
    assert!(
        formula.contains(&format!("version \"{version}\"")),
        "formula should set version {version}:\n{formula}"
    );
    assert!(
        !formula.contains(PLACEHOLDER_HASH),
        "formula must not retain placeholder hashes:\n{formula}"
    );
    assert!(
        formula.contains(HASH_MAC_ARM),
        "formula should contain aarch64-apple-darwin fixture hash"
    );
    assert!(
        formula.contains(HASH_MAC_INTEL),
        "formula should contain x86_64-apple-darwin fixture hash"
    );
    assert!(
        formula.contains(HASH_LINUX),
        "formula should contain linux fixture hash"
    );
    assert!(
        formula.contains(&format!(
            "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v{version}/ledgerful-aarch64-apple-darwin.tar.gz"
        )),
        "formula should use /v{version}/ download URLs:\n{formula}"
    );
    assert!(
        !formula.contains("/v0.0.1/"),
        "formula should not retain stale /v0.0.1/ URLs:\n{formula}"
    );
}

fn assert_scoop_rewritten(scoop: &str, version: &str) {
    assert!(
        scoop.contains(&format!("\"version\": \"{version}\"")),
        "scoop should set version {version}:\n{scoop}"
    );
    assert!(
        !scoop.contains(PLACEHOLDER_HASH),
        "scoop must not retain placeholder hash:\n{scoop}"
    );
    assert!(
        scoop.contains(HASH_WINDOWS),
        "scoop should contain windows zip fixture hash"
    );
    assert!(
        scoop.contains(&format!(
            "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v{version}/ledgerful-x86_64-pc-windows-msvc.zip"
        )),
        "scoop should use /v{version}/ windows zip URL:\n{scoop}"
    );
    assert!(
        !scoop.contains("/v0.0.1/"),
        "scoop should not retain stale /v0.0.1/ URLs:\n{scoop}"
    );
}

#[test]
fn bump_manifests_fixture_v018_writes_expected_hashes() {
    let tmp = tempdir().expect("tempdir");
    let out = tmp.path();

    let output = run_bump(out, &fixture_checksums(), &packaging_dir(), "0.1.8");
    assert_success(&output, "bump-manifests fixture v0.1.8");

    let formula = fs::read_to_string(out.join("homebrew/ledgerful.rb"))
        .expect("homebrew formula should be written");
    let scoop = fs::read_to_string(out.join("scoop/ledgerful.json"))
        .expect("scoop manifest should be written");

    assert!(
        formula.contains("version \"0.1.8\""),
        "formula should set version 0.1.8:\n{formula}"
    );
    assert!(
        formula.contains(HASH_MAC_ARM),
        "formula should contain aarch64-apple-darwin hash"
    );
    assert!(
        formula.contains(HASH_MAC_INTEL),
        "formula should contain x86_64-apple-darwin hash"
    );
    assert!(
        formula.contains(HASH_LINUX),
        "formula should contain linux hash"
    );
    assert!(
        formula.contains(
            "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.1.8/ledgerful-aarch64-apple-darwin.tar.gz"
        ),
        "formula should use v0.1.8 download URLs"
    );

    assert!(
        scoop.contains("\"version\": \"0.1.8\""),
        "scoop should set version 0.1.8:\n{scoop}"
    );
    assert!(
        scoop.contains(HASH_WINDOWS),
        "scoop should contain windows zip hash"
    );
    assert!(
        scoop.contains(
            "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.1.8/ledgerful-x86_64-pc-windows-msvc.zip"
        ),
        "scoop should use v0.1.8 windows zip URL"
    );
}

/// Prove rewrite path: templates start at 0.0.1 with placeholder hashes, not
/// already matching fixture 0.1.8 (so a no-op copy would fail assertions).
#[test]
fn bump_manifests_rewrites_stale_templates_to_fixture_v018() {
    let packaging = tempdir().expect("stale packaging tempdir");
    seed_stale_packaging(packaging.path(), "0.0.1");

    // Sanity: seed is intentionally wrong before bump.
    let seed_formula =
        fs::read_to_string(packaging.path().join("homebrew/ledgerful.rb")).expect("seed formula");
    assert!(seed_formula.contains("version \"0.0.1\""));
    assert!(seed_formula.contains(PLACEHOLDER_HASH));
    assert!(seed_formula.contains("/v0.0.1/"));
    assert!(!seed_formula.contains(HASH_MAC_ARM));

    let out = tempdir().expect("out tempdir");
    let output = run_bump(out.path(), &fixture_checksums(), packaging.path(), "0.1.8");
    assert_success(&output, "bump-manifests rewrite stale → 0.1.8");

    let formula = fs::read_to_string(out.path().join("homebrew/ledgerful.rb"))
        .expect("homebrew formula should be written");
    let scoop = fs::read_to_string(out.path().join("scoop/ledgerful.json"))
        .expect("scoop manifest should be written");

    assert_formula_rewritten(&formula, "0.1.8");
    assert_scoop_rewritten(&scoop, "0.1.8");
}

/// Same stale seed, bump to a non-fixture version: URLs follow the target
/// version tag while hashes still come from the fixture checksums dir.
#[test]
fn bump_manifests_rewrites_stale_templates_to_v999_with_fixture_hashes() {
    let packaging = tempdir().expect("stale packaging tempdir");
    seed_stale_packaging(packaging.path(), "0.0.1");

    let out = tempdir().expect("out tempdir");
    let output = run_bump(out.path(), &fixture_checksums(), packaging.path(), "9.9.9");
    assert_success(&output, "bump-manifests rewrite stale → 9.9.9");

    let formula = fs::read_to_string(out.path().join("homebrew/ledgerful.rb"))
        .expect("homebrew formula should be written");
    let scoop = fs::read_to_string(out.path().join("scoop/ledgerful.json"))
        .expect("scoop manifest should be written");

    assert_formula_rewritten(&formula, "9.9.9");
    assert_scoop_rewritten(&scoop, "9.9.9");
    assert!(
        !formula.contains("/v0.1.8/") && !scoop.contains("/v0.1.8/"),
        "target version 9.9.9 must not leave /v0.1.8/ URLs"
    );
}

#[test]
fn bump_manifests_accepts_v_prefix() {
    let tmp = tempdir().expect("tempdir");
    let out = tmp.path();

    let output = run_bump(out, &fixture_checksums(), &packaging_dir(), "v0.1.8");
    assert_success(&output, "bump-manifests with v-prefix");

    let formula = fs::read_to_string(out.join("homebrew/ledgerful.rb"))
        .expect("homebrew formula should be written");
    assert!(
        formula.contains("version \"0.1.8\""),
        "v-prefix should normalize to 0.1.8"
    );
}

#[test]
fn bump_manifests_fails_closed_on_missing_hash() {
    let incomplete = tempdir().expect("tempdir for incomplete checksums");
    let src = fixture_checksums();
    // Copy only three of four required .sha256 files
    for name in [
        "ledgerful-x86_64-pc-windows-msvc.zip.sha256",
        "ledgerful-x86_64-unknown-linux-gnu.tar.gz.sha256",
        "ledgerful-x86_64-apple-darwin.tar.gz.sha256",
        // intentionally omit aarch64-apple-darwin
    ] {
        fs::copy(src.join(name), incomplete.path().join(name)).expect("copy fixture sha256");
    }

    let out = tempdir().expect("out tempdir");
    let output = run_bump(out.path(), incomplete.path(), &packaging_dir(), "0.1.8");
    assert!(
        !output.status.success(),
        "bump must fail when a required hash is missing\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_lowercase();
    assert!(
        combined.contains("missing")
            || combined.contains("aarch64-apple-darwin")
            || combined.contains("checksum"),
        "error should mention missing checksum; got:\n{combined}"
    );
}
