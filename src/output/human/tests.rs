use super::*;
use crate::exec::ExecutionResult;
use crate::platform::env::ExecutableStatus;

#[test]
fn print_verify_result_quiet_suppresses_success_keeps_failure() {
    use crate::output::verification::verify_step_result_label;

    // Pure gate: SUCCESS text not emitted when verbose=false; FAILURE always.
    assert_eq!(
        verify_step_result_label(0, false),
        None,
        "quiet SUCCESS must not emit SUCCESS text"
    );
    assert_eq!(
        verify_step_result_label(0, true),
        Some("SUCCESS"),
        "verbose SUCCESS must emit SUCCESS"
    );
    assert_eq!(
        verify_step_result_label(1, false),
        Some("FAILURE"),
        "quiet FAILURE must still emit FAILURE"
    );
    assert_eq!(
        verify_step_result_label(1, true),
        Some("FAILURE"),
        "verbose FAILURE must emit FAILURE"
    );

    // Smoke: production print path must not panic either way.
    let pass = ExecutionResult {
        exit_code: 0,
        stdout: String::new(),
        stderr: String::new(),
        duration: std::time::Duration::from_millis(1),
        truncated: false,
    };
    let fail = ExecutionResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: "err".into(),
        duration: std::time::Duration::from_millis(1),
        truncated: false,
    };
    print_verify_result("step", 30, &pass, false);
    print_verify_result("step", 30, &pass, true);
    print_verify_result("step", 30, &fail, false);
    print_verify_result("step", 30, &fail, true);
}

#[test]
fn doctor_summary_text_four_way() {
    assert_eq!(
        format_doctor_summary_text(1, 0, 0, 0),
        "✗ Doctor: 1 block issue(s)"
    );
    assert_eq!(
        format_doctor_summary_text(0, 2, 0, 0),
        "✓ Doctor: ready for publish env · 2 warning(s)"
    );
    assert_eq!(
        format_doctor_summary_text(0, 0, 0, 3),
        "✓ Doctor: ready for publish env · 3 hint(s)"
    );
    assert_eq!(
        format_doctor_summary_text(0, 0, 0, 0),
        "✓ Doctor: all checks passed"
    );
    // Block wins over warn/info when present.
    assert!(format_doctor_summary_text(1, 9, 0, 9).contains("block issue"));
    // Warn uses ready shape — never red soft-fail wording.
    assert!(!format_doctor_summary_text(0, 2, 0, 0).contains("issue(s) found"));
}

/// 0209 DoD-1 six-row header copy (unit fixtures, not live dogfood).
#[test]
fn doctor_summary_text_warn_split_six_row() {
    // Row 1: 3 action + 1 optional
    let row1 = format_doctor_summary_text(0, 3, 1, 0);
    assert!(row1.contains("3 warning(s) · 1 optional"), "{row1}");
    assert!(!row1.contains("optional warning(s)"), "{row1}");

    // Row 2: 3 action only
    let row2 = format_doctor_summary_text(0, 3, 0, 0);
    assert!(row2.contains("3 warning(s)"), "{row2}");
    assert!(!row2.contains("optional"), "{row2}");

    // Row 3: 1 optional only — no "0 warning", no "optional warning(s)"
    let row3 = format_doctor_summary_text(0, 0, 1, 0);
    assert!(row3.contains("· 1 optional"), "{row3}");
    assert!(!row3.contains("0 warning"), "{row3}");
    assert!(!row3.contains("optional warning(s)"), "{row3}");
    assert!(!row3.contains("warning(s)"), "{row3}");

    // Row 4: block wins; no ready-shape
    let row4 = format_doctor_summary_text(1, 2, 1, 0);
    assert!(row4.contains("block issue(s)"), "{row4}");
    assert!(!row4.contains("ready for publish"), "{row4}");

    // Row 5: empty
    let row5 = format_doctor_summary_text(0, 0, 0, 0);
    assert!(row5.contains("all checks passed"), "{row5}");

    // Row 6: info only
    let row6 = format_doctor_summary_text(0, 0, 0, 3);
    assert!(row6.contains("3 hint(s)"), "{row6}");
}

