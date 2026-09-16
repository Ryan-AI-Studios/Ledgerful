use crate::commands::helpers::get_layout;
use crate::config::load::load_config_or_default_warn;
use crate::coverage::deploy::detect_deploy_manifest_changes;
use crate::git::ignore::filter_ignored_changes;
use crate::git::repo::open_repo;
use crate::git::status::get_repo_status;
use crate::git::{ChangeType, FileChange};
use crate::impact::budget::{
    AnalysisCompleteness, CompletenessStop, DEPLOY_BUDGET_WARN, completeness_for_overall,
    install_cancel_flag, overall_deadline_fired, resolve_deploy_overall_budget_secs,
};
use crate::impact::packet::{ChangedFile, DeployManifestChange, ManifestType};
use crate::output::empty::{
    EmptyReason, config_enable_hint, format_json_empty_state, format_json_list_envelope,
};
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
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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
            timeout: None,
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
        /// Overall deploy impact emit budget in seconds. `0` disables that clock (Ctrl-C still works).
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,
    },
}

pub fn execute_deploy(args: DeployArgs) -> Result<()> {
    let current_dir = std::env::current_dir().into_diagnostic()?;
    let in_git_repo = match open_repo(&current_dir) {
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
    match args.command_or_default() {
        DeploySubcommands::Impact {
            changed: _,
            json,
            timeout,
        } => {
            let mut stdout = std::io::stdout();
            let mut stderr = std::io::stderr();
            execute_deploy_impact_in(
                &layout,
                in_git_repo,
                json,
                timeout,
                None,
                None,
                &mut stdout,
                &mut stderr,
            )
        }
    }
}

/// Cheap `deploy impact` (0358): no SQLite, no impact orchestrator.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_deploy_impact_in(
    layout: &Layout,
    in_git_repo: bool,
    json: bool,
    timeout: Option<u64>,
    cancel: Option<Arc<AtomicBool>>,
    overall_deadline_override: Option<Instant>,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<()> {
    let config = load_config_or_default_warn(layout);
    let gated = !config.coverage.enabled || !config.coverage.deploy.enabled;
    if gated {
        return emit_deploy_outcome(layout, &config, Vec::new(), None, json, stdout);
    }

    let overall_secs =
        resolve_deploy_overall_budget_secs(timeout, config.coverage.deploy.overall_budget_secs);
    let cancel = cancel.unwrap_or_else(install_cancel_flag);
    let deadline = overall_deadline_override
        .or_else(|| (overall_secs > 0).then(|| Instant::now() + Duration::from_secs(overall_secs)));

    if overall_deadline_fired(deadline) || cancel.load(Ordering::Relaxed) {
        let stop = if cancel.load(Ordering::Relaxed) {
            CompletenessStop::Cancelled
        } else {
            CompletenessStop::Budget
        };
        let completeness =
            completeness_for_overall(stop, Some(overall_secs).filter(|s| *s > 0), "deploy");
        writeln!(stderr, "{DEPLOY_BUDGET_WARN}").into_diagnostic()?;
        return emit_deploy_outcome(
            layout,
            &config,
            Vec::new(),
            Some(completeness),
            json,
            stdout,
        );
    }

    let project_root = layout.root.as_std_path();
    let manifests = if in_git_repo {
        detect_enabled_manifests(&config, project_root)?
    } else {
        Vec::new()
    };

    let completeness = if overall_deadline_fired(deadline) || cancel.load(Ordering::Relaxed) {
        let stop = if cancel.load(Ordering::Relaxed) {
            CompletenessStop::Cancelled
        } else {
            CompletenessStop::Budget
        };
        writeln!(stderr, "{DEPLOY_BUDGET_WARN}").into_diagnostic()?;
        Some(completeness_for_overall(
            stop,
            Some(overall_secs).filter(|s| *s > 0),
            "deploy",
        ))
    } else {
        None
    };

    emit_deploy_outcome(layout, &config, manifests, completeness, json, stdout)
}

fn detect_enabled_manifests(
    config: &crate::config::model::Config,
    project_root: &Path,
) -> Result<Vec<DeployManifestChange>> {
    let repo = open_repo(project_root)?;
    let all_changes = get_repo_status(&repo).into_diagnostic()?;
    let changes = filter_ignored_changes(all_changes, &config.watch.ignore_patterns, true)?;
    let changed_files: Vec<ChangedFile> = changes.iter().map(file_change_to_changed_file).collect();
    let mut manifests = detect_deploy_manifest_changes(
        &changed_files,
        &config.coverage.deploy.patterns,
        project_root,
    );
    manifests.sort();
    Ok(manifests)
}

fn file_change_to_changed_file(change: &FileChange) -> ChangedFile {
    let (status, old_path) = match &change.change_type {
        ChangeType::Added => ("Added".to_string(), None),
        ChangeType::Modified => ("Modified".to_string(), None),
        ChangeType::Deleted => ("Deleted".to_string(), None),
        ChangeType::Renamed { old_path } => ("Renamed".to_string(), Some(old_path.clone())),
    };
    ChangedFile {
        path: change.path.clone(),
        status,
        old_path,
        is_staged: change.is_staged,
        ..ChangedFile::default()
    }
}

fn slash_path(raw: &str) -> String {
    raw.replace('\\', "/")
}

fn classifier_labels() -> Vec<String> {
    let mut labels = vec![
        manifest_type_label(&ManifestType::CiWorkflow),
        manifest_type_label(&ManifestType::DockerCompose),
        manifest_type_label(&ManifestType::Dockerfile),
        manifest_type_label(&ManifestType::Helm),
        manifest_type_label(&ManifestType::Kubernetes),
        manifest_type_label(&ManifestType::Terraform),
        manifest_type_label(&ManifestType::Unknown),
    ];
    labels.sort();
    labels
}

fn default_patterns_for_envelope(patterns: &[String]) -> Vec<String> {
    let mut out: Vec<String> = patterns.iter().map(|p| slash_path(p)).collect();
    out.sort();
    out
}

fn attach_deploy_envelope(
    output: &mut serde_json::Value,
    patterns: &[String],
    completeness: Option<&AnalysisCompleteness>,
) {
    if let Some(obj) = output.as_object_mut() {
        obj.insert(
            "defaultPatterns".to_string(),
            serde_json::json!(default_patterns_for_envelope(patterns)),
        );
        obj.insert(
            "classifiers".to_string(),
            serde_json::json!(classifier_labels()),
        );
        if let Some(c) = completeness
            && let Ok(v) = serde_json::to_value(c)
        {
            obj.insert("completeness".to_string(), v);
        }
    }
}

fn manifest_json(m: &DeployManifestChange) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "path".to_string(),
        serde_json::json!(m.file.to_string_lossy().replace('\\', "/")),
    );
    obj.insert(
        "type".to_string(),
        serde_json::json!(manifest_type_label(&m.manifest_type)),
    );
    obj.insert("risk_tier".to_string(), serde_json::json!(m.risk_tier));
    obj.insert("service".to_string(), serde_json::json!(m.service_name));
    obj.insert("owner".to_string(), serde_json::json!(m.owner));
    let coupled: Vec<String> = m
        .coupled_files
        .iter()
        .map(|p| slash_path(p))
        .filter(|p| !p.is_empty())
        .collect();
    if !coupled.is_empty() {
        obj.insert("coupledFiles".to_string(), serde_json::json!(coupled));
    }
    let blast: Vec<String> = m
        .high_blast_resources
        .iter()
        .map(|p| slash_path(p))
        .filter(|p| !p.is_empty())
        .collect();
    if !blast.is_empty() {
        obj.insert("highBlastResources".to_string(), serde_json::json!(blast));
    }
    serde_json::Value::Object(obj)
}

