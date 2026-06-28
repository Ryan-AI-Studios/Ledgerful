use crate::config::error::ConfigError;
use crate::config::model::{Config, GeminiConfig};
use miette::Result;

/// Validates the configuration, returning an error for invalid values.
pub fn validate_config(config: &Config) -> Result<()> {
    if config.watch.debounce_ms == 0 {
        return Err(ConfigError::ValidationFailed {
            reason: "watch.debounce_ms must be > 0".to_string(),
        }
        .into());
    }

    if let Some(0) = config.gemini.timeout_secs {
        return Err(ConfigError::ValidationFailed {
            reason: "gemini.timeout_secs must be > 0".to_string(),
        }
        .into());
    }

    validate_optional_model(&config.gemini, "model")?;
    validate_optional_model(&config.gemini, "fast_model")?;
    validate_optional_model(&config.gemini, "deep_model")?;

    for pattern in &config.watch.ignore_patterns {
        if globset::Glob::new(pattern).is_err() {
            return Err(ConfigError::ValidationFailed {
                reason: format!("watch.ignore_patterns contains invalid glob: '{}'", pattern),
            }
            .into());
        }
    }

    // Validate temporal config
    if config.temporal.min_shared_commits == 0 {
        return Err(ConfigError::ValidationFailed {
            reason: "temporal.min_shared_commits must be > 0".to_string(),
        }
        .into());
    }
    if config.temporal.min_revisions == 0 {
        return Err(ConfigError::ValidationFailed {
            reason: "temporal.min_revisions must be > 0".to_string(),
        }
        .into());
    }

    // Validate verify steps
    for (i, step) in config.verify.steps.iter().enumerate() {
        if step.command.trim().is_empty() {
            return Err(ConfigError::ValidationFailed {
                reason: format!("verify.steps[{}] has empty command", i),
            }
            .into());
        }
        if step.timeout_secs == Some(0) {
            return Err(ConfigError::ValidationFailed {
                reason: format!("verify.steps[{}] timeout_secs must be > 0", i),
            }
            .into());
        }
    }
    if config.verify.default_timeout_secs == 0 && !config.verify.steps.is_empty() {
        return Err(ConfigError::ValidationFailed {
            reason: "verify.default_timeout_secs must be > 0 when steps are defined".to_string(),
        }
        .into());
    }

    match (&config.verify.mode, config.verify.steps.is_empty()) {
        (Some(crate::config::model::VerifyMode::Auto), false) => {
            return Err(ConfigError::ValidationFailed {
                reason: "verify.mode = \"auto\" is invalid with non-empty steps. Silently ignoring steps is unsafe.".to_string(),
            }.into());
        }
        (Some(crate::config::model::VerifyMode::Explicit), true) => {
            return Err(ConfigError::ValidationFailed {
                reason: "verify.mode = \"explicit\" is invalid with empty steps. This would create a gate that performs no checks.".to_string(),
            }.into());
        }
        _ => {}
    }

    if config.verify.semantic_weight < 0.0 || config.verify.semantic_weight > 1.0 {
        return Err(ConfigError::ValidationFailed {
            reason: format!(
                "verify.semantic_weight must be in [0.0, 1.0], got {}",
                config.verify.semantic_weight
            ),
        }
        .into());
    }

    if config.semantic.hnsw_rebuild_threshold == Some(0) {
        return Err(ConfigError::ValidationFailed {
            reason: "semantic.hnsw_rebuild_threshold must be > 0".to_string(),
        }
        .into());
    }

    if config.semantic.concurrency == Some(0) {
        return Err(ConfigError::ValidationFailed {
            reason: "semantic.concurrency must be > 0".to_string(),
        }
        .into());
    }

    if config.semantic.parse_concurrency == Some(0) {
        return Err(ConfigError::ValidationFailed {
            reason: "semantic.parse_concurrency must be > 0".to_string(),
        }
        .into());
    }

    if config.semantic.embed_concurrency == Some(0) {
        return Err(ConfigError::ValidationFailed {
            reason: "semantic.embed_concurrency must be > 0".to_string(),
        }
        .into());
    }

    if config.semantic.embed_concurrency_cap == Some(0) {
        return Err(ConfigError::ValidationFailed {
            reason: "semantic.embed_concurrency_cap must be > 0".to_string(),
        }
        .into());
    }

    if config.coverage.max_reachability_depth == 0 {
        return Err(ConfigError::ValidationFailed {
            reason: "coverage.max_reachability_depth must be > 0".to_string(),
        }
        .into());
    }
    if config.coverage.max_reachability_depth > 10 {
        return Err(ConfigError::ValidationFailed {
            reason: "coverage.max_reachability_depth must be <= 10".to_string(),
        }
        .into());
    }

    Ok(())
}

