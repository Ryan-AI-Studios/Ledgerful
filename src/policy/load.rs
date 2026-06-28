use crate::policy::error::PolicyError;
use crate::policy::rules::Rules;
use crate::policy::validate::validate_rules;
use crate::state::layout::Layout;
use miette::Result;
use std::fs;

/// Loads the rules from the workspace root.
/// If the rules file does not exist, it returns the default rules.
pub fn load_rules(layout: &Layout) -> Result<Rules> {
    let path = layout.rules_file();

    if !path.exists() {
        return Ok(Rules::default());
    }

    let content = fs::read_to_string(&path).map_err(|e| PolicyError::ReadFailed {
        path: path.to_string(),
        source: e,
    })?;

    let mut rules: Rules =
        toml::from_str(&content).map_err(|e| PolicyError::ParseFailed { source: e })?;

    validate_rules(&rules)?;

    // Route legacy default through automatic policy
    let legacy_rules: Rules = toml::from_str(crate::policy::defaults::LEGACY_DEFAULT_RULES)
        .expect("LEGACY_DEFAULT_RULES must be valid");
    if rules == legacy_rules {
        rules.overrides.clear();
        rules.was_legacy_default = true;
        tracing::debug!(
            "Detected historical generated rules.toml default; stripping obsolete overrides to route through auto_policy."
        );
    }

    Ok(rules)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::mode::Mode;
    use camino::Utf8Path;
    use tempfile::tempdir;

    #[test]
    fn test_load_default_rules_if_missing() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);

        let rules = load_rules(&layout).unwrap();
        assert_eq!(rules.global.mode, Mode::Analyze);
    }

    #[test]
    fn test_load_custom_rules() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();

        let rules_path = layout.rules_file();
        fs::write(rules_path, "[global]\nmode = \"enforce\"").unwrap();

        let rules = load_rules(&layout).unwrap();
        assert_eq!(rules.global.mode, Mode::Enforce);
    }

    #[test]
    fn test_load_rules_rejects_invalid_pattern() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();

        fs::write(
            layout.rules_file(),
            r#"
[global]
mode = "analyze"

[[overrides]]
pattern = "["
"#,
        )
        .unwrap();

        assert!(load_rules(&layout).is_err());
    }

    #[test]
    fn test_legacy_default_rules_migration() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();

        // Exact legacy default
        fs::write(
            layout.rules_file(),
            crate::policy::defaults::LEGACY_DEFAULT_RULES,
        )
        .unwrap();
        let rules = load_rules(&layout).unwrap();
        assert!(
            rules.overrides.is_empty(),
            "Exact legacy rules should be stripped of overrides"
        );

        // Whitespace-equivalent legacy default
        let whitespace_legacy = crate::policy::defaults::LEGACY_DEFAULT_RULES.replace(" = ", "=");
        fs::write(layout.rules_file(), whitespace_legacy).unwrap();
        let rules = load_rules(&layout).unwrap();
        assert!(
            rules.overrides.is_empty(),
            "Whitespace-equivalent legacy rules should be stripped"
        );

        // One-field-modified custom rules (e.g. changed mode)
        let custom_rules = crate::policy::defaults::LEGACY_DEFAULT_RULES
            .replace("mode = \"analyze\"", "mode = \"enforce\"");
        fs::write(layout.rules_file(), custom_rules).unwrap();
        let rules = load_rules(&layout).unwrap();
        assert!(
            !rules.overrides.is_empty(),
            "Customized legacy rules should retain overrides"
        );
    }
}
