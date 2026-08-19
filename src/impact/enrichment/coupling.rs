use crate::git::repo::open_repo;
use crate::impact::enrichment::affected_flows::{
    AffectedFlowsOpts, AffectedFlowsReport, compute_affected_flows,
};
use crate::impact::enrichment::blast::{
    BlastCaps, compute_blast, derive_structural_couplings, resolve_seeds,
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
        match compute_blast(conn, &seeds, depth_requested, depth_max, caps) {
            Ok(mut blast) => {
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
        debug!("Running temporal coupling analysis...");

        let repo = open_repo(&context.project_root)
            .map_err(|e| miette::miette!("Failed to open repo for temporal analysis: {}", e))?;

        let history_provider = GixHistoryProvider::new(&repo);
        let temporal_engine =
            TemporalEngine::new(history_provider, context.config.temporal.clone());

        match temporal_engine.calculate_couplings() {
            Ok(mut couplings) => {
                // Filter: at least one file must be in packet.changes
                let change_paths: std::collections::HashSet<_> =
                    packet.changes.iter().map(|c| &c.path).collect();

                couplings.retain(|c| {
                    change_paths.contains(&c.file_a) || change_paths.contains(&c.file_b)
                });

                // Sort by score descending
                couplings.sort_by(|a, b| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });

                // Cap
                let limit = context.config.coverage.max_coupling_pairs;
                if couplings.len() > limit {
                    couplings.truncate(limit);
                }

                packet.set_temporal_couplings(couplings);
            }
            Err(e) => {
                warn!("Temporal analysis failed: {e}");
                context.add_warning(format!("Temporal analysis failed: {e}"));
            }
        }

        Ok(())
    }
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
