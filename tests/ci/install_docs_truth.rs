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

fn job_lines<'a>(source: &'a str, job: &str) -> Vec<&'a str> {
    let header = format!("  {job}:");
    let mut body = Vec::new();
    let mut in_job = false;
    for line in source.lines() {
        if !in_job {
            if line == header {
                in_job = true;
                body.push(line);
            }
            continue;
        }
        let next_job = line.starts_with("  ")
            && !line.starts_with("   ")
            && line.ends_with(':')
            && line.len() > 2
            && !line[2..].contains(' ');
        if next_job {
            break;
        }
        body.push(line);
    }
    body
}

const JOB_RUSTUP_TOOLCHAIN_LINE: &str = "      RUSTUP_TOOLCHAIN: stable";

fn contiguous_job_env_lines<'a>(source: &'a str, job: &str) -> Vec<&'a str> {
    let body = job_lines(source, job);
    if body.is_empty() {
        return Vec::new();
    }
    let Some(steps_at) = body.iter().position(|line| *line == "    steps:") else {
        return Vec::new();
    };
    let prefix = &body[..steps_at];
    let Some(env_at) = prefix.iter().position(|line| *line == "    env:") else {
        return Vec::new();
    };
    let mut kept = Vec::new();
    for line in &prefix[env_at + 1..] {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with("      ") {
            kept.push(*line);
            continue;
        }
        break;
    }
    kept
}

fn job_env_map_has_rustup_toolchain(source: &str, job: &str) -> bool {
    contiguous_job_env_lines(source, job).contains(&JOB_RUSTUP_TOOLCHAIN_LINE)
}

fn quoted_channel_token(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("channel = \"")?;
    rest.strip_suffix('"').filter(|token| !token.is_empty())
}

fn trimmed_line_count(source: &str, needle: &str) -> usize {
    source.lines().filter(|line| line.trim() == needle).count()
}

fn assert_job_level_rustup_toolchain(source: &str, file_label: &str, job: &str) {
    let body = job_lines(source, job);
    assert!(!body.is_empty(), "{file_label} is missing job {job}");
    let steps_at = body.iter().position(|line| *line == "    steps:");
    assert!(
        steps_at.is_some(),
        "{file_label} job {job} is missing a steps key"
    );
    let steps_at = steps_at.unwrap_or(body.len());
    let prefix = &body[..steps_at];
    let env_at = prefix.iter().position(|line| *line == "    env:");
    assert!(
        env_at.is_some(),
        "{file_label} job {job} is missing a job-level env map before steps"
    );
    let has_key = job_env_map_has_rustup_toolchain(source, job);
    assert!(
        has_key,
        "{file_label} job {job} is missing job-level RUSTUP_TOOLCHAIN: stable before steps"
    );
}

#[test]
fn release_and_smoke_jobs_set_rustup_toolchain_before_steps() {
    let release = include_str!("../../.github/workflows/release.yml");
    let smoke = include_str!("../../.github/workflows/smoke.yml");
    for job in ["build", "mcp-package", "sbom"] {
        assert_job_level_rustup_toolchain(release, "release.yml", job);
    }
    assert_job_level_rustup_toolchain(smoke, "smoke.yml", "install-smoke");
}

/// 0546: the Windows release exe must not import the MSVC runtime, because a clean
/// image (winget validation) lacks the VC++ Redistributable.
const WINDOWS_STATIC_CRT_RUSTFLAGS: &str = "-C target-feature=+crt-static";
const JOB_RUSTFLAGS_LINE: &str = "      RUSTFLAGS: ${{ matrix.rustflags }}";
const WINDOWS_CRT_GUARD_STEP: &str = "      - name: Assert Windows exe imports no MSVC runtime";

fn matrix_entry_lines<'a>(body: &[&'a str], asset: &str) -> Vec<&'a str> {
    let asset_line = format!("            asset: {asset}");
    let Some(asset_at) = body.iter().position(|line| *line == asset_line) else {
        return Vec::new();
    };
    let start = body[..asset_at]
        .iter()
        .rposition(|line| line.starts_with("          - "))
        .unwrap_or(0);
    let end = body[asset_at..]
        .iter()
        .position(|line| line.starts_with("          - ") || !line.starts_with("   "))
        .map_or(body.len(), |off| asset_at + off);
    body[start..end].to_vec()
}

