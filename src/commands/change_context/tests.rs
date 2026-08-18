use super::build::*;
use super::packet::*;
use super::storage::*;
use crate::config::model::Config;
use crate::impact::packet::{
    BlastRadius, ChangedFile, FileAnalysisStatus, ImpactPacket, RiskLevel, TemporalCoupling,
};
use crate::ledger::{Category, TransactionManager, TransactionRequest};
use crate::state::layout::Layout;
use crate::state::reports::{LATEST_IMPACT_REPORT, write_impact_report};
use crate::state::storage::StorageManager;
use std::fs;
use std::path::PathBuf;
use tempfile::tempdir;

fn make_changed(path: &str) -> ChangedFile {
    ChangedFile {
        path: PathBuf::from(path),
        status: "Modified".to_string(),
        old_path: None,
        is_staged: false,
        symbols: None,
        imports: None,
        runtime_usage: None,
        analysis_status: FileAnalysisStatus::default(),
        analysis_warnings: Vec::new(),
        ..Default::default()
    }
}

fn base_packet(changes: Vec<ChangedFile>) -> ImpactPacket {
    let mut p = ImpactPacket::default();
    p.schema_version = "v1".to_string();
    p.risk_level = RiskLevel::Low;
    p.changes = changes;
    p.tree_clean = p.changes.is_empty();
    p.head_hash = Some("abc123".to_string());
    p
}

#[test]
fn read_set_priority_changed_then_blast_then_temporal() {
    let mut impact = base_packet(vec![make_changed("src/a.rs")]);
    impact.blast_radius = Some(BlastRadius {
        depth_requested: 1,
        depth_applied: 1,
        edges: Vec::new(),
        must_touch_files: vec!["src/b.rs".to_string(), "src/a.rs".to_string()],
        must_touch_symbols: vec!["foo".to_string()],
        test_hints: Vec::new(),
        honesty_notes: Vec::new(),
        ..Default::default()
    });
    impact.temporal_couplings = vec![TemporalCoupling {
        file_a: PathBuf::from("src/a.rs"),
        file_b: PathBuf::from("src/c.rs"),
        score: 0.9,
    }];

    let (set, capped, total) = build_read_set(&impact, 20, 0.75, "code");
    assert!(!capped);
    assert_eq!(total, 3);
    assert_eq!(set.len(), 3);
    assert_eq!(set[0].path, "src/a.rs");
    assert_eq!(set[0].reason, "changed");
    assert_eq!(set[1].path, "src/b.rs");
    assert_eq!(set[1].reason, "blast");
    assert_eq!(set[2].path, "src/c.rs");
    assert_eq!(set[2].reason, "temporal");
}

#[test]
fn read_set_demotes_governance_temporal_under_code_mode() {
    let mut impact = base_packet(vec![make_changed("src/a.rs")]);
    impact.temporal_couplings = vec![
        TemporalCoupling {
            file_a: PathBuf::from("src/a.rs"),
            file_b: PathBuf::from("src/code_partner.rs"),
            score: 0.9,
        },
        TemporalCoupling {
            file_a: PathBuf::from("src/a.rs"),
            file_b: PathBuf::from("conductor/spec.md"),
            score: 0.95,
        },
        TemporalCoupling {
            file_a: PathBuf::from("deferred.md"),
            file_b: PathBuf::from("conductor.md"),
            score: 0.99,
        },
    ];
    let (set, _, total) = build_read_set(&impact, 20, 0.75, "code");
    let paths: Vec<&str> = set.iter().map(|e| e.path.as_str()).collect();
    assert!(paths.contains(&"src/a.rs"));
    assert!(paths.contains(&"src/code_partner.rs"));
    assert!(
        !paths.contains(&"conductor/spec.md"),
        "code↔governance demoted from p3: {paths:?}"
    );
    assert!(
        !paths.contains(&"deferred.md") && !paths.contains(&"conductor.md"),
        "gov↔gov demoted: {paths:?}"
    );
    assert_eq!(total, 2); // changed + code partner only

    let (set_all, _, total_all) = build_read_set(&impact, 20, 0.75, "all");
    assert!(total_all > total);
    assert!(
        set_all
            .iter()
            .any(|e| e.path == "conductor/spec.md" || e.path == "deferred.md")
    );
}

#[test]
fn read_set_max_files_sets_capped_flags() {
    let impact = base_packet(vec![
        make_changed("src/a.rs"),
        make_changed("src/b.rs"),
        make_changed("src/c.rs"),
    ]);
    let (set, capped, total) = build_read_set(&impact, 1, 0.75, "code");
    assert!(capped);
    assert_eq!(total, 3);
    assert_eq!(set.len(), 1);
    assert_eq!(set[0].reason, "changed");
}

