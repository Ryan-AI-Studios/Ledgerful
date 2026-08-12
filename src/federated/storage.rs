use crate::federated::links::{LinkClass, canonical_link_key, classify_link, path_basename};
use miette::{IntoDiagnostic, Result};
use rusqlite::Connection;

pub fn update_federated_link(
    conn: &Connection,
    sibling_name: &str,
    sibling_path: &str,
    timestamp: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO federated_links (sibling_name, sibling_path, last_scanned_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(sibling_name) DO UPDATE SET
            sibling_path = excluded.sibling_path,
            last_scanned_at = excluded.last_scanned_at",
        (sibling_name, sibling_path, timestamp),
    )
    .into_diagnostic()?;
    Ok(())
}

pub fn save_federated_dependencies(
    conn: &Connection,
    sibling_name: &str,
    local_symbol: &str,
    sibling_symbol: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO federated_dependencies (local_symbol, sibling_name, sibling_symbol)
         VALUES (?1, ?2, ?3)",
        (local_symbol, sibling_name, sibling_symbol),
    )
    .into_diagnostic()?;
    Ok(())
}

pub fn clear_federated_dependencies(conn: &Connection, sibling_name: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM federated_dependencies WHERE sibling_name = ?1",
        [sibling_name],
    )
    .into_diagnostic()?;
    Ok(())
}

/// Delete a federated link after clearing deps and SIBLING ledger rows for that name.
/// FK on `federated_dependencies.sibling_name` has no CASCADE.
pub fn delete_federated_link(conn: &Connection, sibling_name: &str) -> Result<()> {
    clear_federated_dependencies(conn, sibling_name)?;
    delete_sibling_ledger_by_trace(conn, sibling_name)?;
    conn.execute(
        "DELETE FROM federated_links WHERE sibling_name = ?1",
        [sibling_name],
    )
    .into_diagnostic()?;
    Ok(())
}

/// Drop imported SIBLING ledger rows keyed by `trace_id = sibling_name`.
pub fn delete_sibling_ledger_by_trace(conn: &Connection, sibling_name: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM ledger_entries WHERE origin = 'SIBLING' AND trace_id = ?1",
        [sibling_name],
    )
    .into_diagnostic()?;
    Ok(())
}

/// Migrate SIBLING `ledger_entries.trace_id` old → new (0184-A).
///
/// If a row already exists under the new name for the same `tx_id`, the
/// old-name duplicate is deleted (keep the already-new row).
pub fn migrate_sibling_trace_id(conn: &Connection, old_name: &str, new_name: &str) -> Result<()> {
    if old_name == new_name {
        return Ok(());
    }
    // Drop old rows that would UNIQUE/dup collide on (tx_id, new).
    conn.execute(
        "DELETE FROM ledger_entries
         WHERE origin = 'SIBLING' AND trace_id = ?1
           AND tx_id IN (
             SELECT tx_id FROM ledger_entries
             WHERE origin = 'SIBLING' AND trace_id = ?2
           )",
        [old_name, new_name],
    )
    .into_diagnostic()?;
    conn.execute(
        "UPDATE ledger_entries SET trace_id = ?1
         WHERE origin = 'SIBLING' AND trace_id = ?2",
        [new_name, old_name],
    )
    .into_diagnostic()?;
    Ok(())
}

/// Upsert a live peer by canonical path; store name = folder basename.
///
/// Same-path rows under a different name are renamed (deps cleared, SIBLING
/// `trace_id` migrated, old link deleted) before the basename upsert.
pub fn upsert_federated_link_by_path(
    conn: &Connection,
    sibling_path: &str,
    timestamp: &str,
) -> Result<String> {
    let basename = path_basename(sibling_path);
    let path_key = canonical_link_key(sibling_path);
    let existing = get_federated_links(conn)?;

    for (old_name, old_path, _) in &existing {
        if old_name == &basename {
            continue;
        }
        if canonical_link_key(old_path) != path_key {
            continue;
        }
        // Same path, different name → migrate identity then drop old link row.
        clear_federated_dependencies(conn, old_name)?;
        migrate_sibling_trace_id(conn, old_name, &basename)?;
        conn.execute(
            "DELETE FROM federated_links WHERE sibling_name = ?1",
            [old_name.as_str()],
        )
        .into_diagnostic()?;
    }

    update_federated_link(conn, &basename, sibling_path, timestamp)?;
    Ok(basename)
}