#[test]
fn format_doctor_tool_line_gemini_cli_vs_cloud_ask() {
    use std::path::PathBuf;

    let not_found = ExecutableStatus::NotFound;
    let gemini_found = ExecutableStatus::Found(PathBuf::from(r"C:\Users\bin\gemini.exe"));
    let gemini_cli_found = ExecutableStatus::Found(PathBuf::from(r"C:\Users\bin\gemini-cli.exe"));

    let banned = |text: &str| {
        let lower = text.to_ascii_lowercase();
        assert!(!lower.contains("install"), "{text}");
        assert!(!lower.contains("npm"), "{text}");
        assert!(!lower.contains("antigravity"), "{text}");
    };

    // Row 1: gemini NotFound
    let (label, text) = format_doctor_tool_line("gemini", &not_found);
    assert_eq!(label, "gemini CLI");
    assert!(text.contains("NOT FOUND"), "{text}");
    assert!(text.contains("optional CLI"), "{text}");
    assert!(text.contains("not the Cloud Ask backend"), "{text}");
    banned(&text);

    // Row 2: gemini-cli NotFound
    let (label, text) = format_doctor_tool_line("gemini-cli", &not_found);
    assert_eq!(label, "gemini CLI");
    assert!(text.contains("NOT FOUND"), "{text}");
    assert!(text.contains("optional CLI"), "{text}");
    assert!(text.contains("not the Cloud Ask backend"), "{text}");
    banned(&text);

    // Row 3: gemini Found
    let (label, text) = format_doctor_tool_line("gemini", &gemini_found);
    assert_eq!(label, "gemini CLI");
    assert!(text.contains("Found ("), "{text}");
    assert!(!text.contains("NOT FOUND"), "{text}");

    // Row 4: git NotFound — unchanged, no CLI/Ask clause
    let (label, text) = format_doctor_tool_line("git", &not_found);
    assert_eq!(label, "git");
    assert_eq!(text, "NOT FOUND");
    assert!(!text.contains("Cloud Ask"), "{text}");
    assert!(!text.contains("optional CLI"), "{text}");

    // Row 5: gemini-cli Found
    let (label, text) = format_doctor_tool_line("gemini-cli", &gemini_cli_found);
    assert_eq!(label, "gemini CLI");
    assert!(text.contains("Found ("), "{text}");
    assert!(!text.contains("NOT FOUND"), "{text}");
}

#[test]
fn format_hygiene_collapse_trailer_optional_clause() {
    let t11 = format_hygiene_collapse_trailer(11, 1);
    assert!(t11.contains("11 hygiene finding(s) collapsed"), "{t11}");
    assert!(t11.contains("1 optional warning"), "{t11}");
    assert!(t11.contains("doctor --full"), "{t11}");

    let t12 = format_hygiene_collapse_trailer(12, 2);
    assert!(t12.contains("2 optional warnings"), "{t12}");

    let t10 = format_hygiene_collapse_trailer(10, 0);
    assert_eq!(t10, "10 hygiene finding(s) collapsed — run doctor --full");
    assert!(!t10.contains("optional"), "{t10}");
}

#[test]
fn format_signing_deferred_trailer_exact() {
    assert_eq!(
        format_signing_deferred_trailer(3),
        "3 signing finding(s) deferred (observe) — run doctor --full"
    );
    assert_eq!(
        format_signing_deferred_trailer(1),
        "1 signing finding(s) deferred (observe) — run doctor --full"
    );
    let hygiene = format_hygiene_collapse_trailer(10, 0);
    assert_eq!(
        hygiene,
        "10 hygiene finding(s) collapsed — run doctor --full"
    );
    assert!(!hygiene.contains("signing"), "{hygiene}");
    assert!(!hygiene.contains("deferred"), "{hygiene}");
}

