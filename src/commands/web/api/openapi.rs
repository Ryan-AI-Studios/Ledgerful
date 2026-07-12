//! OpenAPI document generation for the Ledgerful web dashboard.

use crate::contracts::AffectedContract;

#[cfg(any(test, feature = "openapi", feature = "web"))]
use utoipa::OpenApi;

#[cfg(any(test, feature = "openapi", feature = "web"))]
use crate::commands::web::server::{
    __path_changes_handler, __path_config_handler, __path_health_handler, __path_hotspots_handler,
    __path_ledger_handler, __path_ledger_search_handler, __path_ledger_tx_handler,
    __path_projects_handler, __path_session_handler, __path_snapshot_handler,
    __path_status_handler, __path_sync_status_handler,
};

#[cfg(any(test, feature = "openapi", feature = "web"))]
use crate::commands::web::api::handlers::{
    __path_compliance_export_handler, __path_compliance_signatures_handler,
    __path_compliance_summary_handler, __path_endpoints_changed_handler,
    __path_hotspots_trend_handler, __path_knowledge_graph_handler, __path_latest_impact_handler,
    __path_latest_verify_handler, __path_security_boundaries_handler, __path_trends_handler,
    __path_verify_health_handler, __path_verify_history_handler, __path_verify_steps_handler,
};

// Re-export shared types so the utoipa `#[openapi(components(schemas(...)))]`
// macro can resolve identifiers that live in `web::types`. They are declared
// `pub(crate)` there, so we re-export them at crate visibility.
pub(crate) use crate::commands::web::types::{
    ChangeResponse, ChangedFileResponse, ChangesQuery, ComplianceSignatureEntry,
    ComplianceSummaryResponse, ConfigResponse, HotspotResponse, HotspotTrendQuery,
    HotspotTrendResponse, HotspotTrendSeries, HotspotsQueryParams, KgEdge, KgNode,
    KnowledgeGraphQuery, KnowledgeGraphResponse, LedgerDetailResponse, LedgerEntryResponse,
    LedgerListQuery, LedgerSearchQuery, ProjectResponse, SecurityBoundariesResponse,
    SnapshotResponse, StatusResponse, SyncStatusResponse, TrendPointDto, TrendsQuery,
    TrendsResponse, UserSession, VerificationHealthResponse, VerificationStepResponse,
    VerificationTrendPoint, VerifyHistoryQuery,
};

#[cfg(any(test, feature = "openapi", feature = "web"))]
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Ledgerful Daemon API",
        version = "0.1.8",
        description = "Machine-readable OpenAPI contract for the Ledgerful daemon `/api/*` endpoints. Generated from the Rust DTOs via utoipa."
    ),
    paths(
        health_handler,
        session_handler,
        snapshot_handler,
        status_handler,
        projects_handler,
        ledger_handler,
        ledger_search_handler,
        ledger_tx_handler,
        changes_handler,
        hotspots_handler,
        config_handler,
        hotspots_trend_handler,
        latest_impact_handler,
        latest_verify_handler,
        verify_health_handler,
        verify_history_handler,
        verify_steps_handler,
        compliance_summary_handler,
        compliance_signatures_handler,
        compliance_export_handler,
        endpoints_changed_handler,
        security_boundaries_handler,
        knowledge_graph_handler,
        trends_handler,
        sync_status_handler
    ),
    components(schemas(
        UserSession,
        SnapshotResponse,
        StatusResponse,
        ProjectResponse,
        LedgerEntryResponse,
        LedgerDetailResponse,
        ChangedFileResponse,
        LedgerListQuery,
        LedgerSearchQuery,
        ChangesQuery,
        ChangeResponse,
        HotspotsQueryParams,
        ConfigResponse,
        HotspotResponse,
        HotspotTrendQuery,
        HotspotTrendResponse,
        HotspotTrendSeries,
        VerificationHealthResponse,
        VerifyHistoryQuery,
        VerificationTrendPoint,
        VerificationStepResponse,
        ComplianceSummaryResponse,
        ComplianceSignatureEntry,
        AffectedContract,
        SecurityBoundariesResponse,
        KnowledgeGraphQuery,
        KnowledgeGraphResponse,
        KgNode,
        KgEdge,
        SyncStatusResponse,
        TrendsQuery,
        TrendsResponse,
        TrendPointDto,
        crate::commands::web::error::ProblemDetail
    )),
    tags(
        (name = "health", description = "Daemon liveness"),
        (name = "session", description = "Current user session"),
        (name = "snapshot", description = "Summary metrics"),
        (name = "status", description = "Daemon health status"),
        (name = "projects", description = "Project list"),
        (name = "ledger", description = "Ledger transactions"),
        (name = "changes", description = "Recent changes"),
        (name = "hotspots", description = "Hotspot rankings and trends"),
        (name = "reports", description = "Latest impact/verify report JSON"),
        (name = "verify", description = "Verification health/history/steps"),
        (name = "compliance", description = "Compliance summary/signatures/export"),
        (name = "endpoints", description = "Affected API contracts"),
        (name = "security", description = "Security boundaries"),
        (name = "knowledge-graph", description = "CozoDB knowledge-graph subgraph"),
        (name = "config", description = "Daemon configuration"),
        (name = "trends", description = "Project-level trend series"),
        (name = "sync", description = "Local M0 sync state")
    )
)]
pub struct ApiDoc;

/// Generate the canonical OpenAPI JSON string for this build.
#[cfg(any(test, feature = "openapi", feature = "web"))]
pub fn generate_openapi_json() -> String {
    use utoipa::OpenApi;
    ApiDoc::openapi().to_pretty_json().unwrap_or_else(|e| {
        tracing::error!("OpenAPI serialization failed: {e}");
        String::from("{}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_openapi_json_produces_valid_json() {
        let json = generate_openapi_json();
        assert!(json.starts_with('{'));
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("OpenAPI JSON parses");
        assert!(parsed.get("openapi").is_some());
    }
}
