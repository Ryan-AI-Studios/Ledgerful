use crate::ledger::error::LedgerError;
use rusqlite::Connection;

/// Shared FTS MATCH + filter predicates for ledger search and omitted-count.
///
/// Hyphenated queries are wrapped as a phrase so FTS5 does not treat `-` as a
/// column qualifier. `next_param_idx` is the single source for subsequent
/// bound parameters (LIMIT/OFFSET).
struct FtsWhere {
    clause: String,
    params: Vec<Box<dyn rusqlite::types::ToSql>>,
    next_param_idx: u32,
}

fn fts_where(
    query: &str,
    category: Option<&str>,
    days: Option<u64>,
    breaking_only: bool,
) -> FtsWhere {
    // FTS5 interprets hyphens as column qualifiers (e.g. "dead-code" →
    // column "code"), so wrap the query in double quotes to treat it as a
    // phrase search. This prevents "no such column" errors on hyphenated
    // search terms.
    let fts_query = format!("\"{query}\"");
    let mut clause = "WHERE ledger_fts MATCH ?1".to_string();
    let mut param_idx = 2u32;
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(fts_query)];

    if let Some(cat) = category {
        clause.push_str(&format!(" AND l.category = ?{param_idx}"));
        params.push(Box::new(cat.to_string()));
        param_idx += 1;
    }

    if let Some(d) = days {
        clause.push_str(&format!(
            " AND l.committed_at >= strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?{param_idx})"
        ));
        params.push(Box::new(format!("-{d} days")));
        param_idx += 1;
    }

    if breaking_only {
        clause.push_str(" AND l.is_breaking = 1");
    }

    FtsWhere {
        clause,
        params,
        next_param_idx: param_idx,
    }
}

fn map_fts_prepare_error(e: rusqlite::Error) -> LedgerError {
    if let rusqlite::Error::SqliteFailure(_err, Some(msg)) = &e
        && msg.contains("syntax error")
    {
        return LedgerError::Validation(format!("Invalid search query: {}", msg));
    }
    LedgerError::from(e)
}

fn map_fts_row_error(e: rusqlite::Error) -> LedgerError {
    if let rusqlite::Error::SqliteFailure(_err, Some(msg)) = &e
        && msg.contains("syntax error")
    {
        return LedgerError::Validation(format!("Invalid search query syntax: {}", msg));
    }
    LedgerError::from(e)
}

const SEARCH_SELECT: &str = "SELECT l.id, l.tx_id, l.category, l.entry_type, l.entity, l.entity_normalized,
            l.change_type, l.summary, l.reason, l.is_breaking, l.committed_at,
            l.verification_status, l.verification_basis, l.outcome_notes,
            l.origin, l.trace_id, l.signature, l.public_key, l.risk, l.related_tickets, l.author, l.observed, l.prev_hash, l.sig_version
     FROM ledger_entries l
     JOIN ledger_fts f ON f.rowid = l.id
     ";

#[allow(clippy::too_many_arguments)] // include_rollback is a required extra filter (0213)
pub fn search_ledger(
    conn: &Connection,
    query: &str,
    category: Option<&str>,
    days: Option<u64>,
    breaking_only: bool,
    limit: Option<usize>,
    offset: usize,
    include_rollback: bool,
) -> Result<Vec<crate::ledger::types::LedgerEntry>, LedgerError> {
    let mut fts = fts_where(query, category, days, breaking_only);
    if !include_rollback {
        fts.clause.push_str(" AND l.entry_type != 'ROLLBACK'");
    }

    let mut sql = format!("{SEARCH_SELECT}{}", fts.clause);

    if include_rollback {
        sql.push_str(
            " ORDER BY CASE WHEN l.entry_type = 'ROLLBACK' THEN 1 ELSE 0 END, f.rank, l.committed_at DESC",
        );
    } else {
        sql.push_str(" ORDER BY f.rank, l.committed_at DESC");
    }

    if let Some(lim) = limit {
        sql.push_str(&format!(
            " LIMIT ?{} OFFSET ?{}",
            fts.next_param_idx,
            fts.next_param_idx + 1
        ));
        fts.params.push(Box::new(lim as i64));
        fts.params.push(Box::new(offset as i64));
    }

    let mut stmt = conn.prepare(&sql).map_err(map_fts_prepare_error)?;

    let rows = stmt.query_map(rusqlite::params_from_iter(fts.params), |row| {
        super::map_ledger_entry(row)
    })?;

    let mut entries = Vec::new();
    for entry in rows {
        match entry {
            Ok(e) => entries.push(e),
            Err(e) => return Err(map_fts_row_error(e)),
        }
    }
    Ok(entries)
}

/// Count FTS matches whose `entry_type` is `ROLLBACK` (same MATCH/filters as search).
///
/// Fail-closed: never maps query errors to `0`.
pub fn count_rollback_matches(
    conn: &Connection,
    query: &str,
    category: Option<&str>,
    days: Option<u64>,
    breaking_only: bool,
) -> Result<usize, LedgerError> {
    let mut fts = fts_where(query, category, days, breaking_only);
    fts.clause.push_str(" AND l.entry_type = 'ROLLBACK'");

    let sql = format!(
        "SELECT COUNT(*) FROM ledger_entries l JOIN ledger_fts f ON f.rowid = l.id {}",
        fts.clause
    );

    let mut stmt = conn.prepare(&sql).map_err(map_fts_prepare_error)?;
    let count: i64 = stmt
        .query_row(rusqlite::params_from_iter(fts.params), |row| row.get(0))
        .map_err(map_fts_row_error)?;
    usize::try_from(count)
        .map_err(|_| LedgerError::Validation(format!("invalid rollback match count: {count}")))
}
