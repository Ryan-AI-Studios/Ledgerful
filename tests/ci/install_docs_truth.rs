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
