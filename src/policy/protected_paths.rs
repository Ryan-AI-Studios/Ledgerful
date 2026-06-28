use crate::policy::error::PolicyError;
use crate::policy::rules::Rules;
use globset::{Glob, GlobSet, GlobSetBuilder};
use miette::Result;

pub struct ProtectedPathChecker {
    protected_set: GlobSet,
}

impl ProtectedPathChecker {
    pub fn new(rules: &Rules) -> Result<Self> {
        let mut builder = GlobSetBuilder::new();
        for pattern in &rules.protected_paths {
            let glob = Glob::new(pattern).map_err(|e| PolicyError::InvalidPattern {
                pattern: pattern.clone(),
                source: e,
            })?;
            builder.add(glob);
        }
        let protected_set = builder.build().map_err(|e| PolicyError::ValidationFailed {
            reason: format!("Failed to build globset for protected paths: {}", e),
        })?;

        Ok(Self { protected_set })
    }

    /// Evaluates if a given path falls under a configured protected_path pattern.
    pub fn is_protected(&self, path: &str) -> bool {
        self.protected_set.is_match(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protected_path_checker() {
        let rules = Rules {
            protected_paths: vec!["Cargo.lock".to_string(), ".github/workflows/**".to_string()],
            ..Default::default()
        };

        let checker = ProtectedPathChecker::new(&rules).unwrap();

        assert!(checker.is_protected("Cargo.lock"));
        assert!(checker.is_protected(".github/workflows/ci.yml"));
        assert!(!checker.is_protected("src/main.rs"));
    }
}
