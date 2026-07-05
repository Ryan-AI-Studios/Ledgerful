use crate::state::storage::connection::StorageManager;
use miette::{IntoDiagnostic, Result};
use std::collections::HashMap;

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

impl StorageManager {
    pub fn save_verification_run(
        &self,
        timestamp: &str,
        plan_json: Option<&str>,
        overall_pass: bool,
        tx_id: Option<&str>,
    ) -> Result<i64> {
        debug_assert!(
            !self.is_read_only,
            "write called on read-only StorageManager"
        );
        self.conn
            .execute(
                "INSERT INTO verification_runs (timestamp, plan_json, overall_pass, tx_id) VALUES (?1, ?2, ?3, ?4)",
                (timestamp, plan_json, overall_pass as i32, tx_id),
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
        tx_id: Option<&str>,
    ) -> Result<()> {
        debug_assert!(
            !self.is_read_only,
            "write called on read-only StorageManager"
        );
        self.conn
            .execute(
                "INSERT INTO verification_results (run_id, command, exit_code, duration_ms, truncated, tx_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                (run_id, command, exit_code, duration_ms as i64, truncated as i32, tx_id),
            )
            .into_diagnostic()?;
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
}

#[cfg(test)]
mod tests {
    use crate::state::storage::connection::in_memory_storage;

    #[test]
    fn test_save_verification_run() {
        let storage = in_memory_storage();
        let id = storage
            .save_verification_run("2026-01-01T00:00:00Z", Some(r#"{"steps":[]}"#), true, None)
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
            .save_verification_run("2026-01-01T00:00:00Z", None, false, None)
            .unwrap();
        storage
            .save_verification_result(run_id, "cargo test", 1, 3000, false, None)
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
                .get_connection()
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
                .get_connection()
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
            .get_connection()
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
}
