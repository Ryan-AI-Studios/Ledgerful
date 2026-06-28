use super::ProjectIndexer;
use crate::index::call_graph::CallGraphBuilder;
use crate::index::ci_gates::CIGateExtractor;
use crate::index::data_models::DataModelExtractor;
use crate::index::env_schema::EnvSchemaIndexer;
use crate::index::observability::ObservabilityExtractor;
use crate::index::routes::RouteExtractor;
use crate::index::test_mapping::TestMapper;
use miette::{IntoDiagnostic, Result};
use std::path::PathBuf;

pub fn build_call_graph(
    indexer: &ProjectIndexer,
) -> Result<crate::index::call_graph::CallGraphStats> {
    CallGraphBuilder::new(
        &indexer.storage,
        indexer.repo_path.as_std_path().to_path_buf(),
    )
    .build()
}

pub fn extract_routes(indexer: &ProjectIndexer) -> Result<crate::index::routes::RouteStats> {
    RouteExtractor::new(
        &indexer.storage,
        indexer.repo_path.as_std_path().to_path_buf(),
    )
    .extract()
}

pub fn clear_routes(indexer: &ProjectIndexer, file_ids: &[i64]) -> Result<()> {
    RouteExtractor::new(
        &indexer.storage,
        indexer.repo_path.as_std_path().to_path_buf(),
    )
    .clear_routes(file_ids)
}

pub fn clear_structural_edges(indexer: &ProjectIndexer, file_ids: &[i64]) -> Result<()> {
    if file_ids.is_empty() {
        return Ok(());
    }
    let conn = indexer.storage.get_connection();
    for &fid in file_ids {
        conn.execute(
            "DELETE FROM structural_edges WHERE caller_file_id = ?1",
            [fid],
        )
        .into_diagnostic()?;
    }
    Ok(())
}

pub fn extract_data_models(
    indexer: &ProjectIndexer,
) -> Result<crate::index::data_models::DataModelStats> {
    DataModelExtractor::new(
        &indexer.storage,
        indexer.repo_path.as_std_path().to_path_buf(),
    )
    .extract()
}

pub fn clear_data_models(indexer: &ProjectIndexer, file_ids: &[i64]) -> Result<()> {
    DataModelExtractor::new(
        &indexer.storage,
        indexer.repo_path.as_std_path().to_path_buf(),
    )
    .clear_data_models(file_ids)
}

pub fn extract_observability(
    indexer: &ProjectIndexer,
) -> Result<crate::index::observability::ObservabilityStats> {
    ObservabilityExtractor::new(
        &indexer.storage,
        indexer.repo_path.as_std_path().to_path_buf(),
    )
    .extract()
}

pub fn extract_test_mappings(
    indexer: &ProjectIndexer,
) -> Result<crate::index::test_mapping::TestMappingStats> {
    TestMapper::new(
        &indexer.storage,
        indexer.repo_path.as_std_path().to_path_buf(),
    )
    .extract()
}

pub fn extract_ci_gates(indexer: &ProjectIndexer) -> Result<crate::index::ci_gates::CIGateStats> {
    CIGateExtractor::new(
        &indexer.storage,
        indexer.repo_path.as_std_path().to_path_buf(),
    )
    .extract()
}

pub fn extract_env_schema(
    indexer: &ProjectIndexer,
) -> Result<crate::index::env_schema::EnvSchemaStats> {
    EnvSchemaIndexer::new(
        &indexer.storage,
        indexer.repo_path.as_std_path().to_path_buf(),
    )
    .extract()
}

pub fn get_all_call_edges(
    indexer: &ProjectIndexer,
) -> Result<Vec<crate::index::call_graph::CallEdge>> {
    use crate::index::call_graph::{CallEdge, CallKind, ResolutionStatus};
    let conn = indexer.storage.get_connection();
    let mut stmt = conn.prepare("SELECT COALESCE(ps_caller.qualified_name, ps_caller.symbol_name), pf_caller.file_path, COALESCE(ps_callee.qualified_name, ps_callee.symbol_name), pf_callee.file_path, se.call_kind, se.resolution_status, se.confidence, se.evidence FROM structural_edges se JOIN project_symbols ps_caller ON se.caller_symbol_id = ps_caller.id JOIN project_files pf_caller ON se.caller_file_id = pf_caller.id LEFT JOIN project_symbols ps_callee ON se.callee_symbol_id = ps_callee.id LEFT JOIN project_files pf_callee ON se.callee_file_id = pf_callee.id").into_diagnostic()?;
    let edges = stmt
        .query_map([], |row| {
            Ok(CallEdge {
                caller_name: row.get(0)?,
                caller_file: PathBuf::from(row.get::<_, String>(1)?),
                callee_name: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                callee_file: row.get::<_, Option<String>>(3)?.map(PathBuf::from),
                call_kind: match row.get::<_, String>(4)?.as_str() {
                    "METHOD_CALL" => CallKind::MethodCall,
                    "TRAIT_DISPATCH" => CallKind::TraitDispatch,
                    "DYNAMIC" => CallKind::Dynamic,
                    "EXTERNAL" => CallKind::External,
                    _ => CallKind::Direct,
                },
                resolution_status: match row.get::<_, String>(5)?.as_str() {
                    "AMBIGUOUS" => ResolutionStatus::Ambiguous,
                    "UNRESOLVED" => ResolutionStatus::Unresolved,
                    "CAPPED" => ResolutionStatus::Capped,
                    _ => ResolutionStatus::Resolved,
                },
                confidence: row.get(6)?,
                evidence: row.get(7)?,
            })
        })
        .into_diagnostic()?
        .collect::<Result<Vec<_>, _>>()
        .into_diagnostic()?;
    Ok(edges)
}
