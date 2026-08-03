use super::ProjectIndexer;
use crate::index::analysis::analyze_file;
use crate::index::languages::Language;
use crate::index::rows as row_helpers;
use crate::index::types::{ProjectFile, ProjectSymbol, symbol_to_project_symbol};
use crate::index::worker_pool::{JobResult, ParsedFileJob, WorkerPool};
use crate::state::storage::StorageManager;
use indicatif::{ProgressBar, ProgressStyle};
use miette::{IntoDiagnostic, Result};
use std::collections::HashMap;
use std::fs;
use std::time::Instant;
use tracing::{info, warn};

pub fn check_status(indexer: &ProjectIndexer) -> Result<super::IndexStatus> {
    let conn = indexer.storage.get_connection();

    let total_files: usize = conn
        .query_row(
            "SELECT COUNT(*) FROM project_files WHERE parse_status != 'DELETED'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .into_diagnostic()? as usize;

    let total_symbols: usize = conn
        .query_row("SELECT COUNT(*) FROM project_symbols", [], |row| {
            row.get::<_, i64>(0)
        })
        .into_diagnostic()? as usize;

    // Age-only assess (cheap) then one content-hash drift walk; override fields
    // so assessment.state / assessment.stale_files match top-level drift counts.
    let age_assessment = crate::index::staleness::assess_index_freshness(
        &indexer.storage,
        indexer.config.index.stale_threshold_days,
    );
    let drift =
        crate::index::staleness::count_content_hash_drift(&indexer.storage, &indexer.repo_path)?;
    let assessment = crate::index::staleness::apply_content_drift_override(age_assessment, &drift);
    let last_indexed_at = assessment.last_indexed_at.clone();

    Ok(super::IndexStatus {
        total_files,
        total_symbols,
        stale_files: drift.changed_or_unindexed,
        last_indexed_at,
        assessment: Some(assessment),
    })
}

pub fn full_index(indexer: &mut ProjectIndexer) -> Result<super::IndexStats> {
    let start = Instant::now();
    let files = super::discovery::discover_files(indexer)?;

    clear_project_data(&mut indexer.storage)?;

    let pb = create_progress_bar(files.len());
    let pool = WorkerPool::new(0);
    let repo_path = indexer.repo_path.clone();

    let rx = pool.process_parsing(files, Some(pb.clone()), move |path| {
        let relative = path.strip_prefix(&repo_path).unwrap_or(path);
        let outcome = analyze_file(relative.as_std_path(), repo_path.as_std_path());

        let now = chrono::Utc::now().to_rfc3339();
        let mut pf = ProjectFile {
            id: None,
            file_path: relative.to_string().replace('\\', "/"),
            language: relative
                .extension()
                .and_then(Language::from_extension)
                .map(|l| format!("{:?}", l)),
            content_hash: None,
            git_blob_oid: None,
            file_size: fs::metadata(path).ok().map(|m| m.len() as i64),
            mtime_ns: None,
            parser_version: super::PARSER_VERSION.to_string(),
            parse_status: if outcome.analysis_status.symbols
                == crate::impact::packet::AnalysisStatus::Ok
            {
                "OK".to_string()
            } else {
                "PARSE_FAILED".to_string()
            },
            last_indexed_at: now.clone(),
        };

        let mut bindings = Vec::new();
        if let Ok(content) = crate::util::fs::read_to_string_with_encoding(path.as_std_path()) {
            pf.content_hash = Some(blake3::hash(content.as_bytes()).to_hex().to_string());
            match crate::index::references::extract_file_bindings(path.as_std_path(), &content) {
                Ok(Some(b)) => bindings = b,
                Ok(None) => {}
                Err(e) => {
                    warn!("Binding extraction failed for {}: {}", path, e);
                }
            }
        }

        let ps = outcome
            .symbols
            .unwrap_or_default()
            .into_iter()
            .map(|s| symbol_to_project_symbol(&s, 0, &now))
            .collect();

        Ok(ParsedFileJob {
            file: pf,
            symbols: ps,
            bindings,
        })
    })?;

    let stats = collect_results(&mut indexer.storage, rx, true)?;
    pb.finish_and_clear();
    store_index_metadata(indexer)?;
    report_indexed_repo_size_for_timing(&indexer.storage);

    let duration_ms = start.elapsed().as_millis() as u64;
    info!("Full index complete in {}ms", duration_ms);
    Ok(super::IndexStats {
        duration_ms,
        ..stats
    })
}

pub fn incremental_index(indexer: &mut ProjectIndexer) -> Result<super::IndexStats> {
    let start = Instant::now();
    let current_files = super::discovery::discover_files(indexer)?;

    let existing_files = load_existing_files(&indexer.storage)?;
    let mut files_to_reindex = Vec::new();
    let mut current_relatives: std::collections::HashSet<String> = std::collections::HashSet::new();

    for file_path in &current_files {
        let relative = file_path
            .strip_prefix(&indexer.repo_path)
            .unwrap_or(file_path)
            .to_string()
            .replace('\\', "/");
        current_relatives.insert(relative.clone());
        if let Some(existing) = existing_files.get(&relative) {
            match crate::util::fs::read_to_string_with_encoding(file_path.as_std_path()) {
                Ok(content) => {
                    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
                    if existing.content_hash.as_deref() != Some(&hash) {
                        files_to_reindex.push(file_path.clone());
                    }
                }
                Err(_) => {
                    files_to_reindex.push(file_path.clone());
                }
            }
        } else {
            files_to_reindex.push(file_path.clone());
        }
    }

    // Reconcile deletions for **code** sources only. Enrichment rows (docs, CI,
    // .env.example, …) are stored outside supported-extension discovery and must
    // not be treated as worktree removals on light-path incremental refresh.
    let mut deleted_paths: Vec<String> = existing_files
        .keys()
        .filter(|path| !current_relatives.contains(*path) && is_supported_code_source_path(path))
        .cloned()
        .collect();
    deleted_paths.sort();
    for relative in &deleted_paths {
        // Match watch-delete: prune dependents + symbols, then mark file DELETED.
        row_helpers::delete_file_symbols(&mut indexer.storage, relative)?;
        indexer
            .storage
            .get_connection()
            .execute(
                "UPDATE project_files SET parse_status = 'DELETED' WHERE file_path = ?1",
                [relative],
            )
            .into_diagnostic()?;
    }

    if files_to_reindex.is_empty() {
        // Deletion-only refresh still advances index metadata so time-fresh +
        // content-clean is honest after auto-index.
        store_index_metadata(indexer)?;
        // Still report existing indexed size (no new walk) when nothing changed.
        report_indexed_repo_size_for_timing(&indexer.storage);
        return Ok(super::IndexStats {
            duration_ms: start.elapsed().as_millis() as u64,
            files_indexed: 0,
            symbols_indexed: 0,
            parse_failures: 0,
            skipped_binary: 0,
            skipped_unsupported: 0,
        });
    }

    let pb = create_progress_bar(files_to_reindex.len());
    let pool = WorkerPool::new(0);
    let repo_path = indexer.repo_path.clone();

    let rx = pool.process_parsing(files_to_reindex, Some(pb.clone()), move |path| {
        let relative = path.strip_prefix(&repo_path).unwrap_or(path);
        let outcome = analyze_file(relative.as_std_path(), repo_path.as_std_path());
        let now = chrono::Utc::now().to_rfc3339();
        let mut pf = ProjectFile {
            id: None,
            file_path: relative.to_string().replace('\\', "/"),
            language: relative
                .extension()
                .and_then(Language::from_extension)
                .map(|l| format!("{:?}", l)),
            content_hash: None,
            git_blob_oid: None,
            file_size: fs::metadata(path).ok().map(|m| m.len() as i64),
            mtime_ns: None,
            parser_version: super::PARSER_VERSION.to_string(),
            parse_status: if outcome.analysis_status.symbols
                == crate::impact::packet::AnalysisStatus::Ok
            {
                "OK".to_string()
            } else {
                "PARSE_FAILED".to_string()
            },
            last_indexed_at: now.clone(),
        };
        let mut bindings = Vec::new();
        if let Ok(content) = crate::util::fs::read_to_string_with_encoding(path.as_std_path()) {
            pf.content_hash = Some(blake3::hash(content.as_bytes()).to_hex().to_string());
            match crate::index::references::extract_file_bindings(path.as_std_path(), &content) {
                Ok(Some(b)) => bindings = b,
                Ok(None) => {}
                Err(e) => {
                    warn!("Binding extraction failed for {}: {}", path, e);
                }
            }
        }
        let ps = outcome
            .symbols
            .unwrap_or_default()
            .into_iter()
            .map(|s| symbol_to_project_symbol(&s, 0, &now))
            .collect();
        Ok(ParsedFileJob {
            file: pf,
            symbols: ps,
            bindings,
        })
    })?;

    let stats = collect_results(&mut indexer.storage, rx, false)?;
    pb.finish_and_clear();
    store_index_metadata(indexer)?;
    report_indexed_repo_size_for_timing(&indexer.storage);

    Ok(super::IndexStats {
        duration_ms: start.elapsed().as_millis() as u64,
        ..stats
    })
}

/// Best-effort opportunistic repo size for self-timing: sum of already-known
/// `project_files.file_size` (populated while indexing). Never starts a new walk.
/// File counts alone are not written — only real byte totals.
#[cfg(feature = "self-timing")]
fn report_indexed_repo_size_for_timing(storage: &StorageManager) {
    let Ok(sum) = storage.get_connection().query_row(
        "SELECT COALESCE(SUM(file_size), 0) FROM project_files WHERE parse_status != 'DELETED'",
        [],
        |row| row.get::<_, i64>(0),
    ) else {
        return;
    };
    if sum > 0 {
        crate::observability::self_timing::set_current_repo_size_bytes(sum as u64);
    }
}

#[cfg(not(feature = "self-timing"))]
fn report_indexed_repo_size_for_timing(_storage: &StorageManager) {}

pub fn collect_results(
    storage: &mut StorageManager,
    rx: crossbeam::channel::Receiver<JobResult>,
    is_full: bool,
) -> Result<super::IndexStats> {
    let mut files_indexed = 0;
    let mut symbols_indexed = 0;
    let mut parse_failures = 0;
    let mut batch_files = Vec::new();
    let mut batch_symbols = Vec::new();
    let mut batch_bindings = Vec::new();

    while let Ok(result) = rx.recv() {
        match result {
            JobResult::Parsed(job) => {
                let ParsedFileJob {
                    file: pf,
                    symbols: ps,
                    bindings,
                } = *job;
                if pf.parse_status == "PARSE_FAILED" {
                    parse_failures += 1;
                } else {
                    files_indexed += 1;
                }
                symbols_indexed += ps.len();

                if !is_full {
                    let _ = row_helpers::delete_file_symbols(storage, &pf.file_path);
                }

                batch_files.push(pf);
                batch_symbols.push(ps);
                batch_bindings.push(bindings);

                if batch_files.len() >= super::BATCH_SIZE {
                    if is_full {
                        insert_batch(storage, &batch_files, &batch_symbols, &batch_bindings)?;
                    } else {
                        upsert_batch(storage, &batch_files, &batch_symbols, &batch_bindings)?;
                    }
                    batch_files.clear();
                    batch_symbols.clear();
                    batch_bindings.clear();
                }
            }
            JobResult::Failure(path, err) => {
                warn!("Parallel index failure for {}: {}", path, err);
                parse_failures += 1;
            }
            _ => {}
        }
    }

    if !batch_files.is_empty() {
        if is_full {
            insert_batch(storage, &batch_files, &batch_symbols, &batch_bindings)?;
        } else {
            upsert_batch(storage, &batch_files, &batch_symbols, &batch_bindings)?;
        }
    }

    Ok(super::IndexStats {
        files_indexed,
        symbols_indexed,
        parse_failures,
        skipped_binary: 0,
        skipped_unsupported: 0,
        duration_ms: 0,
    })
}

pub fn clear_project_data(storage: &mut StorageManager) -> Result<()> {
    let conn = storage.get_connection_mut();
    for table in [
        "symbol_centrality",
        "structural_edges",
        "api_routes",
        "data_models",
        "observability_patterns",
        "test_mapping",
        "ci_gates",
        "env_references",
        "env_declarations",
        "project_docs",
        "project_topology",
        "file_bindings",
        "project_symbols",
        "project_files",
        // 0095 DoD-10: scip_indices must not outlive the rows it describes
        "scip_indices",
    ] {
        conn.execute(&format!("DELETE FROM {}", table), [])
            .into_diagnostic()?;
    }
    Ok(())
}

pub fn insert_batch(
    storage: &mut StorageManager,
    files: &[ProjectFile],
    symbols: &[Vec<ProjectSymbol>],
    bindings: &[Vec<crate::index::bindings::FileBinding>],
) -> Result<()> {
    let conn = storage.get_connection_mut();
    let tx = conn.unchecked_transaction().into_diagnostic()?;
    for (i, pf) in files.iter().enumerate() {
        row_helpers::insert_file_row(&tx, pf)?;
        let file_id = tx.last_insert_rowid();
        for ps in &symbols[i] {
            row_helpers::insert_symbol_row(&tx, ps, file_id)?;
        }
        let file_bindings = bindings.get(i).map(Vec::as_slice).unwrap_or(&[]);
        row_helpers::replace_file_bindings(&tx, file_id, file_bindings)?;
    }
    tx.commit().into_diagnostic()
}

pub fn upsert_batch(
    storage: &mut StorageManager,
    files: &[ProjectFile],
    symbols: &[Vec<ProjectSymbol>],
    bindings: &[Vec<crate::index::bindings::FileBinding>],
) -> Result<()> {
    let conn = storage.get_connection_mut();
    let tx = conn.unchecked_transaction().into_diagnostic()?;
    for (i, pf) in files.iter().enumerate() {
        row_helpers::upsert_file_row(&tx, pf)?;
        let file_id = row_helpers::get_file_id_by_path(&tx, &pf.file_path)?;
        for ps in &symbols[i] {
            row_helpers::insert_symbol_row(&tx, ps, file_id)?;
        }
        let file_bindings = bindings.get(i).map(Vec::as_slice).unwrap_or(&[]);
        row_helpers::replace_file_bindings(&tx, file_id, file_bindings)?;
    }
    tx.commit().into_diagnostic()
}

pub fn store_index_metadata(indexer: &mut ProjectIndexer) -> Result<()> {
    let conn = indexer.storage.get_connection_mut();
    let tx = conn.unchecked_transaction().into_diagnostic()?;
    let now = chrono::Utc::now().to_rfc3339();
    for (key, value) in [
        ("parser_version", super::PARSER_VERSION),
        ("last_indexed_at", &now),
        ("index_version", "1"),
        ("schema_version", "1"),
    ] {
        tx.execute(
            "INSERT OR REPLACE INTO index_metadata (key, value) VALUES (?1, ?2)",
            (key, value),
        )
        .into_diagnostic()?;
    }

    // Honest HEAD metadata for verify fast-scope staleness (0135).
    // When git HEAD is resolvable, persist it; when unresolvable (non-git,
    // unborn HEAD, discovery failure), DELETE any prior head_hash so a
    // previous git context cannot leave a stale value.
    let head_hash = crate::git::repo::open_repo(indexer.repo_path.as_std_path())
        .ok()
        .and_then(|repo| crate::git::repo::get_head_info(&repo).ok())
        .and_then(|(hash, _branch)| hash);
    if let Some(ref hash) = head_hash {
        tx.execute(
            "INSERT OR REPLACE INTO index_metadata (key, value) VALUES ('head_hash', ?1)",
            [hash.as_str()],
        )
        .into_diagnostic()?;
    } else {
        tx.execute("DELETE FROM index_metadata WHERE key = 'head_hash'", [])
            .into_diagnostic()?;
    }

    let count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM project_files WHERE parse_status = 'OK'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if count == 0 {
        let walker = crate::index::walker::RepoWalker::new(
            indexer.repo_path.clone(),
            super::SUPPORTED_EXTENSIONS,
            super::BINARY_EXTENSIONS,
        );
        let (reason, diag) = walker.discover_empty_reason();

        let reason_str = serde_json::to_string(&reason).into_diagnostic()?;
        let diag_str = serde_json::to_string(&diag).into_diagnostic()?;

        tx.execute("INSERT OR REPLACE INTO index_metadata (key, value) VALUES ('empty_reason', ?1), ('empty_diagnostics', ?2)",
                     rusqlite::params![reason_str, diag_str]).into_diagnostic()?;
    } else {
        tx.execute(
            "DELETE FROM index_metadata WHERE key IN ('empty_reason', 'empty_diagnostics')",
            [],
        )
        .into_diagnostic()?;
    }

    tx.commit().into_diagnostic()?;
    Ok(())
}

/// Code sources that `discover_files` can see (not enrichment-only rows).
fn is_supported_code_source_path(relative_path: &str) -> bool {
    let ext = std::path::Path::new(relative_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    super::SUPPORTED_EXTENSIONS.contains(&ext) && !super::BINARY_EXTENSIONS.contains(&ext)
}

pub fn load_existing_files(storage: &StorageManager) -> Result<HashMap<String, ProjectFile>> {
    let conn = storage.get_connection();
    let mut stmt = conn.prepare("SELECT id, file_path, language, content_hash, git_blob_oid, file_size, mtime_ns, parser_version, parse_status, last_indexed_at FROM project_files WHERE parse_status != 'DELETED'").into_diagnostic()?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ProjectFile {
                id: Some(row.get(0)?),
                file_path: row.get(1)?,
                language: row.get(2)?,
                content_hash: row.get(3)?,
                git_blob_oid: row.get(4)?,
                file_size: row.get(5)?,
                mtime_ns: row.get(6)?,
                parser_version: row.get(7)?,
                parse_status: row.get(8)?,
                last_indexed_at: row.get(9)?,
            })
        })
        .into_diagnostic()?
        .collect::<Result<Vec<_>, _>>()
        .into_diagnostic()?;
    Ok(rows
        .into_iter()
        .map(|mut pf| {
            // Normalize so Windows backslash rows still match discovery paths.
            pf.file_path = pf.file_path.replace('\\', "/");
            let key = pf.file_path.clone();
            (key, pf)
        })
        .collect())
}

fn create_progress_bar(total: usize) -> ProgressBar {
    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::with_template("Indexing: {pos}/{len} files... {spinner}")
            .unwrap_or_else(|_| ProgressStyle::with_template("{pos}/{len}").unwrap()),
    );
    pb
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    #[test]
    fn clear_project_data_clears_scip_indices() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO scip_indices (index_path, blake3_hash, indexed_at) \
             VALUES ('/tmp/x.scip', 'abc', datetime('now'))",
            [],
        )
        .unwrap();
        let before: i64 = conn
            .query_row("SELECT COUNT(*) FROM scip_indices", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, 1);

        let mut storage = StorageManager::init_from_conn(conn);
        clear_project_data(&mut storage).unwrap();

        let after: i64 = storage
            .get_connection()
            .query_row("SELECT COUNT(*) FROM scip_indices", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            after, 0,
            "DoD-10: scip_indices must clear with project data"
        );
    }
}
