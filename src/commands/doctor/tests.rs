use super::*;
use crate::output::human::DoctorReport;
use crate::platform::env::ExecutableStatus;
use camino::{Utf8Path, Utf8PathBuf};
use std::path::PathBuf;

fn sample_report<'a>(tools: &'a Vec<(String, ExecutableStatus)>) -> DoctorReport<'a> {
    DoctorReport {
        platform: "test",
        shell: "test",
        tools,
        path_display: "test",
        path_kind: "test",
        work_root: "test",
        state_dir: "test/.ledgerful",
        is_wsl_mounted: false,
        embedding_model_status: "OK".to_string(),
        embedding_model_failed: false,
        completion_model_status: "OK".to_string(),
        native_graph_status: "Ready (CozoDB active)".to_string(),
        active_ask_backend: "Gemini (Cloud)".to_string(),
        // 0126: never embed healthy OK-with-zero; fixture uses positive N.
        index_health: vec!["Search index: OK (12 documents)".to_string()],
        target_triple: "test",
    }
}

#[test]
fn doctor_json_plus_apply_hook_refresh_rejected() {
    use crate::cli::Cli;
    use clap::Parser;
    // Rejected at execute_doctor entry (also covered by clap path when wired).
    let err = execute_doctor(true, true, false, false, false).unwrap_err();
    let msg = format!("{err:?}");
    assert!(
        msg.contains("doctor --json cannot be combined with --apply-hook-refresh"),
        "got {msg}"
    );
    // clap accepts the flags; rejection is in execute_doctor.
    let parsed = Cli::try_parse_from(["ledgerful", "doctor", "--json", "--apply-hook-refresh"]);
    assert!(parsed.is_ok(), "flags are parseable; combo rejected later");
}

#[test]
fn doctor_summary_four_way_priority() {
    use crate::output::human::format_doctor_summary_text;
    assert_eq!(
        format_doctor_summary_text(2, 5, 0, 3),
        "✗ Doctor: 2 block issue(s)"
    );
    assert_eq!(
        format_doctor_summary_text(0, 3, 0, 2),
        "✓ Doctor: ready for publish env · 3 warning(s)"
    );
    assert_eq!(
        format_doctor_summary_text(0, 0, 0, 4),
        "✓ Doctor: ready for publish env · 4 hint(s)"
    );
    assert_eq!(
        format_doctor_summary_text(0, 0, 0, 0),
        "✓ Doctor: all checks passed"
    );
    // Block wins; warn never uses red soft-fail "issue(s) found".
    assert!(!format_doctor_summary_text(0, 1, 0, 9).contains("issue(s) found"));
    assert!(format_doctor_summary_text(0, 1, 0, 9).contains("ready for publish"));
}

/// 0209 DoD-1 row 1 JSON object: additive warnAction/warnOptional, schemaVersion 1.
#[test]
fn doctor_json_summary_warn_split_row1() {
    let findings = vec![
        DoctorFinding::warn("sig-pin", DoctorCategory::Signing, "pin"),
        DoctorFinding::warn("sig-version", DoctorCategory::Signing, "version"),
        DoctorFinding::warn("sig-pin-extra", DoctorCategory::Signing, "extra"),
        DoctorFinding::warn(
            "completion-unreachable",
            DoctorCategory::Optional,
            "completion down",
        ),
    ];
    let counts = summarize(&findings);
    let split = split_doctor_warns(&findings);
    assert_eq!(counts.warn, 4);
    assert_eq!(split.action, 3);
    assert_eq!(split.optional, 1);
    assert_eq!(counts.warn, split.action + split.optional);
    assert!(ready_for_publish(&findings));

    let body = serde_json::json!({
        "schemaVersion": 1u32,
        "readyForPublish": ready_for_publish(&findings),
        "summary": {
            "block": counts.block,
            "warn": counts.warn,
            "warnAction": split.action,
            "warnOptional": split.optional,
            "info": counts.info,
        },
    });
    assert_eq!(body["schemaVersion"], 1);
    assert!(body["schemaVersion"].is_number());
    assert_eq!(body["readyForPublish"], true);
    assert_eq!(body["summary"]["warn"], 4);
    assert_eq!(body["summary"]["warnAction"], 3);
    assert_eq!(body["summary"]["warnOptional"], 1);
    assert_eq!(
        body["summary"]["warn"].as_u64().unwrap(),
        body["summary"]["warnAction"].as_u64().unwrap()
            + body["summary"]["warnOptional"].as_u64().unwrap()
    );
}

#[test]
fn chain_checkpoint_practice_finding_signed_info_never_blocks() {
    let finding = DoctorFinding::info(
        "chain-checkpoint-practice",
        DoctorCategory::Optional,
        "Signed chain head present. Periodically run `ledgerful export head`...",
    );
    assert_eq!(finding.code, "chain-checkpoint-practice");
    assert_eq!(finding.severity, DoctorSeverity::Info);
    assert_eq!(finding.category, DoctorCategory::Optional);
    assert_eq!(dashboard_failures(std::slice::from_ref(&finding)), 0);
    assert!(ready_for_publish(std::slice::from_ref(&finding)));
}

#[test]
fn chain_checkpoint_practice_finding_none_without_signed_head() {
    // Empty in-memory DB: no chain_head row → no finding.
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("ledger.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open");
    conn.execute_batch(
        "CREATE TABLE chain_head (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                latest_entry_hash TEXT NOT NULL,
                genesis TEXT NOT NULL,
                length INTEGER NOT NULL,
                head_signature TEXT,
                head_public_key TEXT,
                updated_at TEXT NOT NULL
            );",
    )
    .expect("schema");
    assert!(chain_checkpoint_practice_finding(&conn).is_none());

    conn.execute(
            "INSERT INTO chain_head (id, latest_entry_hash, genesis, length, head_signature, head_public_key, updated_at)
             VALUES (1, 'h', 'g', 1, NULL, NULL, '2026-01-01T00:00:00Z')",
            [],
        )
        .expect("unsigned head");
    assert!(
        chain_checkpoint_practice_finding(&conn).is_none(),
        "unsigned head must not emit practice finding"
    );

    conn.execute(
        "UPDATE chain_head SET head_signature = 'sig', head_public_key = 'pk'",
        [],
    )
    .expect("sign");
    let f = chain_checkpoint_practice_finding(&conn).expect("signed head finding");
    assert_eq!(f.code, "chain-checkpoint-practice");
    assert_eq!(f.severity, DoctorSeverity::Info);
    assert_eq!(f.category, DoctorCategory::Optional);
    assert!(f.message.contains("export head"));
    assert!(f.message.contains("against-export"));
    assert_eq!(dashboard_failures(std::slice::from_ref(&f)), 0);
    assert!(ready_for_publish(std::slice::from_ref(&f)));
}

