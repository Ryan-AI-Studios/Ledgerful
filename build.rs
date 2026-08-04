//! Build script: TARGET triple + fail-soft git short-SHA embed (0137).
//!
//! cwd is CARGO_MANIFEST_DIR by default — do not override for git.
//! Any git failure → LEDGERFUL_GIT_SHA=unknown (never panic the build).

fn main() {
    if let Ok(target) = std::env::var("TARGET") {
        println!("cargo:rustc-env=TARGET={target}");
    }

    // Primary: reflog updates on pull/commit/checkout/merge/rebase.
    // Secondary: branch switch (symbolic ref text). .git/HEAD alone does NOT
    // change on same-branch git pull.
    println!("cargo:rerun-if-changed=.git/logs/HEAD");
    println!("cargo:rerun-if-changed=.git/HEAD");

    let sha = git_short_sha().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=LEDGERFUL_GIT_SHA={sha}");

    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let version_long = if sha != "unknown" {
        format!("{pkg_version} ({sha})")
    } else {
        pkg_version
    };
    println!("cargo:rustc-env=LEDGERFUL_VERSION_LONG={version_long}");
}

/// Prefer 12-hex short SHA via `git rev-parse --short=12 HEAD`.
/// Returns None on any failure (missing git, non-repo, non-zero exit, empty).
fn git_short_sha() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?;
    let s = s.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}
