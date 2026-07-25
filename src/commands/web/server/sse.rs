//! Authenticated Server-Sent Events endpoint (`GET /api/events`).
//!
//! Streams narrow [`DaemonEvent`] snapshots on connect and whenever the
//! change detector observes a ledger `PRAGMA data_version` bump. Terminates
//! cleanly when [`AppState::shutdown`] is set so graceful shutdown does not
//! hang on open streams (track 0085 / DoD-6).

use crate::commands::web::server::startup::open_ledger_connection;
use crate::commands::web::state::AppState;
use crate::commands::web::types::DaemonEvent;
use crate::ledger::db::LedgerDb;
use crate::state::layout::Layout;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::{Stream, unfold};
use std::convert::Infallible;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

/// Detector poll interval (DoD-3 ≤5 s budget; DoD-5 idle cost is one pragma).
pub const DETECTOR_TICK: Duration = Duration::from_millis(500);

/// SSE comment keep-alive interval (local dashboard; low chatter).
pub const SSE_KEEPALIVE: Duration = Duration::from_secs(15);

/// Stable SSE `event:` name for daemon status payloads.
pub const DAEMON_EVENT_NAME: &str = "daemon";

/// Build a [`DaemonEvent`] from layout (drift counts + readiness files).
///
/// Mirrors the readiness bits of `handlers::compute_status` but **never**
/// probes model reachability or `is_demo`.
pub(crate) fn build_daemon_event(layout: &Layout) -> DaemonEvent {
    let (pending_transactions, unaudited_drift) = drift_counts(layout);
    let index_ready = layout
        .search_index_dir()
        .read_dir()
        .map(|mut d| d.next().is_some())
        .unwrap_or(false);
    let graph_ready = layout.state_subdir().join("ledger.cozo").exists();
    DaemonEvent {
        pending_transactions,
        unaudited_drift,
        index_ready,
        graph_ready,
    }
}

fn drift_counts(layout: &Layout) -> (usize, usize) {
    let db_path = layout.state_subdir().join("ledger.db");
    if !db_path.exists() {
        return (0, 0);
    }
    match open_ledger_connection(db_path.as_std_path()) {
        Ok(conn) => {
            let db = LedgerDb::new(&conn);
            db.drift_status_counts().unwrap_or((0, 0))
        }
        Err(e) => {
            tracing::debug!("build_daemon_event: ledger open failed: {e}");
            (0, 0)
        }
    }
}

fn event_to_sse(event: &DaemonEvent) -> Event {
    let data = serde_json::to_string(event).unwrap_or_else(|_| {
        // Extremely unlikely for this flat struct; empty object keeps the frame valid.
        "{}".to_string()
    });
    Event::default().event(DAEMON_EVENT_NAME).data(data)
}

/// `GET /api/events` — authenticated SSE stream of [`DaemonEvent`].
///
/// Wire format:
/// - `event: daemon`
/// - `data: <json DaemonEvent camelCase>`
/// - Keep-alive comments every [`SSE_KEEPALIVE`]
///
/// Auth: inherits `token_layer` (Bearer). Missing/invalid token → 403 before
/// the stream opens.
#[utoipa::path(
    get,
    path = "/api/events",
    operation_id = "getEvents",
    tag = "events",
    responses(
        (
            status = 200,
            description = "SSE stream of DaemonEvent frames (event name `daemon`)",
            content_type = "text/event-stream",
            body = DaemonEvent
        ),
        (status = 403, description = "Missing or invalid Bearer token")
    )
)]
pub async fn events_handler(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let layout = state.layout.clone();
    let rx = state.subscribe_events();
    let shutdown_rx = state.shutdown_rx();

    let stream = unfold(
        StreamPhase::Snapshot {
            layout,
            rx,
            shutdown_rx,
        },
        |phase| async move {
            match phase {
                StreamPhase::Snapshot {
                    layout,
                    rx,
                    shutdown_rx,
                } => {
                    if *shutdown_rx.borrow() {
                        return None;
                    }
                    let snap = build_daemon_event(&layout);
                    let frame = event_to_sse(&snap);
                    Some((
                        Ok(frame),
                        StreamPhase::Live {
                            layout,
                            rx,
                            shutdown_rx,
                        },
                    ))
                }
                StreamPhase::Live {
                    layout,
                    mut rx,
                    mut shutdown_rx,
                } => loop {
                    tokio::select! {
                        changed = shutdown_rx.changed() => {
                            // Sender dropped or flag set → end stream.
                            if changed.is_err() || *shutdown_rx.borrow() {
                                return None;
                            }
                        }
                        msg = rx.recv() => {
                            match msg {
                                Ok(event) => {
                                    let frame = event_to_sse(&event);
                                    return Some((
                                        Ok(frame),
                                        StreamPhase::Live {
                                            layout,
                                            rx,
                                            shutdown_rx,
                                        },
                                    ));
                                }
                                Err(RecvError::Lagged(_)) => {
                                    // Slow consumer: resync with a fresh snapshot; never kill stream.
                                    let snap = build_daemon_event(&layout);
                                    let frame = event_to_sse(&snap);
                                    return Some((
                                        Ok(frame),
                                        StreamPhase::Live {
                                            layout,
                                            rx,
                                            shutdown_rx,
                                        },
                                    ));
                                }
                                Err(RecvError::Closed) => return None,
                            }
                        }
                    }
                },
            }
        },
    );

    Sse::new(stream).keep_alive(KeepAlive::new().interval(SSE_KEEPALIVE).text("keepalive"))
}

