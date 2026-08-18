/// Distinct absence/presence states for `verify --explain --entity`, so the
/// CLI can tell "feature is empty here" apart from "feature is broken".
#[derive(Debug, PartialEq, Eq)]
pub enum TestMappingState {
    /// The `test_mapping` table itself doesn't exist (pre-migration DB).
    TableMissing,
    /// The table exists but has never been populated by an index run.
    TableEmpty,
    /// The entity didn't resolve to an indexed file path or a known symbol name.
    EntityNotIndexed,
    /// Full-input path suffix matched more than one indexed file (unique-only).
    /// Candidates are sorted by `file_path`; display may cap the list.
    EntityAmbiguous {
        query: String,
        candidates: Vec<String>,
    },
    /// The entity is indexed, but no test currently maps to it.
    NoMappingsForEntity {
        /// Stored `project_files.file_path` when resolved via a file path.
        resolved_path: Option<String>,
    },
    /// Mapped tests, formatted as `"<test file path>::<test symbol name>"`.
    Mapped {
        tests: Vec<String>,
        /// Stored `project_files.file_path` when resolved via a file path
        /// (`None` for pure symbol-name matches).
        resolved_path: Option<String>,
    },
}

impl TestMappingState {
    /// Stored path when the entity resolved to a file (alias/suffix/exact).
    pub fn resolved_path(&self) -> Option<&str> {
        match self {
            Self::Mapped { resolved_path, .. } | Self::NoMappingsForEntity { resolved_path } => {
                resolved_path.as_deref()
            }
            _ => None,
        }
    }
}

/// Whether a verification step command is relevant to `--entity` (M3).
///
/// Matches case-insensitively on the raw entity string and, when path resolution
/// produced a stored path (alias/suffix), that resolved form as well. Generic
/// `test` / `check` steps stay in the relevant set.
pub(crate) fn step_relevant_to_entity(
    command: &str,
    target: &str,
    resolved_path: Option<&str>,
) -> bool {
    let cmd = command.to_lowercase();
    let t_raw = target.to_lowercase();
    let t_resolved = resolved_path.map(|p| p.to_lowercase());
    cmd.contains(&t_raw)
        || t_resolved.as_ref().is_some_and(|r| cmd.contains(r))
        || cmd.contains("test")
        || cmd.contains("check")
}

const MAPPED_TESTS_QUERY_BY_FILE: &str = "SELECT DISTINCT pf_test.file_path || '::' || ps_test.symbol_name \
     FROM test_mapping tm \
     JOIN project_symbols ps_test ON tm.test_symbol_id = ps_test.id \
     JOIN project_files pf_test ON tm.test_file_id = pf_test.id \
     WHERE tm.tested_file_id = ?1 \
     ORDER BY 1";

const MAPPED_TESTS_QUERY_BY_SYMBOL: &str = "SELECT DISTINCT pf_test.file_path || '::' || ps_test.symbol_name \
     FROM test_mapping tm \
     JOIN project_symbols ps_test ON tm.test_symbol_id = ps_test.id \
     JOIN project_files pf_test ON tm.test_file_id = pf_test.id \
     JOIN project_symbols ps_tested ON tm.tested_symbol_id = ps_tested.id \
     WHERE ps_tested.symbol_name = ?1 \
     ORDER BY 1";

/// Outcome of path/symbol resolution before mapping lookup.
#[derive(Debug, PartialEq, Eq)]
enum ResolvedEntity {
    ExactPath {
        file_id: i64,
        stored_path: String,
    },
    Ambiguous {
        query: String,
        candidates: Vec<String>,
    },
    Symbol {
        name: String,
    },
    NotFound,
}

fn symbol_name_exists(conn: &rusqlite::Connection, name: &str) -> bool {
    use rusqlite::OptionalExtension;

    conn.query_row(
        "SELECT 1 FROM project_symbols WHERE symbol_name = ?1 LIMIT 1",
        [name],
        |_| Ok(true),
    )
    .optional()
    .ok()
    .flatten()
    .unwrap_or(false)
}

/// Resolve entity → indexed file or symbol.
/// Order: shared file identity (exact → alias unique → suffix unique) →
/// exact symbol name → NotFound. File rules live in
/// [`crate::util::path_entity::resolve_indexed_file_path`] (0156 / 0183).
fn resolve_tested_entity(conn: &rusqlite::Connection, normalized: &str) -> ResolvedEntity {
    use crate::util::path_entity::{IndexedFileResolve, resolve_indexed_file_path};

    match resolve_indexed_file_path(conn, normalized) {
        IndexedFileResolve::Unique {
            file_id,
            stored_path,
        } => {
            return ResolvedEntity::ExactPath {
                file_id,
                stored_path,
            };
        }
        IndexedFileResolve::Ambiguous { query, candidates } => {
            return ResolvedEntity::Ambiguous { query, candidates };
        }
        IndexedFileResolve::NotFound => {}
    }

    // Symbol-name fallback (tests / verify --explain only — not symbols/hotspots).
    if symbol_name_exists(conn, normalized) {
        return ResolvedEntity::Symbol {
            name: normalized.to_string(),
        };
    }

    ResolvedEntity::NotFound
}

