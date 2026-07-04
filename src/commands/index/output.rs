use crate::index::ServiceIndexStats;
use miette::{IntoDiagnostic, Result};

/// Bundles all index statistics for output formatting.
/// Eliminates the 15-parameter signatures on print helpers.
pub(crate) struct IndexOutputStats {
    pub(crate) stats: crate::index::orchestrator::IndexStats,
    pub(crate) doc_stats: crate::index::docs::DocIndexStats,
    pub(crate) topo_stats: crate::index::topology::TopologyIndexStats,
    pub(crate) ep_stats: crate::index::entrypoint::EntrypointStats,
    pub(crate) service_stats: ServiceIndexStats,
    pub(crate) cg_stats: crate::index::call_graph::CallGraphStats,
    pub(crate) route_stats: crate::index::routes::RouteStats,
    pub(crate) dm_stats: crate::index::data_models::DataModelStats,
    pub(crate) obs_stats: crate::index::observability::ObservabilityStats,
    pub(crate) tm_stats: crate::index::test_mapping::TestMappingStats,
    pub(crate) ci_stats: crate::index::ci_gates::CIGateStats,
    pub(crate) env_stats: crate::index::env_schema::EnvSchemaStats,
    pub(crate) cent_stats: crate::index::centrality::CentralityStats,
    pub(crate) contracts_summary: Option<crate::contracts::index::ContractsIndexSummary>,
    pub(crate) analyze_graph: bool,
}

pub(crate) fn print_json_output(output: &IndexOutputStats) -> Result<()> {
    let mut merged = serde_json::to_value(&output.stats).into_diagnostic()?;
    let doc_obj = serde_json::to_value(&output.doc_stats).into_diagnostic()?;
    let topo_obj = serde_json::to_value(&output.topo_stats).into_diagnostic()?;
    let ep_obj = serde_json::to_value(&output.ep_stats).into_diagnostic()?;
    let service_obj = serde_json::to_value(&output.service_stats).into_diagnostic()?;
    if let (Some(map), Some(doc)) = (merged.as_object_mut(), doc_obj.as_object()) {
        for (k, v) in doc {
            map.insert(format!("doc_{}", k), v.clone());
        }
    }
    if let (Some(map), Some(topo)) = (merged.as_object_mut(), topo_obj.as_object()) {
        for (k, v) in topo {
            map.insert(format!("topo_{}", k), v.clone());
        }
    }
    if let (Some(map), Some(ep)) = (merged.as_object_mut(), ep_obj.as_object()) {
        for (k, v) in ep {
            map.insert(format!("ep_{}", k), v.clone());
        }
    }
    if let (Some(map), Some(svc)) = (merged.as_object_mut(), service_obj.as_object()) {
        for (k, v) in svc {
            map.insert(format!("service_{}", k), v.clone());
        }
    }
    let cg_obj = serde_json::to_value(&output.cg_stats).into_diagnostic()?;
    if let (Some(map), Some(cg)) = (merged.as_object_mut(), cg_obj.as_object()) {
        for (k, v) in cg {
            map.insert(format!("cg_{}", k), v.clone());
        }
    }
    let route_obj = serde_json::to_value(&output.route_stats).into_diagnostic()?;
    if let (Some(map), Some(route)) = (merged.as_object_mut(), route_obj.as_object()) {
        for (k, v) in route {
            map.insert(format!("route_{}", k), v.clone());
        }
    }
    let dm_obj = serde_json::to_value(&output.dm_stats).into_diagnostic()?;
    if let (Some(map), Some(dm)) = (merged.as_object_mut(), dm_obj.as_object()) {
        for (k, v) in dm {
            map.insert(format!("dm_{}", k), v.clone());
        }
    }
    let obs_obj = serde_json::to_value(&output.obs_stats).into_diagnostic()?;
    if let (Some(map), Some(obs)) = (merged.as_object_mut(), obs_obj.as_object()) {
        for (k, v) in obs {
            map.insert(format!("obs_{}", k), v.clone());
        }
    }
    let tm_obj = serde_json::to_value(&output.tm_stats).into_diagnostic()?;
    if let (Some(map), Some(tm)) = (merged.as_object_mut(), tm_obj.as_object()) {
        for (k, v) in tm {
            map.insert(format!("tm_{}", k), v.clone());
        }
    }
    let ci_obj = serde_json::to_value(&output.ci_stats).into_diagnostic()?;
    if let (Some(map), Some(ci)) = (merged.as_object_mut(), ci_obj.as_object()) {
        for (k, v) in ci {
            map.insert(format!("ci_{}", k), v.clone());
        }
    }
    let env_obj = serde_json::to_value(&output.env_stats).into_diagnostic()?;
    if let (Some(map), Some(env)) = (merged.as_object_mut(), env_obj.as_object()) {
        for (k, v) in env {
            map.insert(format!("env_{}", k), v.clone());
        }
    }
    if output.analyze_graph {
        let cent_obj = serde_json::to_value(&output.cent_stats).into_diagnostic()?;
        if let (Some(map), Some(cent)) = (merged.as_object_mut(), cent_obj.as_object()) {
            for (k, v) in cent {
                map.insert(format!("cent_{}", k), v.clone());
            }
        }
    }
    if let Some(ref cs) = output.contracts_summary {
        let cs_obj = serde_json::to_value(cs).into_diagnostic()?;
        if let (Some(map), Some(cs)) = (merged.as_object_mut(), cs_obj.as_object()) {
            for (k, v) in cs {
                map.insert(format!("contracts_{}", k), v.clone());
            }
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&merged).into_diagnostic()?
    );
    Ok(())
}

