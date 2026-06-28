use crate::policy::error::PolicyError;
use crate::policy::mode::Mode;
use crate::policy::rules::Rules;
use globset::{Glob, GlobSet, GlobSetBuilder};
use miette::Result;

pub struct RuleMatcher {
    rules: Rules,
    override_set: GlobSet,
}

impl RuleMatcher {
    pub fn new(rules: Rules) -> Result<Self> {
        let mut builder = GlobSetBuilder::new();
        for rule in &rules.overrides {
            let glob = Glob::new(&rule.pattern).map_err(|e| PolicyError::InvalidPattern {
                pattern: rule.pattern.clone(),
                source: e,
            })?;
            builder.add(glob);
        }
        let override_set = builder.build().map_err(|e| PolicyError::ValidationFailed {
            reason: format!("Failed to build globset for overrides: {}", e),
        })?;

        Ok(Self {
            rules,
            override_set,
        })
    }

    /// Evaluates which rules and mode apply to a specific changed file path.
    pub fn match_path(&self, path: &str) -> (Mode, Vec<String>) {
        let matches = self.override_set.matches(path);

        let mut final_mode = self.rules.global.mode;
        let mut final_verifications = self.rules.global.required_verifications.clone();

        // If multiple overrides match, we take the last one that specifies a mode
        // and collect all required verifications (union).
        for &index in &matches {
            let rule = &self.rules.overrides[index];
            if let Some(mode) = rule.mode {
                final_mode = mode;
            }
            for v in &rule.required_verifications {
                if !final_verifications.contains(v) {
                    final_verifications.push(v.clone());
                }
            }
        }

        (final_mode, final_verifications)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::rules::PathRule;

    #[test]
    fn test_rule_matcher_global_fallback() {
        let mut rules = Rules::default();
        rules.global.mode = Mode::Enforce;
        rules.global.required_verifications = vec!["base".to_string()];

        let matcher = RuleMatcher::new(rules).unwrap();
        let (mode, verifications) = matcher.match_path("any_file.txt");

        assert_eq!(mode, Mode::Enforce);
        assert_eq!(verifications, vec!["base".to_string()]);
    }

    #[test]
    fn test_rule_matcher_override() {
        let mut rules = Rules::default();
        rules.global.mode = Mode::Analyze;
        rules.overrides.push(PathRule {
            pattern: "*.rs".to_string(),
            mode: Some(Mode::Review),
            required_verifications: vec!["test".to_string()],
        });

        let matcher = RuleMatcher::new(rules).unwrap();
        let (mode, verifications) = matcher.match_path("src/main.rs");

        assert_eq!(mode, Mode::Review);
        assert!(verifications.contains(&"test".to_string()));
    }

    #[test]
    fn test_rule_matcher_union_verifications() {
        let mut rules = Rules::default();
        rules.global.required_verifications = vec!["base".to_string()];
        rules.overrides.push(PathRule {
            pattern: "src/**".to_string(),
            mode: None,
            required_verifications: vec!["lint".to_string()],
        });
        rules.overrides.push(PathRule {
            pattern: "src/**/*.rs".to_string(),
            mode: Some(Mode::Enforce),
            required_verifications: vec!["test".to_string()],
        });

        let matcher = RuleMatcher::new(rules).unwrap();
        let (mode, verifications) = matcher.match_path("src/lib.rs");

        assert_eq!(mode, Mode::Enforce);
        assert!(verifications.contains(&"base".to_string()));
        assert!(verifications.contains(&"lint".to_string()));
        assert!(verifications.contains(&"test".to_string()));
    }
}