/// 0174 T1–T5: human 3-tier partition + --full expands hygiene.
#[test]
fn doctor_human_partition_three_tier() {
    use crate::commands::doctor::{DoctorCategory, DoctorFinding};

    let findings = vec![
        DoctorFinding::warn(
            "completion-unreachable",
            DoctorCategory::Optional,
            "completion down",
        ),
        DoctorFinding::warn("hook-template-stale", DoctorCategory::Gate, "hooks stale"),
        DoctorFinding::warn("sig-pin", DoctorCategory::Signing, "no keys")
            .with_remediation("ledgerful config set 'intent.trusted_public_keys=[\"hex\"]'"),
        DoctorFinding::block("tool-git", DoctorCategory::Tools, "git missing"),
        DoctorFinding::info("sccache-hint", DoctorCategory::Optional, "install sccache"),
    ];

    let (index, optional, hygiene) = partition_doctor_findings_for_human(&findings, false);
    // T1 optional warn + T2 info (sccache) collapsed → hygiene_count=2
    // 0226: hook-template-stale is Warn/Gate → expanded, not hygiene
    assert_eq!(hygiene, 2);
    // T3 sig-pin + T4 block + hook-template-stale expanded
    let codes: Vec<&str> = index.iter().map(|f| f.code.as_str()).collect();
    assert!(codes.contains(&"sig-pin"));
    assert!(codes.contains(&"tool-git"));
    assert!(codes.contains(&"hook-template-stale"));
    assert!(!codes.contains(&"completion-unreachable"));
    assert!(
        optional.is_empty(),
        "optional findings collapsed by default"
    );

    // T5 --full expands hygiene
    let (index_full, optional_full, hygiene_full) =
        partition_doctor_findings_for_human(&findings, true);
    assert_eq!(hygiene_full, 2);
    let full_index_codes: Vec<&str> = index_full.iter().map(|f| f.code.as_str()).collect();
    assert!(full_index_codes.contains(&"hook-template-stale"));
    assert!(full_index_codes.contains(&"sig-pin"));
    assert!(full_index_codes.contains(&"tool-git"));
    let full_opt_codes: Vec<&str> = optional_full.iter().map(|f| f.code.as_str()).collect();
    assert!(full_opt_codes.contains(&"completion-unreachable"));
    assert!(full_opt_codes.contains(&"sccache-hint"));

    let trailer = format_hygiene_collapse_trailer(3, 1);
    assert!(trailer.contains("3 hygiene finding(s) collapsed"));
    assert!(trailer.contains("doctor --full"));
}

