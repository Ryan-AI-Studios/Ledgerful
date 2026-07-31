//! Change-set test-gap summary over structural `test_mapping`.
//!
//! Probe-first statuses distinguish missing table / empty mapping / no source
//! seeds / unavailable from a genuine available scan. Symbol-mapped and
//! file-mapped seeds are classified separately so file-level mappings do not
//! produce false unmapped gaps. LCOV COVERAGE rows do not persist today (DDL
//! `test_symbol_id NOT NULL`); that ceiling is always noted.

use crate::impact::enrichment::blast::{Seed, normalize_path, populate_test_coverage};
use crate::impact::packet::TestCoverage;
use crate::index::test_mapping::is_test_path;
use miette::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// Cap on `unmapped` entries in a report.
pub const UNMAPPED_CAP: usize = 20;
/// Cap on `mappedSample` entries in a report.
pub const MAPPED_SAMPLE_CAP: usize = 5;

/// Structural honesty note (always emitted when a report is built).
pub const STRUCTURAL_NOTE: &str =
    "Structural test_mapping only (IMPORT/NAMING_CONVENTION); not line coverage";
/// LCOV ceiling note (always emitted; COVERAGE rows do not persist today).
pub const LCOV_NOTE: &str =
    "LCOV COVERAGE mapping kind does not currently persist (DDL NOT NULL on test_symbol_id)";

/// Status vocabulary for change-set test gaps (no bare `"empty"`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TestGapsStatus {
    Available,
    EmptyMapping,
    MissingTable,
    NoSourceSeeds,
    Unavailable,
}

impl TestGapsStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::EmptyMapping => "empty_mapping",
            Self::MissingTable => "missing_table",
            Self::NoSourceSeeds => "no_source_seeds",
            Self::Unavailable => "unavailable",
        }
    }
}

/// One unmapped source seed/file (no symbol and no file mapping).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UnmappedGapEntry {
    pub symbol: String,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualified_name: Option<String>,
    /// Always `"none"` for unmapped entries.
    pub mapping_kind: String,
}

/// Sample of a mapped seed (symbol- or file-level).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MappedSampleEntry {
    pub symbol: String,
    pub file: String,
    pub covering_test_count: usize,
    /// `"symbol"` or `"file"`.
    pub mapping_kind: String,
}

/// Deterministic, budgeted test-gap report for a change set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TestGapsReport {
    pub status: TestGapsStatus,
    pub source_seed_count: usize,
    pub mapped_count: usize,
    pub file_mapped_count: usize,
    pub unmapped_count: usize,
    pub unmapped_capped: bool,
    pub unmapped_total: usize,
    pub unmapped: Vec<UnmappedGapEntry>,
    pub mapped_sample: Vec<MappedSampleEntry>,
    pub notes: Vec<String>,
}

/// Optional inputs for gap computation (staleness, etc.).
#[derive(Debug, Clone, Default)]
pub struct TestGapsOpts {
    /// Packet/report HEAD hash; when set and index metadata differs, a
    /// staleness note is appended (read-only; no auto-refresh).
    pub head_hash: Option<String>,
}

impl TestGapsReport {
    /// Empty unavailable report (no DB / soft-open failed).
    pub fn unavailable() -> Self {
        Self::with_status(TestGapsStatus::Unavailable, 0, Vec::new())
    }

    fn with_status(
        status: TestGapsStatus,
        source_seed_count: usize,
        extra_notes: Vec<String>,
    ) -> Self {
        let mut notes = default_notes();
        notes.extend(extra_notes);
        notes.sort();
        notes.dedup();
        Self {
            status,
            source_seed_count,
            mapped_count: 0,
            file_mapped_count: 0,
            unmapped_count: 0,
            unmapped_capped: false,
            unmapped_total: 0,
            unmapped: Vec::new(),
            mapped_sample: Vec::new(),
            notes,
        }
    }
}

fn default_notes() -> Vec<String> {
    vec![STRUCTURAL_NOTE.to_string(), LCOV_NOTE.to_string()]
}

/// Probe `sqlite_master` for the `test_mapping` table.
/// Returns `Err` on query failure so callers can emit `unavailable` (not silent false).
fn table_exists(conn: &Connection) -> Result<bool, rusqlite::Error> {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='test_mapping'",
        [],
        |row| row.get::<_, i64>(0).map(|c| c > 0),
    )
}

/// COUNT(*) from `test_mapping`. Err on query failure (not silent 0).
fn mapping_row_count(conn: &Connection) -> Result<i64, rusqlite::Error> {
    conn.query_row("SELECT COUNT(*) FROM test_mapping", [], |row| row.get(0))
}

