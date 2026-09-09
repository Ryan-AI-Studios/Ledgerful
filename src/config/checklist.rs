//! Session config checklist (0301). Closed catalog of applicable gaps.
//!
//! Fact assembly only — no config writes, no SCIP install, no cookie persist
//! inside the builder. [`build_session`](crate::commands::session::build_session)
//! loads the 0300 cookie, calls [`build_config_checklist`], then
//! [`apply_checklist_cookie`]. Track 0302 will import the same builder.

use crate::commands::doctor::{collect_scip_findings, skip_scip_rel_path};
use crate::commands::policy_check::{PolicySource, resolve_policy};
use crate::commands::surfaces::{SurfaceStatus, classify_surfaces, repo_root_cedar_present};
use crate::config::model::{Config, ServiceInferenceState};
use crate::federated::links::present_federated_links;
use crate::federated::storage::get_federated_links;
use crate::output::session_notice::SessionNoticeId;
use crate::state::cli_session::CliSession;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use chrono::{DateTime, Duration, Utc};
use miette::IntoDiagnostic;
use serde::{Deserialize, Serialize};

pub const CHECKLIST_COOKIE_ID: &str = "config.checklist";

const NEXT_COVERAGE_GLOBAL: &str = "ledgerful config set coverage.enabled=true";
const NEXT_COVERAGE_SERVICES: &str = "ledgerful config set coverage.services.enabled=true";
const NEXT_COVERAGE_DEPLOY: &str = "ledgerful config set coverage.deploy.enabled=true";
const NEXT_SERVICES_DECLARE: &str = "Declare [services] in .ledgerful/config.toml";
const NEXT_POLICY_FILE: &str = "add policy.toml for a declared merge gate";
const NEXT_GATE_MODE: &str = "ledgerful gate mode enforce";
const NEXT_FEDERATE: &str = "ledgerful federate scan";
const NEXT_SERVICES_READY: &str = "ledgerful services";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChecklistStatus {
    Ready,
    Gated,
    Empty,
    Optional,
}

