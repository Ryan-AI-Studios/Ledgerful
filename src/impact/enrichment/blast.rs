//! Bounded structural blast radius over SQLite `structural_edges`.
//!
//! Depth-1 default; hop N+1 only from nodes reached via high-confidence discovery
//! edges (RESOLVED or evidence `scip:`), along high-confidence expansion edges.
//! Seed join is always file_path+symbol_name and/or qualified_name — never bare name.

use crate::impact::enrichment::edge_confidence::{
    EdgeConfidenceSummary, confidence_class, is_high_confidence as edge_is_high_confidence,
};
use crate::impact::packet::{
    BlastEdge, BlastRadius, CoveringTest, ImpactPacket, StructuralCoupling, TestCoverage,
};
use miette::{IntoDiagnostic, Result};
use rusqlite::{Connection, params_from_iter};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

/// High-confidence edge: non-null callee is assumed by caller; status/evidence rule.
/// Thin wrapper over the shared classifier (0117).
pub use super::edge_confidence::is_high_confidence;

/// A seed symbol bound to the index (never bare-name alone).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seed {
    pub symbol_id: i64,
    pub name: String,
    pub file_path: String,
    pub qualified_name: Option<String>,
}

/// Caps for blast traversal.
#[derive(Debug, Clone, Copy)]
pub struct BlastCaps {
    pub fanout_per_hop: usize,
    pub total_edges: usize,
}

impl Default for BlastCaps {
    fn default() -> Self {
        Self {
            fanout_per_hop: 50,
            total_edges: 200,
        }
    }
}

/// Absolute product ceiling (config may request up to this; never higher).
pub const BLAST_ABSOLUTE_CEILING: u32 = 3;

/// CLI-facing max depth (hard product rule).
pub const BLAST_CLI_MAX: u32 = 2;

/// Normalize path separators for deterministic seed matching.
pub fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

/// Resolve seed symbols from the impact packet against the index.
///
/// Matching order per changed symbol:
/// 1. `qualified_name` exact match when present
/// 2. `file_path` + `symbol_name`
///
/// Never matches project-wide bare `symbol_name` alone.
pub fn resolve_seeds(packet: &ImpactPacket, conn: &Connection) -> Result<Vec<Seed>> {
    let mut seeds: Vec<Seed> = Vec::new();
    let mut seen_ids: BTreeSet<i64> = BTreeSet::new();

    let mut qn_stmt = conn
        .prepare(
            "SELECT ps.id, ps.symbol_name, pf.file_path, ps.qualified_name
             FROM project_symbols ps
             JOIN project_files pf ON ps.file_id = pf.id
             WHERE ps.qualified_name = ?1",
        )
        .into_diagnostic()?;
    let mut file_name_stmt = conn
        .prepare(
            "SELECT ps.id, ps.symbol_name, pf.file_path, ps.qualified_name
             FROM project_symbols ps
             JOIN project_files pf ON ps.file_id = pf.id
             WHERE pf.file_path = ?1 AND ps.symbol_name = ?2",
        )
        .into_diagnostic()?;

    for change in &packet.changes {
        let Some(symbols) = change.symbols.as_ref() else {
            continue;
        };
        let file_path = normalize_path(&change.path.to_string_lossy());

        for symbol in symbols {
            // Prefer qualified_name when present.
            if let Some(ref qn) = symbol.qualified_name
                && !qn.is_empty()
            {
                {
                    let rows = qn_stmt
                        .query_map([qn.as_str()], |row| {
                            Ok(Seed {
                                symbol_id: row.get(0)?,
                                name: row.get(1)?,
                                file_path: normalize_path(&row.get::<_, String>(2)?),
                                qualified_name: row.get(3)?,
                            })
                        })
                        .into_diagnostic()?
                        .collect::<Result<Vec<_>, _>>()
                        .into_diagnostic()?;
                    for seed in rows {
                        if seen_ids.insert(seed.symbol_id) {
                            seeds.push(seed);
                        }
                    }
                }
                // If QN matched, skip file+name for this symbol.
                if seeds
                    .iter()
                    .any(|s| s.qualified_name.as_deref() == Some(qn.as_str()))
                {
                    continue;
                }
            }

            // file_path + symbol_name (mandatory; never bare name alone)
            {
                let rows = file_name_stmt
                    .query_map([file_path.as_str(), symbol.name.as_str()], |row| {
                        Ok(Seed {
                            symbol_id: row.get(0)?,
                            name: row.get(1)?,
                            file_path: normalize_path(&row.get::<_, String>(2)?),
                            qualified_name: row.get(3)?,
                        })
                    })
                    .into_diagnostic()?
                    .collect::<Result<Vec<_>, _>>()
                    .into_diagnostic()?;
                for seed in rows {
                    if seen_ids.insert(seed.symbol_id) {
                        seeds.push(seed);
                    }
                }
            }
        }
    }

    seeds.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.symbol_id.cmp(&b.symbol_id))
    });
    Ok(seeds)
}

/// Raw edge row before collapse / punchlist assembly.
#[derive(Debug, Clone)]
struct RawEdge {
    caller_id: i64,
    caller_name: String,
    caller_file: String,
    callee_id: i64,
    callee_name: String,
    callee_file: String,
    resolution_status: String,
    evidence: String,
    confidence: Option<f64>,
}