#[test]
fn release_windows_leg_builds_with_static_crt_and_guards_imports() {
    let release = include_str!("../../.github/workflows/release.yml");
    let body = job_lines(release, "build");
    assert!(!body.is_empty(), "release.yml is missing job build");

    let windows = matrix_entry_lines(&body, "ledgerful-x86_64-pc-windows-msvc");
    let want_field = format!("            rustflags: \"{WINDOWS_STATIC_CRT_RUSTFLAGS}\"");
    assert!(
        windows.contains(&want_field.as_str()),
        "Windows matrix entry must set {want_field}"
    );
    for asset in [
        "ledgerful-x86_64-unknown-linux-gnu",
        "ledgerful-x86_64-apple-darwin",
        "ledgerful-aarch64-apple-darwin",
    ] {
        let entry = matrix_entry_lines(&body, asset);
        assert!(
            !entry.is_empty(),
            "release.yml is missing matrix entry {asset}"
        );
        assert!(
            !entry.iter().any(|line| line.contains("crt-static")),
            "{asset} must not get the Windows static CRT flag"
        );
    }

    let env_lines = contiguous_job_env_lines(release, "build");
    assert!(
        env_lines.contains(&JOB_RUSTFLAGS_LINE),
        "build job env must set RUSTFLAGS at job level so rust-cache keys on it"
    );
    assert!(
        env_lines.contains(&JOB_RUSTUP_TOOLCHAIN_LINE),
        "build job env must keep RUSTUP_TOOLCHAIN: stable contiguous with RUSTFLAGS"
    );

    let package_at = body
        .iter()
        .position(|line| *line == "      - name: Package Windows");
    let guard_at = body.iter().position(|line| *line == WINDOWS_CRT_GUARD_STEP);
    let attest_at = body
        .iter()
        .position(|line| *line == "      - name: Attest build provenance");
    match (package_at, guard_at, attest_at) {
        (Some(package), Some(guard), Some(attest)) => assert!(
            package < guard && guard < attest,
            "the CRT import guard must run after Package Windows and before attestation and upload"
        ),
        _ => panic!(
            "release.yml build job is missing Package Windows, the CRT guard, or attestation"
        ),
    }
}

#[test]
fn strategy_nested_rustup_toolchain_is_outside_the_job_env_map() {
    let source = [
        "  install-smoke:",
        "    env:",
        "      CARGO_NET_RETRY: 10",
        "    strategy:",
        "      RUSTUP_TOOLCHAIN: stable",
        "    steps:",
    ]
    .join("\n");
    let body = job_lines(&source, "install-smoke");
    assert!(
        !body.is_empty(),
        "strategy fixture is missing the job header"
    );
    assert!(
        source.contains(JOB_RUSTUP_TOOLCHAIN_LINE),
        "strategy fixture is missing the six-space rustup line"
    );
    let steps_at = body.iter().position(|line| *line == "    steps:");
    assert!(steps_at.is_some(), "strategy fixture is missing steps");
    let steps_at = steps_at.unwrap_or(body.len());
    let prefix = &body[..steps_at];
    let env_at = prefix.iter().position(|line| *line == "    env:");
    assert!(env_at.is_some(), "strategy fixture is missing job env");
    let env_at = env_at.unwrap_or(prefix.len().saturating_sub(1));
    assert!(
        prefix[env_at + 1..].contains(&JOB_RUSTUP_TOOLCHAIN_LINE),
        "pre-change scan must still see the strategy-nested rustup line"
    );
    let contiguous = contiguous_job_env_lines(&source, "install-smoke");
    assert!(
        contiguous.contains(&"      CARGO_NET_RETRY: 10"),
        "contiguous env slice dropped the job env key"
    );
    assert!(
        !contiguous.contains(&JOB_RUSTUP_TOOLCHAIN_LINE),
        "contiguous env slice kept the strategy-nested rustup line"
    );
    assert!(!job_env_map_has_rustup_toolchain(&source, "install-smoke"));
    assert!(!job_env_map_has_rustup_toolchain(
        "jobs:\n",
        "install-smoke"
    ));
    assert!(!job_env_map_has_rustup_toolchain(
        "  build:\n    env:\n      RUSTUP_TOOLCHAIN: stable\n",
        "build"
    ));
    assert!(!job_env_map_has_rustup_toolchain(
        "  build:\n    steps:\n",
        "build"
    ));
}