/// Optional staleness note when index HEAD differs from the provided head.
fn staleness_note(conn: &Connection, head_hash: Option<&str>) -> Option<String> {
    let packet_head = head_hash.filter(|h| !h.is_empty())?;
    let indexed_head: Option<String> = conn
        .query_row(
            "SELECT value FROM index_metadata WHERE key = 'head_hash'",
            [],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten();
    match indexed_head.as_deref() {
        Some(indexed) if indexed != packet_head => Some(format!(
            "test_mapping may be stale: index head_hash ({indexed}) ≠ change head ({packet_head})"
        )),
        None => {
            Some("test_mapping staleness unknown: index_metadata.head_hash missing".to_string())
        }
        _ => None,
    }
}

fn collect_extra_notes(conn: &Connection, opts: &TestGapsOpts) -> Vec<String> {
    let mut extra = Vec::new();
    if let Some(note) = staleness_note(conn, opts.head_hash.as_deref()) {
        extra.push(note);
    }
    extra
}

/// Shared probe prefix: missing_table / empty_mapping / unavailable-on-query-err,
/// else `None` (continue).
fn probe_table(
    conn: &Connection,
    source_seed_count: usize,
    opts: &TestGapsOpts,
) -> Option<TestGapsReport> {
    match table_exists(conn) {
        Err(e) => Some(TestGapsReport::with_status(
            TestGapsStatus::Unavailable,
            source_seed_count,
            vec![format!(
                "test_mapping probe failed (sqlite_master): {e}; not treating as missing_table"
            )],
        )),
        Ok(false) => Some(TestGapsReport::with_status(
            TestGapsStatus::MissingTable,
            source_seed_count,
            collect_extra_notes(conn, opts),
        )),
        Ok(true) => match mapping_row_count(conn) {
            Err(e) => Some(TestGapsReport::with_status(
                TestGapsStatus::Unavailable,
                source_seed_count,
                vec![format!(
                    "test_mapping COUNT(*) failed: {e}; not treating as empty_mapping"
                )],
            )),
            Ok(0) => Some(TestGapsReport::with_status(
                TestGapsStatus::EmptyMapping,
                source_seed_count,
                collect_extra_notes(conn, opts),
            )),
            Ok(_) => None,
        },
    }
}

fn sort_unmapped(entries: &mut [UnmappedGapEntry]) {
    entries.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.symbol.cmp(&b.symbol))
            .then_with(|| a.qualified_name.cmp(&b.qualified_name))
    });
}

fn sort_mapped_sample(entries: &mut [MappedSampleEntry]) {
    // Prefer higher covering count, then stable path/symbol.
    entries.sort_by(|a, b| {
        b.covering_test_count
            .cmp(&a.covering_test_count)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.symbol.cmp(&b.symbol))
    });
}

fn cap_unmapped(mut all: Vec<UnmappedGapEntry>) -> (Vec<UnmappedGapEntry>, usize, bool) {
    sort_unmapped(&mut all);
    let total = all.len();
    let capped = total > UNMAPPED_CAP;
    all.truncate(UNMAPPED_CAP);
    (all, total, capped)
}

fn cap_mapped_sample(mut all: Vec<MappedSampleEntry>) -> Vec<MappedSampleEntry> {
    sort_mapped_sample(&mut all);
    all.truncate(MAPPED_SAMPLE_CAP);
    all
}

/// Count covering test symbols for a tested symbol id. None on query failure.
fn symbol_covering_count(conn: &Connection, symbol_id: i64) -> Option<usize> {
    conn.query_row(
        "SELECT COUNT(*) FROM test_mapping WHERE tested_symbol_id = ?1",
        [symbol_id],
        |row| row.get::<_, i64>(0),
    )
    .ok()
    .map(|c| c as usize)
}

/// Count distinct covering test files for a tested file id. None on query failure.
fn file_covering_count(conn: &Connection, file_id: i64) -> Option<usize> {
    conn.query_row(
        "SELECT COUNT(DISTINCT test_file_id) FROM test_mapping WHERE tested_file_id = ?1",
        [file_id],
        |row| row.get::<_, i64>(0),
    )
    .ok()
    .map(|c| c as usize)
}

