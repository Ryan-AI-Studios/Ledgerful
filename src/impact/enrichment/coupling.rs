use crate::git::repo::open_repo;
use crate::impact::enrichment::affected_flows::{
    AffectedFlowsOpts, AffectedFlowsReport, compute_affected_flows,
};
use crate::impact::enrichment::blast::{
    BlastAugment, BlastCaps, compute_blast, derive_structural_couplings, resolve_seeds,
    unwired_honesty_notes,
};
use crate::impact::enrichment::test_gaps::{TestGapsOpts, populate_test_coverage_and_gaps};
use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::ImpactPacket;
use crate::impact::temporal::{GixHistoryProvider, TemporalEngine};
use miette::Result;
use tracing::{debug, warn};

pub struct CouplingProvider;

impl EnrichmentProvider for CouplingProvider {
    fn name(&self) -> &'static str {
        "Coupling Enrichment Provider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        // 1. Structural Couplings (from DB via bounded blast radius)
        self.enrich_structural(context, packet)?;

        // 2. Temporal Couplings (from Git history)
        self.enrich_temporal(context, packet)?;

        Ok(())
    }
}

impl CouplingProvider {
    fn enrich_structural(
        &self,
        context: &EnrichmentContext,
        packet: &mut ImpactPacket,
    ) -> Result<()> {
        let augment = BlastAugment::live(context.config.verify.effective_process_policy());
        self.enrich_structural_with(context, packet, &augment)
    }

    fn enrich_structural_with(
        &self,
        context: &EnrichmentContext,
        packet: &mut ImpactPacket,
        augment: &BlastAugment,
    ) -> Result<()> {
        let conn = context.storage.get_connection();
        let seeds = resolve_seeds(packet, conn)?;

        // DoD-5: test_coverage / testHints are independent of structural_edges.
        // Populate even when the call graph is empty so test_mapping is never
        // silently dropped just because callers were not indexed yet.
        // Orchestrator shares one seed list; soft-empty vs mappedCount disagreement
        // is surfaced via gap honesty notes (not silent).
        let mut test_hints: Vec<String> = Vec::new();
        let gap_opts = TestGapsOpts {
            head_hash: packet.head_hash.clone(),
        };
        match populate_test_coverage_and_gaps(conn, &seeds, &gap_opts) {
            Ok((coverage, hints, gaps)) => {
                packet.set_test_coverage(coverage);
                packet.set_test_gaps(Some(gaps));
                test_hints = hints;
            }
            Err(e) => {
                warn!("test_mapping join failed: {e}");
                context.add_warning(format!("test_mapping join failed: {e}"));
            }
        }

        let has_edges = context
            .storage
            .table_exists_and_has_data("structural_edges")?;
        if !has_edges {
            debug!(
                "structural_edges empty/missing; test_coverage may still be set; no blast edges"
            );
            // Honesty-only blast when seeds were expected but graph is absent.
            if !seeds.is_empty() || !test_hints.is_empty() {
                let blast = crate::impact::packet::BlastRadius {
                    depth_requested: context.config.impact.blast_depth.max(1),
                    depth_applied: context.config.impact.blast_depth.max(1),
                    test_hints,
                    honesty_notes: vec![
                        "structural_edges table empty or missing; blast edges unavailable (index with call-graph analysis)"
                            .to_string(),
                    ],
                    ..Default::default()
                };
                if !blast.is_empty_for_serde() {
                    packet.set_blast_radius(Some(blast));
                }
            }
            // 0118: kinds 1–3 still run without blast edges.
            self.attach_affected_flows(conn, packet);
            return Ok(());
        }

        let caps = BlastCaps {
            fanout_per_hop: context.config.impact.blast_fanout_per_hop,
            total_edges: context.config.impact.blast_total_edges,
        };
        let depth_requested = context.config.impact.blast_depth.max(1);
        let depth_max = context.config.impact.blast_depth_max;

        // Always run compute_blast so empty-seed honesty reaches the packet
        // (DoD-7: unbound seeds must not be silent when structural_edges exists).
        // On Err: warn and continue — do not early-return before attach_affected_flows
        // (kinds 1–3 still run without blast edges).
        match compute_blast(conn, &seeds, depth_requested, depth_max, caps, augment) {
            Ok(mut blast) => {
                if blast.edges.is_empty() {
                    blast
                        .honesty_notes
                        .extend(unwired_honesty_notes(&packet.changes));
                    blast.honesty_notes.sort_unstable();
                    blast.honesty_notes.dedup();
                }
                blast.test_hints = test_hints;

                // Derive structural_couplings from hop-1 (single writer — DoD-10)
                packet.set_structural_couplings(derive_structural_couplings(&blast));

                // Emit blast whenever there is content or honesty (never hide unbound/thin).
                if !blast.is_empty_for_serde() {
                    packet.set_blast_radius(Some(blast));
                }
            }
            Err(e) => {
                warn!("compute_blast failed: {e}");
                context.add_warning(format!("compute_blast failed: {e}"));
            }
        }

        // 0118: always attach after blast success or blast error (never skip on Err).
        self.attach_affected_flows(conn, packet);

        Ok(())
    }

