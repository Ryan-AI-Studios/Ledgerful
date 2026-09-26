use std::process::Command;

#[test]
fn binary_shows_ledgerful_help() {
    let binary = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(binary)
        .arg("--help")
        .output()
        .expect("binary should run");

    assert!(output.status.success(), "{binary} --help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Ledgerful"),
        "{binary} --help should mention Ledgerful"
    );
    assert!(
        stdout.contains("reset"),
        "{binary} --help should list reset command"
    );

    let start = stdout
        .find("Commands:")
        .expect("--help must contain Commands:");
    let rest = &stdout[start..];
    let options = rest
        .find("Options:")
        .expect("--help must contain Options: after Commands:");
    let commands = &rest[..options];
    for hidden in ["services", "deploy", "observability"] {
        let listed = commands
            .lines()
            .any(|line| line.split_whitespace().next() == Some(hidden));
        assert!(
            !listed,
            "Commands block must omit {hidden}; commands={commands}"
        );
    }
    assert!(
        stdout.contains("Gated or empty:"),
        "--help after_help must include Gated or empty: marker"
    );
    assert!(
        stdout.contains("ledgerful surfaces"),
        "--help after_help must point at ledgerful surfaces"
    );
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[test]
fn readme_lists_every_default_help_command() {
    let binary = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(binary)
        .arg("--help")
        .output()
        .expect("binary should run");
    assert!(output.status.success(), "{binary} --help should succeed");
    let stdout = strip_ansi(&String::from_utf8_lossy(&output.stdout));
    let start = stdout
        .find("Commands:")
        .expect("--help must contain Commands:");
    let rest = &stdout[start..];
    let options = rest
        .find("Options:")
        .expect("--help must contain Options: after Commands:");
    let commands = &rest[..options];
    let readme_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    let readme = std::fs::read_to_string(&readme_path).expect("README.md must be readable");

    let mut names: Vec<String> = Vec::new();
    for line in commands.lines() {
        // Clap command rows use two-space indent; wrapped about text is
        // deeper. Do not treat wrap tokens (`npm`, `and`, …) as names.
        let n_spaces = line.chars().take_while(|c| *c == ' ').count();
        if n_spaces != 2 {
            continue;
        }
        let rest = line.trim_start();
        let Some(name) = rest.split_whitespace().next() else {
            continue;
        };
        if name.eq_ignore_ascii_case("Commands:") {
            continue;
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            continue;
        }
        names.push(name.to_string());
    }
    assert!(
        names.len() >= 45,
        "this binary's --help Commands: must list at least the 45 default-feature names, got {}: {names:?}",
        names.len()
    );
    let mut missing: Vec<String> = Vec::new();
    for name in &names {
        let needle = format!("`{name}`");
        if !readme.contains(&needle) {
            missing.push(name.clone());
        }
    }
    for gated in ["services", "deploy", "observability"] {
        let needle = format!("`{gated}`");
        if !readme.contains(&needle) {
            missing.push(gated.to_string());
        }
    }
    assert!(
        missing.is_empty(),
        "README.md must name every default --help Commands: entry plus gated three as `name`:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn binary_reports_package_version() {
    let binary = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .expect("binary should run");

    assert!(output.status.success(), "{binary} --version should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().starts_with("ledgerful "),
        "{binary} --version should report package identity, got: {stdout}"
    );
}