#[test]
fn classify_optional_warn_excluded_from_dashboard_failures() {
    let findings = vec![
        DoctorFinding::warn("sig-pin", DoctorCategory::Signing, SIG_PIN_WARNING),
        DoctorFinding::info("sccache-hint", DoctorCategory::Optional, "sccache hint"),
        DoctorFinding::info(
            "scip-go-not-wired",
            DoctorCategory::Optional,
            "go not wired",
        ),
        DoctorFinding::warn("embed-unreachable", DoctorCategory::Optional, "embed down"),
    ];
    assert_eq!(dashboard_failures(&findings), 1); // sig-pin only
    assert!(ready_for_publish(&findings));
    let s = summarize(&findings);
    assert_eq!(s.warn, 2);
    assert_eq!(s.info, 2);
    assert_eq!(s.block, 0);
}

/// 0100 DoD-3 / F-002: verify + doctor share unknown-key / pin / trusted terms.
/// 0125: builder remediation carries hex + PowerShell-safe outer single quotes.
#[test]
fn dod3_unknown_key_vocabulary_shared_across_verify_and_doctor() {
    use crate::ledger::crypto::SignatureTrustStatus;

    let verify_status = SignatureTrustStatus::ValidUnknownKey.as_str();
    assert!(
        verify_status.to_ascii_lowercase().contains("unknown key"),
        "ValidUnknownKey must contain 'unknown key': {verify_status}"
    );

    let doctor = SIG_PIN_WARNING;
    let doctor_lc = doctor.to_ascii_lowercase();
    assert!(
        doctor_lc.contains("unknown key"),
        "doctor sig-pin must contain 'unknown key': {doctor}"
    );
    assert!(
        doctor_lc.contains("pin") || doctor.contains("Pin"),
        "doctor sig-pin must mention pin: {doctor}"
    );
    assert!(
        doctor_lc.contains("trusted") || doctor_lc.contains("trusted_public_keys"),
        "doctor sig-pin must mention trusted keys: {doctor}"
    );

    // Builder finding keeps vocabulary and adds structured remediation.
    let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let finding = build_sig_pin_finding(Some(hex));
    let msg_lc = finding.message.to_ascii_lowercase();
    assert!(msg_lc.contains("unknown key"));
    assert!(msg_lc.contains("pin") || finding.message.contains("Pin"));
    assert!(msg_lc.contains("trusted"));
    let rem = finding.remediation.expect("remediation Some");
    assert!(
        rem.contains(&format!("'intent.trusted_public_keys=[\"{hex}\"]'")),
        "outer single quotes + hex: {rem}"
    );
}

#[test]
fn test_dashboard_failures_clean() {
    let findings: Vec<DoctorFinding> = Vec::new();
    assert_eq!(dashboard_failures(&findings), 0);
    assert!(ready_for_publish(&findings));
}

#[test]
fn test_dashboard_failures_formula_samples() {
    // Optional backends (info/warn) excluded; index warn + block included.
    let findings = vec![
        DoctorFinding::block("tool-git", DoctorCategory::Tools, "git missing"),
        DoctorFinding::warn(
            "search-corrupt",
            DoctorCategory::Index,
            "Search index corrupt",
        ),
        DoctorFinding::warn("graph-stale", DoctorCategory::Index, "Graph STALE"),
        DoctorFinding::info(
            "embed-not-configured",
            DoctorCategory::Optional,
            "embed not configured",
        ),
        DoctorFinding::warn(
            "completion-unreachable",
            DoctorCategory::Optional,
            "completion down",
        ),
        DoctorFinding::info(
            "graph-not-initialized",
            DoctorCategory::Index,
            "graph not init",
        ),
        DoctorFinding::warn("impact-corrupt", DoctorCategory::Index, "impact corrupt"),
    ];
    // block + search-corrupt + graph-stale + impact-corrupt = 4
    assert_eq!(dashboard_failures(&findings), 4);
    assert!(!ready_for_publish(&findings));
}

#[test]
fn test_optional_not_configured_not_dashboard_failure() {
    let findings = vec![
        DoctorFinding::info(
            "embed-not-configured",
            DoctorCategory::Optional,
            "not configured",
        ),
        DoctorFinding::info(
            "completion-not-configured",
            DoctorCategory::Optional,
            "not configured",
        ),
        DoctorFinding::info("tool-gemini", DoctorCategory::Optional, "gemini missing"),
    ];
    assert_eq!(dashboard_failures(&findings), 0);
    assert!(ready_for_publish(&findings));
}

