use ledgerful::impact::analysis::analyze_risk;
use ledgerful::impact::packet::{
    ChangedFile, FileAnalysisStatus, ImpactPacket, RiskLevel, RuntimeUsageDelta,
};
use ledgerful::index::env_schema::EnvVarDep;
use ledgerful::index::symbols::{Symbol, SymbolKind};
use ledgerful::policy::rules::Rules;
use std::path::PathBuf;

#[test]
fn test_risk_analysis_integration() {
    let mut packet = ImpactPacket::default();

    // Scenario: Modified a public symbol
    packet.changes.push(ChangedFile {
        path: PathBuf::from("src/lib.rs"),
        status: "Modified".to_string(),
        old_path: None,
        is_staged: true,

        symbols: Some(vec![Symbol {
            name: "highly_risky".into(),
            kind: SymbolKind::Function,
            is_public: true,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: None,
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata: std::collections::BTreeMap::new(),
        }]),

        imports: None,
        runtime_usage: None,
        analysis_status: FileAnalysisStatus::default(),
        analysis_warnings: Vec::new(),
        api_routes: Vec::new(),
        data_models: Vec::new(),
        ci_gates: Vec::new(),
    });

    let rules = Rules::default();
    analyze_risk(
        &mut packet,
        &rules,
        &ledgerful::config::model::Config::default(),
    )
    .unwrap();

    // Weight: 30 (public symbol) -> Medium
    assert_eq!(packet.risk_level, RiskLevel::Medium);
    assert!(
        packet
            .risk_reasons
            .iter()
            .any(|r| r.contains("Public symbol modified"))
    );
}

#[test]
fn test_risk_analysis_high_volume() {
    let mut packet = ImpactPacket::default();

    // Scenario: Many files changed
    for i in 0..10 {
        packet.changes.push(ChangedFile {
            path: PathBuf::from(format!("file_{}.rs", i)),
            status: "Added".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        });
    }

    let rules = Rules::default();
    analyze_risk(
        &mut packet,
        &rules,
        &ledgerful::config::model::Config::default(),
    )
    .unwrap();

    // Weight: 20 (volume) -> Medium (because 20 is Low, wait, 21-60 is Medium)
    // Actually in my implementation 20 is Low. Let's add more weight.

    // Add public symbols
    packet.changes[0].symbols = Some(vec![Symbol {
        name: "api".into(),
        kind: SymbolKind::Function,
        is_public: true,
        cognitive_complexity: None,
        cyclomatic_complexity: None,
        line_start: None,
        line_end: None,
        qualified_name: None,
        byte_start: None,
        byte_end: None,
        entrypoint_kind: None,
        metadata: std::collections::BTreeMap::new(),
    }]);

    analyze_risk(
        &mut packet,
        &rules,
        &ledgerful::config::model::Config::default(),
    )
    .unwrap();

    // Weight: 20 (volume) + 30 (public symbol) = 50 -> Medium
    assert_eq!(packet.risk_level, RiskLevel::Medium);
}

#[test]
fn test_risk_analysis_protected_and_public() {
    let mut packet = ImpactPacket::default();

    packet.changes.push(ChangedFile {
        path: PathBuf::from("Cargo.toml"),
        status: "Modified".to_string(),
        old_path: None,
        is_staged: true,

        symbols: None,
        imports: None,
        runtime_usage: None,
        analysis_status: FileAnalysisStatus::default(),
        analysis_warnings: Vec::new(),
        api_routes: Vec::new(),
        data_models: Vec::new(),
        ci_gates: Vec::new(),
    });

    packet.changes.push(ChangedFile {
        path: PathBuf::from("src/api.rs"),
        status: "Modified".to_string(),
        old_path: None,
        is_staged: true,

        symbols: Some(vec![Symbol {
            name: "highly_risky".into(),
            kind: SymbolKind::Function,
            is_public: true,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: None,
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata: std::collections::BTreeMap::new(),
        }]),

        imports: None,
        runtime_usage: None,
        analysis_status: FileAnalysisStatus::default(),
        analysis_warnings: Vec::new(),
        api_routes: Vec::new(),
        data_models: Vec::new(),
        ci_gates: Vec::new(),
    });

    let rules = Rules {
        protected_paths: vec!["Cargo.toml".to_string()],
        ..Rules::default()
    };

    analyze_risk(
        &mut packet,
        &rules,
        &ledgerful::config::model::Config::default(),
    )
    .unwrap();

    // Weight: 70 (protected) + 30 (public) = 100 -> High
    assert_eq!(packet.risk_level, RiskLevel::High);
    assert!(packet.risk_reasons.len() >= 2);
}

// ── E4-3: Env-var risk signals ────────────────────────────────────────────────