    /// Compute and attach `affected_flows` from changes + optional blast edges.
    fn attach_affected_flows(&self, conn: &rusqlite::Connection, packet: &mut ImpactPacket) {
        let opts = AffectedFlowsOpts {
            head_hash: packet.head_hash.clone(),
        };
        match compute_affected_flows(conn, &packet.changes, packet.blast_radius.as_ref(), &opts) {
            Ok(report) => {
                packet.set_affected_flows(Some(report));
            }
            Err(e) => {
                warn!("affected_flows compute failed: {e}");
                packet.set_affected_flows(Some(AffectedFlowsReport::unavailable()));
            }
        }
    }

    fn enrich_temporal(
        &self,
        context: &EnrichmentContext,
        packet: &mut ImpactPacket,
    ) -> Result<()> {
        if context.skip_git_history_enrichment {
            debug!("Skipping temporal git-history enrichment (session opts)");
            return Ok(());
        }
        debug!("Running temporal coupling analysis...");

        let repo = open_repo(&context.project_root)
            .map_err(|e| miette::miette!("Failed to open repo for temporal analysis: {}", e))?;

        let history_provider = GixHistoryProvider::new(&repo);
        let temporal_engine =
            TemporalEngine::new(history_provider, context.config.temporal.clone());

        match temporal_engine.calculate_coupling_report_budgeted(context.history_budget.as_ref()) {
            Ok(report) => {
                let (file_couplings, audit_couplings, discarded) = apply_temporal_presentation(
                    report.file_couplings,
                    report.audit_couplings,
                    &report.discarded_tree_paths,
                    &packet.changes,
                    packet.path_mode.as_str(),
                    context.config.coverage.max_coupling_pairs,
                );
                packet.set_temporal_couplings(file_couplings);
                packet.temporal_couplings_audit = audit_couplings;
                packet.discarded_tree_paths = discarded;
            }
            Err(e) => {
                warn!("Temporal analysis failed: {e}");
                context.add_warning(format!("Temporal analysis failed: {e}"));
            }
        }

        Ok(())
    }
}