/// DoD-6: partial config (model name set, base_url empty) is Not configured
/// and counted as a failure — not a healthy `(0 dims) @ `.
#[test]
fn format_embedding_partial_config_is_not_configured_failure() {
    use crate::config::model::LocalModelConfig;
    use crate::semantic::BackendStatus;

    let config = LocalModelConfig {
        embedding_model: "nomic-embed-text".to_string(),
        base_url: String::new(),
        embedding_url: None,
        dimensions: 768,
        ..Default::default()
    };
    let report = format_embedding_backend_availability(&config, &config);
    assert_eq!(report.status, BackendStatus::NotConfigured);
    assert!(report.is_failure);
    assert!(
        report.display.contains("Not configured"),
        "partial config must not look healthy, got: {}",
        report.display
    );
    assert!(
        !report.display.contains("0 dims"),
        "must not print healthy-looking (0 dims) @ for partial config, got: {}",
        report.display
    );

    // Partial config → optional warn finding; never dashboard failures / never block.
    let finding = embedding_finding(&config, &report).expect("partial finding");
    assert_eq!(finding.code, "embed-partial-config");
    assert_eq!(finding.severity, DoctorSeverity::Warn);
    assert_eq!(finding.category, DoctorCategory::Optional);
    assert_eq!(dashboard_failures(std::slice::from_ref(&finding)), 0);
    assert!(ready_for_publish(std::slice::from_ref(&finding)));
}

/// DoD-6: fully empty config (default install) is also Not configured.
#[test]
fn format_embedding_default_config_is_not_configured() {
    use crate::config::model::LocalModelConfig;
    use crate::semantic::BackendStatus;

    let config = LocalModelConfig::default();
    let report = format_embedding_backend_availability(&config, &config);
    assert_eq!(report.status, BackendStatus::NotConfigured);
    assert!(report.is_failure);
    assert!(
        report.display.contains("Not configured"),
        "got: {}",
        report.display
    );
}

/// 0095 DoD-13 / 0109: SCIP findings are info/optional; never block or dashboard.
#[test]
fn scip_findings_sorted_and_mention_go_unwired() {
    let config = crate::config::model::Config::default();
    let findings = collect_scip_findings(&config);
    assert!(!findings.is_empty(), "expected at least Go unwired line");
    let mut sorted = findings.clone();
    sorted.sort_by(|a, b| a.code.cmp(&b.code).then(a.message.cmp(&b.message)));
    assert_eq!(findings, sorted, "findings must be sorted");
    assert!(
        findings
            .iter()
            .any(|f| f.code == "scip-go-not-wired" || f.message.contains("not wired")),
        "must report Go as upstream/not wired: {findings:?}"
    );
    assert!(
        findings.iter().any(|f| f.code == "scip-clang-not-wired"),
        "must report scip-clang as not wired: {findings:?}"
    );
    for f in &findings {
        assert_eq!(f.severity, DoctorSeverity::Info);
        assert_eq!(f.category, DoctorCategory::Optional);
    }
    assert_eq!(dashboard_failures(&findings), 0);
    assert!(ready_for_publish(&findings));
}

/// Doctor must not advertise SCIP as runnable when process policy denies it.
#[test]
fn scip_findings_report_policy_block_when_denied() {
    let mut config = crate::config::model::Config::default();
    config.verify.denied_commands = vec!["rust-analyzer".to_string()];
    let findings = collect_scip_findings(&config);
    for f in &findings {
        if f.message.contains("rust-analyzer") || f.message.contains("Rust") {
            assert!(
                !f.message.contains("available —"),
                "denied rust-analyzer must not look freely available: {}",
                f.message
            );
        }
    }
    assert!(
        findings
            .iter()
            .any(|f| f.message.contains("blocked by process policy")
                || f.message.contains("not available")
                || f.code == "scip-go-not-wired"),
        "expected policy or probe messaging: {findings:?}"
    );
}

#[test]
fn doctor_zero_dashboard_failures_without_scip_indexers_in_tools() {
    // Indexers must not appear in DoctorReport.tools.
    let tools = vec![(
        "git".to_string(),
        ExecutableStatus::Found(PathBuf::from("git")),
    )];
    let _report = sample_report(&tools);
    assert!(
        !tools
            .iter()
            .any(|(n, _)| n.contains("scip") || n.contains("rust-analyzer"))
    );
    // SCIP absence alone is not a dashboard failure.
    let scip = collect_scip_findings(&crate::config::model::Config::default());
    assert_eq!(dashboard_failures(&scip), 0);
}

#[test]
fn graph_current_status_is_not_a_finding() {
    // Healthy graph status lines are display-only (not findings).
    let findings: Vec<DoctorFinding> = Vec::new();
    assert_eq!(dashboard_failures(&findings), 0);
}

#[test]
fn format_active_ask_backend_prefers_gemini_when_configured() {
    let mut config = crate::config::model::Config::default();
    config.gemini.api_key = Some("AIzaTestKey".to_string());
    config.local_model.base_url = "http://127.0.0.1:8081".to_string();
    // Hermetic readers: Gemini wins via explicit api_key regardless of env.
    assert_eq!(
        format_active_ask_backend_with(&config, &|_| None, &|_| None),
        "Gemini (Cloud)"
    );
}

#[test]
fn format_active_ask_backend_prefers_local_when_configured() {
    let mut config = crate::config::model::Config::default();
    config.local_model.base_url = "http://127.0.0.1:8081".to_string();
    config.local_model.generation_model = "test-model".to_string();
    // Hermetic readers returning None so no ambient GEMINI_API_KEY leaks in.
    assert_eq!(
        format_active_ask_backend_with(&config, &|_| None, &|_| None),
        "Local (127.0.0.1)"
    );
}

#[test]
fn format_active_ask_backend_uses_generation_url_host() {
    let mut config = crate::config::model::Config::default();
    config.local_model.generation_url = Some("https://example.com:8080/v1".to_string());
    config.local_model.generation_model = "test-model".to_string();
    assert_eq!(
        format_active_ask_backend_with(&config, &|_| None, &|_| None),
        "Local (example.com)"
    );
}

#[test]
fn parse_url_host_extracts_host_from_http_and_https() {
    assert_eq!(
        parse_url_host("http://127.0.0.1:8081/v1"),
        Some("127.0.0.1".to_string())
    );
    assert_eq!(
        parse_url_host("https://example.com:8080/path"),
        Some("example.com".to_string())
    );
    assert_eq!(parse_url_host("not-a-url"), None);
    assert_eq!(parse_url_host(""), None);
}

