use crate::cli::{HotspotArgs, HotspotIncludeScope};
use crate::impact::hotspots::{HotspotQuery, calculate_hotspots_detailed};
use crate::impact::temporal::{GixHistoryProvider, TemporalEngine};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use chrono::Utc;
use miette::{IntoDiagnostic, Result};
use serde::Serialize;

/// Truncate to `limit`, wrap as the hotspots list envelope, and echo `limit`.
/// Shared by list and `--semantic` JSON printers so the two arms cannot drift.
pub(super) fn wrap_hotspots_list_json<T: Serialize>(
    mut items: Vec<T>,
    limit: usize,
) -> serde_json::Value {
    items.truncate(limit);
    let mut output = crate::output::empty::format_json_list_envelope(items, "files");
    if let Some(map) = output.as_object_mut() {
        map.insert("limit".to_string(), serde_json::json!(limit));
    }
    output
}

pub(super) fn execute_hotspots_list(
    args: HotspotArgs,
    storage: &StorageManager,
    repo: &gix::Repository,
    config: &crate::config::model::Config,
    layout: &Layout,
) -> Result<()> {
    if args.semantic {
        let cozo = storage
            .cozo()
            .ok_or_else(|| miette::miette!("CozoDB storage not initialized"))?;

        if !args.json {
            println!("Analyzing semantic similarity hotspots (duplication)...");
        }

        let matches = crate::semantic::hotspots::find_semantic_hotspots(
            cozo,
            layout.root.as_std_path(),
            0.85,
        )?;

        if args.json {
            let limit = args.limit.unwrap_or(config.hotspots.limit);
            // find_semantic_hotspots ignores --limit; wrap_hotspots_list_json
            // truncates the already-computed Vec so echoing `limit` matches
            // the serialized `files` (no extra scan).
            let output = wrap_hotspots_list_json(matches, limit);
            println!(
                "{}",
                serde_json::to_string_pretty(&output)
                    .map_err(|e| miette::miette!("Failed to serialize semantic hotspots: {}", e))?
            );
        } else {
            crate::output::human::print_semantic_hotspots(&matches);
        }
        return Ok(());
    }

    let history_provider = GixHistoryProvider::new(repo);
    let exclude_test_paths = !matches!(args.include, Some(HotspotIncludeScope::Tests));
    let query = HotspotQuery {
        limit: args.limit.unwrap_or(config.hotspots.limit),
        commits: args.commits.unwrap_or(config.hotspots.max_commits),
        days: args.days.map(|d| d as u64),
        decay_half_life: config.hotspots.decay_half_life,
        dir_filter: args.entity.clone(),
        centrality: args.centrality,
        exclude_test_paths,
        ..Default::default()
    };

    let calculated = calculate_hotspots_detailed(storage, &history_provider, &query)?;
    let hotspots = calculated.hotspots;

    if args.snapshot {
        let couplings_persisted = persist_hotspots_and_couplings(storage, repo, &hotspots, config)?;
        if !args.json {
            if couplings_persisted {
                println!("Hotspot and temporal coupling snapshot persisted to SQLite.");
            } else {
                println!(
                    "Hotspot snapshot persisted to SQLite (temporal coupling history skipped: repository has fewer than 10 commits)."
                );
            }
        }
    }

    if args.json {
        let output = wrap_hotspots_list_json(hotspots, query.limit);
        println!(
            "{}",
            serde_json::to_string_pretty(&output).map_err(|e| miette::miette!("{}", e))?
        );
    } else if args.centrality {
        crate::output::human::print_hotspots_table_with_centrality(&hotspots);
        if let Some(footer) = omitted_hotspots_footer(calculated.omitted_test_paths) {
            println!("{footer}");
        }
    } else {
        crate::output::human::print_hotspots_table(&hotspots);
        if let Some(footer) = omitted_hotspots_footer(calculated.omitted_test_paths) {
            println!("{footer}");
        }
    }

    Ok(())
}

pub(super) fn omitted_hotspots_footer(omitted: usize) -> Option<String> {
    if omitted == 0 {
        None
    } else {
        Some(format!(
            "{omitted} test/example files omitted; --include tests"
        ))
    }
}

/// Persists a hotspot snapshot (and, history permitting, the accompanying
/// temporal-coupling snapshot) to SQLite.
///
/// Returns whether temporal coupling history was actually persisted: `true`
/// if persisted, `false` if skipped because the repository does not yet have
/// enough commit history (`GitError::InsufficientHistory`). Hotspot rows are
/// always persisted regardless of coupling availability, since couplings
/// require strictly more history than hotspots do.
pub(super) fn persist_hotspots_and_couplings(
    storage: &StorageManager,
    repo: &gix::Repository,
    hotspots: &[crate::impact::packet::Hotspot],
    config: &crate::config::model::Config,
) -> Result<bool> {
    let conn = storage.get_connection();
    let timestamp = Utc::now().to_rfc3339();

    let snapshot_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM snapshots ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok();

    // Insert Hotspots
    for hotspot in hotspots {
        conn.execute(
            "INSERT INTO hotspot_history (snapshot_id, file_path, score, display_score, complexity, frequency, centrality, timestamp) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                snapshot_id,
                hotspot.path.to_string_lossy().to_string(),
                hotspot.score,
                hotspot.display_score,
                hotspot.complexity,
                hotspot.frequency,
                hotspot.centrality.map(|c| c as i64),
                timestamp
            ],
        ).into_diagnostic()?;
    }

    // Calculate and Insert Temporal Couplings. A repository with fewer than
    // 10 commits in the analyzed window is a soft degradation, not a hard
    // failure: the hotspot rows above already succeeded, and the whole point
    // of `--bootstrap` is to give first-time users on young repos a usable
    // first snapshot rather than an error (see CG-F30). Any other GitError
    // still propagates as a hard failure.
    let history_provider = GixHistoryProvider::new(repo);
    let engine = TemporalEngine::new(history_provider, config.temporal.clone());
    let couplings_persisted = match engine.calculate_couplings() {
        Ok(couplings) => {
            for coupling in couplings {
                conn.execute(
                    "INSERT INTO temporal_coupling_history (snapshot_id, file_a, file_b, score, timestamp) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        snapshot_id,
                        coupling.file_a.to_string_lossy().to_string(),
                        coupling.file_b.to_string_lossy().to_string(),
                        coupling.score,
                        timestamp
                    ],
                )
                .into_diagnostic()?;
            }
            true
        }
        Err(crate::git::GitError::InsufficientHistory { .. }) => false,
        Err(e) => {
            return Err(miette::miette!(
                "Failed to calculate temporal couplings: {}",
                e
            ));
        }
    };

    Ok(couplings_persisted)
}
