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

use super::non_code::{all_non_code_cheap, any_packaging_or_bump_script};
use super::shared_infra::touches_shared_infra;

/// nextest `test()` stem injected when packaging templates or bump scripts change.
const BUMP_MANIFESTS_STEM: &str = "bump_manifests";

/// Query `test_mapping` for the test files that cover the changed source
/// files. Returns a sorted, deduplicated list of nextest `test()` stems.
/// Integration-test paths keep the filename stem (`cli_scan` from
/// `tests/integration/cli_scan.rs`); in-file product-path tests use the
/// test function name so `test(boundary)` is not an empty filter.
///
/// Returns `None` (meaning "cannot scope") when:
/// - the connection is not available
/// - the `test_mapping` table doesn't exist or is empty
/// - no mappings are found for any changed file
///
/// Callers decide refuse vs allow-full-fallback; this helper only selects stems.
pub(crate) fn query_scoped_test_files(
    conn: &rusqlite::Connection,
    packet: &ImpactPacket,
) -> Option<Vec<String>> {
    let total: i64 = conn
        .query_row("SELECT count(*) FROM test_mapping", [], |row| row.get(0))
        .unwrap_or(0);
    if total == 0 {
        return None;
    }

    let mut test_files: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for change in &packet.changes {
        let path_str = change.path.to_string_lossy().replace('\\', "/");
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
        let rows = scoped_test_file_rows(conn, fid)?;
        for (file_path, symbol_name) in rows {
            if crate::index::test_mapping::is_test_path(&file_path) {
                if let Some(stem) = test_file_to_nextest_stem(&file_path) {
                    test_files.insert(stem);
                }
            } else if let Some(name) = symbol_name.filter(|s| !s.is_empty()) {
                test_files.insert(name);
            }
        }
    }

    if test_files.is_empty() {
        None
    } else {
        Some(test_files.into_iter().collect())
    }
}