#[test]
fn test_write_doctor_results_writes_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = Utf8Path::from_path(tmp.path()).expect("utf8 path");
    let layout = Layout::new(root);
    layout.ensure_state_dir().expect("ensure_state_dir");

    let findings = vec![
        DoctorFinding::block("tool-git", DoctorCategory::Tools, "git missing"),
        DoctorFinding::info(
            "embed-not-configured",
            DoctorCategory::Optional,
            "embed not configured",
        ),
        DoctorFinding::warn("sig-pin", DoctorCategory::Signing, SIG_PIN_WARNING),
    ];
    write_doctor_results(&layout, &findings).expect("write_doctor_results");

    let path = layout.state_subdir().join("doctor-results.json");
    let body = std::fs::read_to_string(path.as_std_path()).expect("read back");
    let json: serde_json::Value = serde_json::from_str(&body).expect("parse");
    // failures = block(1) + non-optional warn sig-pin(1) = 2; optional excluded
    assert_eq!(json["failures"].as_u64(), Some(2));
    assert_eq!(json["readyForPublish"], false);
    assert_eq!(json["block"].as_u64(), Some(1));
    assert_eq!(json["warn"].as_u64(), Some(1));
    assert_eq!(json["info"].as_u64(), Some(1));
    assert!(json["timestamp"].as_str().is_some());
    assert!(json.get("readyForPublishDefinition").is_none());
    // 0129: findings top-N — block+warn only, block first, info excluded
    let findings_arr = json["findings"].as_array().expect("findings array present");
    assert_eq!(findings_arr.len(), 2);
    assert_eq!(findings_arr[0]["code"], "tool-git");
    assert_eq!(findings_arr[0]["severity"], "block");
    assert_eq!(findings_arr[1]["code"], "sig-pin");
    assert_eq!(findings_arr[1]["severity"], "warn");
    assert!(
        findings_arr
            .iter()
            .all(|f| f.get("severity").and_then(|s| s.as_str()) != Some("info")),
        "info must be excluded from findings"
    );
}

#[test]
fn write_doctor_results_optional_only_zero_failures_ready() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = Utf8Path::from_path(tmp.path()).expect("utf8 path");
    let layout = Layout::new(root);
    layout.ensure_state_dir().expect("ensure_state_dir");

    let findings = vec![
        DoctorFinding::info("embed-not-configured", DoctorCategory::Optional, "embed"),
        DoctorFinding::warn(
            "completion-unreachable",
            DoctorCategory::Optional,
            "completion",
        ),
        DoctorFinding::info("tool-gemini", DoctorCategory::Optional, "gemini"),
    ];
    write_doctor_results(&layout, &findings).expect("write");
    let path = layout.state_subdir().join("doctor-results.json");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path.as_std_path()).unwrap()).unwrap();
    assert_eq!(json["failures"].as_u64(), Some(0));
    assert_eq!(json["readyForPublish"], true);
    // 0138: optional-category warns excluded from sidecar findings; warn count honest
    assert_eq!(json["warn"].as_u64(), Some(1));
    let findings_arr = json["findings"].as_array().expect("findings array present");
    assert!(
        findings_arr.is_empty(),
        "optional-only warns must not appear in sidecar findings: {findings_arr:?}"
    );
}

#[test]
fn write_doctor_results_block_before_warn_under_reverse_alpha_codes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = Utf8Path::from_path(tmp.path()).expect("utf8 path");
    let layout = Layout::new(root);
    layout.ensure_state_dir().expect("ensure_state_dir");

    // Input order / alpha would put warn "aaa-warn" before block "zzz-block".
    // Severity-first re-sort must place block first before take(5).
    let findings = vec![
        DoctorFinding::warn("aaa-warn", DoctorCategory::Index, "early alpha warn"),
        DoctorFinding::warn("bbb-warn", DoctorCategory::Signing, "mid warn"),
        DoctorFinding::block("zzz-block", DoctorCategory::Tools, "late alpha block"),
        DoctorFinding::info("ccc-info", DoctorCategory::Optional, "info excluded"),
    ];
    write_doctor_results(&layout, &findings).expect("write");
    let path = layout.state_subdir().join("doctor-results.json");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path.as_std_path()).unwrap()).unwrap();
    let findings_arr = json["findings"].as_array().expect("findings");
    assert_eq!(findings_arr.len(), 3, "info excluded → 3 entries");
    assert_eq!(findings_arr[0]["code"], "zzz-block");
    assert_eq!(findings_arr[0]["severity"], "block");
    assert_eq!(findings_arr[1]["code"], "aaa-warn");
    assert_eq!(findings_arr[1]["severity"], "warn");
    assert_eq!(findings_arr[2]["code"], "bbb-warn");
}

#[test]
fn write_doctor_results_remediation_round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = Utf8Path::from_path(tmp.path()).expect("utf8 path");
    let layout = Layout::new(root);
    layout.ensure_state_dir().expect("ensure_state_dir");

    let findings = vec![
        DoctorFinding::warn("sig-pin", DoctorCategory::Signing, "pin missing")
            .with_remediation("ledgerful config set intent.trusted_public_keys '[\"abc\"]'"),
        DoctorFinding::warn("graph-stale", DoctorCategory::Index, "graph stale"),
    ];
    write_doctor_results(&layout, &findings).expect("write");
    let path = layout.state_subdir().join("doctor-results.json");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path.as_std_path()).unwrap()).unwrap();
    let findings_arr = json["findings"].as_array().expect("findings");
    assert_eq!(findings_arr.len(), 2);
    let pin = findings_arr
        .iter()
        .find(|f| f["code"] == "sig-pin")
        .expect("sig-pin present");
    assert_eq!(
        pin["remediation"].as_str(),
        Some("ledgerful config set intent.trusted_public_keys '[\"abc\"]'")
    );
    let stale = findings_arr
        .iter()
        .find(|f| f["code"] == "graph-stale")
        .expect("graph-stale present");
    assert!(
        stale.get("remediation").is_none(),
        "must omit remediation key when None, not emit null: {stale}"
    );
}