fn query_callers_of(
    conn: &Connection,
    callee_ids: &[i64],
    high_confidence_only: bool,
) -> Result<Vec<RawEdge>> {
    if callee_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders: String = callee_ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(", ");

    let conf_filter = if high_confidence_only {
        " AND (se.resolution_status = 'RESOLVED' OR IFNULL(se.evidence, '') LIKE 'scip:%')"
    } else {
        ""
    };

    let sql = format!(
        "SELECT se.caller_symbol_id, ps_caller.symbol_name, pf_caller.file_path,
                se.callee_symbol_id, ps_callee.symbol_name, pf_callee.file_path,
                se.resolution_status, IFNULL(se.evidence, ''), se.confidence
         FROM structural_edges se
         JOIN project_symbols ps_caller ON se.caller_symbol_id = ps_caller.id
         JOIN project_files pf_caller ON se.caller_file_id = pf_caller.id
         JOIN project_symbols ps_callee ON se.callee_symbol_id = ps_callee.id
         JOIN project_files pf_callee ON se.callee_file_id = pf_callee.id
         WHERE se.callee_symbol_id IN ({placeholders})
           AND se.callee_symbol_id IS NOT NULL
           {conf_filter}"
    );

    let mut stmt = conn.prepare(&sql).into_diagnostic()?;
    let rows = stmt
        .query_map(params_from_iter(callee_ids.iter().copied()), |row| {
            let conf: f64 = row.get(8)?;
            Ok(RawEdge {
                caller_id: row.get(0)?,
                caller_name: row.get(1)?,
                caller_file: normalize_path(&row.get::<_, String>(2)?),
                callee_id: row.get(3)?,
                callee_name: row.get(4)?,
                callee_file: normalize_path(&row.get::<_, String>(5)?),
                resolution_status: row.get(6)?,
                evidence: row.get(7)?,
                confidence: Some(conf),
            })
        })
        .into_diagnostic()?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row.into_diagnostic()?);
    }
    Ok(out)
}

/// Collapse duplicate (caller, callee) pairs preferring highest-confidence row.
fn collapse_pairs(edges: Vec<RawEdge>) -> Vec<RawEdge> {
    let mut best: HashMap<(i64, i64), RawEdge> = HashMap::new();
    for edge in edges {
        let key = (edge.caller_id, edge.callee_id);
        match best.get(&key) {
            None => {
                best.insert(key, edge);
            }
            Some(existing) => {
                let new_p =
                    confidence_class(&edge.resolution_status, &edge.evidence).collapse_priority();
                let old_p = confidence_class(&existing.resolution_status, &existing.evidence)
                    .collapse_priority();
                // Prefer higher priority; on ties pick deterministic total order
                // (evidence, then resolution_status) so serde/JSON is stable.
                if new_p > old_p
                    || (new_p == old_p
                        && (&edge.evidence, &edge.resolution_status)
                            < (&existing.evidence, &existing.resolution_status))
                {
                    best.insert(key, edge);
                }
            }
        }
    }
    let mut collapsed: Vec<RawEdge> = best.into_values().collect();
    collapsed.sort_by(|a, b| {
        a.caller_file
            .cmp(&b.caller_file)
            .then_with(|| a.caller_name.cmp(&b.caller_name))
            .then_with(|| a.callee_file.cmp(&b.callee_file))
            .then_with(|| a.callee_name.cmp(&b.callee_name))
    });
    collapsed
}

fn raw_to_blast_edge(raw: &RawEdge, hop: u32) -> BlastEdge {
    // Class computed only on post-collapse survivors (callers pass collapsed RawEdge).
    let class = confidence_class(&raw.resolution_status, &raw.evidence);
    let expandable = edge_is_high_confidence(&raw.resolution_status, &raw.evidence);
    BlastEdge {
        hop,
        direction: "caller".to_string(),
        from_symbol: raw.caller_name.clone(),
        from_file: raw.caller_file.clone(),
        to_symbol: raw.callee_name.clone(),
        to_file: raw.callee_file.clone(),
        resolution_status: raw.resolution_status.clone(),
        evidence: raw.evidence.clone(),
        confidence: raw.confidence,
        expandable,
        confidence_class: class,
    }
}