/// 0225: later signing omitted from default Index Health; hygiene_count unchanged.
#[test]
fn doctor_human_partition_later_signing_omitted_not_hygiene() {
    use crate::commands::doctor::{DoctorCategory, DoctorFinding, SessionPriority};

    let mut later_pin = DoctorFinding::warn("sig-pin", DoctorCategory::Signing, "no keys");
    later_pin.session_priority = SessionPriority::Later;
    let mut later_ver = DoctorFinding::warn("sig-version", DoctorCategory::Signing, "v1 rows");
    later_ver.session_priority = SessionPriority::Later;
    let mut later_phantom = DoctorFinding::warn(
        "PHANTOM_PROMOTED_WITHOUT_VERIFY",
        DoctorCategory::Signing,
        "phantoms",
    );
    later_phantom.session_priority = SessionPriority::Later;
    let behind = DoctorFinding::warn(
        "binary-behind-tree",
        DoctorCategory::Tools,
        "PATH binary lags tree",
    );
    let hygiene_info =
        DoctorFinding::info("sccache-hint", DoctorCategory::Optional, "sccache hint");
    let hook_stale =
        DoctorFinding::warn("hook-template-stale", DoctorCategory::Gate, "hooks stale");

    let findings = vec![
        later_pin,
        later_ver,
        later_phantom,
        behind,
        hygiene_info,
        hook_stale,
    ];

    let (index, optional, hygiene) = partition_doctor_findings_for_human(&findings, false);
    assert_eq!(hygiene, 1, "later must not increment hygiene_count");
    let codes: Vec<&str> = index.iter().map(|f| f.code.as_str()).collect();
    assert!(codes.contains(&"binary-behind-tree"));
    assert!(codes.contains(&"hook-template-stale"));
    assert!(optional.is_empty());
    assert!(!codes.contains(&"sig-pin"));
    assert!(!codes.contains(&"sig-version"));
    assert!(!codes.contains(&"PHANTOM_PROMOTED_WITHOUT_VERIFY"));

    let (index_full, _, hygiene_full) = partition_doctor_findings_for_human(&findings, true);
    assert_eq!(hygiene_full, 1);
    let full_codes: Vec<&str> = index_full.iter().map(|f| f.code.as_str()).collect();
    assert!(full_codes.contains(&"sig-pin"));
    assert!(full_codes.contains(&"sig-version"));
    assert!(full_codes.contains(&"PHANTOM_PROMOTED_WITHOUT_VERIFY"));
    assert!(full_codes.contains(&"binary-behind-tree"));
    assert!(full_codes.contains(&"hook-template-stale"));
}

/// 0225 Codex P2-1: printer emits deferred trailer; `--full` expands bodies.
#[test]
fn print_doctor_report_later_trailer_and_full_expand() {
    use crate::commands::doctor::{DoctorCategory, DoctorFinding, SessionPriority, summarize};

    fn later(code: &str, msg: &str) -> DoctorFinding {
        let mut f = DoctorFinding::warn(code, DoctorCategory::Signing, msg);
        f.session_priority = SessionPriority::Later;
        f
    }

    let findings = vec![
        later("PHANTOM_PROMOTED_WITHOUT_VERIFY", "phantoms"),
        later("sig-pin", "no keys"),
        later("sig-version", "v1 rows"),
        DoctorFinding::warn(
            "binary-behind-tree",
            DoctorCategory::Tools,
            "PATH binary lags tree",
        ),
        DoctorFinding::warn("hook-template-stale", DoctorCategory::Gate, "hooks stale"),
        DoctorFinding::info("sccache-hint", DoctorCategory::Optional, "sccache"),
    ];

    let tools: Vec<(String, ExecutableStatus)> = Vec::new();
    let report = DoctorReport {
        platform: "test",
        shell: "test",
        tools: &tools,
        path_display: "test",
        path_kind: "test",
        work_root: "test",
        state_dir: "test/.ledgerful",
        is_wsl_mounted: false,
        embedding_model_status: "OK".to_string(),
        embedding_model_failed: false,
        completion_model_status: "OK".to_string(),
        native_graph_status: "Ready".to_string(),
        active_ask_backend: "test".to_string(),
        index_health: vec!["Search index: OK (1 documents)".to_string()],
        target_triple: "test",
    };
    let counts = summarize(&findings);
    let summary = DoctorSummaryCounts {
        block: counts.block,
        warn: counts.warn,
        info: counts.info,
    };

    let mut default_buf = Vec::new();
    print_doctor_report_to(
        &mut default_buf,
        &report,
        &summary,
        &findings,
        DoctorHumanProfile {
            full: false,
            quiet: true,
        },
    )
    .expect("write default");
    let default = String::from_utf8(default_buf).expect("utf8");
    assert!(
        default.contains("3 signing finding(s) deferred (observe) — run doctor --full"),
        "{default}"
    );
    assert!(
        default.contains("1 hygiene finding(s) collapsed — run doctor --full"),
        "{default}"
    );
    assert!(!default.contains("[sig-pin]"), "{default}");
    assert!(!default.contains("[sig-version]"), "{default}");
    assert!(
        !default.contains("[PHANTOM_PROMOTED_WITHOUT_VERIFY]"),
        "{default}"
    );
    assert!(default.contains("[binary-behind-tree]"), "{default}");
    assert!(default.contains("[hook-template-stale]"), "{default}");
    assert!(!default.contains("[sccache-hint]"), "{default}");
    assert!(default.contains("warning(s)"), "{default}");

    let mut full_buf = Vec::new();
    print_doctor_report_to(
        &mut full_buf,
        &report,
        &summary,
        &findings,
        DoctorHumanProfile {
            full: true,
            quiet: true,
        },
    )
    .expect("write full");
    let full = String::from_utf8(full_buf).expect("utf8");
    assert!(full.contains("[sig-pin]"), "{full}");
    assert!(full.contains("[sig-version]"), "{full}");
    assert!(full.contains("[PHANTOM_PROMOTED_WITHOUT_VERIFY]"), "{full}");
    assert!(full.contains("[binary-behind-tree]"), "{full}");
    assert!(full.contains("[hook-template-stale]"), "{full}");
    assert!(full.contains("[sccache-hint]"), "{full}");
    assert!(!full.contains("signing finding(s) deferred"), "{full}");
    assert!(!full.contains("hygiene finding(s) collapsed"), "{full}");
}