#[test]
fn step_nested_rustup_toolchain_is_outside_the_job_env_map() {
    let source = [
        "  build:",
        "    env:",
        "      CARGO_NET_RETRY: 10",
        "    steps:",
        "      RUSTUP_TOOLCHAIN: stable",
    ]
    .join("\n");
    let body = job_lines(&source, "build");
    assert!(!body.is_empty(), "step fixture is missing the job header");
    assert!(
        source.contains(JOB_RUSTUP_TOOLCHAIN_LINE),
        "step fixture is missing the six-space rustup line"
    );
    let contiguous = contiguous_job_env_lines(&source, "build");
    assert!(
        contiguous.contains(&"      CARGO_NET_RETRY: 10"),
        "contiguous env slice dropped the job env key"
    );
    assert!(
        !contiguous.contains(&JOB_RUSTUP_TOOLCHAIN_LINE),
        "contiguous env slice kept the step-nested rustup line"
    );
    assert!(!job_env_map_has_rustup_toolchain(&source, "build"));
}

#[test]
fn blank_line_inside_job_env_keeps_rustup_toolchain() {
    let source = [
        "  build:",
        "    env:",
        "      CARGO_NET_RETRY: 10",
        "",
        "      # comment",
        "      RUSTUP_TOOLCHAIN: stable",
        "    steps:",
    ]
    .join("\n");
    let body = job_lines(&source, "build");
    assert!(
        !body.is_empty(),
        "blank-line fixture is missing the job header"
    );
    assert!(
        source.contains(JOB_RUSTUP_TOOLCHAIN_LINE),
        "blank-line fixture is missing the six-space rustup line"
    );
    let contiguous = contiguous_job_env_lines(&source, "build");
    assert!(contiguous.contains(&"      CARGO_NET_RETRY: 10"));
    assert!(contiguous.contains(&"      # comment"));
    assert!(contiguous.contains(&JOB_RUSTUP_TOOLCHAIN_LINE));
    assert!(job_env_map_has_rustup_toolchain(&source, "build"));
}

#[test]
fn rust_toolchain_toml_pins_the_pr_path_channel() {
    let source = include_str!("../../rust-toolchain.toml");
    let lines: Vec<&str> = source.lines().collect();
    assert!(
        lines.contains(&"[toolchain]"),
        "rust-toolchain.toml is missing [toolchain]"
    );
    assert!(
        lines.contains(&"profile = \"minimal\""),
        "rust-toolchain.toml is missing profile = \"minimal\""
    );
    let channel_line = "channel = \"1.98.1\"";
    assert!(
        lines.contains(&channel_line),
        "rust-toolchain.toml is missing {channel_line}"
    );
    assert!(
        lines.iter().all(|line| !line.contains("components")),
        "rust-toolchain.toml must not list components"
    );
    assert!(
        lines.iter().all(|line| !line.contains("targets")),
        "rust-toolchain.toml must not list targets"
    );
    assert!(
        lines.iter().all(|line| !line.trim().starts_with("path")),
        "rust-toolchain.toml must not set path"
    );
    let bare = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("rust-toolchain");
    assert!(!bare.exists(), "bare rust-toolchain must stay absent");
    let token = lines.iter().find_map(|line| {
        if *line == channel_line {
            quoted_channel_token(line)
        } else {
            None
        }
    });
    assert!(token.is_some(), "channel line has no quoted token");
    let token = token.unwrap_or("");
    let needle = format!("toolchain: {token}");
    let stable = "toolchain: stable";
    let ci = include_str!("../../.github/workflows/ci.yml");
    let security = include_str!("../../.github/workflows/security.yml");
    let windows = include_str!("../../.github/workflows/windows-path-tests.yml");
    let release = include_str!("../../.github/workflows/release.yml");
    let smoke = include_str!("../../.github/workflows/smoke.yml");
    assert_eq!(trimmed_line_count(ci, &needle), 8, "{needle} in ci.yml");
    assert_eq!(
        trimmed_line_count(security, &needle),
        2,
        "{needle} in security.yml"
    );
    assert_eq!(
        trimmed_line_count(windows, &needle),
        1,
        "{needle} in windows-path-tests.yml"
    );
    assert_eq!(
        trimmed_line_count(ci, stable),
        0,
        "ci.yml toolchain: stable"
    );
    assert_eq!(
        trimmed_line_count(security, stable),
        0,
        "security.yml toolchain: stable"
    );
    assert_eq!(
        trimmed_line_count(windows, stable),
        0,
        "windows-path-tests.yml toolchain: stable"
    );
    assert_eq!(
        trimmed_line_count(release, &needle),
        0,
        "{needle} in release.yml"
    );
    assert_eq!(
        trimmed_line_count(release, stable),
        3,
        "release.yml toolchain: stable"
    );
    assert_eq!(
        trimmed_line_count(smoke, &needle),
        0,
        "{needle} in smoke.yml"
    );
    assert_eq!(
        trimmed_line_count(smoke, stable),
        1,
        "smoke.yml toolchain: stable"
    );
}

