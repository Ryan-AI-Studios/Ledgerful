//! Axum router construction for the Ledgerful web dashboard.

use crate::commands::web::api;
use crate::commands::web::server::handlers;
use crate::commands::web::server::middleware::{
    csp_header_middleware, host_validation_layer, local_cors, peer_allowlist_layer,
    rate_limit_layer, server_header_middleware, token_layer,
};
use crate::commands::web::state::AppState;
use axum::Router;
use axum::middleware;
use axum::routing::get;
use std::sync::Arc;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;

/// Build the axum router for the Ledgerful web dashboard.
pub fn router(state: Arc<AppState>) -> Router {
    let api_router = Router::new()
        .route("/session", get(handlers::session_handler))
        .route("/snapshot", get(handlers::snapshot_handler))
        .route("/status", get(handlers::status_handler))
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
        .route_layer(middleware::from_fn_with_state(state.clone(), token_layer));

    let mut app = Router::new()
        .route("/health", get(handlers::health_handler))
        .nest("/api", api_router);

    if let Some(spa_dir) = &state.spa_dir {
        let fallback = ServeFile::new(spa_dir.join("index.html").as_std_path());
        app = app.fallback_service(ServeDir::new(spa_dir.as_std_path()).fallback(fallback));
    } else {
        app = app.fallback(get(handlers::embedded_spa_handler));
    }

    app.layer(middleware::from_fn(csp_header_middleware))
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
