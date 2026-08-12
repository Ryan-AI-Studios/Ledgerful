//! Shared unique-only indexed file path identity (tracks 0156 / 0183).
//!
//! Order: exact `project_files` hit → Rust module-layout alias (unique) →
//! full-input path suffix (unique). **No** symbol-name fallback, LCS, or silent
//! multi-match pick. Callers that need symbol resolution (tests / verify explain)
//! layer that on top of [`resolve_indexed_file_path`].

use rusqlite::Connection;

/// Outcome of file-only path identity against `project_files`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexedFileResolve {
    /// Exactly one indexed file (exact, unique alias, or unique suffix).
    Unique { file_id: i64, stored_path: String },
    /// Multiple suffix matches — refuse; do not pick.
    Ambiguous {
        query: String,
        candidates: Vec<String>,
    },
    /// No exact / unique alias / unique suffix hit.
    NotFound,
}

impl IndexedFileResolve {
    /// Stored path when resolution is unique; otherwise `None`.
    pub fn unique_path(&self) -> Option<&str> {
        match self {
            Self::Unique { stored_path, .. } => Some(stored_path.as_str()),
            _ => None,
        }
    }
}

/// Build generalized path-alias candidates (0156 M1 / 0183-H). Accept iff exactly one exists.
///
/// | Input shape | Candidates |
/// | ends with `.rs` | `{stem}/mod.rs` only |
/// | no extension / trailing `/` | `{trim}/mod.rs` and `{trim}.rs` |
/// | other extension | none (Rust-only; no `.ts`/`mod.ts`) |
pub fn alias_path_candidates(normalized: &str) -> Vec<String> {
    if normalized.ends_with(".rs") {
        let stem = match normalized.strip_suffix(".rs") {
            Some(s) if !s.is_empty() => s,
            _ => return Vec::new(),
        };
        return vec![format!("{stem}/mod.rs")];
    }

    let trimmed = normalized.trim_end_matches('/');
    if trimmed.is_empty() {
        return Vec::new();
    }

    let last_seg = trimmed.rsplit('/').next().unwrap_or(trimmed);
    // Other extension (.ts, .go, …): no module-layout candidates (0183-H).
    if last_seg.contains('.') {
        return Vec::new();
    }

    vec![format!("{trimmed}/mod.rs"), format!("{trimmed}.rs")]
}

