use crate::ledger::error::LedgerError;
use rusqlite::Connection;

pub fn get_stale_pending_transactions(
    conn: &Connection,
    ttl_days: u64,
) -> Result<Vec<String>, LedgerError> {
    let threshold = (chrono::Utc::now() - chrono::Duration::days(ttl_days as i64)).to_rfc3339();
    let mut stmt = conn
        .prepare("SELECT tx_id FROM transactions WHERE status = 'PENDING' AND started_at < ?1")?;
    let ids = stmt
        .query_map([threshold], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(ids)
}

pub fn delete_stale_pending_transactions(
    conn: &Connection,
    ttl_days: u64,
) -> Result<usize, LedgerError> {
    let threshold = (chrono::Utc::now() - chrono::Duration::days(ttl_days as i64)).to_rfc3339();
    let count = conn.execute(
        "DELETE FROM transactions WHERE status = 'PENDING' AND started_at < ?1",
        rusqlite::params![threshold],
    )?;
    Ok(count)
}

pub fn get_transaction_velocity(conn: &Connection, days: u64) -> Result<usize, LedgerError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ledger_entries WHERE committed_at >= strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?1)",
        [format!("-{} days", days)],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// Strip `canonical_entity`'s ` (+N more)` suffix (space before `(`).
fn strip_plus_n_more(s: &str) -> &str {
    if let Some(i) = s.rfind(" (+") {
        let rest = &s[i + 3..];
        if let Some(digits) = rest.strip_suffix(" more)")
            && !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit())
        {
            return &s[..i];
        }
    }
    s
}

fn normalize_churn_key(s: &str) -> String {
    s.replace('\\', "/")
}

const EXTENSIONLESS_CHURN_FILES: &[&str] = &[
    "LICENSE",
    "Makefile",
    "Dockerfile",
    "NOTICE",
    "COPYING",
    "AUTHORS",
    "Gemfile",
    "Rakefile",
    "CODEOWNERS",
    "THIRD_PARTY_NOTICES",
];

const CHURN_DIR_DENYLIST: &[&str] = &[".git", ".github", ".ledgerful"];

/// True when `s` is a file path for TOP CHURNED FILES (not a dir, slug, or UUID).
fn is_churn_file_path(s: &str) -> bool {
    let stripped = strip_plus_n_more(s);
    if stripped.is_empty() || stripped.contains("drift_adoption:") {
        return false;
    }
    if uuid::Uuid::parse_str(stripped).is_ok() {
        return false;
    }
    let key = normalize_churn_key(stripped);
    let last = key.rsplit('/').next().unwrap_or("");
    if last.is_empty() {
        return false;
    }
    if CHURN_DIR_DENYLIST.contains(&last) {
        return false;
    }
    if EXTENSIONLESS_CHURN_FILES.contains(&last) {
        return true;
    }
    if last.starts_with('.') {
        return last.len() > 1
            && last
                .chars()
                .nth(1)
                .is_some_and(|c| c.is_ascii_alphanumeric());
    }
    if let Some(dot) = last.rfind('.')
        && dot > 0
    {
        let ext = &last[dot + 1..];
        return !ext.is_empty() && ext.len() <= 8 && ext.bytes().all(|b| b.is_ascii_alphanumeric());
    }
    false
}

fn credit_churn_path(path: &str) -> Option<String> {
    if !is_churn_file_path(path) {
        return None;
    }
    Some(normalize_churn_key(strip_plus_n_more(path)))
}

