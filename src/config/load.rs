use crate::config::error::ConfigError;
use crate::config::model::Config;
use crate::config::validate::validate_config;
use crate::state::layout::Layout;
use miette::Result;
use std::fs;
use tracing::warn;

/// Loads the configuration from the workspace root.
/// If the configuration file does not exist, it returns the default configuration.
///
/// Unknown keys are silently dropped at the serde layer (no `deny_unknown_fields` —
/// 0094 deliberately keeps old configs loadable). Call
/// [`load_config_with_unknown_keys`] when the caller needs the ignored-key list
/// for doctor reporting.
pub fn load_config(layout: &Layout) -> Result<Config> {
    let (config, _unknown) = load_config_with_unknown_keys(layout)?;
    Ok(config)
}

/// Like [`load_config`], but also returns sorted unknown/ignored key paths
/// discovered via `serde_ignored` (DoD-8). Empty when the file is missing or
/// fully understood.
///
/// **Do not** add `#[serde(deny_unknown_fields)]` on `Config`: that converts a
/// doctor warning into a hard failure for every user holding an old config.
pub fn load_config_with_unknown_keys(layout: &Layout) -> Result<(Config, Vec<String>)> {
    let path = layout.config_file();

    let content = if path.exists() {
        fs::read_to_string(&path).map_err(|e| ConfigError::ReadFailed {
            path: path.to_string(),
            source: e,
        })?
    } else {
        return finalize_config(
            crate::config::defaults::default_config_contents().map_err(|e| {
                ConfigError::ReadFailed {
                    path: "global default config".to_string(),
                    source: std::io::Error::other(e.to_string()),
                }
            })?,
            Vec::new(),
        );
    };

    let mut unknown: Vec<String> = Vec::new();
    let deserializer =
        toml::Deserializer::parse(&content).map_err(|e| ConfigError::ParseFailed { source: e })?;
    let config: Config = serde_ignored::deserialize(deserializer, |path| {
        unknown.push(path.to_string());
    })
    .map_err(|e| ConfigError::ParseFailed { source: e })?;

    unknown.sort();
    unknown.dedup();

    finalize_loaded(config, unknown)
}

fn finalize_config(content: String, unknown: Vec<String>) -> Result<(Config, Vec<String>)> {
    let config: Config =
        toml::from_str(&content).map_err(|e| ConfigError::ParseFailed { source: e })?;
    finalize_loaded(config, unknown)
}

fn finalize_loaded(mut config: Config, unknown: Vec<String>) -> Result<(Config, Vec<String>)> {
    // Apply environment variable overrides and resolve model settings
    config.local_model = crate::config::model::resolve_local_model_config(&config.local_model);
    config.bridge = crate::config::model::resolve_bridge_config(&config.bridge);

    // Sanitize verify steps: warn and filter invalid ones rather than failing hard
    sanitize_verify_steps(&mut config);

    validate_config(&config)?;

    Ok((config, unknown))
}

/// Removes invalid verify steps with warnings rather than failing the entire config load.
fn sanitize_verify_steps(config: &mut Config) {
    let original_len = config.verify.steps.len();
    if original_len == 0 {
        return;
    }

    config.verify.steps.retain(|step| {
        if step.command.trim().is_empty() {
            warn!(
                "Skipping verify step with empty command: '{}'",
                step.description
            );
            false
        } else if step.timeout_secs == Some(0) {
            warn!(
                "Skipping verify step '{}' with zero timeout (use default_timeout_secs or set > 0)",
                step.description
            );
            false
        } else {
            true
        }
    });

    let removed = original_len - config.verify.steps.len();
    if removed > 0 {
        warn!("Removed {} invalid verify step(s) from config", removed);
    }
}

