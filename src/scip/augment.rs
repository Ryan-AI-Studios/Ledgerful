//! SCIP augment path: resolve definitions to native symbols and add reference edges.
//!
//! **Does not write `project_symbols`** (0095). Native index is the floor; SCIP
//! only adds/updates `structural_edges` with `evidence = scip:ref`.

use crate::index::rows::get_file_id_by_path;
use crate::scip::edges::{ScipEdgeStats, augment_edges_from_scip};
use crate::scip::{ScipIndex, is_scip_stale, normalize_scip_path, register_scip_index};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Outcome of a SCIP augment attempt (always explicit — never omit for "nothing").
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScipRunStatus {
    /// Neither `--auto-scip` nor `--scip` was requested.
    DidNotRun,
    /// Indexer not available / generation failed / ingest failed (native continues).
    Failed,
    /// Loaded index was already registered and hash-matched (skipped).
    SkippedStale,
    /// Edges were (re)applied successfully.
    Success,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScipIndexJson {
    pub status: ScipRunStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edges_added: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edges_updated: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definitions_mapped: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_skipped: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ScipIndexJson {
    pub fn did_not_run() -> Self {
        Self {
            status: ScipRunStatus::DidNotRun,
            edges_added: None,
            edges_updated: None,
            definitions_mapped: None,
            files_skipped: None,
            message: Some("SCIP augment not requested".to_string()),
        }
    }

    pub fn failed(msg: impl Into<String>) -> Self {
        Self {
            status: ScipRunStatus::Failed,
            edges_added: None,
            edges_updated: None,
            definitions_mapped: None,
            files_skipped: None,
            message: Some(msg.into()),
        }
    }

    pub fn from_stats(stats: &ScipEdgeStats) -> Self {
        Self {
            status: ScipRunStatus::Success,
            edges_added: Some(stats.edges_added),
            edges_updated: Some(stats.edges_updated),
            definitions_mapped: Some(stats.definitions_mapped),
            files_skipped: Some(stats.files_skipped),
            message: None,
        }
    }

    pub fn skipped_stale() -> Self {
        Self {
            status: ScipRunStatus::SkippedStale,
            edges_added: Some(0),
            edges_updated: Some(0),
            definitions_mapped: None,
            files_skipped: None,
            message: Some("SCIP index hash unchanged; ingestion skipped".to_string()),
        }
    }
}

/// Detect/generate or use path, run edge augment, clean up temp file.
///
/// On any failure: warn + return Failed (never aborts the native index).
pub fn maybe_run_scip_augment(
    layout: &Layout,
    storage: &mut StorageManager,
    config: &crate::config::model::Config,
    auto_scip: bool,
    scip_path: Option<PathBuf>,
) -> ScipIndexJson {
    if !auto_scip && scip_path.is_none() {
        return ScipIndexJson::did_not_run();
    }

    let policy = config.verify.effective_process_policy();
    let repo_root = layout.root.as_std_path();

    let (path, cleanup_temp) = if let Some(p) = scip_path {
        (p, false)
    } else {
        match crate::scip::orchestrator::ScipToolchain::detect(repo_root) {
            Some(toolchain) => match toolchain.generate(repo_root, &policy) {
                Ok(p) => {
                    info!("Automatically generated SCIP index at {:?}", p);
                    let is_temp = p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n == "ledgerful.temp.scip");
                    (p, is_temp)
                }
                Err(e) => {
                    warn!("SCIP generation failed: {e}. Continuing with native index only.");
                    return ScipIndexJson::failed(format!("generation failed: {e}"));
                }
            },
            None => {
                warn!(
                    "No capable SCIP indexer found (capability probe). Continuing with native index only."
                );
                return ScipIndexJson::failed("no capable SCIP indexer".to_string());
            }
        }
    };

    let result = execute_scip_index(layout, storage, path.clone());

    if cleanup_temp && path.exists() {
        let _ = std::fs::remove_file(&path);
    }

    match result {
        Ok(json) => json,
        Err(e) => {
            warn!("SCIP ingestion failed: {e}. Continuing with native index only.");
            ScipIndexJson::failed(format!("ingestion failed: {e}"))
        }
    }
}

/// Ingest a SCIP index as structural_edges on native symbol ids only.
pub fn execute_scip_index(
    layout: &Layout,
    storage: &mut StorageManager,
    scip_path: PathBuf,
) -> Result<ScipIndexJson> {
    info!(
        "Ingesting SCIP index from {:?} (edges-only augment)",
        scip_path
    );
    let scip_index = ScipIndex::load(&scip_path)?;

    let conn = storage.get_connection();
    if !is_scip_stale(conn, &scip_path, &scip_index.file_hash)? {
        info!("SCIP index is up to date, skipping ingestion.");
        return Ok(ScipIndexJson::skipped_stale());
    }

    let root = layout.root.as_std_path();
    let path_resolver = |rel: &str| -> Option<i64> {
        let normalized = match normalize_scip_path(root, rel) {
            Ok(p) => path_to_db_string(&p),
            Err(e) => {
                warn!("Failed to normalize SCIP path {rel}: {e}");
                return None;
            }
        };
        get_file_id_by_path(conn, &normalized).ok()
    };

    let stats = augment_edges_from_scip(conn, &scip_index.index.documents, &path_resolver)?;

    let conn_mut = storage.get_connection_mut();
    let tx = conn_mut.unchecked_transaction().into_diagnostic()?;
    register_scip_index(&tx, &scip_path, &scip_index.file_hash)?;
    tx.commit().into_diagnostic()?;

    info!(
        "SCIP augment complete: {} edges added, {} updated, {} defs mapped, {} files skipped",
        stats.edges_added, stats.edges_updated, stats.definitions_mapped, stats.files_skipped
    );

    Ok(ScipIndexJson::from_stats(&stats))
}

fn path_to_db_string(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scip::resolver::SCIP_EDGE_EVIDENCE;

    #[test]
    fn did_not_run_is_explicit_status() {
        let j = ScipIndexJson::did_not_run();
        assert!(matches!(j.status, ScipRunStatus::DidNotRun));
        assert!(j.message.is_some());
    }

    #[test]
    fn scip_evidence_marker_is_distinguishable() {
        assert!(SCIP_EDGE_EVIDENCE.starts_with("scip:"));
        assert_ne!(SCIP_EDGE_EVIDENCE, "call_expr:foo");
    }
}
