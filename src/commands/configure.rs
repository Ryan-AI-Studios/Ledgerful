//! Agent `configure` command (0302). Catalog I/O only — fact assembly stays
//! in [`crate::config::checklist`].

use crate::commands::change_context::open_storage_for_change_context;
use crate::commands::config::edit::execute_config_set_in_quiet;
use crate::config::checklist::{
    ChecklistStatus, ConfigChecklistItem, apply_checklist_cookie, build_config_checklist,
    mark_checklist_cookie,
};
use crate::config::model::Config;
use crate::state::cli_session::{CliSession, env_session_id};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use chrono::{DateTime, Utc};
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::Write;

pub const CONFIGURE_SCHEMA_VERSION: u32 = 1;
pub const CONFIGURE_KIND: &str = "configure";

const ALREADY_ENABLED: &str = "already enabled";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppliedItem {
    pub id: String,
    pub ok: bool,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ConfigureOpts<'a> {
    pub json: bool,
    pub apply: &'a [String],
    pub session_id: Option<&'a str>,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigureEnvelope {
    pub schema_version: u32,
    pub kind: String,
    pub items: Vec<ConfigChecklistItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied: Vec<AppliedItem>,
}

/// CLI entry: resolve layout/storage/config, then [`execute_configure_in`].
pub fn execute_configure(json: bool, apply: &[String]) -> Result<()> {
    let layout = crate::commands::helpers::get_layout()
        .map_err(|e| miette::miette!("configure: layout unavailable: {e}"))?;
    let config = crate::config::load::load_config(&layout).unwrap_or_default();
    let storage = match open_storage_for_change_context(&layout) {
        Ok(s) => s,
        Err((e, _)) => {
            return Err(miette::miette!("configure: storage unavailable: {e}"));
        }
    };
    let session_id = env_session_id();
    let opts = ConfigureOpts {
        json,
        apply,
        session_id: session_id.as_deref(),
        now: Utc::now(),
    };
    let mut out = std::io::stdout();
    let result = execute_configure_in(&layout, &storage, &config, &opts, &mut out);
    let _ = storage.shutdown();
    result
}

/// Testable seam. Does not call [`crate::state::layout::get_layout`].
pub fn execute_configure_in(
    layout: &Layout,
    storage: &StorageManager,
    config: &Config,
    opts: &ConfigureOpts<'_>,
    out: &mut impl Write,
) -> Result<()> {
    let mut session = CliSession::load(layout, opts.session_id, opts.now);
    let pre_items = build_config_checklist(layout, storage, config, &session)?;

    let tokens = normalize_apply_tokens(opts.apply)?;
    if !tokens.is_empty() {
        validate_apply_ids(&tokens, &pre_items)?;
    }

    let mut applied = Vec::new();
    if !tokens.is_empty() {
        for id in &tokens {
            applied.push(apply_one(layout, id, &pre_items));
        }
        applied.sort_by(|a, b| a.id.cmp(&b.id));
    }

    let items = if tokens.is_empty() {
        pre_items.clone()
    } else {
        mark_checklist_cookie(&mut session, &pre_items);
        let reloaded = crate::config::load::load_config(layout).unwrap_or_default();
        build_config_checklist(layout, storage, &reloaded, &session)?
    };

    let envelope = ConfigureEnvelope {
        schema_version: CONFIGURE_SCHEMA_VERSION,
        kind: CONFIGURE_KIND.to_string(),
        items,
        applied,
    };

    emit_configure(&envelope, opts.json, out)?;
    apply_checklist_cookie(
        &mut session,
        if tokens.is_empty() {
            &envelope.items
        } else {
            &pre_items
        },
    );

    if envelope.applied.iter().any(|a| !a.ok) {
        return Err(miette::miette!("one or more apply actions failed"));
    }
    Ok(())
}

fn normalize_apply_tokens(apply: &[String]) -> Result<Vec<String>> {
    let mut unique = BTreeSet::new();
    for raw in apply {
        let token = raw.trim();
        if token.is_empty() {
            return Err(miette::miette!(
                "configure --apply: empty id after trim (refused; no writes)"
            ));
        }
        if is_apply_all_alias(token) {
            return Err(miette::miette!(
                "configure --apply: apply-all alias `{token}` is refused (no writes)"
            ));
        }
        unique.insert(token.to_string());
    }
    Ok(unique.into_iter().collect())
}