#[test]
fn next_actions_missing_doctor_does_not_claim_block_findings() {
    let doctor = DoctorSection {
        status: "missing".to_string(),
        ready_for_publish: false,
        block: 0,
        warn: 0,
        info: 0,
        top_findings: Vec::new(),
    };
    let ledger = LedgerSection {
        pending_count: 0,
        active_tx: Vec::new(),
    };
    let actions = compose_next_actions("empty", &doctor, &ledger, false, false, None);
    assert!(
        actions.iter().any(|a| a.contains("ledgerful doctor")),
        "missing doctor should suggest refresh: {actions:?}"
    );
    assert!(
        actions
            .iter()
            .all(|a| !a.contains("resolve doctor block findings")),
        "missing sidecar must not claim block findings: {actions:?}"
    );
}

#[test]
fn next_actions_block_count_suggests_resolve() {
    let doctor = DoctorSection {
        status: "ok".to_string(),
        ready_for_publish: false,
        block: 2,
        warn: 0,
        info: 0,
        top_findings: Vec::new(),
    };
    let ledger = LedgerSection {
        pending_count: 0,
        active_tx: Vec::new(),
    };
    let actions = compose_next_actions("ready", &doctor, &ledger, false, true, None);
    assert!(
        actions
            .iter()
            .any(|a| a.contains("resolve doctor block findings")),
        "block>0 must surface resolve action: {actions:?}"
    );
}

#[test]
fn doctor_missing_when_sidecar_absent() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let section = read_doctor_section(&layout);
    assert_eq!(section.status, "missing");
    assert!(!section.ready_for_publish);
}

#[test]
fn doctor_ok_from_sidecar() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let path = layout.state_subdir().join("doctor-results.json");
    fs::write(
        path.as_std_path(),
        r#"{
                "failures": 0,
                "timestamp": "2099-01-01T00:00:00+00:00",
                "readyForPublish": true,
                "block": 0,
                "warn": 1,
                "info": 2
            }"#,
    )
    .unwrap();
    let section = read_doctor_section(&layout);
    assert_eq!(section.status, "ok");
    assert!(section.ready_for_publish);
    assert_eq!(section.warn, 1);
    assert_eq!(section.info, 2);
    // Pre-0129 sidecars without findings → empty topFindings (tolerant).
    assert!(section.top_findings.is_empty());
}

/// Reader tolerates pre-0138 sidecars that still include optional-category
/// codes (e.g. `completion-unreachable`) until the next doctor write.
/// This is **stale-sidecar tolerance**, not writer policy — post-0138
/// `write_doctor_results` excludes optional warns via `is_action_critical`.
/// Parse still forwards sidecar findings as-is (no read-time category filter).
#[test]
fn parse_doctor_sidecar_tolerates_stale_optional_finding() {
    let contents = r#"{
            "failures": 1,
            "timestamp": "2099-01-01T00:00:00+00:00",
            "readyForPublish": true,
            "block": 0,
            "warn": 2,
            "info": 0,
            "findings": [
                {
                    "code": "sig-pin",
                    "severity": "warn",
                    "message": "pin missing",
                    "remediation": "ledgerful config set intent.trusted_public_keys '[\"abc\"]'"
                },
                {
                    "code": "completion-unreachable",
                    "severity": "warn",
                    "message": "optional down"
                }
            ]
        }"#;
    let section = parse_doctor_sidecar(contents, std::path::Path::new("doctor-results.json"));
    assert_eq!(section.status, "ok");
    assert_eq!(section.top_findings.len(), 2);
    assert_eq!(section.top_findings[0].code, "sig-pin");
    assert_eq!(
        section.top_findings[0].remediation.as_deref(),
        Some("ledgerful config set intent.trusted_public_keys '[\"abc\"]'")
    );
    // Stale optional code accepted as-is (not re-filtered at read time).
    assert_eq!(section.top_findings[1].code, "completion-unreachable");
    assert!(
        section.top_findings[1].remediation.is_none(),
        "must not invent remediation when key absent"
    );
    // serde: remediation None skips null
    let ser = serde_json::to_value(&section.top_findings[1]).unwrap();
    assert!(
        ser.get("remediation").is_none(),
        "skip_serializing_if must omit remediation: {ser}"
    );
}

#[test]
fn parse_doctor_sidecar_no_findings_key_tolerant_empty() {
    let contents = r#"{
            "failures": 0,
            "timestamp": "2099-01-01T00:00:00+00:00",
            "readyForPublish": true,
            "block": 0,
            "warn": 3,
            "info": 1
        }"#;
    let section = parse_doctor_sidecar(contents, std::path::Path::new("doctor-results.json"));
    assert_eq!(section.status, "ok");
    assert_eq!(section.warn, 3);
    assert!(
        section.top_findings.is_empty(),
        "missing findings key → empty topFindings"
    );
}

#[test]
fn doctor_stale_from_old_timestamp() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let path = layout.state_subdir().join("doctor-results.json");
    fs::write(
        path.as_std_path(),
        r#"{
                "failures": 0,
                "timestamp": "2000-01-01T00:00:00+00:00",
                "readyForPublish": true,
                "block": 0,
                "warn": 0,
                "info": 0
            }"#,
    )
    .unwrap();
    let section = read_doctor_section(&layout);
    assert_eq!(section.status, "stale");
}

