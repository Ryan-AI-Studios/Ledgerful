//! 0435: `ledgerful-ledger` must not depend on the root `ledgerful` package.
//! 0439: member `homepage` / `repository` inherit from `[workspace.package]`;
//! description and readme stay local strings.

#![cfg(test)]

fn package_lines(source: &str) -> impl Iterator<Item = &str> {
    source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && *line != "[package]")
}

fn package_has_exact_line(source: &str, expected: &str) -> bool {
    package_lines(source)
        .take_while(|line| !line.starts_with('['))
        .any(|line| line == expected)
}

#[test]
fn ledgerful_ledger_manifest_has_no_root_package_dep() {
    let source = include_str!("../../crates/ledgerful-ledger/Cargo.toml");
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        assert!(
            !trimmed.contains("ledgerful =") && !trimmed.contains("package = \"ledgerful\""),
            "ledgerful-ledger must not depend on the ledgerful package: {trimmed}"
        );
    }
    assert!(
        source.contains("name = \"ledgerful-ledger\""),
        "member package name must stay ledgerful-ledger"
    );
    assert!(
        source.contains("publish = false"),
        "member must not be crates.io-published"
    );
}

#[test]
fn ledgerful_ledger_manifest_inherits_workspace_homepage_and_repository() {
    let source = include_str!("../../crates/ledgerful-ledger/Cargo.toml");
    assert!(
        package_has_exact_line(source, "homepage.workspace = true"),
        "member homepage must be homepage.workspace = true (0439)"
    );
    assert!(
        package_has_exact_line(source, "repository.workspace = true"),
        "member repository must be repository.workspace = true (0439)"
    );
    assert!(
        package_has_exact_line(
            source,
            "description = \"Ledgerful ledger core: types, chain, signatures, SQLite rows\""
        ),
        "member description must stay a local string"
    );
    assert!(
        package_has_exact_line(source, "readme = \"README.md\""),
        "member readme must stay a local string (workspace readme is the root README)"
    );
    assert!(
        !package_has_exact_line(source, "description.workspace = true"),
        "member description must not inherit (workspace description is the engine crate)"
    );
    assert!(
        !package_has_exact_line(source, "readme.workspace = true"),
        "member readme must not inherit (workspace readme is relative to the workspace root)"
    );
}
