//! The ledger-entry COUNT pair (0468). Doctor `collect_lifecycle_findings`
//! and `execute_doctor --fix` call these queries. Defensive `Err` on a
//! missing table or column stays as shipped. This module does not build
//! doctor findings. `LedgerDb` still owns transaction CRUD.

use miette::{IntoDiagnostic, Result};

/// Count committed ledger entries marked Verified without a verification_results row.
pub(crate) fn count_phantom_verified(conn: &rusqlite::Connection) -> Result<i64> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ledger_entries le
             WHERE le.verification_status = 'verified'
               AND NOT EXISTS (
                   SELECT 1 FROM verification_results vr WHERE vr.tx_id = le.tx_id
               )",
            [],
            |row| row.get(0),
        )
        .into_diagnostic()?;
    Ok(count)
}

/// Count LOCAL committed ledger rows with `sig_version < below`.
///
/// Defensive: returns `Err` when the table/column is missing (fresh repos).
/// Callers should use `if let Ok(count) = …` and omit the count on error.
pub(crate) fn count_entries_below_sig_version(
    conn: &rusqlite::Connection,
    below: u32,
) -> Result<i64> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ledger_entries
             WHERE origin = 'LOCAL' AND sig_version < ?1",
            [below as i64],
            |row| row.get(0),
        )
        .into_diagnostic()?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::storage::connection::in_memory_storage;
    use rusqlite::Connection;

    fn seed_entry(
        conn: &Connection,
        tx_id: &str,
        verification_status: &str,
        origin: &str,
        sig_version: i64,
    ) {
        conn.execute(
            "INSERT INTO transactions (
                tx_id, status, category, entity, entity_normalized, session_id, source, started_at
             ) VALUES (?1, 'COMMITTED', 'FEATURE', 'a', 'a', 'test', 'test', '2020-01-01T00:00:00Z')",
            [tx_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ledger_entries (
                tx_id, category, entry_type, entity, entity_normalized, change_type,
                summary, reason, is_breaking, committed_at, origin, author, sig_version, signature,
                verification_status
             ) VALUES (?1, 'FEATURE', 'IMPLEMENTATION', 'a', 'a', 'MODIFY', 's', 'r', 0,
                '2020-01-01T00:00:00Z', ?2, 't', ?3, 'sig', ?4)",
            rusqlite::params![tx_id, origin, sig_version, verification_status],
        )
        .unwrap();
    }

    #[test]
    fn count_phantom_verified_counts_verified_row_without_results() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();
        seed_entry(conn, "tx-phantom", "verified", "LOCAL", 2);
        assert_eq!(count_phantom_verified(conn).unwrap(), 1);
    }

    #[test]
    fn count_phantom_verified_is_zero_when_results_row_binds_tx() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();
        seed_entry(conn, "tx-bound", "verified", "LOCAL", 2);
        conn.execute(
            "INSERT INTO verification_runs (timestamp, plan_json, overall_pass, tx_id)
             VALUES ('2020-01-01T00:00:00Z', '{}', 1, 'tx-bound')",
            [],
        )
        .unwrap();
        let run_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO verification_results (run_id, command, exit_code, duration_ms, truncated, tx_id)
             VALUES (?1, 'test', 0, 1, 0, 'tx-bound')",
            [run_id],
        )
        .unwrap();
        assert_eq!(count_phantom_verified(conn).unwrap(), 0);
    }

    #[test]
    fn count_phantom_verified_ignores_non_verified_status() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();
        seed_entry(conn, "tx-unverified", "unverified", "LOCAL", 2);
        assert_eq!(count_phantom_verified(conn).unwrap(), 0);
    }

    #[test]
    fn count_phantom_verified_errors_on_missing_table() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(count_phantom_verified(&conn).is_err());
    }

    #[test]
    fn count_entries_below_sig_version_counts_local_rows_below_threshold() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();
        seed_entry(conn, "tx-v1", "unverified", "LOCAL", 1);
        assert_eq!(count_entries_below_sig_version(conn, 2).unwrap(), 1);
    }

    #[test]
    fn count_entries_below_sig_version_skips_non_local_origin() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();
        seed_entry(conn, "tx-fed", "unverified", "FEDERATED", 1);
        assert_eq!(count_entries_below_sig_version(conn, 2).unwrap(), 0);
    }

    #[test]
    fn count_entries_below_sig_version_skips_row_at_threshold() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();
        seed_entry(conn, "tx-v2", "unverified", "LOCAL", 2);
        assert_eq!(count_entries_below_sig_version(conn, 2).unwrap(), 0);
    }

    #[test]
    fn count_entries_below_sig_version_errors_on_missing_table() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(count_entries_below_sig_version(&conn, 2).is_err());
    }
}
