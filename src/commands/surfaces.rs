//! `ledgerful surfaces` (alias `tour`) — read-only advanced-surface inventory (0185).
//!
//! Closed six-row map: ready / empty / gated. Ready is live-command index/config
//! data only; filesystem presence only chooses empty `reason` / `next`.

use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::config::model::{Config, ServiceInferenceState};
use crate::index::staleness::check_index_staleness;
use crate::output::table::build_premium_table;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use serde::Serialize;

/// Default `data-models list` confidence floor — ready must match that command.
const DATA_MODELS_LIST_MIN_CONFIDENCE: f64 = 0.5;

/// Wire form: `"gated" | "empty" | "ready"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SurfaceStatus {
    Gated,
    Empty,
    Ready,
}

impl SurfaceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gated => "gated",
            Self::Empty => "empty",
            Self::Ready => "ready",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceItem {
    pub id: String,
    pub name: String,
    pub command: String,
    pub status: SurfaceStatus,
    pub gate: String,
    pub reason: String,
    pub next: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceCounts {
    pub ready: usize,
    pub empty: usize,
    pub gated: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfacesReport {
    pub schema_version: u32,
    pub kind: String,
    pub coverage_enabled: bool,
    pub counts: SurfaceCounts,
    pub surfaces: Vec<SurfaceItem>,
}

/// Pure probe snapshot — unit tests classify without I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceProbes {
    pub coverage_enabled: bool,
    pub service_inference: ServiceInferenceState,
    pub deploy_enabled: bool,
    pub indexed_service_rows: usize,
    pub declared_services_assigned: usize,
    pub deploy_manifest_rows: usize,
    pub cedar_auth_nodes: usize,
    pub slo_nodes: usize,
    pub env_declarations: usize,
    pub data_models: usize,
    pub index_missing: bool,
    pub index_stale: bool,
    pub policies_cedar_on_disk: bool,
    pub openslo_on_disk: bool,
    pub env_example_on_disk: bool,
}

impl SurfaceProbes {
    pub fn default_mint() -> Self {
        Self {
            coverage_enabled: false,
            service_inference: ServiceInferenceState::DisabledGlobally,
            deploy_enabled: false,
            indexed_service_rows: 0,
            declared_services_assigned: 0,
            deploy_manifest_rows: 0,
            cedar_auth_nodes: 0,
            slo_nodes: 0,
            env_declarations: 0,
            data_models: 1,
            index_missing: false,
            index_stale: false,
            policies_cedar_on_disk: false,
            openslo_on_disk: false,
            env_example_on_disk: false,
        }
    }
}

/// Classify the closed six-surface inventory from a probe snapshot.
pub fn classify_from_probes(probes: &SurfaceProbes) -> SurfacesReport {
    let surfaces = vec![
        classify_services(probes),
        classify_deploy(probes),
        classify_security(probes),
        classify_observability(probes),
        classify_schema(probes),
        classify_data_models(probes),
    ];
    let counts = count_statuses(&surfaces);
    SurfacesReport {
        schema_version: 1,
        kind: "surfaces".to_string(),
        coverage_enabled: probes.coverage_enabled,
        counts,
        surfaces,
    }
}

fn count_statuses(surfaces: &[SurfaceItem]) -> SurfaceCounts {
    let mut counts = SurfaceCounts {
        ready: 0,
        empty: 0,
        gated: 0,
    };
    for s in surfaces {
        match s.status {
            SurfaceStatus::Ready => counts.ready += 1,
            SurfaceStatus::Empty => counts.empty += 1,
            SurfaceStatus::Gated => counts.gated += 1,
        }
    }
    counts
}

fn classify_services(probes: &SurfaceProbes) -> SurfaceItem {
    let (status, gate, reason, next) = match probes.service_inference {
        ServiceInferenceState::DisabledGlobally => (
            SurfaceStatus::Gated,
            "coverage.global",
            "Service inference disabled by coverage.enabled=false",
            "ledgerful config set coverage.enabled=true",
        ),
        ServiceInferenceState::DisabledForServices => (
            SurfaceStatus::Gated,
            "coverage.services",
            "Service inference disabled by coverage.services.enabled=false",
            "ledgerful config set coverage.services.enabled=true",
        ),
        ServiceInferenceState::Enabled => {
            let has_rows = probes.indexed_service_rows > 0 || probes.declared_services_assigned > 0;
            if has_rows {
                (
                    SurfaceStatus::Ready,
                    "none",
                    "Indexed service rows present",
                    "ledgerful services",
                )
            } else if probes.index_missing || probes.index_stale {
                (
                    SurfaceStatus::Empty,
                    "content",
                    if probes.index_stale {
                        "Index stale"
                    } else {
                        "Index never built"
                    },
                    "ledgerful index --incremental",
                )
            } else {
                (
                    SurfaceStatus::Empty,
                    "content",
                    "No declared or indexed services",
                    "Declare [services] in .ledgerful/config.toml",
                )
            }
        }
    };

    item(
        "services",
        "Services",
        "ledgerful services",
        status,
        gate,
        reason,
        next,
    )
}

fn classify_deploy(probes: &SurfaceProbes) -> SurfaceItem {
    let (status, gate, reason, next) = if !probes.coverage_enabled {
        (
            SurfaceStatus::Gated,
            "coverage.global",
            "Deploy detection disabled by coverage.enabled=false",
            "ledgerful config set coverage.enabled=true",
        )
    } else if !probes.deploy_enabled {
        (
            SurfaceStatus::Gated,
            "coverage.deploy",
            "Deploy detection disabled by coverage.deploy.enabled=false",
            "ledgerful config set coverage.deploy.enabled=true",
        )
    } else if probes.deploy_manifest_rows > 0 {
        (
            SurfaceStatus::Ready,
            "none",
            "Indexed deploy manifests present",
            "ledgerful deploy",
        )
    } else {
        (
            SurfaceStatus::Empty,
            "content",
            "No indexed deploy manifests",
            "ledgerful index --incremental",
        )
    };

    item(
        "deploy",
        "Deploy",
        "ledgerful deploy",
        status,
        gate,
        reason,
        next,
    )
}

fn classify_security(probes: &SurfaceProbes) -> SurfaceItem {
    let (status, reason, next) = if probes.cedar_auth_nodes > 0 {
        (
            SurfaceStatus::Ready,
            "Cedar/auth nodes present",
            "ledgerful security boundaries",
        )
    } else if probes.policies_cedar_on_disk {
        (
            SurfaceStatus::Empty,
            "Cedar files on disk but not in the graph",
            "ledgerful index --analyze-graph",
        )
    } else {
        (
            SurfaceStatus::Empty,
            "No repo-root policies/*.cedar",
            "add policies/ then ledgerful index --analyze-graph",
        )
    };

    item(
        "security",
        "Security",
        "ledgerful security boundaries",
        status,
        "content",
        reason,
        next,
    )
}

fn classify_observability(probes: &SurfaceProbes) -> SurfaceItem {
    let (status, reason, next) = if probes.slo_nodes > 0 {
        (
            SurfaceStatus::Ready,
            "SLO nodes present",
            "ledgerful observability coverage",
        )
    } else if probes.openslo_on_disk {
        (
            SurfaceStatus::Empty,
            "OpenSLO files on disk but not in the graph",
            "ledgerful index --analyze-graph",
        )
    } else {
        (
            SurfaceStatus::Empty,
            "No repo-root observability/ OpenSLO YAML",
            "add observability/ then ledgerful index --analyze-graph",
        )
    };

    item(
        "observability",
        "Observability",
        "ledgerful observability coverage",
        status,
        "content",
        reason,
        next,
    )
}

fn classify_schema(probes: &SurfaceProbes) -> SurfaceItem {
    let (status, reason, next) = if probes.env_declarations > 0 {
        (
            SurfaceStatus::Ready,
            "Env declarations present",
            "ledgerful config schema",
        )
    } else if probes.index_stale {
        (
            SurfaceStatus::Empty,
            "Index stale",
            "ledgerful index --incremental",
        )
    } else if probes.index_missing {
        (
            SurfaceStatus::Empty,
            "Index never built",
            "ledgerful index --incremental",
        )
    } else if probes.env_example_on_disk {
        (
            SurfaceStatus::Empty,
            "Env example present but not indexed",
            "ledgerful index --incremental",
        )
    } else {
        (
            SurfaceStatus::Empty,
            "No env declarations",
            "add .env.example then ledgerful index --incremental",
        )
    };

    item(
        "schema",
        "Config schema",
        "ledgerful config schema",
        status,
        "content",
        reason,
        next,
    )
}

fn classify_data_models(probes: &SurfaceProbes) -> SurfaceItem {
    let (status, reason, next) = if probes.data_models > 0 {
        (
            SurfaceStatus::Ready,
            "Extracted data models present",
            "ledgerful data-models list",
        )
    } else {
        (
            SurfaceStatus::Empty,
            "No extracted data models",
            "ledgerful index --incremental",
        )
    };

    item(
        "data-models",
        "Data models",
        "ledgerful data-models list",
        status,
        "none",
        reason,
        next,
    )
}

fn item(
    id: &str,
    name: &str,
    command: &str,
    status: SurfaceStatus,
    gate: &str,
    reason: &str,
    next: &str,
) -> SurfaceItem {
    SurfaceItem {
        id: id.to_string(),
        name: name.to_string(),
        command: command.to_string(),
        status,
        gate: gate.to_string(),
        reason: reason.to_string(),
        next: next.to_string(),
    }
}

/// Gated surface ids in §3.2 table order (no extra sort, no cap).
pub fn gated_ids(report: &SurfacesReport) -> Vec<&str> {
    report
        .surfaces
        .iter()
        .filter(|s| s.status == SurfaceStatus::Gated)
        .map(|s| s.id.as_str())
        .collect()
}

/// Gather live probes and classify. Read-only: no config write, no index rebuild.
/// SQLite probe errors propagate (not silently counted as empty).
pub fn classify_surfaces(
    config: &Config,
    layout: &Layout,
    storage: &StorageManager,
) -> Result<SurfacesReport> {
    let probes = gather_probes(config, layout, storage)?;
    Ok(classify_from_probes(&probes))
}

fn gather_probes(
    config: &Config,
    layout: &Layout,
    storage: &StorageManager,
) -> Result<SurfaceProbes> {
    let conn = storage.get_connection();
    let stale = check_index_staleness(storage, config.index.stale_threshold_days);
    let (index_missing, index_stale) = match stale {
        Some(w) if w.is_missing => (true, false),
        Some(_) => (false, true),
        None => (false, false),
    };

    Ok(SurfaceProbes {
        coverage_enabled: config.coverage.enabled,
        service_inference: config.coverage.service_inference_state(),
        deploy_enabled: config.coverage.deploy.enabled,
        indexed_service_rows: count_sql(
            conn,
            "SELECT COUNT(DISTINCT service_name) FROM project_files WHERE service_name IS NOT NULL",
        )?,
        declared_services_assigned: count_declared_assigned(conn, config)?,
        deploy_manifest_rows: count_sql(conn, "SELECT COUNT(*) FROM deploy_manifests")?,
        cedar_auth_nodes: count_cozo_auth_nodes(storage)?,
        slo_nodes: count_cozo_slo_nodes(storage)?,
        env_declarations: count_sql(conn, "SELECT COUNT(*) FROM env_declarations")?,
        data_models: count_data_models_threshold(conn)?,
        index_missing,
        index_stale,
        policies_cedar_on_disk: repo_root_cedar_present(&layout.root),
        openslo_on_disk: repo_root_openslo_present(&layout.root),
        env_example_on_disk: layout.root.join(".env.example").is_file(),
    })
}

fn count_data_models_threshold(conn: &rusqlite::Connection) -> Result<usize> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM data_models dm \
             INNER JOIN project_files pf ON dm.model_file_id = pf.id \
             WHERE dm.confidence >= ?1",
            [DATA_MODELS_LIST_MIN_CONFIDENCE],
            |row| row.get(0),
        )
        .into_diagnostic()?;
    Ok(n.max(0) as usize)
}

