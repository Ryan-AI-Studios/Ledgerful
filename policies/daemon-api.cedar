// Engine dogfood graph pack (0186). Not a live PDP — daemon auth remains Bearer / 0090.
// Re-index: ledgerful index --analyze-graph
// Core operator-facing session/status/snapshot/ledger/hotspots/config/security routes.
// DX1 writes policies/ledgerful-template.cedar from all indexed routes only when
// Cedar nodes are empty; this pack suppresses that prompt. Different filename.

@id("route_get_api_status")
permit (
    principal,
    action == Action::"GET /api/status",
    resource
);

@id("route_get_api_session")
permit (
    principal,
    action == Action::"GET /api/session",
    resource
);

@id("route_post_api_session_exchange")
permit (
    principal,
    action == Action::"POST /api/session/exchange",
    resource
);

@id("route_get_api_snapshot")
permit (
    principal,
    action == Action::"GET /api/snapshot",
    resource
);

@id("route_get_api_ledger")
permit (
    principal,
    action == Action::"GET /api/ledger",
    resource
);

@id("route_get_api_hotspots")
permit (
    principal,
    action == Action::"GET /api/hotspots",
    resource
);

@id("route_get_api_config")
permit (
    principal,
    action == Action::"GET /api/config",
    resource
);

@id("route_get_api_security_boundaries")
permit (
    principal,
    action == Action::"GET /api/security/boundaries",
    resource
);