fn is_apply_all_alias(token: &str) -> bool {
    token == "*" || token.eq_ignore_ascii_case("all")
}

fn validate_apply_ids(ids: &[String], items: &[ConfigChecklistItem]) -> Result<()> {
    for id in ids {
        match items.iter().find(|i| i.id == *id) {
            None => {
                return Err(miette::miette!(
                    "configure --apply: `{id}` is unknown or not in the current catalog (no writes)"
                ));
            }
            Some(item) if item.apply_arg.is_none() => {
                return Err(miette::miette!(
                    "configure --apply: `{id}` has no applyArg (no writes)"
                ));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

fn apply_mapping(id: &str) -> Option<&'static str> {
    match id {
        "coverage.global" => Some("coverage.enabled=true"),
        "coverage.services" => Some("coverage.services.enabled=true"),
        "coverage.deploy" => Some("coverage.deploy.enabled=true"),
        _ => None,
    }
}

fn apply_one(layout: &Layout, id: &str, pre_items: &[ConfigChecklistItem]) -> AppliedItem {
    let already_ready = pre_items
        .iter()
        .any(|i| i.id == id && i.status == ChecklistStatus::Ready);
    let Some(key_value) = apply_mapping(id) else {
        return AppliedItem {
            id: id.to_string(),
            ok: false,
            message: format!("no freeze mapping for `{id}`"),
        };
    };
    match execute_config_set_in_quiet(layout, key_value) {
        Ok(()) => AppliedItem {
            id: id.to_string(),
            ok: true,
            message: if already_ready {
                ALREADY_ENABLED.to_string()
            } else {
                String::new()
            },
        },
        Err(e) => AppliedItem {
            id: id.to_string(),
            ok: false,
            message: e.to_string(),
        },
    }
}

fn emit_configure(envelope: &ConfigureEnvelope, json: bool, out: &mut impl Write) -> Result<()> {
    if json {
        let body = crate::output::json::format_json(envelope)?;
        writeln!(out, "{body}").into_diagnostic()?;
    } else {
        write_human(envelope, out)?;
    }
    Ok(())
}

fn write_human(envelope: &ConfigureEnvelope, out: &mut impl Write) -> Result<()> {
    if !envelope.applied.is_empty() {
        writeln!(out, "applied:").into_diagnostic()?;
        for item in &envelope.applied {
            if item.ok {
                writeln!(out, "  {}: ok", item.id).into_diagnostic()?;
            } else {
                writeln!(out, "  {}: FAILED {}", item.id, item.message).into_diagnostic()?;
            }
        }
    }
    writeln!(out, "id  status  next").into_diagnostic()?;
    for item in &envelope.items {
        writeln!(out, "{}  {}  {}", item.id, item.status.as_str(), item.next).into_diagnostic()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::checklist::CHECKLIST_COOKIE_ID;
    use camino::Utf8Path;
    use std::fs;
    use tempfile::tempdir;

    fn harness() -> (tempfile::TempDir, Layout, StorageManager, Config) {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).expect("utf8 temp");
        let state = root.join(".ledgerful");
        let layout = Layout::from_roots(root, &state);
        layout.ensure_state_dir().unwrap();
        let storage =
            StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
        (tmp, layout, storage, Config::default())
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

    fn write_rust_only(layout: &Layout) {
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
    }

    fn run_in(
        layout: &Layout,
        storage: &StorageManager,
        config: &Config,
        json: bool,
        apply: &[&str],
        session_id: &str,
    ) -> (Result<()>, String) {
        let apply: Vec<String> = apply.iter().map(|s| (*s).to_string()).collect();
        let mut buf = Vec::new();
        let opts = ConfigureOpts {
            json,
            apply: &apply,
            session_id: Some(session_id),
            now: Utc::now(),
        };
        let result = execute_configure_in(layout, storage, config, &opts, &mut buf);
        (result, String::from_utf8_lossy(&buf).into_owned())
    }

    fn parse_envelope(stdout: &str) -> ConfigureEnvelope {
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("json: {e}\n{stdout}"))
    }

    #[test]
    fn configure_json_omits_inapplicable() {
        let (_tmp, layout, storage, config) = harness();
        write_rust_only(&layout);
        let (result, stdout) = run_in(&layout, &storage, &config, true, &[], "t-omit");
        assert!(result.is_ok(), "{result:?}");
        let env = parse_envelope(&stdout);
        assert_eq!(env.schema_version, 1);
        assert_eq!(env.kind, "configure");
        let ids: Vec<&str> = env.items.iter().map(|i| i.id.as_str()).collect();
        assert!(ids.contains(&"policy.file"), "{ids:?}");
        assert!(ids.contains(&"gate.mode"), "{ids:?}");
        assert!(!ids.contains(&"scip.go"), "{ids:?}");
        assert!(!ids.contains(&"coverage.global"), "{ids:?}");
        assert!(!ids.contains(&"observability.openslo"), "{ids:?}");
        assert!(!stdout.contains("\"applied\""));
        let _ = storage.shutdown();
    }

    #[test]
    fn configure_json_includes_coverage_global_when_http() {
        let (_tmp, layout, storage, config) = harness();
        insert_route(&storage, 1, "src/http/routes.rs");
        let (result, stdout) = run_in(&layout, &storage, &config, true, &[], "t-http");
        assert!(result.is_ok(), "{result:?}");
        let env = parse_envelope(&stdout);
        assert_eq!(env.schema_version, 1);
        assert_eq!(env.kind, "configure");
        let global = env
            .items
            .iter()
            .find(|i| i.id == "coverage.global")
            .expect("coverage.global");
        assert_eq!(global.status, ChecklistStatus::Gated);
        assert_eq!(global.apply_arg.as_deref(), Some("coverage.global"));
        assert!(!global.already_shown);
        assert!(env.applied.is_empty());
        assert!(!stdout.contains("\"applied\""));
        let _ = storage.shutdown();
    }

    #[test]
    fn configure_apply_coverage_global_sets_enabled() {
        let (_tmp, layout, storage, config) = harness();
        insert_route(&storage, 1, "src/api.rs");
        let (result, stdout) = run_in(
            &layout,
            &storage,
            &config,
            true,
            &["coverage.global"],
            "t-apply",
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_envelope(&stdout);
        let reloaded = crate::config::load::load_config(&layout).unwrap();
        assert!(reloaded.coverage.enabled);
        let global = env
            .items
            .iter()
            .find(|i| i.id == "coverage.global")
            .expect("coverage.global");
        assert_eq!(global.status, ChecklistStatus::Ready);
        assert!(global.already_shown);
        assert_eq!(env.applied.len(), 1);
        assert_eq!(env.applied[0].id, "coverage.global");
        assert!(env.applied[0].ok);
        assert_eq!(env.applied[0].message, "");
        let _ = storage.shutdown();
    }

    #[test]
    fn configure_apply_ready_id_is_idempotent() {
        let (_tmp, layout, storage, mut config) = harness();
        insert_route(&storage, 1, "src/api.rs");
        config.coverage.enabled = true;
        let (result, stdout) = run_in(
            &layout,
            &storage,
            &config,
            true,
            &["coverage.global"],
            "t-ready",
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let env = parse_envelope(&stdout);
        let reloaded = crate::config::load::load_config(&layout).unwrap();
        assert!(reloaded.coverage.enabled);
        assert_eq!(env.applied.len(), 1);
        assert!(env.applied[0].ok);
        assert_eq!(env.applied[0].message, ALREADY_ENABLED);
        let _ = storage.shutdown();
    }

    #[test]
    fn configure_apply_refuses_unknown_id() {
        let (_tmp, layout, storage, config) = harness();
        insert_route(&storage, 1, "src/api.rs");
        let (result, stdout) = run_in(
            &layout,
            &storage,
            &config,
            true,
            &["not.a.real.id"],
            "t-unknown",
        );
        assert!(result.is_err(), "must refuse unknown");
        assert!(stdout.is_empty(), "refuse must not emit stdout: {stdout}");
        assert!(!layout.cli_session_file_for_id("t-unknown").exists());
        assert!(
            !crate::config::load::load_config(&layout)
                .unwrap()
                .coverage
                .enabled
        );
        let _ = storage.shutdown();
    }

    #[test]
    fn configure_apply_refuses_id_without_apply_arg() {
        let (_tmp, layout, storage, mut config) = harness();
        insert_route(&storage, 1, "src/api.rs");
        config.coverage.enabled = true;
        let (result, stdout) = run_in(
            &layout,
            &storage,
            &config,
            true,
            &["services.declare"],
            "t-noarg",
        );
        assert!(result.is_err(), "must refuse no-applyArg");
        assert!(stdout.is_empty(), "{stdout}");
        assert!(!layout.cli_session_file_for_id("t-noarg").exists());
        let _ = storage.shutdown();
    }

    #[test]
    fn configure_apply_refuses_inapplicable_id() {
        let (_tmp, layout, storage, config) = harness();
        insert_route(&storage, 1, "src/api.rs");
        let (result, stdout) = run_in(
            &layout,
            &storage,
            &config,
            true,
            &["coverage.services"],
            "t-inapp",
        );
        assert!(result.is_err(), "must refuse inapplicable");
        assert!(stdout.is_empty(), "{stdout}");
        assert!(
            !crate::config::load::load_config(&layout)
                .unwrap()
                .coverage
                .enabled
        );
        let _ = storage.shutdown();
    }

    #[test]
    fn configure_apply_refuses_star() {
        let (_tmp, layout, storage, config) = harness();
        insert_route(&storage, 1, "src/api.rs");
        for token in ["*", "all", "ALL"] {
            let (result, stdout) = run_in(&layout, &storage, &config, true, &[token], "t-star");
            assert!(result.is_err(), "must refuse {token}");
            assert!(stdout.is_empty(), "{token} stdout={stdout}");
            assert!(
                !crate::config::load::load_config(&layout)
                    .unwrap()
                    .coverage
                    .enabled
            );
        }
        let _ = storage.shutdown();
    }

    #[test]
    fn configure_apply_refuses_empty_or_whitespace_token() {
        let (_tmp, layout, storage, config) = harness();
        insert_route(&storage, 1, "src/api.rs");
        for token in ["", "   ", ","] {
            let (result, stdout) = run_in(&layout, &storage, &config, true, &[token], "t-ws");
            assert!(result.is_err(), "must refuse empty/whitespace `{token}`");
            assert!(stdout.is_empty(), "{stdout}");
        }
        let (result, stdout) = run_in(
            &layout,
            &storage,
            &config,
            true,
            &[" coverage.global "],
            "t-pad",
        );
        assert!(
            result.is_ok(),
            "padded valid id must trim, not refuse: {result:?}\n{stdout}"
        );
        assert!(
            crate::config::load::load_config(&layout)
                .unwrap()
                .coverage
                .enabled
        );
        let _ = storage.shutdown();
    }

    #[test]
    fn configure_apply_validates_all_before_write() {
        let (_tmp, layout, storage, config) = harness();
        insert_route(&storage, 1, "src/api.rs");
        let (result, stdout) = run_in(
            &layout,
            &storage,
            &config,
            true,
            &["coverage.global", "services.declare"],
            "t-all1",
        );
        assert!(result.is_err(), "mixed valid+noArg must refuse");
        assert!(stdout.is_empty(), "{stdout}");
        assert!(
            !crate::config::load::load_config(&layout)
                .unwrap()
                .coverage
                .enabled
        );

        let (result, stdout) = run_in(
            &layout,
            &storage,
            &config,
            true,
            &["coverage.global", "coverage.services"],
            "t-all2",
        );
        assert!(
            result.is_err(),
            "global+services in one invocation must refuse when services not pre-apply"
        );
        assert!(stdout.is_empty(), "{stdout}");
        assert!(
            !crate::config::load::load_config(&layout)
                .unwrap()
                .coverage
                .enabled,
            "two-step HITL: must not apply global when services is also requested"
        );
        let _ = storage.shutdown();
    }

    #[test]
    fn configure_does_not_prompt() {
        let (_tmp, layout, storage, config) = harness();
        insert_route(&storage, 1, "src/api.rs");
        let (result, stdout) = run_in(&layout, &storage, &config, false, &[], "t-human");
        assert!(result.is_ok(), "{result:?}");
        let lower = stdout.to_ascii_lowercase();
        assert!(!lower.contains("confirm"), "{stdout}");
        assert!(!lower.contains("inquire"), "{stdout}");
        assert!(!lower.contains("press enter"), "{stdout}");
        assert!(stdout.contains("id  status  next"), "{stdout}");
        assert!(stdout.contains("coverage.global"), "{stdout}");
        let _ = storage.shutdown();
    }

    #[test]
    fn configure_human_has_no_confirm() {
        let (_tmp, layout, storage, config) = harness();
        insert_route(&storage, 1, "src/api.rs");
        let (result, stdout) = run_in(
            &layout,
            &storage,
            &config,
            false,
            &["coverage.global"],
            "t-human-apply",
        );
        assert!(result.is_ok(), "{result:?}\n{stdout}");
        let lower = stdout.to_ascii_lowercase();
        assert!(!lower.contains("confirm"), "{stdout}");
        assert!(!lower.contains("inquire"), "{stdout}");
        assert!(stdout.contains("applied:"), "{stdout}");
        assert!(stdout.contains("  coverage.global: ok"), "{stdout}");
        assert!(stdout.contains("id  status  next"), "{stdout}");
        let _ = storage.shutdown();
    }

    #[test]
    fn configure_writes_cookie_when_gaps() {
        let (_tmp, layout, storage, config) = harness();
        insert_route(&storage, 1, "src/api.rs");
        let (result, stdout) = run_in(&layout, &storage, &config, true, &[], "t-cookie");
        assert!(result.is_ok(), "{result:?}");
        let first = parse_envelope(&stdout);
        assert!(
            !first
                .items
                .iter()
                .find(|i| i.id == "coverage.global")
                .unwrap()
                .already_shown
        );
        assert!(
            layout.cli_session_file_for_id("t-cookie").exists(),
            "gap catalog must persist cookie"
        );
        let cookie: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(layout.cli_session_file_for_id("t-cookie").as_std_path()).unwrap(),
        )
        .unwrap();
        let shown: Vec<&str> = cookie["shown"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(shown.contains(&CHECKLIST_COOKIE_ID), "{shown:?}");

        let (result, stdout) = run_in(&layout, &storage, &config, true, &[], "t-cookie");
        assert!(result.is_ok(), "{result:?}");
        let second = parse_envelope(&stdout);
        assert!(
            second
                .items
                .iter()
                .find(|i| i.id == "coverage.global")
                .unwrap()
                .already_shown
        );

        let (_tmp2, layout2, storage2, config2) = harness();
        write_rust_only(&layout2);
        let (result, _) = run_in(&layout2, &storage2, &config2, true, &[], "t-opt");
        assert!(result.is_ok(), "{result:?}");
        assert!(
            !layout2.cli_session_file_for_id("t-opt").exists(),
            "optional-only must not write cookie"
        );
        let _ = storage.shutdown();
        let _ = storage2.shutdown();
    }

    #[test]
    fn configure_human_writes_cookie_when_gaps() {
        let (_tmp, layout, storage, config) = harness();
        insert_route(&storage, 1, "src/api.rs");
        let (result, stdout) = run_in(&layout, &storage, &config, false, &[], "t-hcookie");
        assert!(result.is_ok(), "{result:?}");
        assert!(!stdout.contains("confirm"));
        assert!(
            layout.cli_session_file_for_id("t-hcookie").exists(),
            "bare configure must persist cookie on gaps"
        );
        let (result, _) = run_in(&layout, &storage, &config, false, &[], "t-hcookie");
        assert!(result.is_ok(), "{result:?}");
        let session = CliSession::load(&layout, Some("t-hcookie"), Utc::now());
        assert!(session.is_shown(CHECKLIST_COOKIE_ID));
        let _ = storage.shutdown();
    }

    #[test]
    fn configure_apply_failed_set_emits_then_err() {
        let (_tmp, layout, storage, config) = harness();
        insert_route(&storage, 1, "src/api.rs");
        let config_path = layout.config_file();
        fs::create_dir_all(config_path.as_std_path()).unwrap();

        let (result, stdout) = run_in(
            &layout,
            &storage,
            &config,
            true,
            &["coverage.global"],
            "t-fail",
        );
        assert!(result.is_err(), "failed set must return Err after emit");
        assert!(!stdout.is_empty(), "must emit envelope before Err");
        let env = parse_envelope(&stdout);
        assert_eq!(env.applied.len(), 1);
        assert!(!env.applied[0].ok);
        assert!(!env.applied[0].message.is_empty());

        let (result, stdout) = run_in(
            &layout,
            &storage,
            &config,
            false,
            &["coverage.global"],
            "t-fail-h",
        );
        assert!(result.is_err(), "{result:?}");
        assert!(
            stdout.contains("FAILED"),
            "human applied block must include FAILED: {stdout}"
        );
        let _ = storage.shutdown();
    }
}