fn count_sql(conn: &rusqlite::Connection, sql: &str) -> Result<usize> {
    let n: i64 = conn
        .query_row(sql, [], |row| row.get(0))
        .into_diagnostic()?;
    Ok(n.max(0) as usize)
}

fn count_declared_assigned(conn: &rusqlite::Connection, config: &Config) -> Result<usize> {
    let mut assigned = 0usize;
    for def in &config.services.definitions {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_files WHERE service_name = ?1",
                [&def.name],
                |row| row.get(0),
            )
            .into_diagnostic()?;
        if n > 0 {
            assigned += 1;
        }
    }
    Ok(assigned)
}

fn count_cozo_auth_nodes(storage: &StorageManager) -> Result<usize> {
    let Some(cozo) = storage.cozo.as_ref() else {
        return Ok(0);
    };
    let res = cozo.run_script(
        "?[id] := *node{id, category}, category in ['policy', 'principal', 'action', 'resource']",
    )?;
    Ok(res.rows.len())
}

fn count_cozo_slo_nodes(storage: &StorageManager) -> Result<usize> {
    let Some(cozo) = storage.cozo.as_ref() else {
        return Ok(0);
    };
    let res = cozo.run_script("?[id] := *node{id, category: 'slo'}")?;
    Ok(res.rows.len())
}

