use ledgerful::gemini::narrative::NarrativeEngine;
use ledgerful::impact::packet::{
    ChangedFile, FileAnalysisStatus, Hotspot, ImpactPacket, RiskLevel, TemporalCoupling,
};
use std::path::PathBuf;

#[test]
fn test_narrative_golden_prompt() {
    let mut packet = ImpactPacket {
        schema_version: "v1".to_string(),
        timestamp_utc: "2023-10-27T10:00:00Z".to_string(),
        head_hash: Some("abcdef123456".to_string()),
        branch_name: Some("main".to_string()),
        risk_level: RiskLevel::High,
        risk_reasons: vec![
            "Critical hotspot modified".to_string(),
            "High temporal coupling with authentication module".to_string(),
        ],
        changes: vec![
            ChangedFile {
                path: PathBuf::from("src/auth.rs"),
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
            },
            ChangedFile {
                path: PathBuf::from("src/main.rs"),
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
            },
        ],
        temporal_couplings: vec![TemporalCoupling {
            file_a: PathBuf::from("src/auth.rs"),
            file_b: PathBuf::from("src/session.rs"),
            score: 0.85,
        }],
        structural_couplings: vec![],
        centrality_risks: vec![],
        logging_coverage_delta: vec![],
        error_handling_delta: vec![],
        telemetry_coverage_delta: vec![],
        infrastructure_dirs: vec![],
        env_var_deps: vec![],
        test_coverage: vec![],
        runtime_usage_delta: vec![],
        hotspots: vec![Hotspot {
            path: PathBuf::from("src/auth.rs"),
            score: 0.92,
            display_score: 0.0,
            complexity: 45,
            frequency: 150.0,
            centrality: None,
        }],
        verification_results: Vec::new(),
        relevant_decisions: vec![],
        observability: vec![],
        affected_contracts: vec![],
        service_map_delta: None,
        ..ImpactPacket::default()
    };

    // Ensure deterministic order
    packet.finalize();

    let prompt = NarrativeEngine::generate_risk_prompt(&packet);

    let expected = r#"Act as a Senior Software Architect. Provide a high-level narrative summary of the following change impact report.

## Core Analysis
- Overall Risk Level: High
- Risk Reasons:
  * Critical hotspot modified
  * High temporal coupling with authentication module

## Changes Summary
- Total files changed: 2
  * src/auth.rs (Modified)
  * src/main.rs (Modified)

## Code Hotspots (High Risk Density)
  * src/auth.rs: Score 0.92 (Freq: 150, Complexity: 45)

## Temporal Couplings (Logical Dependencies)
  * src/auth.rs <-> src/session.rs (Affinity: 85%)

## Task
Explain the 'Butterfly Effect' of these changes. What is the most likely thing to break that is NOT in the changed files? What should the reviewer focus on most?"#;

    assert_eq!(prompt.trim(), expected.trim());
}