#[test]
fn doctor_incomplete_sidecar_is_error_not_false_green() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let path = layout.state_subdir().join("doctor-results.json");
    // Timestamp alone must not default to readyForPublish=true.
    fs::write(
        path.as_std_path(),
        r#"{"timestamp": "2099-01-01T00:00:00+00:00"}"#,
    )
    .unwrap();
    let section = read_doctor_section(&layout);
    assert_eq!(section.status, "error");
    assert!(!section.ready_for_publish);
    assert_eq!(section.block, 0);
}

#[test]
fn packet_schema_version_is_one() {
    let p = not_ready_packet(
        "test".into(),
        None,
        DoctorSection {
            status: "missing".into(),
            ready_for_publish: false,
            block: 0,
            warn: 0,
            info: 0,
            top_findings: Vec::new(),
        },
        LedgerSection {
            pending_count: 0,
            active_tx: Vec::new(),
        },
        NotReadyErrorClass::Other,
    );
    assert_eq!(p.schema_version, 1);
    assert_eq!(p.status, "not_ready");
    assert!(p.reason.is_some());
    assert!(!p.next_actions.is_empty());
}

fn empty_doctor_ledger() -> (DoctorSection, LedgerSection) {
    (
        DoctorSection {
            status: "missing".into(),
            ready_for_publish: false,
            block: 0,
            warn: 0,
            info: 0,
            top_findings: Vec::new(),
        },
        LedgerSection {
            pending_count: 0,
            active_tx: Vec::new(),
        },
    )
}

fn next_actions_joined(class: NotReadyErrorClass) -> String {
    next_actions_for_class(class)
        .join("\n")
        .to_ascii_lowercase()
}

#[test]
fn ro_permission_next_actions_exclude_class_c_triad() {
    let (doctor, ledger) = empty_doctor_ledger();
    let p = not_ready_packet(
        "storage unavailable: state directory not writable: permission denied".into(),
        None,
        doctor,
        ledger,
        NotReadyErrorClass::PermissionDenied,
    );
    assert_eq!(p.schema_version, 1);
    assert_eq!(p.status, "not_ready");
    let joined = p.next_actions.join("\n").to_ascii_lowercase();
    assert!(
        !joined.contains("doctor --json"),
        "RO class must not suggest doctor: {:?}",
        p.next_actions
    );
    assert!(
        !joined.contains("ledgerful init"),
        "RO class must not suggest init: {:?}",
        p.next_actions
    );
    // Ban bare index recovery (Class C). "index" alone may appear in prose — check command shape.
    assert!(
        !joined.contains("ledgerful index"),
        "RO class must not suggest index: {:?}",
        p.next_actions
    );
    assert!(
        joined.contains("ledgerful_state_dir") || joined.contains("populated"),
        "expected STATE_DIR / populated guidance: {:?}",
        p.next_actions
    );
    assert!(
        joined.contains("workspace-write") || joined.contains("git-only"),
        "expected workspace-write or git-only: {:?}",
        p.next_actions
    );
    assert!(
        p.reason
            .as_ref()
            .is_some_and(|r| r.contains("storage unavailable:")
                || r.contains("state directory not writable")),
        "greppable reason fragment missing: {:?}",
        p.reason
    );
}

#[test]
fn missing_db_next_actions_distinct_from_ro_class() {
    let (doctor, ledger) = empty_doctor_ledger();
    let p = not_ready_packet(
        "storage unavailable: Storage not initialized".into(),
        None,
        doctor,
        ledger,
        NotReadyErrorClass::MissingDb,
    );
    let joined = p.next_actions.join("\n").to_ascii_lowercase();
    assert!(
        joined.contains("init") || joined.contains("index") || joined.contains("scan"),
        "MissingDb may name writable-env init/scan/index: {:?}",
        p.next_actions
    );
    let ro = next_actions_joined(NotReadyErrorClass::PermissionDenied);
    assert_ne!(
        p.next_actions,
        next_actions_for_class(NotReadyErrorClass::PermissionDenied),
        "MissingDb nextActions must differ from RO class"
    );
    assert!(!ro.contains("ledgerful index") || joined.contains("writable"));
}

#[test]
fn classify_storage_error_permission_and_missing() {
    let perm = miette::miette!("unable to open database file: Access is denied. (os error 5)");
    assert_eq!(
        classify_storage_error(&perm, true),
        NotReadyErrorClass::PermissionDenied
    );
    let missing = miette::miette!(
        "Storage not initialized at /tmp/x/state/ledger.db. Run a write command first."
    );
    assert_eq!(
        classify_storage_error(&missing, false),
        NotReadyErrorClass::MissingDb
    );
    let schema = miette::miette!("schema is not current; migration required");
    assert_eq!(
        classify_storage_error(&schema, true),
        NotReadyErrorClass::SchemaStale
    );
    // Codex R2: pure-RO open can fail during schema probe PRAGMA with
    // "unable to open database file" while still embedding user_version —
    // must NOT classify as SchemaStale (migration advice).
    let pragma_open = miette::miette!(
        "PRAGMA user_version: unable to open database file: Access is denied. (os error 5)"
    );
    assert_eq!(
        classify_storage_error(&pragma_open, true),
        NotReadyErrorClass::PermissionDenied
    );
}