/// Repo-root `policies/*.cedar` only — never `tests/fixtures/**`.
pub fn repo_root_cedar_present(root: &camino::Utf8Path) -> bool {
    let dir = root.join("policies");
    let Ok(entries) = std::fs::read_dir(dir.as_std_path()) else {
        return false;
    };
    entries.flatten().any(|e| {
        let path = e.path();
        path.is_file() && path.extension().and_then(|x| x.to_str()) == Some("cedar")
    })
}

/// Repo-root `observability/*.{yml,yaml}` only — never fixture trees.
pub fn repo_root_openslo_present(root: &camino::Utf8Path) -> bool {
    let dir = root.join("observability");
    let Ok(entries) = std::fs::read_dir(dir.as_std_path()) else {
        return false;
    };
    entries.flatten().any(|e| {
        let path = e.path();
        if !path.is_file() {
            return false;
        }
        matches!(
            path.extension().and_then(|x| x.to_str()),
            Some("yml") | Some("yaml")
        )
    })
}

pub fn execute_surfaces(json: bool) -> Result<()> {
    let layout = get_layout()?;
    let config = load_ledger_config(&layout)?;
    let storage = StorageManager::open_read_only(&layout)?;
    let report = classify_surfaces(&config, &layout, &storage)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).into_diagnostic()?
        );
    } else {
        print_human_report(&report);
    }
    Ok(())
}

