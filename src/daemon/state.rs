use crate::impact::packet::ImpactPacket;
use miette::{IntoDiagnostic, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::thread;
use std::time::Duration;
use tracing::warn;

pub struct QueryResult<T> {
    pub data: T,
    pub data_stale: bool,
}

pub struct ReadOnlyStorage {
    db_path: std::path::PathBuf,
}

impl ReadOnlyStorage {
    pub fn new(db_path: &Path) -> Self {
        Self {
            db_path: db_path.to_path_buf(),
        }
    }

    fn get_connection(&self) -> rusqlite::Result<Connection> {
        Connection::open_with_flags(
            &self.db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
    }

    pub fn query<F, T>(&self, f: F) -> Result<QueryResult<Option<T>>>
    where
        F: Fn(&Connection) -> rusqlite::Result<Option<T>>,
    {
        let mut backoff = Duration::from_millis(100);
        let mut attempts = 0;
        let max_attempts = 3;

        loop {
            match self.get_connection() {
                Ok(conn) => match f(&conn) {
                    Ok(data) => {
                        return Ok(QueryResult {
                            data,
                            data_stale: false,
                        });
                    }
                    Err(e) => {
                        if is_database_busy(&e) {
                            if attempts < max_attempts {
                                warn!(
                                    "Database busy, retrying in {:?} (attempt {})",
                                    backoff,
                                    attempts + 1
                                );
                                thread::sleep(backoff);
                                attempts += 1;
                                backoff *= 2;
                                continue;
                            } else {
                                warn!(
                                    "Database busy, max attempts reached. Returning stale/empty data."
                                );
                                return Ok(QueryResult {
                                    data: None,
                                    data_stale: true,
                                });
                            }
                        }
                        return Err(e).into_diagnostic();
                    }
                },
                Err(e) => {
                    if is_database_busy(&e) && attempts >= max_attempts {
                        warn!("Database busy, max attempts reached. Returning stale/empty data.");
                        return Ok(QueryResult {
                            data: None,
                            data_stale: true,
                        });
                    }

                    if attempts < max_attempts {
                        warn!(
                            "Failed to open connection (busy?), retrying in {:?} (attempt {})",
                            backoff,
                            attempts + 1
                        );
                        thread::sleep(backoff);
                        attempts += 1;
                        backoff *= 2;
                        continue;
                    }
                    return Err(e).into_diagnostic();
                }
            }
        }
    }

    pub fn get_latest_packet(&self) -> Result<QueryResult<Option<ImpactPacket>>> {
        let result = self.query(|conn| {
            let mut stmt =
                conn.prepare("SELECT packet_json FROM snapshots ORDER BY id DESC LIMIT 1")?;
            let mut rows = stmt.query([])?;

            if let Some(row) = rows.next()? {
                row.get::<_, String>(0).map(Some)
            } else {
                Ok(None)
            }
        })?;

        let data = match result.data {
            Some(json) => Some(serde_json::from_str(&json).into_diagnostic()?),
            None => None,
        };

        Ok(QueryResult {
            data,
            data_stale: result.data_stale,
        })
    }
}

fn is_database_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(err, _)
            if err.code == rusqlite::ErrorCode::DatabaseBusy
                || err.code == rusqlite::ErrorCode::DatabaseLocked
    )
}
