//! Server binding and startup helpers.

use miette::{IntoDiagnostic, Result, miette};
use std::net::SocketAddr;
use std::path::Path;
use tokio::net::TcpListener;

/// Production make-service: injects real peer [`SocketAddr`] as
/// [`axum::extract::ConnectInfo`] so rate-limit and peer-allowlist layers
/// attribute correctly (RT-W3). Factored so tests cannot drift from `serve`.
/// Public so integration harnesses share the exact production ConnectInfo wiring.
pub fn make_connect_info_service(
    router: axum::Router,
) -> axum::extract::connect_info::IntoMakeServiceWithConnectInfo<axum::Router, SocketAddr> {
    router.into_make_service_with_connect_info::<SocketAddr>()
}

/// Bind a TCP listener and serve the router until SIGINT.
///
/// Uses [`make_connect_info_service`] so middleware can attribute rate limits
/// and peer allowlist checks to the real peer IP (RT-W3).
pub async fn serve(router: axum::Router, bind: String, port: u16) -> Result<()> {
    let addr = SocketAddr::new(
        bind.parse()
            .map_err(|e| miette!("Invalid bind address {}: {}", bind, e))?,
        port,
    );

    let listener = TcpListener::bind(addr).await.into_diagnostic()?;
    tracing::info!("ledgerful web listening on {}", addr);

    axum::serve(listener, make_connect_info_service(router))
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
