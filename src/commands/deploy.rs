use crate::commands::helpers::get_layout;
use crate::config::load::load_config;
use crate::impact::packet::{DeployManifestChange, ManifestType};
use crate::output::empty::{EmptyReason, config_enable_hint, format_json_empty_state};
use crate::output::session_notice::{apply_empty_notice, notice_id_for_deploy};
use crate::output::table::Table;
use crate::state::cli_session::{CliSession, env_session_id};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use chrono::Utc;
use clap::{Args, Subcommand};
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};
use serde::Serialize;

#[derive(Args, Debug)]
#[command(after_help = "Default when omitted: impact.")]
pub struct DeployArgs {
    #[command(subcommand)]
    pub command: Option<DeploySubcommands>,
}

impl DeployArgs {
    /// Resolve bare `deploy` to read-only impact with default flags.
    pub fn command_or_default(self) -> DeploySubcommands {
        self.command.unwrap_or(DeploySubcommands::Impact {
            changed: false,
            json: false,
        })
    }
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
    let layout = if in_git_repo {
        get_layout()?
    } else {
        let root = camino::Utf8PathBuf::from_path_buf(current_dir)
            .map_err(|_| miette::miette!("Current directory is not valid UTF-8"))?;
        Layout::new(&root)
    };
    let root = layout.root.clone();
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
        Some(StorageManager::init_with_layout(&layout)?)
    } else {
        None
    };

    let result: Result<()> = (|| {
        match args.command_or_default() {
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
                    let (_, full) = deploy_empty_state_message(&config);
                    let (msg, pending_session) = if let Some(id) = notice_id_for_deploy(
                        config.coverage.enabled,
                        config.coverage.deploy.enabled,
                    ) {
                        let mut session =
                            CliSession::load(&layout, env_session_id().as_deref(), Utc::now());
                        let applied =
                            apply_empty_notice(&mut session, id, &full, serde_json::json!({}));
                        (applied.human, Some(session))
                    } else {
                        (full, None)
                    };
                    println!(
                        "  {}",
                        msg.if_supports_color(Stream::Stdout, |s| s.yellow())
                    );
                    if let Some(session) = pending_session {
                        session.persist();
                    }
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
                    let mut output = format_json_empty_state(results, "results", || {
                        deploy_empty_state_message(&config)
                    });
                    let pending_session = if output.get("emptyReason").is_some()
                        && let Some(id) = notice_id_for_deploy(
                            config.coverage.enabled,
                            config.coverage.deploy.enabled,
                        ) {
                        let (_, full) = deploy_empty_state_message(&config);
                        let mut session =
                            CliSession::load(&layout, env_session_id().as_deref(), Utc::now());
                        let applied = apply_empty_notice(&mut session, id, &full, output);
                        output = applied.json;
                        Some(session)
                    } else {
                        None
                    };
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&output).into_diagnostic()?
                    );
                    if let Some(session) = pending_session {
                        session.persist();
                    }
                } else {
                    println!(
                        "{}",
                        "Deployment Manifest Impact".if_supports_color(Stream::Stdout, |s| s
                            .style(Style::new().bold().cyan()))
                    );
                    let mut table = Table::new();
                    table.set_header(vec!["Manifest", "Type", "Risk", "Service", "Owner"]);

                    for m in &manifests {
                        let risk_str = match m.risk_tier {
                            3 => m
                                .risk_tier
                                .to_string()
                                .if_supports_color(Stream::Stdout, |s| s.red())
                                .to_string(),
                            2 => m
                                .risk_tier
                                .to_string()
                                .if_supports_color(Stream::Stdout, |s| s.yellow())
                                .to_string(),
                            _ => m
                                .risk_tier
                                .to_string()
                                .if_supports_color(Stream::Stdout, |s| s.green())
                                .to_string(),
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
                "Deploy manifest detection is disabled by the global \
                 `coverage.enabled = false` switch in `.ledgerful/config.toml` -- \
                 reindexing will not change this. {hint}"
            ),
        )
    } else if !config.coverage.deploy.enabled {
        let hint = config_enable_hint(&["coverage.deploy.enabled"]);
        (
            EmptyReason::DisabledByConfig,
            format!(
                "Deploy manifest detection is disabled by \
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
#[command(after_help = "Default when omitted: list (alias: diff).")]
pub struct CiArgs {
    #[command(subcommand)]
    pub command: Option<CiSubcommands>,
}

impl CiArgs {
    /// Resolve bare `ci` to read-only list (alias: diff) with default flags.
    pub fn command_or_default(self) -> CiSubcommands {
        self.command.unwrap_or(CiSubcommands::Diff { json: false })
    }
}

#[derive(Subcommand, Debug)]
pub enum CiSubcommands {
    /// List indexed CI gates (inventory; not a working-tree diff)
    #[command(name = "list", visible_alias = "diff")]
    Diff {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub(crate) fn format_ci_inventory_preamble(tree_clean: bool) -> &'static str {
    if tree_clean {
        "No CI configuration changes in the working tree. Showing indexed gates (not a diff of this change)."
    } else {
        "Indexed CI gates (catalog; this command does not diff workflow files)."
    }
}

const CI_DECLARED_SCOPE_SENTENCE: &str =
    "Declared workflow jobs from the index — not GitHub required checks or live run status.";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CiGateJson {
    platform: String,
    job: String,
    workflow: Option<String>,
    environment: Option<String>,
    file_path: String,
    triggers: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    job_if: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    needs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uses: Option<String>,
}

struct CiGateRowOut {
    file_path: String,
    platform: String,
    job: String,
    workflow: Option<String>,
    environment: Option<String>,
    triggers: Vec<String>,
    job_if: Option<String>,
    needs: Vec<String>,
    uses: Option<String>,
}

pub fn execute_ci(args: CiArgs) -> Result<()> {
    let layout = get_layout()?;
    let storage = StorageManager::open_read_only(&layout)?;
    let conn = storage.get_connection();

    match args.command_or_default() {
        CiSubcommands::Diff { json } => {
            let mut stmt = conn
                .prepare(
                    "SELECT pf.file_path, g.platform, g.job_name, g.workflow_name, g.environment, \
                     g.trigger, g.job_if, g.needs, g.uses \
                     FROM ci_gates g \
                     INNER JOIN project_files pf ON pf.id = g.ci_file_id",
                )
                .into_diagnostic()?;

            let query_rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                    ))
                })
                .into_diagnostic()?;

            let mut rows = Vec::new();
            for row in query_rows {
                let (file_path, platform, job, workflow, env, trigger, job_if, needs, uses) =
                    row.into_diagnostic()?;
                let file_path = file_path.replace('\\', "/");
                rows.push(CiGateRowOut {
                    triggers: cli_triggers(&platform, trigger.as_deref()),
                    needs: split_name_list(needs.as_deref()),
                    job_if: empty_to_none(job_if),
                    uses: empty_to_none(uses),
                    file_path,
                    platform,
                    job,
                    workflow,
                    environment: env,
                });
            }
            rows.sort_by(|a, b| {
                (&a.file_path, &a.job, &a.platform).cmp(&(&b.file_path, &b.job, &b.platform))
            });

            if json {
                let results: Vec<CiGateJson> = rows
                    .into_iter()
                    .map(|r| CiGateJson {
                        platform: r.platform,
                        job: r.job,
                        workflow: r.workflow,
                        environment: r.environment,
                        file_path: r.file_path,
                        triggers: r.triggers,
                        job_if: r.job_if,
                        needs: r.needs,
                        uses: r.uses,
                    })
                    .collect();
                let mut output = crate::output::empty::format_json_list_envelope(results, "gates");
                attach_ci_inventory_scope(&mut output);
                println!(
                    "{}",
                    serde_json::to_string_pretty(&output).into_diagnostic()?
                );
            } else {
                let tree_clean = crate::git::status::collect_changed_files_for_filter(&layout)
                    .map(|changes| changes.is_empty())
                    .unwrap_or(false);
                println!("{}", format_ci_inventory_preamble(tree_clean));
                println!(
                    "{}",
                    "CI Gate Inventory"
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
                );
                println!("{CI_DECLARED_SCOPE_SENTENCE}");
                let mut table = Table::new();
                table.set_header(vec![
                    "Platform", "Job", "Workflow", "File", "Trigger", "If", "Needs",
                ]);

                for row in rows {
                    table.add_row(vec![
                        row.platform,
                        row.job,
                        row.workflow.unwrap_or_else(|| "-".to_string()),
                        row.file_path,
                        if row.triggers.is_empty() {
                            "-".to_string()
                        } else {
                            row.triggers.join(", ")
                        },
                        row.job_if.unwrap_or_else(|| "-".to_string()),
                        format_needs_cell(&row.needs, row.uses.as_deref()),
                    ]);
                }
                println!("{}", table);
            }
        }
    }

    Ok(())
}

