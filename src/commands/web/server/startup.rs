//! Server binding and startup helpers.

use miette::{IntoDiagnostic, Result, miette};
use std::net::SocketAddr;
use std::path::Path;
use tokio::net::TcpListener;

/// Bind a TCP listener and serve the router until SIGINT.
///
/// Uses `into_make_service_with_connect_info` so middleware can attribute
/// rate limits and peer allowlist checks to the real peer IP (RT-W3).
pub async fn serve(router: axum::Router, bind: String, port: u16) -> Result<()> {
    let addr = SocketAddr::new(
        bind.parse()
            .map_err(|e| miette!("Invalid bind address {}: {}", bind, e))?,
        port,
    );

    let listener = TcpListener::bind(addr).await.into_diagnostic()?;
    tracing::info!("ledgerful web listening on {}", addr);

    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .into_diagnostic()?;

    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

/// Open a SQLite connection to the ledger with concurrency-safe pragmas.
pub(crate) fn open_ledger_connection(path: &Path) -> Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(path).into_diagnostic()?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA busy_timeout = 5000;",
    )
    .into_diagnostic()?;
    Ok(conn)
}
