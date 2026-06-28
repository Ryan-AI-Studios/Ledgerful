use crate::impact::packet::{ChangedFile, ImpactPacket};
use crate::index::storage::persist_symbols;
use crate::state::layout::Layout;
use crate::state::migrations::get_migrations;
use camino::{Utf8Path, Utf8PathBuf};
use miette::{IntoDiagnostic, Result};
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::debug;

use crate::index::symbols::SymbolKind;

pub struct StoredSymbol {
    pub file_path: String,
    pub name: String,
    pub kind: SymbolKind,
    pub is_public: bool,
}

/// One row of the per-date verification trend (`/api/verify/history`).
#[derive(Debug, Clone)]
pub struct VerificationTrendRow {
    pub date: String,
    pub passed: u64,
    pub failed: u64,
}

/// Per-step verification aggregates (`/api/verify/steps`).
#[derive(Debug, Clone)]
pub struct VerificationStepStatsRow {
    pub command: String,
    pub total: u64,
    pub passed: u64,
    pub average_duration_ms: f64,
    pub last_run_at: String,
    pub recent_failures: u64,
}

/// One raw joined row of the SOC2 verification-history export
/// (`verification_runs` × `verification_results`). Owned by `state/` so all
/// SQL stays inside `StorageManager`; the export module only consumes the
/// rows. Ordering matches the SQL `ORDER BY run_timestamp DESC, command ASC`
/// so the CSV emitted by the export is deterministic.
#[derive(Debug, Clone)]
pub struct VerificationExportRow {
    pub run_timestamp: String,
    pub overall_pass: bool,
    pub command: String,
    pub exit_code: i32,
    pub duration_ms: i64,
}

pub struct StorageManager {
    conn: Connection,
    pub cozo: Option<crate::state::storage_cozo::CozoStorage>,
    is_read_only: bool,
    root_path: Utf8PathBuf,
}

impl StorageManager {
    pub fn root_path(&self) -> &Utf8Path {
        &self.root_path
    }

    pub fn init(db_path: &Path) -> Result<Self> {
        debug!("StorageManager::init called with {:?}", db_path);
        // Captured BEFORE `Connection::open`, which itself creates the file:
        // this is the only reliable way to tell "brand-new project" (no
        // prior ledger.db) apart from "existing project, stale schema"
        // (Track TA31 R3 footgun #1 — a fresh DB starting at version 0 is
        // normal first-time init, not a migration the user needs telling
        // about).
        let db_existed_before_open = db_path.exists();
        let mut conn = Connection::open(db_path).into_diagnostic()?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000; PRAGMA foreign_keys = ON;",
        )
        .into_diagnostic()?;

        crate::state::migration_prompt::check_and_prompt_migration(&conn, db_existed_before_open)?;

        let migrations = get_migrations();
        migrations.to_latest(&mut conn).into_diagnostic()?;

        let cozo_path = db_path
            .parent()
            .map(|p| p.join("ledger.cozo"))
            .unwrap_or_default();
        let cozo = if !cozo_path.as_os_str().is_empty() {
            Some(crate::state::storage_cozo::CozoStorage::new(&cozo_path)?)
        } else {
            None
        };

        let root_path = db_path
            .parent() // state/
            .and_then(|p| p.parent()) // .ledgerful/
            .and_then(|p| p.parent()) // root/
            .unwrap_or(Path::new("."));
        let root_path = Utf8PathBuf::from_path_buf(root_path.to_path_buf())
            .map_err(|_| miette::miette!("Invalid UTF-8 in root path"))?;

