use crate::config::error::ConfigError;
use crate::config::model::Config;
use crate::config::validate::validate_config;
use crate::state::layout::Layout;

pub use crate::config::plaintext_secret::doctor_plaintext_secret_findings;
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

/// Greppable 0243 absorb: warn and continue with defaults (do not hide history errors).
pub const CONFIG_LOAD_WARN: &str = "config load failed; using defaults";

pub fn load_config_or_default_warn(layout: &Layout) -> Config {
    match load_config(layout) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "{CONFIG_LOAD_WARN}");
            eprintln!("warning: {CONFIG_LOAD_WARN}");
            Config::default()
        }
    }
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
    config.doctor.normalize();

    // Sanitize verify steps: warn and filter invalid ones rather than failing hard
    sanitize_verify_steps(&mut config);

    // 0562: every config source (project, override, USERPROFILE, HOME,
    // built-in) converges here. The floor is additive and in-memory; it runs
    // before validation so user-supplied invalid globs still fail as today.
    crate::config::floor::apply_internal_state_ignore_floor(&mut config);

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
/// when clean. Severity is **warn** / category **migration** (0109).
pub fn doctor_config_findings(layout: &Layout) -> Vec<crate::doctor::finding::DoctorFinding> {
    use crate::doctor::finding::{DoctorCategory, DoctorFinding};

    let path = layout.config_file();
    if !path.exists() {
        return Vec::new();
    }

    match load_config_with_unknown_keys(layout) {
        Ok((_cfg, unknown)) if unknown.is_empty() => Vec::new(),
        Ok((_cfg, unknown)) => {
            let keys = unknown.join(", ");
            vec![DoctorFinding::warn(
                "legacy-config",
                DoctorCategory::Migration,
                format!(
                    "unknown or retired config key(s) ignored: {keys}. Review the project config in the Ledgerful state directory or run `ledgerful init` to refresh starter config (unknown keys are never a hard error)."
                ),
            )]
        }
        Err(e) => {
            let reason = load_error_reason(&e);
            vec![DoctorFinding::warn(
                "legacy-config",
                DoctorCategory::Migration,
                format!(
                    "Failed to load project config in the Ledgerful state directory ({reason}). Using defaults at runtime; review the project config and rerun ledgerful doctor --json."
                ),
            )]
        }
    }
}