#[test]
fn soft_open_existing_db_builds_valid_packet() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let dir = tmp.path();

    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "t@t.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "T"])
        .current_dir(dir)
        .output()
        .unwrap();
    fs::write(dir.join("README.md"), "hi").unwrap();
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir)
        .output()
        .unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    // Write-mode create + migrate once, then soft-open RO path.
    let write =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let _ = write.shutdown();

    let storage = open_storage_for_change_context(&layout).expect("soft-open RO");
    assert!(
        storage.is_read_only,
        "existing ledger.db should soft-open read-only"
    );
    let config = Config::default();
    let opts = ChangeContextOpts::default();
    let packet = build_change_context(&opts, &layout, &storage, &config).unwrap();
    assert_eq!(packet.schema_version, 1);
    assert!(
        packet.status == "empty" || packet.status == "ready",
        "unexpected status: {}",
        packet.status
    );
    let _ = storage.shutdown();
}

#[test]
fn build_empty_clean_tree() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();

    // git repo + clean commit
    let dir = tmp.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "t@t.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "T"])
        .current_dir(dir)
        .output()
        .unwrap();
    fs::write(dir.join("README.md"), "hi").unwrap();
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir)
        .output()
        .unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let config = Config::default();
    let opts = ChangeContextOpts::default();
    // project_root comes from layout.root — no cwd mutation required.
    let packet = build_change_context(&opts, &layout, &storage, &config).unwrap();

    assert_eq!(packet.status, "empty");
    assert_eq!(packet.schema_version, 1);
    assert_eq!(packet.ledger.pending_count, 0);
    assert_eq!(packet.doctor.status, "missing");
    assert!(packet.read_set.is_empty());
    // No fake high risk on empty
    if let Some(ref r) = packet.risk_level {
        assert_ne!(r, "high");
    }
    // 0173: agentSummary present on empty (coexists with summary)
    let agent = packet
        .agent_summary
        .as_ref()
        .expect("agentSummary on empty status");
    assert_eq!(agent.path_mode, "code");
    assert_eq!(agent.analysis_mode, "working_tree");
    assert_eq!(agent.changed.total, 0);
    assert!(!packet.summary.is_empty());
    let _ = storage.shutdown();
}

#[test]
fn build_clean_tree_with_pending_ledger_is_ready() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let dir = tmp.path();

    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "t@t.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "T"])
        .current_dir(dir)
        .output()
        .unwrap();
    fs::write(dir.join("README.md"), "hi").unwrap();
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir)
        .output()
        .unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let mut storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let config = Config::default();
    {
        let mut mgr = TransactionManager::new(&mut storage, dir.to_path_buf(), config.clone());
        mgr.start_change(TransactionRequest {
            category: Category::Feature,
            entity: "config".to_string(),
            ..Default::default()
        })
        .unwrap();
    }

    let opts = ChangeContextOpts::default();
    let packet = build_change_context(&opts, &layout, &storage, &config).unwrap();

    assert_eq!(packet.status, "ready");
    assert!(packet.ledger.pending_count >= 1);
    assert!(packet.summary.to_lowercase().contains("pending"));
    assert!(packet.summary.to_lowercase().contains("no file"));
    let _ = storage.shutdown();
}

#[test]
fn build_one_changed_file_ready_with_read_set() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let dir = tmp.path();

    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "t@t.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "T"])
        .current_dir(dir)
        .output()
        .unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir)
        .output()
        .unwrap();
    fs::write(dir.join("src/lib.rs"), "pub fn a() {}\npub fn b() {}\n").unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let config = Config::default();
    let opts = ChangeContextOpts::default();
    let packet = build_change_context(&opts, &layout, &storage, &config).unwrap();

    assert_eq!(packet.status, "ready");
    assert!(!packet.read_set.is_empty());
    assert!(
        packet
            .read_set
            .iter()
            .any(|e| e.path.contains("lib.rs") && e.reason == "changed"),
        "readSet should include changed lib.rs: {:?}",
        packet.read_set
    );
    assert!(packet.risk_level.is_some());
    assert!(packet.doctor.status == "missing" || packet.doctor.status == "ok");
    // 0173: agentSummary present on ready with class counts
    let agent = packet
        .agent_summary
        .as_ref()
        .expect("agentSummary on ready status");
    assert_eq!(agent.path_mode, "code");
    assert_eq!(agent.analysis_mode, "working_tree");
    assert!(agent.changed.total >= 1);
    assert!(!agent.risk_one_liner.is_empty());
    let _ = storage.shutdown();
}