        debug!("Initialized storage at {:?}", db_path);
        Ok(Self {
            conn,
            cozo,
            is_read_only: false,
            root_path,
        })
    }

    pub fn root(&self) -> &Utf8Path {
        &self.root_path
    }

    pub fn get_connection(&self) -> &Connection {
        &self.conn
    }

    pub fn get_connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Explicitly shutdown the storage manager, releasing all file locks.
    pub fn shutdown(mut self) -> Result<()> {
        debug!("Shutting down StorageManager");
        if let Some(cozo) = self.cozo.take() {
            cozo.shutdown();
        }

        let conn = std::mem::replace(
            &mut self.conn,
            Connection::open_in_memory().into_diagnostic()?,
        );
        if let Err((_conn, e)) = conn.close() {
            return Err(miette::miette!("Failed to close SQLite connection: {}", e));
        }

        Ok(())
    }

    /// Open storage in read-only mode, skipping migration checks.
    /// This is a fast-path for read-only commands that do not write to storage.
    ///
    /// Returns `Err` if the SQLite database file does not exist.
    pub fn open_read_only(root: &Utf8Path) -> Result<Self> {
        Self::open_read_only_with_options(root, true)
    }

    /// Open storage in read-only mode, skipping migration checks and NOT opening CozoDB.
    /// This is the fastest path for commands that only need metadata or transaction status.
    pub fn open_read_only_sqlite_only(root: &Utf8Path) -> Result<Self> {
        Self::open_read_only_with_options(root, false)
    }

    fn open_read_only_with_options(root: &Utf8Path, include_cozo: bool) -> Result<Self> {
        let db_path = Layout::new(root).state_subdir().join("ledger.db");

        if !db_path.exists() {
            return Err(miette::miette!(
                "Storage not initialized at {}. Run a write command first (e.g. `ledgerful scan`).",
                db_path
            ));
        }

        tracing::debug!(
            "Opening read-only storage at {:?} (include_cozo: {})",
            db_path,
            include_cozo
        );
        let conn = Connection::open(db_path.as_std_path()).into_diagnostic()?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000; PRAGMA foreign_keys = ON;",
        )
        .into_diagnostic()?;

        #[cfg(not(test))]
        {
            let migrations = crate::state::migrations::get_migrations();
            let current_version = migrations.current_version(&conn).into_diagnostic()?;
            let latest_version = crate::state::migrations::get_migrations_count();
            let is_mismatch = match current_version {
                rusqlite_migration::SchemaVersion::NoneSet => latest_version > 0,
                rusqlite_migration::SchemaVersion::Inside(v) => v.get() < latest_version,
                rusqlite_migration::SchemaVersion::Outside(v) => v.get() < latest_version,
            };
            if is_mismatch {
                return Err(crate::state::StateError::SchemaMismatch.into());
            }
        }

        let cozo = if include_cozo {
            let cozo_path = db_path
                .parent()
                .map(|p| p.join("ledger.cozo"))
                .unwrap_or_default();
            if cozo_path.exists() {
                Some(crate::state::storage_cozo::CozoStorage::new_read_only(
                    cozo_path.as_std_path(),
                )?)
            } else {
                None
            }
        } else {
            None
        };

        tracing::debug!("Opened read-only storage at {:?}", db_path);
        Ok(Self {
            conn,
            cozo,
            is_read_only: true,
            root_path: root.to_path_buf(),
        })
    }

    pub fn init_from_conn(conn: Connection) -> Self {
        Self {
            conn,
            cozo: None,
            is_read_only: false,
            root_path: Utf8PathBuf::from("."),
        }
    }

    pub fn save_packet(&self, packet: &ImpactPacket) -> Result<()> {
        debug_assert!(
            !self.is_read_only,
            "write called on read-only StorageManager"
        );
        let packet_json = serde_json::to_string(packet).into_diagnostic()?;
        let is_clean = if packet.changes.is_empty() { 1 } else { 0 };

        self.conn
            .execute(
                "INSERT INTO snapshots (timestamp, head_hash, branch_name, is_clean, packet_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    &packet.timestamp_utc,
                    &packet.head_hash,
                    &packet.branch_name,
                    is_clean,
                    &packet_json,
                ),
            )
            .into_diagnostic()?;

        let snapshot_id = self.conn.last_insert_rowid();
        self.save_changed_files(snapshot_id, &packet.changes)?;
        persist_symbols(&self.conn, snapshot_id, &packet.changes)?;

        Ok(())
    }

    pub fn get_latest_packet(&self) -> Result<Option<ImpactPacket>> {
        let mut stmt = self
            .conn
            .prepare("SELECT packet_json FROM snapshots ORDER BY id DESC LIMIT 1")
            .into_diagnostic()?;

        let mut rows = stmt.query([]).into_diagnostic()?;

        if let Some(row) = rows.next().into_diagnostic()? {
            let json: String = row.get(0).into_diagnostic()?;
            let packet: ImpactPacket = serde_json::from_str(&json).into_diagnostic()?;
            Ok(Some(packet))
        } else {
            Ok(None)
        }
    }

    pub fn get_all_packets(&self) -> Result<Vec<ImpactPacket>> {
        let mut stmt = self
            .conn
            .prepare("SELECT packet_json FROM snapshots ORDER BY id ASC")
            .into_diagnostic()?;

        let rows = stmt
            .query_map([], |row| {
                let json: String = row.get(0)?;
                serde_json::from_str(&json).map_err(|_e| rusqlite::Error::InvalidQuery)
            })
            .into_diagnostic()?;

        let mut packets = Vec::new();
        for packet in rows {
            packets.push(packet.into_diagnostic()?);
        }
        Ok(packets)
    }

    pub fn save_batch(&self, timestamp: &str, event_count: u32, batch_json: &str) -> Result<i64> {
        debug_assert!(
            !self.is_read_only,
            "write called on read-only StorageManager"
        );
        self.conn
            .execute(
                "INSERT INTO batches (timestamp, event_count, batch_json) VALUES (?1, ?2, ?3)",
                (timestamp, event_count, batch_json),
            )
            .into_diagnostic()?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn save_verification_run(
        &self,
        timestamp: &str,
        plan_json: Option<&str>,
        overall_pass: bool,
    ) -> Result<i64> {
        debug_assert!(
            !self.is_read_only,
            "write called on read-only StorageManager"
        );
        self.conn
            .execute(
                "INSERT INTO verification_runs (timestamp, plan_json, overall_pass) VALUES (?1, ?2, ?3)",
                (timestamp, plan_json, overall_pass as i32),
            )
            .into_diagnostic()?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn save_verification_result(
        &self,
        run_id: i64,
        command: &str,
        exit_code: i32,
        duration_ms: u64,
        truncated: bool,
    ) -> Result<()> {
        debug_assert!(
            !self.is_read_only,
            "write called on read-only StorageManager"
        );
        self.conn
            .execute(
                "INSERT INTO verification_results (run_id, command, exit_code, duration_ms, truncated) VALUES (?1, ?2, ?3, ?4, ?5)",
                (run_id, command, exit_code, duration_ms as i64, truncated as i32),
            )
            .into_diagnostic()?;
        Ok(())
    }

    pub fn save_changed_files(&self, snapshot_id: i64, files: &[ChangedFile]) -> Result<()> {
        debug_assert!(
            !self.is_read_only,
            "write called on read-only StorageManager"
        );
        for file in files {
            self.conn
                .execute(
                    "INSERT INTO changed_files (snapshot_id, path, status, is_staged) VALUES (?1, ?2, ?3, ?4)",
                    (snapshot_id, file.path.to_string_lossy().as_ref(), &file.status, file.is_staged as i32),
                )
                .into_diagnostic()?;
        }
        Ok(())
    }

    pub fn get_latest_verification_run(&self) -> Result<Option<(i64, String, bool)>> {
        let result = self.conn.query_row(
            "SELECT id, timestamp, overall_pass FROM verification_runs ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)? != 0)),
        );

        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).into_diagnostic(),
        }
    }

    /// Aggregate `verification_runs` into per-date pass/fail counts over the
    /// last `days` days. Dates with no runs are omitted (deterministic: only
    /// dates that have at least one run appear, sorted ascending by date).
    ///
    /// `cutoff_iso` is an RFC 3339 / ISO 8601 timestamp; rows whose `timestamp`
    /// is lexically `>= cutoff_iso` are included. SQLite's `DATE()` parser
    /// accepts the RFC 3339 form stored by `save_verification_run`.
    pub fn get_verification_history(&self, cutoff_iso: &str) -> Result<Vec<VerificationTrendRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT DATE(timestamp) AS date, \
                        SUM(CASE WHEN overall_pass = 1 THEN 1 ELSE 0 END) AS passed, \
                        SUM(CASE WHEN overall_pass = 0 THEN 1 ELSE 0 END) AS failed \
                 FROM verification_runs \
                 WHERE timestamp >= ?1 \
                 GROUP BY DATE(timestamp) \
                 ORDER BY date ASC",
            )
            .into_diagnostic()?;

        let rows = stmt
            .query_map(rusqlite::params![cutoff_iso], |row| {
                Ok(VerificationTrendRow {
                    date: row.get(0)?,
                    passed: row.get::<_, i64>(1)? as u64,
                    failed: row.get::<_, i64>(2)? as u64,
                })
            })
            .into_diagnostic()?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.into_diagnostic()?);
        }
        Ok(out)
    }

    /// Per-step verification aggregates across all history, plus recent-failure
    /// counts within the last `recent_run_count` verification runs.
    ///
    /// Source: `verification_results` joined to `verification_runs`. The
    /// `command` column is the per-step identifier (the verify plan's
    /// `command` field — the plan step has no separate `id`/`name`, so the
    /// command serves as both). `last_run_at` is the most recent run
    /// timestamp that produced a result for that command. `average_duration_ms`
    /// is the mean `duration_ms` across all results for that command.
    /// `pass_rate_percent` is `passed / total * 100`, rounded to 2 decimal
    /// places. `recent_failures` counts results with `exit_code != 0` within
    /// the last `recent_run_count` runs (by `verification_runs.id DESC`).
    pub fn get_verification_step_stats(
        &self,
        recent_run_count: usize,
    ) -> Result<Vec<VerificationStepStatsRow>> {
        let mut agg_stmt = self
            .conn
            .prepare(
                "SELECT vr.command, \
                        COUNT(*) AS total, \
                        SUM(CASE WHEN vr.exit_code = 0 THEN 1 ELSE 0 END) AS passed, \
                        AVG(vr.duration_ms) AS avg_ms, \
                        MAX(r.timestamp) AS last_run_at \
                 FROM verification_results vr \
                 JOIN verification_runs r ON vr.run_id = r.id \
                 GROUP BY vr.command",
            )
            .into_diagnostic()?;

        let agg_rows = agg_stmt
            .query_map([], |row| {
                let avg_ms: Option<f64> = row.get(3)?;
                Ok(VerificationStepStatsRow {
                    command: row.get(0)?,
                    total: row.get::<_, i64>(1)? as u64,
                    passed: row.get::<_, i64>(2)? as u64,
                    average_duration_ms: avg_ms.unwrap_or(0.0),
                    last_run_at: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    recent_failures: 0,
                })
            })
            .into_diagnostic()?;

        let mut by_command: std::collections::HashMap<String, VerificationStepStatsRow> =
            std::collections::HashMap::new();
        for row in agg_rows {
            let row = row.into_diagnostic()?;
            by_command.insert(row.command.clone(), row);
        }

        // Recent-failure counts within the last `recent_run_count` runs.
        let mut recent_stmt = self
            .conn
            .prepare(
                "SELECT vr.command, \
                        SUM(CASE WHEN vr.exit_code != 0 THEN 1 ELSE 0 END) AS recent_failures \
                 FROM verification_results vr \
                 WHERE vr.run_id IN ( \
                     SELECT id FROM verification_runs ORDER BY id DESC LIMIT ?1 \
                 ) \
                 GROUP BY vr.command",
            )
            .into_diagnostic()?;

        let recent_rows = recent_stmt
            .query_map(rusqlite::params![recent_run_count as i64], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
            })
            .into_diagnostic()?;

        for row in recent_rows {
            let (command, recent_failures) = row.into_diagnostic()?;
            if let Some(stats) = by_command.get_mut(&command) {
                stats.recent_failures = recent_failures;
            }
        }

        let mut out: Vec<VerificationStepStatsRow> = by_command.into_values().collect();
        out.sort_by(|a, b| a.command.cmp(&b.command));
        Ok(out)
    }

    /// Raw joined `verification_runs` × `verification_results` rows for the
    /// SOC2 evidence export's `verification_history.csv`. Persistence is owned
    /// by `state/`: the join and ordering live here, the export module only
    /// formats the rows into CSV.
    ///
    /// Columns: `run_timestamp` (from `verification_runs.timestamp`),
    /// `overall_pass` (from `verification_runs.overall_pass`), `command`,
    /// `exit_code`, `duration_ms` (from `verification_results`). Joined on
    /// `vr.run_id = r.id`, ordered by `run_timestamp DESC` then `command ASC`
    /// for deterministic CSV output.
    ///
    /// Read-only: uses the storage connection without mutation.
    pub fn get_verification_export_rows(&self) -> Result<Vec<VerificationExportRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT r.timestamp, r.overall_pass, vr.command, vr.exit_code, vr.duration_ms \
                 FROM verification_results vr \
                 JOIN verification_runs r ON vr.run_id = r.id \
                 ORDER BY r.timestamp DESC, vr.command ASC",
            )
            .into_diagnostic()?;

        let rows = stmt.query_map([], |row| {
            Ok(VerificationExportRow {
                run_timestamp: row.get(0)?,
                overall_pass: row.get::<_, i64>(1)? != 0,
                command: row.get(2)?,
                exit_code: row.get(3)?,
                duration_ms: row.get(4)?,
            })
        });

        let mut out = Vec::new();
        for row in rows.into_diagnostic()? {
            out.push(row.into_diagnostic()?);
        }
        Ok(out)
    }

    /// Build a `command -> description` map for the friendly step `name` field
    /// of `/api/verify/steps`, by parsing `plan_json` from the most recent
    /// `plan_run_limit` verification runs.
    ///
    /// `plan_json` is a serialized `crate::verify::plan::VerificationPlan` (see
    /// `verify::engine::persist_verify_report`), whose `steps` each carry a
    /// `command` and a human-readable `description`. We iterate from the most
    /// recent run backwards and keep the first (i.e. latest) description seen
    /// for each command, so the map reflects the newest plan that referenced
    /// each step. Any row that fails to parse as a `VerificationPlan` is
    /// silently skipped — a malformed `plan_json` on one run must NOT fail the
    /// whole endpoint. An empty/NULL `plan_json` also skips cleanly.
    pub fn get_verification_command_descriptions(
        &self,
        plan_run_limit: usize,
    ) -> Result<HashMap<String, String>> {
        use crate::verify::plan::VerificationPlan;

        let mut stmt = self
            .conn
            .prepare(
                "SELECT plan_json FROM verification_runs \
                 WHERE plan_json IS NOT NULL AND plan_json != '' \
                 ORDER BY id DESC LIMIT ?1",
            )
            .into_diagnostic()?;

        let rows = stmt
            .query_map(rusqlite::params![plan_run_limit as i64], |row| {
                row.get::<_, Option<String>>(0)
            })
            .into_diagnostic()?;

        let mut map: HashMap<String, String> = HashMap::new();
        for row in rows {
            let row = row.into_diagnostic()?;
            let Some(json) = row else { continue };
            let Ok(plan) = serde_json::from_str::<VerificationPlan>(&json) else {
                continue;
            };
            for step in plan.steps {
                // First (latest) description wins per command.
                map.entry(step.command).or_insert(step.description);
            }
        }
        Ok(map)
    }

    /// Per-snapshot total hotspot counts for the `limit` most recent distinct
    /// `hotspot_history` timestamps, ordered newest-first.
    ///
    /// A "snapshot" is the set of `hotspot_history` rows sharing a `timestamp`
    /// value (the `hotspot_history` migration at
    /// `src/state/migrations/m38_hotspot_history.rs` writes one row per file
    /// per scan, all rows in a scan share the scan's timestamp). The returned
    /// totals are `COUNT(*)` of rows with that timestamp — i.e. the number of
    /// files flagged as hotspots in that scan — one entry per distinct
    /// timestamp, ordered by `timestamp DESC` (newest snapshot first). Fewer
    /// than `limit` entries are returned when fewer distinct snapshots exist.
    ///
    /// Read-only: uses the storage connection without mutation. The caller
    /// owns any derived math (e.g. percent-delta between the two most recent
    /// totals) so this method stays a pure persistence accessor.
    pub fn get_latest_hotspot_snapshot_totals(&self, limit: usize) -> Result<Vec<u64>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT COUNT(*) AS total \
                 FROM hotspot_history \
                 GROUP BY timestamp \
                 ORDER BY timestamp DESC \
                 LIMIT ?1",
            )
            .into_diagnostic()?;

        let rows = stmt
            .query_map(rusqlite::params![limit as i64], |row| {
                Ok(row.get::<_, i64>(0)? as u64)
            })
            .into_diagnostic()?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.into_diagnostic()?);
        }
        Ok(out)
    }

    pub fn get_directory_classifications(
        &self,
    ) -> Result<Vec<crate::index::topology::DirectoryClassification>> {
        let mut stmt = self
            .conn
            .prepare("SELECT dir_path, role, confidence, evidence FROM project_topology")
            .into_diagnostic()?;

        let rows = stmt
            .query_map([], |row| {
                let role_str: String = row.get(1)?;
                let role = crate::index::topology::DirectoryRole::parse(&role_str)
                    .unwrap_or(crate::index::topology::DirectoryRole::Source);
                Ok(crate::index::topology::DirectoryClassification {
                    dir_path: row.get(0)?,
                    role,
                    confidence: row.get(2)?,
                    evidence: row.get(3)?,
                })
            })
            .into_diagnostic()?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.into_diagnostic()?);
        }
        Ok(results)
    }

    /// Returns a map of file paths to their internal IDs in the project_files table.
    /// Only includes files that are not marked as DELETED.
    pub fn get_active_file_id_map(&self) -> Result<HashMap<PathBuf, i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, file_path FROM project_files WHERE parse_status != 'DELETED'")
            .into_diagnostic()?;

        let rows = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let path: String = row.get(1)?;
                Ok((PathBuf::from(path), id))
            })
            .into_diagnostic()?;

        let mut map = HashMap::new();
        for row in rows {
            let (path, id) = row.into_diagnostic()?;
            map.insert(path, id);
        }
        Ok(map)
    }

    /// Checks if a table exists in the database.
    pub fn table_exists(&self, table_name: &str) -> Result<bool> {
        let exists: bool = self
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [table_name],
                |row| row.get(0),
            )
            .into_diagnostic()?;

        Ok(exists)
    }

    /// Checks if a table exists and contains at least one row.
    pub fn table_exists_and_has_data(&self, table_name: &str) -> Result<bool> {
        // Basic validation to prevent injection since we use format! for the table name
        if !table_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(miette::miette!("Invalid table name: {}", table_name));
        }

        if !self.table_exists(table_name)? {
            return Ok(false);
        }

        // Then check if it has data
        let query = format!("SELECT EXISTS(SELECT 1 FROM {} LIMIT 1)", table_name);
        let has_data: bool = self
            .conn
            .query_row(&query, [], |row| row.get(0))
            .into_diagnostic()?;

        Ok(has_data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::{FileAnalysisStatus, ImpactPacket};
    use std::path::PathBuf;

    fn in_memory_storage() -> StorageManager {
        let conn = Connection::open_in_memory().unwrap();
        let mut conn = conn;
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager {
            conn,
            cozo: None,
            is_read_only: false,
            root_path: Utf8PathBuf::from("."),
        }
    }

    #[test]
    fn test_storage_basic_ops() {
        let storage = in_memory_storage();

        let packet = ImpactPacket {
            head_hash: Some("test_hash".to_string()),
            ..Default::default()
        };

        storage.save_packet(&packet).unwrap();

        let latest = storage.get_latest_packet().unwrap().unwrap();
        assert_eq!(latest.head_hash, Some("test_hash".to_string()));
    }

    #[test]
    fn test_save_batch() {
        let storage = in_memory_storage();
        let id = storage
            .save_batch("2026-01-01T00:00:00Z", 3, r#"{"events":[]}"#)
            .unwrap();
        assert!(id > 0);
    }

    #[test]
    fn test_save_verification_run() {
        let storage = in_memory_storage();
        let id = storage
            .save_verification_run("2026-01-01T00:00:00Z", Some(r#"{"steps":[]}"#), true)
            .unwrap();
        assert!(id > 0);

        let latest = storage.get_latest_verification_run().unwrap().unwrap();
        assert_eq!(latest.0, id);
        assert!(latest.2);
    }

    #[test]
    fn test_save_verification_result() {
        let storage = in_memory_storage();
        let run_id = storage
            .save_verification_run("2026-01-01T00:00:00Z", None, false)
            .unwrap();
        storage
            .save_verification_result(run_id, "cargo test", 1, 3000, false)
            .unwrap();
    }

    #[test]
    fn test_save_changed_files() {
        let storage = in_memory_storage();
        let packet = ImpactPacket {
            head_hash: Some("abc".to_string()),
            changes: vec![ChangedFile {
                path: PathBuf::from("src/main.rs"),
                status: "Added".to_string(),
                old_path: None,
                is_staged: true,

                symbols: None,
                imports: None,
                runtime_usage: None,
                analysis_status: FileAnalysisStatus::default(),
                analysis_warnings: Vec::new(),
                api_routes: Vec::new(),
                data_models: Vec::new(),
                ci_gates: Vec::new(),
            }],
            ..Default::default()
        };
        storage.save_packet(&packet).unwrap();

        let snapshot_id = storage.conn.last_insert_rowid();
        storage
            .save_changed_files(snapshot_id, &packet.changes)
            .unwrap();
    }

    #[test]
    fn test_get_latest_verification_run_empty() {
        let storage = in_memory_storage();
        let result = storage.get_latest_verification_run().unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_latest_hotspot_snapshot_totals_orders_newest_first() {
        // Seed two snapshots with distinct row counts so the method returns
        // per-snapshot totals ordered newest-first. Also pins the empty-DB
        // and single-snapshot (zero-guard adjacent) paths.
        let storage = in_memory_storage();

        // No rows yet → empty result, not an error.
        assert!(
            storage
                .get_latest_hotspot_snapshot_totals(2)
                .unwrap()
                .is_empty()
        );

        // Older snapshot: 4 rows at 2026-06-19T09:00:00Z.
        for i in 0..4 {
            storage
                .conn
                .execute(
                    "INSERT INTO hotspot_history \
                     (file_path, score, display_score, complexity, frequency, timestamp) \
                     VALUES (?1, 1.0, 1.0, 1, 1.0, '2026-06-19T09:00:00Z')",
                    rusqlite::params![format!("src/old{i}.rs")],
                )
                .unwrap();
        }
        // Newer snapshot: 5 rows at 2026-06-20T09:00:00Z.
        for i in 0..5 {
            storage
                .conn
                .execute(
                    "INSERT INTO hotspot_history \
                     (file_path, score, display_score, complexity, frequency, timestamp) \
                     VALUES (?1, 2.0, 2.0, 2, 2.0, '2026-06-20T09:00:00Z')",
                    rusqlite::params![format!("src/new{i}.rs")],
                )
                .unwrap();
        }

        // limit=2 → [newer=5, older=4] (newest first).
        let totals = storage.get_latest_hotspot_snapshot_totals(2).unwrap();
        assert_eq!(totals, vec![5, 4]);

        // limit=1 → only the newest snapshot's total.
        let totals_one = storage.get_latest_hotspot_snapshot_totals(1).unwrap();
        assert_eq!(totals_one, vec![5]);

        // limit larger than the number of distinct snapshots → returns all
        // available, still newest-first.
        let totals_over = storage.get_latest_hotspot_snapshot_totals(10).unwrap();
        assert_eq!(totals_over, vec![5, 4]);
    }

    #[test]
    fn test_get_latest_hotspot_snapshot_totals_single_snapshot() {
        // One distinct timestamp → one total. This is the adjacent coverage
        // for the `older_total == 0` division-by-zero guard in
        // `fetch_hotspot_delta_percent`: a DISTINCT timestamp always has
        // COUNT>=1, so `older_total` can only be 0 if the caller passes a
        // synthetic zero. The guard is retained as defensive code.
        let storage = in_memory_storage();
        storage
            .conn
            .execute(
                "INSERT INTO hotspot_history \
                 (file_path, score, display_score, complexity, frequency, timestamp) \
                 VALUES ('src/only.rs', 1.0, 1.0, 1, 1.0, '2026-06-20T09:00:00Z')",
                [],
            )
            .unwrap();
        let totals = storage.get_latest_hotspot_snapshot_totals(2).unwrap();
        assert_eq!(totals, vec![1]);
    }

    #[test]
    fn test_table_exists() {
        let storage = in_memory_storage();
        assert!(storage.table_exists("snapshots").unwrap());
        assert!(!storage.table_exists("non_existent").unwrap());
    }

    #[test]
    fn test_table_exists_and_has_data() {
        let storage = in_memory_storage();
        // snapshots table is created in migrations, but empty
        assert!(!storage.table_exists_and_has_data("snapshots").unwrap());

        // Save a packet to make it non-empty
        let packet = ImpactPacket::default();
        storage.save_packet(&packet).unwrap();
        assert!(storage.table_exists_and_has_data("snapshots").unwrap());

        // Non-existent table
        assert!(!storage.table_exists_and_has_data("non_existent").unwrap());
    }

    #[test]
    fn test_get_active_file_id_map() {
        let storage = in_memory_storage();
        storage.conn.execute(
            "INSERT INTO project_files (file_path, parse_status, last_indexed_at) VALUES ('src/a.rs', 'OK', '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();
        storage.conn.execute(
            "INSERT INTO project_files (file_path, parse_status, last_indexed_at) VALUES ('src/b.rs', 'DELETED', '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();

        let map = storage.get_active_file_id_map().unwrap();
        assert_eq!(map.len(), 1);
        assert!(map.contains_key(&PathBuf::from("src/a.rs")));
        assert!(!map.contains_key(&PathBuf::from("src/b.rs")));
    }

    #[test]
    fn read_only_skips_migrations() {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();

        // Create an empty SQLite file (no migrations have run)
        let db_path = layout.state_subdir().join("ledger.db");
        let conn = Connection::open(db_path.as_std_path()).unwrap();
        let initial_version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(initial_version, 0, "fresh db should have user_version=0");
        drop(conn);

        // Call open_read_only — in RED phase this delegates to init which
        // runs migrations, so the test will fail. In GREEN phase it skips
        // migrations and the test passes.
        let storage = StorageManager::open_read_only(root).unwrap();

        // Verify no migrations ran — user_version should still be 0
        let version: i64 = storage
            .get_connection()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 0, "open_read_only should not run migrations");
    }

    #[test]
    fn read_only_fails_on_missing_db() {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        // Do NOT create an SQLite file

        // In RED phase open_read_only delegates to init which creates the
        // file via Connection::open, so the test fails. In GREEN phase
        // open_read_only checks path existence first and returns Err.
        let result = StorageManager::open_read_only(root);
        assert!(
            result.is_err(),
            "open_read_only should fail without a db file"
        );
    }
}
