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

const CRT_MISSING_INVOCATION: &str = "exactly one `$imports = & $objdump -p $exe | Select-String \"DLL Name:\" | ForEach-Object { ($_ -replace '.*DLL Name:\\s*', '').Trim() }` assignment";
const CRT_MISSING_EMPTY_PARSE: &str =
    "the fail-closed `if (-not $imports) { throw \"No imports parsed` guard";
const CRT_MISSING_BANNED_FILTER: &str = "exactly one `$banned = $imports |` consumer carrying the pinned `^(VCRUNTIME140|...|api-ms-win-crt-)` alternation";
const CRT_MISSING_BANNED_THROW: &str =
    "the `if ($banned) { throw \"Windows exe still imports the MSVC runtime` guard";

/// The `-replace` operand that strips `Select-String`'s `MatchInfo` residue (the `path:N:` prefix
/// and leading spaces). The `^`-anchored banned alternation only matches a stripped import string,
/// so `-replace 'DLL Name:', ''` leaves every element prefixed and silently empties `$banned`.
const CRT_NORMALIZER_PATTERN: &str = ".*DLL Name:\\s*";

/// The banned alternation with its `^` anchor **and closing quote**. The closing quote is part of
/// the pin: `'...api-ms-win-crt-)$'` does not contain this literal, so an "exact match" end anchor
/// (which can never match a real `.dll` import) is red instead of quietly disarming `$banned`.
const CRT_BANNED_ALTERNATION: &str =
    "'^(VCRUNTIME140|MSVCP140|CONCRT140|VCOMP140|VCCORLIB140|ucrtbase|api-ms-win-crt-)'";

/// Requirements that cannot be satisfied by a string that merely names the tool.
///
/// PR #624 comment 4307427196: a bare `llvm-objdump` substring also matches the guard's
/// own path literal and "not found" throw, so a step that kept the throws and dropped (or
/// overwrote) the dump still passed. These requirements pin the dataflow instead: one
/// import-dump assignment with its parse field and normalizer **operand**, exactly one
/// `$banned` consumer carrying the **operand** of its `-match`, and both fail-closed throws
/// as line prefixes.
///
/// The pin is deliberately **one physical line per requirement**: a multi-line re-wrap goes
/// red by design (fail-closed), and the canonical `$var = ` spacing is byte-pinned
/// (`$imports=@(...)` is not assignment-shaped and is fenced in track 0567's spec rather than
/// counted as covered). Fixture C in `crt_guard_negative_fixtures` is the live `run: |` block
/// only; the live assertion feeds the whole step, headers included, and no requirement below
/// targets a step-level line.
fn crt_guard_missing(guard: &[&str]) -> Vec<&'static str> {
    let trimmed: Vec<&str> = guard.iter().map(|line| line.trim()).collect();

    // Exactly one `$imports =` assignment. A second one after the dump overwrites the real
    // result (the guard then filters a fake list) and is a false green, not a refactor.
    let assignments: Vec<&str> = trimmed
        .iter()
        .copied()
        .filter(|line| line.starts_with("$imports ="))
        .collect();
    let has_invocation = match assignments.as_slice() {
        [line] => {
            line.starts_with("$imports = & ")
                && line.contains(" -p $exe")
                && line.contains("Select-String")
                && line.contains("DLL Name:")
                && line.contains("-replace")
                && line.contains(CRT_NORMALIZER_PATTERN)
                && line.contains(".Trim()")
                && !line.contains("Out-Null")
        }
        _ => false,
    };

    let has_empty_parse = trimmed
        .iter()
        .any(|line| line.starts_with("if (-not $imports) { throw \"No imports parsed"));

    // Exactly one `$banned =` assignment, and that one consumes `$imports` through the pinned
    // `^`-anchored alternation. A later `$banned = @()` overwrites the filter while the seven
    // names stay on the original line, so the names assertion alone cannot see it.
    let banned_assignments: Vec<&str> = trimmed
        .iter()
        .copied()
        .filter(|line| line.starts_with("$banned ="))
        .collect();
    let has_banned_filter = match banned_assignments.as_slice() {
        [line] => {
            line.starts_with("$banned = $imports |")
                && line.contains("-match")
                && line.contains(CRT_BANNED_ALTERNATION)
        }
        _ => false,
    };
    let has_banned_throw = trimmed.iter().any(|line| {
        line.starts_with("if ($banned) { throw \"Windows exe still imports the MSVC runtime")
    });

    let mut missing = Vec::new();
    if !has_invocation {
        missing.push(CRT_MISSING_INVOCATION);
    }
    if !has_empty_parse {
        missing.push(CRT_MISSING_EMPTY_PARSE);
    }
    if !has_banned_filter {
        missing.push(CRT_MISSING_BANNED_FILTER);
    }
    if !has_banned_throw {
        missing.push(CRT_MISSING_BANNED_THROW);
    }
    missing
}

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

    // The `RUSTFLAGS: ${{ matrix.rustflags }}` literal is deliberate; adding `|| ''` as a safety
    // net for an undefined matrix key would need this assertion updated with it.
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

    // The guard body must stay load-bearing: a renamed-but-gutted step must fail this test.
    let guard = step_lines(&body, WINDOWS_CRT_GUARD_STEP);
    for needle in ["        if: matrix.archive == 'zip'", "        shell: pwsh"] {
        assert!(
            guard.iter().any(|line| line.contains(needle)),
            "the CRT guard step lost `{needle}`"
        );
    }
    let missing = crt_guard_missing(&guard);
    assert!(missing.is_empty(), "the CRT guard step lost: {missing:?}");
    let banned_line = guard
        .iter()
        .find(|line| line.contains("$banned ="))
        .copied()
        .unwrap_or_default();
    for name in [
        "VCRUNTIME140",
        "MSVCP140",
        "CONCRT140",
        "VCOMP140",
        "VCCORLIB140",
        "ucrtbase",
        "api-ms-win-crt-",
    ] {
        assert!(
            banned_line.contains(name),
            "the CRT guard no longer bans {name}: {banned_line}"
        );
    }

    // The installed tools are not shipped, so they must not inherit the static CRT flag.
    // install-action downloads release binaries and does not compile. A `cargo install`
    // in this step would inherit the job RUSTFLAGS and must still clear them.
    let install = step_lines(&body, "      - name: Install cargo-auditable + cargo-audit");
    let compiles = install.iter().any(|line| line.contains("cargo install"));
    if compiles {
        assert!(
            install.contains(&"          RUSTFLAGS: \"\""),
            "the cargo install step must reset the job-level RUSTFLAGS"
        );
    } else {
        assert!(
            install
                .iter()
                .any(|line| line.contains("taiki-e/install-action@")),
            "cargo-auditable and cargo-audit must be prebuilt binaries so they do not inherit the static CRT RUSTFLAGS"
        );
        assert!(
            install
                .iter()
                .any(|line| line.contains("tool: cargo-auditable,cargo-audit")),
            "the install step must request cargo-auditable and cargo-audit"
        );
    }
}

