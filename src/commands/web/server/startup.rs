//! Server binding and startup helpers.

use crate::commands::web::server::sse::{DETECTOR_TICK, spawn_change_detector};
use crate::commands::web::state::AppState;
use miette::{IntoDiagnostic, Result, miette};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// Production make-service: injects real peer [`SocketAddr`] as
/// [`axum::extract::ConnectInfo`] so rate-limit and peer-allowlist layers
/// attribute correctly (RT-W3). Factored so tests cannot drift from `serve`.
/// Public so integration harnesses share the exact production ConnectInfo wiring.
pub fn make_connect_info_service(
    router: axum::Router,
) -> axum::extract::connect_info::IntoMakeServiceWithConnectInfo<axum::Router, SocketAddr> {
    router.into_make_service_with_connect_info::<SocketAddr>()
}

/// Bind a TCP listener and serve the router until SIGINT (or external cancel).
///
/// Spawns the ledger change detector and signals SSE streams to terminate on
/// graceful shutdown so open connections do not hang the process (track 0085).
///
/// Uses [`make_connect_info_service`] so middleware can attribute rate limits
/// and peer allowlist checks to the real peer IP (RT-W3).
pub async fn serve(
    router: axum::Router,
    bind: String,
    port: u16,
    state: Arc<AppState>,
) -> Result<()> {
    serve_with_shutdown(router, bind, port, state, None).await
}

/// Like [`serve`], but accepts an optional oneshot that triggers the same
/// graceful-shutdown path as Ctrl+C (used by integration tests for DoD-6).
pub async fn serve_with_shutdown(
    router: axum::Router,
    bind: String,
    port: u16,
    state: Arc<AppState>,
    external_shutdown: Option<oneshot::Receiver<()>>,
) -> Result<()> {
    let addr = SocketAddr::new(
        bind.parse()
            .map_err(|e| miette!("Invalid bind address {}: {}", bind, e))?,
        port,
    );

    let listener = TcpListener::bind(addr).await.into_diagnostic()?;
    let bound = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| addr.to_string());
    // 0154: product bind notice must survive default WARN floor (not tracing INFO).
    println!("ledgerful web listening on {}", bound);

    serve_listener(listener, router, state, external_shutdown).await
}

/// Serve on an already-bound [`TcpListener`] (production and tests share this path).
///
/// Spawns the change detector and wires graceful shutdown the same way as
/// [`serve_with_shutdown`]. Tests bind `127.0.0.1:0`, read `local_addr`, then
/// call this so DoD-6 exercises production shutdown wiring (not a fork).
pub async fn serve_listener(
    listener: TcpListener,
    router: axum::Router,
    state: Arc<AppState>,
    external_shutdown: Option<oneshot::Receiver<()>>,
) -> Result<()> {
    // Detector is owned by the server lifetime; cancelled via shutdown watch.
    let _detector = spawn_change_detector(
        state.layout.clone(),
        state.event_tx.clone(),
        state.shutdown_rx(),
        DETECTOR_TICK,
    );

    let state_for_shutdown = state.clone();
    axum::serve(listener, make_connect_info_service(router))
        .with_graceful_shutdown(async move {
            wait_for_shutdown_trigger(external_shutdown).await;
            // Tell SSE streams + detector to end so with_graceful_shutdown
            // is not blocked by never-ending request bodies (DoD-6).
            state_for_shutdown.signal_shutdown();
            // Brief grace for streams to observe the watch and complete.
            tokio::time::sleep(Duration::from_millis(150)).await;
        })
        .await
        .into_diagnostic()?;

    Ok(())
}

async fn wait_for_shutdown_trigger(external: Option<oneshot::Receiver<()>>) {
    match external {
        Some(rx) => {
            tokio::select! {
                _ = shutdown_signal() => {}
                _ = rx => {}
            }
        }
        None => shutdown_signal().await,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::extract::ConnectInfo;
    use axum::http::{Request, StatusCode};
    use axum::middleware::{self, Next};
    use axum::response::Response;
    use axum::routing::get;
    use std::net::SocketAddr;

    /// Middleware that fails closed if ConnectInfo is missing — proves the
    /// production make-service injects peer address into request extensions.
    async fn require_connect_info(
        request: Request<axum::body::Body>,
        next: Next,
    ) -> Result<Response, StatusCode> {
        if request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .is_none()
        {
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        Ok(next.run(request).await)
    }

    #[tokio::test]
    async fn make_connect_info_service_injects_peer_into_extensions() {
        let app = Router::new()
            .route("/probe", get(|| async { "ok" }))
            .layer(middleware::from_fn(require_connect_info));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, make_connect_info_service(app))
                .await
                .unwrap();
        });

        let client = reqwest::Client::new();
        let status = client
            .get(format!("http://{addr}/probe"))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(
            status,
            StatusCode::OK,
            "ConnectInfo must be present on the production make-service path"
        );
        handle.abort();
    }

    #[tokio::test]
    async fn plain_into_make_service_does_not_inject_connect_info() {
        // Control: without into_make_service_with_connect_info, ConnectInfo is
        // absent and require_connect_info returns 500 — so the production
        // helper is load-bearing, not cosmetic.
        let app = Router::new()
            .route("/probe", get(|| async { "ok" }))
            .layer(middleware::from_fn(require_connect_info));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .unwrap();
        });

        let client = reqwest::Client::new();
        let status = client
            .get(format!("http://{addr}/probe"))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "plain into_make_service must lack ConnectInfo"
        );
        handle.abort();
    }
}
