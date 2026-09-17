use crate::config::model::Config;
use crate::index::ProjectIndexer;
use crate::index::staleness::{
    EmptyIndexReason, FreshnessSource, IndexFreshnessAssessment, IndexFreshnessState,
    assess_index_freshness, is_non_interactive,
};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use serde::Serialize;

const PREVIEW_KIND: &str = "indexRepairPreview";
const PROPOSED_FORCE_FULL_INDEX: &str = "force full index";
const PROPOSED_REPLACE_METADATA: &str = "replace metadata if successful";

/// Age-only camelCase assessment for `--repair-metadata --dry-run --json`.
/// Omits `emptyDiagnostics` (age-only assess always sets it `None`).
/// `staleFiles` / `unindexedFiles` are age-only zeros, not a content-drift verdict.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexRepairAssessmentJson {
    state: IndexFreshnessState,
    #[serde(skip_serializing_if = "Option::is_none")]
    empty_reason: Option<EmptyIndexReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_indexed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    days_since_indexed: Option<u64>,
    indexed_files: usize,
    stale_files: usize,
    unindexed_files: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    sample_paths: Vec<String>,
    source: FreshnessSource,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexRepairPreviewJson {
    schema_version: u32,
    kind: &'static str,
    executed: bool,
    dry_run: bool,
    assessment: IndexRepairAssessmentJson,
    proposed: Vec<String>,
}

impl From<&IndexFreshnessAssessment> for IndexRepairAssessmentJson {
    fn from(a: &IndexFreshnessAssessment) -> Self {
        Self {
            state: a.state.clone(),
            empty_reason: a.empty_reason.clone(),
            last_indexed_at: a.last_indexed_at.clone(),
            days_since_indexed: a.days_since_indexed,
            indexed_files: a.indexed_files,
            stale_files: a.stale_files,
            unindexed_files: a.unindexed_files,
            sample_paths: a.sample_paths.clone(),
            source: a.source.clone(),
            warnings: a.warnings.clone(),
        }
    }
}

fn locked_proposed() -> Vec<String> {
    let mut proposed = vec![
        PROPOSED_FORCE_FULL_INDEX.to_string(),
        PROPOSED_REPLACE_METADATA.to_string(),
    ];
    proposed.sort();
    proposed
}

fn repair_preview_json(assessment: &IndexFreshnessAssessment) -> IndexRepairPreviewJson {
    IndexRepairPreviewJson {
        schema_version: 1,
        kind: PREVIEW_KIND,
        executed: false,
        dry_run: true,
        assessment: IndexRepairAssessmentJson::from(assessment),
        proposed: locked_proposed(),
    }
}

