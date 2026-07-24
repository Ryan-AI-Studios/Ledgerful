use crate::config::model::VerifyConfig;
use crate::impact::packet::ImpactPacket;
use crate::policy::rules::Rules;
use crate::verify::predict::PredictedFile;
use crate::verify::timeouts::DEFAULT_AUTO_TIMEOUT_SECS;
use serde::{Deserialize, Serialize};

/// Controls how broadly `ledgerful verify` selects tests.
///
/// `Fast` (the pre-push hook default) uses `test_mapping` to run only the
/// tests that cover the changed files, falling back to the full suite when
/// the diff touches shared infrastructure or the mapping is empty.
///
/// `Full` (the manual `ledgerful verify` default, and CI) runs the entire
/// suite regardless of scope — the safe backstop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VerifyScope {
    /// Scoped test selection via `test_mapping` (Tier 1). Falls back to full
    /// suite when shared infrastructure is touched or the mapping is empty.
    Fast,
    /// Full suite — no scoping. Always used by CI.
    #[default]
    Full,
}

impl VerifyScope {
    pub fn is_fast(self) -> bool {
        matches!(self, Self::Fast)
    }
}

impl std::fmt::Display for VerifyScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fast => write!(f, "fast"),
            Self::Full => write!(f, "full"),
        }
    }
}

impl std::str::FromStr for VerifyScope {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "fast" => Ok(Self::Fast),
            "full" => Ok(Self::Full),
            other => Err(format!(
                "unknown verify scope '{}', expected 'fast' or 'full'",
                other
            )),
        }
    }
}

/// Path patterns that identify shared infrastructure. When any changed file
/// matches one of these, scoped selection is skipped and the full suite runs,
/// because these files can break anything in the project.
const SHARED_INFRA_PATTERNS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "src/cli/args.rs",
    "src/cli/dispatch.rs",
    "src/cli/mod.rs",
    "src/config/**",
    "src/state/migrations/**",
    "src/state/migrations.rs",
    "src/state/storage/**",
    "src/state/storage_cozo.rs",
    ".ledgerful/**",
    ".github/workflows/**",
    "build.rs",
];

/// Returns true if any changed file in the packet matches a shared
/// infrastructure pattern, meaning the full suite must run.
fn touches_shared_infra(packet: &ImpactPacket) -> bool {
    let matchers: Vec<globset::GlobMatcher> = SHARED_INFRA_PATTERNS
        .iter()
        .filter_map(|p| globset::Glob::new(p).ok())
        .map(|g| g.compile_matcher())
        .collect();
    packet.changes.iter().any(|f| {
        let path_str = f.path.to_string_lossy().replace('\\', "/");
        matchers.iter().any(|m| m.is_match(&path_str))
    })
}

/// Query `test_mapping` for the test files that cover the changed source
/// files. Returns a sorted, deduplicated list of test file stems suitable for
/// nextest filterset predicates (e.g. `cli_scan` from
/// `tests/integration/cli_scan.rs`).
///
/// Returns `None` (meaning "fall back to full suite") when:
/// - the connection is not available
/// - the `test_mapping` table doesn't exist or is empty
/// - no mappings are found for any changed file
fn query_scoped_test_files(
    conn: &rusqlite::Connection,
    packet: &ImpactPacket,
) -> Option<Vec<String>> {
    let total: i64 = conn
        .query_row("SELECT count(*) FROM test_mapping", [], |row| row.get(0))
        .unwrap_or(0);
    if total == 0 {
        return None;
    }

    // Collect the file_path of every test file that covers any changed file.
    let mut test_files: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for change in &packet.changes {
        let path_str = change.path.to_string_lossy().replace('\\', "/");
        // Resolve the changed file to its project_files id, then query
        // test_mapping for covering test files.
        let file_id: Option<i64> = conn
            .query_row(
                "SELECT id FROM project_files WHERE file_path = ?1",
                [&path_str],
                |row| row.get(0),
            )
            .ok();
        let Some(fid) = file_id else {
            continue;
        };
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT pf.file_path \
                 FROM test_mapping tm \
                 JOIN project_files pf ON tm.test_file_id = pf.id \
                 WHERE tm.tested_file_id = ?1",
            )
            .ok()?;
        let rows = stmt.query_map([fid], |row| row.get::<_, String>(0)).ok()?;
        for row in rows.flatten() {
            // Normalize the test file path to a nextest test name stem.
            // tests/integration/cli_scan.rs -> cli_scan
            if let Some(stem) = test_file_to_nextest_stem(&row) {
                test_files.insert(stem);
            }
        }
    }

    if test_files.is_empty() {
        None
    } else {
        Some(test_files.into_iter().collect())
    }
}

/// Convert a test file path (e.g. `tests/integration/cli_scan.rs`) to a
/// nextest filterset test-name stem (e.g. `cli_scan`). nextest's `test()`
/// predicate uses a contains matcher by default, so `test(cli_scan)` matches
/// any test whose name contains `cli_scan`.
fn test_file_to_nextest_stem(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");
    let file_name = normalized.rsplit('/').next()?;
    let stem = file_name.strip_suffix(".rs").unwrap_or(file_name);
    if stem.is_empty() {
        None
    } else {
        Some(stem.to_string())
    }
}

/// Build a scoped nextest command using filterset predicates for the given
/// test file stems. Uses `test()` with the default contains matcher.
///
/// The command carries the same feature/target resolution as the scoped
/// clippy step (`--workspace --all-features`) so cargo does not recompile the
/// dependency graph between clippy and nextest. The filterset still scopes
/// which tests run.
///
/// Example: `cargo nextest run --workspace --all-features -E 'test(cli_scan) + test(dead_code_prune)'`
fn build_scoped_nextest_command(test_stems: &[String]) -> String {
    let filtersets: Vec<String> = test_stems.iter().map(|s| format!("test({})", s)).collect();
    format!(
        "cargo nextest run --workspace --all-features -E '{}'",
        filtersets.join(" + ")
    )
}

