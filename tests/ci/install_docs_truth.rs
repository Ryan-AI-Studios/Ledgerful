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
