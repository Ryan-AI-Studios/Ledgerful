//! SCIP augment path: resolve definitions to native symbols and add reference edges.
//!
//! **Does not write `project_symbols`** (0095). Native index is the floor; SCIP
//! only adds/updates `structural_edges` with `evidence = scip:ref`.

use crate::index::rows::get_file_id_by_path;
use crate::scip::edges::{ScipEdgeStats, augment_edges_from_scip};
use crate::scip::{ScipIndex, normalize_scip_path, register_scip_index};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Outcome of a SCIP augment attempt (always explicit — never omit for "nothing").
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScipRunStatus {
    /// Neither `--auto-scip` nor `--scip` was requested.
    DidNotRun,
    /// Indexer not available / generation failed / ingest failed (native continues).
    Failed,
    /// Reserved for API stability. Requested augment always re-applies edges
    /// (idempotent via precedence); hash is audit-only, not a skip gate.
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
    pub definitions_seen: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_skipped: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edges_skipped_enclosing_disagreement: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edges_skipped_unmapped: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edges_skipped_invalid_occ_range: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edges_skipped_duplicate: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definitions_skipped_invalid_range: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalid_enclosing_fallback: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub references_seen: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ScipIndexJson {
    fn empty_stats_fields() -> Self {
        Self {
            status: ScipRunStatus::DidNotRun,
            edges_added: None,
            edges_updated: None,
            definitions_mapped: None,
            definitions_seen: None,
            files_skipped: None,
            edges_skipped_enclosing_disagreement: None,
            edges_skipped_unmapped: None,
            edges_skipped_invalid_occ_range: None,
            edges_skipped_duplicate: None,
            definitions_skipped_invalid_range: None,
            invalid_enclosing_fallback: None,
            references_seen: None,
            message: None,
        }
    }

    pub fn did_not_run() -> Self {
        Self {
            status: ScipRunStatus::DidNotRun,
            message: Some("SCIP augment not requested".to_string()),
            ..Self::empty_stats_fields()
        }
    }

    pub fn failed(msg: impl Into<String>) -> Self {
        Self {
            status: ScipRunStatus::Failed,
            message: Some(msg.into()),
            ..Self::empty_stats_fields()
        }
    }

    pub fn from_stats(stats: &ScipEdgeStats) -> Self {
        // Always Some on Success for the locked skip set (simpler agent code).
        Self {
            status: ScipRunStatus::Success,
            edges_added: Some(stats.edges_added),
            edges_updated: Some(stats.edges_updated),
            definitions_mapped: Some(stats.definitions_mapped),
            definitions_seen: Some(stats.definitions_seen),
            files_skipped: Some(stats.files_skipped),
            edges_skipped_enclosing_disagreement: Some(stats.edges_skipped_enclosing_disagreement),
            edges_skipped_unmapped: Some(stats.edges_skipped_unmapped),
            edges_skipped_invalid_occ_range: Some(stats.edges_skipped_invalid_occ_range),
            edges_skipped_duplicate: Some(stats.edges_skipped_duplicate),
            definitions_skipped_invalid_range: Some(stats.definitions_skipped_invalid_range),
            invalid_enclosing_fallback: Some(stats.invalid_enclosing_fallback),
            references_seen: Some(stats.references_seen),
            message: None,
        }
    }

    /// Constructor retained for API stability; production paths no longer
    /// emit `SkippedStale` (requested augment always re-applies edges).
    #[allow(dead_code)]
    pub fn skipped_stale() -> Self {
        Self {
            status: ScipRunStatus::SkippedStale,
            edges_added: Some(0),
            edges_updated: Some(0),
            message: Some(
                "SCIP index hash unchanged (legacy skip; edges are always re-applied now)"
                    .to_string(),
            ),
            ..Self::empty_stats_fields()
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
///
/// **Always re-applies edges** when called (idempotent via precedence/dedup).
/// Hash matching is recorded in `scip_indices` for audit trail only — never
/// used to skip edge application (partial residual `scip:%` edges after
/// incremental deletes must not block a full re-apply).
pub fn execute_scip_index(
    layout: &Layout,
    storage: &mut StorageManager,
    scip_path: PathBuf,
) -> Result<ScipIndexJson> {
    info!(
        "Ingesting SCIP index from {:?} (edges-only augment; always re-apply)",
        scip_path
    );
    let scip_index = ScipIndex::load(&scip_path)?;

    let conn = storage.get_connection();
    let root = layout.root.as_std_path();
    let path_resolver = |rel: &str| -> Option<i64> {
        let normalized = match normalize_scip_path(root, rel) {
            Ok(p) => path_to_db_string(&p),
            Err(e) => {
                // O(docs); files already counted as files_skipped — debug not warn (0157).
                debug!("Failed to normalize SCIP path {rel}: {e}");
                return None;
            }
        };
        get_file_id_by_path(conn, &normalized).ok()
    };

    let stats = augment_edges_from_scip(conn, &scip_index.index.documents, &path_resolver)?;

    // Audit trail only — registration does not gate future re-applies.
    let conn_mut = storage.get_connection_mut();
    let tx = conn_mut.unchecked_transaction().into_diagnostic()?;
    register_scip_index(&tx, &scip_path, &scip_index.file_hash)?;
    tx.commit().into_diagnostic()?;

    info!(
        "SCIP augment complete: {} edges added, {} updated, {} defs mapped, {} files skipped, \
         {} refs seen, {} enclosing disagreements, {} unmapped",
        stats.edges_added,
        stats.edges_updated,
        stats.definitions_mapped,
        stats.files_skipped,
        stats.references_seen,
        stats.edges_skipped_enclosing_disagreement,
        stats.edges_skipped_unmapped
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
        assert!(j.edges_skipped_enclosing_disagreement.is_none());
        assert!(j.references_seen.is_none());
    }

    #[test]
    fn scip_evidence_marker_is_distinguishable() {
        assert!(SCIP_EDGE_EVIDENCE.starts_with("scip:"));
        assert_ne!(SCIP_EDGE_EVIDENCE, "call_expr:foo");
    }

    #[test]
    fn from_stats_always_some_on_success_including_zeros() {
        let stats = ScipEdgeStats {
            edges_added: 1,
            edges_updated: 2,
            definitions_mapped: 3,
            definitions_seen: 4,
            files_skipped: 0,
            edges_skipped_enclosing_disagreement: 5,
            edges_skipped_unmapped: 6,
            edges_skipped_invalid_occ_range: 0,
            edges_skipped_duplicate: 1,
            definitions_skipped_invalid_range: 0,
            invalid_enclosing_fallback: 2,
            references_seen: 100,
            ..Default::default()
        };
        let j = ScipIndexJson::from_stats(&stats);
        assert!(matches!(j.status, ScipRunStatus::Success));
        assert_eq!(j.edges_added, Some(1));
        assert_eq!(j.edges_skipped_enclosing_disagreement, Some(5));
        assert_eq!(j.edges_skipped_unmapped, Some(6));
        assert_eq!(j.references_seen, Some(100));
        assert_eq!(j.edges_skipped_invalid_occ_range, Some(0));
        assert_eq!(j.definitions_seen, Some(4));
        assert_eq!(j.invalid_enclosing_fallback, Some(2));
        assert_eq!(j.edges_skipped_duplicate, Some(1));
        assert_eq!(j.definitions_skipped_invalid_range, Some(0));
    }

    #[test]
    fn from_stats_serde_snake_case_fields() {
        let stats = ScipEdgeStats {
            edges_added: 10,
            edges_updated: 3,
            definitions_mapped: 50,
            definitions_seen: 55,
            files_skipped: 1,
            edges_skipped_enclosing_disagreement: 7,
            edges_skipped_unmapped: 20,
            edges_skipped_invalid_occ_range: 2,
            edges_skipped_duplicate: 4,
            definitions_skipped_invalid_range: 1,
            invalid_enclosing_fallback: 3,
            references_seen: 99,
            ..Default::default()
        };
        let j = ScipIndexJson::from_stats(&stats);
        let v = serde_json::to_value(&j).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(obj.get("status").and_then(|x| x.as_str()), Some("success"));
        assert_eq!(
            obj.get("edges_skipped_enclosing_disagreement")
                .and_then(|x| x.as_u64()),
            Some(7)
        );
        assert_eq!(
            obj.get("edges_skipped_unmapped").and_then(|x| x.as_u64()),
            Some(20)
        );
        assert_eq!(
            obj.get("references_seen").and_then(|x| x.as_u64()),
            Some(99)
        );
        assert_eq!(
            obj.get("edges_skipped_invalid_occ_range")
                .and_then(|x| x.as_u64()),
            Some(2)
        );
        assert_eq!(
            obj.get("definitions_skipped_invalid_range")
                .and_then(|x| x.as_u64()),
            Some(1)
        );
        assert_eq!(
            obj.get("invalid_enclosing_fallback")
                .and_then(|x| x.as_u64()),
            Some(3)
        );
        assert_eq!(
            obj.get("edges_skipped_duplicate").and_then(|x| x.as_u64()),
            Some(4)
        );
        assert_eq!(
            obj.get("definitions_seen").and_then(|x| x.as_u64()),
            Some(55)
        );
        // snake_case only — no camelCase twin
        assert!(!obj.contains_key("edgesSkippedEnclosingDisagreement"));
        assert!(!obj.contains_key("referencesSeen"));
    }

    #[test]
    fn failed_and_did_not_run_omit_skip_fields() {
        let failed = ScipIndexJson::failed("boom");
        let v = serde_json::to_value(&failed).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("edges_skipped_enclosing_disagreement"));
        assert!(!obj.contains_key("references_seen"));
        assert_eq!(obj.get("status").and_then(|x| x.as_str()), Some("failed"));

        let dnr = ScipIndexJson::did_not_run();
        let v = serde_json::to_value(&dnr).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("references_seen"));
        assert_eq!(
            obj.get("status").and_then(|x| x.as_str()),
            Some("did_not_run")
        );
    }
}
