use rusqlite::{Connection, Row};
use serde::Serialize;
use std::collections::BTreeSet;

const WITNESS_LIMIT: usize = 100;

// Every downstream query uses the same visible projection. Raw unresolved
// expressions, evidence payloads, and row ids never reach the witness packet.
const PROJECT: &str = r"
WITH paths AS (
    SELECT id, replace(file_path, char(92), '/') AS path
    FROM project_files WHERE parse_status != 'DELETED'
), safe_paths AS (
    SELECT id, path FROM paths
    WHERE length(path) BETWEEN 1 AND 4096 AND instr(path, char(0)) = 0
      AND path NOT GLOB ('*[' || char(1) || '-' || char(31) || char(127) || ']*')
      AND path NOT GLOB '/*' AND path NOT GLOB '*/'
      AND path NOT LIKE '%:%' AND path NOT LIKE '%//%'
      AND ('/' || path || '/') NOT LIKE '%/../%'
      AND ('/' || path || '/') NOT LIKE '%/./%'
), facts AS (
    SELECT cp.path AS caller_file, dp.path AS callee_file,
           CASE WHEN length(cs.symbol_name) BETWEEN 1 AND 512
                AND instr(cs.symbol_name, char(0)) = 0
                AND cs.symbol_name NOT GLOB '*[^A-Za-z0-9_$]*'
                THEN cs.symbol_name END AS caller_symbol,
           CASE WHEN length(ds.symbol_name) BETWEEN 1 AND 512
                AND instr(ds.symbol_name, char(0)) = 0
                AND ds.symbol_name NOT GLOB '*[^A-Za-z0-9_$]*'
                THEN ds.symbol_name END AS callee_symbol,
           CASE WHEN e.call_kind IN ('DIRECT', 'METHOD_CALL', 'TRAIT_DISPATCH', 'DYNAMIC', 'EXTERNAL')
                THEN lower(e.call_kind) ELSE 'unknown' END AS kind,
           CASE WHEN e.resolution_status IN ('RESOLVED', 'AMBIGUOUS', 'UNRESOLVED', 'CAPPED')
                THEN lower(e.resolution_status) ELSE 'unknown' END AS resolution,
           CASE WHEN e.evidence COLLATE BINARY = 'scip:ref' THEN 'scipReference'
                WHEN length(e.evidence) > instr(e.evidence, ':')
                 AND substr(e.evidence, 1, instr(e.evidence, ':') - 1)
                     IN ('call_expr', 'method_call', 'trait_dispatch', 'dynamic', 'new_expr', 'new', 'ctor')
                THEN 'nativeCall' ELSE 'unknown' END AS source,
           cf.id IS NULL OR cs.id IS NULL
             OR ((e.resolution_status = 'RESOLVED' OR e.callee_file_id IS NOT NULL OR e.callee_symbol_id IS NOT NULL)
                  AND (df.id IS NULL OR ds.id IS NULL)) AS dangling,
           (cf.id IS NOT NULL AND cp.path IS NULL)
             OR (df.id IS NOT NULL AND dp.path IS NULL) AS unsafe_path
    FROM structural_edges AS e
    LEFT JOIN project_files AS cf ON cf.id = e.caller_file_id AND cf.parse_status != 'DELETED'
    LEFT JOIN project_files AS df ON df.id = e.callee_file_id AND df.parse_status != 'DELETED'
    LEFT JOIN safe_paths AS cp ON cp.id = cf.id
    LEFT JOIN safe_paths AS dp ON dp.id = df.id
    LEFT JOIN project_symbols AS cs ON cs.id = e.caller_symbol_id AND cs.file_id = cf.id
    LEFT JOIN project_symbols AS ds ON ds.id = e.callee_symbol_id AND ds.file_id = df.id
), projected AS (
    SELECT caller_file, caller_symbol,
           CASE WHEN score THEN callee_file END AS callee_file,
           CASE WHEN score THEN callee_symbol END AS callee_symbol,
           kind, resolution,
           CASE WHEN dangling OR unsafe_path OR kind = 'unknown' OR resolution = 'unknown'
                THEN 'unknown' ELSE source END AS provenance,
           dangling, unsafe_path,
           caller_symbol IS NULL OR (resolution = 'resolved' AND callee_symbol IS NULL) AS unsafe_symbol
    FROM (
        SELECT *, NOT dangling AND NOT unsafe_path AND kind != 'unknown'
                     AND resolution = 'resolved' AND source != 'unknown'
                     AND caller_file IS NOT NULL AND caller_symbol IS NOT NULL
                     AND callee_file IS NOT NULL AND callee_symbol IS NOT NULL AS score
        FROM facts
    )
)
";

const GROUPED: &str = r",
groups AS (
    SELECT caller_file, caller_symbol, callee_file, callee_symbol, kind, resolution, provenance,
           COUNT(*) AS equivalent_rows
    FROM projected
    GROUP BY caller_file, caller_symbol, callee_file, callee_symbol, kind, resolution, provenance
)
";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Relationships {
    state: &'static str,
    reasons: BTreeSet<&'static str>,
    total_rows: Option<i64>,
    by_caller_language: Option<Vec<LanguageCount>>,
    by_provenance_and_resolution: Option<Vec<SourceCount>>,
    total_witnesses: Option<i64>,
    truncated: Option<bool>,
    witnesses: Vec<Witness>,
}

