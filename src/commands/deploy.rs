use crate::commands::helpers::{get_layout, get_repo_root};
use crate::config::load::load_config;
use crate::impact::packet::{DeployManifestChange, ManifestType};
use crate::output::empty::{EmptyReason, config_enable_hint, format_json_empty_state};
use crate::output::table::Table;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use clap::{Args, Subcommand};
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;

#[derive(Args, Debug)]
pub struct DeployArgs {
    #[command(subcommand)]
    pub command: DeploySubcommands,
}

#[derive(Subcommand, Debug)]
pub enum DeploySubcommands {
    /// Show impact of changes on deployment manifests
    Impact {
        /// Deprecated: `deploy impact` always reflects changed manifests in
        /// the current diff; this flag is kept for backward compatibility and
        /// has no effect.
        #[arg(long)]
        changed: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub fn execute_deploy(args: DeployArgs) -> Result<()> {
    // Determine the project root. Outside a git repository we fall back to the
    // current directory so `deploy impact` still surfaces the config-gated
    // empty-state message instead of erroring — the OLD code path read from
    // SQLite and did not require a git repo, so we preserve that behavior.
    let current_dir = std::env::current_dir().into_diagnostic()?;
    // Discriminate "not a git repo" (`RepoDiscoveryFailed`) from a broken
    // repo (`RepoOpenFailed`, e.g. corrupt `.git` or permission failure).
    // Only the former falls back to the config-gated empty state; every other
    // git error surfaces to the caller instead of silently reporting a clean
    // empty state (BLOCKER 2).
    let in_git_repo = match crate::git::repo::open_repo(&current_dir) {
        Ok(_) => true,
        Err(crate::git::GitError::RepoDiscoveryFailed { .. }) => false,
        Err(e) => return Err(e.into()),
    };
    let root = if in_git_repo {
        get_repo_root()?
    } else {
        camino::Utf8PathBuf::from_path_buf(current_dir)
            .map_err(|_| miette::miette!("Current directory is not valid UTF-8"))?
    };
    let layout = Layout::new(&root);
    let config = load_config(&layout).unwrap_or_default();
    // Initialize the storage at the repo-root layout's ledger path so the
    // deploy enrichment uses the SAME repo-root `config` for gating as for
    // storage (BLOCKER 1) and avoids the snapshot-persist + report-rewrite
    // side effects of `execute_impact_silent` (SHOULD-FIX 1) by routing
    // through the non-persisting `compute_impact_in_memory`. Storage is only
    // needed on the in-repo path (the non-repo fallback emits the config-gated
    // empty state without analyzing a diff and must not require a
    // `.ledgerful/state/` directory to exist).
    let storage = if in_git_repo {
        let db_path = layout.state_subdir().join("ledger.db");
        Some(StorageManager::init(db_path.as_std_path())?)
    } else {
        None
    };

    let result: Result<()> = (|| {
        match args.command {
            DeploySubcommands::Impact { changed: _, json } => {
                // The deploy enrichment that populates `deploy_manifest_changes`
                // on the impact packet is gated by `coverage.enabled` AND
                // `coverage.deploy.enabled` (see `src/impact/enrichment/deploy.rs`).
                // We run the non-persisting in-memory impact pipeline
                // (`compute_impact_in_memory`) feeding the SAME repo-root
                // `config` and `storage` we resolved above, so the empty state
                // reflects the actual gating policy and the CWD/repo-root split
                // is eliminated (BLOCKER 1). When the gate is OFF, reindexing
                // cannot change the outcome, so we emit a config hint instead of
                // telling the user to reindex.
                //
                // `deploy_manifest_changes` already contains only deploy
                // manifests that appear in the current diff (the enrichment
                // detects them from `packet.changes`), so the `--changed` flag
                // is redundant and kept only for CLI backward compatibility.
                let manifests: Vec<DeployManifestChange> = if let Some(storage) = storage.as_ref() {
                    // Route through the repo-root-aware variant so deploy
                    // manifest detection resolves root-relative paths (e.g.
                    // `docker-compose.yml`) against the resolved repo workdir
                    // (`root`) instead of CWD. This matters when `deploy impact`
                    // is invoked from a subdirectory: CWD=subdir but the repo
                    // root is the parent, and YAML manifests require a content
                    // read (`project_root.join(&file.path)`) to classify — the
                    // CWD-based helper would read `subdir/<root-level-path>`
                    // and miss them.
                    crate::commands::impact::compute_impact_in_memory_at(
                        storage,
                        &config,
                        root.as_std_path(),
                    )?
                    .deploy_manifest_changes
                } else {
                    // No git repo: no diff to analyze. Fall back to the
                    // config-gated empty state so the command still succeeds
                    // outside a repo.
                    Vec::new()
                };

                if !json && manifests.is_empty() {
                    let (_, msg) = deploy_empty_state_message(&config);
                    println!("  {}", msg.yellow());
                    return Ok(());
                }

                if json {
                    let results: Vec<_> = manifests
                        .iter()
                        .map(|m| {
                            serde_json::json!({
                                "path": m.file.to_string_lossy().replace('\\', "/"),
                                "type": manifest_type_label(&m.manifest_type),
                                "risk_tier": m.risk_tier,
                                "service": m.service_name,
                                "owner": m.owner,
                            })
                        })
                        .collect();
                    let output = format_json_empty_state(results, "results", || {
                        deploy_empty_state_message(&config)
                    });
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&output).into_diagnostic()?
                    );
                } else {
                    println!("{}", "Deployment Manifest Impact".bold().cyan());
                    let mut table = Table::new();
                    table.set_header(vec!["Manifest", "Type", "Risk", "Service", "Owner"]);

                    for m in &manifests {
                        let risk_str = match m.risk_tier {
                            3 => m.risk_tier.to_string().red().to_string(),
                            2 => m.risk_tier.to_string().yellow().to_string(),
                            _ => m.risk_tier.to_string().green().to_string(),
                        };

                        table.add_row(vec![
                            m.file.to_string_lossy().replace('\\', "/"),
                            manifest_type_label(&m.manifest_type),
                            risk_str,
                            m.service_name.clone().unwrap_or_else(|| "-".to_string()),
                            m.owner.clone().unwrap_or_else(|| "-".to_string()),
                        ]);
                    }
                    println!("{}", table);
                }
            }
        }
        Ok(())
    })();
    // Shutdown storage on every return path (success or error) before
    // propagating — mirrors `execute_impact_silent`, but unlike it we also shut
    // down on the error path. On the error path the original error wins; on the
    // success path a shutdown error propagates. Never `unwrap()`/`expect()`.
    let shutdown_result = match storage {
        Some(s) => s.shutdown(),
        None => Ok(()),
    };
    result.and(shutdown_result)
}