#[test]
fn deny_job_runs_cargo_audit_without_a_job_name() {
    let ci = include_str!("../../.github/workflows/ci.yml");
    let security = include_str!("../../.github/workflows/security.yml");
    let body = job_lines(ci, "deny");
    assert!(
        body.first().is_some_and(|line| *line == "  deny:"),
        "ci.yml is missing the deny job header"
    );
    let steps_at = body.iter().position(|line| *line == "    steps:");
    assert!(steps_at.is_some(), "deny job is missing a steps key");
    let steps_at = steps_at.unwrap_or(body.len());
    let job_name = body[..steps_at]
        .iter()
        .any(|line| *line == "    name:" || line.starts_with("    name:"));
    assert!(!job_name, "deny job has a job-level name before steps");
    let required = [
        "- run: cargo install cargo-deny --locked",
        "- run: cargo deny check",
        "- run: cargo install cargo-audit --locked",
        "- run: cargo audit",
    ];
    let missing: Vec<&str> = required
        .into_iter()
        .filter(|literal| !body.iter().any(|line| line.trim() == *literal))
        .collect();
    assert!(
        missing.is_empty(),
        "deny job is missing run lines:\n  {}",
        missing.join("\n  ")
    );
    let deny_at = body
        .iter()
        .position(|line| line.trim() == "- run: cargo deny check");
    let audit_at = body
        .iter()
        .position(|line| line.trim() == "- run: cargo audit");
    let deny_before_audit = deny_at.is_some_and(|deny| audit_at.is_some_and(|audit| deny < audit));
    assert!(
        deny_before_audit,
        "cargo deny check must appear before cargo audit"
    );
    assert!(
        security.contains("run: cargo audit"),
        "security.yml must keep run: cargo audit"
    );
}

#[test]
fn release_yml_keeps_all_features_and_names_embed_spa() {
    let source = include_str!("../../.github/workflows/release.yml");
    assert!(source.contains("--all-features"));
    assert!(source.contains("embed-spa"));
}

