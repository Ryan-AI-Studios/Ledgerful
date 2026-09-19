use crate::cli::{HotspotArgs, HotspotSubcommands};
use crate::commands::helpers::get_layout;
use crate::git::repo::open_repo;
use crate::impact::budget::{
    CompletenessStop, HOTSPOTS_BUDGET_WARN, completeness_for_overall, install_cancel_flag,
    is_overall_stop, poll_overall_stop, resolve_hotspots_overall_budget_secs,
};
use crate::index::warn_if_stale;
use crate::state::storage::StorageManager;
use miette::Result;
use std::env;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

mod budget;
mod explain;
mod list;
mod trend;

#[cfg(test)]
mod tests;

pub use explain::{HotspotExplanation, compute_hotspot_explanation};

#[derive(Debug, Clone, Default)]
pub(crate) struct HotspotRunOpts {
    /// Test injection. CLI leaves `None`; list/explain install Ctrl-C once.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Test injection. CLI leaves `None`.
    pub overall_deadline_override: Option<Instant>,
}

pub fn execute_hotspots(args: HotspotArgs) -> Result<()> {
    execute_hotspots_with_opts(args, HotspotRunOpts::default(), None)
}

fn is_list_or_explain(args: &HotspotArgs) -> bool {
    match &args.command {
        None => true,
        Some(HotspotSubcommands::Explain { .. }) => true,
        Some(_) => false,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_hotspots_with_opts(
    args: HotspotArgs,
    opts: HotspotRunOpts,
    mut json_out: Option<&mut Vec<u8>>,
) -> Result<()> {
    let current_dir = env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {}", e))?;
    let repo = open_repo(&current_dir)?;
    let layout = get_layout()?;

    let mut config = crate::config::load::load_config_or_default_warn(&layout);
    crate::impact::budget::apply_resolved_history_budget(&mut config, None);

    let list_or_explain = is_list_or_explain(&args);
    let overall_secs = if list_or_explain {
        resolve_hotspots_overall_budget_secs(args.timeout, config.hotspots.overall_budget_secs)
    } else {
        0
    };
    let cancel = if list_or_explain {
        opts.cancel.clone().unwrap_or_else(install_cancel_flag)
    } else {
        opts.cancel
            .clone()
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)))
    };
    let overall_deadline = if list_or_explain {
        opts.overall_deadline_override.or_else(|| {
            (overall_secs > 0).then(|| Instant::now() + Duration::from_secs(overall_secs))
        })
    } else {
        None
    };

    if list_or_explain && let Some(stop) = poll_overall_stop(overall_deadline, &cancel) {
        let stage = if args.semantic { "semantic" } else { "storage" };
        return emit_overall_skip_open(&args, overall_secs, stage, stop, json_out.as_deref_mut());
    }

    let threshold_days = config.index.stale_threshold_days;
    let need_cozo = args.semantic || args.centrality;
    let need_write = args.snapshot
        || matches!(
            &args.command,
            Some(HotspotSubcommands::Trend {
                bootstrap: true,
                ..
            })
        );
    let storage = if need_write {
        layout.ensure_state_dir()?;
        let storage = StorageManager::init_with_layout(&layout)?;
        if args.auto_index {
            let (storage, _) =
                crate::index::staleness::try_auto_index(storage, threshold_days, &layout)?;
            storage
        } else {
            let _ = warn_if_stale(&storage, threshold_days);
            storage
        }
    } else if args.auto_index {
        let opened = if need_cozo {
            StorageManager::open_read_only(&layout)
        } else {
            StorageManager::open_read_only_sqlite_only(&layout)
        };
        let storage = match opened {
            Ok(s) => s,
            Err(_) => {
                layout.ensure_state_dir()?;
                StorageManager::init_with_layout(&layout)?
            }
        };
        let (storage, _) =
            crate::index::staleness::try_auto_index(storage, threshold_days, &layout)?;
        storage
    } else {
        let storage = if need_cozo {
            StorageManager::open_read_only(&layout)?
        } else {
            StorageManager::open_read_only_sqlite_only(&layout)?
        };
        let _ = warn_if_stale(&storage, threshold_days);
        storage
    };

    if let Some(command) = args.command.clone() {
        match command {
            HotspotSubcommands::Trend {
                entity,
                days,
                limit,
                all,
                json,
                bootstrap,
                samples,
                force,
            } => {
                let limit = usize::try_from(limit).unwrap_or(usize::MAX);
                return trend::execute_hotspots_trend(
                    &storage, &repo, &config, entity, days, limit, all, json, bootstrap, samples,
                    force,
                );
            }
            HotspotSubcommands::Explain { entity, json } => {
                let json = json || args.json;
                return explain::execute_hotspots_explain(
                    &storage,
                    entity,
                    &repo,
                    &config,
                    args.commits,
                    args.days,
                    json,
                    cancel,
                    overall_deadline,
                    overall_secs,
                    json_out.as_deref_mut(),
                );
            }
            HotspotSubcommands::Budget {
                json,
                threshold,
                fail,
            } => {
                return budget::execute_hotspots_budget(
                    &storage, &repo, &config, json, threshold, fail,
                );
            }
        }
    }

    list::execute_hotspots_list(
        args,
        &storage,
        &repo,
        &config,
        &layout,
        cancel,
        overall_deadline,
        overall_secs,
        json_out,
    )
}

