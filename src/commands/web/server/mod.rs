//! Web dashboard axum router and server submodules.

pub mod csp;
pub mod git;
pub mod handlers;
pub mod health;
pub mod middleware;
pub mod router;
pub mod sse;
pub mod startup;

pub use handlers::{
    __path_changes_handler, __path_config_handler, __path_health_handler, __path_hotspots_handler,
    __path_ledger_handler, __path_ledger_search_handler, __path_ledger_tx_handler,
    __path_projects_handler, __path_session_handler, __path_snapshot_handler,
    __path_status_handler, __path_sync_status_handler,
};
pub use router::router;
pub use sse::{__path_events_handler, spawn_change_detector};
pub use startup::{make_connect_info_service, serve, serve_with_shutdown};