pub(crate) fn print_human_output(output: &IndexOutputStats) {
    println!("Indexing complete:");
    println!("  Files indexed:   {}", output.stats.files_indexed);
    println!("  Symbols indexed: {}", output.stats.symbols_indexed);
    if output.stats.parse_failures > 0 {
        println!("  Parse failures:  {}", output.stats.parse_failures);
    }
    if output.stats.skipped_binary > 0 {
        println!("  Skipped binary:  {}", output.stats.skipped_binary);
    }
    if output.stats.skipped_unsupported > 0 {
        println!(
            "  Skipped unsupported: {}",
            output.stats.skipped_unsupported
        );
    }
    println!("  Duration:        {}ms", output.stats.duration_ms);
    println!();
    println!("Documentation:");
    println!("  Docs indexed:    {}", output.doc_stats.docs_indexed);
    if output.doc_stats.parse_failures > 0 {
        println!("  Doc parse failures: {}", output.doc_stats.parse_failures);
    }
    if output.doc_stats.missing_readme {
        println!("  README:          not found");
    } else {
        println!("  README:          found");
    }
    println!();
    println!("Topology:");
    println!(
        "  Directories classified: {}",
        output.topo_stats.directories_classified
    );
    if output.topo_stats.unclassified > 0 {
        println!("  Unclassified:    {}", output.topo_stats.unclassified);
    }
    let role_order = [
        crate::index::topology::DirectoryRole::Source,
        crate::index::topology::DirectoryRole::Test,
        crate::index::topology::DirectoryRole::Config,
        crate::index::topology::DirectoryRole::Infrastructure,
        crate::index::topology::DirectoryRole::Documentation,
        crate::index::topology::DirectoryRole::Generated,
        crate::index::topology::DirectoryRole::Vendor,
        crate::index::topology::DirectoryRole::BuildArtifact,
    ];
    for role in &role_order {
        if let Some(count) = output.topo_stats.role_counts.get(role) {
            println!("  {}: {}", role.as_str(), count);
        }
    }
    println!();
    println!("Entrypoints:");
    println!("  Entrypoints:   {}", output.ep_stats.entrypoints);
    println!("  Handlers:      {}", output.ep_stats.handlers);
    println!("  Public APIs:   {}", output.ep_stats.public_apis);
    println!("  Tests:         {}", output.ep_stats.tests);
    println!("  Internal:     {}", output.ep_stats.internal);
    println!();
    println!("Call Graph:");
    println!("  Edges:          {}", output.cg_stats.total_edges);
    println!("  Resolved:       {}", output.cg_stats.resolved_edges);
    println!("  Unresolved:     {}", output.cg_stats.unresolved_edges);
    println!("  Ambiguous:      {}", output.cg_stats.ambiguous_edges);
    println!("  Files processed: {}", output.cg_stats.files_processed);
    println!();
    println!("API Routes:");
    println!("  Total routes:   {}", output.route_stats.total_routes);
    if !output.route_stats.frameworks_detected.is_empty() {
        println!(
            "  Frameworks:    {}",
            output.route_stats.frameworks_detected.join(", ")
        );
    }
    println!("  Files processed: {}", output.route_stats.files_processed);
    println!();
    println!("Data Models:");
    println!("  Total models:   {}", output.dm_stats.total_models);
    println!("  Files processed: {}", output.dm_stats.files_processed);
    println!();
    println!("Observability:");
    println!("  Total patterns: {}", output.obs_stats.total_patterns);
    println!(
        "  Error handling patterns: {}",
        output.obs_stats.error_handling_patterns
    );
    println!(
        "  Telemetry patterns: {}",
        output.obs_stats.telemetry_patterns
    );
    println!("  Files processed: {}", output.obs_stats.files_processed);
    println!();
    println!("Test Mapping:");
    println!("  Total mappings: {}", output.tm_stats.total_mappings);
    println!("  Import mappings: {}", output.tm_stats.import_mappings);
    println!(
        "  Naming convention mappings: {}",
        output.tm_stats.naming_convention_mappings
    );
    println!("  Files processed: {}", output.tm_stats.files_processed);
    println!();
    println!("CI/CD Gates:");
    println!("  Total gates: {}", output.ci_stats.total_gates);
    println!("  GitHub Actions: {}", output.ci_stats.github_actions_gates);
    println!("  GitLab CI: {}", output.ci_stats.gitlab_ci_gates);
    println!("  CircleCI: {}", output.ci_stats.circleci_gates);
    println!("  Makefile: {}", output.ci_stats.makefile_gates);
    println!("  Files processed: {}", output.ci_stats.files_processed);
    println!();
    println!("Env Schema:");
    println!(
        "  Total declarations: {}",
        output.env_stats.total_declarations
    );
    println!("  Total references: {}", output.env_stats.total_references);
    println!(
        "  Dotenv declarations: {}",
        output.env_stats.dotenv_declarations
    );
    println!(
        "  Config declarations: {}",
        output.env_stats.config_declarations
    );
    println!("  Files processed: {}", output.env_stats.files_processed);
    if output.analyze_graph {
        println!();
        println!("Centrality:");
        println!("  Entry points:   {}", output.cent_stats.entry_points_count);
        println!("  Symbols computed: {}", output.cent_stats.symbols_computed);
        println!("  Max reachable:  {}", output.cent_stats.max_reachable);
    }

    if let Some(ref cs) = output.contracts_summary {
        println!();
        println!("Contracts:");
        println!("  Specs parsed:     {}", cs.specs_parsed);
        println!("  New endpoints:    {}", cs.endpoints_new);
        println!("  Skipped:          {}", cs.endpoints_skipped);
        println!("  Deleted:          {}", cs.endpoints_deleted);
    }

    println!();
    println!("Services:");
    println!(
        "  Services inferred: {}",
        output.service_stats.services_inferred
    );
    println!(
        "  Files assigned:    {}",
        output.service_stats.files_assigned
    );
}
