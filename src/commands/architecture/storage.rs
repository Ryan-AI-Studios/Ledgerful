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
