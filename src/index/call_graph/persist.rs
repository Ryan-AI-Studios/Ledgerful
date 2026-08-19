use miette::IntoDiagnostic;

/// Internal struct for accumulating edge rows before batch insert.
pub(crate) struct EdgeRow {
    pub(crate) caller_symbol_id: i64,
    pub(crate) caller_file_id: i64,
    pub(crate) callee_symbol_id: Option<i64>,
    pub(crate) callee_file_id: Option<i64>,
    pub(crate) unresolved_callee: Option<String>,
    pub(crate) call_kind: String,
    pub(crate) resolution_status: String,
    pub(crate) confidence: f64,
    pub(crate) evidence: String,
    pub(crate) public_priority: bool,
}

pub(crate) fn insert_edge_batch(
    tx: &rusqlite::Transaction<'_>,
    edges: &[EdgeRow],
) -> miette::Result<()> {
    for edge in edges {
        tx.execute(
            "INSERT INTO structural_edges \
             (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id, \
              unresolved_callee, call_kind, resolution_status, confidence, evidence) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                edge.caller_symbol_id,
                edge.caller_file_id,
                edge.callee_symbol_id,
                edge.callee_file_id,
                edge.unresolved_callee,
                edge.call_kind,
                edge.resolution_status,
                edge.confidence,
                edge.evidence,
            ],
        )
        .into_diagnostic()?;
    }
    Ok(())
}

pub(crate) fn clear_native_structural_edges(tx: &rusqlite::Transaction<'_>) -> miette::Result<()> {
    tx.execute(
        "DELETE FROM structural_edges WHERE evidence NOT LIKE 'scip:%'",
        [],
    )
    .into_diagnostic()?;
    Ok(())
}