#[test]
fn print_doctor_report_hygiene_only_trailer_byte_stable_without_later() {
    use crate::commands::doctor::{DoctorCategory, DoctorFinding, summarize};

    let findings = vec![
        DoctorFinding::warn("hook-template-stale", DoctorCategory::Gate, "hooks stale"),
        DoctorFinding::info("sccache-hint", DoctorCategory::Optional, "sccache"),
        DoctorFinding::info("tool-gemini", DoctorCategory::Optional, "gemini"),
        DoctorFinding::info("scip-rust-missing", DoctorCategory::Optional, "scip"),
    ];
    let tools: Vec<(String, ExecutableStatus)> = Vec::new();
    let report = DoctorReport {
        platform: "test",
        shell: "test",
        tools: &tools,
        path_display: "test",
        path_kind: "test",
        work_root: "test",
        state_dir: "test/.ledgerful",
        is_wsl_mounted: false,
        embedding_model_status: "OK".to_string(),
        embedding_model_failed: false,
        completion_model_status: "OK".to_string(),
        native_graph_status: "Ready".to_string(),
        active_ask_backend: "test".to_string(),
        index_health: Vec::new(),
        target_triple: "test",
    };
    let counts = summarize(&findings);
    let summary = DoctorSummaryCounts {
        block: counts.block,
        warn: counts.warn,
        info: counts.info,
    };
    let mut buf = Vec::new();
    print_doctor_report_to(
        &mut buf,
        &report,
        &summary,
        &findings,
        DoctorHumanProfile::default(),
    )
    .expect("write");
    let text = String::from_utf8(buf).expect("utf8");
    assert!(
        text.contains("3 hygiene finding(s) collapsed — run doctor --full"),
        "{text}"
    );
    assert!(text.contains("[hook-template-stale]"), "{text}");
    assert!(!text.contains("[sccache-hint]"), "{text}");
    assert!(!text.contains("[tool-gemini]"), "{text}");
    assert!(!text.contains("[scip-rust-missing]"), "{text}");
    assert!(!text.contains("signing finding(s) deferred"), "{text}");
    assert!(!text.contains("optional warning"), "{text}");
}