/// Build a scoped test plan using `test_mapping` to run only the tests that
/// cover the changed files. Falls back to `build_plan` (full suite) when:
/// - `scope` is `Full`
/// - the diff touches shared infrastructure
/// - `test_mapping` is empty or has no mappings for the changed files
///
/// `conn` is the SQLite connection from the storage manager. When `None`,
/// scoped selection is impossible and the full plan is returned.
#[allow(clippy::too_many_arguments)]
pub fn build_plan_scoped(
    packet: &ImpactPacket,
    rules: &Rules,
    predicted: &[PredictedFile],
    config: &VerifyConfig,
    profile: &crate::platform::repository::RepositoryProfile,
    scope: VerifyScope,
    conn: Option<&rusqlite::Connection>,
    repo_root: &std::path::Path,
) -> VerificationPlan {
    build_plan_scoped_with_options(
        packet, rules, predicted, config, profile, scope, conn, repo_root, false,
    )
}

/// Internal entry point that also accepts `auto_index`. When `auto_index` is
/// true and `test_mapping` is empty/stale relative to the impact packet's
/// `head_hash`, this triggers an incremental index for the changed files and
/// then retries scoped selection once.
#[allow(clippy::too_many_arguments)]
pub fn build_plan_scoped_with_options(
    packet: &ImpactPacket,
    rules: &Rules,
    predicted: &[PredictedFile],
    config: &VerifyConfig,
    profile: &crate::platform::repository::RepositoryProfile,
    scope: VerifyScope,
    conn: Option<&rusqlite::Connection>,
    repo_root: &std::path::Path,
    auto_index: bool,
) -> VerificationPlan {
    if !scope.is_fast() {
        // Explicit full request — no fallback announcement needed.
        return build_plan_with_scope(packet, rules, predicted, config, profile, scope, repo_root);
    }
    if touches_shared_infra(packet) {
        let mut plan =
            build_plan_with_scope(packet, rules, predicted, config, profile, scope, repo_root);
        plan.fallback_reason = Some(format_fallback_reason(
            "shared infrastructure touched",
            "running full (~5-8 min)",
        ));
        return plan;
    }

    // Try scoped selection.
    let scoped_stems = conn.and_then(|c| query_scoped_test_files(c, packet));

    if let Some(test_stems) = scoped_stems {
        return build_fast_scoped_plan(packet, &test_stems);
    }

    // Scoped selection unavailable. If auto_index is enabled and the mapping
    // is empty/stale, refresh the index for changed files and retry once.
    if auto_index
        && let Some(c) = conn
        && is_test_mapping_stale(c, packet)
    {
        if let Err(e) = run_incremental_index_for_changed_files(packet, repo_root, config) {
            let mut plan =
                build_plan_with_scope(packet, rules, predicted, config, profile, scope, repo_root);
            plan.fallback_reason = Some(format_fallback_reason(
                &format!("auto-index failed ({e}); test_mapping empty/stale"),
                "running full (~5-8 min)",
            ));
            return plan;
        }

        // Re-read the mapping after indexing. If it still yields nothing, fall
        // back to full with an announcement.
        let retry_stems = conn.and_then(|c| query_scoped_test_files(c, packet));
        if let Some(test_stems) = retry_stems {
            return build_fast_scoped_plan(packet, &test_stems);
        }
    }

    // Fall back to the full plan with a visible reason.
    let mut plan =
        build_plan_with_scope(packet, rules, predicted, config, profile, scope, repo_root);
    let reason = if let Some(c) = conn {
        if is_test_mapping_stale(c, packet) {
            "test_mapping is stale or empty; run `ledgerful index --incremental` or use `--auto-index`"
        } else {
            "test_mapping has no mappings for the changed files"
        }
    } else {
        "test_mapping unavailable (no database connection)"
    };
    plan.fallback_reason = Some(format_fallback_reason(reason, "running full (~5-8 min)"));
    plan
}

fn format_fallback_reason(trigger: &str, consequence: &str) -> String {
    format!("fast scope unavailable — {trigger}; {consequence}")
}

fn build_fast_scoped_plan(packet: &ImpactPacket, test_stems: &[String]) -> VerificationPlan {
    let scoped_cmd = build_scoped_nextest_command(test_stems);
    let mut steps: Vec<VerificationStep> = Vec::new();

    // Always include fmt + clippy in fast scope — they're cheap and
    // catch issues the test suite doesn't.
    //
    // fmt stays sequential before clippy. The fast path never runs a mutating
    // `cargo fmt` (without `--check`) concurrently with a build: a mutating fmt
    // rewrites .rs files in place, which would cause rustc/clippy torn reads,
    // spurious errors, and incremental-cache invalidation.
    steps.push(VerificationStep {
        command: "cargo fmt --all -- --check".to_string(),
        timeout_secs: 60,
        description: "Scoped: format check".to_string(),
        shell: false,
    });
    steps.push(VerificationStep {
        command: "cargo clippy --all-targets --all-features -- -D warnings".to_string(),
        timeout_secs: DEFAULT_AUTO_TIMEOUT_SECS,
        description: "Scoped: lints".to_string(),
        shell: false,
    });
    steps.push(VerificationStep {
        command: scoped_cmd,
        timeout_secs: DEFAULT_AUTO_TIMEOUT_SECS,
        description: format!(
            "Scoped: tests covering {} changed file(s) via test_mapping",
            packet.changes.len()
        ),
        shell: false,
    });

    VerificationPlan {
        source: Some(PlanSource::AutoPolicy), // Scoped testing is always auto-policy derived
        steps,
        fallback_reason: None,
    }
}

