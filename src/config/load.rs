use crate::config::error::ConfigError;
use crate::config::model::Config;
use crate::config::validate::validate_config;
use crate::state::layout::Layout;
use miette::Result;
use std::fs;
use tracing::warn;

/// Loads the configuration from the workspace root.
/// If the configuration file does not exist, it returns the default configuration.
pub fn load_config(layout: &Layout) -> Result<Config> {
    let path = layout.config_file();

    let content = if path.exists() {
        fs::read_to_string(&path).map_err(|e| ConfigError::ReadFailed {
            path: path.to_string(),
            source: e,
        })?
    } else {
        crate::config::defaults::default_config_contents().map_err(|e| ConfigError::ReadFailed {
            path: "global default config".to_string(),
            source: std::io::Error::other(e.to_string()),
        })?
    };

    let mut config: Config =
        toml::from_str(&content).map_err(|e| ConfigError::ParseFailed { source: e })?;

    // Apply environment variable overrides and resolve model settings
    config.local_model = crate::config::model::resolve_local_model_config(&config.local_model);

    // Sanitize verify steps: warn and filter invalid ones rather than failing hard
    sanitize_verify_steps(&mut config);

    validate_config(&config)?;

    Ok(config)
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
        });
        config.verify.steps.push(crate::config::model::VerifyStep {
            description: "Valid step".to_string(),
            command: "cargo test".to_string(),
            timeout_secs: Some(60),
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
        });
        config.verify.steps.push(crate::config::model::VerifyStep {
            description: "Good step".to_string(),
            command: "cargo fmt --check".to_string(),
            timeout_secs: Some(60),
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
        });
        config.verify.steps.push(crate::config::model::VerifyStep {
            description: "Check formatting".to_string(),
            command: "cargo fmt --check".to_string(),
            timeout_secs: Some(300),
        });

        sanitize_verify_steps(&mut config);

        assert_eq!(config.verify.steps.len(), 2);
    }
}