/// Negative fixtures for [`crt_guard_missing`] — the oracles that keep the Windows CRT guard
/// assertion behaviour-shaped instead of tool-name-shaped (track 0567, PR #624 comment
/// 4307427196).
///
/// Every fixture keeps the *other* correct lines (fixture **C** is the verbatim live
/// `release.yml:259-271` `run: |` block, indentation stripped because the helper trims), so each
/// exact `assert_eq!` stays a focused oracle: N1/N2/N6/N7/N8 fail only the invocation, N3/N4 only
/// the parse/throw control, N5/N9/N10 only the banned filter.
#[test]
fn crt_guard_negative_fixtures() {
    // C — verbatim live release.yml:259-271.
    let control: &[&str] = &[
        r##"$objdump = "C:/Program Files/LLVM/bin/llvm-objdump.exe""##,
        r##"if (-not (Test-Path $objdump)) {"##,
        r##"  $found = Get-Command llvm-objdump -ErrorAction SilentlyContinue"##,
        r##"  if (-not $found) { throw "llvm-objdump not found at $objdump or on PATH; the Windows runner image changed, so install LLVM or switch the guard to dumpbin /dependents" }"##,
        r##"  $objdump = $found.Source"##,
        r##"}"##,
        r##"& $objdump --version | Select-Object -First 2"##,
        r##"$exe = "dist/${{ matrix.asset }}/${{ matrix.binary }}""##,
        r##"$imports = & $objdump -p $exe | Select-String "DLL Name:" | ForEach-Object { ($_ -replace '.*DLL Name:\s*', '').Trim() } | Sort-Object -Unique"##,
        r##"if (-not $imports) { throw "No imports parsed from $exe; the guard would pass vacuously" }"##,
        r##"$imports | ForEach-Object { "import: $_" }"##,
        r##"$banned = $imports | Where-Object { $_ -match '^(VCRUNTIME140|MSVCP140|CONCRT140|VCOMP140|VCCORLIB140|ucrtbase|api-ms-win-crt-)' }"##,
        r##"if ($banned) { throw "Windows exe still imports the MSVC runtime: $($banned -join ', ')" }"##,
    ];

    // N1 — the #624 shape: stub the imports, keep the throws.
    let n1: &[&str] = &[
        r##"$objdump = "C:/Program Files/LLVM/bin/llvm-objdump.exe""##,
        r##"if (-not (Test-Path $objdump)) {"##,
        r##"  $found = Get-Command llvm-objdump -ErrorAction SilentlyContinue"##,
        r##"  if (-not $found) { throw "llvm-objdump not found at $objdump or on PATH; the Windows runner image changed, so install LLVM or switch the guard to dumpbin /dependents" }"##,
        r##"  $objdump = $found.Source"##,
        r##"}"##,
        r##"& $objdump --version | Select-Object -First 2"##,
        r##"$exe = "dist/${{ matrix.asset }}/${{ matrix.binary }}""##,
        r##"$imports = @("kernel32.dll", "ntdll.dll")"##,
        r##"if (-not $imports) { throw "No imports parsed from $exe; the guard would pass vacuously" }"##,
        r##"$imports | ForEach-Object { "import: $_" }"##,
        r##"$banned = $imports | Where-Object { $_ -match '^(VCRUNTIME140|MSVCP140|CONCRT140|VCOMP140|VCCORLIB140|ucrtbase|api-ms-win-crt-)' }"##,
        r##"if ($banned) { throw "Windows exe still imports the MSVC runtime: $($banned -join ', ')" }"##,
    ];

    // N2 — a real call to the real tool with the wrong flag.
    let n2: &[&str] = &[
        r##"$objdump = "C:/Program Files/LLVM/bin/llvm-objdump.exe""##,
        r##"if (-not (Test-Path $objdump)) {"##,
        r##"  $found = Get-Command llvm-objdump -ErrorAction SilentlyContinue"##,
        r##"  if (-not $found) { throw "llvm-objdump not found at $objdump or on PATH; the Windows runner image changed, so install LLVM or switch the guard to dumpbin /dependents" }"##,
        r##"  $objdump = $found.Source"##,
        r##"}"##,
        r##"& $objdump --version | Select-Object -First 2"##,
        r##"$exe = "dist/${{ matrix.asset }}/${{ matrix.binary }}""##,
        r##"$imports = & $objdump --version | Select-String "DLL Name:""##,
        r##"if (-not $imports) { throw "No imports parsed from $exe; the guard would pass vacuously" }"##,
        r##"$imports | ForEach-Object { "import: $_" }"##,
        r##"$banned = $imports | Where-Object { $_ -match '^(VCRUNTIME140|MSVCP140|CONCRT140|VCOMP140|VCCORLIB140|ucrtbase|api-ms-win-crt-)' }"##,
        r##"if ($banned) { throw "Windows exe still imports the MSVC runtime: $($banned -join ', ')" }"##,
    ];

    // N3 — the empty-parse guard downgraded to a non-failing write.
    let n3: &[&str] = &[
        r##"$objdump = "C:/Program Files/LLVM/bin/llvm-objdump.exe""##,
        r##"if (-not (Test-Path $objdump)) {"##,
        r##"  $found = Get-Command llvm-objdump -ErrorAction SilentlyContinue"##,
        r##"  if (-not $found) { throw "llvm-objdump not found at $objdump or on PATH; the Windows runner image changed, so install LLVM or switch the guard to dumpbin /dependents" }"##,
        r##"  $objdump = $found.Source"##,
        r##"}"##,
        r##"& $objdump --version | Select-Object -First 2"##,
        r##"$exe = "dist/${{ matrix.asset }}/${{ matrix.binary }}""##,
        r##"$imports = & $objdump -p $exe | Select-String "DLL Name:" | ForEach-Object { ($_ -replace '.*DLL Name:\s*', '').Trim() } | Sort-Object -Unique"##,
        r##"if (-not $imports) { Write-Host "No imports parsed from $exe; the guard would pass vacuously" }"##,
        r##"$imports | ForEach-Object { "import: $_" }"##,
        r##"$banned = $imports | Where-Object { $_ -match '^(VCRUNTIME140|MSVCP140|CONCRT140|VCOMP140|VCCORLIB140|ucrtbase|api-ms-win-crt-)' }"##,
        r##"if ($banned) { throw "Windows exe still imports the MSVC runtime: $($banned -join ', ')" }"##,
    ];

    // N4 — the banned-import guard downgraded to a non-failing write.
    let n4: &[&str] = &[
        r##"$objdump = "C:/Program Files/LLVM/bin/llvm-objdump.exe""##,
        r##"if (-not (Test-Path $objdump)) {"##,
        r##"  $found = Get-Command llvm-objdump -ErrorAction SilentlyContinue"##,
        r##"  if (-not $found) { throw "llvm-objdump not found at $objdump or on PATH; the Windows runner image changed, so install LLVM or switch the guard to dumpbin /dependents" }"##,
        r##"  $objdump = $found.Source"##,
        r##"}"##,
        r##"& $objdump --version | Select-Object -First 2"##,
        r##"$exe = "dist/${{ matrix.asset }}/${{ matrix.binary }}""##,
        r##"$imports = & $objdump -p $exe | Select-String "DLL Name:" | ForEach-Object { ($_ -replace '.*DLL Name:\s*', '').Trim() } | Sort-Object -Unique"##,
        r##"if (-not $imports) { throw "No imports parsed from $exe; the guard would pass vacuously" }"##,
        r##"$imports | ForEach-Object { "import: $_" }"##,
        r##"$banned = $imports | Where-Object { $_ -match '^(VCRUNTIME140|MSVCP140|CONCRT140|VCOMP140|VCCORLIB140|ucrtbase|api-ms-win-crt-)' }"##,
        r##"if ($banned) { Write-Host "Windows exe still imports the MSVC runtime: $($banned -join ', ')" }"##,
    ];

    // N5 — every banned name still on the line, consumer gone.
    let n5: &[&str] = &[
        r##"$objdump = "C:/Program Files/LLVM/bin/llvm-objdump.exe""##,
        r##"if (-not (Test-Path $objdump)) {"##,
        r##"  $found = Get-Command llvm-objdump -ErrorAction SilentlyContinue"##,
        r##"  if (-not $found) { throw "llvm-objdump not found at $objdump or on PATH; the Windows runner image changed, so install LLVM or switch the guard to dumpbin /dependents" }"##,
        r##"  $objdump = $found.Source"##,
        r##"}"##,
        r##"& $objdump --version | Select-Object -First 2"##,
        r##"$exe = "dist/${{ matrix.asset }}/${{ matrix.binary }}""##,
        r##"$imports = & $objdump -p $exe | Select-String "DLL Name:" | ForEach-Object { ($_ -replace '.*DLL Name:\s*', '').Trim() } | Sort-Object -Unique"##,
        r##"if (-not $imports) { throw "No imports parsed from $exe; the guard would pass vacuously" }"##,
        r##"$imports | ForEach-Object { "import: $_" }"##,
        r##"$banned = @() # VCRUNTIME140 MSVCP140 CONCRT140 VCOMP140 VCCORLIB140 ucrtbase api-ms-win-crt-"##,
        r##"if ($banned) { throw "Windows exe still imports the MSVC runtime: $($banned -join ', ')" }"##,
    ];

    // N6 — a second `$imports =` after the dump overwrites the real result.
    let n6: &[&str] = &[
        r##"$objdump = "C:/Program Files/LLVM/bin/llvm-objdump.exe""##,
        r##"if (-not (Test-Path $objdump)) {"##,
        r##"  $found = Get-Command llvm-objdump -ErrorAction SilentlyContinue"##,
        r##"  if (-not $found) { throw "llvm-objdump not found at $objdump or on PATH; the Windows runner image changed, so install LLVM or switch the guard to dumpbin /dependents" }"##,
        r##"  $objdump = $found.Source"##,
        r##"}"##,
        r##"& $objdump --version | Select-Object -First 2"##,
        r##"$exe = "dist/${{ matrix.asset }}/${{ matrix.binary }}""##,
        r##"$imports = & $objdump -p $exe | Select-String "DLL Name:" | ForEach-Object { ($_ -replace '.*DLL Name:\s*', '').Trim() } | Sort-Object -Unique"##,
        r##"$imports = @("kernel32.dll")"##,
        r##"if (-not $imports) { throw "No imports parsed from $exe; the guard would pass vacuously" }"##,
        r##"$imports | ForEach-Object { "import: $_" }"##,
        r##"$banned = $imports | Where-Object { $_ -match '^(VCRUNTIME140|MSVCP140|CONCRT140|VCOMP140|VCCORLIB140|ucrtbase|api-ms-win-crt-)' }"##,
        r##"if ($banned) { throw "Windows exe still imports the MSVC runtime: $($banned -join ', ')" }"##,
    ];

    // N7 — the normalizer segment dropped; the field filter survives.
    let n7: &[&str] = &[
        r##"$objdump = "C:/Program Files/LLVM/bin/llvm-objdump.exe""##,
        r##"if (-not (Test-Path $objdump)) {"##,
        r##"  $found = Get-Command llvm-objdump -ErrorAction SilentlyContinue"##,
        r##"  if (-not $found) { throw "llvm-objdump not found at $objdump or on PATH; the Windows runner image changed, so install LLVM or switch the guard to dumpbin /dependents" }"##,
        r##"  $objdump = $found.Source"##,
        r##"}"##,
        r##"& $objdump --version | Select-Object -First 2"##,
        r##"$exe = "dist/${{ matrix.asset }}/${{ matrix.binary }}""##,
        r##"$imports = & $objdump -p $exe | Select-String "DLL Name:" | Sort-Object -Unique"##,
        r##"if (-not $imports) { throw "No imports parsed from $exe; the guard would pass vacuously" }"##,
        r##"$imports | ForEach-Object { "import: $_" }"##,
        r##"$banned = $imports | Where-Object { $_ -match '^(VCRUNTIME140|MSVCP140|CONCRT140|VCOMP140|VCCORLIB140|ucrtbase|api-ms-win-crt-)' }"##,
        r##"if ($banned) { throw "Windows exe still imports the MSVC runtime: $($banned -join ', ')" }"##,
    ];

    // N8 — the `-replace` operand loosened: every keyword survives, the residue does not.
    let n8: &[&str] = &[
        r##"$objdump = "C:/Program Files/LLVM/bin/llvm-objdump.exe""##,
        r##"if (-not (Test-Path $objdump)) {"##,
        r##"  $found = Get-Command llvm-objdump -ErrorAction SilentlyContinue"##,
        r##"  if (-not $found) { throw "llvm-objdump not found at $objdump or on PATH; the Windows runner image changed, so install LLVM or switch the guard to dumpbin /dependents" }"##,
        r##"  $objdump = $found.Source"##,
        r##"}"##,
        r##"& $objdump --version | Select-Object -First 2"##,
        r##"$exe = "dist/${{ matrix.asset }}/${{ matrix.binary }}""##,
        r##"$imports = & $objdump -p $exe | Select-String "DLL Name:" | ForEach-Object { ($_ -replace 'DLL Name:', '').Trim() } | Sort-Object -Unique"##,
        r##"if (-not $imports) { throw "No imports parsed from $exe; the guard would pass vacuously" }"##,
        r##"$imports | ForEach-Object { "import: $_" }"##,
        r##"$banned = $imports | Where-Object { $_ -match '^(VCRUNTIME140|MSVCP140|CONCRT140|VCOMP140|VCCORLIB140|ucrtbase|api-ms-win-crt-)' }"##,
        r##"if ($banned) { throw "Windows exe still imports the MSVC runtime: $($banned -join ', ')" }"##,
    ];

    // N9 — the banned alternation end-anchored; all seven names intact.
    let n9: &[&str] = &[
        r##"$objdump = "C:/Program Files/LLVM/bin/llvm-objdump.exe""##,
        r##"if (-not (Test-Path $objdump)) {"##,
        r##"  $found = Get-Command llvm-objdump -ErrorAction SilentlyContinue"##,
        r##"  if (-not $found) { throw "llvm-objdump not found at $objdump or on PATH; the Windows runner image changed, so install LLVM or switch the guard to dumpbin /dependents" }"##,
        r##"  $objdump = $found.Source"##,
        r##"}"##,
        r##"& $objdump --version | Select-Object -First 2"##,
        r##"$exe = "dist/${{ matrix.asset }}/${{ matrix.binary }}""##,
        r##"$imports = & $objdump -p $exe | Select-String "DLL Name:" | ForEach-Object { ($_ -replace '.*DLL Name:\s*', '').Trim() } | Sort-Object -Unique"##,
        r##"if (-not $imports) { throw "No imports parsed from $exe; the guard would pass vacuously" }"##,
        r##"$imports | ForEach-Object { "import: $_" }"##,
        r##"$banned = $imports | Where-Object { $_ -match '^(VCRUNTIME140|MSVCP140|CONCRT140|VCOMP140|VCCORLIB140|ucrtbase|api-ms-win-crt-)$' }"##,
        r##"if ($banned) { throw "Windows exe still imports the MSVC runtime: $($banned -join ', ')" }"##,
    ];

    // N10 — `$banned` overwritten before its throw; the names stay on the filter line.
    let n10: &[&str] = &[
        r##"$objdump = "C:/Program Files/LLVM/bin/llvm-objdump.exe""##,
        r##"if (-not (Test-Path $objdump)) {"##,
        r##"  $found = Get-Command llvm-objdump -ErrorAction SilentlyContinue"##,
        r##"  if (-not $found) { throw "llvm-objdump not found at $objdump or on PATH; the Windows runner image changed, so install LLVM or switch the guard to dumpbin /dependents" }"##,
        r##"  $objdump = $found.Source"##,
        r##"}"##,
        r##"& $objdump --version | Select-Object -First 2"##,
        r##"$exe = "dist/${{ matrix.asset }}/${{ matrix.binary }}""##,
        r##"$imports = & $objdump -p $exe | Select-String "DLL Name:" | ForEach-Object { ($_ -replace '.*DLL Name:\s*', '').Trim() } | Sort-Object -Unique"##,
        r##"if (-not $imports) { throw "No imports parsed from $exe; the guard would pass vacuously" }"##,
        r##"$imports | ForEach-Object { "import: $_" }"##,
        r##"$banned = $imports | Where-Object { $_ -match '^(VCRUNTIME140|MSVCP140|CONCRT140|VCOMP140|VCCORLIB140|ucrtbase|api-ms-win-crt-)' }"##,
        r##"$banned = @()"##,
        r##"if ($banned) { throw "Windows exe still imports the MSVC runtime: $($banned -join ', ')" }"##,
    ];

    let missing = crt_guard_missing(control);
    assert!(missing.is_empty(), "live guard control: {missing:?}");

    // N0 — deterministic accumulation, in the helper's fixed order.
    assert_eq!(
        crt_guard_missing(&[]),
        vec![
            CRT_MISSING_INVOCATION,
            CRT_MISSING_EMPTY_PARSE,
            CRT_MISSING_BANNED_FILTER,
            CRT_MISSING_BANNED_THROW,
        ]
    );
    assert_eq!(crt_guard_missing(n1), vec![CRT_MISSING_INVOCATION]);
    assert_eq!(crt_guard_missing(n2), vec![CRT_MISSING_INVOCATION]);
    assert_eq!(crt_guard_missing(n3), vec![CRT_MISSING_EMPTY_PARSE]);
    assert_eq!(crt_guard_missing(n4), vec![CRT_MISSING_BANNED_THROW]);
    assert_eq!(crt_guard_missing(n5), vec![CRT_MISSING_BANNED_FILTER]);
    assert_eq!(crt_guard_missing(n6), vec![CRT_MISSING_INVOCATION]);
    assert_eq!(crt_guard_missing(n7), vec![CRT_MISSING_INVOCATION]);
    assert_eq!(crt_guard_missing(n8), vec![CRT_MISSING_INVOCATION]);
    assert_eq!(crt_guard_missing(n9), vec![CRT_MISSING_BANNED_FILTER]);
    assert_eq!(crt_guard_missing(n10), vec![CRT_MISSING_BANNED_FILTER]);

    // Pins fixture construction, not a claim that the pre-0567 needle would have caught a
    // version-only call (it would not; N2 documents that additional gap). The retained `:259`
    // path literal / `:262` "not found" throw must keep these four green under the old needle,
    // or they no longer reproduce the #624 false pass.
    let weak_needle = |guard: &[&str]| guard.iter().any(|line| line.contains("llvm-objdump"));
    assert!(
        weak_needle(n1) && weak_needle(n2) && weak_needle(n6) && weak_needle(n7),
        "fixtures must keep the pre-0567 needle green, or they no longer reproduce #624"
    );
}