/// Doctor-facing findings for config staleness / unknown keys. Sorted, empty
/// when clean. Severity is WARNING (not CRITICAL).
pub fn doctor_config_findings(layout: &Layout) -> Vec<String> {
    let path = layout.config_file();
    if !path.exists() {
        return Vec::new();
    }

    match load_config_with_unknown_keys(layout) {
        Ok((_cfg, unknown)) if unknown.is_empty() => Vec::new(),
        Ok((_cfg, unknown)) => {
            let keys = unknown.join(", ");
            vec![format!(
                "Warning [legacy-config]: unknown or retired config key(s) ignored: {keys}. Review `{path}` or run `ledgerful init` to refresh starter config (unknown keys are never a hard error)."
            )]
        }
        Err(e) => {
            vec![format!(
                "Warning [legacy-config]: failed to parse `{path}`: {e}. Using defaults at runtime; fix the file or run `ledgerful init --force` carefully."
            )]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use tempfile::tempdir;

    #[test]
    fn test_load_default_config_if_missing() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);

        let config = load_config(&layout).unwrap();
        assert!(!config.core.strict);
    }

    #[test]
    fn test_load_custom_config() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();

        let config_path = layout.config_file();
        fs::write(config_path, "[core]\nstrict = true").unwrap();

        let config = load_config(&layout).unwrap();
        assert!(config.core.strict);
    }

    #[test]
    fn test_sanitize_removes_empty_command_step() {
        let mut config = Config::default();
        config.verify.steps.push(crate::config::model::VerifyStep {
            description: "Missing command".to_string(),
            command: "   ".to_string(),
            timeout_secs: Some(60),
            shell: false,
        });
        config.verify.steps.push(crate::config::model::VerifyStep {
            description: "Valid step".to_string(),
            command: "cargo test".to_string(),
            timeout_secs: Some(60),
            shell: false,
        });

        sanitize_verify_steps(&mut config);

        assert_eq!(config.verify.steps.len(), 1);
        assert_eq!(config.verify.steps[0].description, "Valid step");
    }

    #[test]
    fn test_sanitize_removes_zero_timeout_step() {
        let mut config = Config::default();
        config.verify.steps.push(crate::config::model::VerifyStep {
            description: "Bad timeout".to_string(),
            command: "cargo test".to_string(),
            timeout_secs: Some(0),
            shell: false,
        });
        config.verify.steps.push(crate::config::model::VerifyStep {
            description: "Good step".to_string(),
            command: "cargo fmt --check".to_string(),
            timeout_secs: Some(60),
            shell: false,
        });

        sanitize_verify_steps(&mut config);

        assert_eq!(config.verify.steps.len(), 1);
        assert_eq!(config.verify.steps[0].description, "Good step");
    }

    #[test]
    fn test_sanitize_keeps_valid_steps() {
        let mut config = Config::default();
        config.verify.steps.push(crate::config::model::VerifyStep {
            description: "Run tests".to_string(),
            command: "cargo test".to_string(),
            timeout_secs: Some(60),
            shell: false,
        });
        config.verify.steps.push(crate::config::model::VerifyStep {
            description: "Check formatting".to_string(),
            command: "cargo fmt --check".to_string(),
            timeout_secs: Some(300),
            shell: false,
        });

        sanitize_verify_steps(&mut config);

        assert_eq!(config.verify.steps.len(), 2);
    }

    /// DoD-8: unknown keys are reported, not hard-failed.
    #[test]
    fn unknown_keys_collected_without_deny() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        // Schema-era sections that no longer exist (Design-shaped).
        fs::write(
            layout.config_file(),
            r#"
[core]
strict = true

[retired_section]
totally_unknown_key = true

[core.also_unknown_nested]
x = 1
"#,
        )
        .unwrap();

        let (config, unknown) = load_config_with_unknown_keys(&layout).unwrap();
        assert!(config.core.strict);
        // Parse must succeed (no deny_unknown_fields) and unknown keys surface.
        assert!(
            !unknown.is_empty(),
            "expected unknown keys from retired_section; got {unknown:?}"
        );
        let findings = doctor_config_findings(&layout);
        assert!(
            findings.iter().any(|f| f.contains("legacy-config")),
            "doctor must name remediation: {findings:?}"
        );
    }

    #[test]
    fn malformed_config_returns_parse_error() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        fs::write(layout.config_file(), "not = [ valid toml").unwrap();
        assert!(load_config(&layout).is_err());
    }
}