#[test]
fn smoke_yml_installs_without_building_the_spa() {
    let smoke = include_str!("../../.github/workflows/smoke.yml");
    let ci = include_str!("../../.github/workflows/ci.yml");
    let release = include_str!("../../.github/workflows/release.yml");
    let gate = include_str!("../../scripts/check-release-state.sh");
    let docs = include_str!("../../docs/package-distribution.md");

    let required = [
        "cargo install --path . --features mcp,web --locked --force",
        "ledgerful --version",
        "./scripts/smoke_mcp.sh",
        "./scripts/smoke_mcp.ps1",
        "ubuntu-latest",
        "macos-14",
        "macos-15",
        "windows-latest",
        "CARGO_HTTP_MULTIPLEXING",
        "CARGO_NET_RETRY",
        "3d3c42e5aac5ba805825da76410c181273ba90b1",
        "e2a55d2ffb04f378e9626c28d38b36d230d1e12f",
        "6323deb102c322ba6fcbdcafc7e3dddab59af2b6",
        "toolchain: stable",
    ];
    let missing: Vec<&str> = required
        .into_iter()
        .filter(|literal| !smoke.contains(literal))
        .collect();
    assert!(
        missing.is_empty(),
        "smoke.yml is missing install-smoke literals:\n  {}",
        missing.join("\n  ")
    );

    let forbidden = [
        "web-build",
        "ledgerful-web-spa",
        "ledgerful-frontend",
        "needs:",
        "npm run build",
        "npm ci",
        "embed-spa",
        "4a1df7e2",
        "actions/setup-node",
        "actions/upload-artifact",
        "actions/download-artifact",
    ];
    let present: Vec<&str> = forbidden
        .into_iter()
        .filter(|literal| smoke.contains(literal))
        .collect();
    assert!(
        present.is_empty(),
        "smoke.yml still contains forbidden literals:\n  {}",
        present.join("\n  ")
    );

    let ci_required = [
        "4a1df7e2be68bb1136a18c52f1ba420801858c89",
        "ledgerful-web-spa",
        "node-version: 22",
        "Windows-only (0087). smoke.yml does not build this frontend (0492); this pin stays on ci.yml.",
    ];
    let ci_missing: Vec<&str> = ci_required
        .into_iter()
        .filter(|literal| !ci.contains(literal))
        .collect();
    assert!(
        ci_missing.is_empty(),
        "ci.yml is missing smoke-boundary literals:\n  {}",
        ci_missing.join("\n  ")
    );
    assert!(
        !ci.contains("Keep identical to smoke.yml"),
        "ci.yml must not keep the old smoke.yml identity clause"
    );
    assert_eq!(ci.matches("needs: web-build").count(), 2);
    assert_eq!(ci.matches("needs: [clippy, web-build]").count(), 1);
    // Line-bounded so this does not also match `needs: [clippy, web-build]`.
    assert_eq!(ci.matches("needs: [clippy]\n").count(), 1);

    let release_required = ["ledgerful-web-spa", "--all-features", "node-version: 24"];
    let release_missing: Vec<&str> = release_required
        .into_iter()
        .filter(|literal| !release.contains(literal))
        .collect();
    assert!(
        release_missing.is_empty(),
        "release.yml is missing embed literals:\n  {}",
        release_missing.join("\n  ")
    );

    assert!(gate.contains("for wf in .github/workflows/ci.yml; do"));
    assert_eq!(
        gate.matches("smoke.yml does not check out the frontend")
            .count(),
        2
    );
    let gate_forbidden = [
        ".github/workflows/smoke.yml",
        "ci.yml / smoke.yml",
        "smoke.yml frontend",
    ];
    let gate_present: Vec<&str> = gate_forbidden
        .into_iter()
        .filter(|literal| gate.contains(literal))
        .collect();
    assert!(
        gate_present.is_empty(),
        "check-release-state.sh still scans smoke:\n  {}",
        gate_present.join("\n  ")
    );

    assert!(docs.contains("`smoke.yml` does not check out the frontend."));
    assert!(!docs.contains("`ci.yml` / `smoke.yml` frontend"));
}

#[test]
fn build_rs_keeps_git_sha_and_names_embed_spa_panic_literals() {
    let source = include_str!("../../build.rs");
    let remedy = format!(
        "Build ledgerful-frontend so out/index.html exists (npm run build) first, or drop embed-spa and use default features. Or {SOURCE_INSTALL_COMMAND}"
    );
    let required = [
        "LEDGERFUL_GIT_SHA",
        "LEDGERFUL_VERSION_LONG",
        "embed-spa",
        "ledgerful-frontend",
        "index.html",
        SOURCE_INSTALL_COMMAND,
        "CARGO_MANIFEST_DIR is unset ({err})",
        "Expected ../ledgerful-frontend/out/index.html",
        "is not a file (checked {})",
    ];
    let missing: Vec<&str> = required
        .into_iter()
        .filter(|literal| !source.contains(literal))
        .collect();
    assert!(
        missing.is_empty(),
        "build.rs is missing panic literals:\n  {}",
        missing.join("\n  ")
    );
    assert_eq!(
        source.matches(remedy.as_str()).count(),
        2,
        "shared embed-spa remedy must occur twice in build.rs"
    );
    assert_eq!(
        source
            .matches("CARGO_MANIFEST_DIR is unset ({err})")
            .count(),
        1
    );
    assert_eq!(
        source
            .matches("Expected ../ledgerful-frontend/out/index.html")
            .count(),
        1
    );
    assert_eq!(source.matches("is not a file (checked {})").count(), 1);
    assert!(
        !source.contains("--spa-dir"),
        "build.rs must not name --spa-dir"
    );
    assert!(
        !source.contains("Expected ledgerful-frontend out/index.html."),
        "build.rs must not keep the old unset prefix"
    );
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

const AUDIT_IGNORE_IDS: &[&str] = &[
    "RUSTSEC-2023-0089",
    "RUSTSEC-2025-0057",
    "RUSTSEC-2025-0052",
    "RUSTSEC-2025-0141",
    "RUSTSEC-2026-0249",
    "RUSTSEC-2024-0449",
];

const FORBIDDEN_AUDIT_IGNORE_IDS: &[&str] = &["RUSTSEC-2026-0253", "RUSTSEC-2026-0186"];

fn trimmed_source_lines(source: &str) -> Vec<&str> {
    source
        .lines()
        .map(|line| line.trim().trim_end_matches('\r'))
        .collect()
}

/// An ignore entry is only `"RUSTSEC-YYYY-NNNN",` with nothing else on the line.
fn quoted_rustsec_ignore_line(line: &str) -> Option<String> {
    let rest = line.strip_prefix("\"RUSTSEC-")?;
    let (year, rest) = rest.split_once('-')?;
    if year.len() != 4 || !year.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let (number, suffix) = rest.split_once('"')?;
    if number.len() != 4 || !number.bytes().all(|byte| byte.is_ascii_digit()) || suffix != "," {
        return None;
    }
    Some(format!("RUSTSEC-{year}-{number}"))
}

fn advisory_ignore_ids(source: &str) -> Result<Vec<String>, String> {
    let lines = trimmed_source_lines(source);
    let advisories: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| **line == "[advisories]")
        .map(|(index, _)| index)
        .collect();
    let [start] = advisories[..] else {
        return Err(format!(
            "expected exactly one [advisories], found {}",
            advisories.len()
        ));
    };
    let table_end = match lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, line)| line.starts_with('['))
    {
        Some((index, _)) => index,
        None => lines.len(),
    };
    let mut open_indexes = Vec::new();
    for (index, line) in lines.iter().enumerate().take(table_end).skip(start + 1) {
        if *line == "ignore = [" {
            open_indexes.push(index);
        }
    }
    let [open] = open_indexes[..] else {
        return Err(format!(
            "expected exactly one ignore = [ before the next table, found {}",
            open_indexes.len()
        ));
    };
    let mut ids = Vec::new();
    let mut closed = false;
    for line in lines.iter().skip(open + 1) {
        if *line == "]" {
            closed = true;
            break;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match quoted_rustsec_ignore_line(line) {
            Some(id) => ids.push(id),
            None => {
                return Err(format!(
                    "ignore entry is not an exact quoted advisory ID: {line}"
                ));
            }
        }
    }
    if !closed {
        return Err("ignore array is missing its closing bracket".to_string());
    }
    Ok(ids)
}