/// Fixed reason token for a project-config load failure.
///
/// Matches typed [`ConfigError`] variants only; `Display` text is never
/// inspected, so a non-config report that happens to mention "parse" is
/// classified as `other`.
fn load_error_reason(report: &miette::Report) -> &'static str {
    match report.downcast_ref::<ConfigError>() {
        Some(ConfigError::ReadFailed { .. }) => "read",
        Some(ConfigError::ParseFailed { .. }) => "parse",
        Some(ConfigError::ValidationFailed { .. }) => "validation",
        _ => "other",
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
            findings
                .iter()
                .any(|f| f.code == "legacy-config" || f.message.contains("legacy-config")),
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

    #[test]
    fn hotspots_config_load_failure_warns() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        fs::write(layout.config_file(), "not = [ valid toml").unwrap();
        assert!(load_config(&layout).is_err());
        let config = load_config_or_default_warn(&layout);
        assert_eq!(
            config.hotspots.history_budget_secs,
            Config::default().hotspots.history_budget_secs
        );
        assert!(
            CONFIG_LOAD_WARN.contains("config load"),
            "warn string must stay greppable: {CONFIG_LOAD_WARN}"
        );
    }

    fn project_layout(contents: Option<&str>) -> (tempfile::TempDir, Layout) {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        if let Some(contents) = contents {
            fs::write(layout.config_file(), contents).unwrap();
        }
        (tmp, layout)
    }

    fn legacy_message(layout: &Layout) -> String {
        let findings = doctor_config_findings(layout);
        assert_eq!(
            findings.len(),
            1,
            "expected one legacy-config: {findings:?}"
        );
        findings[0].message.clone()
    }

    #[test]
    fn legacy_config_load_failure_messages_are_exact_per_reason() {
        let (_parse_tmp, parse_layout) = project_layout(Some("not = [ valid toml"));
        assert_eq!(
            legacy_message(&parse_layout),
            "Failed to load project config in the Ledgerful state directory (parse). Using defaults at runtime; review the project config and rerun ledgerful doctor --json."
        );

        let (_read_tmp, read_layout) = project_layout(None);
        fs::create_dir(read_layout.config_file()).unwrap();
        assert_eq!(
            legacy_message(&read_layout),
            "Failed to load project config in the Ledgerful state directory (read). Using defaults at runtime; review the project config and rerun ledgerful doctor --json."
        );

        let (_val_tmp, val_layout) = project_layout(Some("[watch]\ndebounce_ms = 0\n"));
        assert_eq!(
            legacy_message(&val_layout),
            "Failed to load project config in the Ledgerful state directory (validation). Using defaults at runtime; review the project config and rerun ledgerful doctor --json."
        );
    }

    #[test]
    fn legacy_config_reason_uses_typed_downcast_not_display_text() {
        // Typed variants map to their tokens.
        let read = miette::Report::new(ConfigError::ReadFailed {
            path: "p".to_string(),
            source: std::io::Error::other("x"),
        });
        assert_eq!(load_error_reason(&read), "read");
        let parse = miette::Report::new(ConfigError::ParseFailed {
            source: toml::from_str::<toml::Table>("x = [").unwrap_err(),
        });
        assert_eq!(load_error_reason(&parse), "parse");
        let validation = miette::Report::new(ConfigError::ValidationFailed {
            reason: "r".to_string(),
        });
        assert_eq!(load_error_reason(&validation), "validation");
        // A non-ConfigError whose text mentions parse/read is `other`.
        let misleading = miette::miette!("parse failed while read of a file");
        assert_eq!(load_error_reason(&misleading), "other");
    }

    #[test]
    fn legacy_config_unknown_keys_symbolic_source() {
        let (tmp, layout) = project_layout(Some("[core]\nstrict = false\nzz_unknown_marker = 1\n"));
        let message = legacy_message(&layout);
        assert!(
            message.contains("zz_unknown_marker"),
            "legacy key semantics retained: {message}"
        );
        assert!(
            message.contains("project config in the Ledgerful state directory"),
            "symbolic source: {message}"
        );
        let resolved = tmp.path().to_string_lossy().into_owned();
        assert!(
            !message.contains(&resolved),
            "resolved path leaked: {message}"
        );
    }

    fn floor_entries() -> Vec<String> {
        crate::config::floor::internal_state_ignore_entries().to_vec()
    }

    #[test]
    fn stale_template_list_gets_internal_state_floor() {
        let (config, unknown) = finalize_config(
            "[watch]\nignore_patterns = [\"target\", \"dist/**\"]\n".to_string(),
            Vec::new(),
        )
        .expect("stale template parses");
        assert!(unknown.is_empty());
        let patterns = config.watch.ignore_patterns;
        assert_eq!(&patterns[..2], ["target", "dist/**"]);
        assert_eq!(&patterns[2..], floor_entries().as_slice());
    }

    #[test]
    fn stale_project_config_list_gets_internal_state_floor() {
        let (_tmp, layout) = project_layout(Some("[watch]\nignore_patterns = [\"target\"]\n"));
        let config = load_config(&layout).expect("project config loads");
        let patterns = config.watch.ignore_patterns;
        assert_eq!(patterns[0], "target");
        assert_eq!(&patterns[1..], floor_entries().as_slice());
    }

    #[test]
    fn shared_worktree_project_config_gets_internal_state_floor() {
        let tmp = tempdir().unwrap();
        let base = Utf8Path::from_path(tmp.path()).unwrap();
        let work_root = base.join("worktree");
        let shared_state = base.join("main-state");
        fs::create_dir_all(&work_root).unwrap();
        fs::create_dir_all(&shared_state).unwrap();
        fs::write(
            shared_state.join("config.toml"),
            "[watch]\nignore_patterns = [\"target\"]\n",
        )
        .unwrap();

        let layout = Layout::from_roots(&work_root, &shared_state);
        let config = load_config(&layout).expect("shared project config loads");
        let patterns = config.watch.ignore_patterns;
        assert_eq!(patterns[0], "target");
        assert_eq!(&patterns[1..], floor_entries().as_slice());
    }

    #[test]
    fn empty_watch_ignore_list_gets_internal_state_floor() {
        let (config, _) =
            finalize_config("[watch]\nignore_patterns = []\n".to_string(), Vec::new())
                .expect("empty list parses");
        assert_eq!(config.watch.ignore_patterns, floor_entries());
    }

    #[test]
    fn builtin_default_ignore_list_is_not_reordered() {
        use crate::config::defaults::DEFAULT_CONFIG;

        let parsed: Config = toml::from_str(DEFAULT_CONFIG).expect("built-in TOML");
        let builtin = parsed.watch.ignore_patterns;
        assert_eq!(builtin.len(), 16);

        let (config, _) = finalize_config(DEFAULT_CONFIG.to_string(), Vec::new()).expect("built-in");
        assert_eq!(&config.watch.ignore_patterns[..16], builtin.as_slice());
        assert_eq!(
            config.watch.ignore_patterns,
            Config::default().watch.ignore_patterns,
            "the floored built-in equals the serde field default"
        );
    }

    #[test]
    fn malformed_template_content_still_returns_parse_error() {
        let error = finalize_config("not = [ valid".to_string(), Vec::new())
            .expect_err("malformed content must fail");
        assert!(
            matches!(
                error.downcast_ref::<ConfigError>(),
                Some(ConfigError::ParseFailed { .. })
            ),
            "{error:?}"
        );
    }

    #[test]
    fn invalid_glob_still_fails_validation_after_floor() {
        let error = finalize_config(
            "[watch]\nignore_patterns = [\"[\"]\n".to_string(),
            Vec::new(),
        )
        .expect_err("an invalid user glob must still fail");
        assert!(
            matches!(
                error.downcast_ref::<ConfigError>(),
                Some(ConfigError::ValidationFailed { .. })
            ),
            "floor must not mask a user error: {error:?}"
        );
    }
}