#[test]
fn prospective_paths_produce_non_empty_analysis_mode() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let dir = tmp.path();

    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "t@t.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "T"])
        .current_dir(dir)
        .output()
        .unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/exists.rs"), "pub fn x() {}\n").unwrap();
    fs::write(dir.join("README.md"), "hi").unwrap();
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir)
        .output()
        .unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let config = Config::default();
    let opts = ChangeContextOpts {
        paths: vec!["src/exists.rs".into(), "src/missing.rs".into()],
        ..ChangeContextOpts::default()
    };
    let packet = build_change_context(&opts, &layout, &storage, &config).unwrap();
    assert_eq!(packet.status, "ready");
    let agent = packet.agent_summary.as_ref().expect("agentSummary");
    assert_eq!(agent.analysis_mode, "prospective");
    assert_eq!(agent.path_mode, "code");
    assert!(agent.changed.total >= 1);
    assert!(
        packet
            .read_set
            .iter()
            .any(|e| e.path.contains("exists.rs") || e.path.contains("missing.rs")),
        "prospective readSet: {:?}",
        packet.read_set
    );
    let _ = storage.shutdown();
}

#[test]
fn change_context_does_not_rewrite_latest_impact() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let dir = tmp.path();

    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "t@t.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "T"])
        .current_dir(dir)
        .output()
        .unwrap();
    fs::write(dir.join("README.md"), "hi").unwrap();
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir)
        .output()
        .unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();

    // Pre-seed latest-impact.json with a distinctive marker.
    let seed = ImpactPacket {
        schema_version: "v1".to_string(),
        head_hash: Some("SEED_MARKER_0114".to_string()),
        risk_reasons: vec!["seed-reason-do-not-clobber".to_string()],
        ..Default::default()
    };
    write_impact_report(&layout, &seed).unwrap();

    let report_path = layout.reports_dir().join(LATEST_IMPACT_REPORT);
    let before = fs::read_to_string(report_path.as_std_path()).unwrap();
    assert!(before.contains("SEED_MARKER_0114"));

    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let config = Config::default();
    let opts = ChangeContextOpts::default();
    let _packet = build_change_context(&opts, &layout, &storage, &config).unwrap();

    let after = fs::read_to_string(report_path.as_std_path()).unwrap();
    assert_eq!(
        before, after,
        "change-context must not rewrite latest-impact.json"
    );

    // Prospective --paths must also leave the durable report untouched (0173-G).
    let opts_paths = ChangeContextOpts {
        paths: vec!["src/missing.rs".into()],
        ..ChangeContextOpts::default()
    };
    let packet = build_change_context(&opts_paths, &layout, &storage, &config).unwrap();
    assert_eq!(
        packet
            .agent_summary
            .as_ref()
            .map(|a| a.analysis_mode.as_str()),
        Some("prospective")
    );
    let after_paths = fs::read_to_string(report_path.as_std_path()).unwrap();
    assert_eq!(
        before, after_paths,
        "prospective change-context must not rewrite latest-impact.json"
    );
    let _ = storage.shutdown();
}

#[test]
fn paths_and_base_ref_are_mutually_exclusive() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let dir = tmp.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "t@t.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "T"])
        .current_dir(dir)
        .output()
        .unwrap();
    fs::write(dir.join("README.md"), "hi").unwrap();
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir)
        .output()
        .unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let config = Config::default();
    let opts = ChangeContextOpts {
        paths: vec!["src/a.rs".into()],
        base_ref: Some("HEAD".into()),
        ..ChangeContextOpts::default()
    };
    let err = build_change_context(&opts, &layout, &storage, &config).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("mutually exclusive"),
        "expected mutual exclusion error, got: {msg}"
    );
    let _ = storage.shutdown();
}

#[test]
fn json_roundtrip_camel_case_keys() {
    let p = not_ready_packet(
        "x".into(),
        Some("HEAD~1".into()),
        DoctorSection {
            status: "missing".into(),
            ready_for_publish: false,
            block: 0,
            warn: 0,
            info: 0,
            top_findings: Vec::new(),
        },
        LedgerSection {
            pending_count: 0,
            active_tx: Vec::new(),
        },
        NotReadyErrorClass::Other,
    );
    let s = serde_json::to_string(&p).unwrap();
    assert!(s.contains("\"schemaVersion\":1"));
    assert!(s.contains("\"readSetCapped\""));
    assert!(s.contains("\"readSetTotalCandidates\""));
    assert!(s.contains("\"pendingCount\""));
    assert!(s.contains("\"readyForPublish\""));
    let back: ChangeContextPacket = serde_json::from_str(&s).unwrap();
    assert_eq!(back.schema_version, 1);
}

#[test]
fn detail_parse() {
    assert_eq!(
        ChangeContextDetail::parse("minimal").unwrap(),
        ChangeContextDetail::Minimal
    );
    assert_eq!(
        ChangeContextDetail::parse("standard").unwrap(),
        ChangeContextDetail::Standard
    );
    assert!(ChangeContextDetail::parse("deep").is_err());
}