fn validate_optional_model(config: &GeminiConfig, field: &str) -> Result<()> {
    let value = match field {
        "model" => &config.model,
        "fast_model" => &config.fast_model,
        "deep_model" => &config.deep_model,
        _ => return Ok(()),
    };

    if let Some(model) = value
        && model.trim().is_empty()
    {
        return Err(ConfigError::ValidationFailed {
            reason: format!("gemini.{field} must be non-empty if present"),
        }
        .into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::*;
    use rstest::rstest;

    // Helpers used by #[case] parameters to avoid duplicating config
    // construction across parameterized validation tests.
    fn zero_debounce_config() -> Config {
        Config {
            watch: WatchConfig {
                debounce_ms: 0,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn zero_timeout_secs_config() -> Config {
        Config {
            gemini: GeminiConfig {
                timeout_secs: Some(0),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn empty_model_config() -> Config {
        Config {
            gemini: GeminiConfig {
                model: Some("   ".to_string()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn valid_model_config() -> Config {
        Config {
            gemini: GeminiConfig {
                model: Some("gemini-pro".to_string()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn empty_routed_model_config() -> Config {
        Config {
            gemini: GeminiConfig {
                fast_model: Some("   ".to_string()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn invalid_glob_pattern_config() -> Config {
        Config {
            watch: WatchConfig {
                ignore_patterns: vec!["valid/**".to_string(), "[invalid".to_string()],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn none_timeout_config() -> Config {
        Config {
            gemini: GeminiConfig {
                timeout_secs: None,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn none_model_config() -> Config {
        Config {
            gemini: GeminiConfig {
                model: None,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn verify_empty_steps_config() -> Config {
        Config {
            verify: VerifyConfig {
                mode: None,
                steps: vec![],
                default_timeout_secs: 300,
                semantic_weight: 0.3,
                prefer_nextest: None,
            },
            ..Default::default()
        }
    }

    fn verify_empty_command_config() -> Config {
        Config {
            verify: VerifyConfig {
                mode: None,
                steps: vec![VerifyStep {
                    description: "Missing command".to_string(),
                    command: "   ".to_string(),
                    timeout_secs: Some(60),
                }],
                default_timeout_secs: 300,
                semantic_weight: 0.3,
                prefer_nextest: None,
            },
            ..Default::default()
        }
    }

    fn verify_zero_timeout_step_config() -> Config {
        Config {
            verify: VerifyConfig {
                mode: None,
                steps: vec![VerifyStep {
                    description: "Bad timeout".to_string(),
                    command: "cargo test".to_string(),
                    timeout_secs: Some(0),
                }],
                default_timeout_secs: 300,
                semantic_weight: 0.3,
                prefer_nextest: None,
            },
            ..Default::default()
        }
    }

    fn verify_zero_default_timeout_config() -> Config {
        Config {
            verify: VerifyConfig {
                mode: None,
                steps: vec![VerifyStep {
                    description: "Run tests".to_string(),
                    command: "cargo test".to_string(),
                    timeout_secs: Some(60),
                }],
                default_timeout_secs: 0,
                semantic_weight: 0.3,
                prefer_nextest: None,
            },
            ..Default::default()
        }
    }

    fn temporal_zero_min_shared_commits_config() -> Config {
        Config {
            temporal: TemporalConfig {
                min_shared_commits: 0,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn temporal_zero_min_revisions_config() -> Config {
        Config {
            temporal: TemporalConfig {
                min_revisions: 0,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn semantic_weight_out_of_range_config() -> Config {
        Config {
            verify: VerifyConfig {
                semantic_weight: 1.5,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn semantic_weight_in_range_config() -> Config {
        Config {
            verify: VerifyConfig {
                semantic_weight: 0.5,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn zero_hnsw_rebuild_threshold_config() -> Config {
        Config {
            semantic: SemanticConfig {
                hnsw_rebuild_threshold: Some(0),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn zero_semantic_concurrency_config() -> Config {
        Config {
            semantic: SemanticConfig {
                hnsw_rebuild_threshold: None,
                concurrency: Some(0),
                parse_concurrency: None,
                embed_concurrency: None,
                embed_concurrency_cap: None,
            },
            ..Default::default()
        }
    }

    fn zero_parse_concurrency_config() -> Config {
        Config {
            semantic: SemanticConfig {
                hnsw_rebuild_threshold: None,
                concurrency: None,
                parse_concurrency: Some(0),
                embed_concurrency: None,
                embed_concurrency_cap: None,
            },
            ..Default::default()
        }
    }

    fn zero_embed_concurrency_config() -> Config {
        Config {
            semantic: SemanticConfig {
                hnsw_rebuild_threshold: None,
                concurrency: None,
                parse_concurrency: None,
                embed_concurrency: Some(0),
                embed_concurrency_cap: None,
            },
            ..Default::default()
        }
    }

    fn zero_embed_concurrency_cap_config() -> Config {
        Config {
            semantic: SemanticConfig {
                hnsw_rebuild_threshold: None,
                concurrency: None,
                parse_concurrency: None,
                embed_concurrency: None,
                embed_concurrency_cap: Some(0),
            },
            ..Default::default()
        }
    }

    fn zero_max_reachability_depth_config() -> Config {
        Config {
            coverage: CoverageConfig {
                max_reachability_depth: 0,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn too_high_max_reachability_depth_config() -> Config {
        Config {
            coverage: CoverageConfig {
                max_reachability_depth: 11,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn verify_mode_auto_with_steps_config() -> Config {
        Config {
            verify: VerifyConfig {
                mode: Some(VerifyMode::Auto),
                steps: vec![VerifyStep {
                    description: "Test".to_string(),
                    command: "cargo test".to_string(),
                    timeout_secs: None,
                }],
                default_timeout_secs: 300,
                semantic_weight: 0.3,
                prefer_nextest: None,
            },
            ..Default::default()
        }
    }

    fn verify_mode_explicit_no_steps_config() -> Config {
        Config {
            verify: VerifyConfig {
                mode: Some(VerifyMode::Explicit),
                steps: vec![],
                default_timeout_secs: 300,
                semantic_weight: 0.3,
                prefer_nextest: None,
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_valid_default_config() {
        let config = Config::default();
        let result = validate_config(&config);
        assert!(result.is_ok(), "default config should validate: {result:?}");
    }

    #[rstest]
    #[case::zero_debounce_ms(zero_debounce_config(), "debounce_ms")]
    #[case::zero_timeout_secs(zero_timeout_secs_config(), "timeout_secs")]
    #[case::empty_model(empty_model_config(), "model")]
    #[case::empty_routed_model(empty_routed_model_config(), "fast_model")]
    #[case::invalid_glob_pattern(invalid_glob_pattern_config(), "invalid glob")]
    #[case::verify_empty_command(verify_empty_command_config(), "verify.steps[0]")]
    #[case::verify_empty_command_detail(verify_empty_command_config(), "empty command")]
    #[case::verify_zero_timeout_step(verify_zero_timeout_step_config(), "verify.steps[0]")]
    #[case::verify_zero_timeout_step_detail(
        verify_zero_timeout_step_config(),
        "timeout_secs must be > 0"
    )]
    #[case::verify_zero_default_timeout(
        verify_zero_default_timeout_config(),
        "default_timeout_secs"
    )]
    #[case::temporal_zero_min_shared_commits(
        temporal_zero_min_shared_commits_config(),
        "min_shared_commits"
    )]
    #[case::temporal_zero_min_revisions(temporal_zero_min_revisions_config(), "min_revisions")]
    #[case::semantic_weight_out_of_range(semantic_weight_out_of_range_config(), "semantic_weight")]
    #[case::zero_hnsw_rebuild_threshold(
        zero_hnsw_rebuild_threshold_config(),
        "hnsw_rebuild_threshold"
    )]
    #[case::zero_semantic_concurrency(zero_semantic_concurrency_config(), "semantic.concurrency")]
    #[case::zero_parse_concurrency(zero_parse_concurrency_config(), "semantic.parse_concurrency")]
    #[case::zero_embed_concurrency(zero_embed_concurrency_config(), "semantic.embed_concurrency")]
    #[case::zero_embed_concurrency_cap(
        zero_embed_concurrency_cap_config(),
        "semantic.embed_concurrency_cap"
    )]
    #[case::zero_max_reachability_depth(
        zero_max_reachability_depth_config(),
        "max_reachability_depth must be > 0"
    )]
    #[case::too_high_max_reachability_depth(
        too_high_max_reachability_depth_config(),
        "max_reachability_depth must be <= 10"
    )]
    #[case::verify_auto_with_steps(verify_mode_auto_with_steps_config(), "invalid with non-empty")]
    #[case::verify_explicit_no_steps(verify_mode_explicit_no_steps_config(), "invalid with empty")]
    fn config_validation_error_cases(#[case] config: Config, #[case] expected_err: &str) {
        let result = validate_config(&config);
        assert!(result.is_err(), "expected error containing {expected_err}");
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains(expected_err),
            "expected error to contain {expected_err}: {msg}"
        );
    }

    #[rstest]
    #[case::valid_model(valid_model_config())]
    #[case::none_timeout(none_timeout_config())]
    #[case::none_model(none_model_config())]
    #[case::verify_empty_steps(verify_empty_steps_config())]
    #[case::semantic_weight_in_range(semantic_weight_in_range_config())]
    fn config_validation_ok_cases(#[case] config: Config) {
        let result = validate_config(&config);
        assert!(result.is_ok(), "expected config to validate: {result:?}");
    }
}