enum StreamPhase {
    Snapshot {
        layout: Layout,
        rx: broadcast::Receiver<DaemonEvent>,
        shutdown_rx: watch::Receiver<bool>,
    },
    Live {
        layout: Layout,
        rx: broadcast::Receiver<DaemonEvent>,
        shutdown_rx: watch::Receiver<bool>,
    },
}

/// Spawn the ledger change detector. Cancels when `shutdown_rx` becomes true.
///
/// Holds one SQLite connection open and issues `PRAGMA data_version` in
/// **autocommit** each tick (no open transaction — WAL isolation trap).
pub fn spawn_change_detector(
    layout: Layout,
    event_tx: broadcast::Sender<DaemonEvent>,
    mut shutdown_rx: watch::Receiver<bool>,
    tick: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let db_path = layout.state_subdir().join("ledger.db");
        let mut conn = open_pragma_connection(db_path.as_std_path());
        let mut last_version: Option<i64> = None;

        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        tracing::debug!("change detector shutting down");
                        break;
                    }
                }
                _ = tokio::time::sleep(tick) => {
                    if *shutdown_rx.borrow() {
                        break;
                    }
                    match read_data_version(&mut conn, db_path.as_std_path()) {
                        Ok(version) => {
                            match last_version {
                                None => {
                                    // Establish baseline; do not publish (clients get connect snapshot).
                                    last_version = Some(version);
                                }
                                Some(prev) if prev == version => {
                                    // Unchanged — no ledger reads, no send (DoD-5 idle).
                                }
                                Some(_) => {
                                    last_version = Some(version);
                                    let event = build_daemon_event(&layout);
                                    // Lagged/no subscribers is fine.
                                    let _ = event_tx.send(event);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::debug!(
                                "change detector: data_version read failed (will retry): {e}"
                            );
                            // Drop and reopen next tick (missing/locked ledger.db).
                            conn = None;
                        }
                    }
                }
            }
        }
    })
}

fn open_pragma_connection(path: &Path) -> Option<rusqlite::Connection> {
    if !path.exists() {
        return None;
    }
    match open_ledger_connection(path) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::debug!("change detector: open ledger failed: {e}");
            None
        }
    }
}

/// Read `PRAGMA data_version` as `i64` in autocommit (no transaction open).
fn read_data_version(conn: &mut Option<rusqlite::Connection>, path: &Path) -> Result<i64, String> {
    if conn.is_none() {
        *conn = open_pragma_connection(path);
    }
    let Some(c) = conn.as_ref() else {
        return Err("ledger.db not available".to_string());
    };
    c.query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::tempdir;

    /// Phase 0 / §2.6: held-open read txn freezes data_version in WAL mode;
    /// autocommit sees commits from another connection.
    #[test]
    fn data_version_wal_isolation_trap() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("trap.db");

        let c1 = Connection::open(&path).unwrap();
        c1.execute_batch("PRAGMA journal_mode = WAL; CREATE TABLE t(x INTEGER);")
            .unwrap();

        // --- Under an open read transaction, other commits are invisible ---
        c1.execute_batch("BEGIN").unwrap();
        let under_txn: i64 = c1
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();

        let c2 = Connection::open(&path).unwrap();
        c2.execute("INSERT INTO t VALUES (1)", []).unwrap();

        let still_under_txn: i64 = c1
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            under_txn, still_under_txn,
            "PRAGMA data_version must NOT move under a held-open read txn when another connection commits"
        );

        c1.execute_batch("COMMIT").unwrap();
        let after_commit: i64 = c1
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();
        assert_ne!(
            under_txn, after_commit,
            "after ending the read txn, data_version should observe the foreign commit"
        );

        // --- Pure autocommit path sees foreign commits ---
        let c3 = Connection::open(&path).unwrap();
        let auto_before: i64 = c3
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();
        c2.execute("INSERT INTO t VALUES (2)", []).unwrap();
        let auto_after: i64 = c3
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();
        assert_ne!(
            auto_before, auto_after,
            "autocommit PRAGMA data_version must move when another process commits"
        );
    }

    #[test]
    fn build_daemon_event_missing_ledger_is_zero() {
        let dir = tempdir().unwrap();
        let layout = Layout::new(camino::Utf8Path::from_path(dir.path()).unwrap());
        let event = build_daemon_event(&layout);
        assert_eq!(event.pending_transactions, 0);
        assert_eq!(event.unaudited_drift, 0);
        assert!(!event.index_ready);
        assert!(!event.graph_ready);
    }

    #[test]
    fn daemon_event_serializes_camel_case() {
        let event = DaemonEvent {
            pending_transactions: 2,
            unaudited_drift: 1,
            index_ready: true,
            graph_ready: false,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("pendingTransactions"));
        assert!(json.contains("unauditedDrift"));
        assert!(json.contains("indexReady"));
        assert!(json.contains("graphReady"));
        assert!(!json.contains("model"));
        assert!(!json.contains("isDemo"));
        assert!(!json.contains("is_demo"));
    }
}
