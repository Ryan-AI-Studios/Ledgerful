use crate::config::model::Config;
use crate::index::ProjectIndexer;
use crate::index::staleness::{assess_index_freshness, is_non_interactive};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};

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

    let output = if json {
        serde_json::to_string(&assessment).into_diagnostic()?
    } else {
        format!("Current metadata state: {:?}", assessment.state)
    };

    if !json {
        println!("{}", output);
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
