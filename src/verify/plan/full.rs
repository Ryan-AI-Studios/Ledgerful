use super::{PlanSource, VerificationPlan, VerificationStep, VerifyScope};
use crate::config::model::VerifyConfig;
use crate::impact::packet::ImpactPacket;
use crate::policy::rules::Rules;
use crate::verify::predict::PredictedFile;
use crate::verify::timeouts::DEFAULT_AUTO_TIMEOUT_SECS;

/// Resolve the test command based on nextest availability.
///
/// When `prefer_nextest` is `None` (default) or `Some(true)`, probes for
/// `cargo nextest` on PATH and returns the nextest variant if found.
/// When `prefer_nextest` is `Some(false)`, always falls back to `cargo test`.
///
/// The nextest variant uses the `ci` profile so the pre-push/verify gate
/// respects the test-tier policy: it excludes `__slow` tests.
pub fn resolve_default_test_command(
    prefer_nextest: Option<bool>,
    repo_root: &std::path::Path,
) -> String {
    let use_nextest = match prefer_nextest {
        Some(false) => false,
        _ => crate::verify::engine::probe_nextest(),
    };
    if use_nextest {
        let nextest_config_content =
            std::fs::read_to_string(repo_root.join(".config/nextest.toml")).unwrap_or_default();

        // Use toml::from_str — str::parse::<toml::Value>() fails under toml 1.x on
        // multi-table nextest configs, which silently disabled profile detection.
        let has_ci = nextest_has_profile(&nextest_config_content, "ci");

        if has_ci {
            "cargo nextest run --workspace --all-features --profile ci".to_string()
        } else {
            "cargo nextest run --workspace --all-features".to_string()
        }
    } else {
        "cargo test --workspace --all-features".to_string()
    }
}

/// Resolve the doctest command used for full verification scope.
pub fn resolve_doctest_command() -> String {
    "cargo test --workspace --all-features --doc".to_string()
}

pub fn build_plan(
    packet: &ImpactPacket,
    rules: &Rules,
    predicted: &[PredictedFile],
    config: &VerifyConfig,
    profile: &crate::platform::repository::RepositoryProfile,
    repo_root: &std::path::Path,
) -> VerificationPlan {
    build_plan_with_scope(
        packet,
        rules,
        predicted,
        config,
        profile,
        VerifyScope::Full,
        repo_root,
    )
}

/// Internal build_plan that knows the requested scope so it can assemble the
/// correct tier commands for full verification. Fast scope still falls through
/// to the single default test command.
pub(crate) fn build_plan_with_scope(
    packet: &ImpactPacket,
    rules: &Rules,
    predicted: &[PredictedFile],
    config: &VerifyConfig,
    profile: &crate::platform::repository::RepositoryProfile,
    scope: VerifyScope,
    repo_root: &std::path::Path,
) -> VerificationPlan {
    let mut commands: Vec<String> = Vec::new();
    let mut predicted_steps: Vec<VerificationStep> = Vec::new();

    // Merge global required_verifications
    for cmd in &rules.global.required_verifications {
        commands.push(cmd.clone());
    }

    // Merge path-specific required_verifications from matching PathRule entries
    for override_rule in &rules.overrides {
        let glob = match globset::Glob::new(&override_rule.pattern) {
            Ok(g) => g,
            Err(_) => continue,
        };
        let compiled = match globset::GlobSet::builder().add(glob).build() {
            Ok(s) => s,
            Err(_) => continue,
        };

        let matches_any = packet.changes.iter().any(|f| compiled.is_match(&f.path));
        if matches_any {
            for cmd in &override_rule.required_verifications {
                commands.push(cmd.clone());
            }
        }

        // Check if any predicted file matches an override rule
        for p_file in predicted {
            if compiled.is_match(&p_file.path) {
                for cmd in &override_rule.required_verifications {
                    predicted_steps.push(VerificationStep {
                        command: cmd.clone(),
                        timeout_secs: DEFAULT_AUTO_TIMEOUT_SECS,
                        description: format!(
                            "Predicted impact ({}) on {}",
                            p_file.reason,
                            p_file.path.display()
                        ),
                        shell: false,
                    });
                }
            }
        }
    }

    // Deduplicate by exact command string for explicit rules
    commands.sort_unstable();
    commands.dedup();

    // Build initial steps
    let mut steps: Vec<VerificationStep> = if commands.is_empty() && predicted_steps.is_empty() {
        let auto_steps =
            crate::verify::auto_policy::build_auto_policy(profile, config, repo_root, scope);
        auto_steps
            .into_iter()
            .map(|step| VerificationStep {
                command: step.command,
                timeout_secs: step.timeout_secs.unwrap_or(DEFAULT_AUTO_TIMEOUT_SECS),
                description: step.description,
                shell: false,
            })
            .collect()
    } else {
        commands
            .into_iter()
            .map(|cmd| VerificationStep {
                command: cmd.clone(),
                timeout_secs: DEFAULT_AUTO_TIMEOUT_SECS,
                description: format!("From rules: {}", cmd),
                shell: false,
            })
            .collect()
    };

    // Add predicted steps
    steps.extend(predicted_steps);

    // Deduplicate all steps by command, merging descriptions for traceability
    steps.sort_unstable_by(|a, b| {
        a.command
            .cmp(&b.command)
            .then(a.description.cmp(&b.description))
    });

    let mut unique_steps: Vec<VerificationStep> = Vec::new();
    for step in steps {
        if let Some(existing) = unique_steps.iter_mut().find(|s| s.command == step.command) {
            if !existing.description.contains(&step.description) {
                existing.description.push_str(" | ");
                existing.description.push_str(&step.description);
            }
        } else {
            unique_steps.push(step);
        }
    }

    // For full scope with nextest, ensure the plan contains the complete tier
    // policy: ci profile (already present as the default), slow profile, and
    // doctests. This only applies when there were explicit rules/config that
    // prevented the default path from doing it.
    if scope == VerifyScope::Full {
        let has_rust = profile.rust.is_some();
        append_full_tier_commands(
            &mut unique_steps,
            config.prefer_nextest,
            has_rust,
            repo_root,
        );
    }

    let plan_source = if rules.was_legacy_default {
        PlanSource::HistoricalRulesFallback
    } else if config.effective_mode() == crate::config::model::VerifyMode::Auto {
        PlanSource::AutoPolicy
    } else {
        PlanSource::ExplicitConfig
    };

    VerificationPlan {
        source: Some(plan_source),
        steps: unique_steps,
        fallback_reason: None,
        refused: false,
    }
}

