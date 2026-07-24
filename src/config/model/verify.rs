use crate::platform::process_policy::{
    ProcessPolicy, builtin_allowed_commands, merge_process_policy,
};
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
    /// When true, execute through a system shell. Requires
    /// `verify.allow_shell_steps = true` (default false) for config-declared
    /// steps. Interactive `ledgerful verify -c` is exempt from that gate.
    #[serde(default)]
    pub shell: bool,
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
    /// Additional commands to allow beyond the built-in toolchain allowlist.
    /// Extend-not-replace: adding an entry here does **not** drop cargo/npm/git.
    #[serde(default)]
    pub allowed_commands: Vec<String>,
    /// Commands to deny even if they appear on the built-in or allowed list.
    #[serde(default)]
    pub denied_commands: Vec<String>,
    /// When `None`, inherit default-strict (`true`). `Some(false)` is the
    /// documented footgun escape hatch that re-opens permissive empty-allowlist
    /// behaviour if the effective allowlist is cleared.
    #[serde(default)]
    pub strict: Option<bool>,
    /// Permit config-declared `shell = true` verify steps. Default false.
    /// Does **not** gate interactive `ledgerful verify -c`.
    #[serde(default)]
    pub allow_shell_steps: bool,
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

    /// Build the effective [`ProcessPolicy`] for verify/validator execution.
    ///
    /// Contract: `built_in ∪ allowed_commands − denied_commands`, with
    /// `strict = self.strict.unwrap_or(true)`. Adding one custom command must
    /// not wipe cargo/npm/git from the effective allowlist.
    pub fn effective_process_policy(&self) -> ProcessPolicy {
        merge_process_policy(
            &builtin_allowed_commands(),
            &self.allowed_commands,
            &self.denied_commands,
            self.strict,
            self.default_timeout_secs,
        )
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
            allowed_commands: Vec::new(),
            denied_commands: Vec::new(),
            strict: None,
            allow_shell_steps: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::process_policy::check_policy;

    #[test]
    fn test_verify_config_defaults() {
        let config = VerifyConfig::default();
        assert_eq!(config.mode, None);
        assert!(config.steps.is_empty());
        assert_eq!(config.default_timeout_secs, 300);
        assert!((config.semantic_weight - 0.3).abs() < f64::EPSILON);
        assert!(!config.allow_shell_steps);
        assert!(config.allowed_commands.is_empty());
        assert!(config.denied_commands.is_empty());
        assert_eq!(config.strict, None);
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
        assert!(!config.steps[0].shell);
    }

    #[test]
    fn test_verify_config_policy_fields_roundtrip() {
        let toml_str = r#"
allowed_commands = ["my-tool"]
denied_commands = ["curl"]
strict = false
allow_shell_steps = true

[[steps]]
description = "shell step"
command = "echo hi"
shell = true
"#;
        let config: VerifyConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.allowed_commands, vec!["my-tool".to_string()]);
        assert_eq!(config.denied_commands, vec!["curl".to_string()]);
        assert_eq!(config.strict, Some(false));
        assert!(config.allow_shell_steps);
        assert!(config.steps[0].shell);
    }

    #[test]
    fn effective_process_policy_extends_not_replaces() {
        let config = VerifyConfig {
            allowed_commands: vec!["my-tool".to_string()],
            default_timeout_secs: 90,
            ..Default::default()
        };
        let policy = config.effective_process_policy();
        assert!(check_policy("cargo", &policy).is_ok());
        assert!(check_policy("my-tool", &policy).is_ok());
        assert_eq!(policy.default_timeout_secs, 90);
        assert!(policy.strict);
    }
}