fn emit_overall_skip_open(
    args: &HotspotArgs,
    overall_secs: u64,
    stage: &str,
    stop: CompletenessStop,
    json_out: Option<&mut Vec<u8>>,
) -> Result<()> {
    let json = args.json
        || matches!(
            &args.command,
            Some(HotspotSubcommands::Explain { json: true, .. })
        );
    let completeness = completeness_for_overall(stop, Some(overall_secs).filter(|s| *s > 0), stage);
    eprint_hotspots_overall_stop_if_budget(Some(&completeness));
    if json {
        if let Some(HotspotSubcommands::Explain { entity, .. }) = &args.command {
            let output = explain::explanation_json_envelope(
                entity,
                0,
                0.0,
                None,
                Vec::new(),
                Some("temporal couplings untrusted: overall budget".to_string()),
                Some(&completeness),
            );
            write_json(&output, json_out)?;
        } else {
            let limit = args.limit.unwrap_or(10);
            let output = list::wrap_hotspots_list_json_with_completeness(
                Vec::<serde_json::Value>::new(),
                limit,
                Some(&completeness),
                None,
            );
            write_json(&output, json_out)?;
        }
    } else if matches!(&args.command, Some(HotspotSubcommands::Explain { .. })) {
        let reason = if stop == CompletenessStop::Cancelled {
            "cancelled"
        } else {
            "overall budget"
        };
        println!("Hotspot analysis stopped: {reason} ({stage}).");
    }
    Ok(())
}

pub(super) fn eprint_hotspots_overall_stop() {
    eprintln!("{HOTSPOTS_BUDGET_WARN}");
}

pub(super) fn should_eprint_hotspots_overall_stop(
    completeness: Option<&crate::impact::budget::AnalysisCompleteness>,
) -> bool {
    completeness.is_some_and(|c| c.stop == CompletenessStop::Budget && is_overall_stop(c))
}

pub(super) fn eprint_hotspots_overall_stop_if_budget(
    completeness: Option<&crate::impact::budget::AnalysisCompleteness>,
) {
    if should_eprint_hotspots_overall_stop(completeness) {
        eprint_hotspots_overall_stop();
    }
}

pub(super) fn write_json(value: &serde_json::Value, json_out: Option<&mut Vec<u8>>) -> Result<()> {
    if let Some(buf) = json_out {
        let encoded = serde_json::to_vec(value)
            .map_err(|e| miette::miette!("Failed to serialize hotspots JSON: {e}"))?;
        buf.write_all(&encoded)
            .map_err(|e| miette::miette!("Failed to write hotspots JSON: {e}"))?;
        buf.write_all(b"\n")
            .map_err(|e| miette::miette!("Failed to write hotspots JSON: {e}"))?;
        Ok(())
    } else {
        crate::output::json::emit(value).map_err(|e| miette::miette!("{e}"))
    }
}
