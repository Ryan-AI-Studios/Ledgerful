use super::{
    MappingFreshness, PlanSource, VerificationPlan, VerificationStep, VerifyScope,
    build_empty_changes_plan, build_plan_with_scope, classify_test_mapping_freshness,
    format_fallback_reason, refuse_plan,
};
use crate::config::model::VerifyConfig;
use crate::impact::packet::ImpactPacket;
use crate::policy::rules::Rules;
use crate::verify::predict::PredictedFile;
use crate::verify::timeouts::DEFAULT_AUTO_TIMEOUT_SECS;

use super::shared_infra::touches_shared_infra;

/// Query `test_mapping` for the test files that cover the changed source
/// files. Returns a sorted, deduplicated list of test file stems suitable for
/// nextest filterset predicates (e.g. `cli_scan` from
/// `tests/integration/cli_scan.rs`).
///
/// Returns `None` (meaning "cannot scope") when:
/// - the connection is not available
/// - the `test_mapping` table doesn't exist or is empty
/// - no mappings are found for any changed file
///
/// Callers decide refuse vs allow-full-fallback; this helper only selects stems.
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
pub(crate) fn test_file_to_nextest_stem(path: &str) -> Option<String> {
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
pub(crate) fn build_scoped_nextest_command(test_stems: &[String]) -> String {
    let filtersets: Vec<String> = test_stems.iter().map(|s| format!("test({})", s)).collect();
    format!(
        "cargo nextest run --workspace --all-features -E '{}'",
        filtersets.join(" + ")
    )
}

/// Build a scoped test plan using `test_mapping` to run only the tests that
/// cover the changed files.
///
/// Classifier order under `--scope fast` (load-bearing, 0135 + 0145):
/// 1. **SharedInfra** → full suite + announce
/// 2. **EmptyChanges** → cheap fmt+clippy when Rust detected (no nextest);
///    non-Rust → zero steps, still pass
///    (Live-empty working tree short-circuits in `commands/verify` before
///    this builder — not here, so plan unit tests stay hermetic.)
/// 3. **Freshness gate** (before trusting stems): HeadMismatch auto-repairs
///    once without `--auto-index`; Empty / PacketHeadMissing need the flag
/// 4. **ScopedOk** → stems from a non-stale mapping → 3-step scoped plan
/// 5. **MappingRefuse** → no stems / no conn (unless `allow_full_fallback` → full + announce)
///
/// Spec B7/0145: non-empty mapping + head mismatch never silent ScopedOk on
/// stems alone; HeadMismatch attempts one bounded repair first.
///
/// `conn` is the SQLite connection from the storage manager. When `None` and
/// changes exist, scoped selection is impossible → MappingRefuse (or full if allow).
///
/// `layout` provides the analysis root (`layout.root`) and shared state home
/// (`layout.state_dir`) so auto-index opens the same DB as verify (0108).
#[allow(clippy::too_many_arguments)]
pub fn build_plan_scoped(
    packet: &ImpactPacket,
    rules: &Rules,
    predicted: &[PredictedFile],
    config: &VerifyConfig,
    profile: &crate::platform::repository::RepositoryProfile,
    scope: VerifyScope,
    conn: Option<&rusqlite::Connection>,
    layout: &crate::state::layout::Layout,
) -> VerificationPlan {
    build_plan_scoped_with_options(
        packet, rules, predicted, config, profile, scope, conn, layout, false, false,
    )
}

/// Internal entry point that also accepts `auto_index` and `allow_full_fallback`.
///
/// Freshness-aware repair (0145):
/// - **HeadMismatch** → one bounded incremental repair **without** requiring
///   `--auto-index`, then re-classify; still not Ok → refuse.
/// - **Empty** → repair only with `--auto-index`; still empty → refuse.
/// - **PacketHeadMissing** → repair only with `--auto-index`.
///
/// On still-cannot-scope, **refuses** unless `allow_full_fallback`.
#[allow(clippy::too_many_arguments)]
pub fn build_plan_scoped_with_options(
    packet: &ImpactPacket,
    rules: &Rules,
    predicted: &[PredictedFile],
    config: &VerifyConfig,
    profile: &crate::platform::repository::RepositoryProfile,
    scope: VerifyScope,
    conn: Option<&rusqlite::Connection>,
    layout: &crate::state::layout::Layout,
    auto_index: bool,
    allow_full_fallback: bool,
) -> VerificationPlan {
    let repo_root = layout.root.as_std_path();
    if !scope.is_fast() {
        // Explicit full request — no fallback announcement needed.
        return build_plan_with_scope(packet, rules, predicted, config, profile, scope, repo_root);
    }

    // 1. SharedInfra — justified full suite (unchanged from 0061).
    if touches_shared_infra(packet) {
        let mut plan =
            build_plan_with_scope(packet, rules, predicted, config, profile, scope, repo_root);
        plan.fallback_reason = Some(format_fallback_reason(
            "shared infrastructure touched",
            "running full (~5-8 min)",
        ));
        plan.refused = false;
        return plan;
    }

    // 2. EmptyChanges — before stem query (query_scoped returns None for both
    // empty changes and no-mappings; cannot distinguish after the fact).
    // Profile-aware: only schedule cargo fmt/clippy when Rust is detected so
    // non-Rust / empty repos still exit 0 under --scope fast (Daily 5 honesty
    // without inventing a toolchain).
    // Note: live-clean trees with a non-empty *saved* packet are handled in
    // `commands/verify` (B1) so this packet-empty path stays pure.
    if packet.changes.is_empty() {
        return build_empty_changes_plan(profile);
    }

    // 3. Freshness gate — must run before trusting stems.
    // HeadMismatch auto-repairs once (no flag). Empty still needs --auto-index.
    const EMPTY_REMEDIATION: &str =
        "test_mapping is empty; run `ledgerful index --incremental` or use `--auto-index`";
    const HEAD_LAG_REMEDIATION: &str =
        "test_mapping head_hash lags HEAD; run `ledgerful index --incremental`";
    const UNVERIFIABLE_REMEDIATION: &str = "test_mapping freshness unverifiable; run `ledgerful index --incremental` or use `--auto-index`";

    if let Some(c) = conn {
        let mut freshness = classify_test_mapping_freshness(c, packet);
        let should_repair = should_attempt_mapping_repair(freshness, auto_index);

        if should_repair {
            if let Err(e) = run_incremental_index_for_changed_files(packet, layout, config) {
                let class_hint = match freshness {
                    MappingFreshness::HeadMismatch => "test_mapping head_hash lags HEAD",
                    MappingFreshness::Empty => "test_mapping empty",
                    MappingFreshness::PacketHeadMissing => "test_mapping freshness unverifiable",
                    MappingFreshness::Ok => "test_mapping",
                };
                let trigger = format!("auto-index failed ({e}); {class_hint}");
                return mapping_cannot_scope_outcome(
                    &trigger,
                    allow_full_fallback,
                    packet,
                    rules,
                    predicted,
                    config,
                    profile,
                    scope,
                    repo_root,
                );
            }
            freshness = classify_test_mapping_freshness(c, packet);
            if freshness == MappingFreshness::Ok {
                tracing::debug!("test_mapping freshness repaired; continuing to stem query");
            }
        }

        if freshness != MappingFreshness::Ok {
            let remediation = match freshness {
                MappingFreshness::Empty => EMPTY_REMEDIATION,
                MappingFreshness::HeadMismatch => HEAD_LAG_REMEDIATION,
                MappingFreshness::PacketHeadMissing => UNVERIFIABLE_REMEDIATION,
                MappingFreshness::Ok => EMPTY_REMEDIATION, // unreachable
            };
            return mapping_cannot_scope_outcome(
                remediation,
                allow_full_fallback,
                packet,
                rules,
                predicted,
                config,
                profile,
                scope,
                repo_root,
            );
        }
        // Fresh — fall through to stem query.
    } else {
        // No conn + has changes → MappingRefuse (or allow full).
        return mapping_cannot_scope_outcome(
            "test_mapping unavailable (no database connection)",
            allow_full_fallback,
            packet,
            rules,
            predicted,
            config,
            profile,
            scope,
            repo_root,
        );
    }

    // 4. ScopedOk — stems only after non-stale gate passed.
    let scoped_stems = conn.and_then(|c| query_scoped_test_files(c, packet));
    if let Some(test_stems) = scoped_stems {
        return build_fast_scoped_plan(packet, &test_stems);
    }

    // 5. MappingRefuse — fresh mapping but no coverage for changed files.
    mapping_cannot_scope_outcome(
        "test_mapping has no mappings for the changed files",
        allow_full_fallback,
        packet,
        rules,
        predicted,
        config,
        profile,
        scope,
        repo_root,
    )
}
/// Shared outcome for mapping-cannot-scope: refuse by default, or 0061 full
/// execute + announce when `allow_full_fallback`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn mapping_cannot_scope_outcome(
    trigger: &str,
    allow_full_fallback: bool,
    packet: &ImpactPacket,
    rules: &Rules,
    predicted: &[PredictedFile],
    config: &VerifyConfig,
    profile: &crate::platform::repository::RepositoryProfile,
    scope: VerifyScope,
    repo_root: &std::path::Path,
) -> VerificationPlan {
    if allow_full_fallback {
        let mut plan =
            build_plan_with_scope(packet, rules, predicted, config, profile, scope, repo_root);
        plan.fallback_reason = Some(format_fallback_reason(trigger, "running full (~5-8 min)"));
        plan.refused = false;
        plan
    } else {
        refuse_plan(trigger)
    }
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
        refused: false,
    }
}

