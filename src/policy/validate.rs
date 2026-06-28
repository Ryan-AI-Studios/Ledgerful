use crate::policy::error::PolicyError;
use crate::policy::rules::Rules;
use globset::Glob;
use miette::Result;

/// Validates the rules.
pub fn validate_rules(rules: &Rules) -> Result<()> {
    // Validate glob patterns in overrides
    for rule in &rules.overrides {
        Glob::new(&rule.pattern).map_err(|e| PolicyError::InvalidPattern {
            pattern: rule.pattern.clone(),
            source: e,
        })?;
    }

    // Validate glob patterns in protected_paths
    for pattern in &rules.protected_paths {
        Glob::new(pattern).map_err(|e| PolicyError::InvalidPattern {
            pattern: pattern.clone(),
            source: e,
        })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::rules::PathRule;

    #[test]
    fn test_validate_valid_rules() {
        let mut rules = Rules::default();
        rules.overrides.push(PathRule {
            pattern: "*.rs".to_string(),
            mode: None,
            required_verifications: vec![],
        });
        assert!(validate_rules(&rules).is_ok());
    }

    #[test]
    fn test_validate_invalid_pattern() {
        let mut rules = Rules::default();
        rules.overrides.push(PathRule {
            pattern: "[".to_string(),
            mode: None,
            required_verifications: vec![],
        });
        assert!(validate_rules(&rules).is_err());
    }
}
