//! Install docs-truth guard — fails if user-facing engine docs re-leak stale
//! package-manager availability language.
//!
//! Track 0068 DoD-3: brew + scoop are live; docs must not claim those channels
//! are still "coming" / "until those channels are live". Live one-liners in
//! `docs/installation.md` must match the marketing install page.

#![cfg(test)]

/// User-facing docs that must not re-advertise brew/scoop as unavailable.
const DOC_SOURCES: &[(&str, &str)] = &[
    ("README.md", include_str!("../../README.md")),
    (
        "docs/installation.md",
        include_str!("../../docs/installation.md"),
    ),
    ("SECURITY.md", include_str!("../../SECURITY.md")),
    (
        "docs/package-distribution.md",
        include_str!("../../docs/package-distribution.md"),
    ),
];

/// Live install one-liners that must remain in `docs/installation.md`
/// (byte-for-byte match with the web install page).
const REQUIRED_INSTALLATION_SNIPPETS: &[&str] = &[
    "brew install Ryan-AI-Studios/tap/ledgerful",
    "scoop bucket add ledgerful https://github.com/Ryan-AI-Studios/scoop-bucket",
    "scoop install ledgerful",
];

fn line_matches_homebrew_coming(line: &str) -> bool {
    // e.g. "### Homebrew / Scoop / winget (coming)"
    let lower = line.to_ascii_lowercase();
    lower.contains("homebrew") && lower.contains("(coming)")
}

fn line_matches_until_channels_live(line: &str) -> bool {
    line.to_ascii_lowercase()
        .contains("until those channels are live")
}

