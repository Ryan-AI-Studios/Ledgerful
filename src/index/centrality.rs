use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CentralityStats {
    pub entry_points_count: usize,
    pub symbols_computed: usize,
    pub max_reachable: usize,
}

pub struct CentralityComputer<'a> {
    storage: &'a StorageManager,
}

const MAX_DEPTH: usize = 20;
const MAX_REACHABLE_PER_ENTRY: usize = 50_000;
const BATCH_SIZE: usize = 500;

impl<'a> CentralityComputer<'a> {
    pub fn new(storage: &'a StorageManager) -> Self {
        Self { storage }
    }

    pub fn compute(&self) -> Result<CentralityStats> {
        let conn = self.storage.get_connection();

        // 1. Load resolved edges: caller_symbol_id -> [callee_symbol_ids]
        let mut edge_stmt = conn
            .prepare(
                "SELECT caller_symbol_id, callee_symbol_id FROM structural_edges \
                 WHERE callee_symbol_id IS NOT NULL",
            )
            .into_diagnostic()?;

        let edge_rows: Vec<(i64, i64)> = edge_stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
            .into_diagnostic()?
            .collect::<Result<Vec<_>, _>>()
            .into_diagnostic()?;

        drop(edge_stmt);

        if edge_rows.is_empty() {
            info!("No resolved structural edges; skipping centrality computation.");
            return Ok(CentralityStats {
                entry_points_count: 0,
                symbols_computed: 0,
                max_reachable: 0,
            });
        }

        let mut adjacency: HashMap<i64, Vec<i64>> = HashMap::new();
        for (caller, callee) in &edge_rows {
            adjacency.entry(*caller).or_default().push(*callee);
        }

        // 2. Load entry point symbols
        let mut ep_stmt = conn
            .prepare(
                "SELECT id FROM project_symbols WHERE entrypoint_kind IN ('ENTRYPOINT', 'HANDLER')",
            )
            .into_diagnostic()?;

        let entry_points: Vec<i64> = ep_stmt
            .query_map([], |row| row.get::<_, i64>(0))
            .into_diagnostic()?
            .collect::<Result<Vec<_>, _>>()
            .into_diagnostic()?;

        drop(ep_stmt);

        if entry_points.is_empty() {
            info!("No entry point symbols found; skipping centrality computation.");
            return Ok(CentralityStats {
                entry_points_count: 0,
                symbols_computed: 0,
                max_reachable: 0,
            });
        }

        // 3. BFS from each entry point
        let mut reachable_counts: HashMap<i64, usize> = HashMap::new();

        for &ep_id in &entry_points {
            let mut visited: HashSet<i64> = HashSet::new();
            let mut queue: VecDeque<(i64, usize)> = VecDeque::new();
            queue.push_back((ep_id, 0));
            visited.insert(ep_id);

            let mut count_this_ep = 0usize;

            while let Some((sym_id, depth)) = queue.pop_front() {
                if depth >= MAX_DEPTH {
                    continue;
                }

                if let Some(neighbors) = adjacency.get(&sym_id) {
                    for &neighbor in neighbors {
                        if visited.insert(neighbor) {
                            // neighbor is reachable (not the entry point itself counted below)
                            if neighbor != ep_id {
                                *reachable_counts.entry(neighbor).or_insert(0) += 1;
                                count_this_ep += 1;
                            }
                            if count_this_ep >= MAX_REACHABLE_PER_ENTRY {
                                warn!(
                                    "Entry point {} reached {} symbols, capping.",
                                    ep_id, MAX_REACHABLE_PER_ENTRY
                                );
                                // Stop adding to queue for this entry point
                                break;
                            }
                            queue.push_back((neighbor, depth + 1));
                        }
                    }
                    if count_this_ep >= MAX_REACHABLE_PER_ENTRY {
                        break;
                    }
                }
            }
        }

        // 4. Load id and qualified_name from project_symbols to map SQLite symbol_id to URN representation
        let mut sym_stmt = conn
            .prepare("SELECT id, qualified_name FROM project_symbols")
            .into_diagnostic()?;
        let sym_rows: Vec<(i64, String)> = sym_stmt
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .into_diagnostic()?
            .collect::<Result<Vec<_>, _>>()
            .into_diagnostic()?;
        drop(sym_stmt);

        let mut urn_to_symbol_id = HashMap::new();
        for (id, qname) in &sym_rows {
            let urn =
                crate::platform::urn::build_urn(crate::state::graph_kinds::NodeKind::Symbol, qname);
            urn_to_symbol_id.insert(urn, *id);
        }

        // 5. Run PageRank algorithm query on CozoDB if present
        let mut pagerank_scores: HashMap<i64, f64> = HashMap::new();
        if let Some(cozo) = &self.storage.cozo {
            let script = "
                edges[src, dst] := *edge{source: src, target: dst, relation: 'calls'}
                ?[node, rank] <~ PageRank(edges[src, dst])
            ";
            let res = cozo.run_script(script)?;
            for row in res.rows {
                if let (Some(cozo::DataValue::Str(node_urn)), Some(cozo::DataValue::Num(num))) =
                    (row.first(), row.get(1))
                {
                    let rank_score = match num {
                        cozo::Num::Float(f) => *f,
                        cozo::Num::Int(i) => *i as f64,
                    };
                    if let Some(&sym_id) = urn_to_symbol_id.get(node_urn.as_str()) {
                        pagerank_scores.insert(sym_id, rank_score);
                    }
                }
            }
        }

        // 6. Load symbol -> file_id mapping
        let mut symbol_file_stmt = conn
            .prepare("SELECT id, file_id FROM project_symbols")
            .into_diagnostic()?;

        let symbol_file_rows: Vec<(i64, i64)> = symbol_file_stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
            .into_diagnostic()?
            .collect::<Result<Vec<_>, _>>()
            .into_diagnostic()?;

        drop(symbol_file_stmt);

        let symbol_to_file: HashMap<i64, i64> = symbol_file_rows.into_iter().collect();

        // Clear existing centrality data
        let conn = self.storage.get_connection();
        let tx = conn.unchecked_transaction().into_diagnostic()?;
        tx.execute("DELETE FROM symbol_centrality", [])
            .into_diagnostic()?;
        tx.commit().into_diagnostic()?;

        // Union of keys from reachable_counts and pagerank_scores
        let mut all_sym_ids: HashSet<i64> = reachable_counts.keys().copied().collect();
        all_sym_ids.extend(pagerank_scores.keys().copied());

        // Batch insert centrality rows
        let now = chrono::Utc::now().to_rfc3339();
        let entries: Vec<(i64, i64, usize, f64)> = all_sym_ids
            .iter()
            .filter(|sym_id| symbol_to_file.contains_key(sym_id))
            .map(|sym_id| {
                let file_id = symbol_to_file[sym_id];
                let count = reachable_counts.get(sym_id).copied().unwrap_or(0);
                let pagerank = pagerank_scores.get(sym_id).copied().unwrap_or(0.0_f64);
                (*sym_id, file_id, count, pagerank)
            })
            .collect();

        let max_reachable = entries.iter().map(|(_, _, c, _)| *c).max().unwrap_or(0);
        let symbols_computed = entries.len();

        for chunk in entries.chunks(BATCH_SIZE) {
            let conn = self.storage.get_connection();
            let tx = conn.unchecked_transaction().into_diagnostic()?;
            for (symbol_id, file_id, count, pagerank) in chunk {
                tx.execute(
                    "INSERT INTO symbol_centrality (symbol_id, file_id, entrypoints_reachable, betweenness, last_computed_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        symbol_id,
                        file_id,
                        *count as i64,
                        *pagerank,
                        now,
                    ],
                )
                .into_diagnostic()?;
            }
            tx.commit().into_diagnostic()?;
        }