#[test]
fn write_doctor_results_findings_cap_five() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = Utf8Path::from_path(tmp.path()).expect("utf8 path");
    let layout = Layout::new(root);
    layout.ensure_state_dir().expect("ensure_state_dir");

    let mut findings = Vec::new();
    for i in 0..4 {
        findings.push(DoctorFinding::block(
            format!("block-{i}"),
            DoctorCategory::Tools,
            format!("block msg {i}"),
        ));
    }
    for i in 0..4 {
        findings.push(DoctorFinding::warn(
            format!("warn-{i}"),
            DoctorCategory::Index,
            format!("warn msg {i}"),
        ));
    }
    write_doctor_results(&layout, &findings).expect("write");
    let path = layout.state_subdir().join("doctor-results.json");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path.as_std_path()).unwrap()).unwrap();
    let findings_arr = json["findings"].as_array().expect("findings");
    assert_eq!(findings_arr.len(), 5);
    // All 4 blocks come first, then first warn alphabetically
    assert!(
        findings_arr
            .iter()
            .take(4)
            .all(|f| f["severity"] == "block"),
        "first 4 must be blocks: {findings_arr:?}"
    );
    assert_eq!(findings_arr[4]["severity"], "warn");
}

#[test]
fn select_sidecar_top_findings_excludes_info() {
    let findings = vec![
        DoctorFinding::info("i1", DoctorCategory::Optional, "info"),
        DoctorFinding::warn("w1", DoctorCategory::Signing, "warn"),
        DoctorFinding::block("b1", DoctorCategory::Tools, "block"),
    ];
    let top = select_sidecar_top_findings(&findings);
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].code, "b1");
    assert_eq!(top[1].code, "w1");
}

#[test]
fn select_sidecar_top_findings_excludes_optional_warn() {
    // Mixed optional + signing → only signing (0138).
    let findings = vec![
        DoctorFinding::warn(
            "completion-unreachable",
            DoctorCategory::Optional,
            "optional down",
        ),
        DoctorFinding::warn("sig-pin", DoctorCategory::Signing, "pin missing"),
        DoctorFinding::warn("embed-unreachable", DoctorCategory::Optional, "embed down"),
        DoctorFinding::warn(
            "binary-behind-tree",
            DoctorCategory::Tools,
            "PATH binary lags tree",
        ),
    ];
    let top = select_sidecar_top_findings(&findings);
    assert_eq!(top.len(), 2, "optional warns excluded: {top:?}");
    let codes: Vec<&str> = top.iter().map(|f| f.code.as_str()).collect();
    assert_eq!(codes, vec!["binary-behind-tree", "sig-pin"]);
    assert!(
        top.iter().all(|f| f.category != DoctorCategory::Optional),
        "no optional codes in top: {codes:?}"
    );
}

#[test]
fn select_sidecar_top_findings_cap_five_optional_does_not_consume() {
    // 4 Tools blocks + 2 optional warns + 2 Index warns → 5 entries
    // (4 blocks + 1 index by sort); optional does not consume cap budget.
    let mut findings = Vec::new();
    for i in 0..4 {
        findings.push(DoctorFinding::block(
            format!("block-{i}"),
            DoctorCategory::Tools,
            format!("block msg {i}"),
        ));
    }
    findings.push(DoctorFinding::warn(
        "completion-unreachable",
        DoctorCategory::Optional,
        "optional a",
    ));
    findings.push(DoctorFinding::warn(
        "embed-unreachable",
        DoctorCategory::Optional,
        "optional b",
    ));
    findings.push(DoctorFinding::warn(
        "search-corrupt",
        DoctorCategory::Index,
        "index a",
    ));
    findings.push(DoctorFinding::warn(
        "search-empty",
        DoctorCategory::Index,
        "index b",
    ));
    let top = select_sidecar_top_findings(&findings);
    assert_eq!(top.len(), 5, "cap 5: {top:?}");
    assert!(
        top.iter()
            .take(4)
            .all(|f| f.severity == DoctorSeverity::Block),
        "first 4 must be blocks: {top:?}"
    );
    assert_eq!(top[4].severity, DoctorSeverity::Warn);
    assert_eq!(top[4].category, DoctorCategory::Index);
    // First index warn alphabetically by code: search-corrupt before search-empty
    assert_eq!(top[4].code, "search-corrupt");
    assert!(
        top.iter().all(|f| f.category != DoctorCategory::Optional),
        "optional must not consume cap: {:?}",
        top.iter().map(|f| f.code.as_str()).collect::<Vec<_>>()
    );
}

#[test]
fn select_sidecar_top_findings_optional_block_still_surfaces() {
    // 1 Optional block + 1 Signing warn → both (block first) — locks B1.
    let findings = vec![
        DoctorFinding::warn("sig-pin", DoctorCategory::Signing, "pin missing"),
        DoctorFinding::block(
            "optional-block",
            DoctorCategory::Optional,
            "hypothetical optional block",
        ),
    ];
    let top = select_sidecar_top_findings(&findings);
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].code, "optional-block");
    assert_eq!(top[0].severity, DoctorSeverity::Block);
    assert_eq!(top[1].code, "sig-pin");
    assert_eq!(top[1].severity, DoctorSeverity::Warn);
}

#[test]
fn test_is_transient_error() {
    assert!(is_transient_error("unreachable (connection refused)"));
    assert!(is_transient_error("timed out after 2s"));
    assert!(is_transient_error("503 server error (Service Unavailable)"));
    assert!(is_transient_error("502 Bad Gateway"));
    assert!(is_transient_error("504 Gateway Timeout"));

    // Semantic errors should not be transient
    assert!(!is_transient_error("400 server error (pooling type none)"));
    assert!(!is_transient_error("401 server error (Unauthorized)"));
    assert!(!is_transient_error("404 server error (Not Found)"));
    assert!(!is_transient_error("some custom error"));
}