/// Lines of the workflow step whose `- name:` line equals `header`, up to the next step.
fn step_lines<'a>(body: &[&'a str], header: &str) -> Vec<&'a str> {
    let Some(start) = body.iter().position(|line| *line == header) else {
        return Vec::new();
    };
    let end = body[start + 1..]
        .iter()
        .position(|line| line.starts_with("      - "))
        .map_or(body.len(), |off| start + 1 + off);
    body[start..end].to_vec()
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
    assert_eq!(trimmed_line_count(ci, &needle), 9, "{needle} in ci.yml");
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
        "tool: cargo-deny,cargo-audit",
        "run: cargo deny check",
        "run: cargo audit",
    ];
    let missing: Vec<&str> = required
        .into_iter()
        .filter(|literal| !body.iter().any(|line| line.trim() == *literal))
        .collect();
    assert!(
        missing.is_empty(),
        "deny job is missing install/run lines:\n  {}",
        missing.join("\n  ")
    );
    assert!(
        !body
            .iter()
            .any(|line| line.contains("cargo install cargo-deny")
                || line.contains("cargo install cargo-audit")),
        "deny job must not compile cargo-deny or cargo-audit"
    );
    let deny_at = body
        .iter()
        .position(|line| line.trim() == "run: cargo deny check");
    let audit_at = body
        .iter()
        .position(|line| line.trim() == "run: cargo audit");
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
    assert_eq!(ci.matches("needs: web-build").count(), 0);
    assert_eq!(ci.matches("needs: [clippy, web-build]").count(), 0);
    // Line-bounded so a future `needs: [clippy, …]` cannot hide inside a longer list.
    assert_eq!(ci.matches("needs: [clippy]\n").count(), 0);

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
    "RUSTSEC-2026-0381",
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

#[test]
fn webpki_roots_ignore_comment_matches_locked_wording() {
    let deny = include_str!("../../deny.toml");
    let audit = include_str!("../../.cargo/audit.toml");
    const EXPECTED: &[&str] = &[
        "# RUSTSEC-2026-0381 stays an advisory-id ignore. Neither ignore list can",
        "# limit that id to one crate or version. ureq 2.12.1 depends on",
        "# webpki-roots 0.26.11, which depends on webpki-roots 1.0.10. 0.26.11",
        "# has no trust store of its own; it re-exports that 1.x parent. The",
        "# advisory marks =0.26.11 unaffected and >=1.0.10 patched. cargo-deny",
        "# check advisories reports this id advisory-not-detected on this",
        "# lockfile. The sentence is cargo-deny's report. The id still covers",
        "# every later webpki-roots version that matches.",
        "# Remove this id when ureq no longer depends on webpki-roots 0.26.x (the ureq 3 migration).",
    ];
    const FORBIDDEN: &[&str] = &[
        "risk accepted",
        "no 0.26.x patch",
        "fix requires >=1.0.10",
        "direct rustls-webpki",
    ];

    for (label, source) in [("deny.toml", deny), (".cargo/audit.toml", audit)] {
        let lines = trimmed_source_lines(source);
        let id_indexes: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| **line == "\"RUSTSEC-2026-0381\",")
            .map(|(index, _)| index)
            .collect();
        assert_eq!(
            id_indexes.len(),
            1,
            "{label} should contain one quoted RUSTSEC-2026-0381 line, found {}",
            id_indexes.len()
        );
        let id_index = id_indexes[0];
        let mut comment_start = id_index;
        while comment_start > 0 && lines[comment_start - 1].starts_with('#') {
            comment_start -= 1;
        }
        assert_eq!(
            &lines[comment_start..id_index],
            EXPECTED,
            "{label} comment above the quoted id"
        );
        for phrase in FORBIDDEN {
            assert!(
                !source.contains(phrase),
                "{label} still contains `{phrase}`"
            );
        }
    }
}

// ---- 0568: advisory CI diagnostics ----
//
// `search-perf` and `mutation` both carry `continue-on-error: true`, so a failure never
// blocks a merge. These fixtures pin the *conditions* that name the failure class, the copy
// that class is allowed to claim, the advisory policy that has to survive them, and the
// rust-cache comment. They are static: no probe, dispatch or induced failure runs them.

/// A step outcome as the Actions `steps` context reports it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum StepOutcome {
    Success,
    Failure,
    Skipped,
    Cancelled,
}

const STEP_OUTCOMES: [StepOutcome; 4] = [
    StepOutcome::Success,
    StepOutcome::Failure,
    StepOutcome::Skipped,
    StepOutcome::Cancelled,
];

