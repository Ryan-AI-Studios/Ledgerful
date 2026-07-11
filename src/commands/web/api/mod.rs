//! Additional read-only JSON API handlers for the Ledgerful web dashboard.
//!
//! These endpoints back the remaining SPA screens that were not covered by the
//! core handler set in `server.rs`: report downloads, hotspot trends, contract
//! impact, security boundaries, and the CozoDB knowledge-graph subgraph.
//!
//! This module is split into focused submodules:
//! - `openapi.rs`: OpenAPI document generation (`ApiDoc`, `generate_openapi_json`).
//! - `handlers.rs`: API request handlers and business logic helpers.
//! - `cozo.rs`: CozoDB knowledge-graph and security-boundary queries.
//! - `dto.rs`: API-specific DTO re-exports (most shared DTOs live in `web::types`).

pub mod cozo;
pub mod dto;
pub mod handlers;
pub mod openapi;

// Re-export public shared types so existing external imports keep working.
pub use crate::commands::web::types::{
    ComplianceSignatureEntry, ComplianceSummaryResponse, HotspotResponse, HotspotTrendQuery,
    HotspotTrendResponse, HotspotTrendSeries, KgEdge, KgNode, KnowledgeGraphQuery,
    KnowledgeGraphResponse, SecurityBoundariesResponse, VerificationHealthResponse,
    VerificationStepResponse, VerificationTrendPoint, VerifyHistoryQuery,
};

// Re-export the generated OpenAPI helpers and report generation entry point.
pub use openapi::generate_openapi_json;

// Re-export all API handlers so `server::router` and integration tests can keep
// importing them as `crate::commands::web::api::*`.
pub use handlers::{
    compliance_export_handler, compliance_signatures_handler, compliance_summary_handler,
    endpoints_changed_handler, hotspots_trend_handler, knowledge_graph_handler,
    latest_impact_handler, latest_verify_handler, security_boundaries_handler, trends_handler,
    verify_health_handler, verify_history_handler, verify_steps_handler,
};

use std::time::Duration;

/// Time-to-live for the cached knowledge-graph response in `AppState`.
pub(crate) const KG_CACHE_TTL: Duration = Duration::from_secs(60);

// Allow the unused import when no feature is enabled; it is required by the
// `openapi` and `web` feature builds for the `#[derive(OpenApi)]` macro.
#[allow(unused_imports)]
use openapi::ApiDoc;
