use crate::cli::{HotspotArgs, HotspotIncludeScope};
use crate::git::blob::head_path_exists;
use crate::impact::budget::{
    AnalysisBudget, HotspotProvenance, HotspotProvenanceSource, completeness_for_walk,
    eprint_walk_stop, filter_for_cli_include, format_provenance_footer,
};
use crate::impact::hotspots::{HotspotQuery, calculate_hotspots_detailed};
use crate::impact::packet::Hotspot;
use crate::impact::temporal::{GixHistoryProvider, TemporalEngine};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use chrono::{DateTime, Utc};
use miette::{IntoDiagnostic, Result};
use serde::Serialize;

/// Truncate to `limit`, wrap as the hotspots list envelope, and echo `limit`.
/// Shared by list and `--semantic` JSON printers so the two arms cannot drift.
pub(super) fn wrap_hotspots_list_json<T: Serialize>(
    items: Vec<T>,
    limit: usize,
) -> serde_json::Value {
    wrap_hotspots_list_json_with_completeness(items, limit, None, None)
}

pub(super) fn wrap_hotspots_list_json_with_completeness<T: Serialize>(
    mut items: Vec<T>,
    limit: usize,
    completeness: Option<&crate::impact::budget::AnalysisCompleteness>,
    provenance: Option<&HotspotProvenance>,
) -> serde_json::Value {
    items.truncate(limit);
    let mut output = crate::output::empty::format_json_list_envelope(items, "files");
    if let Some(map) = output.as_object_mut() {
        map.insert("limit".to_string(), serde_json::json!(limit));
        if let Some(c) = completeness
            && let Ok(v) = serde_json::to_value(c)
        {
            map.insert("completeness".to_string(), v);
        }
        if let Some(p) = provenance
            && let Ok(v) = serde_json::to_value(p)
        {
            map.insert("provenance".to_string(), v);
        }
    }
    output
}

/// Emit-time `presence` only — never a field on shared [`Hotspot`].
pub(super) fn list_hotspot_json(repo: &gix::Repository, hotspot: &Hotspot) -> serde_json::Value {
    let mut value = match serde_json::to_value(hotspot) {
        Ok(v) => v,
        Err(_) => serde_json::json!({
            "path": hotspot.path.to_string_lossy(),
            "score": hotspot.score,
            "displayScore": hotspot.display_score,
            "complexity": hotspot.complexity,
            "frequency": hotspot.frequency,
        }),
    };
    if !head_path_exists(repo, &hotspot.path.to_string_lossy())
        && let Some(obj) = value.as_object_mut()
    {
        obj.insert(
            "presence".to_string(),
            serde_json::Value::String("historical".to_string()),
        );
    }
    value
}

pub(super) fn latest_hotspot_history_timestamp(storage: &StorageManager) -> Option<String> {
    storage
        .get_connection()
        .query_row("SELECT MAX(timestamp) FROM hotspot_history", [], |row| {
            row.get::<_, Option<String>>(0)
        })
        .ok()
        .flatten()
}

pub(super) fn snapshot_age_secs_from(snapshot_at: &str, now: DateTime<Utc>) -> Option<u64> {
    let parsed = DateTime::parse_from_rfc3339(snapshot_at).ok()?;
    let age = now.signed_duration_since(parsed.with_timezone(&Utc));
    Some(age.num_seconds().max(0) as u64)
}