/// Compute bounded blast radius from bound seeds.
pub fn compute_blast(
    conn: &Connection,
    seeds: &[Seed],
    depth_requested: u32,
    depth_max: u32,
    caps: BlastCaps,
) -> Result<BlastRadius> {
    let mut honesty_notes: Vec<String> = Vec::new();

    let ceiling = depth_max.clamp(1, BLAST_ABSOLUTE_CEILING);
    let depth_applied = depth_requested.clamp(1, ceiling);
    if depth_requested > ceiling {
        honesty_notes.push(format!(
            "Requested blast depth {depth_requested} clamped to {depth_applied} (ceiling {ceiling})"
        ));
    }

    let mut result = BlastRadius {
        depth_requested,
        depth_applied,
        edges: Vec::new(),
        must_touch_files: Vec::new(),
        must_touch_symbols: Vec::new(),
        test_hints: Vec::new(),
        honesty_notes: Vec::new(),
        confidence_summary: EdgeConfidenceSummary::default(),
    };

    if seeds.is_empty() {
        result.honesty_notes.push(
            "No seed symbols bound to the index (file+name or qualified_name required)".to_string(),
        );
        result.honesty_notes.extend(honesty_notes);
        result.honesty_notes.sort_unstable();
        result.honesty_notes.dedup();
        return Ok(result);
    }

    let seed_ids: Vec<i64> = seeds.iter().map(|s| s.symbol_id).collect();
    let total_budget = caps.total_edges;
    let mut all_edges: Vec<BlastEdge> = Vec::new();

    // --- Hop 1: reverse callers of seeds (all resolution statuses with bound callee) ---
    let hop1_raw = collapse_pairs(query_callers_of(conn, &seed_ids, false)?);
    let hop1_capped = apply_fanout(hop1_raw, caps.fanout_per_hop, &mut honesty_notes, 1);
    let mut expandable_ids: BTreeSet<i64> = BTreeSet::new();

    for raw in &hop1_capped {
        if all_edges.len() >= total_budget {
            honesty_notes.push(format!(
                "Total edge cap ({total_budget}) hit at hop 1; remaining edges omitted"
            ));
            break;
        }
        let edge = raw_to_blast_edge(raw, 1);
        if edge.expandable {
            expandable_ids.insert(raw.caller_id);
        }
        all_edges.push(edge);
    }

    // Honesty for AMBIGUOUS listed but not expanded
    let ambig_count = all_edges
        .iter()
        .filter(|e| e.hop == 1 && e.resolution_status == "AMBIGUOUS")
        .count();
    if ambig_count > 0 {
        honesty_notes.push(format!(
            "{ambig_count} AMBIGUOUS hop-1 edge(s) listed but not expanded transitively"
        ));
    }

    // --- Hop 2+: only from high-confidence-reached nodes, high-confidence edges ---
    if depth_applied >= 2 && !expandable_ids.is_empty() && all_edges.len() < total_budget {
        let mut frontier: Vec<i64> = expandable_ids.iter().copied().collect();
        frontier.sort_unstable();

        for hop in 2..=depth_applied {
            if frontier.is_empty() || all_edges.len() >= total_budget {
                break;
            }
            let hop_raw = collapse_pairs(query_callers_of(
                conn, &frontier, true, /* high conf only */
            )?);
            let hop_capped = apply_fanout(hop_raw, caps.fanout_per_hop, &mut honesty_notes, hop);

            let mut next_frontier: BTreeSet<i64> = BTreeSet::new();
            for raw in &hop_capped {
                if all_edges.len() >= total_budget {
                    honesty_notes.push(format!(
                        "Total edge cap ({total_budget}) hit at hop {hop}; remaining edges omitted"
                    ));
                    break;
                }
                // Skip self-loops already listed as same pair at earlier hop? keep multi-hop path
                let edge = raw_to_blast_edge(raw, hop);
                if edge.expandable {
                    next_frontier.insert(raw.caller_id);
                }
                all_edges.push(edge);
            }
            frontier = next_frontier.into_iter().collect();
            frontier.sort_unstable();
        }
    } else if depth_applied >= 2 && expandable_ids.is_empty() {
        honesty_notes
            .push("Hop >1 requested but no high-confidence hop-1 nodes to expand".to_string());
    }

    // Thin neighborhood honesty (DoD-7): bound seeds but no reverse callers.
    if all_edges.is_empty() && !seeds.is_empty() {
        honesty_notes.push(
            "No reverse callers found for bound seeds (thin neighborhood; try `ledgerful index --auto-scip` for higher-precision edges — opt-in, not default)"
                .to_string(),
        );
    }

    // Punchlist: neighbors only — seeds are already known via packet.changes.
    // Filter seed names/files on *both* sides so hop-2 cycles (seed ← A ← seed)
    // cannot reintroduce the changed seed into must-touch (codex R2 P2).
    let seed_names: BTreeSet<&str> = seeds.iter().map(|s| s.name.as_str()).collect();
    let seed_files: BTreeSet<&str> = seeds.iter().map(|s| s.file_path.as_str()).collect();
    let mut files: BTreeSet<String> = BTreeSet::new();
    let mut symbols: BTreeSet<String> = BTreeSet::new();
    for e in &all_edges {
        if !seed_files.contains(e.from_file.as_str()) {
            files.insert(e.from_file.clone());
        }
        if !seed_names.contains(e.from_symbol.as_str()) {
            symbols.insert(e.from_symbol.clone());
        }
        if !seed_files.contains(e.to_file.as_str()) {
            files.insert(e.to_file.clone());
        }
        if !seed_names.contains(e.to_symbol.as_str()) {
            symbols.insert(e.to_symbol.clone());
        }
    }

    all_edges.sort_unstable();
    result.confidence_summary = EdgeConfidenceSummary::from_blast_edges(&all_edges);
    result.edges = all_edges;
    result.must_touch_files = files.into_iter().collect();
    result.must_touch_symbols = symbols.into_iter().collect();
    honesty_notes.sort_unstable();
    honesty_notes.dedup();
    result.honesty_notes = honesty_notes;
    Ok(result)
}

fn apply_fanout(
    mut edges: Vec<RawEdge>,
    fanout: usize,
    notes: &mut Vec<String>,
    hop: u32,
) -> Vec<RawEdge> {
    if edges.len() > fanout {
        notes.push(format!(
            "Hop {hop} fan-out capped at {fanout} (had {} edges after pair collapse)",
            edges.len()
        ));
        edges.truncate(fanout);
    }
    edges
}