/// `(file_path, test symbol_name)`. Falls back to path-only when hermetic
/// fixtures omit `project_symbols` (LEFT JOIN would fail to prepare).
fn scoped_test_file_rows(
    conn: &rusqlite::Connection,
    tested_file_id: i64,
) -> Option<Vec<(String, Option<String>)>> {
    let mut joined = match conn.prepare(
        "SELECT pf.file_path, ps.symbol_name \
         FROM test_mapping tm \
         JOIN project_files pf ON tm.test_file_id = pf.id \
         LEFT JOIN project_symbols ps ON tm.test_symbol_id = ps.id \
         WHERE tm.tested_file_id = ?1",
    ) {
        Ok(stmt) => stmt,
        Err(_) => {
            let mut stmt = conn
                .prepare(
                    "SELECT pf.file_path \
                     FROM test_mapping tm \
                     JOIN project_files pf ON tm.test_file_id = pf.id \
                     WHERE tm.tested_file_id = ?1",
                )
                .ok()?;
            let rows = stmt
                .query_map([tested_file_id], |row| row.get::<_, String>(0))
                .ok()?;
            return Some(rows.flatten().map(|p| (p, None)).collect());
        }
    };
    let rows = joined
        .query_map([tested_file_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .ok()?;
    Some(rows.flatten().collect())
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
/// Classifier order under `--scope fast` (load-bearing, 0135 + 0145 + 0203):
/// 1. **SharedInfra** → full suite + announce
/// 2. **NonCodeCheap** → all paths match cheap globs (docs/CHANGELOG/packaging
///    / bump scripts); skip freshness; A2 inject `bump_manifests` or fmt+clippy
/// 3. **EmptyChanges** → cheap fmt+clippy when Rust detected (no nextest);
///    non-Rust → zero steps, still pass
///    (Live-empty working tree short-circuits in `commands/verify` before
///    this builder — not here, so plan unit tests stay hermetic.)
/// 4. **Freshness gate** (before trusting stems): HeadMismatch auto-repairs
///    once without `--auto-index`; Empty / PacketHeadMissing need the flag
/// 5. **ScopedOk** → stems from a non-stale mapping → 3-step scoped plan;
///    union `bump_manifests` when any packaging / bump-script path (P7)
/// 6. **MappingRefuse** → no stems / no conn (unless `allow_full_fallback` → full + announce)
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

    // 2. NonCodeCheap — all classified paths match cheap globs (docs /
    // CHANGELOG / packaging / bump scripts). Skip freshness and mapping.
    // `fallback_reason` stays None so JSON `scopeExecuted` remains `"fast"`.
    if all_non_code_cheap(packet) {
        return build_non_code_cheap_plan(packet, profile);
    }

    // 3. EmptyChanges — before stem query (query_scoped returns None for both
    // empty changes and no-mappings; cannot distinguish after the fact).
    // Profile-aware: only schedule cargo fmt/clippy when Rust is detected so
    // non-Rust / empty repos still exit 0 under --scope fast (Daily 5 honesty
    // without inventing a toolchain).
    // Note: live-clean trees with a non-empty *saved* packet are handled in
    // `commands/verify` (B1) so this packet-empty path stays pure.
    if packet.changes.is_empty() {
        return build_empty_changes_plan(profile);
    }

    // 4. Freshness gate — must run before trusting stems.
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

    // 5. ScopedOk — stems only after non-stale gate passed.
    // P7: union `bump_manifests` when any packaging / bump-script path is in
    // the classified set (mixed src+packaging that already ScopedOk on src).
    let scoped_stems = conn.and_then(|c| query_scoped_test_files(c, packet));
    if let Some(mut test_stems) = scoped_stems {
        let unioned = union_bump_manifests_if_needed(packet, &mut test_stems);
        return build_fast_scoped_plan(packet, &test_stems, unioned);
    }

    // 6. MappingRefuse — fresh mapping but no coverage for changed files.
    // Mixed src unmapped + packaging still refuses (do not cheap mixed).
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

fn build_non_code_cheap_plan(
    packet: &ImpactPacket,
    profile: &crate::platform::repository::RepositoryProfile,
) -> VerificationPlan {
    let rust_steps: Vec<VerificationStep> = if profile.rust.is_some() {
        vec![
            VerificationStep {
                command: "cargo fmt --all -- --check".to_string(),
                timeout_secs: 60,
                description: "Non-code changes: format check (scoped tests N/A)".to_string(),
                shell: false,
            },
            VerificationStep {
                command: "cargo clippy --all-targets --all-features -- -D warnings".to_string(),
                timeout_secs: DEFAULT_AUTO_TIMEOUT_SECS,
                description: "Non-code changes: lints (scoped tests N/A)".to_string(),
                shell: false,
            },
        ]
    } else {
        Vec::new()
    };
    let packaging_step = any_packaging_or_bump_script(packet).then(|| {
        let stems = vec![BUMP_MANIFESTS_STEM.to_string()];
        VerificationStep {
            command: build_scoped_nextest_command(&stems),
            timeout_secs: DEFAULT_AUTO_TIMEOUT_SECS,
            description: "Non-code changes: packaging/scripts tests (bump_manifests)".to_string(),
            shell: false,
        }
    });
    let steps: Vec<VerificationStep> = rust_steps.into_iter().chain(packaging_step).collect();
    VerificationPlan {
        source: Some(PlanSource::AutoPolicy),
        steps,
        fallback_reason: None,
        refused: false,
    }
}

/// Union stem `bump_manifests` into `stems` when any classified path is
/// packaging or a bump script. Returns true when the stem was added (inject).
fn union_bump_manifests_if_needed(packet: &ImpactPacket, stems: &mut Vec<String>) -> bool {
    if !any_packaging_or_bump_script(packet) {
        return false;
    }
    if stems.iter().any(|s| s == BUMP_MANIFESTS_STEM) {
        return false;
    }
    stems.push(BUMP_MANIFESTS_STEM.to_string());
    stems.sort();
    stems.dedup();
    true
}

fn scoped_nextest_description(packet: &ImpactPacket, injected: bool) -> String {
    if injected {
        format!(
            "Scoped: tests covering {} changed file(s) plus packaging/scripts (bump_manifests)",
            packet.changes.len()
        )
    } else {
        format!(
            "Scoped: tests covering {} changed file(s) via test_mapping",
            packet.changes.len()
        )
    }
}

fn build_fast_scoped_plan(
    packet: &ImpactPacket,
    test_stems: &[String],
    injected: bool,
) -> VerificationPlan {
    let scoped_cmd = build_scoped_nextest_command(test_stems);
    // Always include fmt + clippy in fast scope — they're cheap and
    // catch issues the test suite doesn't.
    //
    // fmt stays sequential before clippy. The fast path never runs a mutating
    // `cargo fmt` (without `--check`) concurrently with a build: a mutating fmt
    // rewrites .rs files in place, which would cause rustc/clippy torn reads,
    // spurious errors, and incremental-cache invalidation.
    let steps = vec![
        VerificationStep {
            command: "cargo fmt --all -- --check".to_string(),
            timeout_secs: 60,
            description: "Scoped: format check".to_string(),
            shell: false,
        },
        VerificationStep {
            command: "cargo clippy --all-targets --all-features -- -D warnings".to_string(),
            timeout_secs: DEFAULT_AUTO_TIMEOUT_SECS,
            description: "Scoped: lints".to_string(),
            shell: false,
        },
        VerificationStep {
            command: scoped_cmd,
            timeout_secs: DEFAULT_AUTO_TIMEOUT_SECS,
            description: scoped_nextest_description(packet, injected),
            shell: false,
        },
    ];

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