fn coupled_human_line(manifests: &[DeployManifestChange]) -> Option<String> {
    let mut paths: Vec<String> = manifests
        .iter()
        .flat_map(|m| m.coupled_files.iter())
        .map(|p| slash_path(p))
        .filter(|p| !p.is_empty())
        .collect();
    if paths.is_empty() {
        return None;
    }
    paths.sort();
    paths.dedup();
    Some(format!("Coupled: {}", paths.join(", ")))
}

fn emit_deploy_outcome(
    layout: &Layout,
    config: &crate::config::model::Config,
    manifests: Vec<DeployManifestChange>,
    completeness: Option<AnalysisCompleteness>,
    json: bool,
    stdout: &mut impl Write,
) -> Result<()> {
    if json {
        let results: Vec<serde_json::Value> = manifests.iter().map(manifest_json).collect();
        let mut output = if completeness.is_some() && results.is_empty() {
            format_json_list_envelope(results, "results")
        } else {
            format_json_empty_state(results, "results", || deploy_empty_state_message(config))
        };
        let pending_session = if completeness.is_none()
            && output.get("emptyReason").is_some()
            && let Some(id) =
                notice_id_for_deploy(config.coverage.enabled, config.coverage.deploy.enabled)
        {
            let (_, full) = deploy_empty_state_message(config);
            let mut session = CliSession::load(layout, env_session_id().as_deref(), Utc::now());
            let applied = apply_empty_notice(&mut session, id, &full, output);
            output = applied.json;
            Some(session)
        } else {
            None
        };
        attach_deploy_envelope(
            &mut output,
            &config.coverage.deploy.patterns,
            completeness.as_ref(),
        );
        writeln!(
            stdout,
            "{}",
            serde_json::to_string_pretty(&output).into_diagnostic()?
        )
        .into_diagnostic()?;
        if let Some(session) = pending_session {
            session.persist();
        }
        return Ok(());
    }

    if manifests.is_empty() {
        if completeness.is_some() {
            return Ok(());
        }
        let (_, full) = deploy_empty_state_message(config);
        let (msg, pending_session) = if let Some(id) =
            notice_id_for_deploy(config.coverage.enabled, config.coverage.deploy.enabled)
        {
            let mut session = CliSession::load(layout, env_session_id().as_deref(), Utc::now());
            let applied = apply_empty_notice(&mut session, id, &full, serde_json::json!({}));
            (applied.human, Some(session))
        } else {
            (full, None)
        };
        writeln!(
            stdout,
            "  {}",
            msg.if_supports_color(Stream::Stdout, |s| s.yellow())
        )
        .into_diagnostic()?;
        if let Some(session) = pending_session {
            session.persist();
        }
        return Ok(());
    }

    writeln!(
        stdout,
        "{}",
        "Deployment Manifest Impact"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
    )
    .into_diagnostic()?;
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
    writeln!(stdout, "{table}").into_diagnostic()?;
    if let Some(line) = coupled_human_line(&manifests) {
        writeln!(stdout, "{line}").into_diagnostic()?;
    }
    Ok(())
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

    #[test]
    fn deploy_impact_gated_message_does_not_lead_with_no_deployment() {
        let mut config = crate::config::model::Config::default();
        config.coverage.enabled = false;
        let (reason, msg) = super::deploy_empty_state_message(&config);
        assert_eq!(reason, crate::output::empty::EmptyReason::DisabledByConfig);
        assert!(!msg.starts_with("No deployment impact detected"), "{msg}");
        assert!(!msg.starts_with(' '), "{msg}");
    }

    #[test]
    fn deploy_impact_classifiers_sorted_locked_vocab() {
        use crate::impact::packet::ManifestType;
        let mut expected = vec![
            super::manifest_type_label(&ManifestType::CiWorkflow),
            super::manifest_type_label(&ManifestType::DockerCompose),
            super::manifest_type_label(&ManifestType::Dockerfile),
            super::manifest_type_label(&ManifestType::Helm),
            super::manifest_type_label(&ManifestType::Kubernetes),
            super::manifest_type_label(&ManifestType::Terraform),
            super::manifest_type_label(&ManifestType::Unknown),
        ];
        expected.sort();
        assert_eq!(super::classifier_labels(), expected);
    }

    #[test]
    fn deploy_impact_default_patterns_sorted_for_envelope() {
        let unsorted = vec!["**/k8s/**/*.yaml".to_string(), "**/Dockerfile*".to_string()];
        let sorted = super::default_patterns_for_envelope(&unsorted);
        let mut expect = unsorted.clone();
        expect.sort();
        assert_eq!(sorted, expect);
        assert_eq!(unsorted[0], "**/k8s/**/*.yaml");
    }

    #[test]
    fn deploy_impact_timeout_zero_is_unlimited() {
        assert_eq!(
            crate::impact::budget::resolve_deploy_overall_budget_secs(Some(0), 25),
            0
        );
    }

    #[test]
    fn deploy_impact_completeness_overall_stage_is_deploy() {
        let c = crate::impact::budget::completeness_for_overall(
            crate::impact::budget::CompletenessStop::Budget,
            Some(25),
            "deploy",
        );
        let v = serde_json::to_value(&c).expect("json");
        assert_eq!(v["stop"], "budget");
        assert_eq!(v["scope"], "overall");
        assert_eq!(v["stage"], "deploy");
        assert_eq!(v["budgetSecs"], 25);
    }

    #[test]
    fn deploy_impact_elapsed_instant_emits_completeness_without_empty_reason() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8 temp path");
        let cg = root.join(".ledgerful");
        std::fs::create_dir_all(&cg).expect("config dir");
        std::fs::write(
            cg.join("config.toml"),
            "[coverage]\nenabled = true\n\n[coverage.deploy]\nenabled = true\n",
        )
        .expect("config");
        let layout = crate::state::layout::Layout::new(&root);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let elapsed = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap_or_else(std::time::Instant::now);
        super::execute_deploy_impact_in(
            &layout,
            false,
            true,
            None,
            Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            ))),
            Some(elapsed),
            &mut out,
            &mut err,
        )
        .expect("elapsed instant should emit");
        let stderr = String::from_utf8_lossy(&err);
        assert!(
            stderr.contains(crate::impact::budget::DEPLOY_BUDGET_WARN),
            "{stderr}"
        );
        let stdout = String::from_utf8_lossy(&out);
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
        assert_eq!(v["completeness"]["stop"], "budget");
        assert_eq!(v["completeness"]["scope"], "overall");
        assert_eq!(v["completeness"]["stage"], "deploy");
        assert!(v.get("emptyReason").is_none(), "{v}");
        assert!(v.get("defaultPatterns").is_some(), "{v}");
        assert!(v.get("classifiers").is_some(), "{v}");
    }
}
