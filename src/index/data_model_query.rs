//! Indexed `data_models` list query (0450).
//!
//! Confidence filter, keep-best dedupe, and test-path omit. The
//! `data-models` command stays in the command layer. This is not
//! `crate::index::data_models` (source extraction).

use crate::index::test_mapping::is_test_path;
use miette::{IntoDiagnostic, Result};
use std::collections::HashMap;

/// One `data_models` row as selected for list / impact surfaces.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DataModelRow {
    pub(crate) id: i64,
    pub(crate) model_name: String,
    pub(crate) language: String,
    pub(crate) model_kind: String,
    pub(crate) confidence: f64,
    pub(crate) model_file_id: i64,
    pub(crate) file_path: String,
}

/// Dedupe key: (model_name, language, model_kind, model_file_id).
type DataModelDedupeKey = (String, String, String, i64);

/// Collapse stacked identical model identities to one row.
///
/// Keep-best: higher confidence; equal confidence keeps lower `id` (no flip).
/// After dedupe, sort name ASC, file_path ASC, language ASC, kind ASC.
pub(crate) fn dedupe_data_model_rows(rows: Vec<DataModelRow>) -> Vec<DataModelRow> {
    let mut best: HashMap<DataModelDedupeKey, DataModelRow> = HashMap::new();

    for row in rows {
        let key = (
            row.model_name.clone(),
            row.language.clone(),
            row.model_kind.clone(),
            row.model_file_id,
        );
        match best.get(&key) {
            None => {
                best.insert(key, row);
            }
            Some(prev) => {
                if data_model_row_better_than(&row, prev) {
                    best.insert(key, row);
                }
            }
        }
    }

    let mut out: Vec<DataModelRow> = best.into_values().collect();
    out.sort_by(|a, b| {
        a.model_name
            .cmp(&b.model_name)
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.language.cmp(&b.language))
            .then_with(|| a.model_kind.cmp(&b.model_kind))
    });
    out
}

/// Whether `cand` should replace `prev` under keep-best rules.
/// Strict replace-if-better only: higher confidence; equal conf keeps lower id
/// (no flip on equal confidence when cand has higher id).
pub(crate) fn data_model_row_better_than(cand: &DataModelRow, prev: &DataModelRow) -> bool {
    if cand.confidence > prev.confidence {
        return true;
    }
    if cand.confidence < prev.confidence {
        return false;
    }
    // Equal confidence: keep lower id (replace only if cand id is lower).
    cand.id < prev.id
}

/// SELECT `data_models` JOIN `project_files`, apply confidence threshold, then
/// emit-time dedupe. Mirrors endpoints `query_filter_and_dedupe_endpoints`.
pub(crate) fn query_and_dedupe_data_models(
    conn: &rusqlite::Connection,
    threshold: f64,
) -> Result<Vec<DataModelRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT dm.id, dm.model_name, dm.language, dm.model_kind, dm.confidence, \
             dm.model_file_id, pf.file_path \
             FROM data_models dm \
             INNER JOIN project_files pf ON dm.model_file_id = pf.id \
             WHERE dm.confidence >= ?1",
        )
        .into_diagnostic()?;

    let rows_iter = stmt
        .query_map([threshold], |row| {
            Ok(DataModelRow {
                id: row.get::<_, i64>(0)?,
                model_name: row.get::<_, String>(1)?,
                language: row.get::<_, String>(2)?,
                model_kind: row.get::<_, String>(3)?,
                confidence: row.get::<_, f64>(4)?,
                model_file_id: row.get::<_, i64>(5)?,
                file_path: row.get::<_, String>(6)?.replace('\\', "/"),
            })
        })
        .into_diagnostic()?;

    let mut model_rows: Vec<DataModelRow> = Vec::new();
    for row in rows_iter {
        model_rows.push(row.into_diagnostic()?);
    }
    // Dedupe after confidence filter; sort inside helper.
    Ok(dedupe_data_model_rows(model_rows))
}

pub(crate) fn omit_fixture_data_model_rows(
    rows: Vec<DataModelRow>,
    include_fixtures: bool,
) -> (Vec<DataModelRow>, usize) {
    if include_fixtures {
        return (rows, 0);
    }
    let mut kept = Vec::new();
    let mut omitted = 0usize;
    for row in rows {
        if is_test_path(&row.file_path) {
            omitted += 1;
        } else {
            kept.push(row);
        }
    }
    (kept, omitted)
}

/// Product-scope counts for `surfaces` ready: default `data-models list`
/// pipeline (confidence threshold + emit-time `is_test_path` omit).
/// Caller passes the list clap default (`0.5`), not `--all` (`0.0`).
pub(crate) fn product_data_model_counts(
    conn: &rusqlite::Connection,
    threshold: f64,
) -> Result<(usize, usize)> {
    let rows = query_and_dedupe_data_models(conn, threshold)?;
    let (kept, omitted) = omit_fixture_data_model_rows(rows, false);
    Ok((kept.len(), omitted))
}