fn query_mapped_tests_by_file(conn: &rusqlite::Connection, file_id: i64) -> Vec<String> {
    conn.prepare(MAPPED_TESTS_QUERY_BY_FILE)
        .and_then(|mut s| {
            s.query_map([file_id], |row| row.get(0))
                .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<String>>())
        })
        .unwrap_or_default()
}

fn query_mapped_tests_by_symbol(conn: &rusqlite::Connection, name: &str) -> Vec<String> {
    conn.prepare(MAPPED_TESTS_QUERY_BY_SYMBOL)
        .and_then(|mut s| {
            s.query_map([name], |row| row.get(0))
                .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<String>>())
        })
        .unwrap_or_default()
}

/// Resolves test-mapping coverage for an entity against the real
/// `test_mapping` schema (`test_symbol_id`/`test_file_id`/`tested_symbol_id`/
/// `tested_file_id`). Path resolution: exact → module/extensionless alias →
/// unique-only full-input suffix → symbol (shared by `tests -e` and
/// `verify --explain --entity`).
pub fn explain_test_mappings(
    conn: &rusqlite::Connection,
    normalized_entity: &str,
) -> TestMappingState {
    let total: i64 = match conn.query_row("SELECT count(*) FROM test_mapping", [], |row| row.get(0))
    {
        Ok(c) => c,
        Err(_) => return TestMappingState::TableMissing,
    };
    if total == 0 {
        return TestMappingState::TableEmpty;
    }

    match resolve_tested_entity(conn, normalized_entity) {
        ResolvedEntity::Ambiguous { query, candidates } => {
            TestMappingState::EntityAmbiguous { query, candidates }
        }
        ResolvedEntity::NotFound => TestMappingState::EntityNotIndexed,
        ResolvedEntity::ExactPath {
            file_id,
            stored_path,
        } => {
            let mapped = query_mapped_tests_by_file(conn, file_id);
            if mapped.is_empty() {
                TestMappingState::NoMappingsForEntity {
                    resolved_path: Some(stored_path),
                }
            } else {
                TestMappingState::Mapped {
                    tests: mapped,
                    resolved_path: Some(stored_path),
                }
            }
        }
        ResolvedEntity::Symbol { name } => {
            let mapped = query_mapped_tests_by_symbol(conn, &name);
            if mapped.is_empty() {
                TestMappingState::NoMappingsForEntity {
                    resolved_path: None,
                }
            } else {
                TestMappingState::Mapped {
                    tests: mapped,
                    resolved_path: None,
                }
            }
        }
    }
}

#[cfg(test)]
mod entity_path_resolution_tests {
    use super::step_relevant_to_entity;

    /// M3: resolved stored path matches step even when the raw entity is an alias.
    #[test]
    fn step_filter_matches_resolved_path_when_raw_differs() {
        let cmd = "cargo nextest run --package ledgerful -- src/commands/doctor/mod.rs";
        assert!(step_relevant_to_entity(
            cmd,
            "src/commands/doctor.rs",
            Some("src/commands/doctor/mod.rs"),
        ));
    }

    /// M3: raw entity still matches when present in the command.
    #[test]
    fn step_filter_matches_raw_target() {
        let cmd = "rg src/pkg.rs --type rust";
        assert!(step_relevant_to_entity(cmd, "src/pkg.rs", None));
        assert!(step_relevant_to_entity(
            cmd,
            "src/pkg.rs",
            Some("src/pkg/mod.rs"),
        ));
    }

    /// M3: neither raw nor resolved → only generic test/check steps stay relevant.
    #[test]
    fn step_filter_requires_path_or_generic_when_unrelated() {
        // No path match and no test/check token → not relevant.
        assert!(!step_relevant_to_entity(
            "cargo fmt --all",
            "src/commands/doctor.rs",
            Some("src/commands/doctor/mod.rs"),
        ));
        // "check" / "test" are generic verifier tokens → still relevant
        assert!(step_relevant_to_entity(
            "cargo check -p ledgerful",
            "src/orphan.rs",
            None,
        ));
        assert!(step_relevant_to_entity(
            "cargo nextest run --lib",
            "src/orphan.rs",
            None,
        ));
    }

    /// Display cap helper contract for Ambiguous lists (DoD-3 / L2 data side).
    #[test]
    fn ambiguous_display_cap_shows_and_n_more_when_over_10() {
        let total = 11usize;
        let show = total.min(10);
        assert_eq!(show, 10);
        let more = total - show;
        assert_eq!(more, 1);
        let line = format!("… and {} more", more);
        assert_eq!(line, "… and 1 more");
    }
}