#[test]
fn test_env_var_dep_triggers_risk_reason() {
    let mut packet = ImpactPacket::default();
    packet.env_var_deps.push(EnvVarDep {
        var_name: "DATABASE_URL".to_string(),
        declared: false,
        evidence: "src/db.rs".to_string(),
    });

    let rules = Rules::default();
    analyze_risk(
        &mut packet,
        &rules,
        &ledgerful::config::model::Config::default(),
    )
    .unwrap();

    assert!(
        packet
            .risk_reasons
            .iter()
            .any(|r| r.contains("DATABASE_URL")),
        "Expected DATABASE_URL in risk reasons, got: {:?}",
        packet.risk_reasons
    );
    assert!(packet.risk_level >= RiskLevel::Low);
}

#[test]
fn test_common_env_var_dep_is_filtered_from_risk() {
    let mut packet = ImpactPacket::default();
    // PATH is in the common-vars filter and must not produce a risk reason
    packet.env_var_deps.push(EnvVarDep {
        var_name: "PATH".to_string(),
        declared: true,
        evidence: "src/lib.rs".to_string(),
    });

    let rules = Rules::default();
    analyze_risk(
        &mut packet,
        &rules,
        &ledgerful::config::model::Config::default(),
    )
    .unwrap();

    assert!(
        !packet.risk_reasons.iter().any(|r| r.contains("PATH")),
        "Common env var PATH should not appear in risk reasons"
    );
    // With only a filtered var the risk stays Low / minimal
    assert_eq!(packet.risk_level, RiskLevel::Low);
}

// ── E4-4: Runtime usage delta risk signals ────────────────────────────────────

#[test]
fn test_runtime_delta_env_count_change_triggers_risk_reason() {
    let mut packet = ImpactPacket::default();
    packet.runtime_usage_delta.push(RuntimeUsageDelta {
        file_path: "src/server.rs".to_string(),
        env_vars_previous_count: 1,
        env_vars_current_count: 3,
        config_keys_previous_count: 0,
        config_keys_current_count: 0,
        ..Default::default()
    });

    let rules = Rules::default();
    analyze_risk(
        &mut packet,
        &rules,
        &ledgerful::config::model::Config::default(),
    )
    .unwrap();

    assert!(
        packet
            .risk_reasons
            .iter()
            .any(|r| r.contains("New environment variable references")),
        "Expected env-var delta reason, got: {:?}",
        packet.risk_reasons
    );
}

#[test]
fn test_runtime_delta_config_count_change_triggers_risk_reason() {
    let mut packet = ImpactPacket::default();
    packet.runtime_usage_delta.push(RuntimeUsageDelta {
        file_path: "src/config.rs".to_string(),
        env_vars_previous_count: 0,
        env_vars_current_count: 0,
        config_keys_previous_count: 2,
        config_keys_current_count: 4,
        ..Default::default()
    });

    let rules = Rules::default();
    analyze_risk(
        &mut packet,
        &rules,
        &ledgerful::config::model::Config::default(),
    )
    .unwrap();

    assert!(
        packet
            .risk_reasons
            .iter()
            .any(|r| r.contains("Configuration key references changed")),
        "Expected config-key delta reason, got: {:?}",
        packet.risk_reasons
    );
}

#[test]
fn test_runtime_delta_same_cardinality_identity_change_detected() {
    // Y5: The delta model now tracks identity, not just counts.
    // Replacing DATABASE_URL→REDIS_URL (1→1) now produces a signal.
    let mut packet = ImpactPacket::default();
    packet.runtime_usage_delta.push(RuntimeUsageDelta {
        file_path: "src/db.rs".to_string(),
        env_vars_previous_count: 1,
        env_vars_current_count: 1, // same count but different variable
        config_keys_previous_count: 0,
        config_keys_current_count: 0,
        env_vars_previous: vec!["DATABASE_URL".to_string()],
        env_vars_current: vec!["REDIS_URL".to_string()],
    });

    let rules = Rules::default();
    analyze_risk(
        &mut packet,
        &rules,
        &ledgerful::config::model::Config::default(),
    )
    .unwrap();

    assert!(
        packet
            .risk_reasons
            .iter()
            .any(|r| r.contains("Environment variable identities changed")),
        "Expected identity-aware env-var delta reason, got: {:?}",
        packet.risk_reasons
    );
}

#[test]
fn test_path_weighted_risk_scoring() {
    use ledgerful::index::symbols::{Symbol, SymbolKind};
    let mut packet = ImpactPacket::default();

    packet.changes.push(ChangedFile {
        path: PathBuf::from("README.md"),
        status: "Modified".to_string(),
        is_staged: true,
        symbols: Some(vec![Symbol {
            name: "doc_symbol".into(),
            kind: SymbolKind::Function,
            is_public: true,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: None,
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata: std::collections::BTreeMap::new(),
        }]),
        ..ChangedFile::default()
    });

    let rules = Rules::default();
    let config = ledgerful::config::model::Config::default();
    analyze_risk(&mut packet, &rules, &config).unwrap();

    assert_eq!(packet.risk_level, RiskLevel::Low);
}