#[test]
fn test_coverage_never_bare_empty_or_track_0115_handoff() {
    use crate::impact::enrichment::test_gaps::TestGapsStatus;
    use crate::state::migrations::get_migrations;

    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let dir = tmp.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "t@t.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "T"])
        .current_dir(dir)
        .output()
        .unwrap();
    fs::write(dir.join("README.md"), "hi").unwrap();
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir)
        .output()
        .unwrap();

    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    get_migrations().to_latest(&mut conn).unwrap();
    // Use real layout storage so build_change_context works end-to-end.
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();
    let config = Config::default();
    let opts = ChangeContextOpts::default();
    let packet = build_change_context(&opts, &layout, &storage, &config).unwrap();

    let cov = packet.test_coverage.as_ref().expect("testCoverage present");
    // Never bare "empty"
    assert_ne!(cov.status.as_str(), "empty");
    assert!(matches!(
        cov.status,
        TestGapsStatus::Available
            | TestGapsStatus::EmptyMapping
            | TestGapsStatus::MissingTable
            | TestGapsStatus::NoSourceSeeds
            | TestGapsStatus::Unavailable
    ));

    let json = serde_json::to_string(&packet).unwrap();
    assert!(
        !json.contains("track 0115"),
        "handoff string must be gone: {json}"
    );
    assert!(
        !json.contains("see ledgerful tests"),
        "handoff string must be gone: {json}"
    );
    // Guard testCoverage.status specifically (top-level packet status may be "empty").
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let tc_status = v["testCoverage"]["status"].as_str().unwrap_or("");
    assert_ne!(tc_status, "empty", "testCoverage.status bare empty: {json}");
    assert!(
        matches!(
            tc_status,
            "available" | "empty_mapping" | "missing_table" | "no_source_seeds" | "unavailable"
        ),
        "unexpected testCoverage.status={tc_status}: {json}"
    );
    // Structural + LCOV ceiling always present
    assert!(json.contains("Structural test_mapping"));
    assert!(json.contains("LCOV COVERAGE"));
    let _ = storage.shutdown();
}

#[test]
fn summarize_test_coverage_uses_impact_attached_gaps() {
    use crate::impact::enrichment::test_gaps::{TestGapsReport, TestGapsStatus};

    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();

    let impact = ImpactPacket {
        test_gaps: Some(TestGapsReport {
            status: TestGapsStatus::Available,
            source_seed_count: 3,
            mapped_count: 1,
            file_mapped_count: 1,
            unmapped_count: 1,
            unmapped_capped: false,
            unmapped_total: 1,
            unmapped: vec![],
            mapped_sample: vec![],
            notes: vec!["note".into()],
        }),
        ..ImpactPacket::default()
    };
    let summary = summarize_test_coverage(&storage, &impact);
    assert_eq!(summary.status, TestGapsStatus::Available);
    assert_eq!(summary.source_seed_count, 3);
    assert_eq!(summary.mapped_count, 1);
    assert_eq!(summary.unmapped_count, 1);
    let _ = storage.shutdown();
}

#[test]
fn change_context_blast_confidence_summary_counts_only_no_edges() {
    use crate::impact::enrichment::edge_confidence::EdgeConfidenceSummary;

    // Unit-level packet shape: BlastSummary carries counts, never edges.
    let summary = BlastSummary {
        depth: 1,
        must_touch_file_count: 2,
        must_touch_symbol_count: 1,
        confidence_summary: EdgeConfidenceSummary {
            scip_bound: 3,
            resolved: 5,
            ambiguous: 0,
            unresolved: 0,
            capped: 0,
            unknown: 0,
            expandable: 7,
            total: 8,
        },
    };
    let v = serde_json::to_value(&summary).unwrap();
    assert!(
        v.get("edges").is_none(),
        "change-context must not dump edges"
    );
    assert_eq!(v["confidenceSummary"]["scipBound"], 3);
    assert_eq!(v["confidenceSummary"]["resolved"], 5);
    assert_eq!(v["confidenceSummary"]["total"], 8);
    assert_eq!(v["depth"], 1);

    // Both detail levels use the same BlastSummary shape (no detail gate on counts).
    for detail in [ChangeContextDetail::Minimal, ChangeContextDetail::Standard] {
        assert!(
            matches!(
                detail,
                ChangeContextDetail::Minimal | ChangeContextDetail::Standard
            ),
            "detail levels are minimal|standard only"
        );
    }
}

fn make_flow_entry(i: usize) -> crate::impact::enrichment::affected_flows::AffectedFlowEntry {
    use crate::impact::enrichment::affected_flows::{AffectedFlowEntry, MatchKind};
    AffectedFlowEntry {
        method: "GET".into(),
        path_pattern: format!("/p{i:02}"),
        handler_symbol_name: Some(format!("h{i:02}")),
        handler_file: Some(format!("src/h{i:02}.rs")),
        framework: "Axum".into(),
        match_kind: MatchKind::RouteFile,
        route_confidence: Some(1.0),
        confidence_class: None,
        evidence: None,
    }
}