fn attach_ci_inventory_scope(output: &mut serde_json::Value) {
    if let Some(obj) = output.as_object_mut() {
        obj.insert(
            "scope".to_string(),
            serde_json::json!({
                "inventory": "declaredWorkflowJobs",
                "notIncluded": ["branchProtectionRequired", "liveRunStatus"],
            }),
        );
    }
}

fn cli_triggers(platform: &str, stored: Option<&str>) -> Vec<String> {
    if platform != "github_actions" {
        return Vec::new();
    }
    split_name_list(stored)
}

fn split_name_list(stored: Option<&str>) -> Vec<String> {
    let Some(raw) = stored.map(str::trim).filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = raw
        .split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    names.sort();
    names.dedup();
    names
}

fn empty_to_none(value: Option<String>) -> Option<String> {
    value.and_then(|s| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    })
}

fn format_needs_cell(needs: &[String], uses: Option<&str>) -> String {
    match (needs.is_empty(), uses) {
        (true, None) => "-".to_string(),
        (false, None) => needs.join(", "),
        (true, Some(u)) => format!("uses: {u}"),
        (false, Some(u)) => format!("{}; uses: {u}", needs.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CI_DECLARED_SCOPE_SENTENCE, cli_triggers, format_ci_inventory_preamble, format_needs_cell,
    };

    #[test]
    fn format_ci_inventory_preamble_clean_vs_dirty() {
        assert_eq!(
            format_ci_inventory_preamble(true),
            "No CI configuration changes in the working tree. Showing indexed gates (not a diff of this change)."
        );
        assert_eq!(
            format_ci_inventory_preamble(false),
            "Indexed CI gates (catalog; this command does not diff workflow files)."
        );
    }

    #[test]
    fn cli_triggers_gha_only_splits_events() {
        assert_eq!(
            cli_triggers("github_actions", Some("push, pull_request, schedule")),
            vec![
                "pull_request".to_string(),
                "push".to_string(),
                "schedule".to_string()
            ]
        );
        assert_eq!(
            cli_triggers("makefile", Some("manual")),
            Vec::<String>::new()
        );
        assert_eq!(
            cli_triggers("gitlab_ci", Some("stages: build, test")),
            Vec::<String>::new()
        );
        assert_eq!(cli_triggers("circleci", Some("push")), Vec::<String>::new());
    }

    #[test]
    fn format_needs_cell_pins_uses_in_needs_column() {
        assert_eq!(format_needs_cell(&[], None), "-");
        assert_eq!(format_needs_cell(&["a".into(), "b".into()], None), "a, b");
        assert_eq!(
            format_needs_cell(&[], Some("org/repo/.github/workflows/ci.yml@main")),
            "uses: org/repo/.github/workflows/ci.yml@main"
        );
        assert_eq!(
            format_needs_cell(&["web-build".into()], Some("reusable.yml@main")),
            "web-build; uses: reusable.yml@main"
        );
        assert_eq!(
            CI_DECLARED_SCOPE_SENTENCE,
            "Declared workflow jobs from the index — not GitHub required checks or live run status."
        );
    }
}