/// Derive legacy structural_couplings from hop-1 reverse-caller edges (single writer).
pub fn derive_structural_couplings(blast: &BlastRadius) -> Vec<StructuralCoupling> {
    let mut out: Vec<StructuralCoupling> = blast
        .edges
        .iter()
        .filter(|e| e.hop == 1 && e.direction == "caller")
        .map(|e| StructuralCoupling {
            caller_symbol_name: e.from_symbol.clone(),
            callee_symbol_name: e.to_symbol.clone(),
            caller_file_path: PathBuf::from(&e.from_file),
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// First production writer for `test_coverage` / test hints from `test_mapping`.
pub fn populate_test_coverage(
    conn: &Connection,
    seeds: &[Seed],
) -> Result<(Vec<TestCoverage>, Vec<String>)> {
    if seeds.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    // Fail soft if table missing
    let table_ok: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='test_mapping'",
            [],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        )
        .unwrap_or(false);
    if !table_ok {
        return Ok((Vec::new(), Vec::new()));
    }

    let seed_ids: Vec<i64> = seeds.iter().map(|s| s.symbol_id).collect();
    let placeholders: String = seed_ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(", ");

    let sql = format!(
        "SELECT ps_tested.symbol_name, pf_tested.file_path,
                ps_test.symbol_name, pf_test.file_path,
                tm.confidence, tm.mapping_kind
         FROM test_mapping tm
         JOIN project_symbols ps_tested ON tm.tested_symbol_id = ps_tested.id
         JOIN project_files pf_tested ON tm.tested_file_id = pf_tested.id
         JOIN project_symbols ps_test ON tm.test_symbol_id = ps_test.id
         JOIN project_files pf_test ON tm.test_file_id = pf_test.id
         WHERE tm.tested_symbol_id IN ({placeholders})"
    );

    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return Ok((Vec::new(), Vec::new())),
    };

    let rows = match stmt.query_map(params_from_iter(seed_ids.iter().copied()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            normalize_path(&row.get::<_, String>(1)?),
            row.get::<_, String>(2)?,
            normalize_path(&row.get::<_, String>(3)?),
            row.get::<_, f64>(4)?,
            row.get::<_, String>(5)?,
        ))
    }) {
        Ok(r) => r,
        Err(_) => return Ok((Vec::new(), Vec::new())),
    };

    // Group covering tests by (changed_symbol, changed_file)
    let mut grouped: BTreeMap<(String, String), Vec<CoveringTest>> = BTreeMap::new();
    let mut hints: BTreeSet<String> = BTreeSet::new();

    for row in rows {
        let Ok((changed_sym, changed_file, test_sym, test_file, conf, kind)) = row else {
            continue;
        };
        hints.insert(format!("{test_file}::{test_sym}"));
        grouped
            .entry((changed_sym, changed_file))
            .or_default()
            .push(CoveringTest {
                test_file,
                test_symbol: test_sym,
                confidence: conf,
                mapping_kind: kind,
            });
    }

    let mut coverage: Vec<TestCoverage> = grouped
        .into_iter()
        .map(|((changed_symbol, changed_file), mut covering_tests)| {
            covering_tests.sort_unstable();
            TestCoverage {
                changed_symbol,
                changed_file,
                covering_tests,
            }
        })
        .collect();
    coverage.sort_unstable();
    let test_hints: Vec<String> = hints.into_iter().collect();
    Ok((coverage, test_hints))
}

