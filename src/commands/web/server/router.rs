//! Axum router construction for the Ledgerful web dashboard.

use crate::commands::web::api;
use crate::commands::web::server::handlers;
use crate::commands::web::server::middleware::{
    csp_header_middleware, host_validation_layer, local_cors, peer_allowlist_layer,
    rate_limit_layer, security_headers_middleware, server_header_middleware, token_layer,
};
use crate::commands::web::server::sse;
use crate::commands::web::state::AppState;
use axum::Router;
use axum::middleware;
use axum::routing::{get, post};
use std::sync::Arc;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;

/// Build the axum router for the Ledgerful web dashboard.
pub fn router(state: Arc<AppState>) -> Router {
    // `/events` is nested inside `api_router` so it inherits `token_layer`.
    // Do not attach it to the outer router (would skip Bearer auth).
    let api_router = Router::new()
        .route("/session", get(handlers::session_handler))
        .route("/snapshot", get(handlers::snapshot_handler))
        .route("/status", get(handlers::status_handler))
        .route("/events", get(sse::events_handler))
        .route("/projects", get(handlers::projects_handler))
        .route("/ledger", get(handlers::ledger_handler))
        .route("/ledger/search", get(handlers::ledger_search_handler))
        .route("/ledger/{tx_id}", get(handlers::ledger_tx_handler))
        .route("/changes", get(handlers::changes_handler))
        .route("/hotspots", get(handlers::hotspots_handler))
        .route("/hotspots/trend", get(api::hotspots_trend_handler))
        .route("/trends", get(api::trends_handler))
        .route(
            "/reports/latest-impact.json",
            get(api::latest_impact_handler),
        )
        .route(
            "/reports/latest-verify.json",
            get(api::latest_verify_handler),
        )
        .route("/verify/health", get(api::verify_health_handler))
        .route("/verify/history", get(api::verify_history_handler))
        .route("/verify/steps", get(api::verify_steps_handler))
        .route("/compliance/summary", get(api::compliance_summary_handler))
        .route(
            "/compliance/signatures",
            get(api::compliance_signatures_handler),
        )
        .route("/compliance/export", get(api::compliance_export_handler))
        .route("/endpoints/changed", get(api::endpoints_changed_handler))
        .route(
            "/security/boundaries",
            get(api::security_boundaries_handler),
        )
        .route("/knowledge-graph", get(api::knowledge_graph_handler))
        .route("/config", get(handlers::config_handler))
        .route("/sync/status", get(handlers::sync_status_handler))
        // Apply token_layer to protected routes only — must complete before
        // merge so the public exchange route does not inherit this layer.
        .route_layer(middleware::from_fn_with_state(state.clone(), token_layer));

    // Public API routes under `/api` without Bearer auth (track 0090).
    // Merged after `route_layer` so they stay unauthenticated while protected
    // routes retain `token_layer` (axum merge-separately-middleware idiom).
    let api_public = Router::new().route(
        "/session/exchange",
        post(handlers::session_exchange_handler),
    );

    let mut app = Router::new()
        .route("/health", get(handlers::health_handler))
        .nest("/api", api_router.merge(api_public));

    if let Some(spa_dir) = &state.spa_dir {
        // Hashed assets under `/_next/` must 404 when missing — never fall back to
        // index.html. Returning HTML for `*.css`/`*.js` breaks MIME checks and
        // leaves the browser on a half-dead SPA after a rebuild (stale chunk names
        // in a cached HTML shell). Page routes still use index.html fallback.
        let next_dir = spa_dir.join("_next");
        let page_fallback = ServeFile::new(spa_dir.join("index.html").as_std_path());
        let pages = ServeDir::new(spa_dir.as_std_path()).fallback(page_fallback);
        app = app
            .nest_service("/_next", ServeDir::new(next_dir.as_std_path()))
            .fallback_service(pages);
    } else {
        app = app.fallback(get(handlers::embedded_spa_handler));
    }

    // CSP is state-aware (embedded hashes vs --spa-dir sidecar/fallback).
    // Security headers never include HSTS on the daemon path.
    app.layer(middleware::from_fn_with_state(
        state.clone(),
        csp_header_middleware,
    ))
    .layer(middleware::from_fn(security_headers_middleware))
    .layer(middleware::from_fn(server_header_middleware))
    .layer(middleware::from_fn_with_state(
        state.clone(),
        rate_limit_layer,
    ))
    .layer(
        TraceLayer::new_for_http()
            .make_span_with(
                DefaultMakeSpan::new()
                    .level(Level::INFO)
                    .include_headers(false),
            )
            .on_response(
                DefaultOnResponse::new()
                    .level(Level::INFO)
                    .include_headers(false),
            ),
    )
    .layer(local_cors())
    // Peer allowlist (public mode) then Host rebinding defense.
    .layer(middleware::from_fn_with_state(
        state.clone(),
        peer_allowlist_layer,
    ))
    .layer(middleware::from_fn(host_validation_layer))
    .with_state(state)
}