fn print_human_report(report: &SurfacesReport) {
    let mut table = build_premium_table(["Surface", "Status", "Why", "Next"]);
    for s in &report.surfaces {
        table.add_row(vec![
            s.name.as_str(),
            s.status.as_str(),
            s.reason.as_str(),
            s.next.as_str(),
        ]);
    }
    println!("{table}");
    println!(
        "{} gated · {} empty · {} ready",
        report.counts.gated, report.counts.empty, report.counts.ready
    );
    if report.counts.gated == 0 && report.counts.empty == 0 && report.counts.ready == 6 {
        println!("All listed advanced surfaces are populated.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids_of(report: &SurfacesReport) -> Vec<&str> {
        report.surfaces.iter().map(|s| s.id.as_str()).collect()
    }

    fn status_of(report: &SurfacesReport, id: &str) -> SurfaceStatus {
        report
            .surfaces
            .iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("missing surface {id}"))
            .status
    }

    fn item_of<'a>(report: &'a SurfacesReport, id: &str) -> &'a SurfaceItem {
        report
            .surfaces
            .iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("missing surface {id}"))
    }

    #[test]
    fn mint_default_is_two_gated_three_empty_one_ready() {
        let report = classify_from_probes(&SurfaceProbes::default_mint());
        assert_eq!(
            ids_of(&report),
            [
                "services",
                "deploy",
                "security",
                "observability",
                "schema",
                "data-models"
            ]
        );
        assert_eq!(report.schema_version, 1);
        assert_eq!(report.kind, "surfaces");
        assert!(!report.coverage_enabled);
        assert_eq!(report.counts.gated, 2);
        assert_eq!(report.counts.empty, 3);
        assert_eq!(report.counts.ready, 1);
        assert_eq!(status_of(&report, "services"), SurfaceStatus::Gated);
        assert_eq!(status_of(&report, "deploy"), SurfaceStatus::Gated);
        assert_eq!(status_of(&report, "security"), SurfaceStatus::Empty);
        assert_eq!(status_of(&report, "observability"), SurfaceStatus::Empty);
        assert_eq!(status_of(&report, "schema"), SurfaceStatus::Empty);
        assert_eq!(status_of(&report, "data-models"), SurfaceStatus::Ready);
        assert_eq!(gated_ids(&report), ["services", "deploy"]);
        assert_eq!(
            item_of(&report, "services").next,
            "ledgerful config set coverage.enabled=true"
        );
        assert_eq!(item_of(&report, "services").gate, "coverage.global");
        assert_eq!(item_of(&report, "deploy").gate, "coverage.global");
        assert_eq!(item_of(&report, "security").gate, "content");
        assert_eq!(item_of(&report, "data-models").gate, "none");
        assert_eq!(
            item_of(&report, "data-models").next,
            "ledgerful data-models list"
        );
    }

    #[test]
    fn coverage_on_without_content_is_empty_not_gated() {
        let mut probes = SurfaceProbes::default_mint();
        probes.coverage_enabled = true;
        probes.service_inference = ServiceInferenceState::Enabled;
        probes.deploy_enabled = true;
        probes.data_models = 0;
        let report = classify_from_probes(&probes);
        assert_eq!(status_of(&report, "services"), SurfaceStatus::Empty);
        assert_eq!(status_of(&report, "deploy"), SurfaceStatus::Empty);
        assert_eq!(report.counts.gated, 0);
        assert_eq!(report.counts.empty, 6);
        assert_eq!(item_of(&report, "services").gate, "content");
        assert_eq!(item_of(&report, "deploy").gate, "content");
    }

    #[test]
    fn cedar_on_disk_without_graph_is_empty_not_ready() {
        let mut probes = SurfaceProbes::default_mint();
        probes.policies_cedar_on_disk = true;
        probes.cedar_auth_nodes = 0;
        let report = classify_from_probes(&probes);
        assert_eq!(status_of(&report, "security"), SurfaceStatus::Empty);
        assert_eq!(
            item_of(&report, "security").next,
            "ledgerful index --analyze-graph"
        );
    }

    #[test]
    fn empty_next_uses_add_path_when_repo_root_files_absent() {
        let report = classify_from_probes(&SurfaceProbes::default_mint());
        assert_eq!(
            item_of(&report, "security").next,
            "add policies/ then ledgerful index --analyze-graph"
        );
        assert_eq!(
            item_of(&report, "observability").next,
            "add observability/ then ledgerful index --analyze-graph"
        );
        assert_eq!(
            item_of(&report, "schema").next,
            "add .env.example then ledgerful index --incremental"
        );
    }

    #[test]
    fn enabled_services_current_index_empty_points_at_declare() {
        let mut probes = SurfaceProbes::default_mint();
        probes.coverage_enabled = true;
        probes.service_inference = ServiceInferenceState::Enabled;
        probes.index_missing = false;
        probes.index_stale = false;
        let report = classify_from_probes(&probes);
        assert_eq!(status_of(&report, "services"), SurfaceStatus::Empty);
        assert_eq!(
            item_of(&report, "services").next,
            "Declare [services] in .ledgerful/config.toml"
        );
    }

    #[test]
    fn cedar_graph_nodes_are_ready() {
        let mut probes = SurfaceProbes::default_mint();
        probes.cedar_auth_nodes = 2;
        let report = classify_from_probes(&probes);
        assert_eq!(status_of(&report, "security"), SurfaceStatus::Ready);
        assert_eq!(
            item_of(&report, "security").next,
            "ledgerful security boundaries"
        );
    }

    #[test]
    fn env_example_on_disk_without_rows_is_empty_index_next() {
        let mut probes = SurfaceProbes::default_mint();
        probes.env_example_on_disk = true;
        probes.env_declarations = 0;
        let report = classify_from_probes(&probes);
        assert_eq!(status_of(&report, "schema"), SurfaceStatus::Empty);
        assert_eq!(
            item_of(&report, "schema").next,
            "ledgerful index --incremental"
        );
    }

    #[test]
    fn stale_index_schema_reason_is_index_stale() {
        let mut probes = SurfaceProbes::default_mint();
        probes.index_stale = true;
        probes.env_declarations = 0;
        let report = classify_from_probes(&probes);
        assert_eq!(status_of(&report, "schema"), SurfaceStatus::Empty);
        assert_eq!(item_of(&report, "schema").reason, "Index stale");
        assert_eq!(
            item_of(&report, "schema").next,
            "ledgerful index --incremental"
        );
    }

    #[test]
    fn data_models_rows_are_ready() {
        let mut probes = SurfaceProbes::default_mint();
        probes.data_models = 4;
        let report = classify_from_probes(&probes);
        assert_eq!(status_of(&report, "data-models"), SurfaceStatus::Ready);
    }

    #[test]
    fn data_models_zero_is_empty() {
        let mut probes = SurfaceProbes::default_mint();
        probes.data_models = 0;
        let report = classify_from_probes(&probes);
        assert_eq!(status_of(&report, "data-models"), SurfaceStatus::Empty);
    }

    #[test]
    fn services_enabled_with_indexed_rows_is_ready() {
        let mut probes = SurfaceProbes::default_mint();
        probes.coverage_enabled = true;
        probes.service_inference = ServiceInferenceState::Enabled;
        probes.indexed_service_rows = 3;
        let report = classify_from_probes(&probes);
        assert_eq!(status_of(&report, "services"), SurfaceStatus::Ready);
        assert_eq!(item_of(&report, "services").next, "ledgerful services");
    }

    #[test]
    fn services_disabled_for_services_only_gates_services_key() {
        let mut probes = SurfaceProbes::default_mint();
        probes.coverage_enabled = true;
        probes.service_inference = ServiceInferenceState::DisabledForServices;
        probes.deploy_enabled = true;
        let report = classify_from_probes(&probes);
        assert_eq!(status_of(&report, "services"), SurfaceStatus::Gated);
        assert_eq!(item_of(&report, "services").gate, "coverage.services");
        assert_eq!(
            item_of(&report, "services").next,
            "ledgerful config set coverage.services.enabled=true"
        );
        assert_eq!(status_of(&report, "deploy"), SurfaceStatus::Empty);
        assert_eq!(gated_ids(&report), ["services"]);
    }

    #[test]
    fn json_envelope_camel_case() {
        let report = classify_from_probes(&SurfaceProbes::default_mint());
        let v = serde_json::to_value(&report).expect("serialize");
        assert_eq!(v["schemaVersion"], 1);
        assert_eq!(v["kind"], "surfaces");
        assert_eq!(v["coverageEnabled"], false);
        assert_eq!(v["counts"]["ready"], 1);
        assert_eq!(v["counts"]["empty"], 3);
        assert_eq!(v["counts"]["gated"], 2);
        assert!(v["surfaces"][0].get("id").is_some());
        assert!(v["surfaces"][0].get("status").is_some());
        assert!(v.get("schema_version").is_none());
        assert!(v.get("coverage_enabled").is_none());
    }

    #[test]
    fn repo_root_cedar_ignores_fixtures_and_non_cedar() {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        std::fs::create_dir_all(root.join("tests/fixtures/policies")).unwrap();
        std::fs::write(root.join("tests/fixtures/policies/deny.cedar"), "permit();").unwrap();
        assert!(
            !repo_root_cedar_present(&root),
            "fixture cedar must not count"
        );
        std::fs::create_dir_all(root.join("policies")).unwrap();
        std::fs::write(root.join("policies/readme.txt"), "nope").unwrap();
        assert!(!repo_root_cedar_present(&root));
        std::fs::write(root.join("policies/app.cedar"), "permit();").unwrap();
        assert!(repo_root_cedar_present(&root));
    }

    #[test]
    fn all_ready_human_banner_condition() {
        let mut probes = SurfaceProbes::default_mint();
        probes.coverage_enabled = true;
        probes.service_inference = ServiceInferenceState::Enabled;
        probes.deploy_enabled = true;
        probes.indexed_service_rows = 1;
        probes.deploy_manifest_rows = 1;
        probes.cedar_auth_nodes = 1;
        probes.slo_nodes = 1;
        probes.env_declarations = 1;
        probes.data_models = 1;
        let report = classify_from_probes(&probes);
        assert_eq!(report.counts.ready, 6);
        assert_eq!(report.counts.gated, 0);
        assert_eq!(report.counts.empty, 0);
    }
}