pub(super) fn live_list_provenance(
    query: &HotspotQuery,
    include: Option<HotspotIncludeScope>,
    head: Option<String>,
    snapshot_at: Option<String>,
    now: DateTime<Utc>,
) -> HotspotProvenance {
    let (snapshot_at, snapshot_age_secs) = match snapshot_at {
        Some(ts) => match snapshot_age_secs_from(&ts, now) {
            Some(age) => (Some(ts), Some(age)),
            None => (None, None),
        },
        None => (None, None),
    };
    HotspotProvenance {
        source: HotspotProvenanceSource::Live,
        commits_requested: Some(query.commits as u64),
        days_requested: query.days,
        limit: Some(query.limit as u64),
        filter: Some(filter_for_cli_include(include)),
        head,
        snapshot_at,
        snapshot_age_secs,
        ..HotspotProvenance::default()
    }
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
            crate::output::json::emit(&output)
                .map_err(|e| miette::miette!("Failed to serialize semantic hotspots: {}", e))?;
        } else {
            crate::output::human::print_semantic_hotspots(&matches);
        }
        return Ok(());
    }

    let history_provider = GixHistoryProvider::new(repo);
    let (exclude_test_paths, exclude_docs_paths, docs_frequency_lane, exclude_vendor_paths) =
        match args.include {
            Some(HotspotIncludeScope::Tests) => (false, false, false, false),
            Some(HotspotIncludeScope::Docs) => (false, false, true, false),
            Some(HotspotIncludeScope::Vendor) => (true, true, false, false),
            None => (true, true, false, true),
        };
    let cancel = crate::impact::budget::install_cancel_flag();
    let query = HotspotQuery {
        limit: args.limit.unwrap_or(config.hotspots.limit),
        commits: args.commits.unwrap_or(config.hotspots.max_commits),
        days: args.days.map(|d| d as u64),
        decay_half_life: config.hotspots.decay_half_life,
        dir_filter: args.entity.clone(),
        centrality: args.centrality,
        exclude_test_paths,
        exclude_docs_paths,
        docs_frequency_lane,
        exclude_vendor_paths,
        budget: Some(AnalysisBudget::from_secs(
            config.hotspots.history_budget_secs,
            cancel,
        )),
        ..Default::default()
    };

    let calculated = calculate_hotspots_detailed(storage, &history_provider, &query)?;
    let hotspots = calculated.hotspots;
    let completeness = completeness_for_walk(
        calculated.walk_stop,
        query.commits as u64,
        calculated.commits_walked as u64,
        query.days,
        filter_for_cli_include(args.include),
        calculated.head.clone(),
        Some(config.hotspots.history_budget_secs).filter(|s| *s > 0),
    );
    eprint_walk_stop(
        calculated.walk_stop,
        calculated.commits_walked,
        query.commits,
    );

    if args.snapshot {
        if matches!(args.include, Some(HotspotIncludeScope::Docs)) {
            return Err(miette::miette!(
                "--snapshot cannot be combined with --include docs (docs lane score is frequency-only; hotspot_history stores f×c)"
            ));
        }
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

    let snapshot_at = if args.json {
        latest_hotspot_history_timestamp(storage)
    } else {
        None
    };
    let provenance = live_list_provenance(
        &query,
        args.include,
        calculated.head.clone(),
        snapshot_at,
        Utc::now(),
    );

    if args.json {
        let files: Vec<serde_json::Value> = hotspots
            .iter()
            .map(|h| list_hotspot_json(repo, h))
            .collect();
        let output = wrap_hotspots_list_json_with_completeness(
            files,
            query.limit,
            completeness.as_ref(),
            Some(&provenance),
        );
        crate::output::json::emit(&output).map_err(|e| miette::miette!("{}", e))?;
    } else if args.centrality {
        crate::output::human::print_hotspots_table_with_centrality(&hotspots);
        print_omit_footers(
            calculated.omitted_test_paths,
            calculated.omitted_docs_paths,
            calculated.omitted_vendor_paths,
            docs_frequency_lane,
        );
        println!("{}", format_provenance_footer(&provenance));
    } else {
        crate::output::human::print_hotspots_table(&hotspots);
        print_omit_footers(
            calculated.omitted_test_paths,
            calculated.omitted_docs_paths,
            calculated.omitted_vendor_paths,
            docs_frequency_lane,
        );
        println!("{}", format_provenance_footer(&provenance));
    }

    Ok(())
}

fn print_omit_footers(
    omitted_tests: usize,
    omitted_docs: usize,
    omitted_vendor: usize,
    docs_lane: bool,
) {
    if !docs_lane && let Some(footer) = omitted_hotspots_footer(omitted_tests) {
        println!("{footer}");
    }
    if let Some(footer) = omitted_docs_footer(omitted_docs) {
        println!("{footer}");
    }
    if !docs_lane && let Some(footer) = omitted_vendor_hotspots_footer(omitted_vendor) {
        println!("{footer}");
    }
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

pub(super) fn omitted_docs_footer(omitted: usize) -> Option<String> {
    if omitted == 0 {
        None
    } else {
        Some(format!(
            "{omitted} documentation files omitted; --include docs"
        ))
    }
}

pub(super) fn omitted_vendor_hotspots_footer(omitted: usize) -> Option<String> {
    if omitted == 0 {
        None
    } else {
        Some(format!(
            "{omitted} vendored files omitted; --include vendor"
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