/// 0209-D: mixed info tool-gemini + optional warn; trailer uses warnOptional.
#[test]
fn doctor_mixed_info_tool_gemini_trailer_uses_warn_optional() {
    use crate::commands::doctor::{DoctorCategory, DoctorFinding, split_doctor_warns};

    let findings = vec![
        DoctorFinding::info(
            "tool-gemini",
            DoctorCategory::Optional,
            "gemini NOT FOUND (optional CLI; not the Cloud Ask backend)",
        ),
        DoctorFinding::warn(
            "completion-unreachable",
            DoctorCategory::Optional,
            "completion down",
        ),
        DoctorFinding::info("sccache-hint", DoctorCategory::Optional, "sccache hint"),
    ];
    let split = split_doctor_warns(&findings);
    assert_eq!(split.optional, 1, "info must not increment warnOptional");
    assert_eq!(split.action, 0);
    assert_eq!(split.total, 1);
    let hygiene = findings.len();
    let trailer = format_hygiene_collapse_trailer(hygiene, split.optional);
    assert!(trailer.contains("1 optional warning"), "{trailer}");
    assert!(!trailer.contains("3 optional"), "{trailer}");
    assert!(trailer.contains("doctor --full"), "{trailer}");
}

#[test]
fn doctor_human_profile_defaults() {
    let p = DoctorHumanProfile::default();
    assert!(!p.full);
    assert!(!p.quiet);
}

/// 0174 T6: quiet suppresses remediations; default prints them.
#[test]
fn doctor_quiet_suppresses_remediation_gate() {
    assert!(doctor_should_print_remediation(false));
    assert!(!doctor_should_print_remediation(true));
    let quiet = DoctorHumanProfile {
        full: false,
        quiet: true,
    };
    assert!(!doctor_should_print_remediation(quiet.quiet));
    // Full + quiet still suppress remediations (quiet orthogonal to full).
    let full_quiet = DoctorHumanProfile {
        full: true,
        quiet: true,
    };
    assert!(!doctor_should_print_remediation(full_quiet.quiet));
}

/// 0226 invert of 0174 hook-template collapse: Warn/Gate is visible by default.
#[test]
fn doctor_human_hook_template_stale_visible_on_default() {
    use crate::commands::doctor::{DoctorCategory, DoctorFinding, summarize};

    let findings = vec![
        DoctorFinding::warn("hook-template-stale", DoctorCategory::Gate, "hooks stale"),
        DoctorFinding::info("tool-gemini", DoctorCategory::Optional, "gemini"),
        DoctorFinding::info("scip-rust-missing", DoctorCategory::Optional, "scip"),
        DoctorFinding::info("sccache-hint", DoctorCategory::Optional, "sccache"),
        DoctorFinding::warn("impact-stale", DoctorCategory::Index, "impact stale"),
    ];
    let (index, _, hygiene) = partition_doctor_findings_for_human(&findings, false);
    let codes: Vec<&str> = index.iter().map(|f| f.code.as_str()).collect();
    assert!(codes.contains(&"hook-template-stale"));
    assert!(codes.contains(&"impact-stale"));
    assert!(!codes.contains(&"tool-gemini"));
    assert!(!codes.contains(&"scip-rust-missing"));
    assert_eq!(hygiene, 3);

    let tools: Vec<(String, ExecutableStatus)> = Vec::new();
    let report = DoctorReport {
        platform: "test",
        shell: "test",
        tools: &tools,
        path_display: "test",
        path_kind: "test",
        work_root: "test",
        state_dir: "test/.ledgerful",
        is_wsl_mounted: false,
        embedding_model_status: "OK".to_string(),
        embedding_model_failed: false,
        completion_model_status: "OK".to_string(),
        native_graph_status: "Ready".to_string(),
        active_ask_backend: "test".to_string(),
        index_health: Vec::new(),
        target_triple: "test",
    };
    let counts = summarize(&findings);
    let summary = DoctorSummaryCounts {
        block: counts.block,
        warn: counts.warn,
        info: counts.info,
    };
    let mut buf = Vec::new();
    print_doctor_report_to(
        &mut buf,
        &report,
        &summary,
        &findings,
        DoctorHumanProfile {
            full: false,
            quiet: true,
        },
    )
    .expect("write");
    let text = String::from_utf8(buf).expect("utf8");
    assert!(text.contains("[hook-template-stale]"), "{text}");
    assert!(text.contains("[impact-stale]"), "{text}");
    assert!(!text.contains("[tool-gemini]"), "{text}");
    assert!(!text.contains("[scip-rust-missing]"), "{text}");
}