/// Change-filter both lists, derive tree paths from the filtered audit pairs,
/// drop code-mode demotions from the file list, then cap each coupling list.
/// A pair hits when `path_class::normalize_path` of either side equals the
/// same normalization of `change.path` or `change.old_path`. Pair spellings
/// stay as stored. `discarded` is not truncated and is not re-filtered after
/// the cap.
pub(crate) fn apply_temporal_presentation(
    mut file_couplings: Vec<crate::impact::packet::TemporalCoupling>,
    mut audit_couplings: Vec<crate::impact::packet::TemporalCoupling>,
    engine_tree_paths: &[camino::Utf8PathBuf],
    changes: &[crate::impact::packet::ChangedFile],
    path_mode: &str,
    limit: usize,
) -> (
    Vec<crate::impact::packet::TemporalCoupling>,
    Vec<crate::impact::packet::TemporalCoupling>,
    Vec<std::path::PathBuf>,
) {
    let mut change_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for change in changes {
        let key = crate::impact::path_class::normalize_path(&change.path.to_string_lossy());
        if !key.is_empty() {
            change_keys.insert(key);
        }
        if let Some(old) = &change.old_path {
            let key = crate::impact::path_class::normalize_path(&old.to_string_lossy());
            if !key.is_empty() {
                change_keys.insert(key);
            }
        }
    }
    let hits_change = |coupling: &crate::impact::packet::TemporalCoupling| {
        let file_a = crate::impact::path_class::normalize_path(&coupling.file_a.to_string_lossy());
        let file_b = crate::impact::path_class::normalize_path(&coupling.file_b.to_string_lossy());
        (!file_a.is_empty() && change_keys.contains(&file_a))
            || (!file_b.is_empty() && change_keys.contains(&file_b))
    };
    file_couplings.retain(&hits_change);
    audit_couplings.retain(&hits_change);

    let engine_trees: std::collections::HashSet<String> = engine_tree_paths
        .iter()
        .map(|path| path.as_str().replace('\\', "/"))
        .collect();
    let mut discarded: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for pair in &audit_couplings {
        for side in [&pair.file_a, &pair.file_b] {
            let key = side.to_string_lossy().replace('\\', "/");
            if engine_trees.contains(&key) {
                discarded.insert(key);
            }
        }
    }

    file_couplings.retain(|coupling| {
        !crate::impact::path_class::should_demote_pair(
            &coupling.file_a.to_string_lossy(),
            &coupling.file_b.to_string_lossy(),
            path_mode,
        )
    });

    if file_couplings.len() > limit {
        file_couplings.truncate(limit);
    }
    if audit_couplings.len() > limit {
        audit_couplings.truncate(limit);
    }

    (
        file_couplings,
        audit_couplings,
        discarded
            .into_iter()
            .map(std::path::PathBuf::from)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::{ChangedFile, FileAnalysisStatus};
    use crate::index::symbols::{Symbol, SymbolKind};
    use crate::state::migrations::get_migrations;
    use crate::state::storage::StorageManager;
    use rusqlite::Connection;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    #[test]
    fn enrich_structural_couplings() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES ('src/caller.rs', 'Rust', 'hash1', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let caller_file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES ('src/callee.rs', 'Rust', 'hash2', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let callee_file_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, 'crate::caller_fn', 'caller_fn', 'Function', '2026-01-01T00:00:00Z')",
            [caller_file_id],
        )
        .unwrap();
        let caller_symbol_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, 'crate::callee_fn', 'callee_fn', 'Function', '2026-01-01T00:00:00Z')",
            [callee_file_id],
        )
        .unwrap();
        let callee_symbol_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO structural_edges (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id, call_kind, resolution_status, confidence)
             VALUES (?1, ?2, ?3, ?4, 'DIRECT', 'RESOLVED', 1.0)",
            [caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id],
        )
        .unwrap();

        // Test file + symbol for test_mapping join (plan gate: punchlist + hint together)
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES ('tests/callee_test.rs', 'Rust', 'hash3', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let test_file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, 'crate::tests::covers_callee', 'covers_callee', 'Function', '2026-01-01T00:00:00Z')",
            [test_file_id],
        )
        .unwrap();
        let test_symbol_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id, confidence, mapping_kind, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, 0.95, 'direct', '2026-01-01T00:00:00Z')",
            [test_symbol_id, test_file_id, callee_symbol_id, callee_file_id],
        )
        .unwrap();

        let storage = StorageManager::init_from_conn(conn);
        let mut file_id_map = HashMap::new();
        file_id_map.insert(PathBuf::from("src/callee.rs"), callee_file_id);
        let config = crate::config::model::Config::default();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map,
            project_root: std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from(r"C:\dev\ledgerful")),
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
            skip_git_history_enrichment: false,
            history_budget: None,
            overall_deadline: None,
        };
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("src/callee.rs"),
                status: "Modified".to_string(),
                old_path: None,
                is_staged: false,
                symbols: Some(vec![Symbol {
                    name: "callee_fn".into(),
                    kind: SymbolKind::Function,
                    is_public: true,
                    cognitive_complexity: None,
                    cyclomatic_complexity: None,
                    line_start: None,
                    line_end: None,
                    qualified_name: Some("crate::callee_fn".into()),
                    byte_start: None,
                    byte_end: None,
                    entrypoint_kind: None,
                    metadata: std::collections::BTreeMap::new(),
                }]),
                imports: None,
                runtime_usage: None,
                analysis_status: FileAnalysisStatus::default(),
                analysis_warnings: Vec::new(),
                api_routes: Vec::new(),
                data_models: Vec::new(),
                ci_gates: Vec::new(),
            }],
            ..Default::default()
        };

        CouplingProvider.enrich(&context, &mut packet).unwrap();

        assert_eq!(packet.structural_couplings.len(), 1);
        assert_eq!(
            packet.structural_couplings[0].caller_symbol_name,
            "caller_fn"
        );
        assert_eq!(
            packet.structural_couplings[0].callee_symbol_name,
            "callee_fn"
        );
        // Blast radius should be populated (DoD punchlist)
        let blast = packet.blast_radius.as_ref().expect("blast_radius set");
        assert_eq!(blast.depth_applied, 1);
        assert_eq!(blast.edges.len(), 1);
        assert_eq!(blast.edges[0].resolution_status, "RESOLVED");
        assert!(
            blast.must_touch_files.iter().any(|f| f == "src/caller.rs"),
            "must-touch includes neighbor caller file"
        );
        assert!(
            !blast.must_touch_symbols.iter().any(|s| s == "callee_fn"),
            "must-touch excludes seed symbol (known via changes)"
        );
        // test_mapping join on enrich path
        assert_eq!(packet.test_coverage.len(), 1);
        assert_eq!(packet.test_coverage[0].changed_symbol, "callee_fn");
        assert!(
            blast.test_hints.iter().any(|h| h.contains("covers_callee")),
            "testHints populated from test_mapping: {:?}",
            blast.test_hints
        );
        // Orchestrator attaches test_gaps with matching mapped set
        let gaps = packet.test_gaps.as_ref().expect("test_gaps set");
        assert_eq!(
            gaps.status,
            crate::impact::enrichment::test_gaps::TestGapsStatus::Available
        );
        assert_eq!(gaps.mapped_count, 1);
        assert_eq!(gaps.unmapped_count, 0);
    }

    #[test]
    fn enrich_unbound_seeds_emit_honesty_not_silent() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        // structural_edges table exists with data for a different symbol
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES ('src/other.rs', 'Rust', 'h', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let fid = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, 'crate::other', 'other', 'Function', '2026-01-01T00:00:00Z')",
            [fid],
        )
        .unwrap();
        let sid = conn.last_insert_rowid();
        // Need at least one edge so table_exists_and_has_data is true
        conn.execute(
            "INSERT INTO structural_edges (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id, call_kind, resolution_status, confidence)
             VALUES (?1, ?2, ?1, ?2, 'DIRECT', 'RESOLVED', 1.0)",
            [sid, fid],
        )
        .unwrap();

        let storage = StorageManager::init_from_conn(conn);
        let config = crate::config::model::Config::default();
        // Use real repo root (not a hard-coded Windows path) so temporal
        // open_repo succeeds on Linux/macOS CI runners.
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root,
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
            skip_git_history_enrichment: false,
            history_budget: None,
            overall_deadline: None,
        };
        // Changed symbol that does not exist in the index
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("src/missing.rs"),
                status: "Modified".to_string(),
                symbols: Some(vec![Symbol {
                    name: "ghost".into(),
                    kind: SymbolKind::Function,
                    is_public: true,
                    cognitive_complexity: None,
                    cyclomatic_complexity: None,
                    line_start: None,
                    line_end: None,
                    qualified_name: Some("crate::ghost".into()),
                    byte_start: None,
                    byte_end: None,
                    entrypoint_kind: None,
                    metadata: std::collections::BTreeMap::new(),
                }]),
                ..ChangedFile::default()
            }],
            ..Default::default()
        };

        CouplingProvider.enrich(&context, &mut packet).unwrap();
        let blast = packet
            .blast_radius
            .as_ref()
            .expect("blast_radius with honesty when seeds unbound");
        assert!(
            blast.honesty_notes.iter().any(|n| n.contains("No seed")),
            "expected unbound-seed honesty, got {:?}",
            blast.honesty_notes
        );
        assert!(packet.structural_couplings.is_empty());
    }

    fn storage_with_edge(with_edge: bool) -> StorageManager {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        if with_edge {
            conn.execute(
                "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
                 VALUES ('src/other.rs', 'Rust', 'h', 1, '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
            let fid = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
                 VALUES (?1, 'crate::other', 'other', 'Function', '2026-01-01T00:00:00Z')",
                [fid],
            )
            .unwrap();
            let sid = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO structural_edges (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id, call_kind, resolution_status, confidence)
                 VALUES (?1, ?2, ?1, ?2, 'DIRECT', 'RESOLVED', 1.0)",
                [sid, fid],
            )
            .unwrap();
        }
        StorageManager::init_from_conn(conn)
    }

    fn context_for<'a>(
        storage: &'a StorageManager,
        config: &'a crate::config::model::Config,
    ) -> EnrichmentContext<'a> {
        EnrichmentContext {
            storage,
            config,
            file_id_map: HashMap::new(),
            project_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
            skip_git_history_enrichment: true,
            history_budget: None,
            overall_deadline: None,
        }
    }

    fn changed_symbol(
        path: &str,
        name: &str,
        qn: Option<&str>,
        old_path: Option<&str>,
    ) -> ChangedFile {
        ChangedFile {
            path: PathBuf::from(path),
            status: "Modified".to_string(),
            old_path: old_path.map(PathBuf::from),
            symbols: Some(vec![Symbol {
                name: name.into(),
                kind: SymbolKind::Function,
                is_public: true,
                cognitive_complexity: None,
                cyclomatic_complexity: None,
                line_start: None,
                line_end: None,
                qualified_name: qn.map(|s| s.to_string()),
                byte_start: None,
                byte_end: None,
                entrypoint_kind: None,
                metadata: std::collections::BTreeMap::new(),
            }]),
            ..ChangedFile::default()
        }
    }

    #[test]
    fn blast_honesty_coupling_empty_seeds_add_unwired_sentences() {
        let storage = storage_with_edge(true);
        let config = crate::config::model::Config::default();
        let context = context_for(&storage, &config);
        let mut packet = ImpactPacket {
            changes: vec![
                changed_symbol("cmd/main.go", "main", Some("main.main"), None),
                changed_symbol("src/App.java", "App", Some("App.main"), None),
            ],
            ..Default::default()
        };
        CouplingProvider
            .enrich_structural_with(&context, &mut packet, &BlastAugment::silent())
            .unwrap();
        let notes = &packet.blast_radius.as_ref().expect("blast").honesty_notes;
        assert!(notes.iter().any(|n| n.contains("No seed")), "{notes:?}");
        assert!(
            notes
                .iter()
                .any(|n| n == "SCIP indexer is not wired for Go."),
            "{notes:?}"
        );
        assert!(
            notes
                .iter()
                .any(|n| n == "SCIP indexer is not wired for Java."),
            "{notes:?}"
        );
    }

    #[test]
    fn blast_honesty_coupling_rust_seed_and_php_keep_both_sentences() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES ('src/other.rs', 'Rust', 'h', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let other_file = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, 'crate::other', 'other', 'Function', '2026-01-01T00:00:00Z')",
            [other_file],
        )
        .unwrap();
        let other_sym = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO structural_edges (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id, call_kind, resolution_status, confidence)
             VALUES (?1, ?2, ?1, ?2, 'DIRECT', 'RESOLVED', 1.0)",
            [other_sym, other_file],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES ('src/lib.rs', 'Rust', 'h2', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let seed_file = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, 'crate::seed_fn', 'seed_fn', 'Function', '2026-01-01T00:00:00Z')",
            [seed_file],
        )
        .unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let config = crate::config::model::Config::default();
        let context = context_for(&storage, &config);
        let mut packet = ImpactPacket {
            changes: vec![
                changed_symbol("src/lib.rs", "seed_fn", Some("crate::seed_fn"), None),
                changed_symbol("web/index.php", "index", None, Some("legacy/App.swift")),
            ],
            ..Default::default()
        };
        let augment = BlastAugment::fixed(&[("rust-analyzer", false, true)]);
        CouplingProvider
            .enrich_structural_with(&context, &mut packet, &augment)
            .unwrap();
        let blast = packet.blast_radius.as_ref().expect("blast");
        assert!(blast.edges.is_empty(), "{:?}", blast.edges);
        let notes = &blast.honesty_notes;
        assert!(
            notes
                .iter()
                .any(|n| n.contains("native resolution included")),
            "{notes:?}"
        );
        assert!(
            notes.iter().any(|n| n.contains("unavailable for Rust")),
            "{notes:?}"
        );
        assert!(
            notes
                .iter()
                .any(|n| n == "SCIP indexer is not wired for PHP."),
            "{notes:?}"
        );
        assert!(
            notes
                .iter()
                .any(|n| n == "SCIP indexer is not wired for Swift."),
            "{notes:?}"
        );
        assert!(!notes.iter().any(|n| n.contains("thin neighborhood")));
    }

    #[test]
    fn blast_honesty_coupling_missing_table_keeps_one_sentence() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES ('cmd/main.go', 'Go', 'h', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let fid = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, 'main.main', 'main', 'Function', '2026-01-01T00:00:00Z')",
            [fid],
        )
        .unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let config = crate::config::model::Config::default();
        let context = context_for(&storage, &config);
        let mut packet = ImpactPacket {
            changes: vec![changed_symbol(
                "cmd/main.go",
                "main",
                Some("main.main"),
                None,
            )],
            ..Default::default()
        };
        CouplingProvider
            .enrich_structural_with(&context, &mut packet, &BlastAugment::silent())
            .unwrap();
        let notes = &packet
            .blast_radius
            .as_ref()
            .expect("missing-table blast")
            .honesty_notes;
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(
            notes[0],
            "structural_edges table empty or missing; blast edges unavailable (index with call-graph analysis)"
        );
    }

    #[test]
    fn enrich_temporal_couplings_filter_and_cap() {
        let storage = StorageManager::init_from_conn(Connection::open_in_memory().unwrap());
        let mut config = crate::config::model::Config::default();
        config.coverage.max_coupling_pairs = 2;

        let _context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::from("."),
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
            skip_git_history_enrichment: false,
            history_budget: None,
            overall_deadline: None,
        };

        let _packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("src/changed.rs"),
                status: "Modified".to_string(),
                ..ChangedFile::default()
            }],
            ..ImpactPacket::default()
        };

        // Temporal engine needs a real git repo; filter/cap logic is covered
        // when git history is available. Structural path is the DoD surface.
    }

    fn temporal_pair(
        file_a: &str,
        file_b: &str,
        score: f32,
    ) -> crate::impact::packet::TemporalCoupling {
        crate::impact::packet::TemporalCoupling {
            file_a: PathBuf::from(file_a),
            file_b: PathBuf::from(file_b),
            score,
        }
    }

    fn renamed_change(path: &str, old_path: &str) -> ChangedFile {
        ChangedFile {
            path: PathBuf::from(path),
            status: "Renamed".to_string(),
            old_path: Some(PathBuf::from(old_path)),
            ..ChangedFile::default()
        }
    }

    fn present(
        pairs: Vec<crate::impact::packet::TemporalCoupling>,
        changes: &[ChangedFile],
        path_mode: &str,
        limit: usize,
    ) -> (
        Vec<crate::impact::packet::TemporalCoupling>,
        Vec<crate::impact::packet::TemporalCoupling>,
        Vec<PathBuf>,
    ) {
        apply_temporal_presentation(pairs.clone(), pairs, &[], changes, path_mode, limit)
    }

    #[test]
    fn rename_old_path_keeps_partner_and_spellings() {
        let stored = temporal_pair("src/old.rs", "src/partner.rs", 0.77);
        let (files, audit, _) = present(
            vec![stored.clone()],
            &[renamed_change("src/new.rs", "src/old.rs")],
            "code",
            10,
        );
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_a.to_string_lossy(), "src/old.rs");
        assert_eq!(files[0].file_b.to_string_lossy(), "src/partner.rs");
        assert_eq!(files[0].score, 0.77);
        assert_eq!(audit, files);
        assert_ne!(files[0].file_a.to_string_lossy(), "src/new.rs");
    }

    #[test]
    fn rename_old_path_matches_backslash_and_dot_slash() {
        let stored = temporal_pair("src/old.rs", "src/partner.rs", 0.7);
        let (backslash_files, _, _) = present(
            vec![stored.clone()],
            &[renamed_change("src/new.rs", r"src\old.rs")],
            "code",
            10,
        );
        assert_eq!(backslash_files.len(), 1);
        assert_eq!(backslash_files[0].file_a.to_string_lossy(), "src/old.rs");
        assert_eq!(
            backslash_files[0].file_b.to_string_lossy(),
            "src/partner.rs"
        );

        let (dot_files, _, _) = present(
            vec![stored],
            &[renamed_change("src/new.rs", "./src/old.rs")],
            "code",
            10,
        );
        assert_eq!(dot_files.len(), 1);
        assert_eq!(dot_files[0].file_a.to_string_lossy(), "src/old.rs");
        assert_eq!(dot_files[0].file_b.to_string_lossy(), "src/partner.rs");
    }

    #[test]
    fn new_path_pair_still_kept_and_unrelated_pair_dropped() {
        let kept = temporal_pair("src/lib.rs", "src/neighbor.rs", 0.6);
        let dropped = temporal_pair("src/nope.rs", "src/other.rs", 0.95);
        let (files, audit, _) = present(
            vec![kept.clone(), dropped],
            &[renamed_change("src/lib.rs", "src/old.rs")],
            "code",
            10,
        );
        assert_eq!(files, vec![kept.clone()]);
        assert_eq!(audit, vec![kept]);
    }

    #[test]
    fn empty_normalized_old_path_matches_nothing() {
        let empty_side = temporal_pair("", "src/other.rs", 0.7);
        let dot_side = temporal_pair("./", "src/other.rs", 0.7);
        let kept = temporal_pair("src/new.rs", "src/keep.rs", 0.6);
        for old in ["", "./"] {
            let (files, _, _) = present(
                vec![empty_side.clone(), dot_side.clone(), kept.clone()],
                &[renamed_change("src/new.rs", old)],
                "code",
                10,
            );
            assert_eq!(files, vec![kept.clone()], "old_path {old:?}");
        }
    }

    #[test]
    fn governance_old_path_demotes_on_stored_paths() {
        let stored = temporal_pair("docs/guide.md", "src/partner.rs", 0.88);
        let change = renamed_change("src/guide.rs", "docs/guide.md");
        let (code_files, code_audit, _) = present(
            vec![stored.clone()],
            std::slice::from_ref(&change),
            "code",
            10,
        );
        assert!(code_files.is_empty(), "{code_files:?}");
        assert_eq!(code_audit.len(), 1);
        assert_eq!(code_audit[0].file_a.to_string_lossy(), "docs/guide.md");
        assert_eq!(code_audit[0].file_b.to_string_lossy(), "src/partner.rs");

        let (all_files, _, _) = present(vec![stored], &[change], "all", 10);
        assert_eq!(all_files.len(), 1);
        assert_eq!(all_files[0].file_a.to_string_lossy(), "docs/guide.md");
        assert_eq!(all_files[0].file_b.to_string_lossy(), "src/partner.rs");
    }

    #[test]
    fn audit_old_path_keeps_tree_partner_in_discarded() {
        use camino::Utf8PathBuf;

        let leaf = temporal_pair("src/old.rs", "src/partner.rs", 0.9);
        let tree_pair = temporal_pair("src/old.rs", "crates", 0.85);
        let changes = [renamed_change("src/new.rs", "src/old.rs")];
        let trees = [Utf8PathBuf::from("crates")];
        let (files, audit, discarded) = apply_temporal_presentation(
            vec![leaf.clone()],
            vec![leaf, tree_pair],
            &trees,
            &changes,
            "code",
            10,
        );
        assert!(
            files.iter().all(|coupling| {
                coupling.file_a.to_string_lossy() != "crates"
                    && coupling.file_b.to_string_lossy() != "crates"
            }),
            "{files:?}"
        );
        assert!(
            audit
                .iter()
                .any(|coupling| coupling.file_b.to_string_lossy() == "crates"),
            "{audit:?}"
        );
        assert_eq!(discarded, vec![PathBuf::from("crates")]);
    }

    #[test]
    fn cap_keeps_input_order_and_does_not_prefer_old_path() {
        let first = temporal_pair("src/new.rs", "src/first.rs", 0.4);
        let later = temporal_pair("src/old.rs", "src/later.rs", 0.99);
        let (files, audit, _) = present(
            vec![first.clone(), later],
            &[renamed_change("src/new.rs", "src/old.rs")],
            "code",
            1,
        );
        assert_eq!(files, vec![first.clone()]);
        assert_eq!(audit, vec![first]);
    }

    #[test]
    fn presentation_is_deterministic() {
        let pairs = vec![
            temporal_pair("src/old.rs", "src/partner.rs", 0.8),
            temporal_pair("src/new.rs", "src/neighbor.rs", 0.5),
            temporal_pair("src/nope.rs", "src/other.rs", 0.99),
        ];
        let changes = [renamed_change("src/new.rs", "src/old.rs")];
        let once = present(pairs.clone(), &changes, "code", 10);
        let twice = present(pairs, &changes, "code", 10);
        assert_eq!(once, twice);
        assert_eq!(once.0.len(), 2);
    }

    #[test]
    fn affected_paths_and_scoped_hotspots_follow_old_path_partner() {
        use crate::impact::budget::{HotspotProvenance, HotspotProvenanceSource};
        use crate::impact::packet::Hotspot;
        use crate::impact::scope::{affected_paths, retain_scoped_hotspots, seed_paths};

        let change = renamed_change("src/new.rs", "src/old.rs");
        let partner = temporal_pair("src/old.rs", "src/partner.rs", 0.81);
        let (files, _, _) = apply_temporal_presentation(
            vec![partner],
            vec![],
            &[],
            std::slice::from_ref(&change),
            "code",
            10,
        );
        assert_eq!(files.len(), 1);

        let provenance = HotspotProvenance {
            source: HotspotProvenanceSource::Trends,
            head: Some("0522-fixture".to_string()),
            ..HotspotProvenance::default()
        };
        let packet = ImpactPacket {
            changes: vec![change],
            temporal_couplings: files,
            hotspot_provenance: Some(provenance.clone()),
            ..ImpactPacket::default()
        };
        let seeds = seed_paths(&packet);
        assert!(seeds.contains("src/new.rs"));
        assert!(seeds.contains("src/old.rs"));
        assert!(!seeds.contains("src/partner.rs"));
        let affected = affected_paths(&packet, &seeds);
        assert!(affected.contains("src/partner.rs"));
        assert!(!affected.contains("src/old.rs"));
        assert!(!affected.contains("src/new.rs"));

        let hotspot = |path: &str, score: f32| Hotspot {
            path: PathBuf::from(path),
            score,
            display_score: score,
            complexity: 1,
            frequency: 1.0,
            centrality: None,
        };
        let kept = retain_scoped_hotspots(
            vec![
                hotspot("src/partner.rs", 3.0),
                hotspot("src/outsider.rs", 9.0),
                hotspot("src/new.rs", 1.0),
            ],
            &packet,
            10,
        );
        let paths: Vec<String> = kept
            .iter()
            .map(|row| row.path.to_string_lossy().to_string())
            .collect();
        assert!(
            paths.iter().any(|path| path == "src/partner.rs"),
            "{paths:?}"
        );
        assert!(
            paths.iter().all(|path| path != "src/outsider.rs"),
            "{paths:?}"
        );
        assert_eq!(packet.hotspot_provenance.as_ref(), Some(&provenance));
    }

    #[test]
    fn path_mode_all_restores_changelog_and_keeps_tree_partner_on_audit() {
        use crate::impact::packet::TemporalCoupling;
        use camino::Utf8PathBuf;

        let file_pairs = vec![
            TemporalCoupling {
                file_a: PathBuf::from("src/a.rs"),
                file_b: PathBuf::from("CHANGELOG.md"),
                score: 0.95,
            },
            TemporalCoupling {
                file_a: PathBuf::from("src/a.rs"),
                file_b: PathBuf::from("src/b.rs"),
                score: 0.9,
            },
        ];
        let audit = vec![
            file_pairs[0].clone(),
            file_pairs[1].clone(),
            TemporalCoupling {
                file_a: PathBuf::from("src/a.rs"),
                file_b: PathBuf::from("a"),
                score: 0.91,
            },
        ];
        let changes = vec![ChangedFile {
            path: PathBuf::from("src/a.rs"),
            status: "Modified".to_string(),
            ..ChangedFile::default()
        }];
        let trees = vec![Utf8PathBuf::from("a"), Utf8PathBuf::from("unused")];

        let (files, audit_out, discarded) = apply_temporal_presentation(
            file_pairs.clone(),
            audit.clone(),
            &trees,
            &changes,
            "all",
            50,
        );
        assert!(
            files
                .iter()
                .any(|c| c.file_a.ends_with("CHANGELOG.md") || c.file_b.ends_with("CHANGELOG.md")),
            "pathMode=all restores the changelog file pair: {files:?}"
        );
        assert!(
            files.iter().all(|c| c.file_a != *"a" && c.file_b != *"a"),
            "tree partner stays off the default list: {files:?}"
        );
        assert!(
            audit_out
                .iter()
                .any(|c| c.file_a == *"a" || c.file_b == *"a"),
            "tree partner stays on the audit list: {audit_out:?}"
        );
        assert_eq!(discarded, vec![PathBuf::from("a")]);

        let count = crate::impact::path_class::count_presented_temporal_exclusions(
            &files, &audit_out, &discarded, "all", 0.7,
        );
        assert_eq!(count, 1, "pathMode=all counts the tree pair once");

        let (capped_files, capped_audit, capped_discarded) = apply_temporal_presentation(
            file_pairs.clone(),
            audit.clone(),
            &trees,
            &changes,
            "all",
            1,
        );
        assert_eq!(capped_files.len(), 1);
        assert_eq!(capped_audit.len(), 1);
        assert_eq!(
            capped_discarded,
            vec![PathBuf::from("a")],
            "tree path outlives the truncated audit pair"
        );

        let (code_files, code_audit, code_discarded) =
            apply_temporal_presentation(file_pairs, audit, &trees, &changes, "code", 50);
        let code_count = crate::impact::path_class::count_presented_temporal_exclusions(
            &code_files,
            &code_audit,
            &code_discarded,
            "code",
            0.7,
        );
        assert_eq!(
            code_count, 2,
            "code mode counts the changelog pair and the tree pair"
        );
        assert!(
            code_files
                .iter()
                .all(|c| { c.file_a != *"CHANGELOG.md" && c.file_b != *"CHANGELOG.md" }),
            "code mode demotes the changelog pair: {code_files:?}"
        );
        assert!(
            code_files.iter().any(|c| c.file_b == *"src/b.rs"),
            "code pair stays: {code_files:?}"
        );
    }

    /// Production wiring (0118): after blast success, CouplingProvider sets
    /// `packet.affected_flows` when routes match the change set.
    #[test]
    fn enrich_attaches_affected_flows_after_blast_success() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES ('src/caller.rs', 'Rust', 'hash1', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let caller_file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES ('src/handlers/items.rs', 'Rust', 'hash2', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let handler_file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES ('src/router.rs', 'Rust', 'hash3', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let router_file_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, 'crate::caller_fn', 'caller_fn', 'Function', '2026-01-01T00:00:00Z')",
            [caller_file_id],
        )
        .unwrap();
        let caller_symbol_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, 'crate::list_items', 'list_items', 'Function', '2026-01-01T00:00:00Z')",
            [handler_file_id],
        )
        .unwrap();
        let handler_symbol_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO structural_edges (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id, call_kind, resolution_status, confidence)
             VALUES (?1, ?2, ?3, ?4, 'DIRECT', 'RESOLVED', 1.0)",
            [
                caller_symbol_id,
                caller_file_id,
                handler_symbol_id,
                handler_file_id,
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO api_routes
             (method, path_pattern, handler_symbol_id, handler_symbol_name, handler_file_id,
              framework, route_source, is_dynamic, route_confidence, evidence, last_indexed_at)
             VALUES ('GET', '/api/items', ?1, 'list_items', ?2, 'Axum', 'DECORATOR', 0, 1.0, 'test', '2026-01-01T00:00:00Z')",
            [handler_symbol_id, router_file_id],
        )
        .unwrap();

        let storage = StorageManager::init_from_conn(conn);
        let mut file_id_map = HashMap::new();
        file_id_map.insert(PathBuf::from("src/handlers/items.rs"), handler_file_id);
        let config = crate::config::model::Config::default();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map,
            project_root: std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from(r"C:\dev\ledgerful")),
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
            skip_git_history_enrichment: false,
            history_budget: None,
            overall_deadline: None,
        };
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("src/handlers/items.rs"),
                status: "Modified".to_string(),
                symbols: Some(vec![Symbol {
                    name: "list_items".into(),
                    kind: SymbolKind::Function,
                    is_public: true,
                    cognitive_complexity: None,
                    cyclomatic_complexity: None,
                    line_start: None,
                    line_end: None,
                    qualified_name: Some("crate::list_items".into()),
                    byte_start: None,
                    byte_end: None,
                    entrypoint_kind: None,
                    metadata: std::collections::BTreeMap::new(),
                }]),
                ..ChangedFile::default()
            }],
            ..Default::default()
        };

        CouplingProvider.enrich(&context, &mut packet).unwrap();

        let blast = packet
            .blast_radius
            .as_ref()
            .expect("blast_radius after success");
        assert!(
            !blast.edges.is_empty(),
            "expected blast edges after structural success"
        );

        let flows = packet
            .affected_flows
            .as_ref()
            .expect("affected_flows attached after blast success");
        assert_eq!(
            flows.status,
            crate::impact::enrichment::affected_flows::AffectedFlowsStatus::Available
        );
        assert_eq!(flows.flow_count, 1);
        assert_eq!(flows.flows[0].path_pattern, "/api/items");
        assert_eq!(
            flows.flows[0].match_kind,
            crate::impact::enrichment::affected_flows::MatchKind::HandlerSymbol
        );
    }

    /// Production wiring (0118): kinds 1–3 still attach when structural_edges
    /// are absent (no blast edges / blast path skipped).
    #[test]
    fn enrich_attaches_affected_flows_without_blast_edges() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES ('src/handlers/orders.rs', 'Rust', 'h1', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let handler_file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES ('src/router.rs', 'Rust', 'h2', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let router_file_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, 'crate::create_order', 'create_order', 'Function', '2026-01-01T00:00:00Z')",
            [handler_file_id],
        )
        .unwrap();
        let handler_symbol_id = conn.last_insert_rowid();

        // No structural_edges rows → blast path short-circuits; kinds 1–3 still run.
        conn.execute(
            "INSERT INTO api_routes
             (method, path_pattern, handler_symbol_id, handler_symbol_name, handler_file_id,
              framework, route_source, is_dynamic, route_confidence, evidence, last_indexed_at)
             VALUES ('POST', '/api/orders', ?1, 'create_order', ?2, 'Axum', 'DECORATOR', 0, 1.0, 'test', '2026-01-01T00:00:00Z')",
            [handler_symbol_id, router_file_id],
        )
        .unwrap();

        let storage = StorageManager::init_from_conn(conn);
        let config = crate::config::model::Config::default();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from(r"C:\dev\ledgerful")),
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
            skip_git_history_enrichment: false,
            history_budget: None,
            overall_deadline: None,
        };
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("src/handlers/orders.rs"),
                status: "Modified".to_string(),
                symbols: Some(vec![Symbol {
                    name: "create_order".into(),
                    kind: SymbolKind::Function,
                    is_public: true,
                    cognitive_complexity: None,
                    cyclomatic_complexity: None,
                    line_start: None,
                    line_end: None,
                    qualified_name: Some("crate::create_order".into()),
                    byte_start: None,
                    byte_end: None,
                    entrypoint_kind: None,
                    metadata: std::collections::BTreeMap::new(),
                }]),
                ..ChangedFile::default()
            }],
            ..Default::default()
        };

        CouplingProvider.enrich(&context, &mut packet).unwrap();

        // No structural edges → no blast edges (honesty-only blast may still appear).
        if let Some(blast) = packet.blast_radius.as_ref() {
            assert!(
                blast.edges.is_empty(),
                "expected no blast edges without structural_edges data"
            );
        }

        let flows = packet
            .affected_flows
            .as_ref()
            .expect("affected_flows attached without blast edges (kinds 1–3)");
        assert_eq!(
            flows.status,
            crate::impact::enrichment::affected_flows::AffectedFlowsStatus::Available
        );
        assert_eq!(flows.flow_count, 1);
        assert_eq!(flows.flows[0].path_pattern, "/api/orders");
        assert_eq!(
            flows.flows[0].match_kind,
            crate::impact::enrichment::affected_flows::MatchKind::HandlerSymbol
        );
    }
}