/// Returns true if the test_mapping table is empty or otherwise unusable for
/// the fast gate without repair. Thin wrapper over
/// [`classify_test_mapping_freshness`].
#[allow(dead_code)] // retained public-style API; production uses classify directly
pub(crate) fn is_test_mapping_stale(conn: &rusqlite::Connection, packet: &ImpactPacket) -> bool {
    matches!(
        classify_test_mapping_freshness(conn, packet),
        MappingFreshness::Empty
            | MappingFreshness::HeadMismatch
            | MappingFreshness::PacketHeadMissing
    )
}

/// Whether the fast-scope freshness gate should attempt one bounded incremental
/// repair before re-classifying / refusing.
///
/// Behavior table (0145):
/// | freshness            | auto_index | attempt |
/// | Ok                   | *          | false   |
/// | HeadMismatch         | *          | true    |
/// | Empty                | false      | false   |
/// | Empty                | true       | true    |
/// | PacketHeadMissing    | false      | false   |
/// | PacketHeadMissing    | true       | true    |
pub(crate) fn should_attempt_mapping_repair(freshness: MappingFreshness, auto_index: bool) -> bool {
    match freshness {
        MappingFreshness::Ok => false,
        MappingFreshness::HeadMismatch => true,
        MappingFreshness::Empty | MappingFreshness::PacketHeadMissing => auto_index,
    }
}

