use std::path::Path;
use std::time::Instant;

use gix::ObjectId;

use super::{ProposalRequest, Snapshot};

#[cfg(any(
    windows,
    all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")
))]
mod native {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    use rusqlite::{Connection, ErrorCode};

    use crate::change_evidence::pack_store::{
        capture_budget::{CaptureBudget, CapturePhase},
        proposal_source::{ProposalBudget, ProposalCheckout, context_changed, discover_layout},
        with_proposal_reader,
    };
    use crate::commands::architecture::config::DEFAULT_CONFIG_REL;
    use crate::commands::baseline::extract::capture_snapshot::{
        CaptureSnapshotError, CaptureSnapshotLimits, extract_capture_snapshot_with,
    };
    use crate::state::{
        layout::{LEDGERFUL_STATE_DIR_ENV, Layout},
        migrations::get_migrations_count,
    };

    use super::super::MAX_PAIRS;
    use super::*;

    const MAX_INDEX_FILES: usize = 2_000;
    const MAX_RAW_EDGES: usize = 25_000;
    const MAX_READ_BYTES: usize = 128 * 1024 * 1024;
    const SNAPSHOT_LIMITS: CaptureSnapshotLimits = CaptureSnapshotLimits {
        active_files: MAX_INDEX_FILES,
        scanned_files: MAX_INDEX_FILES,
        scanned_edges: MAX_RAW_EDGES,
        scanned_metadata: 256,
        text_bytes: MAX_READ_BYTES,
    };
    const DEADLINE_POLL: usize = 1_000;
    const VM_CEILING: usize = 2_000_000;
    const MAX_BUSY: Duration = Duration::from_millis(100);
    const BUSY_STEP: Duration = Duration::from_millis(1);

    type Result<T> = std::result::Result<T, &'static str>;

    pub(super) fn observe(
        request: &ProposalRequest,
        cwd: &Path,
        head: ObjectId,
        deadline: Instant,
    ) -> Result<Snapshot> {
        if std::env::var_os(LEDGERFUL_STATE_DIR_ENV).is_some() {
            return Err("STORAGE_UNAVAILABLE");
        }
        let start = Instant::now();
        let budget = CaptureBudget::at(
            start,
            deadline.saturating_duration_since(start),
            MAX_READ_BYTES,
        );
        let phase = budget.phase().ok_or("LIMIT_EXCEEDED")?;
        let source_budget = ProposalBudget::new();
        let (layout, cwd_identity) = discover_layout(cwd, phase)?;
        let first = ProposalCheckout::open(&layout, head, phase, DEFAULT_CONFIG_REL)?;
        let observed = with_proposal_reader(&layout, phase, |mut connection, deadline| {
            let result = SqlBudget::bind(&mut connection, deadline).and_then(|vm| {
                let result = read_snapshot(
                    &connection,
                    request,
                    head,
                    &layout,
                    phase,
                    &source_budget,
                    first,
                );
                if vm.expired() {
                    Err("LIMIT_EXCEEDED")
                } else {
                    result
                }
            });
            (connection, result)
        })
        .ok_or("STORAGE_UNAVAILABLE")??;
        let (confirmed, identity_after) = discover_layout(cwd, phase).map_err(context_changed)?;
        if identity_after != cwd_identity || confirmed.root != layout.root {
            return Err("CONTEXT_CHANGED");
        }
        Ok(observed)
    }