pub(crate) fn execute_repair_metadata(
    layout: &Layout,
    storage: StorageManager,
    config: &Config,
    dry_run: bool,
    yes: bool,
    json: bool,
) -> Result<()> {
    let threshold = config.index.stale_threshold_days;
    let assessment = assess_index_freshness(&storage, threshold);

    if json && dry_run {
        let preview = repair_preview_json(&assessment);
        println!(
            "{}",
            serde_json::to_string_pretty(&preview).into_diagnostic()?
        );
        return Ok(());
    }

    if !json {
        println!("Current metadata state: {:?}", assessment.state);
    }

    if dry_run {
        if !json {
            println!("Dry run mode. Would force full index and replace metadata if successful.");
        }
        return Ok(());
    }

    // Check gross future clock skew
    if assessment
        .warnings
        .iter()
        .any(|w| w.contains("Future timestamp detected"))
        && !json
    {
        println!(
            "WARNING: Material future clock skew detected. Please correct your system clock before repairing."
        );
    }

    if !yes && !is_non_interactive() {
        let ans = inquire::Confirm::new(
            "Are you sure you want to repair metadata and rebuild the index?",
        )
        .with_default(false)
        .prompt()
        .unwrap_or(false);
        if !ans {
            if !json {
                println!("Aborted.");
            }
            return Ok(());
        }
    }

    if !json {
        println!("Repairing metadata (forcing full index pass)...");
    }

    let db_path = layout.state_subdir().join("ledger.db");
    let cozo_path = layout.state_subdir().join("ledger.cozo");
    let shadow_db_path = layout.state_subdir().join("ledger_shadow.db");
    let shadow_cozo_path = layout.state_subdir().join("ledger_shadow.cozo");

    // Ensure clean shadow state
    let _ = std::fs::remove_file(&shadow_db_path);
    let _ = std::fs::remove_dir_all(&shadow_cozo_path);
    let _ = std::fs::remove_file(&shadow_cozo_path);

    let shadow_storage = StorageManager::init(shadow_db_path.as_std_path())?;
    let mut indexer = ProjectIndexer::new(shadow_storage, layout.root.clone(), config.clone());

    match indexer.full_index() {
        Ok(_) => {
            // Drop indexer to release file locks before renaming
            drop(indexer);
            drop(storage);

            // Promote shadow DB
            std::fs::rename(&shadow_db_path, &db_path).into_diagnostic()?;
            if shadow_cozo_path.exists() {
                let _ = std::fs::remove_dir_all(&cozo_path);
                let _ = std::fs::remove_file(&cozo_path);
                std::fs::rename(&shadow_cozo_path, &cozo_path).into_diagnostic()?;
            }

            if !json {
                println!("Repair successful.");
            }
            Ok(())
        }
        Err(e) => {
            drop(indexer);
            let _ = std::fs::remove_file(&shadow_db_path);
            let _ = std::fs::remove_dir_all(&shadow_cozo_path);
            let _ = std::fs::remove_file(&shadow_cozo_path);
            if !json {
                eprintln!("Repair failed: {}. Rollback successful.", e);
            }
            Err(e)
        }
    }
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;

    fn sample_assessment() -> IndexFreshnessAssessment {
        IndexFreshnessAssessment {
            state: IndexFreshnessState::FreshPopulated,
            empty_reason: None,
            empty_diagnostics: None,
            last_indexed_at: Some("2026-09-17T12:00:00Z".into()),
            days_since_indexed: Some(0),
            indexed_files: 1,
            stale_files: 0,
            unindexed_files: 0,
            sample_paths: Vec::new(),
            source: FreshnessSource::RepositoryMetadata,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn repair_preview_json__fresh_assessment__locked_envelope_tokens() {
        let dto = repair_preview_json(&sample_assessment());
        let v = serde_json::to_value(&dto).expect("json");
        assert_eq!(v["schemaVersion"], 1);
        assert_eq!(v["kind"], PREVIEW_KIND);
        assert_eq!(v["executed"], false);
        assert_eq!(v["dryRun"], true);
        assert!(v.get("ok").is_none(), "ok must be omitted: {v}");
        assert_eq!(
            v["proposed"],
            serde_json::json!([PROPOSED_FORCE_FULL_INDEX, PROPOSED_REPLACE_METADATA])
        );
        assert_eq!(v["assessment"]["state"], "FreshPopulated");
        assert_eq!(v["assessment"]["source"], "RepositoryMetadata");
        assert_eq!(v["assessment"]["indexedFiles"], 1);
        assert_eq!(v["assessment"]["staleFiles"], 0);
        assert_eq!(v["assessment"]["unindexedFiles"], 0);
        assert_eq!(v["assessment"]["lastIndexedAt"], "2026-09-17T12:00:00Z");
        assert_eq!(v["assessment"]["daysSinceIndexed"], 0);
        assert!(v["assessment"].get("emptyDiagnostics").is_none());
        assert!(v["assessment"].get("emptyReason").is_none());
        assert!(v["assessment"].get("warnings").is_none());
        assert!(v["assessment"].get("samplePaths").is_none());
        let dumped = serde_json::to_string(&v).expect("dump");
        assert!(!dumped.contains(":null"), "never JSON null: {dumped}");
    }

    #[test]
    fn repair_preview_json__warnings_present__camel_case_omit_empty() {
        let mut assessment = sample_assessment();
        assessment.state = IndexFreshnessState::Indeterminate;
        assessment.warnings = vec!["Malformed timestamp in metadata: not-a-date".into()];
        assessment.last_indexed_at = None;
        assessment.days_since_indexed = None;
        let v = serde_json::to_value(repair_preview_json(&assessment)).expect("json");
        assert_eq!(v["assessment"]["state"], "Indeterminate");
        assert_eq!(
            v["assessment"]["warnings"][0],
            "Malformed timestamp in metadata: not-a-date"
        );
        assert!(v["assessment"].get("lastIndexedAt").is_none());
        assert!(v["assessment"].get("daysSinceIndexed").is_none());
        assert!(v["assessment"].get("emptyDiagnostics").is_none());
    }

    #[test]
    fn repair_preview_json__pretty__one_trailing_newline_shape() {
        let pretty = serde_json::to_string_pretty(&repair_preview_json(&sample_assessment()))
            .expect("pretty");
        assert!(
            pretty.contains('\n'),
            "pretty JSON must be multi-line: {pretty}"
        );
        assert!(pretty.contains("\"kind\": \"indexRepairPreview\""));
        assert!(!pretty.contains("emptyDiagnostics"));
    }
}