/// Run an incremental index for the analysis root. This delegates to the same
/// indexer used by `ledgerful index --incremental` but does not spawn a
/// separate CLI process.
///
/// Uses `layout.state_dir` for the DB (shared across linked worktrees) and
/// `layout.root` as the analysis root — never invents `Layout::new(repo_root)`.
///
/// `packet` is retained for call-site symmetry / future delta-scoped repair;
/// head_hash is **not** written from the packet (trust store_index_metadata).
fn run_incremental_index_for_changed_files(
    _packet: &ImpactPacket,
    layout: &crate::state::layout::Layout,
    config: &VerifyConfig,
) -> Result<(), String> {
    use crate::config::model::Config;
    use crate::index::ProjectIndexer;
    use crate::state::storage::StorageManager;

    let storage = StorageManager::init_with_layout(layout)
        .map_err(|e| format!("failed to open storage for auto-index: {e}"))?;

    let mut full_config = crate::config::load::load_config(layout).unwrap_or_else(|err| {
        tracing::warn!("Failed to load config for auto-index: {err}. Using defaults.");
        Config::default()
    });
    full_config.verify = config.clone();

    let mut indexer = ProjectIndexer::new(storage, layout.root.clone(), full_config);
    indexer
        .incremental_index()
        .map_err(|e| format!("incremental index failed: {e}"))?;

    // Trust `store_index_metadata` from incremental_index (writes current git
    // HEAD). Do **not** overwrite with packet.head_hash — a stale packet head
    // would mark the mapping "fresh" against obsolete stems (0145 DoD-6).

    // Return ownership of storage so it is dropped cleanly.
    let _ = indexer.into_storage().shutdown();
    Ok(())
}