        info!(
            "Centrality computation complete: {} entry points, {} symbols computed, max reachable = {}",
            entry_points.len(),
            symbols_computed,
            max_reachable,
        );

        Ok(CentralityStats {
            entry_points_count: entry_points.len(),
            symbols_computed,
            max_reachable,
        })
    }
}

pub fn enrich_hotspots_with_centrality(
    hotspots: &mut [crate::impact::packet::Hotspot],
    storage: &StorageManager,
) -> Result<()> {
    let conn = storage.get_connection();
    let mut stmt = conn
        .prepare(
            "SELECT pf.file_path, MAX(sc.entrypoints_reachable), MAX(sc.betweenness) \
         FROM symbol_centrality sc \
         JOIN project_files pf ON sc.file_id = pf.id \
         GROUP BY pf.file_path",
        )
        .into_diagnostic()?;

    let rows = stmt
        .query_map([], |row| {
            let path: String = row.get(0)?;
            let reachable: i64 = row.get(1)?;
            let pagerank: f64 = row.get(2)?;
            Ok((path, reachable, pagerank))
        })
        .into_diagnostic()?;

    let mut file_centrality = HashMap::new();
    for row in rows {
        let (path, reachable, pagerank) = row.into_diagnostic()?;
        file_centrality.insert(path, (reachable as f64, pagerank));
    }

    for h in hotspots {
        let path_str = h.path.to_string_lossy().to_string().replace('\\', "/");
        if let Some(&(reachable, pagerank)) = file_centrality.get(&path_str) {
            h.centrality = Some(reachable as usize);
            let boost = (reachable.ln_1p() as f32) * 0.1 + (pagerank as f32) * 0.3;
            h.score += boost;
            h.display_score += boost;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    fn in_memory_storage() -> StorageManager {
        let conn = Connection::open_in_memory().unwrap();
        let mut conn = conn;
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    /// Helper: insert a file and return its id
    fn insert_file(conn: &Connection, path: &str) -> i64 {
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (path, "Rust", "hash", 100, "2026-05-01T00:00:00Z"),
        ).unwrap();
        conn.last_insert_rowid()
    }

    /// Helper: insert a symbol and return its id
    fn insert_symbol(conn: &Connection, file_id: i64, name: &str, entrypoint_kind: &str) -> i64 {
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (file_id, format!("crate::{}", name), name, "Function", entrypoint_kind, "2026-05-01T00:00:00Z"),
        ).unwrap();
        conn.last_insert_rowid()
    }

    /// Helper: insert an edge
    fn insert_edge(
        conn: &Connection,
        caller: i64,
        callee: i64,
        caller_file: i64,
        callee_file: i64,
    ) {
        conn.execute(
            "INSERT INTO structural_edges (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id, call_kind, resolution_status, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (caller, caller_file, callee, callee_file, "DIRECT", "RESOLVED", 1.0_f64),
        ).unwrap();
    }

    #[test]
    fn test_empty_edges_skip() {
        let storage = in_memory_storage();
        let computer = CentralityComputer::new(&storage);
        let stats = computer.compute().unwrap();
        assert_eq!(stats.entry_points_count, 0);
        assert_eq!(stats.symbols_computed, 0);
        assert_eq!(stats.max_reachable, 0);
    }

    #[test]
    fn test_single_entry_point_with_call_chain() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();

        let file_id = insert_file(conn, "src/lib.rs");
        let ep_id = insert_symbol(conn, file_id, "main", "ENTRYPOINT");
        let a_id = insert_symbol(conn, file_id, "a", "INTERNAL");
        let b_id = insert_symbol(conn, file_id, "b", "INTERNAL");

        // main -> a -> b
        insert_edge(conn, ep_id, a_id, file_id, file_id);
        insert_edge(conn, a_id, b_id, file_id, file_id);

        let _ = conn;

        let computer = CentralityComputer::new(&storage);
        let stats = computer.compute().unwrap();

        assert_eq!(stats.entry_points_count, 1);
        assert_eq!(stats.symbols_computed, 2); // a and b are reachable
        assert_eq!(stats.max_reachable, 1); // each has entrypoints_reachable = 1

        // Verify DB contents
        let conn = storage.get_connection();
        let a_reachable: i64 = conn
            .query_row(
                "SELECT entrypoints_reachable FROM symbol_centrality WHERE symbol_id = ?1",
                [a_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(a_reachable, 1);

        let b_reachable: i64 = conn
            .query_row(
                "SELECT entrypoints_reachable FROM symbol_centrality WHERE symbol_id = ?1",
                [b_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(b_reachable, 1);
    }

    #[test]
    fn test_multiple_entry_points_shared_callee() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();

        let file_id = insert_file(conn, "src/lib.rs");
        let ep1_id = insert_symbol(conn, file_id, "handler1", "HANDLER");
        let ep2_id = insert_symbol(conn, file_id, "handler2", "HANDLER");
        let shared_id = insert_symbol(conn, file_id, "shared_util", "INTERNAL");

        // Both handlers call shared_util
        insert_edge(conn, ep1_id, shared_id, file_id, file_id);
        insert_edge(conn, ep2_id, shared_id, file_id, file_id);

        let _ = conn;

        let computer = CentralityComputer::new(&storage);
        let stats = computer.compute().unwrap();

        assert_eq!(stats.entry_points_count, 2);
        assert_eq!(stats.symbols_computed, 1); // only shared_util is reachable
        assert_eq!(stats.max_reachable, 2); // shared_util reached by 2 entry points

        let conn = storage.get_connection();
        let shared_reachable: i64 = conn
            .query_row(
                "SELECT entrypoints_reachable FROM symbol_centrality WHERE symbol_id = ?1",
                [shared_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(shared_reachable, 2);
    }

    #[test]
    fn test_cycle_handling() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();

        let file_id = insert_file(conn, "src/lib.rs");
        let ep_id = insert_symbol(conn, file_id, "main", "ENTRYPOINT");
        let a_id = insert_symbol(conn, file_id, "a", "INTERNAL");
        let b_id = insert_symbol(conn, file_id, "b", "INTERNAL");

        // main -> a -> b -> a (cycle)
        insert_edge(conn, ep_id, a_id, file_id, file_id);
        insert_edge(conn, a_id, b_id, file_id, file_id);
        insert_edge(conn, b_id, a_id, file_id, file_id);

        let _ = conn;

        let computer = CentralityComputer::new(&storage);
        let stats = computer.compute().unwrap();

        // Should not hang and should find both a and b reachable
        assert_eq!(stats.entry_points_count, 1);
        assert_eq!(stats.symbols_computed, 2);

        let conn = storage.get_connection();
        let a_reachable: i64 = conn
            .query_row(
                "SELECT entrypoints_reachable FROM symbol_centrality WHERE symbol_id = ?1",
                [a_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(a_reachable, 1);

        let b_reachable: i64 = conn
            .query_row(
                "SELECT entrypoints_reachable FROM symbol_centrality WHERE symbol_id = ?1",
                [b_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(b_reachable, 1);
    }

    #[test]
    fn test_depth_cap() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();

        let file_id = insert_file(conn, "src/lib.rs");
        let ep_id = insert_symbol(conn, file_id, "main", "ENTRYPOINT");

        // Build a chain of 25 symbols: main -> s0 -> s1 -> ... -> s24
        let mut symbol_ids: Vec<i64> = vec![ep_id];
        for i in 0..25 {
            let sid = insert_symbol(conn, file_id, &format!("s{}", i), "INTERNAL");
            symbol_ids.push(sid);
        }

        // ep -> s0, s0 -> s1, ... s23 -> s24
        for i in 0..25 {
            insert_edge(conn, symbol_ids[i], symbol_ids[i + 1], file_id, file_id);
        }

        let _ = conn;

        let computer = CentralityComputer::new(&storage);
        let stats = computer.compute().unwrap();

        // Symbols beyond depth 20 should not be counted
        // s0 through s19 (20 symbols) are within 20 hops from the entry point
        assert_eq!(stats.entry_points_count, 1);
        assert_eq!(stats.symbols_computed, 20); // only 20 symbols within depth cap

        let conn = storage.get_connection();
        // s19 should be reachable (depth 20)
        let s19_reachable: i64 = conn
            .query_row(
                "SELECT entrypoints_reachable FROM symbol_centrality WHERE symbol_id = ?1",
                [symbol_ids[20]], // symbol_ids[20] is s19
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(s19_reachable, 1);

        // s20 (depth 21) should not be in the centrality table
        let s20_result: Option<i64> = conn
            .query_row(
                "SELECT entrypoints_reachable FROM symbol_centrality WHERE symbol_id = ?1",
                [symbol_ids[21]], // symbol_ids[21] is s20
                |row| row.get(0),
            )
            .ok();
        assert!(
            s20_result.is_none(),
            "Symbol at depth 21 should not be in centrality table"
        );
    }

    fn in_memory_storage_with_cozo() -> StorageManager {
        let conn = Connection::open_in_memory().unwrap();
        let mut conn = conn;
        get_migrations().to_latest(&mut conn).unwrap();
        let cozo = crate::state::storage_cozo::CozoStorage::new_in_memory().unwrap();
        cozo.setup_schema().unwrap();
        let mut storage = StorageManager::init_from_conn(conn);
        storage.cozo = Some(cozo);
        storage
    }

    #[test]
    fn test_pagerank_computation_and_blending() {
        let storage = in_memory_storage_with_cozo();
        let conn = storage.get_connection();

        let file_id = insert_file(conn, "src/lib.rs");
        let ep_id = insert_symbol(conn, file_id, "main", "ENTRYPOINT");
        let a_id = insert_symbol(conn, file_id, "a", "INTERNAL");
        let b_id = insert_symbol(conn, file_id, "b", "INTERNAL");

        // main -> a -> b
        insert_edge(conn, ep_id, a_id, file_id, file_id);
        insert_edge(conn, a_id, b_id, file_id, file_id);

        // Map symbols to URNs
        let ep_urn = crate::platform::urn::build_urn(
            crate::state::graph_kinds::NodeKind::Symbol,
            "crate::main",
        );
        let a_urn = crate::platform::urn::build_urn(
            crate::state::graph_kinds::NodeKind::Symbol,
            "crate::a",
        );
        let b_urn = crate::platform::urn::build_urn(
            crate::state::graph_kinds::NodeKind::Symbol,
            "crate::b",
        );

        // Insert edges into Cozo
        let cozo = storage.cozo.as_ref().unwrap();
        cozo.put_edge_batch(&[
            crate::state::storage_cozo::GraphEdge {
                source: ep_urn.clone(),
                target: a_urn.clone(),
                relation: crate::state::graph_kinds::EdgeKind::Calls,
                confidence: 1.0,
                provenance_id: "tx_1".to_string(),
            },
            crate::state::storage_cozo::GraphEdge {
                source: a_urn.clone(),
                target: b_urn.clone(),
                relation: crate::state::graph_kinds::EdgeKind::Calls,
                confidence: 1.0,
                provenance_id: "tx_1".to_string(),
            },
        ])
        .unwrap();

        let computer = CentralityComputer::new(&storage);
        let stats = computer.compute().unwrap();

        assert_eq!(stats.entry_points_count, 1);

        // Verify SQLite centrality scores
        let a_betweenness: f64 = conn
            .query_row(
                "SELECT betweenness FROM symbol_centrality WHERE symbol_id = ?1",
                [a_id],
                |row| row.get(0),
            )
            .unwrap();
        let b_betweenness: f64 = conn
            .query_row(
                "SELECT betweenness FROM symbol_centrality WHERE symbol_id = ?1",
                [b_id],
                |row| row.get(0),
            )
            .unwrap();

        assert!(a_betweenness > 0.0);
        assert!(b_betweenness > 0.0);

        // Now mock symbol_centrality in SQLite to test enrich_hotspots_with_centrality
        conn.execute("DELETE FROM symbol_centrality", []).unwrap();
        conn.execute(
            "INSERT INTO symbol_centrality (symbol_id, file_id, entrypoints_reachable, betweenness, last_computed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                a_id,
                file_id,
                5i64,
                0.75f64,
                "now"
            ]
        ).unwrap();

        let mut hotspots = vec![crate::impact::packet::Hotspot {
            path: std::path::PathBuf::from("src/lib.rs"),
            score: 1.0,
            display_score: 1.0,
            complexity: 10,
            frequency: 1.0,
            centrality: None,
        }];

        enrich_hotspots_with_centrality(&mut hotspots, &storage).unwrap();

        assert_eq!(hotspots[0].centrality, Some(5));
        // Expected display_score = 1.0 + ln(1+5)*0.1 + 0.75*0.3 ≈ 1.0 + 0.179176 + 0.225 = 1.404176
        let diff = (hotspots[0].display_score - 1.404176).abs();
        assert!(diff < 1e-4);
    }
}
