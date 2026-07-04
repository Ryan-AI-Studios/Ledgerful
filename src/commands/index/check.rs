use crate::git;
use crate::index::staleness::{StalenessWarning, check_index_staleness, print_staleness_warning};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use camino::Utf8PathBuf;
use miette::Result;
use std::collections::HashMap;
use std::path::Path;
use std::process::exit;

pub fn execute_index_check(path: &Path, threshold: u64, json: bool, strict: bool) -> Result<()> {
    let root = Utf8PathBuf::from_path_buf(path.to_path_buf())
        .map_err(|_| miette::miette!("Invalid UTF-8 in path"))?;
    let layout = Layout::new(root.as_str());

    let storage_res = StorageManager::open_read_only(&layout.root);
    let mut warning = match storage_res {
        Ok(ref storage) => check_index_staleness(storage, threshold),
        Err(_) => Some(StalenessWarning {
            days_since_indexed: 999,
            stale_files: 0,
            unindexed_files: 0,
            sample_paths: vec![],
            last_indexed_at: None,
            is_missing: true,
        }),
    };

    if let Ok(repo) = git::repo::open_repo(path)
        && let Ok(files) = git::status::get_repo_status(&repo)
    {
        let indexed_files = if let Ok(ref storage) = storage_res {
            storage.get_active_file_id_map().unwrap_or_default()
        } else {
            HashMap::new()
        };

        let mut unindexed = 0;
        for file in &files {
            let rel_path = &file.path;
            if !indexed_files.contains_key(rel_path) {
                let ext = rel_path.extension().and_then(|s| s.to_str()).unwrap_or("");
                if matches!(ext, "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "md") {
                    unindexed += 1;
                }
            }
        }

        if unindexed > 0 {
            if let Some(ref mut w) = warning {
                w.unindexed_files = unindexed;
            } else {
                warning = Some(StalenessWarning {
                    days_since_indexed: 0,
                    stale_files: 0,
                    unindexed_files: unindexed,
                    sample_paths: vec![],
                    last_indexed_at: None,
                    is_missing: false,
                });
            }
        }
    }

    if let Some(warning) = warning {
        if json {
            println!("{}", serde_json::to_string(&warning).unwrap_or_default());
        } else {
            print_staleness_warning(&warning);
        }

        if warning.is_missing {
            if warning.unindexed_files > 0 {
                exit(1);
            }
        } else if strict && (warning.days_since_indexed > threshold || warning.unindexed_files > 0)
        {
            exit(1);
        }
    } else if json {
        println!(r#"{{"status": "fresh"}}"#);
    } else {
        println!("Index is fresh.");
    }

    Ok(())
}