/// Returns true if the test_mapping table is empty or its last-indexed HEAD
/// differs from the impact packet's `head_hash`. This detects the common case
/// where the index was built before the current set of changes landed.
fn is_test_mapping_stale(conn: &rusqlite::Connection, packet: &ImpactPacket) -> bool {
    let total: i64 = conn
        .query_row("SELECT count(*) FROM test_mapping", [], |row| row.get(0))
        .unwrap_or(0);
    if total == 0 {
        return true;
    }

    let indexed_head: Option<String> = conn
        .query_row(
            "SELECT value FROM index_metadata WHERE key = 'head_hash'",
            [],
            |row| row.get(0),
        )
        .ok();

    match (&packet.head_hash, indexed_head.as_deref()) {
        (Some(packet_head), Some(indexed_head)) => packet_head != indexed_head,
        // If either side is missing, treat as stale to be safe.
        _ => true,
    }
}

/// Run an incremental index limited to the changed files in the packet. This
/// delegates to the same indexer used by `ledgerful index --incremental` but
/// does not spawn a separate CLI process.
fn run_incremental_index_for_changed_files(
    packet: &ImpactPacket,
    repo_root: &std::path::Path,
    config: &VerifyConfig,
) -> Result<(), String> {
    use crate::config::model::Config;
    use crate::index::ProjectIndexer;
    use crate::state::layout::Layout;
    use crate::state::storage::StorageManager;

    let root = camino::Utf8PathBuf::from_path_buf(repo_root.to_path_buf())
        .map_err(|_| "repo root is not valid UTF-8".to_string())?;
    let layout = Layout::new(&root);
    let db_path = layout.state_subdir().join("ledger.db");

    let storage = StorageManager::init(db_path.as_std_path())
        .map_err(|e| format!("failed to open storage for auto-index: {e}"))?;

    let mut full_config = crate::config::load::load_config(&layout).unwrap_or_else(|err| {
        tracing::warn!("Failed to load config for auto-index: {err}. Using defaults.");
        Config::default()
    });
    full_config.verify = config.clone();

    let mut indexer = ProjectIndexer::new(storage, root, full_config);
    indexer
        .incremental_index()
        .map_err(|e| format!("incremental index failed: {e}"))?;

    // Persist the packet's HEAD as the index HEAD so future runs can detect
    // freshness without re-scanning.
    if let Some(head) = &packet.head_hash {
        let conn = indexer.storage().get_connection();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO index_metadata (key, value) VALUES ('head_hash', ?1)",
            [head],
        );
    }

    // Return ownership of storage so it is dropped cleanly.
    let _ = indexer.into_storage().shutdown();
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerificationStep {
    pub command: String,
    pub timeout_secs: u64,
    pub description: String,
    /// When false (default), the command is parsed into argv tokens and
    /// executed directly. When true, the command is executed through a
    /// system shell (cmd /C on Windows, sh -c on Unix). Shell execution is
    /// an explicit opt-in because it exposes shell-injection risk.
    #[serde(default)]
    pub shell: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PlanSource {
    ExplicitConfig,
    AutoPolicy,
    HistoricalRulesFallback,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerificationPlan {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<PlanSource>,
    pub steps: Vec<VerificationStep>,
    /// When the requested fast scope could not be honored and the plan fell
    /// back to the full suite, this records the human-readable reason so the
    /// runner can announce it before executing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

impl VerificationPlan {
    /// Reorder the verification steps based on historical failure probabilities.
    /// Steps with a higher probability of failure are sorted to execute first.
    pub fn apply_probability_ordering(
        &mut self,
        probabilities: &std::collections::HashMap<String, f64>,
    ) {
        self.steps.sort_by(|a, b| {
            let prob_a = probabilities.get(&a.command).copied().unwrap_or(0.0);
            let prob_b = probabilities.get(&b.command).copied().unwrap_or(0.0);

            // Sort descending (higher probability first)
            prob_b
                .partial_cmp(&prob_a)
                .unwrap_or(std::cmp::Ordering::Equal)
                // Fallback to alphabetical sorting for deterministic ordering if probs are equal
                .then(a.command.cmp(&b.command))
        });
    }
}

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
fn build_plan_with_scope(
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
    }
}

/// Parse nextest.toml content and report whether `[profile.<name>]` exists.
///
/// Prefer `toml::from_str` over `str::parse::<toml::Value>()`: under `toml` 1.x the
/// `FromStr` impl rejects multi-document / multi-table files that `from_str` accepts,
/// which previously left `has_ci` / `has_slow` permanently false.
fn nextest_has_profile(content: &str, profile_name: &str) -> bool {
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
fn append_full_tier_commands(
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

/// Builds a verification plan from config-defined steps.
/// Returns None if no steps are defined.
pub fn build_plan_from_config(config: &VerifyConfig) -> Option<VerificationPlan> {
    if config.steps.is_empty() {
        return None;
    }

    let steps = config
        .steps
        .iter()
        .map(|step| VerificationStep {
            command: step.command.clone(),
            timeout_secs: step.timeout_secs.unwrap_or(config.default_timeout_secs),
            description: if step.description.is_empty() {
                format!("From config: {}", step.command)
            } else {
                step.description.clone()
            },
            shell: step.shell,
        })
        .collect();

    Some(VerificationPlan {
        source: Some(PlanSource::ExplicitConfig),
        steps,
        fallback_reason: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::{ChangedFile, FileAnalysisStatus, ImpactPacket};
    use crate::policy::mode::Mode;
    use crate::policy::rules::{GlobalRules, PathRule, Rules};
    use std::path::PathBuf;

    fn empty_packet() -> ImpactPacket {
        ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("src/main.rs"),
                status: "Modified".to_string(),
                old_path: None,
                is_staged: false,

                symbols: None,
                imports: None,
                runtime_usage: None,
                analysis_status: FileAnalysisStatus::default(),
                analysis_warnings: Vec::new(),
                api_routes: Vec::new(),
                data_models: Vec::new(),
                ci_gates: Vec::new(),
            }],
            ..ImpactPacket::default()
        }
    }

    #[test]
    fn test_build_plan_default_when_no_rules() {
        let packet = empty_packet();
        let rules = Rules::default();
        let config = VerifyConfig {
            prefer_nextest: Some(false),
            ..Default::default()
        };
        let profile = crate::platform::repository::RepositoryProfile::default();
        let plan = build_plan(
            &packet,
            &rules,
            &[],
            &config,
            &profile,
            std::path::Path::new("."),
        );

        assert_eq!(plan.steps.len(), 2);
        // When prefer_nextest is Some(false), falls back to cargo test
    }

    #[test]
    fn test_build_plan_with_global_verifications() {
        let packet = empty_packet();
        let rules = Rules {
            was_legacy_default: false,
            global: GlobalRules {
                mode: Mode::Analyze,
                required_verifications: vec!["cargo test".to_string(), "cargo clippy".to_string()],
            },
            overrides: Vec::new(),
            protected_paths: Vec::new(),
        };
        let config = VerifyConfig {
            prefer_nextest: Some(false),
            ..Default::default()
        };
        let profile = crate::platform::repository::RepositoryProfile::default();

        let plan = build_plan(
            &packet,
            &rules,
            &[],
            &config,
            &profile,
            std::path::Path::new("."),
        );

        assert_eq!(plan.steps.len(), 2);
        assert!(plan.steps.iter().any(|s| s.command == "cargo clippy"));
        assert!(plan.steps.iter().any(|s| s.command == "cargo test"));
    }

    #[test]
    fn test_build_plan_deduplicates() {
        let packet = empty_packet();
        let rules = Rules {
            was_legacy_default: false,
            global: GlobalRules {
                mode: Mode::Analyze,
                required_verifications: vec!["cargo test".to_string()],
            },
            overrides: vec![PathRule {
                pattern: "*.rs".to_string(),
                mode: None,
                required_verifications: vec!["cargo test".to_string()],
            }],
            protected_paths: Vec::new(),
        };
        let config = VerifyConfig {
            prefer_nextest: Some(false),
            ..Default::default()
        };
        let profile = crate::platform::repository::RepositoryProfile::default();

        let plan = build_plan(
            &packet,
            &rules,
            &[],
            &config,
            &profile,
            std::path::Path::new("."),
        );

        assert_eq!(plan.steps.len(), 1);
        assert!(plan.steps.iter().any(|s| s.command == "cargo test"));
    }

    #[test]
    fn test_build_plan_path_rule_matching() {
        let packet = empty_packet(); // src/main.rs matches *.rs
        let rules = Rules {
            was_legacy_default: false,
            global: GlobalRules {
                mode: Mode::Analyze,
                required_verifications: vec!["cargo test".to_string()],
            },
            overrides: vec![PathRule {
                pattern: "*.rs".to_string(),
                mode: None,
                required_verifications: vec!["cargo clippy".to_string()],
            }],
            protected_paths: Vec::new(),
        };
        let config = VerifyConfig {
            prefer_nextest: Some(false),
            ..Default::default()
        };
        let profile = crate::platform::repository::RepositoryProfile::default();

        let plan = build_plan(
            &packet,
            &rules,
            &[],
            &config,
            &profile,
            std::path::Path::new("."),
        );

        assert_eq!(plan.steps.len(), 2);
        assert!(plan.steps.iter().any(|s| s.command == "cargo clippy"));
        assert!(plan.steps.iter().any(|s| s.command == "cargo test"));
    }

    #[test]
    fn test_build_plan_path_rule_no_match() {
        let packet = empty_packet(); // src/main.rs
        let rules = Rules {
            was_legacy_default: false,
            global: GlobalRules {
                mode: Mode::Analyze,
                required_verifications: vec![],
            },
            overrides: vec![PathRule {
                pattern: "*.py".to_string(),
                mode: None,
                required_verifications: vec!["pytest".to_string()],
            }],
            protected_paths: Vec::new(),
        };
        let config = VerifyConfig {
            prefer_nextest: Some(false),
            ..Default::default()
        };
        let profile = crate::platform::repository::RepositoryProfile::default();

        let plan = build_plan(
            &packet,
            &rules,
            &[],
            &config,
            &profile,
            std::path::Path::new("."),
        );

        // No match, falls back to default empty auto policy? Wait!
        // The old test expected it to fall back to 'cargo test' because it was hardcoded.
        // With auto_policy, a neutral repo emits 2 git diff steps.
        // But since we are full scope, append_full_tier_commands will append 'cargo test -j 1 ...' !
        assert_eq!(plan.steps.len(), 2);
    }

    #[test]
    fn test_build_plan_deterministic() {
        let packet = empty_packet();
        let rules = Rules {
            was_legacy_default: false,
            global: GlobalRules {
                mode: Mode::Analyze,
                required_verifications: vec!["z_cmd".to_string(), "a_cmd".to_string()],
            },
            overrides: Vec::new(),
            protected_paths: Vec::new(),
        };
        let config = VerifyConfig {
            prefer_nextest: Some(false),
            ..Default::default()
        };
        let profile = crate::platform::repository::RepositoryProfile::default();

        let plan1 = build_plan(
            &packet,
            &rules,
            &[],
            &config,
            &profile,
            std::path::Path::new("."),
        );
        let plan2 = build_plan(
            &packet,
            &rules,
            &[],
            &config,
            &profile,
            std::path::Path::new("."),
        );

        assert_eq!(plan1, plan2);
        // Sorted alphabetically
        assert!(plan1.steps.iter().any(|s| s.command == "a_cmd"));
        assert!(plan1.steps.iter().any(|s| s.command == "z_cmd"));
    }

    #[test]
    fn test_build_plan_empty_changes_no_path_match() {
        let packet = ImpactPacket {
            changes: vec![],
            ..ImpactPacket::default()
        };

        let rules = Rules {
            was_legacy_default: false,
            global: GlobalRules {
                mode: Mode::Analyze,
                required_verifications: vec!["cargo test".to_string()],
            },
            overrides: vec![PathRule {
                pattern: "*.rs".to_string(),
                mode: None,
                required_verifications: vec!["cargo clippy".to_string()],
            }],
            protected_paths: Vec::new(),
        };
        let config = VerifyConfig {
            prefer_nextest: Some(false),
            ..Default::default()
        };
        let profile = crate::platform::repository::RepositoryProfile::default();

        let plan = build_plan(
            &packet,
            &rules,
            &[],
            &config,
            &profile,
            std::path::Path::new("."),
        );

        // Global is included, path rule doesn't match empty changes
        assert_eq!(plan.steps.len(), 1);
        assert!(plan.steps.iter().any(|s| s.command == "cargo test"));
    }

    #[test]
    fn test_build_plan_with_predicted_files() {
        let packet = empty_packet(); // changed src/main.rs
        let rules = Rules {
            was_legacy_default: false,
            global: GlobalRules::default(),
            overrides: vec![PathRule {
                pattern: "tests/*.rs".to_string(),
                mode: None,
                required_verifications: vec!["cargo test --test '*'".to_string()],
            }],
            protected_paths: Vec::new(),
        };

        use crate::verify::predict::{PredictedFile, PredictionReason};
        let predicted = vec![PredictedFile {
            path: PathBuf::from("tests/integration.rs"),
            reason: PredictionReason::Temporal,
        }];
        let config = VerifyConfig {
            prefer_nextest: Some(false),
            ..Default::default()
        };
        let profile = crate::platform::repository::RepositoryProfile::default();

        let plan = build_plan(
            &packet,
            &rules,
            &predicted,
            &config,
            &profile,
            std::path::Path::new("."),
        );

        // Predicted rule match overrides default, but full scope appends the
        // fallback full-suite cargo test command.
        assert_eq!(plan.steps.len(), 1);
        assert!(
            plan.steps
                .iter()
                .any(|s| s.command == "cargo test --test '*'"),
            "expected cargo test --test '*' but got {:?}",
            plan.steps
        );

        let predicted_step = plan
            .steps
            .iter()
            .find(|s| s.command == "cargo test --test '*'")
            .unwrap();
        assert!(predicted_step.description.contains("Predicted impact"));
    }

    #[test]
    fn test_build_plan_merges_descriptions() {
        let packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("src/lib.rs"),
                status: "Modified".to_string(),
                old_path: None,
                is_staged: true,

                symbols: None,
                imports: None,
                runtime_usage: None,
                analysis_status: FileAnalysisStatus::default(),
                analysis_warnings: Vec::new(),
                api_routes: Vec::new(),
                data_models: Vec::new(),
                ci_gates: Vec::new(),
            }],
            ..ImpactPacket::default()
        };

        let rules = Rules {
            was_legacy_default: false,
            global: GlobalRules::default(),
            overrides: vec![PathRule {
                pattern: "src/*.rs".to_string(),
                mode: None,
                required_verifications: vec!["cargo check".to_string()],
            }],
            protected_paths: Vec::new(),
        };

        use crate::verify::predict::{PredictedFile, PredictionReason};
        let predicted = vec![PredictedFile {
            path: PathBuf::from("src/other.rs"),
            reason: PredictionReason::Structural,
        }];
        let config = VerifyConfig {
            prefer_nextest: Some(false),
            ..Default::default()
        };
        let profile = crate::platform::repository::RepositoryProfile::default();

        let plan = build_plan(
            &packet,
            &rules,
            &predicted,
            &config,
            &profile,
            std::path::Path::new("."),
        );

        // 'cargo check' is triggered by BOTH the direct change in src/lib.rs
        // AND the predicted impact on src/other.rs. Full scope also appends the
        // fallback full-suite command, so we expect 2 steps.
        assert_eq!(plan.steps.len(), 1);
        let check_step = plan
            .steps
            .iter()
            .find(|s| s.command == "cargo check")
            .expect("cargo check step");
        assert!(check_step.description.contains("From rules"));
        assert!(check_step.description.contains("Predicted impact"));
        assert!(check_step.description.contains(" | "));
    }

    #[test]
    fn test_nextest_has_profile_multi_table_nextest_toml_detects_ci_and_slow() {
        // Regression for 0067/codex P1: str::parse::<toml::Value> failed on real
        // nextest.toml (multi-table), so profile probes were permanently false.
        let content = r#"
[profile.default]
slow-timeout = { period = "60s", terminate-after = 1 }

[profile.ci]
retries = 1

[profile.slow]
default-filter = 'test(/__slow$/)'
"#;
        assert!(nextest_has_profile(content, "ci"));
        assert!(nextest_has_profile(content, "slow"));
        assert!(!nextest_has_profile(content, "compile-fail"));
        assert!(!nextest_has_profile("not toml {{{", "ci"));
    }

    #[test]
    fn test_resolve_default_test_command_with_ci_profile_uses_profile_ci() {
        if !crate::verify::engine::probe_nextest() {
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path().join(".config");
        std::fs::create_dir_all(&config_dir).expect("mkdir .config");
        std::fs::write(
            config_dir.join("nextest.toml"),
            "[profile.ci]\nretries = 1\n",
        )
        .expect("write nextest.toml");
        let cmd = resolve_default_test_command(Some(true), dir.path());
        assert_eq!(
            cmd, "cargo nextest run --workspace --all-features --profile ci",
            "must detect [profile.ci] via toml::from_str"
        );
    }

    #[test]
    fn test_append_full_tier_commands_emits_slow_and_doctest_not_compile_fail() {
        if !crate::verify::engine::probe_nextest() {
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path().join(".config");
        std::fs::create_dir_all(&config_dir).expect("mkdir .config");
        std::fs::write(
            config_dir.join("nextest.toml"),
            "[profile.ci]\nretries = 1\n\n[profile.slow]\ndefault-filter = 'test(/__slow$/)'\n",
        )
        .expect("write nextest.toml");

        let mut steps = Vec::new();
        append_full_tier_commands(&mut steps, Some(true), true, dir.path());
        let cmds: Vec<&str> = steps.iter().map(|s| s.command.as_str()).collect();
        assert!(
            cmds.contains(&"cargo nextest run --workspace --all-features --profile slow"),
            "full tier must emit slow profile: {cmds:?}"
        );
        assert!(
            cmds.contains(&"cargo test --workspace --all-features --doc"),
            "full tier must emit doctests: {cmds:?}"
        );
        assert!(
            cmds.iter().all(|c| !c.contains("compile-fail")),
            "full tier must not emit compile-fail after 0067: {cmds:?}"
        );
    }

    #[test]
    fn test_default_command_fallback_when_nextest_disabled() {
        let cmd = resolve_default_test_command(Some(false), std::path::Path::new("."));
        assert_eq!(cmd, "cargo test --workspace --all-features");
    }

    #[test]
    fn test_default_command_nextest_preferred() {
        // On CI/generic runners nextest might not be installed, but the function
        // should probe and fall back gracefully. We verify the command resolves
        // to a concrete default and contains nextest when probe succeeds.
        let cmd = resolve_default_test_command(None, std::path::Path::new("."));
        assert!(!cmd.is_empty(), "default test command must not be empty");
        assert!(
            cmd.starts_with("cargo "),
            "default command should start with cargo: {cmd}"
        );
        if crate::verify::engine::probe_nextest() {
            assert!(
                cmd.contains("nextest"),
                "with nextest installed command should contain nextest: {cmd}"
            );
        } else {
            assert_eq!(cmd, "cargo test --workspace --all-features");
        }
    }

    #[test]
    fn test_build_plan_from_config_empty() {
        let config = VerifyConfig::default();
        assert!(build_plan_from_config(&config).is_none());
    }

    #[test]
    fn test_build_plan_from_config_with_steps() {
        let config = VerifyConfig {
            mode: None,
            steps: vec![
                crate::config::model::VerifyStep {
                    description: "Run tests".to_string(),
                    command: "cargo test".to_string(),
                    timeout_secs: Some(60),
                    shell: false,
                },
                crate::config::model::VerifyStep {
                    description: String::new(),
                    command: "cargo fmt --check".to_string(),
                    timeout_secs: None, // uses default_timeout_secs
                    shell: false,
                },
            ],
            default_timeout_secs: 120,
            semantic_weight: 0.3,
            prefer_nextest: None,
            ..Default::default()
        };
        let plan = build_plan_from_config(&config).unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].description, "Run tests");
        assert_eq!(plan.steps[0].timeout_secs, 60);
        assert_eq!(plan.steps[1].description, "From config: cargo fmt --check");
        // None timeout_secs should resolve to default_timeout_secs
        assert_eq!(plan.steps[1].timeout_secs, 120);
    }

    // ── Scoped selection tests (Tier 1 + Tier 6) ─────────────────────────

    #[test]
    fn test_touches_shared_infra_cargo_toml() {
        let packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("Cargo.toml"),
                ..Default::default()
            }],
            ..ImpactPacket::default()
        };
        assert!(touches_shared_infra(&packet));
    }

    #[test]
    fn test_touches_shared_infra_cli_args() {
        let packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("src/cli/args.rs"),
                ..Default::default()
            }],
            ..ImpactPacket::default()
        };
        assert!(touches_shared_infra(&packet));
    }

    #[test]
    fn test_touches_shared_infra_config_glob() {
        let packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("src/config/model/coverage.rs"),
                ..Default::default()
            }],
            ..ImpactPacket::default()
        };
        assert!(touches_shared_infra(&packet));
    }

    #[test]
    fn test_touches_shared_infra_normal_source() {
        let packet = empty_packet(); // src/main.rs
        assert!(!touches_shared_infra(&packet));
    }

    #[test]
    fn test_touches_shared_infra_migrations() {
        let packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("src/state/migrations/m11.rs"),
                ..Default::default()
            }],
            ..ImpactPacket::default()
        };
        assert!(touches_shared_infra(&packet));
    }

    #[test]
    fn test_touches_shared_infra_storage_subdir() {
        let packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("src/state/storage/connection.rs"),
                ..Default::default()
            }],
            ..ImpactPacket::default()
        };
        assert!(touches_shared_infra(&packet));
    }

    #[test]
    fn test_test_file_to_nextest_stem() {
        assert_eq!(
            test_file_to_nextest_stem("tests/integration/cli_scan.rs"),
            Some("cli_scan".to_string())
        );
        assert_eq!(
            test_file_to_nextest_stem("tests\\integration\\cli_dead_code.rs"),
            Some("cli_dead_code".to_string())
        );
        assert_eq!(
            test_file_to_nextest_stem("src/lib.rs"),
            Some("lib".to_string())
        );
        assert_eq!(test_file_to_nextest_stem(""), None);
    }

    #[test]
    fn test_build_scoped_nextest_command_single() {
        let cmd = build_scoped_nextest_command(&["cli_scan".to_string()]);
        assert_eq!(
            cmd,
            "cargo nextest run --workspace --all-features -E 'test(cli_scan)'"
        );
    }

    #[test]
    fn test_build_scoped_nextest_command_multiple() {
        let cmd =
            build_scoped_nextest_command(&["cli_scan".to_string(), "dead_code_prune".to_string()]);
        assert_eq!(
            cmd,
            "cargo nextest run --workspace --all-features -E 'test(cli_scan) + test(dead_code_prune)'"
        );
    }

    #[test]
    fn test_scoped_clippy_and_nextest_share_feature_flags() {
        // §B regression guard: clippy and scoped nextest must share
        // --all-features so cargo does not recompile the dependency graph
        // between the two steps under a different feature resolution.
        let test_stems = vec!["cli_scan".to_string()];
        let nextest_cmd = build_scoped_nextest_command(&test_stems);
        let clippy_cmd = "cargo clippy --all-targets --all-features -- -D warnings";

        assert!(
            nextest_cmd.contains("--all-features"),
            "scoped nextest must carry --all-features, got: {nextest_cmd}"
        );
        assert!(
            nextest_cmd.contains("--workspace"),
            "scoped nextest must carry --workspace, got: {nextest_cmd}"
        );
        assert!(
            clippy_cmd.contains("--all-features"),
            "scoped clippy must carry --all-features, got: {clippy_cmd}"
        );
        // Both must carry --all-features (the cache-buster was that nextest lacked it).
        // clippy uses --all-targets, nextest uses --workspace — different selection
        // scopes, but the feature resolution must be identical.
    }

    #[test]
    fn test_build_plan_scoped_full_scope_uses_build_plan() {
        let packet = empty_packet();
        let rules = Rules::default();
        let plan = build_plan_scoped(
            &packet,
            &rules,
            &[],
            &crate::config::model::VerifyConfig::default(),
            &crate::platform::repository::RepositoryProfile::default(),
            VerifyScope::Full,
            None,
            std::path::Path::new("."),
        );
        // Full scope → falls through to build_plan → default cargo test command.
        assert_eq!(plan.steps.len(), 2);
    }

    #[test]
    fn test_build_plan_scoped_fast_no_conn_falls_back() {
        let packet = empty_packet();
        let rules = Rules::default();
        let plan = build_plan_scoped(
            &packet,
            &rules,
            &[],
            &crate::config::model::VerifyConfig::default(),
            &crate::platform::repository::RepositoryProfile::default(),
            VerifyScope::Fast,
            None,
            std::path::Path::new("."),
        );
        // No connection → can't scope → falls back to full build_plan.
        assert_eq!(plan.steps.len(), 2);
    }

    #[test]
    fn test_build_plan_scoped_fast_shared_infra_falls_back() {
        let packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("Cargo.toml"),
                ..Default::default()
            }],
            ..ImpactPacket::default()
        };
        let rules = Rules::default();
        // Even with a conn, shared infra → full plan.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let plan = build_plan_scoped(
            &packet,
            &rules,
            &[],
            &crate::config::model::VerifyConfig::default(),
            &crate::platform::repository::RepositoryProfile::default(),
            VerifyScope::Fast,
            Some(&conn),
            std::path::Path::new("."),
        );
        // Falls back to build_plan (no rules match Cargo.toml except the
        // rules.toml override, but in this test rules are default/empty).
        assert_eq!(plan.steps.len(), 2);
        assert!(
            plan.fallback_reason
                .as_deref()
                .unwrap_or("")
                .contains("shared infrastructure"),
            "expected fallback reason to mention shared infrastructure, got {:?}",
            plan.fallback_reason
        );
    }

    #[test]
    fn test_build_plan_scoped_fast_empty_test_mapping_falls_back() {
        let packet = empty_packet(); // src/main.rs, not shared infra
        let rules = Rules::default();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // Create the test_mapping table but leave it empty.
        conn.execute(
            "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE project_files (id INTEGER PRIMARY KEY, file_path TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE project_symbols (id INTEGER PRIMARY KEY, symbol_name TEXT, file_id INTEGER)",
            [],
        )
        .unwrap();
        let plan = build_plan_scoped(
            &packet,
            &rules,
            &[],
            &crate::config::model::VerifyConfig::default(),
            &crate::platform::repository::RepositoryProfile::default(),
            VerifyScope::Fast,
            Some(&conn),
            std::path::Path::new("."),
        );
        // Empty mapping → falls back to full plan.
        assert_eq!(plan.steps.len(), 2);
        let reason = plan.fallback_reason.as_deref().unwrap_or("");
        assert!(
            reason.contains("test_mapping is stale or empty")
                || reason.contains("test_mapping has no mappings for the changed files")
                || reason.contains("test_mapping unavailable"),
            "expected fallback reason to explain mapping unavailability, got {:?}",
            plan.fallback_reason
        );
    }

    #[test]
    fn test_build_plan_scoped_fast_with_mappings_emits_scoped_command() {
        let packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("src/commands/hotspots.rs"),
                ..Default::default()
            }],
            ..ImpactPacket::default()
        };
        let rules = Rules::default();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE project_files (id INTEGER PRIMARY KEY, file_path TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE project_symbols (id INTEGER PRIMARY KEY, symbol_name TEXT, file_id INTEGER)",
            [],
        )
        .unwrap();
        // src/commands/hotspots.rs is file id 1.
        conn.execute(
            "INSERT INTO project_files (id, file_path) VALUES (1, 'src/commands/hotspots.rs')",
            [],
        )
        .unwrap();
        // tests/integration/cli_hotspots.rs is the test file, id 2.
        conn.execute(
            "INSERT INTO project_files (id, file_path) VALUES (2, 'tests/integration/cli_hotspots.rs')",
            [],
        )
        .unwrap();
        // Map: tested_file_id=1 (hotspots.rs) → test_file_id=2 (cli_hotspots.rs).
        conn.execute(
            "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id) \
             VALUES (10, 2, 20, 1)",
            [],
        )
        .unwrap();

        let plan = build_plan_scoped(
            &packet,
            &rules,
            &[],
            &crate::config::model::VerifyConfig::default(),
            &crate::platform::repository::RepositoryProfile::default(),
            VerifyScope::Fast,
            Some(&conn),
            std::path::Path::new("."),
        );
        // Should produce 3 steps: fmt, clippy, scoped test command.
        assert_eq!(plan.steps.len(), 3);
        assert!(plan.steps.iter().any(|s| s.command.contains("fmt")));
        assert!(plan.steps.iter().any(|s| s.command.contains("clippy")));
        let scoped_step = plan
            .steps
            .iter()
            .find(|s| {
                s.command
                    .contains("nextest run --workspace --all-features -E")
            })
            .expect("scoped nextest command");
        assert!(
            scoped_step.command.contains("test(cli_hotspots)"),
            "expected cli_hotspots in command, got: {}",
            scoped_step.command
        );
        assert!(
            scoped_step.command.contains("--all-features"),
            "scoped nextest must carry --all-features, got: {}",
            scoped_step.command
        );
    }

    #[test]
    fn test_verify_scope_display() {
        assert_eq!(format!("{}", VerifyScope::Fast), "fast");
        assert_eq!(format!("{}", VerifyScope::Full), "full");
    }

    #[test]
    fn test_verify_scope_default_is_full() {
        assert_eq!(VerifyScope::default(), VerifyScope::Full);
    }

    #[test]
    fn test_is_test_mapping_stale_empty_mapping() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
            [],
        )
        .unwrap();
        let packet = ImpactPacket::default();
        assert!(is_test_mapping_stale(&conn, &packet));
    }

    #[test]
    fn test_is_test_mapping_stale_head_hash_mismatch() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id) \
             VALUES (1, 1, 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE index_metadata (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO index_metadata (key, value) VALUES ('head_hash', 'old-hash')",
            [],
        )
        .unwrap();
        let packet = ImpactPacket {
            head_hash: Some("new-hash".to_string()),
            ..ImpactPacket::default()
        };
        assert!(is_test_mapping_stale(&conn, &packet));
    }

    #[test]
    fn test_is_test_mapping_stale_head_hash_matches() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id) \
             VALUES (1, 1, 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE index_metadata (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO index_metadata (key, value) VALUES ('head_hash', 'current-hash')",
            [],
        )
        .unwrap();
        let packet = ImpactPacket {
            head_hash: Some("current-hash".to_string()),
            ..ImpactPacket::default()
        };
        assert!(!is_test_mapping_stale(&conn, &packet));
    }

    #[test]
    fn test_build_plan_scoped_fast_auto_index_failure_announcement() {
        let packet = ImpactPacket {
            head_hash: Some("abc123".to_string()),
            changes: vec![ChangedFile {
                path: PathBuf::from("src/main.rs"),
                ..Default::default()
            }],
            ..ImpactPacket::default()
        };
        let rules = Rules::default();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // Create the tables so is_test_mapping_stale sees an empty mapping.
        conn.execute(
            "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
            [],
        )
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let plan = build_plan_scoped_with_options(
            &packet,
            &rules,
            &[],
            &crate::config::model::VerifyConfig::default(),
            &crate::platform::repository::RepositoryProfile::default(),
            VerifyScope::Fast,
            Some(&conn),
            tmp.path(),
            true,
        );
        assert_eq!(plan.steps.len(), 2);
        let reason = plan.fallback_reason.as_deref().unwrap_or("");
        assert!(
            reason.contains("auto-index failed"),
            "expected fallback reason to mention auto-index failure, got: {reason}"
        );
        assert!(
            reason.contains("running full"),
            "expected fallback reason to announce full run, got: {reason}"
        );
    }

    #[test]
    fn test_build_plan_scoped_fast_auto_index_not_triggered_when_mapping_exists() {
        // When test_mapping already has entries and is not stale,
        // auto_index=true should NOT trigger a reindex — the scoped plan
        // is returned directly.
        let packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("src/commands/hotspots.rs"),
                ..Default::default()
            }],
            ..ImpactPacket::default()
        };
        let rules = Rules::default();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // Set up tables with a mapping.
        conn.execute(
            "CREATE TABLE test_mapping (test_symbol_id INTEGER, test_file_id INTEGER, \
             tested_symbol_id INTEGER, tested_file_id INTEGER)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE project_files (id INTEGER PRIMARY KEY, file_path TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE project_symbols (id INTEGER PRIMARY KEY, symbol_name TEXT, file_id INTEGER)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE index_metadata (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project_files (id, file_path) VALUES (1, 'src/commands/hotspots.rs')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project_files (id, file_path) VALUES (2, 'tests/integration/cli_hotspots.rs')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id) \
             VALUES (10, 2, 20, 1)",
            [],
        )
        .unwrap();

        let plan = build_plan_scoped_with_options(
            &packet,
            &rules,
            &[],
            &crate::config::model::VerifyConfig::default(),
            &crate::platform::repository::RepositoryProfile::default(),
            VerifyScope::Fast,
            Some(&conn),
            std::path::Path::new("."),
            true, // auto_index=true
        );
        // Should return scoped plan (3 steps), not full plan.
        assert_eq!(
            plan.steps.len(),
            3,
            "expected scoped plan, got {} steps: {:?}",
            plan.steps.len(),
            plan.steps
        );
        assert!(
            plan.fallback_reason.is_none(),
            "should not have fallback reason"
        );
        assert!(
            plan.steps
                .iter()
                .any(|s| s.command.contains("test(cli_hotspots)"))
        );
    }
}
