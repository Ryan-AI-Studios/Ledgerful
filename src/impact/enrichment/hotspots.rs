use crate::git::repo::open_repo;
use crate::impact::budget::{
    CompletenessFilter, HotspotProvenance, HotspotProvenanceSource, completeness_for_walk,
};
use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::hotspots::calculate_hotspots_detailed;
use crate::impact::packet::ImpactPacket;
use crate::impact::scope::retain_scoped_hotspots;
use crate::impact::temporal::GixHistoryProvider;
use miette::Result;
use tracing::{debug, warn};

pub struct HotspotProvider;

impl EnrichmentProvider for HotspotProvider {
    fn name(&self) -> &'static str {
        "Hotspot Enrichment Provider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        if context.skip_git_history_enrichment {
            debug!("Skipping hotspot git-history enrichment (session opts)");
            return Ok(());
        }
        debug!("Calculating hotspots...");

        let repo = open_repo(&context.project_root)
            .map_err(|e| miette::miette!("Failed to open repo for hotspot analysis: {}", e))?;

        let history_provider = GixHistoryProvider::new(&repo);

        // The shared scorer truncates to `query.limit`. The packet query asks
        // for the full scored list so a seed outside the global top-N survives,
        // then `retain_scoped_hotspots` applies the configured cap.
        let configured_limit = context.config.hotspots.limit;
        let query = crate::impact::hotspots::HotspotQuery {
            commits: context.config.hotspots.max_commits,
            limit: usize::MAX,
            all_parents: context.config.temporal.all_parents,
            decay_half_life: context.config.hotspots.decay_half_life,
            budget: context.history_budget.clone(),
            skip_unindexed_complexity_fallback: context.overall_deadline.is_some(),
            ..Default::default()
        };
        match calculate_hotspots_detailed(context.storage, &history_provider, &query) {
            Ok(calc) => {
                if let Some(c) = completeness_for_walk(
                    calc.walk_stop,
                    query.commits as u64,
                    calc.commits_walked as u64,
                    query.days,
                    CompletenessFilter::Unfiltered,
                    calc.head.clone(),
                    context
                        .history_budget
                        .as_ref()
                        .and_then(|b| b.budget_secs)
                        .or_else(|| {
                            Some(context.config.hotspots.history_budget_secs).filter(|s| *s > 0)
                        }),
                ) {
                    let msg = format!(
                        "history walk stopped ({}): walked {} of {} commits",
                        match c.stop {
                            crate::impact::budget::CompletenessStop::Budget => "budget",
                            crate::impact::budget::CompletenessStop::Cancelled => "cancelled",
                            crate::impact::budget::CompletenessStop::Error => "error",
                        },
                        calc.commits_walked,
                        query.commits
                    );
                    context.add_warning(msg);
                    packet.completeness = Some(c);
                }
                let scoped = retain_scoped_hotspots(calc.hotspots, packet, configured_limit);
                packet.set_hotspots(scoped);
                packet.hotspot_provenance = Some(HotspotProvenance {
                    source: HotspotProvenanceSource::Live,
                    commits_requested: Some(query.commits as u64),
                    days_requested: query.days,
                    limit: Some(configured_limit as u64),
                    filter: Some(CompletenessFilter::Unfiltered),
                    head: calc.head.clone(),
                    ..HotspotProvenance::default()
                });
            }
            Err(e) => {
                warn!("Hotspot analysis failed: {e}");
                context.add_warning(format!("Hotspot analysis failed: {e}"));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use crate::state::storage::StorageManager;
    use rusqlite::Connection;
    use std::collections::HashMap;
    use std::path::Path;
    use std::process::Command;
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

    fn setup_git_repo(dir: &Path) {
        git_cmd(dir, &["init"]);
        git_cmd(dir, &["config", "user.email", "test@test.com"]);
        git_cmd(dir, &["config", "user.name", "Test User"]);
    }

    fn git_add_and_commit(dir: &Path, msg: &str) {
        git_cmd(dir, &["add", "-A"]);
        git_cmd(dir, &["commit", "-m", msg]);
    }

    fn git_cmd(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("Failed to execute git command");
        if !output.status.success() {
            panic!(
                "git command failed: {:?}\nstderr: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    /// Creates a temporary git repo with 12+ commits of history,
    /// sufficient to clear the 10-commit floor required by
    /// TemporalEngine::calculate_couplings (GitError::InsufficientHistory).
    fn setup_temp_repo_with_history() -> tempfile::TempDir {
        let tmp = tempdir().unwrap();
        let root = tmp.path();

        setup_git_repo(root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn hotspot_fn(x: i32) -> i32 {\n    if x > 0 { x + 1 } else { x - 1 }\n}\n",
        )
        .unwrap();
        git_add_and_commit(root, "initial");

        // TemporalEngine::calculate_couplings requires at least 10 commits of
        // history (see GitError::InsufficientHistory in src/git/mod.rs).
        // Touch the file across enough additional commits to clear that floor with margin.
        for i in 1..=12 {
            std::fs::write(
                root.join("src/lib.rs"),
                format!(
                    "pub fn hotspot_fn(x: i32) -> i32 {{\n    if x > {i} {{ x + 1 }} else {{ x - 1 }}\n}}\n"
                ),
            )
            .unwrap();
            git_add_and_commit(root, &format!("touch {i}"));
        }

        tmp
    }

    #[test]
    fn enrich_calculates_hotspots() {
        let _temp_repo = setup_temp_repo_with_history();
        let temp_root = _temp_repo.path().to_path_buf();

        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let config = crate::config::model::Config::default();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: temp_root,
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
            skip_git_history_enrichment: false,
            history_budget: None,
            overall_deadline: None,
        };
        let mut packet = ImpactPacket::default();

        HotspotProvider.enrich(&context, &mut packet).unwrap();

        assert!(
            packet.hotspots.is_empty(),
            "an empty change list keeps no hotspot row"
        );
        let provenance = packet
            .hotspot_provenance
            .as_ref()
            .expect("a finished calculation still labels the window");
        assert_eq!(provenance.source, HotspotProvenanceSource::Live);
        assert_eq!(
            provenance.commits_requested,
            Some(config.hotspots.max_commits as u64)
        );
        assert_eq!(provenance.limit, Some(config.hotspots.limit as u64));
        assert_eq!(provenance.filter, Some(CompletenessFilter::Unfiltered));
        assert_ne!(provenance.limit, Some(u64::MAX));
    }
}