impl ChecklistStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Gated => "gated",
            Self::Empty => "empty",
            Self::Optional => "optional",
        }
    }

    fn is_gap(self) -> bool {
        matches!(self, Self::Gated | Self::Empty)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigChecklistItem {
    pub id: String,
    pub status: ChecklistStatus,
    pub applicable: bool,
    pub next: String,
    pub already_shown: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_arg: Option<String>,
}

/// Classify applicable checklist rows. Read-only vs config and index.
///
/// Probe / SQLite / Cozo failures omit HTTP-gated rows (fail-open) and still
/// attempt `policy.file` + `gate.mode`. Never `unwrap`.
pub fn build_config_checklist(
    layout: &Layout,
    storage: &StorageManager,
    config: &Config,
    session: &CliSession,
) -> miette::Result<Vec<ConfigChecklistItem>> {
    let mut items = Vec::new();

    match classify_http_items(layout, storage, config, session) {
        Ok(http_items) => items.extend(http_items),
        Err(e) => {
            tracing::debug!(error = %e, "config checklist: omitting HTTP-gated rows");
        }
    }

    match policy_file_item(layout, session) {
        Ok(item) => items.push(item),
        Err(e) => {
            tracing::debug!(error = %e, "config checklist: omitting policy.file");
        }
    }
    items.push(gate_mode_item(config, session));

    match scip_items(layout, config, session) {
        Ok(scip) => items.extend(scip),
        Err(e) => {
            tracing::debug!(error = %e, "config checklist: omitting SCIP rows");
        }
    }

    match federate_item(layout, storage, config, session) {
        Ok(Some(item)) => items.push(item),
        Ok(None) => {}
        Err(e) => {
            tracing::debug!(error = %e, "config checklist: omitting federate.freshness");
        }
    }

    items.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(items)
}

/// Mark mapped 0300 notice ids + `config.checklist` when any row is gated/empty.
/// Optional-only / empty catalog does not write the cookie.
pub fn apply_checklist_cookie(session: &mut CliSession, items: &[ConfigChecklistItem]) {
    if !items.iter().any(|i| i.status.is_gap()) {
        return;
    }
    session.mark_shown(CHECKLIST_COOKIE_ID);
    for item in items {
        if item.status.is_gap()
            && let Some(id) = mapped_notice_id(item)
        {
            session.mark_shown(id);
        }
    }
    session.persist();
}

fn classify_http_items(
    layout: &Layout,
    storage: &StorageManager,
    config: &Config,
    session: &CliSession,
) -> miette::Result<Vec<ConfigChecklistItem>> {
    let http = has_http_character(layout, storage, config);
    let deploy_rows = match count_deploy_manifests(storage) {
        Ok(n) => n,
        Err(e) => {
            tracing::debug!(
                error = %e,
                "config checklist: deploy_manifests count failed; treating as 0"
            );
            0
        }
    };
    let report = classify_surfaces(config, layout, storage)?;
    let mut items = Vec::new();

    if http || deploy_rows > 0 {
        let gated = !config.coverage.enabled;
        items.push(item(
            "coverage.global",
            if gated {
                ChecklistStatus::Gated
            } else {
                ChecklistStatus::Ready
            },
            if gated { NEXT_COVERAGE_GLOBAL } else { "" },
            already_shown(session, Some(SessionNoticeId::CoverageGlobal.as_str())),
            Some("coverage.global"),
        ));
    }

    if http && config.coverage.enabled {
        let services_gated =
            config.coverage.service_inference_state() == ServiceInferenceState::DisabledForServices;
        items.push(item(
            "coverage.services",
            if services_gated {
                ChecklistStatus::Gated
            } else {
                ChecklistStatus::Ready
            },
            if services_gated {
                NEXT_COVERAGE_SERVICES
            } else {
                ""
            },
            already_shown(session, Some(SessionNoticeId::CoverageServices.as_str())),
            Some("coverage.services"),
        ));

        let has_services = has_declared_or_indexed_services(storage, config);
        items.push(item(
            "services.declare",
            if has_services {
                ChecklistStatus::Ready
            } else {
                ChecklistStatus::Empty
            },
            if has_services {
                NEXT_SERVICES_READY
            } else {
                NEXT_SERVICES_DECLARE
            },
            already_shown(session, None),
            None,
        ));
    }

    if deploy_rows > 0 && config.coverage.enabled {
        let deploy_gated = !config.coverage.deploy.enabled;
        items.push(item(
            "coverage.deploy",
            if deploy_gated {
                ChecklistStatus::Gated
            } else {
                ChecklistStatus::Ready
            },
            if deploy_gated {
                NEXT_COVERAGE_DEPLOY
            } else {
                ""
            },
            already_shown(session, Some(SessionNoticeId::CoverageDeploy.as_str())),
            Some("coverage.deploy"),
        ));
    }

    if http {
        if let Some(obs) = report.surfaces.iter().find(|s| s.id == "observability") {
            let status = surface_to_checklist(obs.status);
            let notice = if status == ChecklistStatus::Empty {
                Some(SessionNoticeId::ObservabilityEmpty.as_str())
            } else {
                None
            };
            items.push(item(
                "observability.openslo",
                status,
                obs.next.as_str(),
                already_shown(session, notice),
                None,
            ));
        }
        if let Some(sec) = report.surfaces.iter().find(|s| s.id == "security") {
            items.push(item(
                "security.cedar",
                surface_to_checklist(sec.status),
                sec.next.as_str(),
                already_shown(session, None),
                None,
            ));
        }
    }

    Ok(items)
}

fn has_http_character(layout: &Layout, storage: &StorageManager, config: &Config) -> bool {
    if !config.services.definitions.is_empty() {
        return true;
    }
    if repo_root_cedar_present(&layout.root) {
        return true;
    }
    match product_api_routes(storage) {
        Ok(present) => present,
        Err(e) => {
            tracing::debug!(
                error = %e,
                "config checklist: product api_routes query failed; no HTTP from routes"
            );
            false
        }
    }
}

fn product_api_routes(storage: &StorageManager) -> miette::Result<bool> {
    let conn = storage.get_connection();
    let mut stmt = conn
        .prepare(
            "SELECT pf.file_path FROM api_routes ar \
             JOIN project_files pf ON ar.handler_file_id = pf.id",
        )
        .into_diagnostic()?;
    let paths = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .into_diagnostic()?;
    for path in paths {
        let path = path.into_diagnostic()?;
        if !skip_scip_rel_path(&path) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn count_deploy_manifests(storage: &StorageManager) -> miette::Result<usize> {
    let n: i64 = storage
        .get_connection()
        .query_row("SELECT COUNT(*) FROM deploy_manifests", [], |row| {
            row.get(0)
        })
        .into_diagnostic()?;
    Ok(n.max(0) as usize)
}

fn has_declared_or_indexed_services(storage: &StorageManager, config: &Config) -> bool {
    if !config.services.definitions.is_empty() {
        return true;
    }
    match storage.get_connection().query_row(
        "SELECT COUNT(DISTINCT service_name) FROM project_files WHERE service_name IS NOT NULL",
        [],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(n) => n > 0,
        Err(e) => {
            tracing::debug!(
                error = %e,
                "config checklist: indexed services count failed; treating as none"
            );
            false
        }
    }
}

fn policy_file_item(layout: &Layout, session: &CliSession) -> miette::Result<ConfigChecklistItem> {
    let (_cfg, source) = resolve_policy(layout, None, None, false)?;
    let synthesized = source == PolicySource::Synthesized;
    Ok(item(
        "policy.file",
        if synthesized {
            ChecklistStatus::Optional
        } else {
            ChecklistStatus::Ready
        },
        if synthesized { NEXT_POLICY_FILE } else { "" },
        already_shown(session, None),
        None,
    ))
}

fn gate_mode_item(config: &Config, session: &CliSession) -> ConfigChecklistItem {
    let enforce = config.gate.is_enforce();
    item(
        "gate.mode",
        if enforce {
            ChecklistStatus::Ready
        } else {
            ChecklistStatus::Optional
        },
        if enforce { "" } else { NEXT_GATE_MODE },
        already_shown(session, None),
        None,
    )
}

fn scip_items(
    layout: &Layout,
    config: &Config,
    session: &CliSession,
) -> miette::Result<Vec<ConfigChecklistItem>> {
    let findings = collect_scip_findings(config, &layout.root);
    let mut items = Vec::new();
    for finding in findings {
        let Some((id, ready)) = scip_row_from_code(&finding.code) else {
            continue;
        };
        items.push(item(
            id,
            if ready {
                ChecklistStatus::Ready
            } else {
                ChecklistStatus::Optional
            },
            if ready { "" } else { finding.message.as_str() },
            already_shown(session, None),
            None,
        ));
    }
    Ok(items)
}

fn scip_row_from_code(code: &str) -> Option<(&'static str, bool)> {
    match code {
        "scip-rust-available" => Some(("scip.rust", true)),
        "scip-rust-missing" | "scip-rust-policy-blocked" => Some(("scip.rust", false)),
        "scip-typescript-available" => Some(("scip.typescript", true)),
        "scip-typescript-missing" | "scip-typescript-policy-blocked" => {
            Some(("scip.typescript", false))
        }
        "scip-python-available" => Some(("scip.python", true)),
        "scip-python-missing" | "scip-python-policy-blocked" => Some(("scip.python", false)),
        "scip-go-not-wired" => Some(("scip.go", false)),
        "scip-clang-not-wired" => Some(("scip.clang", false)),
        _ => None,
    }
}

fn federate_item(
    layout: &Layout,
    storage: &StorageManager,
    config: &Config,
    session: &CliSession,
) -> miette::Result<Option<ConfigChecklistItem>> {
    let raw = get_federated_links(storage.get_connection())?;
    let presented = present_federated_links(&raw, layout.root.as_str());
    if presented.live.is_empty() {
        return Ok(None);
    }
    let now = Utc::now();
    let threshold = Duration::days(config.index.stale_threshold_days as i64);
    let cutoff = now - threshold;
    let stale = presented.live.iter().any(|link| {
        DateTime::parse_from_rfc3339(&link.last_scanned)
            .map(|ts| ts.with_timezone(&Utc) < cutoff)
            .unwrap_or(true)
    });
    Ok(Some(item(
        "federate.freshness",
        if stale {
            ChecklistStatus::Empty
        } else {
            ChecklistStatus::Ready
        },
        if stale { NEXT_FEDERATE } else { "" },
        already_shown(session, None),
        None,
    )))
}

fn surface_to_checklist(status: SurfaceStatus) -> ChecklistStatus {
    match status {
        SurfaceStatus::Ready => ChecklistStatus::Ready,
        SurfaceStatus::Empty => ChecklistStatus::Empty,
        SurfaceStatus::Gated => ChecklistStatus::Gated,
    }
}

fn already_shown(session: &CliSession, notice: Option<&str>) -> bool {
    session.is_shown(CHECKLIST_COOKIE_ID) || notice.is_some_and(|id| session.is_shown(id))
}

fn mapped_notice_id(item: &ConfigChecklistItem) -> Option<&'static str> {
    match item.id.as_str() {
        "coverage.global" => Some(SessionNoticeId::CoverageGlobal.as_str()),
        "coverage.services" => Some(SessionNoticeId::CoverageServices.as_str()),
        "coverage.deploy" => Some(SessionNoticeId::CoverageDeploy.as_str()),
        "observability.openslo" if item.status == ChecklistStatus::Empty => {
            Some(SessionNoticeId::ObservabilityEmpty.as_str())
        }
        _ => None,
    }
}

fn item(
    id: &str,
    status: ChecklistStatus,
    next: &str,
    already_shown: bool,
    apply_arg: Option<&str>,
) -> ConfigChecklistItem {
    ConfigChecklistItem {
        id: id.to_string(),
        status,
        applicable: true,
        next: next.to_string(),
        already_shown,
        apply_arg: apply_arg.map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use std::fs;
    use tempfile::tempdir;

    fn harness() -> (
        tempfile::TempDir,
        Layout,
        StorageManager,
        Config,
        CliSession,
    ) {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).expect("utf8 temp");
        let state = root.join(".ledgerful");
        let layout = Layout::from_roots(root, &state);
        layout.ensure_state_dir().unwrap();
        let storage =
            StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
        let config = Config::default();
        let session = CliSession::load(&layout, Some("t0301"), Utc::now());
        (tmp, layout, storage, config, session)
    }

    fn insert_route(storage: &StorageManager, id: i64, file_path: &str) {
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES (?1, ?2, '2026-01-01T00:00:00Z')",
            rusqlite::params![id, file_path],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO api_routes (method, path_pattern, handler_symbol_name, handler_file_id, framework, last_indexed_at) \
             VALUES ('GET', '/api/probe', 'handler', ?1, 'axum', '2026-01-01T00:00:00Z')",
            rusqlite::params![id],
        )
        .unwrap();
    }

    fn ids_of(items: &[ConfigChecklistItem]) -> Vec<&str> {
        items.iter().map(|i| i.id.as_str()).collect()
    }

    fn find<'a>(items: &'a [ConfigChecklistItem], id: &str) -> &'a ConfigChecklistItem {
        items
            .iter()
            .find(|i| i.id == id)
            .unwrap_or_else(|| panic!("missing {id} in {:?}", ids_of(items)))
    }

    #[test]
    fn config_checklist_omits_scip_go_without_product_go() {
        let (_tmp, layout, storage, config, session) = harness();
        fs::create_dir_all(layout.root.join("src").as_std_path()).unwrap();
        fs::write(
            layout.root.join("src/lib.rs").as_std_path(),
            "pub fn x() {}\n",
        )
        .unwrap();
        fs::create_dir_all(layout.root.join("tests/fixtures").as_std_path()).unwrap();
        fs::write(
            layout.root.join("tests/fixtures/sample.go").as_std_path(),
            "package main\n",
        )
        .unwrap();
        let items = build_config_checklist(&layout, &storage, &config, &session).unwrap();
        assert!(
            ids_of(&items).contains(&"scip.rust"),
            "product Rust must emit scip.rust: {:?}",
            ids_of(&items)
        );
        assert!(
            !ids_of(&items).contains(&"scip.go"),
            "fixture Go must not emit scip.go: {:?}",
            ids_of(&items)
        );
        let _ = storage.shutdown();
    }

    #[test]
    fn config_checklist_omits_http_items_without_http_character() {
        let (_tmp, layout, storage, config, session) = harness();
        let items = build_config_checklist(&layout, &storage, &config, &session).unwrap();
        for id in [
            "coverage.global",
            "coverage.services",
            "coverage.deploy",
            "services.declare",
            "observability.openslo",
            "security.cedar",
        ] {
            assert!(
                !ids_of(&items).contains(&id),
                "{id} must be omitted without HTTP character: {:?}",
                ids_of(&items)
            );
        }
        assert!(ids_of(&items).contains(&"policy.file"));
        assert!(ids_of(&items).contains(&"gate.mode"));
        let _ = storage.shutdown();
    }

    #[test]
    fn config_checklist_ignores_fixture_routes_for_http_character() {
        let (_tmp, layout, storage, config, session) = harness();
        insert_route(&storage, 1, "tests/fixtures/go_sample/pkg/handlers.go");
        insert_route(&storage, 2, r"tests\fixtures\go_sample\pkg\handlers.go");
        let items = build_config_checklist(&layout, &storage, &config, &session).unwrap();
        for id in [
            "coverage.global",
            "coverage.services",
            "services.declare",
            "observability.openslo",
            "security.cedar",
        ] {
            assert!(
                !ids_of(&items).contains(&id),
                "fixture routes must not count as HTTP ({id}): {:?}",
                ids_of(&items)
            );
        }
        let _ = storage.shutdown();
    }

    #[test]
    fn config_checklist_engine_probes_include_coverage_global() {
        let (_tmp, layout, storage, config, session) = harness();
        insert_route(&storage, 1, "src/http/routes.rs");
        let items = build_config_checklist(&layout, &storage, &config, &session).unwrap();
        let global = find(&items, "coverage.global");
        assert_eq!(global.status, ChecklistStatus::Gated);
        assert_eq!(global.next, NEXT_COVERAGE_GLOBAL);
        assert_eq!(global.apply_arg.as_deref(), Some("coverage.global"));
        assert!(global.applicable);
        assert!(!global.already_shown);
        assert!(ids_of(&items).contains(&"observability.openslo"));
        assert!(ids_of(&items).contains(&"security.cedar"));
        assert!(!ids_of(&items).contains(&"coverage.services"));
        let _ = storage.shutdown();
    }

    #[test]
    fn config_checklist_second_session_marks_already_shown() {
        let (_tmp, layout, storage, config, mut session) = harness();
        insert_route(&storage, 1, "src/api.rs");
        let first = build_config_checklist(&layout, &storage, &config, &session).unwrap();
        assert!(!find(&first, "coverage.global").already_shown);
        apply_checklist_cookie(&mut session, &first);
        let cookie: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(layout.cli_session_file_for_id("t0301").as_std_path()).unwrap(),
        )
        .unwrap();
        let shown = cookie["shown"]
            .as_array()
            .expect("shown")
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();
        assert!(
            shown.contains(&CHECKLIST_COOKIE_ID),
            "cookie must mark config.checklist: {shown:?}"
        );
        assert!(
            shown.contains(&"coverage.global"),
            "cookie must mark mapped 0300 id: {shown:?}"
        );
        let session2 = CliSession::load(&layout, Some("t0301"), Utc::now());
        let second = build_config_checklist(&layout, &storage, &config, &session2).unwrap();
        assert!(
            find(&second, "coverage.global").already_shown,
            "second build must see config.checklist"
        );
        assert!(
            find(&second, "observability.openslo").already_shown
                || find(&second, "observability.openslo").status != ChecklistStatus::Empty,
            "empty obs should inherit cookie"
        );
        let _ = storage.shutdown();
    }

    #[test]
    fn config_checklist_session_writes_cookie_only_when_gaps() {
        let (_tmp, layout, storage, config, mut session) = harness();
        let optional_only = build_config_checklist(&layout, &storage, &config, &session).unwrap();
        assert!(
            optional_only.iter().all(|i| !i.status.is_gap()),
            "no-HTTP catalog must be ready/optional: {:?}",
            optional_only
                .iter()
                .map(|i| (i.id.as_str(), i.status))
                .collect::<Vec<_>>()
        );
        apply_checklist_cookie(&mut session, &optional_only);
        assert!(
            !layout.cli_session_file_for_id("t0301").exists(),
            "optional-only must not write cookie"
        );

        insert_route(&storage, 1, "src/api.rs");
        let session2 = CliSession::load(&layout, Some("t0301-gaps"), Utc::now());
        let with_gaps = build_config_checklist(&layout, &storage, &config, &session2).unwrap();
        assert!(with_gaps.iter().any(|i| i.status.is_gap()));
        let mut session2 = session2;
        apply_checklist_cookie(&mut session2, &with_gaps);
        let cookie_path = layout.cli_session_file_for_id("t0301-gaps");
        assert!(cookie_path.exists(), "gated/empty must persist cookie");
        let cookie: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(cookie_path.as_std_path()).unwrap()).unwrap();
        let shown = cookie["shown"]
            .as_array()
            .expect("shown")
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();
        assert!(shown.contains(&CHECKLIST_COOKIE_ID), "{shown:?}");
        assert!(shown.contains(&"coverage.global"), "{shown:?}");
        let _ = config;
        let _ = storage.shutdown();
    }

    #[test]
    fn config_checklist_ready_next_blank_except_surfaces_reuse() {
        let (_tmp, layout, storage, mut config, session) = harness();
        insert_route(&storage, 1, "src/api.rs");
        config.coverage.enabled = true;
        config.coverage.services.enabled = true;
        config.gate.mode = "enforce".to_string();
        let items = build_config_checklist(&layout, &storage, &config, &session).unwrap();
        assert_eq!(find(&items, "coverage.global").next, "");
        assert_eq!(find(&items, "coverage.services").next, "");
        assert_eq!(find(&items, "gate.mode").status, ChecklistStatus::Ready);
        assert_eq!(find(&items, "gate.mode").next, "");
        assert_eq!(find(&items, "services.declare").next, NEXT_SERVICES_DECLARE);
        assert!(!find(&items, "observability.openslo").next.is_empty());
        assert!(!find(&items, "security.cedar").next.is_empty());
        let _ = storage.shutdown();
    }
}