/// Apply CLI `--blast-depth` to config: clamp to 1..=min(2, config.blast_depth_max, 3).
/// Returns honesty note when clamped from a higher request.
pub fn apply_cli_blast_depth(
    config_depth: &mut u32,
    config_max: u32,
    requested: Option<u32>,
) -> Option<String> {
    let Some(n) = requested else {
        // Still clamp config defaults to absolute ceiling
        let ceiling = config_max.clamp(1, BLAST_ABSOLUTE_CEILING);
        *config_depth = (*config_depth).clamp(1, ceiling);
        return None;
    };

    let cli_ceiling = BLAST_CLI_MAX
        .min(config_max)
        .clamp(1, BLAST_ABSOLUTE_CEILING);
    let applied = if n == 0 { 1 } else { n.min(cli_ceiling) };
    let note = if n > cli_ceiling {
        Some(format!(
            "CLI --blast-depth {n} clamped to {applied} (CLI max {cli_ceiling})"
        ))
    } else if n == 0 {
        Some("CLI --blast-depth 0 treated as 1".to_string())
    } else {
        None
    };
    *config_depth = applied;
    note
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::{ChangedFile, FileAnalysisStatus};
    use crate::index::symbols::{Symbol, SymbolKind};
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    fn setup_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn
    }

    fn insert_file(conn: &Connection, path: &str) -> i64 {
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, 'Rust', 'h', 1, '2026-01-01T00:00:00Z')",
            [path],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_symbol(conn: &Connection, file_id: i64, name: &str, qn: &str) -> i64 {
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at)
             VALUES (?1, ?2, ?3, 'Function', '2026-01-01T00:00:00Z')",
            rusqlite::params![file_id, qn, name],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn insert_edge(
        conn: &Connection,
        caller_id: i64,
        caller_file: i64,
        callee_id: i64,
        callee_file: i64,
        status: &str,
        evidence: &str,
    ) {
        conn.execute(
            "INSERT INTO structural_edges
             (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id,
              call_kind, resolution_status, confidence, evidence)
             VALUES (?1, ?2, ?3, ?4, 'DIRECT', ?5, 1.0, ?6)",
            rusqlite::params![
                caller_id,
                caller_file,
                callee_id,
                callee_file,
                status,
                evidence
            ],
        )
        .unwrap();
    }

    fn packet_with_symbol(path: &str, name: &str, qn: Option<&str>) -> ImpactPacket {
        ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from(path),
                status: "Modified".to_string(),
                old_path: None,
                is_staged: false,
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
                imports: None,
                runtime_usage: None,
                analysis_status: FileAnalysisStatus::default(),
                analysis_warnings: Vec::new(),
                api_routes: Vec::new(),
                data_models: Vec::new(),
                ci_gates: Vec::new(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn blast_resolved_listed() {
        let conn = setup_conn();
        let seed_f = insert_file(&conn, "src/seed.rs");
        let caller_f = insert_file(&conn, "src/caller.rs");
        let seed = insert_symbol(&conn, seed_f, "seed_fn", "crate::seed_fn");
        let caller = insert_symbol(&conn, caller_f, "caller_fn", "crate::caller_fn");
        insert_edge(
            &conn,
            caller,
            caller_f,
            seed,
            seed_f,
            "RESOLVED",
            "call_expr",
        );

        let packet = packet_with_symbol("src/seed.rs", "seed_fn", Some("crate::seed_fn"));
        let seeds = resolve_seeds(&packet, &conn).unwrap();
        assert_eq!(seeds.len(), 1);

        let blast = compute_blast(&conn, &seeds, 1, 3, BlastCaps::default()).unwrap();
        assert_eq!(blast.depth_applied, 1);
        assert_eq!(blast.edges.len(), 1);
        assert_eq!(blast.edges[0].from_symbol, "caller_fn");
        assert_eq!(blast.edges[0].resolution_status, "RESOLVED");
        assert!(blast.edges[0].expandable);
        assert!(blast.must_touch_files.iter().any(|f| f == "src/caller.rs"));
    }

    #[test]
    fn blast_hop2_cycle_does_not_reintroduce_seed_in_must_touch() {
        // seed ← A RESOLVED; A ← seed RESOLVED (cycle). Edge list may include seed as
        // hop-2 caller, but must-touch must not list the seed file/symbol.
        let conn = setup_conn();
        let seed_f = insert_file(&conn, "src/seed.rs");
        let a_f = insert_file(&conn, "src/a.rs");
        let seed = insert_symbol(&conn, seed_f, "seed_fn", "crate::seed_fn");
        let a = insert_symbol(&conn, a_f, "a_fn", "crate::a_fn");
        insert_edge(&conn, a, a_f, seed, seed_f, "RESOLVED", "call_expr");
        insert_edge(&conn, seed, seed_f, a, a_f, "RESOLVED", "scip:ref");

        let packet = packet_with_symbol("src/seed.rs", "seed_fn", Some("crate::seed_fn"));
        let seeds = resolve_seeds(&packet, &conn).unwrap();
        let blast = compute_blast(&conn, &seeds, 2, 3, BlastCaps::default()).unwrap();

        assert!(
            !blast.must_touch_files.iter().any(|f| f == "src/seed.rs"),
            "seed file must not re-enter mustTouch via hop-2 cycle: {:?}",
            blast.must_touch_files
        );
        assert!(
            !blast.must_touch_symbols.iter().any(|s| s == "seed_fn"),
            "seed symbol must not re-enter mustTouch via hop-2 cycle: {:?}",
            blast.must_touch_symbols
        );
        assert!(
            blast.must_touch_files.iter().any(|f| f == "src/a.rs"),
            "neighbor a.rs still listed"
        );
    }

    #[test]
    fn blast_hop2_expands_only_high_confidence_path() {
        let conn = setup_conn();
        let seed_f = insert_file(&conn, "src/seed.rs");
        let a_f = insert_file(&conn, "src/a.rs");
        let b_f = insert_file(&conn, "src/b.rs");
        let seed = insert_symbol(&conn, seed_f, "seed_fn", "crate::seed_fn");
        let a = insert_symbol(&conn, a_f, "a_fn", "crate::a_fn");
        let b = insert_symbol(&conn, b_f, "b_fn", "crate::b_fn");
        // seed ← A RESOLVED; A ← B RESOLVED → B at hop-2
        insert_edge(&conn, a, a_f, seed, seed_f, "RESOLVED", "call_expr");
        insert_edge(&conn, b, b_f, a, a_f, "RESOLVED", "scip:ref");

        let packet = packet_with_symbol("src/seed.rs", "seed_fn", Some("crate::seed_fn"));
        let seeds = resolve_seeds(&packet, &conn).unwrap();
        let blast = compute_blast(&conn, &seeds, 2, 3, BlastCaps::default()).unwrap();

        assert!(
            blast
                .edges
                .iter()
                .any(|e| e.hop == 1 && e.from_symbol == "a_fn")
        );
        assert!(
            blast
                .edges
                .iter()
                .any(|e| e.hop == 2 && e.from_symbol == "b_fn"),
            "B must appear at hop-2 when discovery edge was high-confidence"
        );
    }

    #[test]
    fn blast_ambiguous_hop1_listed_not_expanded() {
        let conn = setup_conn();
        let seed_f = insert_file(&conn, "src/seed.rs");
        let a_f = insert_file(&conn, "src/a.rs");
        let b_f = insert_file(&conn, "src/b.rs");
        let seed = insert_symbol(&conn, seed_f, "seed_fn", "crate::seed_fn");
        let a = insert_symbol(&conn, a_f, "a_fn", "crate::a_fn");
        let b = insert_symbol(&conn, b_f, "b_fn", "crate::b_fn");
        // seed ← A AMBIGUOUS; A ← B RESOLVED (transitive trap)
        insert_edge(&conn, a, a_f, seed, seed_f, "AMBIGUOUS", "call_expr");
        insert_edge(&conn, b, b_f, a, a_f, "RESOLVED", "call_expr");

        let packet = packet_with_symbol("src/seed.rs", "seed_fn", Some("crate::seed_fn"));
        let seeds = resolve_seeds(&packet, &conn).unwrap();
        let blast = compute_blast(&conn, &seeds, 2, 3, BlastCaps::default()).unwrap();

        assert!(
            blast.edges.iter().any(|e| e.hop == 1
                && e.from_symbol == "a_fn"
                && e.resolution_status == "AMBIGUOUS")
        );
        assert!(
            !blast.edges.iter().any(|e| e.from_symbol == "b_fn"),
            "B must not appear at hop-2 when A was AMBIGUOUS"
        );
        assert!(
            blast.honesty_notes.iter().any(|n| n.contains("AMBIGUOUS")),
            "honesty notes should mention AMBIGUOUS non-expansion"
        );
    }

    #[test]
    fn blast_unresolved_and_capped_never_expand() {
        let conn = setup_conn();
        let seed_f = insert_file(&conn, "src/seed.rs");
        let u_f = insert_file(&conn, "src/u.rs");
        let c_f = insert_file(&conn, "src/c.rs");
        let outer_f = insert_file(&conn, "src/outer.rs");
        let seed = insert_symbol(&conn, seed_f, "seed_fn", "crate::seed_fn");
        let u = insert_symbol(&conn, u_f, "u_fn", "crate::u_fn");
        let c = insert_symbol(&conn, c_f, "c_fn", "crate::c_fn");
        let outer = insert_symbol(&conn, outer_f, "outer_fn", "crate::outer_fn");
        insert_edge(&conn, u, u_f, seed, seed_f, "UNRESOLVED", "call_expr");
        insert_edge(&conn, c, c_f, seed, seed_f, "CAPPED", "call_expr");
        // outer calls u with RESOLVED — must not pull via UNRESOLVED u
        insert_edge(&conn, outer, outer_f, u, u_f, "RESOLVED", "call_expr");

        let packet = packet_with_symbol("src/seed.rs", "seed_fn", Some("crate::seed_fn"));
        let seeds = resolve_seeds(&packet, &conn).unwrap();
        let blast = compute_blast(&conn, &seeds, 2, 3, BlastCaps::default()).unwrap();

        assert!(blast.edges.iter().any(|e| e.from_symbol == "u_fn"));
        assert!(blast.edges.iter().any(|e| e.from_symbol == "c_fn"));
        assert!(!blast.edges.iter().any(|e| e.from_symbol == "outer_fn"));
        assert!(!blast.edges.iter().any(|e| e.expandable
            && (e.resolution_status == "UNRESOLVED" || e.resolution_status == "CAPPED")));
    }

    #[test]
    fn blast_collapse_prefers_scip_over_ambiguous() {
        let conn = setup_conn();
        let seed_f = insert_file(&conn, "src/seed.rs");
        let caller_f = insert_file(&conn, "src/caller.rs");
        let seed = insert_symbol(&conn, seed_f, "seed_fn", "crate::seed_fn");
        let caller = insert_symbol(&conn, caller_f, "caller_fn", "crate::caller_fn");
        insert_edge(
            &conn,
            caller,
            caller_f,
            seed,
            seed_f,
            "AMBIGUOUS",
            "call_expr",
        );
        insert_edge(
            &conn, caller, caller_f, seed, seed_f, "RESOLVED", "scip:ref",
        );

        let packet = packet_with_symbol("src/seed.rs", "seed_fn", Some("crate::seed_fn"));
        let seeds = resolve_seeds(&packet, &conn).unwrap();
        let blast = compute_blast(&conn, &seeds, 2, 3, BlastCaps::default()).unwrap();

        assert_eq!(blast.edges.len(), 1, "pair must collapse to one edge");
        assert_eq!(blast.edges[0].evidence, "scip:ref");
        assert!(
            blast.edges[0].expandable,
            "scip/RESOLVED must be expandable"
        );
    }

    #[test]
    fn blast_common_name_seed_trap() {
        let conn = setup_conn();
        let a_f = insert_file(&conn, "src/a.rs");
        let b_f = insert_file(&conn, "src/b.rs");
        let caller_a_f = insert_file(&conn, "src/caller_a.rs");
        let caller_b_f = insert_file(&conn, "src/caller_b.rs");
        let new_a = insert_symbol(&conn, a_f, "new", "crate::a::new");
        let new_b = insert_symbol(&conn, b_f, "new", "crate::b::new");
        let ca = insert_symbol(&conn, caller_a_f, "call_a", "crate::call_a");
        let cb = insert_symbol(&conn, caller_b_f, "call_b", "crate::call_b");
        insert_edge(&conn, ca, caller_a_f, new_a, a_f, "RESOLVED", "call_expr");
        insert_edge(&conn, cb, caller_b_f, new_b, b_f, "RESOLVED", "call_expr");

        // Change only a.rs::new — must not pull callers of b.rs::new
        let packet = packet_with_symbol("src/a.rs", "new", Some("crate::a::new"));
        let seeds = resolve_seeds(&packet, &conn).unwrap();
        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds[0].symbol_id, new_a);

        let blast = compute_blast(&conn, &seeds, 1, 3, BlastCaps::default()).unwrap();
        assert_eq!(blast.edges.len(), 1);
        assert_eq!(blast.edges[0].from_symbol, "call_a");
        assert!(
            !blast.edges.iter().any(|e| e.from_symbol == "call_b"),
            "common-name bare join must not fan into b.rs callers"
        );
    }

    #[test]
    fn blast_common_name_file_only_match() {
        // Without QN, file+name must still isolate.
        let conn = setup_conn();
        let a_f = insert_file(&conn, "src/a.rs");
        let b_f = insert_file(&conn, "src/b.rs");
        let caller_a_f = insert_file(&conn, "src/caller_a.rs");
        let caller_b_f = insert_file(&conn, "src/caller_b.rs");
        let new_a = insert_symbol(&conn, a_f, "new", "crate::a::new");
        let new_b = insert_symbol(&conn, b_f, "new", "crate::b::new");
        let ca = insert_symbol(&conn, caller_a_f, "call_a", "crate::call_a");
        let cb = insert_symbol(&conn, caller_b_f, "call_b", "crate::call_b");
        insert_edge(&conn, ca, caller_a_f, new_a, a_f, "RESOLVED", "call_expr");
        insert_edge(&conn, cb, caller_b_f, new_b, b_f, "RESOLVED", "call_expr");

        let packet = packet_with_symbol("src/a.rs", "new", None);
        let seeds = resolve_seeds(&packet, &conn).unwrap();
        assert_eq!(seeds.len(), 1);
        let blast = compute_blast(&conn, &seeds, 1, 3, BlastCaps::default()).unwrap();
        assert_eq!(blast.edges.len(), 1);
        assert_eq!(blast.edges[0].from_symbol, "call_a");
    }

    #[test]
    fn blast_resolve_seeds_shared_qn_dedups_and_sorts() {
        // Two packet symbols share one QN; reuse of the prepared QN statement
        // must still dedup to one seed and keep (file_path, name, symbol_id) order.
        let conn = setup_conn();
        let a_f = insert_file(&conn, "src/a.rs");
        let b_f = insert_file(&conn, "src/b.rs");
        let foo = insert_symbol(&conn, a_f, "foo", "crate::foo");
        let bar = insert_symbol(&conn, b_f, "bar", "crate::bar");

        let mut packet = packet_with_symbol("src/b.rs", "bar", Some("crate::bar"));
        packet.changes.push(ChangedFile {
            path: PathBuf::from("src/a.rs"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: false,
            symbols: Some(vec![
                Symbol {
                    name: "foo".into(),
                    kind: SymbolKind::Function,
                    is_public: true,
                    cognitive_complexity: None,
                    cyclomatic_complexity: None,
                    line_start: None,
                    line_end: None,
                    qualified_name: Some("crate::foo".into()),
                    byte_start: None,
                    byte_end: None,
                    entrypoint_kind: None,
                    metadata: std::collections::BTreeMap::new(),
                },
                Symbol {
                    name: "foo".into(),
                    kind: SymbolKind::Function,
                    is_public: true,
                    cognitive_complexity: None,
                    cyclomatic_complexity: None,
                    line_start: None,
                    line_end: None,
                    qualified_name: Some("crate::foo".into()),
                    byte_start: None,
                    byte_end: None,
                    entrypoint_kind: None,
                    metadata: std::collections::BTreeMap::new(),
                },
            ]),
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        });

        let seeds = resolve_seeds(&packet, &conn).unwrap();
        assert_eq!(seeds.len(), 2, "shared QN must collapse to one seed");
        assert_eq!(seeds[0].symbol_id, foo);
        assert_eq!(seeds[0].name, "foo");
        assert_eq!(seeds[0].file_path, "src/a.rs");
        assert_eq!(seeds[0].qualified_name.as_deref(), Some("crate::foo"));
        assert_eq!(seeds[1].symbol_id, bar);
        assert_eq!(seeds[1].name, "bar");
        assert_eq!(seeds[1].file_path, "src/b.rs");
        assert_eq!(seeds[1].qualified_name.as_deref(), Some("crate::bar"));
    }

    #[test]
    fn blast_caps_fanout_and_total() {
        let conn = setup_conn();
        let seed_f = insert_file(&conn, "src/seed.rs");
        let seed = insert_symbol(&conn, seed_f, "seed_fn", "crate::seed_fn");
        for i in 0..10 {
            let f = insert_file(&conn, &format!("src/c{i}.rs"));
            let c = insert_symbol(&conn, f, &format!("c{i}"), &format!("crate::c{i}"));
            insert_edge(&conn, c, f, seed, seed_f, "RESOLVED", "call_expr");
        }
        let packet = packet_with_symbol("src/seed.rs", "seed_fn", Some("crate::seed_fn"));
        let seeds = resolve_seeds(&packet, &conn).unwrap();
        let blast = compute_blast(
            &conn,
            &seeds,
            1,
            3,
            BlastCaps {
                fanout_per_hop: 3,
                total_edges: 2,
            },
        )
        .unwrap();
        // total cap bites after fanout
        assert!(blast.edges.len() <= 2);
        assert!(
            blast
                .honesty_notes
                .iter()
                .any(|n| n.contains("cap") || n.contains("fan-out") || n.contains("fan"))
        );
    }

    #[test]
    fn blast_test_mapping_join() {
        let conn = setup_conn();
        let seed_f = insert_file(&conn, "src/seed.rs");
        let test_f = insert_file(&conn, "tests/seed_test.rs");
        let seed = insert_symbol(&conn, seed_f, "seed_fn", "crate::seed_fn");
        let test_sym = insert_symbol(&conn, test_f, "test_seed", "crate::test_seed");
        conn.execute(
            "INSERT INTO test_mapping
             (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id,
              confidence, mapping_kind, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, 0.9, 'IMPORT', '2026-01-01T00:00:00Z')",
            rusqlite::params![test_sym, test_f, seed, seed_f],
        )
        .unwrap();

        let packet = packet_with_symbol("src/seed.rs", "seed_fn", Some("crate::seed_fn"));
        let seeds = resolve_seeds(&packet, &conn).unwrap();
        let (coverage, hints) = populate_test_coverage(&conn, &seeds).unwrap();
        assert_eq!(coverage.len(), 1);
        assert_eq!(coverage[0].changed_symbol, "seed_fn");
        assert_eq!(coverage[0].covering_tests.len(), 1);
        assert_eq!(coverage[0].covering_tests[0].test_symbol, "test_seed");
        assert!(hints.iter().any(|h| h.contains("test_seed")));
    }

    #[test]
    fn blast_derive_structural_couplings_hop1_only() {
        let blast = BlastRadius {
            depth_requested: 2,
            depth_applied: 2,
            edges: vec![
                BlastEdge {
                    hop: 1,
                    direction: "caller".into(),
                    from_symbol: "a".into(),
                    from_file: "a.rs".into(),
                    to_symbol: "s".into(),
                    to_file: "s.rs".into(),
                    resolution_status: "RESOLVED".into(),
                    evidence: "call_expr".into(),
                    confidence: Some(1.0),
                    expandable: true,
                    confidence_class:
                        crate::impact::enrichment::edge_confidence::ConfidenceClass::Resolved,
                },
                BlastEdge {
                    hop: 2,
                    direction: "caller".into(),
                    from_symbol: "b".into(),
                    from_file: "b.rs".into(),
                    to_symbol: "a".into(),
                    to_file: "a.rs".into(),
                    resolution_status: "RESOLVED".into(),
                    evidence: "scip:ref".into(),
                    confidence: Some(1.0),
                    expandable: true,
                    confidence_class:
                        crate::impact::enrichment::edge_confidence::ConfidenceClass::ScipBound,
                },
            ],
            ..Default::default()
        };
        let couplings = derive_structural_couplings(&blast);
        assert_eq!(couplings.len(), 1);
        assert_eq!(couplings[0].caller_symbol_name, "a");
    }

    #[test]
    fn blast_cli_depth_clamp() {
        let mut depth = 1u32;
        let note = apply_cli_blast_depth(&mut depth, 3, Some(5));
        assert_eq!(depth, 2);
        assert!(note.unwrap().contains("clamped"));

        let mut depth = 1u32;
        let note = apply_cli_blast_depth(&mut depth, 3, Some(0));
        assert_eq!(depth, 1);
        assert!(note.is_some());
    }

    #[test]
    fn high_confidence_rules() {
        assert!(is_high_confidence("RESOLVED", "call_expr"));
        assert!(is_high_confidence("AMBIGUOUS", "scip:ref"));
        assert!(!is_high_confidence("AMBIGUOUS", "call_expr"));
        assert!(!is_high_confidence("UNRESOLVED", ""));
        assert!(!is_high_confidence("CAPPED", "call_expr"));
    }

    #[test]
    fn blast_edge_confidence_class_scip_and_ambiguous() {
        let conn = setup_conn();
        let seed_f = insert_file(&conn, "src/seed.rs");
        let a_f = insert_file(&conn, "src/a.rs");
        let s_f = insert_file(&conn, "src/scip_caller.rs");
        let seed = insert_symbol(&conn, seed_f, "seed_fn", "crate::seed_fn");
        let a = insert_symbol(&conn, a_f, "a_fn", "crate::a_fn");
        let sc = insert_symbol(&conn, s_f, "scip_fn", "crate::scip_fn");
        insert_edge(&conn, a, a_f, seed, seed_f, "AMBIGUOUS", "call_expr");
        insert_edge(&conn, sc, s_f, seed, seed_f, "RESOLVED", "scip:ref");

        let packet = packet_with_symbol("src/seed.rs", "seed_fn", Some("crate::seed_fn"));
        let seeds = resolve_seeds(&packet, &conn).unwrap();
        let blast = compute_blast(&conn, &seeds, 1, 3, BlastCaps::default()).unwrap();

        let amb = blast
            .edges
            .iter()
            .find(|e| e.from_symbol == "a_fn")
            .expect("AMBIGUOUS edge");
        assert_eq!(
            amb.confidence_class,
            crate::impact::enrichment::edge_confidence::ConfidenceClass::Ambiguous
        );
        assert!(!amb.expandable);

        let scip = blast
            .edges
            .iter()
            .find(|e| e.from_symbol == "scip_fn")
            .expect("SCIP edge");
        assert_eq!(
            scip.confidence_class,
            crate::impact::enrichment::edge_confidence::ConfidenceClass::ScipBound
        );
        assert!(scip.expandable);

        assert_eq!(blast.confidence_summary.ambiguous, 1);
        assert_eq!(blast.confidence_summary.scip_bound, 1);
        assert_eq!(blast.confidence_summary.total, 2);
        assert_eq!(blast.confidence_summary.expandable, 1);
    }
}
