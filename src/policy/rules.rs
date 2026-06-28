use crate::policy::mode::Mode;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    #[serde(default)]
    pub global: GlobalRules,
    #[serde(default)]
    pub overrides: Vec<PathRule>,
    #[serde(default)]
    pub protected_paths: Vec<String>,
    #[serde(skip)]
    pub was_legacy_default: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
pub struct GlobalRules {
    pub mode: Mode,
    #[serde(default)]
    pub required_verifications: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct PathRule {
    pub pattern: String,
    pub mode: Option<Mode>,
    #[serde(default)]
    pub required_verifications: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rules_defaults() {
        let rules = Rules::default();
        assert_eq!(rules.global.mode, Mode::Analyze);
        assert!(rules.overrides.is_empty());
    }

    #[test]
    fn test_rules_deserialization() {
        let toml_str = r#"
            protected_paths = ["secret.txt"]

            [global]
            mode = "enforce"
            required_verifications = ["test"]

            [[overrides]]
            pattern = "*.rs"
            mode = "review"
        "#;
        let rules: Rules = toml::from_str(toml_str).unwrap();
        assert_eq!(rules.global.mode, Mode::Enforce);
        assert_eq!(rules.overrides[0].pattern, "*.rs");
        assert_eq!(rules.overrides[0].mode, Some(Mode::Review));
        assert!(rules.protected_paths.contains(&"secret.txt".to_string()));
    }
}