fn audit_toml_allowlist(source: &str) -> Result<(), String> {
    let mut advisories = 0_usize;
    let mut ignore_open = 0_usize;
    let mut close = 0_usize;
    for line in trimmed_source_lines(source) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[advisories]" {
            advisories += 1;
            continue;
        }
        if line == "ignore = [" {
            ignore_open += 1;
            continue;
        }
        if line == "]" {
            close += 1;
            continue;
        }
        if quoted_rustsec_ignore_line(line).is_none() {
            return Err(format!("disallowed line: {line}"));
        }
    }
    if advisories != 1 || ignore_open != 1 || close != 1 {
        return Err(format!(
            "expected one [advisories], one ignore = [, and one ], found {advisories}, {ignore_open}, {close}"
        ));
    }
    Ok(())
}

#[test]
fn audit_toml_ignore_matches_deny_advisories() {
    let deny = include_str!("../../deny.toml");
    let audit = include_str!("../../.cargo/audit.toml");
    let mut violations = Vec::new();
    let deny_ids = match advisory_ignore_ids(deny) {
        Ok(ids) => ids,
        Err(err) => {
            violations.push(format!("deny.toml: {err}"));
            Vec::new()
        }
    };
    let audit_ids = match advisory_ignore_ids(audit) {
        Ok(ids) => ids,
        Err(err) => {
            violations.push(format!(".cargo/audit.toml: {err}"));
            Vec::new()
        }
    };
    if let Err(err) = audit_toml_allowlist(audit) {
        violations.push(format!(".cargo/audit.toml allowlist: {err}"));
    }
    assert!(
        violations.is_empty(),
        "audit ignore parity:\n  {}",
        violations.join("\n  ")
    );
    assert_eq!(deny_ids.as_slice(), AUDIT_IGNORE_IDS);
    assert_eq!(audit_ids.as_slice(), AUDIT_IGNORE_IDS);
    for id in FORBIDDEN_AUDIT_IGNORE_IDS {
        assert!(
            !deny_ids.iter().any(|got| got == *id),
            "deny.toml ignores {id}"
        );
        assert!(
            !audit_ids.iter().any(|got| got == *id),
            ".cargo/audit.toml ignores {id}"
        );
    }
}