#[test]
fn no_stale_package_manager_availability_language() {
    let mut violations: Vec<String> = Vec::new();

    for (path, source) in DOC_SOURCES {
        for (idx, line) in source.lines().enumerate() {
            let line_no = idx + 1;
            if line_matches_homebrew_coming(line) {
                violations.push(format!(
                    "{path}:{line_no}: matches Homebrew.*(coming) — {line}"
                ));
            }
            if line_matches_until_channels_live(line) {
                violations.push(format!(
                    "{path}:{line_no}: matches \"until those channels are live\" — {line}"
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Stale package-manager availability language found in user-facing docs:\n  {}\n\
         Homebrew and Scoop are live; remove \"(coming)\" / \"until those channels are live\" \
         and use version-agnostic prose with real install commands.",
        violations.join("\n  ")
    );
}

#[test]
fn installation_md_has_live_brew_and_scoop_commands() {
    let source = include_str!("../../docs/installation.md");
    let mut missing: Vec<&str> = Vec::new();

    for snippet in REQUIRED_INSTALLATION_SNIPPETS {
        if !source.contains(snippet) {
            missing.push(snippet);
        }
    }

    assert!(
        missing.is_empty(),
        "docs/installation.md is missing required live install one-liners \
         (must match web install.ts):\n  {}",
        missing.join("\n  ")
    );
}

/// Locked Daily 5 fence literals (0436). Clap `before_help` names the door;
/// this block is the skill Edit ladder made clap-valid (`search` needs QUERY).
const DAILY_5_FENCE_LITERALS: &[&str] = &[
    "ledgerful doctor --json",
    "ledgerful change-context --json",
    "ledgerful ledger status --compact",
    "ledgerful search init --auto-index",
    "ledgerful verify --scope fast",
];

#[test]
fn readme_has_daily_5_front_door() {
    let source = include_str!("../../README.md");
    let heading = source
        .find("## Daily 5")
        .expect("README.md must have a ## Daily 5 heading");
    let after = &source[heading..];
    let fence_start = after
        .find("```")
        .expect("## Daily 5 must be followed by a fenced command block");
    let after_open = &after[fence_start + 3..];
    let nl = after_open
        .find('\n')
        .expect("Daily 5 opening fence must include a newline");
    let body_and_rest = &after_open[nl + 1..];
    let fence_end = body_and_rest
        .find("```")
        .expect("Daily 5 fenced block must close");
    let body = &body_and_rest[..fence_end];
    let mut missing: Vec<&str> = Vec::new();
    for lit in DAILY_5_FENCE_LITERALS {
        if !body.contains(lit) {
            missing.push(lit);
        }
    }
    assert!(
        missing.is_empty(),
        "Daily 5 fence is missing locked literals:\n  {}\nblock was:\n{body}",
        missing.join("\n  ")
    );
}

const SOURCE_INSTALL_COMMAND: &str = "cargo install --git https://github.com/Ryan-AI-Studios/Ledgerful --branch main --locked --bin ledgerful";
const CARGO_PATH_SENTENCE: &str = "Source build does not embed the dashboard. ledgerful web needs --spa-dir, or use a GitHub release asset.";

#[test]
fn installation_md_locks_source_install_and_embed_spa() {
    let source = include_str!("../../docs/installation.md");
    assert!(
        source.contains(SOURCE_INSTALL_COMMAND),
        "docs/installation.md must contain the locked git install command"
    );
    assert!(
        source.contains("does not embed the dashboard"),
        "docs/installation.md must say the source build does not embed the dashboard"
    );
    assert!(
        source.contains("--all-features") && source.contains("embed-spa"),
        "docs/installation.md must say release binaries use --all-features, which includes embed-spa"
    );
    assert!(
        source.contains("cargo install --path . --locked --features embed-spa"),
        "docs/installation.md must lock the checkout embed command"
    );
    let floor_start = source
        .find("<!-- lf-floor:I-57 -->")
        .expect("winget floor open marker");
    let floor_end = source
        .find("<!-- /lf-floor:I-57 -->")
        .expect("winget floor close marker");
    assert!(floor_end > floor_start);
}

#[test]
fn readme_points_default_path_install_at_installation_md() {
    let source = include_str!("../../README.md");
    assert!(source.contains("does not embed the dashboard"));
    assert!(source.contains("docs/installation.md"));
}

#[test]
fn installers_warn_on_cargo_path_and_do_not_pass_embed_spa() {
    for (path, source) in [
        (
            "install/install.sh",
            include_str!("../../install/install.sh"),
        ),
        (
            "install/install.ps1",
            include_str!("../../install/install.ps1"),
        ),
    ] {
        assert!(
            source.contains(CARGO_PATH_SENTENCE),
            "{path} must print the locked cargo-path sentence"
        );
        assert!(
            !source.contains("embed-spa"),
            "{path} must not pass embed-spa"
        );
    }
}

#[test]
fn release_yml_keeps_all_features_and_names_embed_spa() {
    let source = include_str!("../../.github/workflows/release.yml");
    assert!(source.contains("--all-features"));
    assert!(source.contains("embed-spa"));
}

#[test]
fn build_rs_keeps_git_sha_and_names_embed_spa_panic_literals() {
    let source = include_str!("../../build.rs");
    assert!(source.contains("LEDGERFUL_GIT_SHA"));
    assert!(source.contains("LEDGERFUL_VERSION_LONG"));
    for literal in [
        "embed-spa",
        "ledgerful-frontend",
        "index.html",
        "--spa-dir",
        SOURCE_INSTALL_COMMAND,
    ] {
        assert!(
            source.contains(literal),
            "build.rs is missing panic literal {literal}"
        );
    }
}

const WINGET_USES_PREFIX: &str = "vedantmgoyal9/winget-releaser@";
const OLD_WINGET_SHA: &str = "4ffc7888bffd451b357355dc214d43bb9f23917e";

fn sha_after_winget_uses(source: &str) -> &str {
    let Some(at) = source.find(WINGET_USES_PREFIX) else {
        return "";
    };
    let rest = &source[at + WINGET_USES_PREFIX.len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn package_distribution_winget_sha_matches_release_yml() {
    let docs = include_str!("../../docs/package-distribution.md");
    let release = include_str!("../../.github/workflows/release.yml");
    let doc_sha = sha_after_winget_uses(docs);
    let release_sha = sha_after_winget_uses(release);
    assert_eq!(doc_sha.len(), 40, "doc winget sha length");
    assert_eq!(release_sha.len(), 40, "release.yml winget sha length");
    assert!(
        doc_sha.chars().all(|c| c.is_ascii_hexdigit()),
        "doc winget sha is not ASCII hex"
    );
    assert!(
        release_sha.chars().all(|c| c.is_ascii_hexdigit()),
        "release.yml winget sha is not ASCII hex"
    );
    assert_eq!(doc_sha, release_sha);
    assert_ne!(doc_sha, OLD_WINGET_SHA);
    assert!(
        !docs.contains(OLD_WINGET_SHA),
        "old winget sha remains in package-distribution.md"
    );
    assert!(
        !release.contains(OLD_WINGET_SHA),
        "old winget sha remains in release.yml"
    );
}