#[test]
fn test_probe_with_retry_healthy() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let count = std::sync::Arc::new(AtomicUsize::new(0));
    let count_probe = std::sync::Arc::clone(&count);
    let res = probe_with_retry(move || {
        count_probe.fetch_add(1, Ordering::SeqCst);
        Ok("success")
    });
    assert!(matches!(res, ProbeResult::Healthy("success")));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn test_probe_with_retry_flaky_success() {
    // Tiny budget, but generous enough relative to the tiny test delay
    // for 2 quick retries to land before the budget is exhausted.
    // max_retries must be ≥ 2 so the flap-recovery path under test can
    // reach the third attempt (0143).
    use std::sync::atomic::{AtomicUsize, Ordering};
    let budget = std::time::Duration::from_millis(50);
    let delay = std::time::Duration::from_millis(1);
    let deadline = std::time::Duration::from_millis(500);
    let count = std::sync::Arc::new(AtomicUsize::new(0));
    let count_probe = std::sync::Arc::clone(&count);
    let res = probe_with_retry_budgeted(
        move || {
            let n = count_probe.fetch_add(1, Ordering::SeqCst) + 1;
            if n < 3 {
                Err("unreachable (connection refused)".to_string())
            } else {
                Ok("success")
            }
        },
        budget,
        delay,
        deadline,
        2, // ≥ 2 so flap recovery can reach attempt 3
    );
    assert!(matches!(
        res,
        ProbeResult::ReachableAfterRetry {
            val: "success",
            retries: 2
        }
    ));
    assert_eq!(count.load(Ordering::SeqCst), 3);
}

#[test]
fn test_probe_with_retry_hard_unreachable() {
    // A probe that always fails transiently must eventually stop
    // retrying once the (tiny, test-only) budget is exhausted, rather
    // than retrying forever. High max_retries so the wall budget (not
    // the attempt cap) is the stop condition under test.
    use std::sync::atomic::{AtomicUsize, Ordering};
    let budget = std::time::Duration::from_millis(20);
    let delay = std::time::Duration::from_millis(5);
    let deadline = std::time::Duration::from_millis(500);
    let count = std::sync::Arc::new(AtomicUsize::new(0));
    let count_probe = std::sync::Arc::clone(&count);
    let res: ProbeResult<()> = probe_with_retry_budgeted(
        move || {
            count_probe.fetch_add(1, Ordering::SeqCst);
            Err("unreachable (connection refused)".to_string())
        },
        budget,
        delay,
        deadline,
        100,
    );
    match res {
        ProbeResult::Unreachable { err, retries } => {
            assert_eq!(err, "unreachable (connection refused)");
            // Budget is small relative to delay, so retries must be bounded.
            assert!(retries <= 10, "retries should stay small: {retries}");
            assert_eq!(
                count.load(Ordering::SeqCst),
                retries as usize + 1,
                "count is always retries + 1 initial attempt"
            );
        }
        other => panic!("expected Unreachable, got {other:?}"),
    }
}

#[test]
fn test_probe_with_retry_budget_exhausted_stops_retrying() {
    // With a zero retry budget, a transient failure must return
    // Unreachable after exactly the first attempt with zero retries -
    // i.e. the budget check itself (not just is_transient_error) gates
    // whether a retry happens at all.
    use std::sync::atomic::{AtomicUsize, Ordering};
    let budget = std::time::Duration::from_millis(0);
    let delay = std::time::Duration::from_millis(1);
    let deadline = std::time::Duration::from_millis(500);
    let count = std::sync::Arc::new(AtomicUsize::new(0));
    let count_probe = std::sync::Arc::clone(&count);
    let res: ProbeResult<()> = probe_with_retry_budgeted(
        move || {
            count_probe.fetch_add(1, Ordering::SeqCst);
            Err("unreachable (connection refused)".to_string())
        },
        budget,
        delay,
        deadline,
        10,
    );
    assert!(
        matches!(res, ProbeResult::Unreachable { ref err, retries: 0 } if err == "unreachable (connection refused)")
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn test_probe_with_retry_max_retries_caps_attempts() {
    // Production-shaped max_retries=1: fast-fail transient errors get at
    // most one retry (two attempts total), even when the wall budget
    // would still allow more (0143 Fix A).
    use std::sync::atomic::{AtomicUsize, Ordering};
    let budget = std::time::Duration::from_millis(1500);
    let delay = std::time::Duration::from_millis(1);
    let deadline = std::time::Duration::from_millis(500);
    let count = std::sync::Arc::new(AtomicUsize::new(0));
    let count_probe = std::sync::Arc::clone(&count);
    let res: ProbeResult<()> = probe_with_retry_budgeted(
        move || {
            count_probe.fetch_add(1, Ordering::SeqCst);
            Err("unreachable (connection refused)".to_string())
        },
        budget,
        delay,
        deadline,
        1, // production PROBE_MAX_RETRIES
    );
    assert!(
        matches!(res, ProbeResult::Unreachable { ref err, retries: 1 } if err == "unreachable (connection refused)")
    );
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[test]
fn test_probe_with_retry_wall_clock_bounded() {
    // Regression test for the latency-regression finding: a probe that
    // always fails transiently must not cause probe_with_retry to
    // spend more than a small, bounded amount of wall-clock time
    // sleeping between retries. Uses the tiny test budget (not the
    // real RETRY_BUDGET) so the test itself stays fast; the ceiling
    // is generous relative to that budget to avoid flakiness on a
    // loaded CI machine, while still catching an unbounded-retry
    // regression (which would blow well past it).
    let budget = std::time::Duration::from_millis(50);
    let delay = std::time::Duration::from_millis(5);
    let deadline = std::time::Duration::from_millis(500);
    let start = std::time::Instant::now();
    let res: ProbeResult<()> = probe_with_retry_budgeted(
        || Err("unreachable (connection refused)".to_string()),
        budget,
        delay,
        deadline,
        100,
    );
    let elapsed = start.elapsed();
    assert!(matches!(res, ProbeResult::Unreachable { .. }));
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "probe_with_retry_budgeted took {elapsed:?}, expected well under 500ms for a {budget:?} budget"
    );
}

#[test]
fn test_probe_with_retry_semantic_fail_no_retry() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let count = std::sync::Arc::new(AtomicUsize::new(0));
    let count_probe = std::sync::Arc::clone(&count);
    let res: ProbeResult<()> = probe_with_retry(move || {
        count_probe.fetch_add(1, Ordering::SeqCst);
        Err("401 server error (Unauthorized)".to_string())
    });
    assert!(
        matches!(res, ProbeResult::Unreachable { ref err, retries: 0 } if err == "401 server error (Unauthorized)")
    );
    assert_eq!(count.load(Ordering::SeqCst), 1); // Fail immediately, no retry
}

/// DoD-5 / 0143 B1: hung probe must surface Unreachable via hard deadline
/// without waiting for the full sleep (join-first would block ~1s).
#[test]
fn test_probe_with_retry_hang_hard_deadline() {
    let budget = std::time::Duration::from_millis(30);
    let delay = std::time::Duration::from_millis(1);
    let deadline = std::time::Duration::from_millis(50);
    let start = std::time::Instant::now();
    let res: ProbeResult<()> = probe_with_retry_budgeted(
        || {
            std::thread::sleep(std::time::Duration::from_secs(1));
            Ok(())
        },
        budget,
        delay,
        deadline,
        1, // production-shaped; budget also blocks further retries after hang
    );
    let elapsed = start.elapsed();
    match res {
        ProbeResult::Unreachable { err, .. } => {
            assert!(
                err.contains("timed out"),
                "error should mention timed out, got: {err}"
            );
        }
        other => panic!("expected Unreachable on hang, got {other:?}"),
    }
    assert!(
        elapsed < std::time::Duration::from_millis(200),
        "hang hard-deadline took {elapsed:?}, expected well under 200ms"
    );
}

/// DoD-6 / R5: clean repo produces zero legacy-migration findings.
#[test]
fn legacy_findings_silent_on_clean_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    std::fs::write(root.join(".gitignore"), "target/\n.ledgerful/\n").unwrap();
    std::fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
    std::fs::write(
        root.join(".git").join("hooks").join("pre-commit"),
        "#!/bin/sh\necho ok\n",
    )
    .unwrap();

    let findings = collect_legacy_migration_findings(root, &layout);
    assert!(
        findings.is_empty(),
        "clean repo must be silent: {findings:?}"
    );
}