#[derive(Serialize)]
struct LanguageCount {
    language: String,
    rows: i64,
}

#[derive(Serialize)]
struct SourceCount {
    provenance: String,
    resolution: String,
    rows: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Witness {
    caller_file: Option<String>,
    caller_symbol: Option<String>,
    callee_file: Option<String>,
    callee_symbol: Option<String>,
    kind: String,
    resolution: String,
    provenance: String,
    equivalent_rows: i64,
}

impl Witness {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            caller_file: row.get(0)?,
            caller_symbol: row.get(1)?,
            callee_file: row.get(2)?,
            callee_symbol: row.get(3)?,
            kind: row.get(4)?,
            resolution: row.get(5)?,
            provenance: row.get(6)?,
            equivalent_rows: row.get(7)?,
        })
    }
}

impl Relationships {
    pub(super) fn unknown(reason: &'static str) -> Self {
        Self {
            state: "unknown",
            reasons: BTreeSet::from([reason]),
            total_rows: None,
            by_caller_language: None,
            by_provenance_and_resolution: None,
            total_witnesses: None,
            truncated: None,
            witnesses: Vec::new(),
        }
    }
}

pub(super) fn collect(conn: &Connection) -> Relationships {
    read(conn).unwrap_or_else(|_| Relationships::unknown("relationshipReadFailed"))
}

fn read(conn: &Connection) -> rusqlite::Result<Relationships> {
    let (total_rows, ambiguous, unresolved, capped, unknown, dangling, unsafe_path, unsafe_symbol) =
        conn.query_row(
            &format!(
                "{PROJECT}
            SELECT COUNT(*),
                   count(CASE WHEN resolution = 'ambiguous' THEN 1 END),
                   count(CASE WHEN resolution = 'unresolved' THEN 1 END),
                   count(CASE WHEN resolution = 'capped' THEN 1 END),
                   count(CASE WHEN provenance = 'unknown' THEN 1 END),
                   count(CASE WHEN dangling THEN 1 END),
                   count(CASE WHEN unsafe_path THEN 1 END),
                   count(CASE WHEN unsafe_symbol THEN 1 END)
            FROM projected"
            ),
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            },
        )?;
    let reasons = [
        ("ambiguous", ambiguous),
        ("unresolved", unresolved),
        ("capped", capped),
        ("unknownEvidence", unknown),
        ("danglingReference", dangling),
        ("unsafePath", unsafe_path),
        ("unsafeSymbol", unsafe_symbol),
    ]
    .into_iter()
    .filter_map(|(reason, count)| (count != 0).then_some(reason))
    .collect::<BTreeSet<_>>();
    let state = if total_rows == 0 {
        "unknown"
    } else if reasons.is_empty() {
        "observed"
    } else {
        "partial"
    };
    let language = super::language_sql("caller_file", "unknown");
    let mut stmt = conn.prepare(&format!(
        "{PROJECT}
        SELECT {language} AS language, COUNT(*) FROM projected GROUP BY language ORDER BY language"
    ))?;
    let by_caller_language = stmt
        .query_map([], |row| {
            Ok(LanguageCount {
                language: row.get(0)?,
                rows: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut stmt = conn.prepare(&format!(
        "{PROJECT}
        SELECT provenance, resolution, COUNT(*) FROM projected
        GROUP BY provenance, resolution ORDER BY provenance, resolution"
    ))?;
    let by_provenance_and_resolution = stmt
        .query_map([], |row| {
            Ok(SourceCount {
                provenance: row.get(0)?,
                resolution: row.get(1)?,
                rows: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let total_witnesses = conn.query_row(
        &format!("{PROJECT}{GROUPED} SELECT COUNT(*) FROM groups"),
        [],
        |row| row.get(0),
    )?;
    let mut stmt = conn.prepare(&format!("{PROJECT}{GROUPED}
        SELECT caller_file, caller_symbol, callee_file, callee_symbol, kind, resolution, provenance,
               equivalent_rows
        FROM groups
        ORDER BY caller_file, caller_symbol, callee_file, callee_symbol, kind, resolution, provenance
        LIMIT 101"))?;
    let mut witnesses = stmt
        .query_map([], Witness::read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let truncated = witnesses.len() > WITNESS_LIMIT;
    witnesses.truncate(WITNESS_LIMIT);
    let mut reasons = reasons;
    if total_rows == 0 {
        reasons.insert("noRecordedRelationships");
    }
    Ok(Relationships {
        state,
        reasons,
        total_rows: Some(total_rows),
        by_caller_language: Some(by_caller_language),
        by_provenance_and_resolution: Some(by_provenance_and_resolution),
        total_witnesses: Some(total_witnesses),
        truncated: Some(truncated),
        witnesses,
    })
}
