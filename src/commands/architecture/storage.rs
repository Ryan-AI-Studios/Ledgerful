//! Keep the 0601 reads in one transaction on the guarded native connection.
use chrono::Utc;
use miette::{IntoDiagnostic, Result};
use std::time::{Duration, Instant};

use crate::change_evidence::pack_store::with_architecture_reader;
use crate::commands::baseline::{BaselineError, FileEdgeSet, extract::ArchitectureSnapshot};
use crate::config::model::Config;
use crate::state::storage::StorageManager;
use crate::state::{StateError, layout::Layout, migrations::get_migrations_count};

use super::{BaselineInput, read_baseline};

pub(super) fn read_snapshots(
    layout: &Layout,
    config: &Config,
    baseline: Option<String>,
    verified_binary_startup: bool,
) -> Result<(FileEdgeSet, Option<BaselineInput>)> {
    let (captured, baseline) = with_architecture_reader(
        layout,
        verified_binary_startup,
        |mut connection, deadline| {
            if let Err(error) = bind_read_budget(&mut connection, deadline).into_diagnostic() {
                return (connection, Err(error));
            }
            StorageManager::with_architecture_reader(connection, layout, |storage| {
                let connection = storage.get_connection();
                connection
                    .pragma_update(None, "query_only", true)
                    .into_diagnostic()?;
                let transaction = connection.unchecked_transaction().into_diagnostic()?;
                let found: i64 = transaction
                    .pragma_query_value(None, "user_version", |row| row.get(0))
                    .into_diagnostic()?;
                let supported = i64::try_from(get_migrations_count()).into_diagnostic()?;
                if found < supported {
                    return Err(StateError::SchemaMismatch.into());
                }
                if found > supported {
                    return Err(BaselineError::SchemaTooNew { found, supported }.into());
                }
                let current = ArchitectureSnapshot::capture(storage, config, Utc::now())?;
                let baseline = baseline
                    .map(|name| read_baseline(storage, name))
                    .transpose()?;
                transaction.commit().into_diagnostic()?;
                Ok((current, baseline))
            })
        },
    )?;
    // The native reader closed and released custody before filesystem discovery.
    let current = captured.evaluate(&layout.root)?;
    let baseline = baseline.map(|baseline| baseline.evaluate(&current));
    Ok((current, baseline))
}

fn bind_read_budget(
    connection: &mut rusqlite::Connection,
    deadline: Instant,
) -> rusqlite::Result<()> {
    connection.busy_timeout(Duration::from_millis(100))?;
    connection.progress_handler(1_000, Some(move || Instant::now() >= deadline))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn architecture_sql_deadline_releases_the_read_transaction() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        bind_read_budget(&mut connection, Instant::now()).unwrap();
        let transaction = connection.transaction().unwrap();
        let result: rusqlite::Result<i64> = transaction.query_row(
            "WITH RECURSIVE n(i) AS (
                VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1000000
             )
             SELECT SUM(i) FROM n",
            [],
            |row| row.get(0),
        );
        assert_eq!(
            result.unwrap_err().sqlite_error_code(),
            Some(rusqlite::ErrorCode::OperationInterrupted)
        );
        transaction.rollback().unwrap();
        connection.close().unwrap();
    }
}

#[cfg(all(windows, test))]
mod native_tests {
    use super::*;
    use crate::change_evidence::pack_store::with_architecture_reader_for_native_test;
    use rusqlite::{Connection, ErrorCode};
    use std::{
        io::{BufRead, BufReader, Write},
        path::Path,
        process::{Command, Stdio},
        sync::mpsc,
        thread,
    };

    const WRITER_FIXTURE: &str = "LEDGERFUL_TEST_ARCHITECTURE_DEADLINE_WRITER";
    const WAIT: Duration = Duration::from_secs(10);
    const WRITE: &str = "BEGIN IMMEDIATE; INSERT INTO observed VALUES (2); COMMIT";