/// 0226 DoD-1 / DoD-4: acked later signing omits bodies and later trailer.
#[test]
fn doctor_human_acked_signing_omits_bodies_not_later_trailer() {
    use crate::commands::doctor::{DoctorCategory, DoctorFinding, SessionPriority, summarize};

    fn acked_later(code: &str, msg: &str) -> DoctorFinding {
        let mut f = DoctorFinding::warn(code, DoctorCategory::Signing, msg);
        f.session_priority = SessionPriority::Later;
        f.acknowledged = true;
        f
    }

    let findings = vec![
        acked_later("PHANTOM_PROMOTED_WITHOUT_VERIFY", "phantoms"),
        acked_later("sig-pin", "no keys"),
        acked_later("sig-version", "v1 rows"),
        DoctorFinding::warn(
            "binary-behind-tree",
            DoctorCategory::Tools,
            "PATH binary lags tree",
        ),
    ];
    let tools: Vec<(String, ExecutableStatus)> = Vec::new();
    let report = DoctorReport {
        platform: "test",
        shell: "test",
        tools: &tools,
        path_display: "test",
        path_kind: "test",
        work_root: "test",
        state_dir: "test/.ledgerful",
        is_wsl_mounted: false,
        embedding_model_status: "OK".to_string(),
        embedding_model_failed: false,
        completion_model_status: "OK".to_string(),
        native_graph_status: "Ready".to_string(),
        active_ask_backend: "test".to_string(),
        index_health: Vec::new(),
        target_triple: "test",
    };
    let counts = summarize(&findings);
    let summary = DoctorSummaryCounts {
        block: counts.block,
        warn: counts.warn,
        info: counts.info,
    };
    let mut buf = Vec::new();
    print_doctor_report_to(
        &mut buf,
        &report,
        &summary,
        &findings,
        DoctorHumanProfile {
            full: false,
            quiet: true,
        },
    )
    .expect("write");
    let text = String::from_utf8(buf).expect("utf8");
    assert!(!text.contains("[sig-pin]"), "{text}");
    assert!(!text.contains("[sig-version]"), "{text}");
    assert!(
        !text.contains("[PHANTOM_PROMOTED_WITHOUT_VERIFY]"),
        "{text}"
    );
    assert!(text.contains("[binary-behind-tree]"), "{text}");
    assert!(
        !text.contains("signing finding(s) deferred"),
        "acked codes must not inflate later trailer: {text}"
    );
    let json = serde_json::to_value(&findings).expect("json");
    for code in ["sig-pin", "sig-version", "PHANTOM_PROMOTED_WITHOUT_VERIFY"] {
        let row = json
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["code"] == code)
            .unwrap_or_else(|| panic!("{code}"));
        assert_eq!(row["acknowledged"], true);
        assert_eq!(row["sessionPriority"], "later");
    }
}

#[test]
fn dead_code_honesty_strings_present() {
    assert!(DEAD_CODE_HONESTY_FOOTER.contains("Heuristic evidence"));
    assert!(DEAD_CODE_HONESTY_FOOTER.contains("not proof of dead code"));
    assert!(DEAD_CODE_EMPTY_STATE.contains("heuristic analysis"));
    assert!(!DEAD_CODE_EMPTY_STATE.contains("No dead code found"));
}

#[test]
fn wsl_support_line_mounted_and_unmounted() {
    assert_eq!(
        wsl_support_line(true),
        Some("WSL Support:         Active (Mounted)")
    );
    assert_eq!(wsl_support_line(false), None);
}