pub fn get_top_churned_entities(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<(String, usize)>, LedgerError> {
    let mut stmt = conn.prepare(
        "SELECT e.tx_id, e.entity, cf.path
         FROM ledger_entries e
         LEFT JOIN transactions t ON t.tx_id = e.tx_id
         LEFT JOIN changed_files cf ON cf.snapshot_id = t.snapshot_id
         WHERE e.origin = 'LOCAL'",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;

    let mut per_tx: std::collections::BTreeMap<
        String,
        (String, std::collections::BTreeSet<String>),
    > = std::collections::BTreeMap::new();
    for res in rows {
        let (tx_id, entity, path) = res?;
        let entry = per_tx
            .entry(tx_id)
            .or_insert_with(|| (entity, std::collections::BTreeSet::new()));
        if let Some(p) = path
            && let Some(key) = credit_churn_path(&p)
        {
            entry.1.insert(key);
        }
    }

    let mut counts: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for (tx_id, (entity, files)) in per_tx {
        if files.is_empty() {
            if let Some(key) = credit_churn_path(&entity) {
                counts.entry(key).or_default().insert(tx_id);
            }
        } else {
            for key in files {
                counts.entry(key).or_default().insert(tx_id.clone());
            }
        }
    }

    let mut ranked: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(path, txs)| (path, txs.len()))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    if ranked.len() > limit {
        ranked.truncate(limit);
    }
    Ok(ranked)
}

pub fn get_recent_ledger_entries_paginated(
    conn: &Connection,
    limit: usize,
    offset: usize,
) -> Result<Vec<crate::ledger::types::LedgerEntry>, LedgerError> {
    let mut stmt = conn.prepare(
        "SELECT id, tx_id, category, entry_type, entity, entity_normalized,
            change_type, summary, reason, is_breaking, committed_at,
            verification_status, verification_basis, outcome_notes,
            origin, trace_id, signature, public_key, risk, related_tickets, author, observed, prev_hash, sig_version
     FROM ledger_entries
     ORDER BY committed_at DESC
     LIMIT ?1 OFFSET ?2",
    )?;

    let rows = stmt.query_map(rusqlite::params![limit as i64, offset as i64], |row| {
        super::map_ledger_entry(row)
    })?;

    let mut entries = Vec::new();
    for entry in rows {
        entries.push(entry?);
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::types::{Category, ChangeType, EntryType, LedgerEntry, Transaction};
    use rusqlite::Connection;

    fn setup_in_memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE transactions (
                tx_id TEXT PRIMARY KEY,
                operation_id TEXT,
                status TEXT NOT NULL,
                category TEXT NOT NULL,
                entity TEXT NOT NULL,
                entity_normalized TEXT NOT NULL,
                planned_action TEXT,
                session_id TEXT NOT NULL,
                source TEXT NOT NULL DEFAULT 'CLI',
                started_at TEXT NOT NULL,
                resolved_at TEXT,
                detected_at TEXT,
                drift_count INTEGER DEFAULT 1,
                first_seen_at TEXT,
                last_seen_at TEXT,
                issue_ref TEXT,
                snapshot_id INTEGER
            );
            CREATE TABLE snapshots (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                head_hash TEXT,
                branch_name TEXT,
                is_clean INTEGER NOT NULL,
                packet_json TEXT NOT NULL
            );
            CREATE TABLE changed_files (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                snapshot_id INTEGER,
                path TEXT NOT NULL,
                status TEXT NOT NULL,
                is_staged INTEGER NOT NULL
            );
            CREATE TABLE ledger_entries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                tx_id TEXT NOT NULL,
                category TEXT NOT NULL,
                entry_type TEXT NOT NULL DEFAULT 'IMPLEMENTATION',
                entity TEXT NOT NULL,
                entity_normalized TEXT NOT NULL,
                change_type TEXT NOT NULL,
                summary TEXT NOT NULL,
                reason TEXT NOT NULL,
                is_breaking INTEGER DEFAULT 0,
                committed_at TEXT NOT NULL,
                verification_status TEXT,
                verification_basis TEXT,
                outcome_notes TEXT,
                origin TEXT NOT NULL DEFAULT 'LOCAL',
                trace_id TEXT,
                signature TEXT,
                public_key TEXT,
                risk TEXT,
                related_tickets TEXT,
                author TEXT NOT NULL DEFAULT 'unknown',
                observed INTEGER,
                prev_hash TEXT,
                sig_version INTEGER NOT NULL DEFAULT 1
            );",
        )
        .unwrap();
        // sig_version already in CREATE above (m53)
        conn
    }

    fn sample_tx(entity: &str, status: &str) -> Transaction {
        Transaction {
            tx_id: uuid::Uuid::new_v4().to_string(),
            operation_id: None,
            status: status.to_string(),
            category: Category::Feature,
            entity: entity.to_string(),
            entity_normalized: entity.to_string(),
            planned_action: None,
            session_id: "test".to_string(),
            source: "CLI".to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            resolved_at: None,
            detected_at: None,
            drift_count: 1,
            first_seen_at: None,
            last_seen_at: None,
            issue_ref: None,
            snapshot_id: None,
        }
    }

    fn sample_entry(tx_id: &str, entity: &str) -> LedgerEntry {
        LedgerEntry {
            id: 0,
            tx_id: tx_id.to_string(),
            category: Category::Feature,
            entry_type: EntryType::Implementation,
            entity: entity.to_string(),
            entity_normalized: entity.to_string(),
            change_type: ChangeType::Modify,
            summary: "test".to_string(),
            reason: "reason".to_string(),
            is_breaking: false,
            committed_at: chrono::Utc::now().to_rfc3339(),
            verification_status: None,
            verification_basis: None,
            outcome_notes: None,
            origin: "LOCAL".to_string(),
            trace_id: None,
            signature: None,
            public_key: None,
            risk: None,
            related_tickets: None,
            author: "Test User".to_string(),
            observed: None,
            prev_hash: None,
            sig_version: 1,
        }
    }

    #[test]
    fn test_transaction_velocity() {
        let conn = setup_in_memory_db();
        let tx = sample_tx("a.rs", "PENDING");
        crate::ledger::db::transactions::insert_transaction(&conn, &tx).unwrap();
        crate::ledger::db::transactions::insert_ledger_entry(
            &conn,
            &sample_entry(&tx.tx_id, "a.rs"),
        )
        .unwrap();

        let v = get_transaction_velocity(&conn, 7).unwrap();
        assert_eq!(v, 1);
    }

    fn insert_entry(conn: &Connection, tx: &Transaction, entity: &str, origin: &str) {
        crate::ledger::db::transactions::insert_transaction(conn, tx).unwrap();
        let mut entry = sample_entry(&tx.tx_id, entity);
        entry.origin = origin.to_string();
        crate::ledger::db::transactions::insert_ledger_entry(conn, &entry).unwrap();
    }

    fn insert_changed_file(conn: &Connection, snapshot_id: i64, path: &str) {
        conn.execute(
            "INSERT INTO changed_files (snapshot_id, path, status, is_staged) VALUES (?1, ?2, 'MODIFIED', 1)",
            rusqlite::params![snapshot_id, path],
        )
        .unwrap();
    }

    #[test]
    fn test_top_churned_entities() {
        let conn = setup_in_memory_db();
        let tx1 = sample_tx("a.rs", "PENDING");
        let tx3 = sample_tx("a.rs", "PENDING");
        let tx2 = sample_tx("b.rs", "PENDING");
        insert_entry(&conn, &tx1, "a.rs", "LOCAL");
        insert_entry(&conn, &tx3, "a.rs", "LOCAL");
        insert_entry(&conn, &tx2, "b.rs", "LOCAL");

        let churn = get_top_churned_entities(&conn, 10).unwrap();
        assert_eq!(churn.len(), 2);
        assert_eq!(churn[0], ("a.rs".to_string(), 2));
        assert_eq!(churn[1], ("b.rs".to_string(), 1));
    }

    #[test]
    fn test_top_churned_entities_empty_db() {
        let conn = setup_in_memory_db();
        let churn = get_top_churned_entities(&conn, 10).unwrap();
        assert_eq!(churn, Vec::<(String, usize)>::new());
    }

    #[test]
    fn test_top_churned_entities_plus_n_more_collapses() {
        let conn = setup_in_memory_db();
        for entity in [
            "CHANGELOG.md",
            "CHANGELOG.md (+1 more)",
            "CHANGELOG.md (+2 more)",
            "CHANGELOG.md (+3 more)",
        ] {
            insert_entry(&conn, &sample_tx(entity, "PENDING"), entity, "LOCAL");
        }
        let churn = get_top_churned_entities(&conn, 10).unwrap();
        assert_eq!(churn, vec![("CHANGELOG.md".to_string(), 4)]);
    }

    #[test]
    fn test_top_churned_entities_collapse_before_limit() {
        let conn = setup_in_memory_db();
        insert_entry(&conn, &sample_tx("z.rs", "PENDING"), "z.rs", "LOCAL");
        insert_entry(
            &conn,
            &sample_tx("CHANGELOG.md (+1 more)", "PENDING"),
            "CHANGELOG.md (+1 more)",
            "LOCAL",
        );
        insert_entry(
            &conn,
            &sample_tx("CHANGELOG.md (+2 more)", "PENDING"),
            "CHANGELOG.md (+2 more)",
            "LOCAL",
        );
        insert_entry(
            &conn,
            &sample_tx("CHANGELOG.md", "PENDING"),
            "CHANGELOG.md",
            "LOCAL",
        );
        let churn = get_top_churned_entities(&conn, 2).unwrap();
        assert_eq!(churn.len(), 2);
        assert!(
            churn.iter().any(|(p, c)| p == "CHANGELOG.md" && *c == 3),
            "canonical CHANGELOG.md must survive LIMIT after collapse: {churn:?}"
        );
    }

    #[test]
    fn test_top_churned_entities_drops_dirs_and_slugs() {
        let conn = setup_in_memory_db();
        for entity in [
            "src",
            ".github/workflows",
            "track-0037-diff-stats",
            "unknown",
            "0135-verify-fast-scope-honesty",
        ] {
            insert_entry(&conn, &sample_tx(entity, "PENDING"), entity, "LOCAL");
        }
        insert_entry(&conn, &sample_tx("keep.rs", "PENDING"), "keep.rs", "LOCAL");
        let churn = get_top_churned_entities(&conn, 10).unwrap();
        assert_eq!(churn, vec![("keep.rs".to_string(), 1)]);
    }

    #[test]
    fn test_top_churned_entities_expands_dir_via_changed_files() {
        let conn = setup_in_memory_db();
        let mut tx = sample_tx("src", "PENDING");
        tx.snapshot_id = Some(1);
        insert_entry(&conn, &tx, "src", "LOCAL");
        insert_changed_file(&conn, 1, "src/a.rs");
        insert_changed_file(&conn, 1, "src/b.rs");
        let churn = get_top_churned_entities(&conn, 10).unwrap();
        assert_eq!(
            churn,
            vec![("src/a.rs".to_string(), 1), ("src/b.rs".to_string(), 1)]
        );
    }

    #[test]
    fn test_top_churned_entities_one_tx_three_files() {
        let conn = setup_in_memory_db();
        let mut tx = sample_tx("src", "PENDING");
        tx.snapshot_id = Some(7);
        insert_entry(&conn, &tx, "src", "LOCAL");
        insert_changed_file(&conn, 7, "src/one.rs");
        insert_changed_file(&conn, 7, "src/two.rs");
        insert_changed_file(&conn, 7, "src/three.rs");
        let churn = get_top_churned_entities(&conn, 10).unwrap();
        assert_eq!(churn.len(), 3);
        for (_, count) in &churn {
            assert_eq!(*count, 1);
        }
        assert!(!churn.iter().any(|(p, _)| p == "src"));
    }

    #[test]
    fn test_top_churned_entities_skips_federated() {
        let conn = setup_in_memory_db();
        insert_entry(
            &conn,
            &sample_tx("fed.rs", "PENDING"),
            "fed.rs",
            "FEDERATED",
        );
        insert_entry(
            &conn,
            &sample_tx("local.rs", "PENDING"),
            "local.rs",
            "LOCAL",
        );
        let churn = get_top_churned_entities(&conn, 10).unwrap();
        assert_eq!(churn, vec![("local.rs".to_string(), 1)]);
    }

    #[test]
    fn test_top_churned_entities_backslash_slash_collapse() {
        let conn = setup_in_memory_db();
        insert_entry(
            &conn,
            &sample_tx(r"src\cli\args.rs", "PENDING"),
            r"src\cli\args.rs",
            "LOCAL",
        );
        insert_entry(
            &conn,
            &sample_tx("src/cli/args.rs", "PENDING"),
            "src/cli/args.rs",
            "LOCAL",
        );
        let churn = get_top_churned_entities(&conn, 10).unwrap();
        assert_eq!(churn, vec![("src/cli/args.rs".to_string(), 2)]);
    }

    #[test]
    fn test_top_churned_entities_tie_break_path_asc() {
        let conn = setup_in_memory_db();
        insert_entry(&conn, &sample_tx("b.rs", "PENDING"), "b.rs", "LOCAL");
        insert_entry(&conn, &sample_tx("a.rs", "PENDING"), "a.rs", "LOCAL");
        let churn = get_top_churned_entities(&conn, 10).unwrap();
        assert_eq!(
            churn,
            vec![("a.rs".to_string(), 1), ("b.rs".to_string(), 1)]
        );
    }

    #[test]
    fn test_top_churned_entities_limit_after_collapse_fills() {
        let conn = setup_in_memory_db();
        for entity in [
            "CHANGELOG.md",
            "CHANGELOG.md (+1 more)",
            "a.rs",
            "b.rs",
            "c.rs",
        ] {
            insert_entry(&conn, &sample_tx(entity, "PENDING"), entity, "LOCAL");
        }
        let churn = get_top_churned_entities(&conn, 3).unwrap();
        assert_eq!(churn.len(), 3);
        assert!(
            churn.iter().any(|(p, c)| p == "CHANGELOG.md" && *c == 2),
            "collapsed CHANGELOG must occupy a slot: {churn:?}"
        );
    }

    #[test]
    fn test_stale_pending_ttl() {
        let conn = setup_in_memory_db();
        let old = Transaction {
            tx_id: uuid::Uuid::new_v4().to_string(),
            operation_id: None,
            status: "PENDING".to_string(),
            category: Category::Feature,
            entity: "old.rs".to_string(),
            entity_normalized: "old.rs".to_string(),
            planned_action: None,
            session_id: "test".to_string(),
            source: "CLI".to_string(),
            started_at: (chrono::Utc::now() - chrono::Duration::days(10)).to_rfc3339(),
            resolved_at: None,
            detected_at: None,
            drift_count: 1,
            first_seen_at: None,
            last_seen_at: None,
            issue_ref: None,
            snapshot_id: None,
        };
        let new = sample_tx("new.rs", "PENDING");
        crate::ledger::db::transactions::insert_transaction(&conn, &old).unwrap();
        crate::ledger::db::transactions::insert_transaction(&conn, &new).unwrap();

        let stale = get_stale_pending_transactions(&conn, 7).unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0], old.tx_id);
    }
}