    fn independent_writer(path: &Path) {
        let connection = Connection::open(path).unwrap();
        connection.busy_timeout(Duration::ZERO).unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;
                 CREATE TABLE observed(value INTEGER NOT NULL);
                 INSERT INTO observed VALUES (1);",
            )
            .unwrap();
        let mut commands = std::io::stdin().lock().lines();
        eprintln!("architecture-writer: ready");
        assert_eq!(commands.next().unwrap().unwrap(), "during");
        let blocked = connection.execute_batch(WRITE).unwrap_err();
        assert_eq!(blocked.sqlite_error_code(), Some(ErrorCode::DatabaseBusy));
        eprintln!("architecture-writer: busy");
        assert_eq!(commands.next().unwrap().unwrap(), "after");
        connection.execute_batch(WRITE).unwrap();
        let count: i64 = connection
            .query_row("SELECT count(*) FROM observed", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
        connection.close().unwrap();
        eprintln!("architecture-writer: committed");
    }

    #[test]
    #[allow(non_snake_case)]
    fn architecture_native_sql_deadline_releases_writer_custody__slow() {
        if let Some(path) = std::env::var_os(WRITER_FIXTURE) {
            independent_writer(Path::new(&path));
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        // The reader verifies exact component spelling; avoid 8.3 temp aliases.
        let canonical = dunce::canonicalize(directory.path()).unwrap();
        let layout = Layout::new(camino::Utf8Path::from_path(&canonical).unwrap());
        std::fs::create_dir_all(layout.state_subdir()).unwrap();
        let db = layout.state_subdir().join("ledger.db");
        let test_name = format!(
            "{}::architecture_native_sql_deadline_releases_writer_custody__slow",
            module_path!().split_once("::").unwrap().1
        );
        // Test-only fixture: re-enter this harness as the independent writer. No trust
        // or security decision depends on the reported executable path.
        // nosemgrep: rust.lang.security.current-exe.current-exe
        let mut writer = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test_name, "--nocapture"])
            .env(WRITER_FIXTURE, db.as_std_path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut commands = writer.stdin.take().unwrap();
        let responses = writer.stderr.take().unwrap();
        let (send, receive) = mpsc::channel();
        let read_responses = thread::spawn(move || {
            for line in BufReader::new(responses).lines() {
                if send.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let expect = |answer| assert_eq!(receive.recv_timeout(WAIT).unwrap(), answer);
        expect("architecture-writer: ready");

        let mut interrupted = false;
        let refusal = with_architecture_reader_for_native_test(
            &layout,
            Duration::from_secs(2),
            |mut connection, deadline| {
                bind_read_budget(&mut connection, deadline).unwrap();
                let transaction = connection.transaction().unwrap();
                let value: i64 = transaction
                    .query_row("SELECT value FROM observed", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(value, 1, "the native reader must see the WAL row");
                writeln!(commands, "during").unwrap();
                expect("architecture-writer: busy");

                let query: rusqlite::Result<i64> = transaction.query_row(
                    "WITH RECURSIVE n(i) AS (
                        VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1000000000
                     )
                     SELECT SUM(i) FROM n",
                    [],
                    |row| row.get(0),
                );
                let error = query.unwrap_err();
                assert_eq!(
                    error.sqlite_error_code(),
                    Some(ErrorCode::OperationInterrupted)
                );
                assert!(Instant::now() >= deadline);
                interrupted = true;
                drop(transaction);
                (connection, Err::<(), _>(miette::miette!("{error}")))
            },
        );
        assert!(
            interrupted,
            "the real guarded callback must run to its deadline"
        );
        assert!(
            refusal
                .unwrap_err()
                .to_string()
                .contains("ARCHITECTURE_STORAGE_UNAVAILABLE")
        );
        writeln!(commands, "after").unwrap();
        expect("architecture-writer: committed");
        drop(commands);
        assert!(writer.wait().unwrap().success());
        read_responses.join().unwrap();
        println!(
            "native SQLite deadline: WAL row read; independent writer busy during custody; SQL interrupted; checked reader returned unavailable; independent writer committed without waiting"
        );
    }
}