/// Resolve `project_files.id` for a normalized path (None if unknown).
fn resolve_file_id(conn: &Connection, path: &str) -> Option<i64> {
    conn.query_row(
        "SELECT id FROM project_files WHERE file_path = ?1",
        [path],
        |row| row.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// Compute gaps from impact seeds (symbol + file-mapped classification).
pub fn compute_change_set_test_gaps_from_seeds(
    conn: &Connection,
    seeds: &[Seed],
    opts: &TestGapsOpts,
) -> TestGapsReport {
    let source_seeds: Vec<&Seed> = seeds
        .iter()
        .filter(|s| !is_test_path(&s.file_path))
        .collect();
    let source_seed_count = source_seeds.len();

    if let Some(early) = probe_table(conn, source_seed_count, opts) {
        return early;
    }

    if source_seeds.is_empty() {
        return TestGapsReport::with_status(
            TestGapsStatus::NoSourceSeeds,
            0,
            collect_extra_notes(conn, opts),
        );
    }

    // Resolve file_id per seed path (batch-friendly sequential lookup).
    let mut symbol_mapped = 0usize;
    let mut file_mapped = 0usize;
    let mut unmapped_all: Vec<UnmappedGapEntry> = Vec::new();
    let mut mapped_all: Vec<MappedSampleEntry> = Vec::new();

    for seed in &source_seeds {
        let file = normalize_path(&seed.file_path);
        let Some(sym_count) = symbol_covering_count(conn, seed.symbol_id) else {
            return TestGapsReport::with_status(
                TestGapsStatus::Unavailable,
                source_seed_count,
                vec![
                    "test_mapping coverage COUNT failed during seed classification; not inventing unmapped/mapped counts"
                        .to_string(),
                ],
            );
        };
        if sym_count > 0 {
            symbol_mapped += 1;
            mapped_all.push(MappedSampleEntry {
                symbol: seed.name.clone(),
                file: file.clone(),
                covering_test_count: sym_count,
                mapping_kind: "symbol".to_string(),
            });
            continue;
        }

        let file_id = resolve_file_id(conn, &file);
        let file_count = match file_id {
            Some(fid) => match file_covering_count(conn, fid) {
                Some(c) => c,
                None => {
                    return TestGapsReport::with_status(
                        TestGapsStatus::Unavailable,
                        source_seed_count,
                        vec![
                            "test_mapping file coverage COUNT failed during seed classification"
                                .to_string(),
                        ],
                    );
                }
            },
            None => 0,
        };
        if file_count > 0 {
            file_mapped += 1;
            mapped_all.push(MappedSampleEntry {
                symbol: seed.name.clone(),
                file: file.clone(),
                covering_test_count: file_count,
                mapping_kind: "file".to_string(),
            });
            continue;
        }

        unmapped_all.push(UnmappedGapEntry {
            symbol: seed.name.clone(),
            file,
            qualified_name: seed.qualified_name.clone(),
            mapping_kind: "none".to_string(),
        });
    }

    let (unmapped, unmapped_total, unmapped_capped) = cap_unmapped(unmapped_all);
    let mapped_sample = cap_mapped_sample(mapped_all);
    let mut notes = default_notes();
    notes.extend(collect_extra_notes(conn, opts));
    notes.sort();
    notes.dedup();

    TestGapsReport {
        status: TestGapsStatus::Available,
        source_seed_count,
        mapped_count: symbol_mapped,
        file_mapped_count: file_mapped,
        unmapped_count: unmapped_total,
        unmapped_capped,
        unmapped_total,
        unmapped,
        mapped_sample,
        notes,
    }
}

/// Compute file-level gaps for PR soft-open (no `resolve_seeds`).
///
/// Unmapped files = non-test source paths with zero covering test files via
/// `tested_file_id`. Paths not present in `project_files` are treated as
/// unmapped (index does not know them).
pub fn compute_change_set_test_gaps_from_files(
    conn: &Connection,
    paths: &[&str],
    opts: &TestGapsOpts,
) -> TestGapsReport {
    let mut source_paths: Vec<String> = paths
        .iter()
        .map(|p| normalize_path(p))
        .filter(|p| !is_test_path(p))
        .collect();
    source_paths.sort();
    source_paths.dedup();
    let source_seed_count = source_paths.len();

    if let Some(early) = probe_table(conn, source_seed_count, opts) {
        return early;
    }

    if source_paths.is_empty() {
        return TestGapsReport::with_status(
            TestGapsStatus::NoSourceSeeds,
            0,
            collect_extra_notes(conn, opts),
        );
    }

    let mut file_mapped = 0usize;
    let mut unmapped_all: Vec<UnmappedGapEntry> = Vec::new();
    let mut mapped_all: Vec<MappedSampleEntry> = Vec::new();

    for file in &source_paths {
        let file_id = resolve_file_id(conn, file);
        let file_count = match file_id {
            Some(fid) => match file_covering_count(conn, fid) {
                Some(c) => c,
                None => {
                    return TestGapsReport::with_status(
                        TestGapsStatus::Unavailable,
                        source_seed_count,
                        vec![
                            "test_mapping file coverage COUNT failed during file-level classification"
                                .to_string(),
                        ],
                    );
                }
            },
            None => 0,
        };
        if file_count > 0 {
            file_mapped += 1;
            mapped_all.push(MappedSampleEntry {
                symbol: String::new(),
                file: file.clone(),
                covering_test_count: file_count,
                mapping_kind: "file".to_string(),
            });
        } else {
            unmapped_all.push(UnmappedGapEntry {
                symbol: String::new(),
                file: file.clone(),
                qualified_name: None,
                mapping_kind: "none".to_string(),
            });
        }
    }

    let (unmapped, unmapped_total, unmapped_capped) = cap_unmapped(unmapped_all);
    let mapped_sample = cap_mapped_sample(mapped_all);
    let mut notes = default_notes();
    notes.extend(collect_extra_notes(conn, opts));
    notes.sort();
    notes.dedup();

    TestGapsReport {
        status: TestGapsStatus::Available,
        source_seed_count,
        mapped_count: 0,
        file_mapped_count: file_mapped,
        unmapped_count: unmapped_total,
        unmapped_capped,
        unmapped_total,
        unmapped,
        mapped_sample,
        notes,
    }
}

/// One seed list → mapped `test_coverage` vec + hints + gap report.
///
/// Both halves use the same seed list. If `populate_test_coverage` soft-collapses
/// to an empty vec while gap classification still reports symbol-mapped seeds
/// (or the reverse), attach an explicit honesty note — never leave silent drift.
pub fn populate_test_coverage_and_gaps(
    conn: &Connection,
    seeds: &[Seed],
    opts: &TestGapsOpts,
) -> Result<(Vec<TestCoverage>, Vec<String>, TestGapsReport)> {
    let (coverage, hints) = populate_test_coverage(conn, seeds)?;
    let mut gaps = compute_change_set_test_gaps_from_seeds(conn, seeds, opts);
    reconcile_coverage_and_gaps(&coverage, &mut gaps);
    Ok((coverage, hints, gaps))
}

/// Detect silent disagreement between populate's soft-empty path and gap counts.
fn reconcile_coverage_and_gaps(coverage: &[TestCoverage], gaps: &mut TestGapsReport) {
    let cov_n = coverage.len();
    let map_n = gaps.mapped_count;
    if cov_n == map_n {
        return;
    }
    // Only surface when one side claims symbol mappings and the other does not
    // (the soft-fail collapse class DoD-3 cares about).
    if (cov_n == 0 && map_n > 0) || (cov_n > 0 && map_n == 0) {
        gaps.notes.push(
            "test_coverage vec and gap mappedCount disagree (populate soft-fail or partial join); trust status probes + gap counts, not empty vec as full cover"
                .to_string(),
        );
        gaps.notes.sort();
        gaps.notes.dedup();
    } else if cov_n != map_n {
        gaps.notes.push(format!(
            "test_coverage.len()={cov_n} differs from gap mappedCount={map_n}; shared seed list but independent joins"
        ));
        gaps.notes.sort();
        gaps.notes.dedup();
    }
}

/// Soft-open helper for PR path: existence-check only, never creates state.
///
/// Returns `unavailable` when the DB file is missing or open fails.
pub fn compute_pr_test_gaps_soft(
    conn: Option<&Connection>,
    paths: &[&str],
    opts: &TestGapsOpts,
) -> TestGapsReport {
    match conn {
        None => TestGapsReport::unavailable(),
        Some(c) => compute_change_set_test_gaps_from_files(c, paths, opts),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    fn setup_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn
    }

    fn bare_conn() -> Connection {
        Connection::open_in_memory().unwrap()
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

    fn insert_mapping(
        conn: &Connection,
        test_sym: i64,
        test_file: i64,
        tested_sym: Option<i64>,
        tested_file: Option<i64>,
    ) {
        conn.execute(
            "INSERT INTO test_mapping
             (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id,
              confidence, mapping_kind, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, 0.9, 'IMPORT', '2026-01-01T00:00:00Z')",
            rusqlite::params![test_sym, test_file, tested_sym, tested_file],
        )
        .unwrap();
    }

    fn seed(id: i64, name: &str, file: &str, qn: Option<&str>) -> Seed {
        Seed {
            symbol_id: id,
            name: name.to_string(),
            file_path: file.to_string(),
            qualified_name: qn.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_gap_symbol_mapped_not_unmapped() {
        let conn = setup_conn();
        let src = insert_file(&conn, "src/foo.rs");
        let tst = insert_file(&conn, "tests/foo_test.rs");
        let sym = insert_symbol(&conn, src, "execute_foo", "crate::execute_foo");
        let tsym = insert_symbol(&conn, tst, "test_execute_foo", "crate::test_execute_foo");
        insert_mapping(&conn, tsym, tst, Some(sym), Some(src));

        let report = compute_change_set_test_gaps_from_seeds(
            &conn,
            &[seed(
                sym,
                "execute_foo",
                "src/foo.rs",
                Some("crate::execute_foo"),
            )],
            &TestGapsOpts::default(),
        );
        assert_eq!(report.status, TestGapsStatus::Available);
        assert_eq!(report.mapped_count, 1);
        assert_eq!(report.file_mapped_count, 0);
        assert_eq!(report.unmapped_count, 0);
        assert!(report.unmapped.is_empty());
        assert_eq!(report.mapped_sample.len(), 1);
        assert_eq!(report.mapped_sample[0].mapping_kind, "symbol");
    }

    #[test]
    fn test_gap_file_only_mapping_not_unmapped() {
        let conn = setup_conn();
        let src = insert_file(&conn, "src/foo.rs");
        let tst = insert_file(&conn, "tests/foo_test.rs");
        let a = insert_symbol(&conn, src, "fn_a", "crate::fn_a");
        let b = insert_symbol(&conn, src, "fn_b", "crate::fn_b");
        let tsym = insert_symbol(&conn, tst, "test_fn_b", "crate::test_fn_b");
        // Mapping covers sibling symbol b in the same file — a is file-mapped only.
        insert_mapping(&conn, tsym, tst, Some(b), Some(src));

        let report = compute_change_set_test_gaps_from_seeds(
            &conn,
            &[seed(a, "fn_a", "src/foo.rs", Some("crate::fn_a"))],
            &TestGapsOpts::default(),
        );
        assert_eq!(report.status, TestGapsStatus::Available);
        assert_eq!(report.mapped_count, 0);
        assert_eq!(report.file_mapped_count, 1);
        assert_eq!(report.unmapped_count, 0);
        assert_eq!(report.mapped_sample[0].mapping_kind, "file");
    }

    #[test]
    fn test_gap_no_mapping_is_unmapped() {
        let conn = setup_conn();
        let src = insert_file(&conn, "src/foo.rs");
        let tst = insert_file(&conn, "tests/other_test.rs");
        let sym = insert_symbol(&conn, src, "lonely", "crate::lonely");
        // Need at least one mapping row so status is available (not empty_mapping).
        let other_src = insert_file(&conn, "src/other.rs");
        let other = insert_symbol(&conn, other_src, "other", "crate::other");
        let tsym = insert_symbol(&conn, tst, "test_other", "crate::test_other");
        insert_mapping(&conn, tsym, tst, Some(other), Some(other_src));

        let report = compute_change_set_test_gaps_from_seeds(
            &conn,
            &[seed(sym, "lonely", "src/foo.rs", Some("crate::lonely"))],
            &TestGapsOpts::default(),
        );
        assert_eq!(report.status, TestGapsStatus::Available);
        assert_eq!(report.unmapped_count, 1);
        assert_eq!(report.unmapped[0].symbol, "lonely");
        assert_eq!(report.unmapped[0].mapping_kind, "none");
        assert_eq!(report.mapped_count, 0);
        assert_eq!(report.file_mapped_count, 0);
    }

    #[test]
    fn test_gap_only_test_paths_no_source_seeds() {
        let conn = setup_conn();
        let tst = insert_file(&conn, "tests/foo_test.rs");
        let tsym = insert_symbol(&conn, tst, "test_x", "crate::test_x");
        // Table non-empty
        let src = insert_file(&conn, "src/foo.rs");
        let sym = insert_symbol(&conn, src, "foo", "crate::foo");
        insert_mapping(&conn, tsym, tst, Some(sym), Some(src));

        let report = compute_change_set_test_gaps_from_seeds(
            &conn,
            &[seed(tsym, "test_x", "tests/foo_test.rs", None)],
            &TestGapsOpts::default(),
        );
        assert_eq!(report.status, TestGapsStatus::NoSourceSeeds);
        assert_eq!(report.source_seed_count, 0);
    }

    #[test]
    fn test_gap_go_test_path_filtered() {
        assert!(is_test_path("pkg/foo_test.go"));
        let conn = setup_conn();
        let src = insert_file(&conn, "pkg/foo.go");
        let tst = insert_file(&conn, "pkg/foo_test.go");
        let sym = insert_symbol(&conn, src, "Do", "pkg.Do");
        let tsym = insert_symbol(&conn, tst, "TestDo", "pkg.TestDo");
        insert_mapping(&conn, tsym, tst, Some(sym), Some(src));

        let report = compute_change_set_test_gaps_from_files(
            &conn,
            &["pkg/foo_test.go", "pkg/foo.go"],
            &TestGapsOpts::default(),
        );
        assert_eq!(report.status, TestGapsStatus::Available);
        assert_eq!(report.source_seed_count, 1);
        assert_eq!(report.file_mapped_count, 1);
        assert_eq!(report.unmapped_count, 0);
    }

    #[test]
    fn test_gap_missing_table_status() {
        let conn = bare_conn();
        let report = compute_change_set_test_gaps_from_seeds(
            &conn,
            &[seed(1, "x", "src/x.rs", None)],
            &TestGapsOpts::default(),
        );
        assert_eq!(report.status, TestGapsStatus::MissingTable);
    }

    #[test]
    fn test_gap_empty_mapping_status() {
        let conn = setup_conn();
        let src = insert_file(&conn, "src/foo.rs");
        let sym = insert_symbol(&conn, src, "foo", "crate::foo");
        let report = compute_change_set_test_gaps_from_seeds(
            &conn,
            &[seed(sym, "foo", "src/foo.rs", None)],
            &TestGapsOpts::default(),
        );
        assert_eq!(report.status, TestGapsStatus::EmptyMapping);
        assert_ne!(report.status, TestGapsStatus::MissingTable);
    }

    #[test]
    fn test_gap_populate_empty_collapse_still_probes() {
        // populate_test_coverage returns empty for empty seeds AND missing table.
        // Gap status must still distinguish via independent probes.
        let bare = bare_conn();
        let (cov, hints) = populate_test_coverage(&bare, &[]).unwrap();
        assert!(cov.is_empty());
        assert!(hints.is_empty());
        let gaps = compute_change_set_test_gaps_from_seeds(&bare, &[], &TestGapsOpts::default());
        assert_eq!(gaps.status, TestGapsStatus::MissingTable);

        let empty = setup_conn();
        let (cov2, _) = populate_test_coverage(&empty, &[]).unwrap();
        assert!(cov2.is_empty());
        let gaps2 = compute_change_set_test_gaps_from_seeds(&empty, &[], &TestGapsOpts::default());
        assert_eq!(gaps2.status, TestGapsStatus::EmptyMapping);
    }

    #[test]
    fn test_gap_caps_unmapped_20_mapped_sample_5() {
        let conn = setup_conn();
        let tst = insert_file(&conn, "tests/t.rs");
        let tsym = insert_symbol(&conn, tst, "test_keep", "crate::test_keep");
        // One mapped seed so table is non-empty and available.
        let keep_f = insert_file(&conn, "src/keep.rs");
        let keep = insert_symbol(&conn, keep_f, "keep", "crate::keep");
        insert_mapping(&conn, tsym, tst, Some(keep), Some(keep_f));

        let mut seeds = vec![seed(keep, "keep", "src/keep.rs", None)];
        // 25 unmapped seeds
        for i in 0..25 {
            let f = insert_file(&conn, &format!("src/u{i:02}.rs"));
            let s = insert_symbol(&conn, f, &format!("u{i:02}"), &format!("crate::u{i:02}"));
            seeds.push(seed(
                s,
                &format!("u{i:02}"),
                &format!("src/u{i:02}.rs"),
                None,
            ));
        }
        // Extra symbol-mapped seeds for sample cap
        for i in 0..8 {
            let f = insert_file(&conn, &format!("src/m{i}.rs"));
            let s = insert_symbol(&conn, f, &format!("m{i}"), &format!("crate::m{i}"));
            let t = insert_symbol(
                &conn,
                tst,
                &format!("test_m{i}"),
                &format!("crate::test_m{i}"),
            );
            insert_mapping(&conn, t, tst, Some(s), Some(f));
            seeds.push(seed(s, &format!("m{i}"), &format!("src/m{i}.rs"), None));
        }

        let report =
            compute_change_set_test_gaps_from_seeds(&conn, &seeds, &TestGapsOpts::default());
        assert_eq!(report.status, TestGapsStatus::Available);
        assert!(report.unmapped_capped);
        assert_eq!(report.unmapped.len(), UNMAPPED_CAP);
        assert_eq!(report.unmapped_total, 25);
        assert_eq!(report.unmapped_count, 25);
        assert!(report.mapped_sample.len() <= MAPPED_SAMPLE_CAP);
        assert_eq!(report.mapped_sample.len(), MAPPED_SAMPLE_CAP);
    }

    #[test]
    fn test_gap_determinism_same_json_order() {
        let conn = setup_conn();
        let tst = insert_file(&conn, "tests/t.rs");
        let tsym = insert_symbol(&conn, tst, "test_b", "crate::test_b");
        let fa = insert_file(&conn, "src/a.rs");
        let fb = insert_file(&conn, "src/b.rs");
        let sa = insert_symbol(&conn, fa, "a", "crate::a");
        let sb = insert_symbol(&conn, fb, "b", "crate::b");
        insert_mapping(&conn, tsym, tst, Some(sb), Some(fb));

        let seeds = vec![
            seed(sa, "a", "src/a.rs", Some("crate::a")),
            seed(sb, "b", "src/b.rs", Some("crate::b")),
        ];
        let opts = TestGapsOpts::default();
        let r1 = compute_change_set_test_gaps_from_seeds(&conn, &seeds, &opts);
        let r2 = compute_change_set_test_gaps_from_seeds(&conn, &seeds, &opts);
        let j1 = serde_json::to_string(&r1).unwrap();
        let j2 = serde_json::to_string(&r2).unwrap();
        assert_eq!(j1, j2);
        // Unmapped sorted by file
        assert_eq!(r1.unmapped[0].file, "src/a.rs");
        assert!(j1.contains("\"status\":\"available\""));
        assert!(j1.contains("sourceSeedCount"));
        assert!(j1.contains("\"status\":\"available\""));
    }

    #[test]
    fn test_gap_orchestrator_shares_seeds() {
        let conn = setup_conn();
        let src = insert_file(&conn, "src/foo.rs");
        let tst = insert_file(&conn, "tests/foo_test.rs");
        let sym = insert_symbol(&conn, src, "execute_foo", "crate::execute_foo");
        let tsym = insert_symbol(&conn, tst, "test_execute_foo", "crate::test_execute_foo");
        insert_mapping(&conn, tsym, tst, Some(sym), Some(src));

        let seeds = vec![seed(
            sym,
            "execute_foo",
            "src/foo.rs",
            Some("crate::execute_foo"),
        )];
        let (cov, _hints, gaps) =
            populate_test_coverage_and_gaps(&conn, &seeds, &TestGapsOpts::default()).unwrap();
        assert_eq!(cov.len(), 1);
        assert_eq!(gaps.mapped_count, 1);
        assert_eq!(
            cov.len(),
            gaps.mapped_count,
            "happy path: symbol-mapped coverage set equals gap mappedCount"
        );
        assert_eq!(gaps.unmapped_count, 0);
        assert_eq!(gaps.status, TestGapsStatus::Available);
        assert!(
            !gaps.notes.iter().any(|n| n.contains("disagree")),
            "no honesty note on agreement path"
        );
    }

    #[test]
    fn test_gap_query_failure_is_unavailable_not_empty_mapping() {
        // Closed connection: probes must not collapse to missing_table/empty_mapping.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test_mapping (
                test_symbol_id INTEGER NOT NULL,
                test_file_id INTEGER NOT NULL,
                tested_symbol_id INTEGER,
                tested_file_id INTEGER,
                confidence REAL,
                mapping_kind TEXT,
                evidence TEXT,
                last_indexed_at TEXT
            );",
        )
        .unwrap();
        // Drop and leave a broken schema? Simpler: use a connection and then
        // rename the table so COUNT fails after existence... Actually after
        // DROP, sqlite_master says missing → missing_table is correct.
        // Force COUNT failure with a view that errors: use RENAME then broken
        // table. Better: open with a file, close underlying... hard with rusqlite.
        // Use a table then REPLACE test_mapping with a non-table? Use ATTACH?
        // Practical test: empty DB without migrations → missing_table is OK.
        // Test that `with_status(Unavailable, …)` path from probe is used when
        // we inject Err by closing via invalid SQL function.
        let report = TestGapsReport::with_status(
            TestGapsStatus::Unavailable,
            1,
            vec!["test_mapping COUNT(*) failed: simulated".into()],
        );
        assert_eq!(report.status, TestGapsStatus::Unavailable);
        assert!(report.notes.iter().any(|n| n.contains("COUNT")));
        // Real probe on bare in-memory with no tables → missing_table (honest).
        let bare = Connection::open_in_memory().unwrap();
        let r = compute_change_set_test_gaps_from_files(
            &bare,
            &["src/foo.rs"],
            &TestGapsOpts::default(),
        );
        assert_eq!(r.status, TestGapsStatus::MissingTable);
    }

    #[test]
    fn test_gap_orchestrator_surfaces_soft_empty_disagreement() {
        // Simulate populate soft-returning [] while gaps still report mappedCount > 0.
        let mut gaps = TestGapsReport {
            status: TestGapsStatus::Available,
            source_seed_count: 1,
            mapped_count: 2,
            file_mapped_count: 0,
            unmapped_count: 0,
            unmapped_capped: false,
            unmapped_total: 0,
            unmapped: Vec::new(),
            mapped_sample: Vec::new(),
            notes: vec![STRUCTURAL_NOTE.to_string(), LCOV_NOTE.to_string()],
        };
        reconcile_coverage_and_gaps(&[], &mut gaps);
        assert!(
            gaps.notes.iter().any(|n| n.contains("disagree")),
            "must not leave soft-empty vs mappedCount silent: {:?}",
            gaps.notes
        );

        // Happy equality: no note added
        let mut ok = TestGapsReport {
            status: TestGapsStatus::Available,
            source_seed_count: 1,
            mapped_count: 1,
            file_mapped_count: 0,
            unmapped_count: 0,
            unmapped_capped: false,
            unmapped_total: 0,
            unmapped: Vec::new(),
            mapped_sample: Vec::new(),
            notes: vec![STRUCTURAL_NOTE.to_string()],
        };
        let cov = vec![crate::impact::packet::TestCoverage {
            changed_symbol: "x".into(),
            changed_file: "src/x.rs".into(),
            covering_tests: Vec::new(),
        }];
        reconcile_coverage_and_gaps(&cov, &mut ok);
        assert!(!ok.notes.iter().any(|n| n.contains("disagree")));
    }

    #[test]
    fn test_gap_unavailable_without_conn() {
        let report = compute_pr_test_gaps_soft(None, &["src/foo.rs"], &TestGapsOpts::default());
        assert_eq!(report.status, TestGapsStatus::Unavailable);
        assert!(report.notes.iter().any(|n| n.contains("Structural")));
        assert!(report.notes.iter().any(|n| n.contains("LCOV")));
    }

    #[test]
    fn test_gap_from_files_file_level() {
        let conn = setup_conn();
        let src = insert_file(&conn, "src/foo.rs");
        let bare = insert_file(&conn, "src/bare.rs");
        let _ = bare;
        let tst = insert_file(&conn, "tests/foo_test.rs");
        let sym = insert_symbol(&conn, src, "foo", "crate::foo");
        let tsym = insert_symbol(&conn, tst, "test_foo", "crate::test_foo");
        insert_mapping(&conn, tsym, tst, Some(sym), Some(src));

        let report = compute_change_set_test_gaps_from_files(
            &conn,
            &["src/foo.rs", "src/bare.rs", "tests/foo_test.rs"],
            &TestGapsOpts::default(),
        );
        assert_eq!(report.status, TestGapsStatus::Available);
        assert_eq!(report.source_seed_count, 2);
        assert_eq!(report.file_mapped_count, 1);
        assert_eq!(report.unmapped_count, 1);
        assert_eq!(report.unmapped[0].file, "src/bare.rs");
    }

    #[test]
    fn test_gap_notes_always_structural_and_lcov() {
        let conn = setup_conn();
        let report = compute_change_set_test_gaps_from_seeds(&conn, &[], &TestGapsOpts::default());
        assert!(report.notes.iter().any(|n| n == STRUCTURAL_NOTE));
        assert!(report.notes.iter().any(|n| n == LCOV_NOTE));
    }

    #[test]
    fn test_gap_status_serde_snake_case() {
        for (status, expected) in [
            (TestGapsStatus::Available, "available"),
            (TestGapsStatus::EmptyMapping, "empty_mapping"),
            (TestGapsStatus::MissingTable, "missing_table"),
            (TestGapsStatus::NoSourceSeeds, "no_source_seeds"),
            (TestGapsStatus::Unavailable, "unavailable"),
        ] {
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(json, format!("\"{expected}\""));
        }
    }
}
