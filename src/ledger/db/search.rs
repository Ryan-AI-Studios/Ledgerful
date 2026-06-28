use crate::ledger::error::LedgerError;
use rusqlite::Connection;

pub fn search_ledger(
    conn: &Connection,
    query: &str,
    category: Option<&str>,
    days: Option<u64>,
    breaking_only: bool,
    limit: Option<usize>,
    offset: usize,
) -> Result<Vec<crate::ledger::types::LedgerEntry>, LedgerError> {
    let mut sql = "SELECT l.id, l.tx_id, l.category, l.entry_type, l.entity, l.entity_normalized,
            l.change_type, l.summary, l.reason, l.is_breaking, l.committed_at,
            l.verification_status, l.verification_basis, l.outcome_notes,
            l.origin, l.trace_id, l.signature, l.public_key, l.risk, l.related_tickets, l.author
     FROM ledger_entries l
     JOIN ledger_fts f ON f.rowid = l.id
     WHERE ledger_fts MATCH ?1"
        .to_string();

    // FTS5 interprets hyphens as column qualifiers (e.g. "dead-code" →
    // column "code"), so wrap the query in double quotes to treat it as a
    // phrase search. This prevents "no such column" errors on hyphenated
    // search terms.
    let fts_query = format!("\"{query}\"");
    let mut param_idx = 2u32;
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(fts_query)];

    if let Some(cat) = category {
        sql.push_str(&format!(" AND l.category = ?{param_idx}"));
        params.push(Box::new(cat.to_string()));
        param_idx += 1;
    }

    if let Some(d) = days {
        sql.push_str(&format!(
            " AND l.committed_at >= strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?{param_idx})"
        ));
        params.push(Box::new(format!("-{d} days")));
        param_idx += 1;
    }

    if breaking_only {
        sql.push_str(" AND l.is_breaking = 1");
    }

    sql.push_str(" ORDER BY f.rank, l.committed_at DESC");

    if let Some(lim) = limit {
        sql.push_str(&format!(" LIMIT ?{param_idx} OFFSET ?{}", param_idx + 1));
        params.push(Box::new(lim as i64));
        params.push(Box::new(offset as i64));
    }

    let mut stmt = conn.prepare(&sql).map_err(|e| {
        if let rusqlite::Error::SqliteFailure(_err, Some(msg)) = &e
            && msg.contains("syntax error")
        {
            return LedgerError::Validation(format!("Invalid search query: {}", msg));
        }
        LedgerError::from(e)
    })?;

    let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
        super::map_ledger_entry(row)
    })?;

    let mut entries = Vec::new();
    for entry in rows {
        match entry {
            Ok(e) => entries.push(e),
            Err(e) => {
                if let rusqlite::Error::SqliteFailure(_err, Some(msg)) = &e
                    && msg.contains("syntax error")
                {
                    return Err(LedgerError::Validation(format!(
                        "Invalid search query syntax: {}",
                        msg
                    )));
                }
                return Err(LedgerError::from(e));
            }
        }
    }
    Ok(entries)
}