#[test]
fn git_missing_is_block_gemini_missing_is_info() {
    // Classification table sample (tools loop identity).
    let git = DoctorFinding::block("tool-git", DoctorCategory::Tools, "git NOT FOUND");
    let gemini = DoctorFinding::info("tool-gemini", DoctorCategory::Optional, "gemini NOT FOUND");
    assert_eq!(git.severity, DoctorSeverity::Block);
    assert_eq!(gemini.severity, DoctorSeverity::Info);
    assert!(!ready_for_publish(std::slice::from_ref(&git)));
    assert!(ready_for_publish(std::slice::from_ref(&gemini)));
    assert_eq!(dashboard_failures(&[git, gemini]), 1);
}

/// 0209-B2: collect_tool_findings gemini message matches Tools phrasing.
#[test]
fn collect_tool_findings_gemini_message_is_optional_cli_not_cloud_ask() {
    let tools = vec![("gemini".to_string(), ExecutableStatus::NotFound)];
    let findings = super::checks::tools::collect_tool_findings(&tools);
    assert_eq!(findings.len(), 1);
    let f = &findings[0];
    assert_eq!(f.code, "tool-gemini");
    assert_eq!(f.severity, DoctorSeverity::Info);
    assert_eq!(f.category, DoctorCategory::Optional);
    assert_eq!(
        f.message,
        "gemini NOT FOUND (optional CLI; not the Cloud Ask backend)"
    );
    assert!(f.remediation.is_none());
    let lower = f.message.to_ascii_lowercase();
    assert!(!lower.contains("install"));
    assert!(!lower.contains("npm"));
    assert!(!lower.contains("antigravity"));
    assert!(ready_for_publish(std::slice::from_ref(f)));

    let cli_tools = vec![("gemini-cli".to_string(), ExecutableStatus::NotFound)];
    let cli_findings = super::checks::tools::collect_tool_findings(&cli_tools);
    assert_eq!(cli_findings.len(), 1);
    assert_eq!(cli_findings[0].severity, DoctorSeverity::Info);
    assert_eq!(cli_findings[0].category, DoctorCategory::Optional);
    assert_eq!(
        cli_findings[0].message,
        "gemini-cli NOT FOUND (optional CLI; not the Cloud Ask backend)"
    );
    assert!(cli_findings[0].remediation.is_none());
}

/// DoD-6: Design-shaped residue produces expected finding categories.
#[test]
fn legacy_findings_report_four_surfaces() {
    let tmp = tempfile::tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let layout = Layout::new(root);
    // Both dirs present (no-merge case) so legacy state is still visible.
    layout.ensure_state_dir().unwrap();
    let legacy = root.join(crate::state::layout::LEGACY_STATE_DIR);
    std::fs::create_dir_all(legacy.join("state")).unwrap();
    std::fs::write(legacy.join("state").join("ledger.db"), b"x").unwrap();

    // Gitignore only names the legacy path.
    std::fs::write(
        root.join(".gitignore"),
        format!("{}/\n", crate::state::layout::LEGACY_STATE_DIR),
    )
    .unwrap();

    // Legacy hook marker + invocation.
    std::fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
    let brand = crate::state::layout::LEGACY_STATE_DIR.trim_start_matches('.');
    std::fs::write(
            root.join(".git").join("hooks").join("pre-commit"),
            format!(
                "#!/bin/sh\n# {brand}-ledger-gate: x\nif command -v {brand} &>/dev/null; then\n  {brand} ledger status\nfi\n"
            ),
        )
        .unwrap();

    // Unknown config keys.
    std::fs::write(
        layout.config_file(),
        "[core]\nstrict = false\n[totally_unknown_section]\nx = 1\n",
    )
    .unwrap();

    let findings = collect_legacy_migration_findings(root, &layout);
    assert!(
        findings.iter().any(|f| f.code == "legacy-state"),
        "state: {findings:?}"
    );
    assert!(
        findings.iter().any(|f| f.code == "legacy-hooks"),
        "hooks: {findings:?}"
    );
    assert!(
        findings.iter().any(|f| f.code == "legacy-gitignore"),
        "gitignore: {findings:?}"
    );
    assert!(
        findings.iter().any(|f| f.code == "legacy-config"),
        "config: {findings:?}"
    );
    for f in &findings {
        assert_eq!(f.severity, DoctorSeverity::Warn);
        assert_eq!(f.category, DoctorCategory::Migration);
    }
    // Deterministic sort.
    let mut sorted = findings.clone();
    sorted.sort_by(|a, b| a.code.cmp(&b.code).then(a.message.cmp(&b.message)));
    assert_eq!(findings, sorted);
    // Remediation commands named.
    assert!(
        findings
            .iter()
            .any(|f| f.message.contains("update --repair-hooks")),
        "must name repair command: {findings:?}"
    );
}

