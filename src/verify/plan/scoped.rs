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
use std::path::Path;

use super::embedded::{self, EmbeddedDecision};
use super::non_code::{
    all_non_code_cheap, any_include_str_material, any_packaging_or_bump_script,
    is_legacy_material_path, is_non_code_cheap_path, union_include_str_material_stems,
};
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
    build_plan_scoped_with_rust_crates(
        packet,
        rules,
        predicted,
        config,
        profile,
        scope,
        conn,
        layout,
        auto_index,
        allow_full_fallback,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_plan_scoped_with_rust_crates(
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
    rust_crates: Option<embedded::RustCrates>,
) -> VerificationPlan {
    let repo_root = layout.root.as_std_path();
    if !scope.is_fast() {
        // Explicit full request — no fallback announcement needed.
        return build_plan_with_scope(packet, rules, predicted, config, profile, scope, repo_root);
    }

    // A broken root manifest is an execution precondition, not a discovery
    // failure. Preserve the empty-packet contract and root-based profile; this
    // guard precedes SharedInfra so malformed Cargo.toml cannot become a
    // zero-step full fallback. No manifest walk or profile injection occurs.
    if !packet.changes.is_empty()
        && profile.rust.is_none()
        && profile
            .evidence
            .contains(&crate::platform::repository::DetectionEvidence::FoundCargoToml)
    {
        let class = if profile.warnings.iter().any(|warning| {
            matches!(
                warning,
                crate::platform::repository::DetectionWarning::UnreadableManifest(details) if details.starts_with("Cargo.toml:")
            )
        }) {
            "unreadable Cargo.toml"
        } else {
            "malformed Cargo.toml"
        };
        return super::refuse_plan(&format!("Rust detection inconclusive: {class}"));
    }

    // None is the memoized NotNeeded state. Only execution owns the discovery
    // trigger; planning never turns it into a second directory walk.
    let detected = rust_crates.unwrap_or(embedded::RustCrates::Absent);
    if let embedded::RustCrates::Unknown(class) = detected
        && packet
            .changes
            .iter()
            .any(|change| !is_non_code_cheap_path(&change.path))
    {
        return super::refuse_plan(&format!("Rust detection inconclusive: {class}"));
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
    //
    // 0549: before trusting the cheap globs, look up whether a changed
    // non-source path is compiled into or read by Rust tests. A referenced
    // path contributes test function names; anything the lookup cannot
    // prove refuses (or runs full under `allow_full_fallback`).
    // The engine's own tree keeps the finer hand-written `INCLUDE_STR_MATERIAL`
    // table (0480/0483) for the cheap set; a file-level union would select
    // dozens of unrelated tests per README edit. Non-cheap paths (fixtures,
    // templates, data) are looked up everywhere, including this tree.
    let engine_tree = work_root_is_engine_tree(repo_root);
    let lookup_active = (profile.rust.is_some() || detected == embedded::RustCrates::Present)
        && !packet.changes.is_empty();
    let emb: EmbeddedDecision = if lookup_active {
        let candidates: Vec<String> = embedded::candidate_paths(packet)
            .into_iter()
            .filter(|p| !engine_tree || !is_non_code_cheap_path(Path::new(p)))
            .collect();
        embedded::decide(repo_root, &candidates, |p| {
            is_non_code_cheap_path(Path::new(p))
        })
    } else {
        EmbeddedDecision::default()
    };
    if detected == embedded::RustCrates::Present
        && !repo_root.join("Cargo.toml").is_file()
        && (!emb.stems.is_empty() || !emb.needs_mapping.is_empty())
    {
        return super::refuse_plan(
            "Rust crates found only below the repo root (no root Cargo.toml); add a root workspace Cargo.toml or run verify from a checkout rooted at the crate",
        );
    }
    if let Some(trigger) = &emb.refuse {
        return mapping_cannot_scope_outcome(
            trigger,
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
    let non_rust_candidates: Vec<String> = if lookup_active {
        emb.no_dependency
            .iter()
            .filter(|path| !engine_tree || !is_non_code_cheap_path(Path::new(path)))
            .cloned()
            .collect()
    } else {
        embedded::candidate_paths(packet)
            .into_iter()
            .filter(|path| !engine_tree || !is_non_code_cheap_path(Path::new(path)))
            .collect()
    };
    if !non_rust_candidates.is_empty() {
        let non_rust = super::decide_non_rust(repo_root, &non_rust_candidates, |path| {
            is_non_code_cheap_path(Path::new(path))
        });
        if let Some(trigger) = non_rust.refuse {
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
    }
    let has_source = packet
        .changes
        .iter()
        .any(|c| embedded::is_source_path(&c.path));
    let pure_non_source = !packet.changes.is_empty() && !has_source;
    let pure_non_legacy = pure_non_source
        && packet
            .changes
            .iter()
            .all(|change| !is_legacy_material_path(&change.path));
    // Gate2 P1-1: an unreferenced path that the pre-0549 definition treated as
    // material (nested `Cargo.toml`, `.json`/`.yaml`/`.sql` fixtures, ...) must
    // keep reaching the mapping gate (refuse), never become a fmt+clippy pass.
    let legacy_unreferenced: Vec<&str> = emb
        .no_dependency
        .iter()
        .filter(|p| !is_non_code_cheap_path(Path::new(p)) && is_legacy_material_path(Path::new(p)))
        .map(String::as_str)
        .collect();
    if !legacy_unreferenced.is_empty() {
        return mapping_cannot_scope_outcome(
            &format!(
                "unreferenced legacy-material path(s): {}",
                legacy_unreferenced.join(", ")
            ),
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
    if emb.needs_mapping.is_empty()
        && (all_non_code_cheap(packet)
            || (lookup_active
                && pure_non_source
                && legacy_unreferenced.is_empty()
                && (!engine_tree || !emb.stems.is_empty()))
            || (!lookup_active && pure_non_legacy))
    {
        return build_non_code_cheap_plan(
            packet,
            profile,
            repo_root,
            config.suite_timeout_secs,
            &emb,
        );
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

    // 0549 D3(d): production includers without inline tests are resolved
    // through `test_mapping` under the same freshness gate as changed code.
    let packet_for_mapping = packet;

    if let Some(c) = conn {
        let mut freshness = classify_test_mapping_freshness(c, packet_for_mapping);
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
            freshness = classify_test_mapping_freshness(c, packet_for_mapping);
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
    // 0480: union `include_str!` cheap-doc stems the same way.
    // 0549 P1-2: every production includer must itself have mapped tests; a
    // sibling file's stems must not stand in for it.
    if has_source {
        let mut source_packet = packet.clone();
        source_packet
            .changes
            .retain(|change| embedded::is_source_path(&change.path));
        if conn
            .and_then(|c| query_scoped_test_files(c, &source_packet))
            .is_none()
        {
            return mapping_cannot_scope_outcome(
                "test_mapping has no mappings for changed source independently of embedded includers",
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
    }
    let mut includer_stems = Vec::new();
    for includer in &emb.needs_mapping {
        let single = ImpactPacket {
            changes: vec![crate::impact::packet::ChangedFile {
                path: std::path::PathBuf::from(includer),
                status: "Modified".to_string(),
                ..Default::default()
            }],
            head_hash: packet.head_hash.clone(),
            ..ImpactPacket::default()
        };
        if let Some(stems) = conn.and_then(|c| query_scoped_test_files(c, &single)) {
            includer_stems.extend(stems);
        } else {
            return mapping_cannot_scope_outcome(
                &format!("embedded path is read by {includer}, which has no test_mapping rows"),
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
    }
    // P1-1: embedded stems never stand in for unmapped changed source; the
    // code-change mapping must scope on its own before they are unioned.
    let mut scoped_stems = if has_source {
        conn.and_then(|c| query_scoped_test_files(c, packet))
    } else if !includer_stems.is_empty() || !emb.stems.is_empty() {
        Some(Vec::new())
    } else {
        conn.and_then(|c| query_scoped_test_files(c, packet))
    };
    if let Some(stems) = scoped_stems.as_mut() {
        stems.extend(includer_stems);
        stems.extend(emb.stems.iter().cloned());
        stems.sort();
        stems.dedup();
    }
    if let Some(mut test_stems) = scoped_stems {
        let inject = union_cheap_inject_stems(packet, profile, repo_root, &mut test_stems);
        return build_fast_scoped_plan(
            packet,
            &test_stems,
            inject,
            config.suite_timeout_secs,
            &emb,
        );
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

/// True when `root` is the package tree this binary was compiled from.
/// A failed canonicalize does not inject and does not refuse.
fn work_root_is_engine_tree(root: &Path) -> bool {
    let Ok(work) = dunce::canonicalize(root) else {
        return false;
    };
    let Ok(engine) = dunce::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR"))) else {
        return false;
    };
    work == engine
}

fn build_non_code_cheap_plan(
    packet: &ImpactPacket,
    profile: &crate::platform::repository::RepositoryProfile,
    repo_root: &Path,
    suite: Option<u64>,
    emb: &EmbeddedDecision,
) -> VerificationPlan {
    let rust_steps: Vec<VerificationStep> = if profile.rust.is_some() {
        vec![
            VerificationStep {
                command: "cargo fmt --all -- --check".to_string(),
                timeout_secs: 60,
                description: "Non-code changes: format check (scoped tests N/A)".to_string(),
                shell: false,
                budget_source: None,
            },
            VerificationStep {
                command: "cargo clippy --all-targets --all-features -- -D warnings".to_string(),
                timeout_secs: DEFAULT_AUTO_TIMEOUT_SECS,
                description: "Non-code changes: lints (scoped tests N/A)".to_string(),
                shell: false,
                budget_source: None,
            },
        ]
    } else {
        Vec::new()
    };
    let mut stems: Vec<String> = Vec::new();
    let packaging = any_packaging_or_bump_script(packet);
    if packaging {
        stems.push(BUMP_MANIFESTS_STEM.to_string());
    }
    // Doc inject is engine-dogfood nextest stems. Rust must be detected and
    // the work root must be this binary's compile tree. The compare runs
    // before the mutating union. Packaging A2 still injects `bump_manifests`
    // without a rust profile and without the engine-tree check.
    let include_str = profile.rust.is_some()
        && work_root_is_engine_tree(repo_root)
        && any_include_str_material(packet);
    if include_str {
        union_include_str_material_stems(packet, &mut stems);
    }
    // 0549: lookup-derived test function names (any repo, not engine-only).
    stems.extend(emb.stems.iter().cloned());
    if packaging || include_str || !emb.stems.is_empty() {
        stems.sort();
        stems.dedup();
    }
    let nextest_step = (!stems.is_empty()).then(|| VerificationStep {
        command: build_scoped_nextest_command(&stems),
        timeout_secs: DEFAULT_AUTO_TIMEOUT_SECS,
        description: with_embedded_note(non_code_nextest_description(packaging, include_str), emb),
        shell: false,
        budget_source: None,
    });
    let mut steps: Vec<VerificationStep> = rust_steps.into_iter().chain(nextest_step).collect();
    // D4: unreferenced non-cheap paths are annotated, not silently ignored.
    let unreferenced: Vec<&str> = emb
        .no_dependency
        .iter()
        .filter(|p| !is_non_code_cheap_path(Path::new(p.as_str())))
        .map(String::as_str)
        .collect();
    if !unreferenced.is_empty()
        && let Some(first) = steps.first_mut()
    {
        first.description = format!(
            "{} (no direct include/read found by the lexical lookup for: {})",
            first.description,
            unreferenced.join(", ")
        );
    }
    if !emb.annotations.is_empty()
        && let Some(first) = steps.first_mut()
    {
        first
            .description
            .push_str(&format!(" ({})", emb.annotations.join("; ")));
    }
    for step in &mut steps {
        crate::verify::runner::stamp_auto_budget(step, suite);
    }
    VerificationPlan {
        source: Some(PlanSource::AutoPolicy),
        steps,
        fallback_reason: None,
        refused: false,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CheapInject {
    packaging: bool,
    include_str: bool,
}

/// Union packaging + `include_str!` cheap-doc stems into `stems`.
/// Doc stems require a Rust profile (engine-dogfood nextest). Packaging A2
/// still injects `bump_manifests` when `profile.rust` is None.
fn union_cheap_inject_stems(
    packet: &ImpactPacket,
    profile: &crate::platform::repository::RepositoryProfile,
    repo_root: &Path,
    stems: &mut Vec<String>,
) -> CheapInject {
    CheapInject {
        packaging: union_bump_manifests_if_needed(packet, stems),
        include_str: profile.rust.is_some()
            && work_root_is_engine_tree(repo_root)
            && union_include_str_material_stems(packet, stems),
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

/// Append the lookup evidence (`path -> includer -> tests`) to a step description.
fn with_embedded_note(base: String, emb: &EmbeddedDecision) -> String {
    let mut description = base;
    if !emb.stems.is_empty() || !emb.needs_mapping.is_empty() {
        description.push_str(&format!(
            " plus embedded-material tests ({})",
            emb.evidence.join("; ")
        ));
    }
    let unreferenced: Vec<&str> = emb
        .no_dependency
        .iter()
        .filter(|path| !is_non_code_cheap_path(Path::new(path.as_str())))
        .map(String::as_str)
        .collect();
    if !unreferenced.is_empty() {
        description.push_str(&format!(
            " (no direct include/read found by the lexical lookup for: {})",
            unreferenced.join(", ")
        ));
    }
    if !emb.annotations.is_empty() {
        description.push_str(&format!(" ({})", emb.annotations.join("; ")));
    }
    description
}

fn non_code_nextest_description(packaging: bool, include_str: bool) -> String {
    match (packaging, include_str) {
        (true, true) => {
            "Non-code changes: packaging/scripts tests (bump_manifests) plus include_str embedded docs"
                .to_string()
        }
        (true, false) => {
            "Non-code changes: packaging/scripts tests (bump_manifests)".to_string()
        }
        (false, true) => "Non-code changes: include_str embedded docs tests".to_string(),
        (false, false) => "Non-code changes: scoped tests".to_string(),
    }
}

fn scoped_nextest_description(packet: &ImpactPacket, inject: CheapInject) -> String {
    match (inject.packaging, inject.include_str) {
        (true, true) => format!(
            "Scoped: tests covering {} changed file(s) plus packaging/scripts (bump_manifests) plus include_str embedded docs",
            packet.changes.len()
        ),
        (true, false) => format!(
            "Scoped: tests covering {} changed file(s) plus packaging/scripts (bump_manifests)",
            packet.changes.len()
        ),
        (false, true) => format!(
            "Scoped: tests covering {} changed file(s) plus include_str embedded docs",
            packet.changes.len()
        ),
        (false, false) => format!(
            "Scoped: tests covering {} changed file(s) via test_mapping",
            packet.changes.len()
        ),
    }
}

fn build_fast_scoped_plan(
    packet: &ImpactPacket,
    test_stems: &[String],
    inject: CheapInject,
    suite: Option<u64>,
    emb: &EmbeddedDecision,
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
            budget_source: None,
        },
        VerificationStep {
            command: "cargo clippy --all-targets --all-features -- -D warnings".to_string(),
            timeout_secs: DEFAULT_AUTO_TIMEOUT_SECS,
            description: "Scoped: lints".to_string(),
            shell: false,
            budget_source: None,
        },
        VerificationStep {
            command: scoped_cmd,
            timeout_secs: DEFAULT_AUTO_TIMEOUT_SECS,
            description: with_embedded_note(scoped_nextest_description(packet, inject), emb),
            shell: false,
            budget_source: None,
        },
    ];
    let mut steps = steps;
    for step in &mut steps {
        crate::verify::runner::stamp_auto_budget(step, suite);
    }

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

#[cfg(test)]
mod rust_detection_memo_tests {
    use super::*;
    use crate::impact::packet::ChangedFile;
    use crate::platform::repository::{RepositoryProfile, detect_repository};

    fn plan(
        root: &camino::Utf8Path,
        path: Option<&str>,
        profile: &RepositoryProfile,
        memo: Option<embedded::RustCrates>,
        scope: VerifyScope,
    ) -> VerificationPlan {
        let packet = ImpactPacket {
            changes: path
                .into_iter()
                .map(|path| ChangedFile {
                    path: path.into(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        build_plan_scoped_with_rust_crates(
            &packet,
            &Rules::default(),
            &[],
            &VerifyConfig::default(),
            profile,
            scope,
            None,
            &crate::state::layout::Layout::new(root),
            false,
            false,
            memo,
        )
    }

    #[rstest::rstest]
    #[case(None)]
    #[case(Some("README.md"))]
    fn unknown_memo_does_not_refuse_empty_or_cheap_packet(#[case] path: Option<&str>) {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        // A scanner would reject this unlexable Rust input. Unknown cheap-only
        // planning must not scan it or turn the memo into a refusal.
        std::fs::write(tmp.path().join("bad.rs"), [0xff]).unwrap();
        let plan = plan(
            root,
            path,
            &RepositoryProfile::default(),
            Some(embedded::RustCrates::Unknown("unreadable")),
            VerifyScope::Fast,
        );
        assert!(!plan.refused, "{:?}", plan.fallback_reason);
        assert!(plan.steps.is_empty(), "{:?}", plan.steps);
    }

    #[test]
    fn not_needed_memo_is_not_recomputed_from_nested_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        std::fs::create_dir(tmp.path().join("nested")).unwrap();
        std::fs::write(tmp.path().join("nested/Cargo.toml"), "not valid toml {{{").unwrap();
        let plan = plan(
            root,
            Some("notes.txt"),
            &RepositoryProfile::default(),
            None,
            VerifyScope::Fast,
        );
        // 0570: an unreferenced non-legacy path is a zero-step plan. The nested
        // manifest is still not walked, so this is not a Rust-detection refusal.
        assert!(!plan.refused, "{:?}", plan.fallback_reason);
        assert!(plan.steps.is_empty(), "{:?}", plan.steps);
        let reason = plan.fallback_reason.as_deref().unwrap_or("");
        assert!(!reason.contains("Rust detection inconclusive"), "{reason}");
    }

    #[test]
    fn unknown_strict_packet_refuses_before_shared_infrastructure_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let plan = plan(
            root,
            Some("Cargo.toml"),
            &RepositoryProfile::default(),
            Some(embedded::RustCrates::Unknown("oversized")),
            VerifyScope::Fast,
        );
        assert!(plan.refused);
        assert!(plan.steps.is_empty());
        assert!(
            plan.fallback_reason
                .as_deref()
                .unwrap()
                .contains("Rust detection inconclusive: oversized")
        );
    }

    #[rstest::rstest]
    #[case(None, false)]
    #[case(Some("README.md"), true)]
    #[case(Some("Cargo.toml"), true)]
    fn malformed_root_guard_is_nonempty_and_precedes_full_fallback(
        #[case] path: Option<&str>,
        #[case] refused: bool,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "not valid toml {{{").unwrap();
        let profile = detect_repository(tmp.path());
        assert!(profile.rust.is_none());
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let plan = plan(root, path, &profile, None, VerifyScope::Fast);
        assert_eq!(plan.refused, refused, "{:?}", plan.fallback_reason);
        if refused {
            assert!(
                plan.fallback_reason
                    .as_deref()
                    .unwrap()
                    .contains("malformed Cargo.toml")
            );
        }
        assert!(plan.steps.is_empty());
    }

    #[test]
    fn unreadable_root_manifest_has_named_class() {
        let tmp = tempfile::tempdir().unwrap();
        // A manifest directory is a portable read failure without ACL or root
        // permission assumptions; profile detection records unreadable evidence.
        std::fs::create_dir(tmp.path().join("Cargo.toml")).unwrap();
        let profile = detect_repository(tmp.path());
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let plan = plan(root, Some("README.md"), &profile, None, VerifyScope::Fast);
        assert!(plan.refused);
        assert!(
            plan.fallback_reason
                .as_deref()
                .unwrap()
                .contains("unreadable Cargo.toml")
        );
    }

    #[test]
    fn full_scope_ignores_unknown_detection_and_root_manifest_guard() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "not valid toml {{{").unwrap();
        let profile = detect_repository(tmp.path());
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let plan = plan(
            root,
            Some("README.md"),
            &profile,
            Some(embedded::RustCrates::Unknown("unreadable")),
            VerifyScope::Full,
        );
        assert!(!plan.refused, "{:?}", plan.fallback_reason);
        assert!(profile.rust.is_none());
        assert!(
            plan.steps
                .iter()
                .all(|step| !step.command.starts_with("cargo "))
        );
        let packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: "README.md".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let baseline = build_plan_with_scope(
            &packet,
            &Rules::default(),
            &[],
            &VerifyConfig::default(),
            &profile,
            VerifyScope::Full,
            root.as_std_path(),
        );
        assert_eq!(
            plan, baseline,
            "Rust discovery must not alter the normal full plan"
        );
    }
}