/// Maps a `ManifestType` enum variant to the string label used in both the
/// human-readable table and the JSON `type` field, matching the serde variant
/// name so the two surfaces never drift.
fn manifest_type_label(mt: &ManifestType) -> String {
    match mt {
        ManifestType::Dockerfile => "Dockerfile".to_string(),
        ManifestType::DockerCompose => "DockerCompose".to_string(),
        ManifestType::Kubernetes => "Kubernetes".to_string(),
        ManifestType::Terraform => "Terraform".to_string(),
        ManifestType::Helm => "Helm".to_string(),
        ManifestType::CiWorkflow => "CiWorkflow".to_string(),
        ManifestType::Unknown => "Unknown".to_string(),
    }
}

/// Builds the empty-state message for `deploy impact`, consulting the same
/// `coverage.enabled` / `coverage.deploy.enabled` switches the deploy
/// enrichment provider gates on, so the message never tells a user to reindex
/// when reindexing cannot change the outcome.
pub fn deploy_empty_state_message(config: &crate::config::model::Config) -> (EmptyReason, String) {
    if !config.coverage.enabled {
        let hint = config_enable_hint(&["coverage.enabled", "coverage.deploy.enabled"]);
        (
            EmptyReason::DisabledByConfig,
            format!(
                "No deployment impact detected. Deploy manifest detection is disabled by the \
                 global `coverage.enabled = false` switch in `.ledgerful/config.toml` -- \
                 reindexing will not change this. {hint}"
            ),
        )
    } else if !config.coverage.deploy.enabled {
        let hint = config_enable_hint(&["coverage.deploy.enabled"]);
        (
            EmptyReason::DisabledByConfig,
            format!(
                "No deployment impact detected. Deploy manifest detection is disabled by \
                 `coverage.deploy.enabled = false` in `.ledgerful/config.toml` -- reindexing \
                 will not change this. {hint}"
            ),
        )
    } else {
        (
            EmptyReason::NoMatches,
            "No deployment impact detected for current changes.".to_string(),
        )
    }
}

#[derive(Args, Debug)]
pub struct CiArgs {
    #[command(subcommand)]
    pub command: CiSubcommands,
}

#[derive(Subcommand, Debug)]
pub enum CiSubcommands {
    /// Show differences in CI configuration and gates
    Diff {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub fn execute_ci(args: CiArgs) -> Result<()> {
    let layout = get_layout()?;
    let storage = StorageManager::open_read_only(&layout.root)?;
    let conn = storage.get_connection();

    match args.command {
        CiSubcommands::Diff { json } => {
            let mut stmt = conn
                .prepare("SELECT platform, job_name, workflow_name, environment FROM ci_gates")
                .into_diagnostic()?;

            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })
                .into_diagnostic()?;

            if json {
                let mut results = Vec::new();
                for row in rows {
                    let (plat, job, workflow, env) = row.into_diagnostic()?;
                    results.push(serde_json::json!({
                        "platform": plat,
                        "job": job,
                        "workflow": workflow,
                        "environment": env,
                    }));
                }
                println!(
                    "{}",
                    serde_json::to_string_pretty(&results).into_diagnostic()?
                );
            } else {
                println!("{}", "CI Gate Summary".bold().cyan());
                let mut table = Table::new();
                table.set_header(vec!["Platform", "Job", "Workflow", "Environment"]);

                for row in rows {
                    let (plat, job, workflow, env) = row.into_diagnostic()?;
                    table.add_row(vec![
                        plat,
                        job,
                        workflow.unwrap_or_else(|| "-".to_string()),
                        env.unwrap_or_else(|| "-".to_string()),
                    ]);
                }
                println!("{}", table);
            }
        }
    }

    Ok(())
}