/// The job-level `failure()` / `cancelled()` status plus the observable step outcomes.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct AdvisoryCtx {
    failure: bool,
    cancelled: bool,
    build: StepOutcome,
    gate: StepOutcome,
    mutants: StepOutcome,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum SearchPerfWarning {
    Setup,
    Build,
    Gate,
    Cancelled,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum MutationWarning {
    Setup,
    Run,
    Cancelled,
}

/// Step name, pinned `if:` literal, and the class that condition selects (spec 0568 §3.1).
const SEARCH_PERF_WARNINGS: &[(&str, &str, SearchPerfWarning)] = &[
    (
        "Warn if search-perf setup failed",
        "failure() && steps.build.outcome == 'skipped' && steps.gate.outcome == 'skipped'",
        SearchPerfWarning::Setup,
    ),
    (
        "Warn if search-perf build failed or timed out",
        "failure() && steps.build.outcome == 'failure'",
        SearchPerfWarning::Build,
    ),
    (
        "Warn if search-perf gate failed or timed out",
        "failure() && steps.gate.outcome == 'failure'",
        SearchPerfWarning::Gate,
    ),
    (
        "Warn if search-perf was cancelled",
        "cancelled() && !failure()",
        SearchPerfWarning::Cancelled,
    ),
];

/// Step name, pinned `if:` literal, and the class that condition selects (spec 0568 §3.2).
const MUTATION_WARNINGS: &[(&str, &str, MutationWarning)] = &[
    (
        "Warn if mutation setup failed",
        "failure() && steps.mutants.outcome == 'skipped'",
        MutationWarning::Setup,
    ),
    (
        "Warn if mutation run failed or timed out",
        "failure() && steps.mutants.outcome == 'failure'",
        MutationWarning::Run,
    ),
    (
        "Warn if mutation was cancelled",
        "cancelled() && !failure()",
        MutationWarning::Cancelled,
    ),
];

/// The exact body text each `search-perf` warning step may emit, once after `::warning::`
/// and once on the job summary.
const SEARCH_PERF_WARNING_COPY: &[(&str, &str)] = &[
    (
        "Warn if search-perf setup failed",
        "search-perf setup failed before the build or gate ran (checkout, artifact download, toolchain or cache restore; non-blocking). No perf result is claimed.",
    ),
    (
        "Warn if search-perf build failed or timed out",
        "search-perf build failed or timed out (45-minute step budget; non-blocking). If this step ran its full budget it is infrastructure, not a perf regression; otherwise read the step log.",
    ),
    (
        "Warn if search-perf gate failed or timed out",
        "search-perf gate failed or timed out (10-minute step budget; non-blocking). A gate that completed inside its budget is a perf or correctness result, see the search-perf-samples artifact. A gate that ran its full budget is infrastructure.",
    ),
    (
        "Warn if search-perf was cancelled",
        "search-perf was cancelled before it finished (non-blocking). A cancelled run produces no perf result, so no regression is claimed.",
    ),
];

/// The exact body text each `mutation` warning step may emit.
const MUTATION_WARNING_COPY: &[(&str, &str)] = &[
    (
        "Warn if mutation setup failed",
        "mutation setup failed before cargo mutants ran (checkout, toolchain or tool install; non-blocking). No mutation result is claimed.",
    ),
    (
        "Warn if mutation run failed or timed out",
        "mutation run failed or timed out (120-minute step budget; non-blocking). A run that used its full budget is infrastructure; otherwise the mutants-report artifact records the outcome (surviving mutants, test timeouts, a failing baseline or an error).",
    ),
    (
        "Warn if mutation was cancelled",
        "mutation was cancelled before it finished (non-blocking). A cancelled run produces no mutation result.",
    ),
];

/// The 0568 replacement for the two rust-cache comment lines (spec §3.3).
const CACHE_COMMENT_LINES: &[&str] = &[
    "          # Save dependency artifacts even when the build fails or times out.",
    "          # A pull_request cache is scoped to that PR's ref, so it warms re-runs of",
    "          # the same PR only; other PRs restore main's push/schedule cache. rust-cache",
    "          # prunes workspace crates, so what a failed build saves is dependencies.",
];

const RUST_CACHE_PIN: &str = "6323deb102c322ba6fcbdcafc7e3dddab59af2b6";

fn advisory_step_if(body: &[&str], header: &str) -> Option<String> {
    step_lines(body, header)
        .iter()
        .find_map(|line| line.strip_prefix("        if: "))
        .map(str::to_string)
}

fn advisory_step_outcome(id: &str, ctx: &AdvisoryCtx) -> StepOutcome {
    match id {
        "build" => ctx.build,
        "gate" => ctx.gate,
        "mutants" => ctx.mutants,
        other => panic!("unknown steps context id `{other}`"),
    }
}

fn advisory_eval_atom(atom: &str, ctx: &AdvisoryCtx, expr: &str) -> bool {
    match atom {
        "failure()" => return ctx.failure,
        "cancelled()" => return ctx.cancelled,
        "!failure()" => return !ctx.failure,
        "!cancelled()" => return !ctx.cancelled,
        _ => {}
    }
    let Some(rest) = atom.strip_prefix("steps.") else {
        panic!("unhandled advisory condition atom `{atom}` in `{expr}`");
    };
    let Some((id, rest)) = rest.split_once(".outcome") else {
        panic!("unhandled advisory condition atom `{atom}` in `{expr}`");
    };
    let rest = rest.trim();
    let (want_equal, value) = if let Some(value) = rest.strip_prefix("==") {
        (true, value)
    } else if let Some(value) = rest.strip_prefix("!=") {
        (false, value)
    } else {
        panic!("unhandled advisory condition operator in `{expr}`");
    };
    let value = value
        .trim()
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
        .unwrap_or_else(|| panic!("unquoted outcome value in `{expr}`"));
    let equal = match value {
        "success" => advisory_step_outcome(id, ctx) == StepOutcome::Success,
        "failure" => advisory_step_outcome(id, ctx) == StepOutcome::Failure,
        "skipped" => advisory_step_outcome(id, ctx) == StepOutcome::Skipped,
        "cancelled" => advisory_step_outcome(id, ctx) == StepOutcome::Cancelled,
        other => panic!("unknown step outcome `{other}` in `{expr}`"),
    };
    if want_equal { equal } else { !equal }
}

/// Evaluate the restricted grammar these warnings use: `&&`-joined atoms over `failure()`,
/// `cancelled()`, their negations, and `steps.<id>.outcome ==|!= '<value>'`. Anything else
/// panics. The runner's implicit `success()` is modelled: an expression with no status
/// function is ANDed with `!failure() && !cancelled()`.
fn advisory_eval(expr: &str, ctx: &AdvisoryCtx) -> bool {
    let has_status_function = ["success()", "failure()", "cancelled()", "always()"]
        .iter()
        .any(|needle| expr.contains(needle));
    let inner = expr
        .split(" && ")
        .all(|atom| advisory_eval_atom(atom.trim(), ctx, expr));
    if has_status_function {
        inner
    } else {
        !ctx.failure && !ctx.cancelled && inner
    }
}

fn advisory_selected<W: Copy>(
    table: &[(&str, &str, W)],
    exprs: &[String],
    ctx: &AdvisoryCtx,
) -> Vec<W> {
    table
        .iter()
        .zip(exprs)
        .filter(|(_, expr)| advisory_eval(expr.as_str(), ctx))
        .map(|((_, _, warning), _)| *warning)
        .collect()
}

// Sequential reachability for `search-perf`. Neither `build` nor `gate` carries an `if:`,
// so a skipped `build` skips the gate, a non-skipped gate implies a successful build, and a
// `failure`/`cancelled` outcome can only exist when the matching status function is true.
fn advisory_search_perf_reachable(ctx: &AdvisoryCtx) -> bool {
    if ctx.failure && ctx.cancelled {
        return false;
    }
    if ctx.build == StepOutcome::Skipped && ctx.gate != StepOutcome::Skipped {
        return false;
    }
    if ctx.gate == StepOutcome::Skipped
        && !matches!(
            ctx.build,
            StepOutcome::Skipped | StepOutcome::Failure | StepOutcome::Cancelled
        )
    {
        return false;
    }
    if ctx.gate != StepOutcome::Skipped && ctx.build != StepOutcome::Success {
        return false;
    }
    if ctx.build == StepOutcome::Cancelled && !(ctx.cancelled && !ctx.failure) {
        return false;
    }
    if ctx.gate == StepOutcome::Cancelled && !(ctx.cancelled && !ctx.failure) {
        return false;
    }
    if ctx.build == StepOutcome::Failure && !ctx.failure {
        return false;
    }
    if ctx.gate == StepOutcome::Failure && !ctx.failure {
        return false;
    }
    if !ctx.failure
        && !ctx.cancelled
        && (ctx.build != StepOutcome::Success || ctx.gate != StepOutcome::Success)
    {
        return false;
    }
    true
}

// The mutation job has one outcome-bearing step, so its 16 tuples are 7 reachable rows plus
// 9 unreachable ones. `failure && cancelled` is unreachable: both functions read one job status.
fn advisory_mutation_reachable(ctx: &AdvisoryCtx) -> bool {
    if ctx.failure && ctx.cancelled {
        return false;
    }
    if ctx.mutants == StepOutcome::Skipped && !(ctx.failure || ctx.cancelled) {
        return false;
    }
    if ctx.mutants == StepOutcome::Cancelled && !(ctx.cancelled && !ctx.failure) {
        return false;
    }
    if ctx.mutants == StepOutcome::Failure && !ctx.failure {
        return false;
    }
    if !ctx.failure && !ctx.cancelled && ctx.mutants != StepOutcome::Success {
        return false;
    }
    true
}

fn advisory_search_perf_states() -> Vec<AdvisoryCtx> {
    let mut states = Vec::new();
    for failure in [false, true] {
        for cancelled in [false, true] {
            for build in STEP_OUTCOMES {
                for gate in STEP_OUTCOMES {
                    states.push(AdvisoryCtx {
                        failure,
                        cancelled,
                        build,
                        gate,
                        mutants: StepOutcome::Success,
                    });
                }
            }
        }
    }
    states
}

fn advisory_mutation_states() -> Vec<AdvisoryCtx> {
    let mut states = Vec::new();
    for failure in [false, true] {
        for cancelled in [false, true] {
            for mutants in STEP_OUTCOMES {
                states.push(AdvisoryCtx {
                    failure,
                    cancelled,
                    build: StepOutcome::Success,
                    gate: StepOutcome::Success,
                    mutants,
                });
            }
        }
    }
    states
}

/// The 9 sequentially reachable `search-perf` traces, written out (never derived from the
/// predicate) so the predicate can neither widen nor narrow silently.
fn advisory_search_perf_pinned_reachable() -> Vec<AdvisoryCtx> {
    let rows: [(bool, bool, StepOutcome, StepOutcome); 9] = [
        (true, false, StepOutcome::Skipped, StepOutcome::Skipped),
        (false, true, StepOutcome::Skipped, StepOutcome::Skipped),
        (true, false, StepOutcome::Failure, StepOutcome::Skipped),
        (false, true, StepOutcome::Cancelled, StepOutcome::Skipped),
        (true, false, StepOutcome::Success, StepOutcome::Failure),
        (false, true, StepOutcome::Success, StepOutcome::Cancelled),
        (false, false, StepOutcome::Success, StepOutcome::Success),
        (true, false, StepOutcome::Success, StepOutcome::Success),
        (false, true, StepOutcome::Success, StepOutcome::Success),
    ];
    rows.into_iter()
        .map(|(failure, cancelled, build, gate)| AdvisoryCtx {
            failure,
            cancelled,
            build,
            gate,
            mutants: StepOutcome::Success,
        })
        .collect()
}

/// The 7 sequentially reachable mutation traces.
fn advisory_mutation_pinned_reachable() -> Vec<AdvisoryCtx> {
    let rows: [(bool, bool, StepOutcome); 7] = [
        (false, false, StepOutcome::Success),
        (true, false, StepOutcome::Success),
        (true, false, StepOutcome::Failure),
        (true, false, StepOutcome::Skipped),
        (false, true, StepOutcome::Success),
        (false, true, StepOutcome::Skipped),
        (false, true, StepOutcome::Cancelled),
    ];
    rows.into_iter()
        .map(|(failure, cancelled, mutants)| AdvisoryCtx {
            failure,
            cancelled,
            build: StepOutcome::Success,
            gate: StepOutcome::Success,
            mutants,
        })
        .collect()
}

/// The expected selection per reachable `search-perf` trace (spec §3.4 table). Written as an
/// independent `match`; it never calls `advisory_eval`.
fn advisory_search_perf_expected(ctx: &AdvisoryCtx) -> Vec<SearchPerfWarning> {
    match (ctx.build, ctx.gate, ctx.failure, ctx.cancelled) {
        (StepOutcome::Skipped, StepOutcome::Skipped, true, false) => vec![SearchPerfWarning::Setup],
        (StepOutcome::Skipped, StepOutcome::Skipped, false, true) => {
            vec![SearchPerfWarning::Cancelled]
        }
        (StepOutcome::Failure, StepOutcome::Skipped, true, false) => vec![SearchPerfWarning::Build],
        (StepOutcome::Cancelled, StepOutcome::Skipped, false, true) => {
            vec![SearchPerfWarning::Cancelled]
        }
        (StepOutcome::Success, StepOutcome::Failure, true, false) => vec![SearchPerfWarning::Gate],
        (StepOutcome::Success, StepOutcome::Cancelled, false, true) => {
            vec![SearchPerfWarning::Cancelled]
        }
        (StepOutcome::Success, StepOutcome::Success, false, false) => Vec::new(),
        (StepOutcome::Success, StepOutcome::Success, true, false) => Vec::new(),
        (StepOutcome::Success, StepOutcome::Success, false, true) => {
            vec![SearchPerfWarning::Cancelled]
        }
        other => panic!("unmapped reachable search-perf state {other:?}"),
    }
}

/// The expected selection per reachable mutation trace (spec §3.4 table).
fn advisory_mutation_expected(ctx: &AdvisoryCtx) -> Vec<MutationWarning> {
    match (ctx.mutants, ctx.failure, ctx.cancelled) {
        (StepOutcome::Skipped, true, false) => vec![MutationWarning::Setup],
        (StepOutcome::Skipped, false, true) => vec![MutationWarning::Cancelled],
        (StepOutcome::Failure, true, false) => vec![MutationWarning::Run],
        (StepOutcome::Cancelled, false, true) => vec![MutationWarning::Cancelled],
        (StepOutcome::Success, false, false) => Vec::new(),
        (StepOutcome::Success, true, false) => Vec::new(),
        (StepOutcome::Success, false, true) => vec![MutationWarning::Cancelled],
        other => panic!("unmapped reachable mutation state {other:?}"),
    }
}

fn advisory_pinned_expressions<W>(
    body: &[&str],
    table: &[(&'static str, &'static str, W)],
    job: &str,
) -> (Vec<String>, Vec<&'static str>) {
    let mut exprs = Vec::new();
    let mut missing = Vec::new();
    for (name, want, _) in table {
        let header = format!("      - name: {name}");
        match advisory_step_if(body, &header) {
            Some(got) => {
                assert_eq!(&got, want, "{job} step `{name}` condition drifted");
                assert!(
                    got.contains("failure()") || got.contains("cancelled()"),
                    "{job} step `{name}` has no failure()/cancelled() status function: {got}"
                );
                exprs.push(got);
            }
            None => missing.push(*name),
        }
    }
    (exprs, missing)
}

fn advisory_assert_step_timeouts(body: &[&str], job: &str, floor: usize) {
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in body {
        if line.starts_with("      - ") {
            if let Some(block) = current.take() {
                blocks.push(block);
            }
            current = Some(vec![*line]);
        } else if let Some(block) = current.as_mut() {
            block.push(*line);
        }
    }
    if let Some(block) = current {
        blocks.push(block);
    }
    let steps = blocks.len();
    assert!(
        steps >= floor,
        "{job} step scan found {steps} steps; floor is {floor}, so an empty scan cannot pass vacuously"
    );
    for (index, block) in blocks.iter().enumerate() {
        let header = block.first().copied().unwrap_or_default();
        assert!(
            block
                .iter()
                .any(|line| line.starts_with("        timeout-minutes: ")),
            "{job} step {} has no step-level timeout-minutes: {header}",
            index + 1
        );
    }
}

fn advisory_assert_copy(
    body: &[&str],
    table: &[(&str, &str)],
    job: &str,
    expected_warnings: usize,
) {
    let joined = body.join("\n");
    for (name, text) in table {
        let header = format!("      - name: {name}");
        let step = step_lines(body, &header);
        assert!(!step.is_empty(), "{job} is missing warning step `{name}`");
        let step = step.join("\n");
        assert_eq!(
            step.matches(text).count(),
            2,
            "{job} step `{name}` must print its text once as a ::warning:: and once on the step summary"
        );
        assert_eq!(
            step.matches(&format!("::warning::{text}")).count(),
            1,
            "{job} step `{name}` must emit exactly one annotated ::warning::"
        );
        assert_eq!(
            step.matches(&format!("{text}\" >> \"$GITHUB_STEP_SUMMARY\""))
                .count(),
            1,
            "{job} step `{name}` must write the same text to $GITHUB_STEP_SUMMARY"
        );
        assert_eq!(
            step.matches("::warning::").count(),
            1,
            "{job} step `{name}` must carry exactly one ::warning:: line"
        );
    }
    assert_eq!(
        joined.matches("::warning::").count(),
        expected_warnings,
        "{job} warning set must stay closed at {expected_warnings} literal ::warning:: lines"
    );
    assert_eq!(
        joined.matches("$GITHUB_STEP_SUMMARY").count(),
        expected_warnings,
        "{job} must carry one summary line per warning"
    );
    assert_eq!(
        body.iter()
            .filter(|line| line.starts_with("      - name: Warn if "))
            .count(),
        expected_warnings,
        "{job} must carry exactly {expected_warnings} warning steps"
    );
}

#[test]
fn advisory_jobs_keep_advisory_policy() {
    let ci = include_str!("../../.github/workflows/ci.yml");
    for job in ["search-perf", "mutation"] {
        let body = job_lines(ci, job);
        let header = format!("  {job}:");
        assert_eq!(
            body.first().copied(),
            Some(header.as_str()),
            "ci.yml is missing job {job}"
        );
        assert!(
            body.contains(&"    continue-on-error: true"),
            "{job} must stay advisory (continue-on-error: true)"
        );
        let steps_at = body
            .iter()
            .position(|line| *line == "    steps:")
            .unwrap_or_else(|| panic!("{job} is missing a steps key"));
        let prefix = &body[..steps_at];
        assert!(
            !prefix
                .iter()
                .any(|line| line.starts_with("    timeout-minutes:")),
            "{job} must not gain a job-level timeout-minutes (0440)"
        );
        assert!(
            !prefix.iter().any(|line| line.starts_with("    name:")),
            "{job} must not gain a job-level name"
        );
        assert!(
            !body
                .iter()
                .any(|line| line.starts_with("        continue-on-error:")),
            "{job} must not set step-level continue-on-error, so outcome stays equal to conclusion"
        );
    }
    let search = job_lines(ci, "search-perf");
    assert!(
        !search
            .iter()
            .any(|line| line.trim_start().starts_with("needs:")),
        "search-perf must not wait on clippy or web-build"
    );
    assert!(
        search.contains(&"    if: github.event_name == 'schedule'"),
        "search-perf must run only on schedule"
    );
    assert!(
        search.contains(&"    runs-on: windows-latest"),
        "search-perf must keep runs-on: windows-latest"
    );
    let mutation = job_lines(ci, "mutation");
    assert!(
        mutation.contains(&"    if: github.event_name == 'schedule'"),
        "mutation must stay schedule-only"
    );
    assert!(
        mutation.contains(&"    runs-on: ubuntu-latest"),
        "mutation must keep runs-on: ubuntu-latest"
    );
}

#[test]
fn advisory_jobs_bind_every_step_to_a_timeout() {
    let ci = include_str!("../../.github/workflows/ci.yml");
    for (job, floor) in [("search-perf", 9_usize), ("mutation", 7_usize)] {
        let body = job_lines(ci, job);
        assert!(!body.is_empty(), "ci.yml is missing job {job}");
        advisory_assert_step_timeouts(&body, job, floor);
    }
}

#[test]
fn advisory_warning_conditions_partition_the_failure_states() {
    let ci = include_str!("../../.github/workflows/ci.yml");
    let search_body = job_lines(ci, "search-perf");
    let mutation_body = job_lines(ci, "mutation");
    assert!(!search_body.is_empty(), "ci.yml is missing job search-perf");
    assert!(!mutation_body.is_empty(), "ci.yml is missing job mutation");

    // 0. The evaluator must model the runner's implicit `success()` and refuse anything
    //    outside the restricted grammar, or the table below proves nothing.
    let failed = AdvisoryCtx {
        failure: true,
        cancelled: false,
        build: StepOutcome::Success,
        gate: StepOutcome::Success,
        mutants: StepOutcome::Success,
    };
    assert!(
        !advisory_eval("steps.build.outcome == 'success'", &failed),
        "a condition without a status function must inherit the runner's implicit success()"
    );
    assert!(
        advisory_eval("failure() && steps.build.outcome == 'success'", &failed),
        "an explicit failure() must satisfy the status slot"
    );
    assert!(
        !advisory_eval("cancelled() && !failure()", &failed),
        "cancelled() must be false on a failed-but-not-cancelled job"
    );
    let foreign = std::panic::catch_unwind(|| {
        advisory_eval(
            "failure() && github.event_name == 'schedule'",
            &AdvisoryCtx {
                failure: true,
                cancelled: false,
                build: StepOutcome::Success,
                gate: StepOutcome::Success,
                mutants: StepOutcome::Success,
            },
        )
    });
    assert!(
        foreign.is_err(),
        "the evaluator must panic on an atom outside the restricted grammar"
    );

    // 1. Pin every warning `if:` literal, and prove each carries a status function
    //    *before* the enumeration so a missing one cannot be masked.
    let (search_exprs, missing) =
        advisory_pinned_expressions(&search_body, SEARCH_PERF_WARNINGS, "search-perf");
    assert!(
        missing.is_empty(),
        "search-perf is missing warning steps: {missing:?}"
    );
    let (mutation_exprs, missing) =
        advisory_pinned_expressions(&mutation_body, MUTATION_WARNINGS, "mutation");
    assert!(
        missing.is_empty(),
        "mutation is missing warning steps: {missing:?}"
    );

    // 2. Reachability: the predicate and the pinned list must agree exactly.
    let search_states = advisory_search_perf_states();
    assert_eq!(
        search_states.len(),
        64,
        "search-perf state space is 2*2*4*4"
    );
    let mut search_reachable: Vec<AdvisoryCtx> = search_states
        .iter()
        .copied()
        .filter(advisory_search_perf_reachable)
        .collect();
    search_reachable.sort_unstable();
    let mut search_pinned = advisory_search_perf_pinned_reachable();
    search_pinned.sort_unstable();
    assert_eq!(
        search_reachable, search_pinned,
        "the reachability predicate and the pinned 9 search-perf states disagree"
    );
    assert_eq!(
        search_states.len() - search_reachable.len(),
        55,
        "the 55 unreachable search-perf tuples stay classified, not asserted away"
    );

    let mutation_states = advisory_mutation_states();
    assert_eq!(mutation_states.len(), 16, "mutation state space is 2*2*4");
    let mut mutation_reachable: Vec<AdvisoryCtx> = mutation_states
        .iter()
        .copied()
        .filter(advisory_mutation_reachable)
        .collect();
    mutation_reachable.sort_unstable();
    let mut mutation_pinned = advisory_mutation_pinned_reachable();
    mutation_pinned.sort_unstable();
    assert_eq!(
        mutation_reachable, mutation_pinned,
        "the reachability predicate and the pinned 7 mutation states disagree"
    );
    assert_eq!(
        mutation_states.len() - mutation_reachable.len(),
        9,
        "the 9 unreachable mutation tuples stay classified, not asserted away"
    );

    // 3. Total mapping: every reachable state selects exactly its expected set, and every
    //    non-empty expectation is exactly one warning (that is the disjointness claim).
    let mut search_warned = 0_usize;
    for ctx in &search_reachable {
        let expected = advisory_search_perf_expected(ctx);
        let selected = advisory_selected(SEARCH_PERF_WARNINGS, &search_exprs, ctx);
        assert_eq!(
            selected, expected,
            "search-perf state {ctx:?} selects the wrong warning set"
        );
        if expected.is_empty() {
            assert!(
                selected.is_empty(),
                "search-perf state {ctx:?} must write no warning"
            );
        } else {
            assert_eq!(
                selected.len(),
                1,
                "search-perf state {ctx:?} must select exactly one bounded warning"
            );
            search_warned += 1;
        }
    }
    assert_eq!(
        search_warned, 7,
        "7 of the 9 reachable search-perf states carry a warning"
    );

    let mut mutation_warned = 0_usize;
    for ctx in &mutation_reachable {
        let expected = advisory_mutation_expected(ctx);
        let selected = advisory_selected(MUTATION_WARNINGS, &mutation_exprs, ctx);
        assert_eq!(
            selected, expected,
            "mutation state {ctx:?} selects the wrong warning set"
        );
        if expected.is_empty() {
            assert!(
                selected.is_empty(),
                "mutation state {ctx:?} must write no warning"
            );
        } else {
            assert_eq!(
                selected.len(),
                1,
                "mutation state {ctx:?} must select exactly one bounded warning"
            );
            mutation_warned += 1;
        }
    }
    assert_eq!(
        mutation_warned, 5,
        "5 of the 7 reachable mutation states carry a warning"
    );

    // 4. The states the claim is scoped away from, stated instead of omitted.
    let green = AdvisoryCtx {
        failure: false,
        cancelled: false,
        build: StepOutcome::Success,
        gate: StepOutcome::Success,
        mutants: StepOutcome::Success,
    };
    assert!(
        !advisory_eval("failure() && steps.build.outcome == 'skipped'", &green)
            && advisory_selected(SEARCH_PERF_WARNINGS, &search_exprs, &green).is_empty(),
        "a green search-perf run must produce zero warnings"
    );
    assert!(
        advisory_selected(SEARCH_PERF_WARNINGS, &search_exprs, &failed).is_empty(),
        "a failure after the last warning step (artifact upload) must produce zero warnings"
    );

    // `(build=failure, gate=failure)` cannot be produced by a runner: a failed build skips
    // the gate. Both outcome branches select there, which is why disjointness is scoped by
    // reachability rather than asserted over the raw Cartesian product.
    let collision = AdvisoryCtx {
        failure: true,
        cancelled: false,
        build: StepOutcome::Failure,
        gate: StepOutcome::Failure,
        mutants: StepOutcome::Success,
    };
    assert!(
        !advisory_search_perf_reachable(&collision),
        "(build=Failure, gate=Failure) is unreachable"
    );
    assert_eq!(
        advisory_selected(SEARCH_PERF_WARNINGS, &search_exprs, &collision).len(),
        2,
        "the unreachable collision selects both outcome branches"
    );
}

#[test]
fn advisory_search_perf_warning_copy_is_pinned() {
    let ci = include_str!("../../.github/workflows/ci.yml");
    let body = job_lines(ci, "search-perf");
    assert!(!body.is_empty(), "ci.yml is missing job search-perf");
    advisory_assert_copy(&body, SEARCH_PERF_WARNING_COPY, "search-perf", 4);
    assert!(
        !body
            .iter()
            .any(|line| line.contains("missed its step budget")),
        "search-perf step names must not claim a budget event the outcome cannot establish"
    );
}

#[test]
fn advisory_mutation_warning_copy_is_pinned() {
    let ci = include_str!("../../.github/workflows/ci.yml");
    let body = job_lines(ci, "mutation");
    assert!(!body.is_empty(), "ci.yml is missing job mutation");
    advisory_assert_copy(&body, MUTATION_WARNING_COPY, "mutation", 3);
    assert!(
        !ci.contains(
            "::warning::mutation failed or timed out (120-minute step budget; non-blocking)"
        ),
        "the old catch-all mutation warning must be gone"
    );
    assert!(
        !body
            .iter()
            .any(|line| line.contains("missed its step budget")),
        "the mutation warning set must not keep the budget catch-all"
    );
    let mutants_at = body
        .iter()
        .position(|line| *line == "      - name: cargo mutants")
        .expect("mutation job is missing the cargo mutants step");
    assert_eq!(
        body[mutants_at + 1],
        "        id: mutants",
        "the cargo mutants step must be addressable as steps.mutants"
    );
    assert_eq!(
        body[mutants_at + 2],
        "        timeout-minutes: 120",
        "the cargo mutants step must keep its 120-minute budget"
    );
    assert!(
        body[mutants_at + 3].starts_with(
            "        run: cargo mutants --workspace --in-place --file 'src/ledger/**'"
        ),
        "the cargo mutants argv must stay unchanged"
    );
}

#[test]
fn advisory_cache_comment_states_pr_ref_scope_and_dependency_only() {
    let ci = include_str!("../../.github/workflows/ci.yml");
    let body = job_lines(ci, "search-perf");
    assert!(!body.is_empty(), "ci.yml is missing job search-perf");
    for line in CACHE_COMMENT_LINES {
        assert!(
            body.contains(line),
            "the rust-cache comment must state the ref-scope/dependency-only fact: {line}"
        );
    }
    for banned in [
        "does not leave the next run cold",
        "next run is not cold",
        "Save target/",
    ] {
        assert!(
            !ci.contains(banned),
            "ci.yml must not repeat the over-promising cache claim: {banned}"
        );
    }
    assert_eq!(
        body.iter()
            .filter(|line| line.trim() == "cache-on-failure: true")
            .count(),
        1,
        "cache-on-failure: true must stay exactly once in the search-perf job"
    );
    assert_eq!(
        body.iter()
            .filter(|line| line.contains(RUST_CACHE_PIN))
            .count(),
        1,
        "the rust-cache pin must stay exactly once in the search-perf job"
    );
    for banned in ["save-if", "prefix-key"] {
        assert!(
            !body.iter().any(|line| line.contains(banned)),
            "the cache policy must not change: {banned} is not part of 0568"
        );
    }
}

/// Cheap paths compiled into the library or a test. A new `include_str!` /
/// `include_bytes!` / `include!` of a cheap path fails this test until the
/// workflow comment, the classifier `is_exception` arm, and this list move
/// together.
const COMPILE_TIME_CHEAP_EXCEPTIONS: &[&str] = &[
    "README.md",
    "SECURITY.md",
    "docs/Call-Resolution.md",
    "docs/Ledgerful/skill.md",
    "docs/agent-output-contract.md",
    "docs/golden-path.md",
    "docs/installation.md",
    "docs/package-distribution.md",
    "docs/verify-performance.md",
];

/// Cheap paths read from this work tree via `CARGO_MANIFEST_DIR` or
/// `cargo_root()`. `docs/examples/config.toml` and `README.md` force `rust=true`.
const RUNTIME_CHEAP_EXCEPTIONS: &[&str] = &["README.md", "docs/examples/config.toml"];

fn is_cheap_ci_path(path: &str) -> bool {
    if path == "docs/api/openapi.json" {
        return false;
    }
    const EXACT: &[&str] = &[
        "CHANGELOG.md",
        "README.md",
        "LICENSE",
        "SECURITY.md",
        "AGENTS.md",
        "Agents.md",
        "Claude.md",
        "scripts/bump-manifests.ps1",
        "scripts/bump-manifests.sh",
    ];
    EXACT.contains(&path) || path.starts_with("docs/") || path.starts_with("packaging/")
}

fn strip_rust_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(b.len());
            continue;
        }
        if b[i] == b'"' {
            out.push('"');
            i += 1;
            while i < b.len() {
                out.push(b[i] as char);
                if b[i] == b'\\' && i + 1 < b.len() {
                    out.push(b[i + 1] as char);
                    i += 2;
                    continue;
                }
                if b[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

fn lexical_repo_rel(file: &std::path::Path, lit: &str) -> Option<String> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = file.parent()?.to_path_buf();
    for comp in std::path::Path::new(&lit.replace('\\', "/")).components() {
        match comp {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => out.push(part),
            _ => return None,
        }
    }
    let rel = out.strip_prefix(root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

fn push_include_literal(src: &str, at: usize, out: &mut Vec<(String, bool)>) {
    let bytes = src.as_bytes();
    let mut i = at;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || (bytes[i] != b'(' && bytes[i] != b'{') {
        return;
    }
    i += 1;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let rest = &src[i..];
    if let Some(stripped) = rest.strip_prefix("r#\"") {
        if let Some(end) = stripped.find("\"#") {
            out.push((stripped[..end].to_string(), false));
        }
        return;
    }
    if let Some(stripped) = rest.strip_prefix("r\"") {
        if let Some(end) = stripped.find('"') {
            out.push((stripped[..end].to_string(), false));
        }
        return;
    }
    if let Some(stripped) = rest.strip_prefix('"') {
        if let Some(end) = stripped.find('"') {
            out.push((stripped[..end].to_string(), false));
        }
        return;
    }
    if rest.starts_with("concat!") {
        let marker = "env!(\"CARGO_MANIFEST_DIR\")";
        let Some(pos) = rest.find(marker) else {
            return;
        };
        let after = &rest[pos + marker.len()..];
        let after = after.trim_start().trim_start_matches(',').trim_start();
        let Some(body) = after.strip_prefix('"') else {
            return;
        };
        let Some(end) = body.find('"') else {
            return;
        };
        out.push((body[..end].trim_start_matches('/').to_string(), true));
    }
}

fn discover_compile_time_cheap(root: &std::path::Path) -> Vec<String> {
    let mut found = Vec::new();
    for path in rust_sources(root) {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let src = strip_rust_comments(&raw);
        for needle in ["include_str!", "include_bytes!", "include!"] {
            let mut from = 0;
            while let Some(rel) = src[from..].find(needle) {
                let at = from + rel;
                if needle == "include!" {
                    let prev = src[..at].chars().next_back();
                    if prev.is_some_and(|c| c == '_' || c.is_ascii_alphanumeric()) {
                        from = at + needle.len();
                        continue;
                    }
                }
                let mut lits = Vec::new();
                push_include_literal(&src, at + needle.len(), &mut lits);
                for (lit, repo_relative) in lits {
                    let repo = if repo_relative {
                        lit
                    } else {
                        lexical_repo_rel(&path, &lit).unwrap_or(lit)
                    };
                    if is_cheap_ci_path(&repo) {
                        found.push(repo);
                    }
                }
                from = at + needle.len();
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

fn discover_runtime_cheap(root: &std::path::Path) -> Vec<String> {
    let mut found = Vec::new();
    for path in rust_sources(root) {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let src = strip_rust_comments(&raw);
        if !src.contains("CARGO_MANIFEST_DIR") && !src.contains("cargo_root()") {
            continue;
        }
        let mut from = 0;
        while let Some(rel) = src[from..].find("seed_file(\"") {
            let at = from + rel + "seed_file(\"".len();
            if let Some(end) = src[at..].find('"') {
                let lit = &src[at..at + end];
                if is_cheap_ci_path(lit) {
                    found.push(lit.to_string());
                }
            }
            from = at;
        }
        from = 0;
        while let Some(rel) = src[from..].find(".join(\"") {
            let at = from + rel + ".join(\"".len();
            let Some(end) = src[at..].find('"') else {
                break;
            };
            let lit = &src[at..at + end];
            let start = from.saturating_sub(400);
            let window_end = (at + end + 200).min(src.len());
            let window = &src[start..window_end];
            let rooted = window.contains("CARGO_MANIFEST_DIR") || window.contains("cargo_root()");
            let reads = window.contains("read_to_string")
                || window.contains("fs::read")
                || window.contains("File::open");
            if rooted && reads && is_cheap_ci_path(lit) {
                found.push(lit.to_string());
            }
            from = at + end;
        }
    }
    found.sort();
    found.dedup();
    found
}

fn rust_sources(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if matches!(
                    name.as_ref(),
                    "target" | ".git" | "output" | ".ledgerful" | "node_modules"
                ) {
                    continue;
                }
                stack.push(path);
            } else if name.ends_with(".rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn comment_section(ci: &str, begin: &str, end: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut on = false;
    for line in ci.lines() {
        if line.contains(begin) {
            on = true;
            continue;
        }
        if on && line.contains(end) {
            break;
        }
        if on {
            let path = line.trim().trim_start_matches('#').trim();
            if !path.is_empty() {
                out.push(path.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[test]
fn docs_only_exception_lists_match_includes_and_manifest_reads() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let ci = include_str!("../../.github/workflows/ci.yml");
    let compile = discover_compile_time_cheap(root);
    let runtime = discover_runtime_cheap(root);
    let compile_expected: Vec<String> = COMPILE_TIME_CHEAP_EXCEPTIONS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let runtime_expected: Vec<String> = RUNTIME_CHEAP_EXCEPTIONS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert_eq!(
        compile, compile_expected,
        "a new compile-time include of a cheap path is not on the exception list"
    );
    assert_eq!(
        runtime, runtime_expected,
        "a new manifest-rooted read of a cheap path is not on the exception list"
    );
    assert_eq!(
        comment_section(
            ci,
            "0573-compile-exceptions-begin",
            "0573-compile-exceptions-end"
        ),
        compile_expected
    );
    assert_eq!(
        comment_section(
            ci,
            "0573-runtime-exceptions-begin",
            "0573-runtime-exceptions-end"
        ),
        runtime_expected
    );
    let case = ci
        .lines()
        .find(|line| line.contains("docs/examples/config.toml) return 0"))
        .unwrap_or_else(|| panic!("classifier is_exception arm is missing"));
    let start = case
        .find("README.md")
        .unwrap_or_else(|| panic!("exception arm has no README.md"));
    let end = case[start..]
        .find(')')
        .map(|off| start + off)
        .unwrap_or_else(|| panic!("exception arm has no closing paren"));
    let mut arms: Vec<String> = case[start..end]
        .split('|')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect();
    arms.sort();
    arms.dedup();
    let mut union = compile_expected.clone();
    for path in &runtime_expected {
        if !union.contains(path) {
            union.push(path.clone());
        }
    }
    union.sort();
    assert_eq!(arms, union, "classifier case list drifted from the fixture");
}

#[derive(Clone, Copy)]
enum OldPath {
    Absent,
    Null,
    False,
    Zero,
    Text(&'static str),
}

struct RenameCase {
    label: &'static str,
    status: &'static str,
    filename: &'static str,
    old: OldPath,
    expect_write: bool,
    guard_must_block: bool,
}

struct ShellIf {
    cond: String,
    header: usize,
    else_at: Option<usize>,
    end: usize,
}

enum JqExpr {
    Any(Box<JqExpr>),
    And(Box<JqExpr>, Box<JqExpr>),
    Or(Box<JqExpr>, Box<JqExpr>),
    Eq(JqVal, JqVal),
    Ne(JqVal, JqVal),
}

enum JqVal {
    Field(String),
    TypeOf(String),
    Lit(serde_json::Value),
}

struct JqParser<'a> {
    source: &'a str,
    index: usize,
}

fn bound_jq_program(lines: &[&str]) -> Result<String, String> {
    let whiles: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains("while IFS= read -r path; do"))
        .map(|(idx, _)| idx)
        .collect();
    if whiles.len() != 1 {
        return Err(format!(
            "expected one `while IFS= read -r path` loop, found {}",
            whiles.len()
        ));
    }
    let while_at = whiles[0];
    let done_at = lines
        .iter()
        .enumerate()
        .skip(while_at)
        .find(|(_, line)| line.contains("done < <("))
        .map(|(idx, _)| idx)
        .ok_or_else(|| "the path loop has no `done < <(...)` process substitution".to_string())?;
    let done_line = lines[done_at];
    let marker = "done < <(";
    let open = done_line
        .find(marker)
        .ok_or_else(|| "process substitution marker vanished".to_string())?
        + marker.len()
        - 1;
    let mut depth = 0_i32;
    let mut end = None;
    for (offset, ch) in done_line[open..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(open + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let end =
        end.ok_or_else(|| "process substitution is not closed on the done line".to_string())?;
    let inside = &done_line[open + 1..end];
    if inside.matches("jq -r").count() != 1 {
        return Err(format!(
            "the path-loop process substitution must contain exactly one jq -r, not `{inside}`"
        ));
    }
    let quote = "jq -r '";
    let start = inside
        .find(quote)
        .ok_or_else(|| "jq -r program is not a single-quoted literal".to_string())?
        + quote.len();
    let rest = &inside[start..];
    let close = rest
        .find('\'')
        .ok_or_else(|| "jq -r program has no closing quote".to_string())?;
    Ok(rest[..close].to_string())
}

fn shell_ifs(lines: &[&str]) -> Result<Vec<ShellIf>, String> {
    let mut closed = Vec::new();
    let mut stack: Vec<ShellIf> = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("if ") && trimmed.ends_with("then") {
            stack.push(ShellIf {
                cond: trimmed.to_string(),
                header: idx,
                else_at: None,
                end: 0,
            });
            continue;
        }
        if trimmed == "else" {
            let frame = stack
                .last_mut()
                .ok_or_else(|| "else without if".to_string())?;
            if frame.else_at.is_some() {
                return Err("an if has two else branches".to_string());
            }
            frame.else_at = Some(idx);
            continue;
        }
        if trimmed == "fi" {
            let mut frame = stack.pop().ok_or_else(|| "fi without if".to_string())?;
            frame.end = idx;
            closed.push(frame);
        }
    }
    if !stack.is_empty() {
        return Err("an if in the changes job is not closed".to_string());
    }
    Ok(closed)
}

/// `Some(true)` is the else branch. `Some(false)` is the then branch.
fn shell_branch(frame: &ShellIf, line: usize) -> Option<bool> {
    if line <= frame.header || line >= frame.end {
        return None;
    }
    match frame.else_at {
        Some(else_at) if line > else_at => Some(true),
        Some(else_at) if line < else_at => Some(false),
        Some(_) => None,
        None => Some(false),
    }
}

fn shell_function<'a>(lines: &[&'a str], name: &str) -> Result<Vec<&'a str>, String> {
    let header = format!("{name}() {{");
    let start = lines
        .iter()
        .position(|line| line.contains(&header))
        .ok_or_else(|| format!("changes job is missing {name}()"))?;
    let mut depth = 0_i32;
    let mut body = Vec::new();
    for line in &lines[start..] {
        body.push(*line);
        depth += i32::try_from(line.matches('{').count()).unwrap_or(i32::MAX);
        depth -= i32::try_from(line.matches('}').count()).unwrap_or(i32::MAX);
        if body.len() > 1 && depth == 0 {
            return Ok(body);
        }
    }
    Err(format!("{name}() is not closed"))
}

fn shell_case_returns_zero(body: &[&str], arg: &str) -> Result<bool, String> {
    let text = body.join("\n");
    let case_at = text
        .find("case ")
        .ok_or_else(|| "shell function has no case".to_string())?;
    let after = &text[case_at..];
    let in_rel = after
        .find(" in")
        .ok_or_else(|| "case has no in".to_string())?;
    let rest = &after[in_rel + 3..];
    let esac = rest
        .find("esac")
        .ok_or_else(|| "case has no esac".to_string())?;
    for arm in rest[..esac].split(";;") {
        let Some((pattern, action)) = arm.trim().split_once(')') else {
            continue;
        };
        let returns_zero = action.contains("return 0");
        let returns_one = action.contains("return 1");
        if returns_zero == returns_one {
            return Err(format!("case arm has no single return: {arm}"));
        }
        if case_pattern_matches(pattern.trim(), arg) {
            return Ok(returns_zero);
        }
    }
    Err(format!("no case arm matched `{arg}`"))
}

fn case_pattern_matches(pattern: &str, arg: &str) -> bool {
    pattern.split('|').any(|alt| shell_glob(alt.trim(), arg))
}

fn shell_glob(pattern: &str, arg: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == arg;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    if !arg.starts_with(parts[0]) {
        return false;
    }
    let mut pos = parts[0].len();
    for (idx, part) in parts.iter().enumerate().skip(1) {
        let last = idx + 1 == parts.len();
        if last {
            if part.is_empty() {
                return true;
            }
            return arg.len() >= pos + part.len() && arg[pos..].ends_with(part);
        }
        if part.is_empty() {
            continue;
        }
        match arg[pos..].find(part) {
            Some(found) => pos += found + part.len(),
            None => return false,
        }
    }
    pos == arg.len()
}

fn jq_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn jq_equal(left: &serde_json::Value, right: &serde_json::Value) -> bool {
    match (left, right) {
        (serde_json::Value::Null, serde_json::Value::Null) => true,
        (serde_json::Value::Bool(lhs), serde_json::Value::Bool(rhs)) => lhs == rhs,
        (serde_json::Value::Number(lhs), serde_json::Value::Number(rhs)) => lhs == rhs,
        (serde_json::Value::String(lhs), serde_json::Value::String(rhs)) => lhs == rhs,
        _ => false,
    }
}

fn jq_raw_output(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::Bool(flag) => flag.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Semantics of the byte-locked program `.[] | .filename, (.previous_filename // empty)`.
/// `//` omits only `false` and `null` (including a missing field). `""` and `0` stay.
fn paths_of_bound_program(files: &serde_json::Value) -> Result<Vec<String>, String> {
    let entries = files
        .as_array()
        .ok_or_else(|| "fixture is not a JSON array".to_string())?;
    let mut paths = Vec::new();
    for entry in entries {
        let filename = entry
            .get("filename")
            .ok_or_else(|| "entry has no filename".to_string())?;
        paths.push(jq_raw_output(filename));
        match entry.get("previous_filename") {
            None | Some(serde_json::Value::Null) | Some(serde_json::Value::Bool(false)) => {}
            Some(other) => paths.push(jq_raw_output(other)),
        }
    }
    Ok(paths)
}

fn cheap_loop_rejects(lines: &[&str], paths: &[String]) -> Result<bool, String> {
    let cheap = shell_function(lines, "is_cheap")?;
    let exception = shell_function(lines, "is_exception")?;
    for path in paths {
        let is_cheap = shell_case_returns_zero(&cheap, path)?;
        let is_exception = shell_case_returns_zero(&exception, path)?;
        if !is_cheap || is_exception {
            return Ok(true);
        }
    }
    Ok(false)
}

fn rename_guard_blocks(
    cond: &str,
    echo_in_else: bool,
    files: &serde_json::Value,
) -> Result<bool, String> {
    let rest = cond.trim().trim_start_matches("if").trim_start();
    let negated = rest.starts_with('!');
    let marker = "jq -e '";
    let start = cond
        .find(marker)
        .ok_or_else(|| "the dominating guard has no jq -e program".to_string())?
        + marker.len();
    let after = &cond[start..];
    let end = after
        .find('\'')
        .ok_or_else(|| "the dominating jq -e program has no closing quote".to_string())?;
    let expr = JqParser::parse_filter(&after[..end])?;
    let truthy = eval_jq(&expr, files)?;
    let command_ok = if negated { !truthy } else { truthy };
    let echo_runs = if echo_in_else {
        !command_ok
    } else {
        command_ok
    };
    Ok(!echo_runs)
}

fn eval_jq(expr: &JqExpr, current: &serde_json::Value) -> Result<bool, String> {
    match expr {
        JqExpr::Any(inner) => {
            let items = current
                .as_array()
                .ok_or_else(|| "any() input is not an array".to_string())?;
            for item in items {
                if eval_jq(inner, item)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        JqExpr::And(left, right) => Ok(eval_jq(left, current)? && eval_jq(right, current)?),
        JqExpr::Or(left, right) => Ok(eval_jq(left, current)? || eval_jq(right, current)?),
        JqExpr::Eq(left, right) => Ok(jq_equal(
            &eval_jq_val(left, current)?,
            &eval_jq_val(right, current)?,
        )),
        JqExpr::Ne(left, right) => Ok(!jq_equal(
            &eval_jq_val(left, current)?,
            &eval_jq_val(right, current)?,
        )),
    }
}

fn eval_jq_val(val: &JqVal, current: &serde_json::Value) -> Result<serde_json::Value, String> {
    match val {
        JqVal::Lit(lit) => Ok(lit.clone()),
        JqVal::Field(name) => Ok(current
            .get(name)
            .cloned()
            .unwrap_or(serde_json::Value::Null)),
        JqVal::TypeOf(name) => {
            let found = current
                .get(name)
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Ok(serde_json::Value::String(jq_type_name(&found).to_string()))
        }
    }
}

fn one_entry(status: &str, filename: &str, old: OldPath) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "status".to_string(),
        serde_json::Value::String(status.to_string()),
    );
    map.insert(
        "filename".to_string(),
        serde_json::Value::String(filename.to_string()),
    );
    match old {
        OldPath::Absent => {}
        OldPath::Null => {
            map.insert("previous_filename".to_string(), serde_json::Value::Null);
        }
        OldPath::False => {
            map.insert(
                "previous_filename".to_string(),
                serde_json::Value::Bool(false),
            );
        }
        OldPath::Zero => {
            map.insert("previous_filename".to_string(), serde_json::json!(0));
        }
        OldPath::Text(text) => {
            map.insert(
                "previous_filename".to_string(),
                serde_json::Value::String(text.to_string()),
            );
        }
    }
    serde_json::Value::Array(vec![serde_json::Value::Object(map)])
}

impl<'a> JqParser<'a> {
    fn parse_filter(source: &'a str) -> Result<JqExpr, String> {
        let mut parser = Self { source, index: 0 };
        parser.skip();
        if !parser.eat_word("any") || !parser.eat("(") || !parser.eat_dot_each() || !parser.eat(";")
        {
            return Err(format!("guard filter must be any(.[]; ...): {source}"));
        }
        let inner = parser.parse_or()?;
        if !parser.eat(")") {
            return Err("any() is missing its closing paren".to_string());
        }
        parser.skip();
        if parser.index != parser.source.len() {
            return Err(format!(
                "trailing jq after the guard filter: {}",
                &parser.source[parser.index..]
            ));
        }
        Ok(JqExpr::Any(Box::new(inner)))
    }

    fn parse_or(&mut self) -> Result<JqExpr, String> {
        let mut left = self.parse_and()?;
        while self.eat_word("or") {
            let right = self.parse_and()?;
            left = JqExpr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<JqExpr, String> {
        let mut left = self.parse_atom()?;
        while self.eat_word("and") {
            let right = self.parse_atom()?;
            left = JqExpr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_atom(&mut self) -> Result<JqExpr, String> {
        self.skip();
        if self.peek() == Some('(') {
            let saved = self.index;
            if let Some(expr) = self.try_grouped_expr() {
                return Ok(expr);
            }
            self.index = saved;
        }
        let left = self.parse_val()?;
        self.skip();
        if self.eat("==") {
            return Ok(JqExpr::Eq(left, self.parse_val()?));
        }
        if self.eat("!=") {
            return Ok(JqExpr::Ne(left, self.parse_val()?));
        }
        Err(format!(
            "expected a comparison in the rename guard at {}",
            &self.source[self.index..]
        ))
    }

    fn try_grouped_expr(&mut self) -> Option<JqExpr> {
        if !self.eat("(") {
            return None;
        }
        let inner = self.parse_or().ok()?;
        if !self.eat(")") {
            return None;
        }
        Some(inner)
    }

    fn parse_val(&mut self) -> Result<JqVal, String> {
        self.skip();
        if self.eat("(") {
            let inner = self.parse_val()?;
            if !self.eat(")") {
                return Err("unclosed '(' around a jq value".to_string());
            }
            return Ok(inner);
        }
        if self.eat(".") {
            let name = self.parse_ident()?;
            self.skip();
            if self.eat("|") {
                if !self.eat_word("type") {
                    return Err("the guard may only pipe a field to type".to_string());
                }
                return Ok(JqVal::TypeOf(name));
            }
            return Ok(JqVal::Field(name));
        }
        if self.peek() == Some('"') {
            return Ok(JqVal::Lit(serde_json::Value::String(self.parse_string()?)));
        }
        if self.eat_word("false") {
            return Ok(JqVal::Lit(serde_json::Value::Bool(false)));
        }
        if self.eat_word("true") {
            return Ok(JqVal::Lit(serde_json::Value::Bool(true)));
        }
        if self.eat_word("null") {
            return Ok(JqVal::Lit(serde_json::Value::Null));
        }
        if self
            .peek()
            .is_some_and(|ch| ch == '-' || ch.is_ascii_digit())
        {
            return Ok(JqVal::Lit(self.parse_number()?));
        }
        Err(format!(
            "unexpected jq value at {}",
            &self.source[self.index..]
        ))
    }

    fn parse_string(&mut self) -> Result<String, String> {
        if !self.eat("\"") {
            return Err("expected a jq string".to_string());
        }
        let start = self.index;
        while self.index < self.source.len() && self.peek() != Some('"') {
            if self.peek() == Some('\\') {
                return Err("jq string escapes are outside this guard".to_string());
            }
            let ch = self
                .peek()
                .ok_or_else(|| "unterminated jq string".to_string())?;
            self.index += ch.len_utf8();
        }
        let text = self.source[start..self.index].to_string();
        if !self.eat("\"") {
            return Err("unterminated jq string".to_string());
        }
        Ok(text)
    }

    fn parse_ident(&mut self) -> Result<String, String> {
        self.skip();
        let start = self.index;
        while let Some(ch) = self.peek() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                self.index += 1;
            } else {
                break;
            }
        }
        if start == self.index {
            return Err(format!(
                "expected a jq field at {}",
                &self.source[self.index..]
            ));
        }
        Ok(self.source[start..self.index].to_string())
    }

    fn parse_number(&mut self) -> Result<serde_json::Value, String> {
        let start = self.index;
        if self.peek() == Some('-') {
            self.index += 1;
        }
        let digits = self.index;
        while self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
            self.index += 1;
        }
        if self.index == digits {
            return Err("expected a jq number".to_string());
        }
        let text = &self.source[start..self.index];
        let number: i64 = text
            .parse()
            .map_err(|_| format!("jq number `{text}` is out of range"))?;
        Ok(serde_json::Value::Number(number.into()))
    }

    fn skip(&mut self) {
        while self.peek().is_some_and(|ch| ch.is_ascii_whitespace()) {
            self.index += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.source[self.index..].chars().next()
    }

    fn eat(&mut self, token: &str) -> bool {
        self.skip();
        if self.source[self.index..].starts_with(token) {
            self.index += token.len();
            true
        } else {
            false
        }
    }

    fn eat_word(&mut self, word: &str) -> bool {
        self.skip();
        if !self.source[self.index..].starts_with(word) {
            return false;
        }
        let after = self.index + word.len();
        let boundary = self.source[after..]
            .chars()
            .next()
            .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_');
        if boundary {
            self.index = after;
            true
        } else {
            false
        }
    }

    fn eat_dot_each(&mut self) -> bool {
        self.skip();
        if !self.source[self.index..].starts_with(".[") {
            return false;
        }
        self.index += 2;
        self.skip();
        if self.source[self.index..].starts_with(']') {
            self.index += 1;
            true
        } else {
            false
        }
    }
}

#[test]
fn docs_only_rename_guard_dominates_rust_false() {
    let ci = include_str!("../../.github/workflows/ci.yml");
    let lines = job_lines(ci, "changes");
    assert!(!lines.is_empty(), "ci.yml is missing the changes job");

    let program = bound_jq_program(&lines).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(
        program, ".[] | .filename, (.previous_filename // empty)",
        "the while-loop process substitution must carry the 0576 path program"
    );
    assert!(
        lines
            .join("\n")
            .contains("if ! is_cheap \"$path\" || is_exception \"$path\"; then"),
        "the cheap check inside the path loop must stay the 0573 predicate"
    );

    let frames = shell_ifs(&lines).unwrap_or_else(|err| panic!("{err}"));
    let echo_lines: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains("echo \"rust=false\""))
        .map(|(idx, _)| idx)
        .collect();
    assert_eq!(
        echo_lines.len(),
        1,
        "the changes job must write rust=false once"
    );
    let echo_at = echo_lines[0];
    let mentioning: Vec<&ShellIf> = frames
        .iter()
        .filter(|frame| frame.cond.contains("previous_filename"))
        .collect();
    assert!(
        !mentioning.is_empty(),
        "no changes-job condition mentions previous_filename"
    );
    let dominating: Vec<&ShellIf> = mentioning
        .iter()
        .copied()
        .filter(|frame| echo_at > frame.header && echo_at < frame.end)
        .collect();
    assert_eq!(
        dominating.len(),
        mentioning.len(),
        "a condition mentions previous_filename but does not dominate the rust=false write"
    );
    assert_eq!(dominating.len(), 1, "expected one dominating rename guard");
    let guard = dominating[0];
    let echo_branch = shell_branch(guard, echo_at);
    assert!(
        echo_branch.is_some(),
        "rust=false is outside the rename guard's branches"
    );
    let while_at = lines
        .iter()
        .position(|line| line.contains("while IFS= read -r path; do"))
        .unwrap_or_else(|| panic!("missing path loop"));
    assert!(
        guard.header < while_at && while_at < echo_at,
        "the rename guard must run before the cheap check, and that check before the write"
    );
    assert_eq!(
        shell_branch(guard, while_at),
        echo_branch,
        "the cheap check and the rust=false write must sit in the same guard branch"
    );
    let echo_in_else = echo_branch == Some(true);

    let cases = [
        RenameCase {
            label: "renamed src/foo.rs to docs/foo.md",
            status: "renamed",
            filename: "docs/foo.md",
            old: OldPath::Text("src/foo.rs"),
            expect_write: false,
            guard_must_block: false,
        },
        RenameCase {
            label: "renamed docs/a.md to docs/b.md",
            status: "renamed",
            filename: "docs/b.md",
            old: OldPath::Text("docs/a.md"),
            expect_write: true,
            guard_must_block: false,
        },
        RenameCase {
            label: "added docs/only.md",
            status: "added",
            filename: "docs/only.md",
            old: OldPath::Absent,
            expect_write: true,
            guard_must_block: false,
        },
        RenameCase {
            label: "modified docs/only.md",
            status: "modified",
            filename: "docs/only.md",
            old: OldPath::Absent,
            expect_write: true,
            guard_must_block: false,
        },
        RenameCase {
            label: "added src/foo.rs",
            status: "added",
            filename: "src/foo.rs",
            old: OldPath::Absent,
            expect_write: false,
            guard_must_block: false,
        },
        RenameCase {
            label: "modified src/foo.rs",
            status: "modified",
            filename: "src/foo.rs",
            old: OldPath::Absent,
            expect_write: false,
            guard_must_block: false,
        },
        RenameCase {
            label: "renamed missing previous_filename",
            status: "renamed",
            filename: "docs/foo.md",
            old: OldPath::Absent,
            expect_write: false,
            guard_must_block: true,
        },
        RenameCase {
            label: "renamed null previous_filename",
            status: "renamed",
            filename: "docs/foo.md",
            old: OldPath::Null,
            expect_write: false,
            guard_must_block: true,
        },
        RenameCase {
            label: "renamed empty previous_filename",
            status: "renamed",
            filename: "docs/foo.md",
            old: OldPath::Text(""),
            expect_write: false,
            guard_must_block: true,
        },
        RenameCase {
            label: "renamed false previous_filename",
            status: "renamed",
            filename: "docs/foo.md",
            old: OldPath::False,
            expect_write: false,
            guard_must_block: true,
        },
        RenameCase {
            label: "renamed zero previous_filename",
            status: "renamed",
            filename: "docs/foo.md",
            old: OldPath::Zero,
            expect_write: false,
            guard_must_block: true,
        },
    ];

    for case in cases {
        let files = one_entry(case.status, case.filename, case.old);
        let blocks = rename_guard_blocks(&guard.cond, echo_in_else, &files)
            .unwrap_or_else(|err| panic!("{}: {err}", case.label));
        let paths =
            paths_of_bound_program(&files).unwrap_or_else(|err| panic!("{}: {err}", case.label));
        let cheap_rejects = cheap_loop_rejects(&lines, &paths)
            .unwrap_or_else(|err| panic!("{}: {err}", case.label));
        let writes = !blocks && !cheap_rejects;
        assert_eq!(
            blocks, case.guard_must_block,
            "{}: guard decision (paths={paths:?}, cheap_rejects={cheap_rejects})",
            case.label
        );
        assert_eq!(
            writes, case.expect_write,
            "{}: rust=false write",
            case.label
        );
        if case.guard_must_block {
            assert!(
                blocks,
                "{}: the guard itself must reject before is_cheap",
                case.label
            );
        }
        if case.status == "renamed"
            && matches!(case.old, OldPath::Absent | OldPath::Null | OldPath::False)
        {
            assert_eq!(
                paths,
                vec![case.filename.to_string()],
                "{}: // empty must omit the old path while filename stays cheap",
                case.label
            );
            assert!(
                !cheap_rejects,
                "{}: the new path is cheap, so only the guard stops the write",
                case.label
            );
        }
        if matches!(case.old, OldPath::Text("")) {
            assert!(
                paths.iter().any(|path| path.is_empty()),
                "{}: // empty keeps a blank previous_filename",
                case.label
            );
        }
        if matches!(case.old, OldPath::Zero) {
            assert!(
                paths.iter().any(|path| path == "0"),
                "{}: // empty keeps JSON 0",
                case.label
            );
        }
    }
}
