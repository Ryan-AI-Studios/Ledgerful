use camino::Utf8Path;
use ledgerful::config::{load_config, validate_config};
use ledgerful::policy::{Mode, ProtectedPathChecker, RuleMatcher, load_rules, validate_rules};
use ledgerful::state::layout::Layout;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_full_config_workflow() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();

    // 1. Create custom config
    let config_toml = r#"
        [core]
        strict = true
    "#;
    fs::write(layout.config_file(), config_toml).unwrap();

    // 2. Load and validate config
    let config = load_config(&layout).unwrap();
    assert!(config.core.strict);
    validate_config(&config).unwrap();

    // 3. Create custom rules
    let rules_toml = r#"
        protected_paths = ["secret/**"]

        [global]
        mode = "review"
        required_verifications = ["lint"]

        [[overrides]]
        pattern = "src/**/*.rs"
        mode = "enforce"
        required_verifications = ["test"]
    "#;
    fs::write(layout.rules_file(), rules_toml).unwrap();

    // 4. Load and validate rules
    let rules = load_rules(&layout).unwrap();
    validate_rules(&rules).unwrap();

    // 5. Test RuleMatcher
    let matcher = RuleMatcher::new(rules.clone()).unwrap();

    // Test global fallback
    let (mode, verifications) = matcher.match_path("README.md");
    assert_eq!(mode, Mode::Review);
    assert!(verifications.contains(&"lint".to_string()));

    // Test override
    let (mode, verifications) = matcher.match_path("src/lib.rs");
    assert_eq!(mode, Mode::Enforce);
    assert!(verifications.contains(&"lint".to_string()));
    assert!(verifications.contains(&"test".to_string()));

    // 6. Test ProtectedPathChecker
    let checker = ProtectedPathChecker::new(&rules).unwrap();
    assert!(checker.is_protected("secret/key.txt"));
    assert!(!checker.is_protected("src/lib.rs"));
}