/// DoD-12: documented sequence auto-clears state/hooks/gitignore; config
/// residual may remain as WARNING with named remediation (spec §4 forbids
/// auto-rewriting user config). Not a "fully clean doctor" claim.
#[test]
fn e2e_four_surface_stale_auto_surfaces_clean_config_may_remain() {
    let tmp = tempfile::tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();

    // Surface 1: legacy state dir only (will migrate on load_startup_config).
    let legacy = root.join(crate::state::layout::LEGACY_STATE_DIR);
    std::fs::create_dir_all(legacy.join("state")).unwrap();
    std::fs::write(legacy.join("state").join("marker"), "x").unwrap();
    std::fs::write(
        legacy.join("config.toml"),
        "[core]\nstrict = false\n[totally_unknown_section]\nx = 1\n",
    )
    .unwrap();

    // Surface 3: gitignore only legacy.
    std::fs::write(
        root.join(".gitignore"),
        format!("target/\n{}/\n", crate::state::layout::LEGACY_STATE_DIR),
    )
    .unwrap();

    // Surface 2: legacy hooks.
    std::fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
    let brand = crate::state::layout::LEGACY_STATE_DIR.trim_start_matches('.');
    std::fs::write(
            root.join(".git").join("hooks").join("pre-commit"),
            format!(
                "#!/bin/sh\n# {brand}-ledger-gate: auto-installed by `{brand} init`\nif command -v {brand} &>/dev/null; then\n    if ! {brand} ledger status --compact --exit-code 2>/dev/null; then\n        exit 1\n    fi\nfi\n"
            ),
        )
        .unwrap();

    let layout = Layout::new(root);

    // Documented sequence step 1: repo-scoped command → migrate + gitignore
    // side-effect on successful rename (emulate load_startup_config).
    let renamed = layout.migrate_legacy_state_dir().unwrap();
    assert!(renamed);
    crate::git::ignore::add_to_gitignore(root, ".ledgerful/").unwrap();

    // Documented sequence step 2: update --repair-hooks.
    let report = crate::commands::hook_repair::repair_hooks_at(root, false).unwrap();
    assert!(
        report.residual_invocations.is_empty(),
        "hooks must be fully repaired: {report:?}"
    );

    // After steps 1–2: auto-fixed surfaces must be clean.
    // Unknown config keys may still warn until the user edits config — that
    // is reported with remediation, not auto-rewritten (spec §4).
    let findings = collect_legacy_migration_findings(root, &layout);
    assert!(
        !findings.iter().any(|f| f.code == "legacy-hooks"),
        "hooks clean after repair: {findings:?}"
    );
    assert!(
        !findings.iter().any(|f| f.code == "legacy-gitignore"),
        "gitignore has .ledgerful/ after migrate: {findings:?}"
    );
    assert!(
        !findings.iter().any(|f| f.code == "legacy-state"),
        "legacy dir renamed away: {findings:?}"
    );
    // Config residual is allowed and must name explicit remediation
    // (review/init) — never silent auto-rewrite.
    if let Some(cfg_f) = findings.iter().find(|f| f.code == "legacy-config") {
        assert!(
            cfg_f.message.contains("init") || cfg_f.message.contains("Review"),
            "config finding must name remediation: {cfg_f:?}"
        );
    }
}

#[test]
fn split_brain_warns_when_local_and_shared_db_differ() {
    let tmp = tempfile::tempdir().unwrap();
    let work = Utf8PathBuf::from_path_buf(tmp.path().join("linked")).unwrap();
    let main_state =
        Utf8PathBuf::from_path_buf(tmp.path().join("main").join(".ledgerful")).unwrap();
    std::fs::create_dir_all(work.join(".ledgerful").join("state").as_std_path()).unwrap();
    std::fs::create_dir_all(main_state.join("state").as_std_path()).unwrap();
    std::fs::write(
        work.join(".ledgerful")
            .join("state")
            .join("ledger.db")
            .as_std_path(),
        b"local",
    )
    .unwrap();
    std::fs::write(
        main_state.join("state").join("ledger.db").as_std_path(),
        b"shared",
    )
    .unwrap();

    let layout = Layout::from_roots(&work, &main_state);
    let warn = split_brain_ledger_warning(&layout);
    assert!(warn.is_some(), "must warn when local != shared");
    assert!(
        warn.unwrap().contains("worktree-split-brain"),
        "expected split-brain tag"
    );
}

#[test]
fn split_brain_silent_when_paths_are_same_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let layout = Layout::new(&root);
    layout.ensure_state_dir().unwrap();
    std::fs::write(layout.state_subdir().join("ledger.db").as_std_path(), b"db").unwrap();
    assert!(
        split_brain_ledger_warning(&layout).is_none(),
        "single-tree layout must not warn"
    );
}
