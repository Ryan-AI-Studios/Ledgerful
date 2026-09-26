//! 0435: `ledgerful-ledger` must not depend on the root `ledgerful` package.

#![cfg(test)]

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
