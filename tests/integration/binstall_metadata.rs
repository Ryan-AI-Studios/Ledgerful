//! Regression guard for cargo-binstall metadata (track 0051 DoD-4b).
//!
//! Live smoke (prebuilt download + `--version`) is recorded in the track review
//! log; this test keeps the committed metadata shape aligned with release assets
//! so a silent template drift fails CI.

use std::fs;
use std::path::PathBuf;

fn cargo_toml() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read Cargo.toml: {e}"))
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
        "repository field required for binstall {{ repo }} template"
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