/// Exact `project_files` lookup. Windows uses `LOWER` equality (dead-code mirror).
pub fn lookup_file_exact(conn: &Connection, path: &str) -> Option<(i64, String)> {
    use rusqlite::OptionalExtension;

    let sql = if cfg!(target_os = "windows") {
        "SELECT id, file_path FROM project_files WHERE LOWER(file_path) = LOWER(?1)"
    } else {
        "SELECT id, file_path FROM project_files WHERE file_path = ?1"
    };
    conn.query_row(sql, [path], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()
        .ok()
        .flatten()
}

/// Unique-only full-input path suffix (0156 M2). No LCS scoring.
///
/// Equality + `LIKE '%/' || query`, ordered by `file_path`.
/// On Windows, equality uses `LOWER` to match [`lookup_file_exact`] (0183-B).
pub fn lookup_files_by_suffix(conn: &Connection, query: &str) -> Vec<(i64, String)> {
    let sql = if cfg!(target_os = "windows") {
        "SELECT id, file_path FROM project_files \
         WHERE LOWER(file_path) = LOWER(?1) OR file_path LIKE '%/' || ?1 \
         ORDER BY file_path"
    } else {
        "SELECT id, file_path FROM project_files \
         WHERE file_path = ?1 OR file_path LIKE '%/' || ?1 \
         ORDER BY file_path"
    };
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = match stmt.query_map([query], |row| Ok((row.get(0)?, row.get(1)?))) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    rows.filter_map(|r| r.ok()).collect()
}

/// Resolve entity → indexed file path only (exact → alias unique → suffix unique).
///
/// Does **not** fall through to symbol names — use this for symbols `--path`
/// zero-match fallback and hotspots explain complexity (`project_files`).
pub fn resolve_indexed_file_path(conn: &Connection, normalized: &str) -> IndexedFileResolve {
    // 1. Exact path always first.
    if let Some((file_id, stored_path)) = lookup_file_exact(conn, normalized) {
        return IndexedFileResolve::Unique {
            file_id,
            stored_path,
        };
    }

    // 2. Generalized path alias: accept iff exactly one candidate exists.
    let mut alias_hits: Vec<(i64, String)> = Vec::new();
    for cand in alias_path_candidates(normalized) {
        if let Some(hit) = lookup_file_exact(conn, &cand) {
            // Deduplicate by file id (Windows LOWER can collapse case variants).
            if !alias_hits.iter().any(|(id, _)| *id == hit.0) {
                alias_hits.push(hit);
            }
        }
    }
    if alias_hits.len() == 1 {
        let (file_id, stored_path) = alias_hits.remove(0);
        return IndexedFileResolve::Unique {
            file_id,
            stored_path,
        };
    }
    // 0 or >1 alias hits: do not guess; fall through.

    // 3. Unique-only full-input path suffix (no LCS).
    let mut suffix_hits = lookup_files_by_suffix(conn, normalized);
    match suffix_hits.len() {
        1 => {
            let (file_id, stored_path) = suffix_hits.remove(0);
            IndexedFileResolve::Unique {
                file_id,
                stored_path,
            }
        }
        n if n > 1 => {
            let candidates: Vec<String> = suffix_hits.into_iter().map(|(_, p)| p).collect();
            IndexedFileResolve::Ambiguous {
                query: normalized.to_string(),
                candidates,
            }
        }
        _ => IndexedFileResolve::NotFound,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    fn mem_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn
    }

    fn seed_file(conn: &Connection, id: i64, path: &str) {
        conn.execute(
            "INSERT INTO project_files (id, file_path, last_indexed_at) \
             VALUES (?1, ?2, '2026-01-01T00:00:00Z')",
            rusqlite::params![id, path],
        )
        .unwrap();
    }

    #[test]
    fn alias_candidates_rs_to_mod() {
        assert_eq!(
            alias_path_candidates("src/pkg.rs"),
            vec!["src/pkg/mod.rs".to_string()]
        );
    }

    #[test]
    fn alias_candidates_extensionless() {
        let mut c = alias_path_candidates("src/pkg");
        c.sort();
        assert_eq!(
            c,
            vec!["src/pkg.rs".to_string(), "src/pkg/mod.rs".to_string()]
        );
    }

    #[test]
    fn alias_candidates_other_ext_empty_rust_only() {
        // 0183-H: no invented .ts / .go module aliases.
        assert!(alias_path_candidates("src/pkg.ts").is_empty());
        assert!(alias_path_candidates("src/pkg.go").is_empty());
        assert!(alias_path_candidates("src/pkg.py").is_empty());
        assert!(alias_path_candidates("src/pkg.cpp").is_empty());
    }

    #[test]
    fn resolve_exact_beats_alias() {
        let conn = mem_conn();
        seed_file(&conn, 1, "src/pkg.rs");
        seed_file(&conn, 2, "src/pkg/mod.rs");
        match resolve_indexed_file_path(&conn, "src/pkg.rs") {
            IndexedFileResolve::Unique { stored_path, .. } => {
                assert_eq!(stored_path, "src/pkg.rs");
            }
            other => panic!("expected Unique exact, got {other:?}"),
        }
    }

    #[test]
    fn resolve_alias_rs_to_mod_when_unique() {
        let conn = mem_conn();
        seed_file(&conn, 1, "src/pkg/mod.rs");
        match resolve_indexed_file_path(&conn, "src/pkg.rs") {
            IndexedFileResolve::Unique { stored_path, .. } => {
                assert_eq!(stored_path, "src/pkg/mod.rs");
            }
            other => panic!("expected Unique alias, got {other:?}"),
        }
    }

    #[test]
    fn resolve_extensionless_to_mod() {
        let conn = mem_conn();
        seed_file(&conn, 1, "src/pkg/mod.rs");
        match resolve_indexed_file_path(&conn, "src/pkg") {
            IndexedFileResolve::Unique { stored_path, .. } => {
                assert_eq!(stored_path, "src/pkg/mod.rs");
            }
            other => panic!("expected Unique extensionless, got {other:?}"),
        }
    }

    #[test]
    fn resolve_unique_suffix() {
        let conn = mem_conn();
        seed_file(&conn, 1, "src/commands/doctor/finding.rs");
        match resolve_indexed_file_path(&conn, "finding.rs") {
            IndexedFileResolve::Unique { stored_path, .. } => {
                assert_eq!(stored_path, "src/commands/doctor/finding.rs");
            }
            other => panic!("expected Unique suffix, got {other:?}"),
        }
    }

    #[test]
    fn resolve_ambiguous_suffix_refuses() {
        let conn = mem_conn();
        seed_file(&conn, 1, "src/a/mod.rs");
        seed_file(&conn, 2, "src/b/mod.rs");
        match resolve_indexed_file_path(&conn, "mod.rs") {
            IndexedFileResolve::Ambiguous { query, candidates } => {
                assert_eq!(query, "mod.rs");
                assert_eq!(
                    candidates,
                    vec!["src/a/mod.rs".to_string(), "src/b/mod.rs".to_string()]
                );
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn resolve_not_found() {
        let conn = mem_conn();
        seed_file(&conn, 1, "src/pkg/mod.rs");
        assert_eq!(
            resolve_indexed_file_path(&conn, "src/never_exists.rs"),
            IndexedFileResolve::NotFound
        );
    }

    #[test]
    fn resolve_does_not_symbol_fallback() {
        // File-only: a bare symbol name that is not a path must be NotFound here.
        let conn = mem_conn();
        conn.execute(
            "INSERT INTO project_files (id, file_path, last_indexed_at) \
             VALUES (1, 'src/lib.rs', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project_symbols \
             (id, file_id, qualified_name, symbol_name, symbol_kind, is_public, last_indexed_at) \
             VALUES (1, 1, 'foo', 'foo', 'Function', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        assert_eq!(
            resolve_indexed_file_path(&conn, "foo"),
            IndexedFileResolve::NotFound
        );
    }

    #[cfg(windows)]
    #[test]
    fn suffix_equality_case_insensitive_on_windows() {
        let conn = mem_conn();
        seed_file(&conn, 1, "src/pkg/Finding.rs");
        // Equality branch uses LOWER on Windows (0183-B).
        let hits = lookup_files_by_suffix(&conn, "finding.rs");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1, "src/pkg/Finding.rs");
    }
}