fn make_available_flows(
    n: usize,
) -> crate::impact::enrichment::affected_flows::AffectedFlowsReport {
    use crate::impact::enrichment::affected_flows::{
        AffectedFlowsReport, AffectedFlowsStatus, HONESTY_NOTE,
    };
    let flows: Vec<_> = (0..n).map(make_flow_entry).collect();
    AffectedFlowsReport {
        status: AffectedFlowsStatus::Available,
        flow_count: n,
        flow_capped: false,
        flow_total: n,
        flows,
        notes: vec![HONESTY_NOTE.into()],
    }
}

#[test]
fn summarize_affected_flows_uses_impact_attached_report() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();

    let impact = ImpactPacket {
        affected_flows: Some(make_available_flows(3)),
        ..ImpactPacket::default()
    };
    let summary = summarize_affected_flows(&storage, &impact, ChangeContextDetail::Minimal);
    assert_eq!(
        summary.status,
        crate::impact::enrichment::affected_flows::AffectedFlowsStatus::Available
    );
    assert_eq!(summary.flow_count, 3);
    assert_eq!(summary.flows.len(), 3);
    let _ = storage.shutdown();
}

#[test]
fn summarize_affected_flows_sample_caps_by_detail() {
    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();

    let impact = ImpactPacket {
        affected_flows: Some(make_available_flows(15)),
        ..ImpactPacket::default()
    };

    let minimal = summarize_affected_flows(&storage, &impact, ChangeContextDetail::Minimal);
    assert_eq!(minimal.flows.len(), AFFECTED_FLOWS_SAMPLE_MINIMAL);
    // Counts pass through full report — not the sample size.
    assert_eq!(minimal.flow_count, 15);
    assert_eq!(minimal.flow_total, 15);

    let standard = summarize_affected_flows(&storage, &impact, ChangeContextDetail::Standard);
    assert_eq!(standard.flows.len(), AFFECTED_FLOWS_SAMPLE_STANDARD);
    assert_eq!(standard.flow_count, 15);

    // No huge arrays on either detail.
    assert!(minimal.flows.len() <= 5);
    assert!(standard.flows.len() <= 10);
    let _ = storage.shutdown();
}

#[test]
fn summarize_affected_flows_status_passthrough_and_available_zero() {
    use crate::impact::enrichment::affected_flows::{
        AffectedFlowsReport, AffectedFlowsStatus, HONESTY_NOTE,
    };

    let tmp = tempdir().unwrap();
    let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
    let layout = Layout::new(root);
    layout.ensure_state_dir().unwrap();
    let storage =
        StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path()).unwrap();

    // Status passthrough (empty_map).
    let impact_empty = ImpactPacket {
        affected_flows: Some(AffectedFlowsReport {
            status: AffectedFlowsStatus::EmptyMap,
            flow_count: 0,
            flow_capped: false,
            flow_total: 0,
            flows: vec![],
            notes: vec![HONESTY_NOTE.into()],
        }),
        ..ImpactPacket::default()
    };
    let empty = summarize_affected_flows(&storage, &impact_empty, ChangeContextDetail::Standard);
    assert_eq!(empty.status, AffectedFlowsStatus::EmptyMap);
    assert_eq!(empty.flow_count, 0);
    assert!(empty.flows.is_empty());

    // available + 0 = all-clear (no registered routes touched).
    let impact_clear = ImpactPacket {
        affected_flows: Some(AffectedFlowsReport {
            status: AffectedFlowsStatus::Available,
            flow_count: 0,
            flow_capped: false,
            flow_total: 0,
            flows: vec![],
            notes: vec![HONESTY_NOTE.into()],
        }),
        ..ImpactPacket::default()
    };
    let clear = summarize_affected_flows(&storage, &impact_clear, ChangeContextDetail::Minimal);
    assert_eq!(clear.status, AffectedFlowsStatus::Available);
    assert_eq!(clear.flow_count, 0);
    assert!(clear.flows.is_empty());

    let json = serde_json::to_value(&clear).unwrap();
    assert_eq!(json["status"], "available");
    assert_eq!(json["flowCount"], 0);
    assert!(json["flows"].as_array().unwrap().is_empty());
    let _ = storage.shutdown();
}