/// After scan/refresh discovery, prune cache rows classified Dead or Self only.
/// Does **not** delete Live peers merely absent from this scan.
pub fn prune_dead_and_self_links(conn: &Connection, repo_root: &str) -> Result<usize> {
    let existing = get_federated_links(conn)?;
    let mut pruned = 0usize;
    for (name, path, _) in existing {
        match classify_link(&path, repo_root) {
            LinkClass::Dead | LinkClass::Self_ => {
                delete_federated_link(conn, &name)?;
                pruned += 1;
            }
            LinkClass::Live => {}
        }
    }
    Ok(pruned)
}

pub fn get_federated_links(conn: &Connection) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn
        .prepare("SELECT sibling_name, sibling_path, last_scanned_at FROM federated_links ORDER BY sibling_name")
        .into_diagnostic()?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .into_diagnostic()?;

    let mut links = Vec::new();
    for link in rows {
        links.push(link.into_diagnostic()?);
    }
    Ok(links)
}

pub fn get_dependencies_for_sibling(
    conn: &Connection,
    sibling_name: &str,
) -> Result<Vec<(String, String)>> {
    let mut stmt = conn
        .prepare("SELECT local_symbol, sibling_symbol FROM federated_dependencies WHERE sibling_name = ?1 ORDER BY local_symbol, sibling_symbol")
        .into_diagnostic()?;
    let rows = stmt
        .query_map([sibling_name], |row| Ok((row.get(0)?, row.get(1)?)))
        .into_diagnostic()?;

    let mut deps = Vec::new();
    for dep in rows {
        deps.push(dep.into_diagnostic()?);
    }
    Ok(deps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;
    use std::fs;
    use tempfile::tempdir;

    fn open_migrated() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn
    }

    fn write_schema(dir: &std::path::Path) {
        let state = dir.join(".ledgerful").join("state");
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("schema.json"), r#"{}"#).unwrap();
    }

    #[test]
    fn delete_clears_deps_then_link() {
        let conn = open_migrated();
        update_federated_link(&conn, "peer", r"C:\dev\peer", "t1").unwrap();
        save_federated_dependencies(&conn, "peer", "local", "sib").unwrap();
        delete_federated_link(&conn, "peer").unwrap();
        assert!(get_federated_links(&conn).unwrap().is_empty());
        assert!(
            get_dependencies_for_sibling(&conn, "peer")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn after_rename_migrate_impact_query_finds_entries_under_new_basename() {
        use crate::ledger::db::LedgerDb;

        let peer = tempdir().unwrap();
        write_schema(peer.path());
        let path = peer.path().to_str().unwrap();
        let conn = open_migrated();

        update_federated_link(&conn, "oldname", path, "2026-01-01T00:00:00Z").unwrap();
        conn.execute(
            "INSERT INTO transactions (
                tx_id, status, category, entity, entity_normalized, session_id, source, started_at, resolved_at
             ) VALUES ('tx-impact', 'COMMITTED', 'FEATURE', 'src/lib.rs', 'src/lib.rs', 'FEDERATED', 'FEDERATED', 't', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ledger_entries (
                tx_id, category, entry_type, entity, entity_normalized,
                change_type, summary, reason, is_breaking, committed_at,
                origin, trace_id, author
             ) VALUES ('tx-impact', 'FEATURE', 'IMPLEMENTATION', 'src/lib.rs', 'src/lib.rs', 'MODIFY',
                       'sibling change', 'r', 0, '2026-08-10T00:00:00Z', 'SIBLING', 'oldname', 'unknown')",
            [],
        )
        .unwrap();

        let basename = upsert_federated_link_by_path(&conn, path, "2026-08-12T00:00:00Z").unwrap();
        let db = LedgerDb::new(&conn);
        let under_new = db
            .get_federated_entries_by_entity("src/lib.rs", &basename, 30)
            .unwrap();
        let under_old = db
            .get_federated_entries_by_entity("src/lib.rs", "oldname", 30)
            .unwrap();
        assert_eq!(
            under_new.len(),
            1,
            "impact must find SIBLING history under new basename"
        );
        assert_eq!(under_new[0].summary, "sibling change");
        assert!(
            under_old.is_empty(),
            "old trace_id must not retain migrated rows"
        );
    }

    #[test]
    fn upsert_renames_same_path_and_migrates_trace_id() {
        let peer = tempdir().unwrap();
        write_schema(peer.path());
        let root = tempdir().unwrap();
        let path = peer.path().to_str().unwrap();
        let conn = open_migrated();

        update_federated_link(&conn, "oldname", path, "2026-01-01T00:00:00Z").unwrap();
        // Seed SIBLING ledger under old name
        conn.execute(
            "INSERT INTO transactions (
                tx_id, status, category, entity, entity_normalized, session_id, source, started_at, resolved_at
             ) VALUES ('tx-1', 'COMMITTED', 'FEATURE', 'e', 'e', 'FEDERATED', 'FEDERATED', 't', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ledger_entries (
                tx_id, category, entry_type, entity, entity_normalized,
                change_type, summary, reason, is_breaking, committed_at,
                origin, trace_id, author
             ) VALUES ('tx-1', 'FEATURE', 'COMMIT', 'e', 'e', 'MODIFIED', 's', 'r', 0,
                       '2026-08-01T00:00:00Z', 'SIBLING', 'oldname', 'unknown')",
            [],
        )
        .unwrap();

        let basename = upsert_federated_link_by_path(&conn, path, "2026-08-12T00:00:00Z").unwrap();
        assert_eq!(basename, path_basename(path));
        let links = get_federated_links(&conn).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].0, basename);
        assert!(!links.iter().any(|(n, _, _)| n == "oldname"));

        let count_new: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ledger_entries WHERE origin='SIBLING' AND trace_id=?1",
                [basename.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        let count_old: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ledger_entries WHERE origin='SIBLING' AND trace_id='oldname'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count_new, 1);
        assert_eq!(count_old, 0);

        // prune should not remove live peer
        let pruned = prune_dead_and_self_links(&conn, root.path().to_str().unwrap()).unwrap();
        assert_eq!(pruned, 0);
        assert_eq!(get_federated_links(&conn).unwrap().len(), 1);
    }

    #[test]
    fn migrate_dedupes_when_new_trace_already_has_tx() {
        let conn = open_migrated();
        for (tx, trace) in [("tx-1", "old"), ("tx-1", "new"), ("tx-2", "old")] {
            conn.execute(
                "INSERT OR IGNORE INTO transactions (
                    tx_id, status, category, entity, entity_normalized, session_id, source, started_at, resolved_at
                 ) VALUES (?1, 'COMMITTED', 'FEATURE', 'e', 'e', 'FEDERATED', 'FEDERATED', 't', 't')",
                [tx],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO ledger_entries (
                    tx_id, category, entry_type, entity, entity_normalized,
                    change_type, summary, reason, is_breaking, committed_at,
                    origin, trace_id, author
                 ) VALUES (?1, 'FEATURE', 'COMMIT', 'e', 'e', 'MODIFIED', 's', 'r', 0,
                           '2026-08-01T00:00:00Z', 'SIBLING', ?2, 'unknown')",
                [tx, trace],
            )
            .unwrap();
        }
        migrate_sibling_trace_id(&conn, "old", "new").unwrap();
        let rows: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT tx_id, trace_id FROM ledger_entries WHERE origin='SIBLING' ORDER BY tx_id, trace_id",
                )
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(
            rows,
            vec![("tx-1".into(), "new".into()), ("tx-2".into(), "new".into()),]
        );
    }

    #[test]
    fn prune_removes_dead_and_self_only() {
        let root = tempdir().unwrap();
        write_schema(root.path());
        let live = tempdir().unwrap();
        write_schema(live.path());
        let husk = tempdir().unwrap();
        fs::write(husk.path().join("x.md"), "x").unwrap();

        let conn = open_migrated();
        update_federated_link(&conn, "selfrow", root.path().to_str().unwrap(), "t").unwrap();
        update_federated_link(&conn, "husk", husk.path().to_str().unwrap(), "t").unwrap();
        update_federated_link(&conn, "live", live.path().to_str().unwrap(), "t").unwrap();

        let pruned = prune_dead_and_self_links(&conn, root.path().to_str().unwrap()).unwrap();
        assert_eq!(pruned, 2);
        let links = get_federated_links(&conn).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].0, "live");
    }
}
