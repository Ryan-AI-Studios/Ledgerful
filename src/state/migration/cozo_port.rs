use crate::state::storage::StorageManager;
use crate::state::storage_cozo::CozoStorage;
use miette::{IntoDiagnostic, Result};
use serde_json::json;
use tracing::info;

pub fn migrate_sqlite_to_cozo(sqlite: &StorageManager, cozo: &CozoStorage) -> Result<()> {
    info!("Starting migration from SQLite to CozoDB...");

    // 1. Migrate Ledger Entries
    migrate_ledger_entries(sqlite, cozo)?;

    // 2. Migrate Project Symbols
    migrate_project_symbols(sqlite, cozo)?;

    info!("Migration completed successfully.");
    Ok(())
}

fn migrate_ledger_entries(sqlite: &StorageManager, cozo: &CozoStorage) -> Result<()> {
    let conn = sqlite.get_connection();
    let mut stmt = conn.prepare(
        "SELECT id, tx_id, category, entry_type, entity_normalized, change_type, summary, reason, committed_at, is_breaking, verification_status, trace_id, signature, public_key, risk, related_tickets 
         FROM ledger_entries"
    ).into_diagnostic()?;

    let rows = stmt
        .query_map([], |row| {
            Ok(json!([
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, i32>(9)? != 0,
                row.get::<_, Option<String>>(10)?.unwrap_or_default(),
                row.get::<_, Option<String>>(11)?.unwrap_or_default(),
                row.get::<_, Option<String>>(12)?.unwrap_or_default(),
                row.get::<_, Option<String>>(13)?.unwrap_or_default(),
                row.get::<_, Option<String>>(14)?.unwrap_or_default(),
                row.get::<_, Option<String>>(15)?.unwrap_or_default(),
            ]))
        })
        .into_diagnostic()?;

    let mut batch = Vec::new();
    for row in rows {
        batch.push(row.into_diagnostic()?);
    }

    if !batch.is_empty() {
        let script = format!(
            "?[id, tx_id, category, entry_type, entity_normalized, change_type, summary, reason, committed_at, is_breaking, verification_status, trace_id, signature, public_key, risk, related_tickets] <- {} :put ledger_entry",
            serde_json::to_string(&batch).into_diagnostic()?
        );
        cozo.run_script(&script)?;
        info!("Migrated {} ledger entries.", batch.len());
    }

    Ok(())
}

fn migrate_project_symbols(sqlite: &StorageManager, cozo: &CozoStorage) -> Result<()> {
    let conn = sqlite.get_connection();
    let mut stmt = conn.prepare(
        "SELECT ps.id, pf.file_path, ps.qualified_name, ps.symbol_name, ps.symbol_kind, ps.is_public, ps.line_start, ps.line_end 
         FROM project_symbols ps
         JOIN project_files pf ON ps.file_id = pf.id"
    ).into_diagnostic()?;

    let rows = stmt
        .query_map([], |row| {
            Ok(json!([
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i32>(5)? != 0,
                row.get::<_, Option<i64>>(6)?.unwrap_or(0),
                row.get::<_, Option<i64>>(7)?.unwrap_or(0),
            ]))
        })
        .into_diagnostic()?;

    let mut batch = Vec::new();
    for row in rows {
        batch.push(row.into_diagnostic()?);
    }

    if !batch.is_empty() {
        let script = format!(
            "?[id, file_path, qualified_name, symbol_name, symbol_kind, is_public, line_start, line_end] <- {} :put project_symbol",
            serde_json::to_string(&batch).into_diagnostic()?
        );
        cozo.run_script(&script)?;
        info!("Migrated {} project symbols.", batch.len());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use crate::state::storage::StorageManager;
    use crate::state::storage_cozo::CozoStorage;
    use rusqlite::Connection;
    use std::path::PathBuf;

    fn setup_sqlite() -> StorageManager {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    #[test]
    fn test_migration_parity() {
        let sqlite = setup_sqlite();
        let cozo = CozoStorage::new(&PathBuf::from("")).unwrap();

        // 1. Insert into SQLite
        let tx_id = "tx_123";
        sqlite.get_connection().execute(
            "INSERT INTO transactions (tx_id, status, category, entity, entity_normalized, session_id, started_at) 
             VALUES (?1, 'COMMITTED', 'FEAT', 'file.rs', 'file.rs', 'sess', 'now')",
            [tx_id]
        ).unwrap();

        sqlite.get_connection().execute(
            "INSERT INTO ledger_entries (tx_id, category, entity, entity_normalized, change_type, summary, reason, committed_at)
             VALUES (?1, 'FEAT', 'file.rs', 'file.rs', 'ADD', 'desc', 'why', 'now')",
            [tx_id]
        ).unwrap();

        // 2. Run Migration
        migrate_sqlite_to_cozo(&sqlite, &cozo).unwrap();

        // 3. Verify in Cozo
        let res = cozo
            .run_script("?[summary] := *ledger_entry{tx_id: 'tx_123', summary: summary}")
            .unwrap();
        assert_eq!(res.rows.len(), 1);
        if let cozo::DataValue::Str(s) = &res.rows[0][0] {
            assert_eq!(s.as_str(), "desc");
        } else {
            panic!("Expected String value");
        }
    }
}