#[test]
fn change_context_schema_version_stays_one_with_affected_flows_key() {
    use crate::impact::enrichment::affected_flows::HONESTY_NOTE;

    let packet = ChangeContextPacket {
        schema_version: CHANGE_CONTEXT_SCHEMA_VERSION,
        status: "ready".into(),
        summary: "test".into(),
        agent_summary: None,
        reason: None,
        head_hash: Some("abc".into()),
        base_ref: None,
        risk_level: Some("low".into()),
        risk_reasons: vec![],
        read_set: vec![],
        read_set_capped: false,
        read_set_total_candidates: 0,
        blast: None,
        test_coverage: None,
        affected_flows: Some(make_available_flows(1)),
        change_hints: None,
        doctor: DoctorSection {
            status: "ok".into(),
            ready_for_publish: true,
            block: 0,
            warn: 0,
            info: 0,
            top_findings: vec![],
        },
        ledger: LedgerSection {
            pending_count: 0,
            active_tx: vec![],
        },
        analysis_warnings: vec![],
        next_actions: vec![],
        impact_schema_version: Some("v1".into()),
    };
    let v = serde_json::to_value(&packet).unwrap();
    assert_eq!(v["schemaVersion"], 1);
    assert!(v.get("affectedFlows").is_some());
    assert_eq!(v["affectedFlows"]["status"], "available");
    assert_eq!(v["affectedFlows"]["flowCount"], 1);
    assert_eq!(v["affectedFlows"]["flows"][0]["method"], "GET");
    assert_eq!(v["affectedFlows"]["flows"][0]["pathPattern"], "/p00");
    let notes = v["affectedFlows"]["notes"].as_array().unwrap();
    assert!(notes.iter().any(|n| n.as_str() == Some(HONESTY_NOTE)));
}

fn make_added(path: &str) -> ChangedFile {
    ChangedFile {
        path: PathBuf::from(path),
        status: "Added".to_string(),
        old_path: None,
        is_staged: false,
        symbols: None,
        imports: None,
        runtime_usage: None,
        analysis_status: FileAnalysisStatus::default(),
        analysis_warnings: Vec::new(),
        ..Default::default()
    }
}

#[test]
fn change_hints_greenfield_on_pure_add_fixture() {
    use crate::impact::enrichment::change_hints::{
        ChangeHintsKind, ChangeHintsOpts, compute_change_hints,
    };

    let files = vec![
        make_added("src/newpkg/mod.rs"),
        make_added("src/newpkg/cli.rs"),
        make_added("src/main.rs"),
    ];
    let impact = base_packet(files);
    let hints = compute_change_hints(
        &impact.changes,
        &ChangeHintsOpts {
            project_root: None,
            mapped_hint_paths: mapped_paths_from_impact(&impact),
        },
    );
    assert_eq!(hints.kind, ChangeHintsKind::Greenfield);
    assert!(
        !hints.suggested_tests.is_empty() || !hints.notes.is_empty(),
        "suggestions or notes required: {hints:?}"
    );

    let doctor = DoctorSection {
        status: "ok".into(),
        ready_for_publish: true,
        block: 0,
        warn: 0,
        info: 0,
        top_findings: vec![],
    };
    let ledger = LedgerSection {
        pending_count: 0,
        active_tx: vec![],
    };
    let summary = compose_summary("ready", true, &impact, &ledger, &doctor, Some(&hints));
    assert!(
        summary.contains("greenfield-ish"),
        "summary must mention greenfield: {summary}"
    );

    let actions = compose_next_actions("ready", &doctor, &ledger, false, true, Some(&hints));
    if !hints.suggested_tests.is_empty() {
        assert!(
            actions
                .iter()
                .any(|a| a == GREENFIELD_SUGGESTED_TESTS_ACTION),
            "must include greppable suggestedTests action: {actions:?}"
        );
    }
    assert!(
        actions.iter().any(|a| a.contains("verify --scope fast")),
        "must keep verify: {actions:?}"
    );
}

#[test]
fn change_hints_omitted_on_not_ready_and_empty_changes() {
    let p = not_ready_packet(
        "x".into(),
        None,
        DoctorSection {
            status: "missing".into(),
            ready_for_publish: false,
            block: 0,
            warn: 0,
            info: 0,
            top_findings: Vec::new(),
        },
        LedgerSection {
            pending_count: 0,
            active_tx: Vec::new(),
        },
        NotReadyErrorClass::Other,
    );
    assert!(p.change_hints.is_none());
    let v = serde_json::to_value(&p).unwrap();
    assert!(
        v.get("changeHints").is_none(),
        "not_ready must omit changeHints key: {v}"
    );

    // Empty changes → no changeHints when building summary path signals
    let impact = base_packet(vec![]);
    assert!(impact.changes.is_empty());
    // mapped helper still safe
    assert!(mapped_paths_from_impact(&impact).is_empty());
}

#[test]
fn change_hints_rename_control_not_greenfield() {
    use crate::impact::enrichment::change_hints::{
        ChangeHintsKind, ChangeHintsOpts, compute_change_hints,
    };

    let files = vec![ChangedFile {
        path: PathBuf::from("src/newpkg/mod.rs"),
        status: "Renamed".to_string(),
        old_path: Some(PathBuf::from("src/oldpkg/mod.rs")),
        is_staged: false,
        symbols: None,
        imports: None,
        runtime_usage: None,
        analysis_status: FileAnalysisStatus::default(),
        analysis_warnings: Vec::new(),
        ..Default::default()
    }];
    let hints = compute_change_hints(&files, &ChangeHintsOpts::default());
    assert_eq!(hints.kind, ChangeHintsKind::None);
    assert!(!hints.mostly_added);
}