    fn read_snapshot<'a>(
        connection: &Connection,
        request: &ProposalRequest,
        head: ObjectId,
        layout: &Layout,
        phase: CapturePhase<'a>,
        budget: &'a ProposalBudget,
        mut first: ProposalCheckout<'a>,
    ) -> Result<Snapshot> {
        connection
            .pragma_update(None, "temp_store", "MEMORY")
            .map_err(sql_error)?;
        connection
            .pragma_update(None, "query_only", true)
            .map_err(sql_error)?;
        connection
            .pragma_update(None, "trusted_schema", false)
            .map_err(sql_error)?;
        let tx = connection.unchecked_transaction().map_err(sql_error)?;
        let version: i64 = tx
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(sql_error)?;
        if usize::try_from(version).ok() != Some(get_migrations_count()) {
            return Err("STORAGE_UNAVAILABLE");
        }
        let mut prior = None;
        let captured =
            extract_capture_snapshot_with(&tx, SNAPSHOT_LIMITS, Some(MAX_PAIRS), |inputs| {
                prior = Some(first.observe_inputs(
                    inputs,
                    budget,
                    layout,
                    &request.from,
                    &request.to,
                )?);
                Ok(())
            })
            .map_err(snapshot_error)?;
        let mut second = ProposalCheckout::open(layout, head, phase, DEFAULT_CONFIG_REL)
            .map_err(context_changed)?;
        let verified = second
            .observe_inputs(&captured, budget, layout, &request.from, &request.to)
            .map_err(context_changed)?;
        if prior.as_ref() != Some(&verified) {
            return Err("CONTEXT_CHANGED");
        }
        let (scope, raw_config) = first.confirm(second, layout, DEFAULT_CONFIG_REL)?;
        tx.commit().map_err(sql_error)?;
        let edges = captured.finish_proposal(verified.stale_threshold_days());
        Ok(Snapshot {
            raw_config,
            scope,
            index_config_digest: verified.config_digest(),
            edges,
        })
    }

    struct SqlBudget {
        opcodes: Arc<AtomicUsize>,
        deadline: Instant,
    }

    impl SqlBudget {
        fn bind(connection: &mut Connection, deadline: Instant) -> Result<Self> {
            let opcodes = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&opcodes);
            let mut busy_budget = MAX_BUSY;
            connection
                .busy_handler(Some(move |_| {
                    let started = Instant::now();
                    let delay = busy_delay(busy_budget, deadline, started);
                    if delay.is_zero() {
                        return false;
                    }
                    std::thread::sleep(delay);
                    busy_budget = busy_budget.saturating_sub(started.elapsed());
                    Instant::now() < deadline && !busy_budget.is_zero()
                }))
                .map_err(sql_error)?;
            // One poll per opcode avoids uncharged instruction tails when
            // SQLite resets its progress counter at a statement boundary.
            connection
                .progress_handler(
                    1,
                    Some(move || {
                        let charged = counter.fetch_add(1, Ordering::Relaxed) + 1;
                        charged >= VM_CEILING
                            || (charged.is_multiple_of(DEADLINE_POLL) && Instant::now() >= deadline)
                    }),
                )
                .map_err(sql_error)?;
            Ok(Self { opcodes, deadline })
        }

        fn expired(&self) -> bool {
            self.opcodes.load(Ordering::Relaxed) >= VM_CEILING || Instant::now() >= self.deadline
        }
    }

    fn busy_delay(remaining: Duration, deadline: Instant, now: Instant) -> Duration {
        BUSY_STEP
            .min(remaining)
            .min(deadline.saturating_duration_since(now))
    }

    fn sql_error(error: rusqlite::Error) -> &'static str {
        if error.sqlite_error_code() == Some(ErrorCode::OperationInterrupted) {
            "LIMIT_EXCEEDED"
        } else {
            "STORAGE_UNAVAILABLE"
        }
    }

    fn snapshot_error(error: CaptureSnapshotError) -> &'static str {
        match error {
            CaptureSnapshotError::Admission(reason) => reason,
            CaptureSnapshotError::Limit(_) | CaptureSnapshotError::Allocation => "LIMIT_EXCEEDED",
            CaptureSnapshotError::Sql(error) => sql_error(error),
            CaptureSnapshotError::Graph(_) | CaptureSnapshotError::Corrupt(_) => {
                "STORAGE_UNAVAILABLE"
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::commands::baseline::extract::capture_snapshot::{
            CaptureSnapshot, SnapshotBound,
        };

        fn snapshot_fixture() -> Connection {
            let connection = Connection::open_in_memory().unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE project_files (
                        id INTEGER PRIMARY KEY, parse_status TEXT, file_path TEXT,
                        content_hash TEXT, parser_version TEXT, file_size INTEGER, mtime_ns INTEGER
                     );
                     CREATE TABLE structural_edges (
                         caller_file_id INTEGER, callee_file_id INTEGER,
                         resolution_status TEXT, evidence TEXT
                     );
                     CREATE TABLE index_metadata (key TEXT, value TEXT);",
                )
                .unwrap();
            connection
        }

        fn seed_files(connection: &Connection, count: usize) {
            connection
                .execute(
                    "WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i < ?1)
                     INSERT INTO project_files
                     SELECT i, 'OK', printf('src/file-%04d.rs', i), printf('%064x', i), '1', 1, 1
                     FROM n",
                    [count as i64],
                )
                .unwrap();
        }

        fn extract_test_snapshot(
            connection: &mut Connection,
        ) -> std::result::Result<CaptureSnapshot, CaptureSnapshotError> {
            let transaction = connection.transaction().unwrap();
            extract_capture_snapshot_with(
                &transaction,
                SNAPSHOT_LIMITS,
                Some(MAX_PAIRS),
                |_| Ok(()),
            )
        }

        #[test]
        fn architecture_proposal_index_limit_counts_the_first_excess_row_even_if_deleted() {
            let mut database = snapshot_fixture();
            seed_files(&database, MAX_INDEX_FILES);
            assert_eq!(
                extract_test_snapshot(&mut database)
                    .unwrap()
                    .witnesses()
                    .len(),
                MAX_INDEX_FILES
            );
            database
                .execute(
                    "INSERT INTO project_files (parse_status) VALUES ('DELETED')",
                    [],
                )
                .unwrap();
            assert!(matches!(
                extract_test_snapshot(&mut database),
                Err(CaptureSnapshotError::Limit(SnapshotBound::ScannedFiles))
            ));
        }

        #[test]
        fn architecture_proposal_raw_row_limit_counts_unresolved_ambiguous_and_capped_rows() {
            let mut database = snapshot_fixture();
            seed_files(&database, 1);
            database
                .execute(
                    "WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i < ?1)
                     INSERT INTO structural_edges
                     SELECT 1, NULL,
                            CASE i % 3 WHEN 0 THEN 'CAPPED' WHEN 1 THEN 'AMBIGUOUS' ELSE 'UNRESOLVED' END,
                            NULL FROM n",
                    [MAX_RAW_EDGES as i64],
                )
                .unwrap();
            let admitted = extract_test_snapshot(&mut database).unwrap().coverage;
            assert_eq!(
                admitted.edges_capped + admitted.edges_ambiguous + admitted.edges_unresolved,
                MAX_RAW_EDGES as u64
            );
            database
                .execute(
                    "INSERT INTO structural_edges VALUES (1, NULL, 'CAPPED', NULL)",
                    [],
                )
                .unwrap();
            assert!(matches!(
                extract_test_snapshot(&mut database),
                Err(CaptureSnapshotError::Limit(SnapshotBound::ScannedEdges))
            ));
        }

        #[test]
        fn architecture_proposal_projected_pair_limit_precedes_materializing_excess_pair() {
            let mut database = snapshot_fixture();
            seed_files(&database, 151);
            database
                .execute(
                    "WITH RECURSIVE n(i) AS (VALUES(0) UNION ALL SELECT i+1 FROM n WHERE i+1 < ?1)
                     INSERT INTO structural_edges
                     SELECT i / 100 + 1, i % 100 + 51, 'RESOLVED', NULL FROM n",
                    [MAX_PAIRS as i64],
                )
                .unwrap();
            assert_eq!(
                extract_test_snapshot(&mut database).unwrap().edges.len(),
                MAX_PAIRS
            );
            database
                .execute(
                    "INSERT INTO structural_edges VALUES (1, 151, 'RESOLVED', NULL)",
                    [],
                )
                .unwrap();
            assert!(matches!(
                extract_test_snapshot(&mut database),
                Err(CaptureSnapshotError::Limit(SnapshotBound::ProjectedPairs))
            ));
        }

        #[test]
        fn architecture_proposal_interrupts_cumulative_sql_work() {
            let mut connection = Connection::open_in_memory().unwrap();
            let vm =
                SqlBudget::bind(&mut connection, Instant::now() + Duration::from_secs(30)).unwrap();
            let query = "WITH RECURSIVE steps(n) AS (
                            VALUES(0) UNION ALL SELECT n+1 FROM steps WHERE n<20000
                         ) SELECT sum(n) FROM steps";
            let mut completed = 0;
            for _ in 0..100 {
                match connection.query_row::<i64, _, _>(query, [], |row| row.get(0)) {
                    Ok(sum) => {
                        assert_eq!(sum, 200_010_000);
                        completed += 1;
                    }
                    Err(error) => {
                        assert_eq!(sql_error(error), "LIMIT_EXCEEDED");
                        break;
                    }
                }
            }
            assert!(completed > 0 && completed < 100);
            assert!(vm.expired());
        }

        #[test]
        fn architecture_proposal_busy_retry_respects_request_and_sql_cutoffs() {
            let now = Instant::now();
            let quarter_ms = Duration::from_micros(250);
            assert_eq!(
                busy_delay(MAX_BUSY, now + Duration::from_secs(1), now),
                BUSY_STEP
            );
            assert_eq!(busy_delay(MAX_BUSY, now + quarter_ms, now), quarter_ms);
            assert_eq!(busy_delay(quarter_ms, now + BUSY_STEP, now), quarter_ms);
            assert_eq!(
                busy_delay(Duration::ZERO, now + BUSY_STEP, now),
                Duration::ZERO
            );
            assert_eq!(busy_delay(MAX_BUSY, now, now), Duration::ZERO);
        }
    }
}

pub(super) fn observe(
    request: &ProposalRequest,
    cwd: &Path,
    head: ObjectId,
    deadline: Instant,
) -> Result<Snapshot, &'static str> {
    #[cfg(any(
        windows,
        all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")
    ))]
    return native::observe(request, cwd, head, deadline);
    #[cfg(not(any(
        windows,
        all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")
    )))]
    {
        let _ = (request, cwd, head, deadline);
        Err("STORAGE_UNAVAILABLE")
    }
}