/// Parse nextest.toml content and report whether `[profile.<name>]` exists.
///
/// Prefer `toml::from_str` over `str::parse::<toml::Value>()`: under `toml` 1.x the
/// `FromStr` impl rejects multi-document / multi-table files that `from_str` accepts,
/// which previously left `has_ci` / `has_slow` permanently false.
pub(crate) fn nextest_has_profile(content: &str, profile_name: &str) -> bool {
    match toml::from_str::<toml::Value>(content) {
        Ok(parsed) => parsed
            .get("profile")
            .and_then(|p| p.get(profile_name))
            .is_some(),
        Err(_) => false,
    }
}

/// Ensures a full-scope plan contains the slow and doctest tier commands,
/// deduplicated against any commands already present.
pub(crate) fn append_full_tier_commands(
    steps: &mut Vec<VerificationStep>,
    prefer_nextest: Option<bool>,
    has_rust: bool,
    repo_root: &std::path::Path,
) {
    if !has_rust {
        return;
    }
    let use_nextest = match prefer_nextest {
        Some(false) => false,
        _ => crate::verify::engine::probe_nextest(),
    };
    let existing: std::collections::BTreeSet<String> =
        steps.iter().map(|s| s.command.clone()).collect();
    let mut extra: Vec<VerificationStep> = Vec::new();
    if use_nextest {
        let nextest_config_content =
            std::fs::read_to_string(repo_root.join(".config/nextest.toml")).unwrap_or_default();

        let has_slow = nextest_has_profile(&nextest_config_content, "slow");

        if has_slow {
            let cmd = "cargo nextest run --workspace --all-features --profile slow";
            if !existing.contains(cmd) {
                extra.push(VerificationStep {
                    command: cmd.to_string(),
                    timeout_secs: DEFAULT_AUTO_TIMEOUT_SECS,
                    description: "Tier: slow tests".to_string(),
                    shell: false,
                });
            }
        }

        let doctest = "cargo test --workspace --all-features --doc";
        if !existing.contains(doctest) {
            extra.push(VerificationStep {
                command: doctest.to_string(),
                timeout_secs: DEFAULT_AUTO_TIMEOUT_SECS,
                description: "Tier: doctests".to_string(),
                shell: false,
            });
        }
    } else {
        let fallback = "cargo test --workspace --all-features";
        if !existing.contains(fallback) && !existing.contains("cargo test") {
            extra.push(VerificationStep {
                command: fallback.to_string(),
                timeout_secs: DEFAULT_AUTO_TIMEOUT_SECS,
                description: "Fallback: full cargo test".to_string(),
                shell: false,
            });
        }
    }
    steps.extend(extra);
    // Re-sort deterministically after extending.
    steps.sort_unstable_by(|a, b| {
        a.command
            .cmp(&b.command)
            .then(a.description.cmp(&b.description))
    });
}
