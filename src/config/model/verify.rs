use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct VerifyStep {
    /// Human-readable description of what this step verifies
    pub description: String,
    /// The shell command to execute
    pub command: String,
    /// Per-step timeout in seconds. None means use verify.default_timeout_secs.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VerifyMode {
    Auto,
    Explicit,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VerifyConfig {
    /// The verification mode. Auto infers checks; Explicit uses only defined steps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<VerifyMode>,
    /// Ordered list of verification steps to run when no `-c` flag is provided
    #[serde(default)]
    pub steps: Vec<VerifyStep>,
    /// Default timeout for steps that don't specify one
    #[serde(default = "default_verify_timeout")]
    pub default_timeout_secs: u64,
    /// Weight of semantic prediction in score blending [0.0, 1.0]. 0.0 disables.
    #[serde(default = "default_semantic_weight")]
    pub semantic_weight: f64,
    /// Prefer `cargo nextest run` over `cargo test` when nextest is installed.
    /// None means true (auto-detect). Set to false to always use cargo test.
    #[serde(default)]
    pub prefer_nextest: Option<bool>,
}

impl VerifyConfig {
    /// Returns the effective verification mode, defaulting to Auto if no steps exist,
    /// or Explicit if steps exist for backward compatibility.
    pub fn effective_mode(&self) -> VerifyMode {
        match &self.mode {
            Some(m) => m.clone(),
            None => {
                if self.steps.is_empty() {
                    VerifyMode::Auto
                } else {
                    VerifyMode::Explicit
                }
            }
        }
    }
}

fn default_semantic_weight() -> f64 {
    0.3
}

fn default_verify_timeout() -> u64 {
    300
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self {
            mode: None,
            steps: Vec::new(),
            default_timeout_secs: default_verify_timeout(),
            semantic_weight: default_semantic_weight(),
            prefer_nextest: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_config_defaults() {
        let config = VerifyConfig::default();
        assert_eq!(config.mode, None);
        assert!(config.steps.is_empty());
        assert_eq!(config.default_timeout_secs, 300);
        assert!((config.semantic_weight - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn test_verify_config_roundtrip() {
        // old empty configs (defaults to auto implicitly)
        let toml_str = "";
        let config: VerifyConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mode, None);

        // valid auto
        let toml_str = "mode = \"auto\"";
        let config: VerifyConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mode, Some(VerifyMode::Auto));
        let ser = toml::to_string(&config).unwrap();
        assert!(ser.contains("mode = \"auto\""));

        // valid explicit
        let toml_str = r#"
mode = "explicit"
[[steps]]
command = "cargo test"
description = "test"
"#;
        let config: VerifyConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mode, Some(VerifyMode::Explicit));
        assert_eq!(config.steps.len(), 1);

        // old explicit configs
        let toml_str = r#"
[[steps]]
command = "cargo test"
description = "test"
"#;
        let config: VerifyConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mode, None);
        assert_eq!(config.steps.len(), 1);
    }
}
