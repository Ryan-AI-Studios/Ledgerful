mod changed_file;
pub use self::changed_file::*;

mod coverage;
pub use self::coverage::*;

mod intelligence;
pub use self::intelligence::*;

mod risk;
pub use self::risk::*;

mod serialization;

mod surfaces;
pub use self::surfaces::*;

mod verification;
pub use self::verification::*;

mod metadata;
pub use self::metadata::*;

#[cfg(test)]
mod schema_golden_tests {
    use super::*;
    use crate::contracts::AffectedContract;
    use crate::index::env_schema::EnvVarDep;
    use crate::index::references::ImportExport;
    use crate::index::runtime_usage::RuntimeUsage;
    use crate::index::symbols::Symbol;
    use crate::observability::signal::{ObservabilitySignal, SignalSeverity};
    use std::path::PathBuf;

    /// Assert that a JSON object has exactly the expected keys (no more, no less).
    fn assert_exact_keys(value: &serde_json::Value, expected: &[&str]) {
        let obj = value.as_object().expect("value must be an object");
        let mut actual: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
        let mut expected: Vec<&str> = expected.to_vec();
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(
            actual, expected,
            "object keys do not match expected set for {value}"
        );
    }

    /// Assert that object keys at the given indentation level appear in the exact
    /// expected order in a pretty-printed JSON string. This verifies emitted
    /// serialization order without relying on parsed map ordering.
    #[allow(clippy::collapsible_if)]
    fn assert_field_order(json: &str, indent: usize, expected: &[&str]) {
        let prefix = " ".repeat(indent);
        let next_prefix = " ".repeat(indent + 1);
        let mut actual: Vec<&str> = Vec::new();
        for line in json.lines() {
            if line.starts_with(&prefix) && !line.starts_with(&next_prefix) {
                let rest = &line[indent..];
                if let Some(stripped) = rest.strip_prefix('"') {
                    if let Some(end) = stripped.find('"') {
                        actual.push(&stripped[..end]);
                    }
                }
            }
        }
        let expected: Vec<&str> = expected.to_vec();
        assert_eq!(actual, expected, "field order mismatch at indent {indent}");
    }

    /// Constructs a fully-populated ImpactPacket to verify schema-stability.
    /// This test must pass before and after any refactoring.
    #[test]
    fn test_schema_stability_golden() {
        let packet = ImpactPacket {
            schema_version: "v1".to_string(),
            timestamp_utc: "2023-10-27T10:00:00Z".to_string(),
            head_hash: Some("abc123".to_string()),
            branch_name: Some("main".to_string()),
            tree_clean: false,
            risk_level: RiskLevel::High,
            risk_reasons: vec!["reason-a".to_string()],
            changes: vec![ChangedFile {
                path: PathBuf::from("src/main.rs"),
                status: "Modified".to_string(),
                old_path: Some(PathBuf::from("src/old.rs")),
                is_staged: true,
                symbols: Some(vec![Symbol {
                    name: "foo".into(),
                    kind: crate::index::symbols::SymbolKind::Function,
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
                imports: Some(ImportExport {
                    imported_from: vec!["dep".to_string()],
                    exported_symbols: vec!["bar".to_string()],
                }),
                runtime_usage: Some(RuntimeUsage {
                    env_vars: vec!["FOO".to_string()],
                    config_keys: vec!["key".to_string()],
                }),
                analysis_status: FileAnalysisStatus {
                    symbols: AnalysisStatus::Ok,
                    imports: AnalysisStatus::Ok,
                    runtime_usage: AnalysisStatus::Ok,
                },
                analysis_warnings: vec!["warn".to_string()],
                api_routes: vec![ApiRoute {
                    method: "GET".to_string(),
                    path_pattern: "/api".to_string(),
                    handler_symbol_name: Some("handler".to_string()),
                    framework: "axum".to_string(),
                    route_source: "file".to_string(),
                    mount_prefix: None,
                    is_dynamic: false,
                    route_confidence: 1.0,
                    evidence: "ev".to_string(),
                    auth_requirements: None,
                    schema_refs: None,
                    owning_service: None,
                    consumers: None,
                }],
                data_models: vec![DataModel {
                    model_name: "User".to_string(),
                    model_kind: "struct".to_string(),
                    confidence: 0.9,
                    evidence: Some("e".to_string()),
                }],
                ci_gates: vec![CIGate {
                    platform: "github".to_string(),
                    job_name: "ci".to_string(),
                    trigger: Some("push".to_string()),
                    workflow_name: None,
                    environment: None,
                    artifacts: Vec::new(),
                    release_gates: Vec::new(),
                }],
            }],
            temporal_couplings: vec![TemporalCoupling {
                file_a: PathBuf::from("a.rs"),
                file_b: PathBuf::from("b.rs"),
                score: 0.8,
            }],
            structural_couplings: vec![StructuralCoupling {
                caller_symbol_name: "x".to_string(),
                callee_symbol_name: "y".to_string(),
                caller_file_path: PathBuf::from("x.rs"),
            }],
            centrality_risks: vec![CentralityRisk {
                symbol_name: "main".to_string(),
                entrypoints_reachable: 3,
            }],
            logging_coverage_delta: vec![CoverageDelta {
                file_path: "a.rs".to_string(),
                pattern_kind: "log".to_string(),
                previous_count: 1,
                current_count: 2,
                message: "m".to_string(),
            }],
            error_handling_delta: vec![],
            telemetry_coverage_delta: vec![],
            infrastructure_dirs: vec!["infra".to_string()],
            env_var_deps: vec![EnvVarDep {
                var_name: "VAR".to_string(),
                declared: true,
                evidence: "ev".to_string(),
            }],
            test_coverage: vec![TestCoverage {
                changed_symbol: "s".to_string(),
                changed_file: "f.rs".to_string(),
                covering_tests: vec![CoveringTest {
                    test_file: "t.rs".to_string(),
                    test_symbol: "test".to_string(),
                    confidence: 0.8,
                    mapping_kind: "direct".to_string(),
                }],
            }],
            runtime_usage_delta: vec![RuntimeUsageDelta {
                file_path: "f.rs".to_string(),
                env_vars_previous_count: 0,
                env_vars_current_count: 1,
                config_keys_previous_count: 0,
                config_keys_current_count: 1,
                env_vars_previous: Vec::new(),
                env_vars_current: vec!["VAR".to_string()],
            }],
            hotspots: vec![Hotspot {
                path: PathBuf::from("h.rs"),
                score: 0.9,
                display_score: 0.9,
                complexity: 5,
                frequency: 1.0,
                centrality: Some(2),
            }],
            verification_results: vec![VerificationResult {
                name: "fmt".to_string(),
                command: "cargo fmt".to_string(),
                exit_code: 0,
                stdout: "ok".to_string(),
                stderr: "".to_string(),
                duration_ms: 100,
                truncated: false,
            }],
            relevant_decisions: vec![RelevantDecision {
                file_path: PathBuf::from("d.md"),
                heading: Some("h".to_string()),
                excerpt: "e".to_string(),
                similarity: 0.7,
                rerank_score: None,
                staleness_days: None,
                staleness_tier: None,
            }],
            observability: vec![ObservabilitySignal::new(
                "error_rate",
                "svc",
                0.15,
                SignalSeverity::Critical,
                "Error rate 15%",
                "prometheus",
            )],
            affected_contracts: vec![AffectedContract {
                endpoint_id: "api/openapi.json::GET::/pets".to_string(),
                path: "/pets".to_string(),
                method: "GET".to_string(),
                summary: "List all pets".to_string(),
                similarity: 0.85,
                spec_file: "api/openapi.json".to_string(),
            }],
            ai_insights: vec![AiInsight {
                memory_id: "mid".to_string(),
                relevance: 0.5,
                content: "c".to_string(),
            }],
            service_map_delta: Some(ServiceMapDelta {
                services: vec![Service {
                    name: "svc".to_string(),
                    directory: PathBuf::from("svc"),
                    routes: vec!["/r".to_string()],
                    data_models: vec!["M".to_string()],
                    owners: Vec::new(),
                    runtime_name: None,
                    queues: Vec::new(),
                    topics: Vec::new(),
                    rpc_endpoints: Vec::new(),
                }],
                affected_services: vec!["svc".to_string()],
                cross_service_edges: Vec::new(),
                total_services: 1,
            }),
            data_flow_matches: vec![DataFlowMatch {
                chain_label: "l".to_string(),
                changed_nodes: vec!["n".to_string()],
                total_nodes: 2,
                change_pct: 0.5,
                risk: RiskLevel::Medium,
            }],
            trace_config_drift: vec![TraceConfigChange {
                file: PathBuf::from("otel.yml"),
                config_type: TraceConfigType::OpenTelemetryCollector,
                risk_weight: 1,
                is_deleted: false,
            }],
            trace_env_vars: vec![TraceEnvVarChange {
                var_name: "TRACE".to_string(),
                pattern: "p".to_string(),
                risk_weight: 1,
            }],
            sdk_dependencies_delta: Some(SdkDependencyDelta {
                added: vec![SdkDependency {
                    sdk_name: "sdk".to_string(),
                    file_path: PathBuf::from("f.rs"),
                    import_statement: "use sdk;".to_string(),
                }],
                removed: Vec::new(),
                modified: Vec::new(),
            }),
            deploy_manifest_changes: vec![DeployManifestChange {
                file: PathBuf::from("Dockerfile"),
                manifest_type: ManifestType::Dockerfile,
                risk_tier: 2,
                coupled_files: Vec::new(),
                high_blast_resources: Vec::new(),
                service_name: None,
                owner: None,
            }],
            ci_config_change: Some(CiConfigChange {
                known_ci_files: vec![".github/ci.yml".to_string()],
                unknown_ci_files: Vec::new(),
                pre_commit_files: Vec::new(),
                generated_ci_files: Vec::new(),
                source_changed: true,
                deploy_changed: false,
            }),
            ci_predictions: vec![CIPrediction {
                job_name: "ci".to_string(),
                platform: "gh".to_string(),
                failure_probability: 0.1,
                explanation: None,
            }],
            knowledge_graph: vec![KGImpact {
                source_node: "s".to_string(),
                source_category: "cat".to_string(),
                impacted_node: "i".to_string(),
                impacted_category: "cat".to_string(),
                relation: "r".to_string(),
                path_length: 1,
                reason: "r".to_string(),
            }],
            service_impact: Vec::new(),
            analysis_warnings: vec!["w".to_string()],
            dead_code_findings: vec![DeadCodeFinding {
                symbol_name: "unused".to_string(),
                file_path: PathBuf::from("u.rs"),
                confidence: 0.8,
                factors: vec![ConfidenceFactor::NoTestCoverage],
                recommendation: "del".to_string(),
                line_start: None,
                line_end: None,
            }],
        };

        let json = serde_json::to_string_pretty(&packet).unwrap();

        // Key field presence assertions to ensure schema stability
        assert!(
            json.contains(r#""schemaVersion": "v1""#),
            "schemaVersion missing"
        );
        assert!(
            json.contains(r#""timestampUtc": "2023-10-27T10:00:00Z""#),
            "timestampUtc missing"
        );
        assert!(json.contains(r#""headHash": "abc123""#), "headHash missing");
        assert!(
            json.contains(r#""branchName": "main""#),
            "branchName missing"
        );
        assert!(json.contains(r#""treeClean": false"#), "treeClean missing");
        assert!(json.contains(r#""riskLevel": "high""#), "riskLevel missing");
        assert!(json.contains(r#""riskReasons""#), "riskReasons missing");
        assert!(json.contains(r#""changes""#), "changes missing");
        assert!(json.contains(r#""path": "src/main.rs""#), "path missing");
        assert!(json.contains(r#""status": "Modified""#), "status missing");
        assert!(
            json.contains(r#""oldPath": "src/old.rs""#),
            "oldPath missing"
        );
        assert!(json.contains(r#""isStaged": true"#), "isStaged missing");
        assert!(json.contains(r#""symbols""#), "symbols missing");
        assert!(json.contains(r#""imports""#), "imports missing");
        assert!(json.contains(r#""runtimeUsage""#), "runtimeUsage missing");
        assert!(
            json.contains(r#""analysisStatus""#),
            "analysisStatus missing"
        );
        assert!(
            json.contains(r#""analysisWarnings""#),
            "analysisWarnings missing"
        );
        assert!(json.contains(r#""apiRoutes""#), "apiRoutes missing");
        assert!(json.contains(r#""dataModels""#), "dataModels missing");
        assert!(json.contains(r#""ciGates""#), "ciGates missing");
        assert!(
            json.contains(r#""temporalCouplings""#),
            "temporalCouplings missing"
        );
        assert!(
            json.contains(r#""structuralCouplings""#),
            "structuralCouplings missing"
        );
        assert!(
            json.contains(r#""centralityRisks""#),
            "centralityRisks missing"
        );
        assert!(
            json.contains(r#""loggingCoverageDelta""#),
            "loggingCoverageDelta missing"
        );
        assert!(
            json.contains(r#""infrastructureDirs""#),
            "infrastructureDirs missing"
        );
        assert!(json.contains(r#""envVarDeps""#), "envVarDeps missing");
        assert!(json.contains(r#""testCoverage""#), "testCoverage missing");
        assert!(
            json.contains(r#""runtimeUsageDelta""#),
            "runtimeUsageDelta missing"
        );
        assert!(json.contains(r#""hotspots""#), "hotspots missing");
        assert!(
            json.contains(r#""verificationResults""#),
            "verificationResults missing"
        );
        assert!(
            json.contains(r#""relevantDecisions""#),
            "relevantDecisions missing"
        );
        assert!(json.contains(r#""aiInsights""#), "aiInsights missing");
        assert!(
            json.contains(r#""serviceMapDelta""#),
            "serviceMapDelta missing"
        );
        assert!(
            json.contains(r#""dataFlowMatches""#),
            "dataFlowMatches missing"
        );
        assert!(
            json.contains(r#""traceConfigDrift""#),
            "traceConfigDrift missing"
        );
        assert!(json.contains(r#""traceEnvVars""#), "traceEnvVars missing");
        assert!(
            json.contains(r#""sdkDependenciesDelta""#),
            "sdkDependenciesDelta missing"
        );
        assert!(
            json.contains(r#""deployManifestChanges""#),
            "deployManifestChanges missing"
        );
        assert!(
            json.contains(r#""ciConfigChange""#),
            "ciConfigChange missing"
        );
        assert!(json.contains(r#""ciPredictions""#), "ciPredictions missing");
        assert!(
            json.contains(r#""knowledgeGraph""#),
            "knowledgeGraph missing"
        );
        assert!(
            json.contains(r#""deadCodeFindings""#),
            "deadCodeFindings missing"
        );

        // Round-trip verification
        let parsed: ImpactPacket = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.schema_version, "v1");
        assert_eq!(parsed.changes.len(), 1);
        assert_eq!(parsed.changes[0].path, PathBuf::from("src/main.rs"));
        assert_eq!(parsed.risk_level, RiskLevel::High);
        assert_eq!(parsed.temporal_couplings.len(), 1);
        assert_eq!(parsed.hotspots.len(), 1);

        // Exact shape verification via serde_json::Value
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Top-level exact key set (35 keys — no more, no less)
        assert_exact_keys(
            &value,
            &[
                "schemaVersion",
                "timestampUtc",
                "headHash",
                "branchName",
                "treeClean",
                "riskLevel",
                "riskReasons",
                "changes",
                "temporalCouplings",
                "structuralCouplings",
                "centralityRisks",
                "loggingCoverageDelta",
                "errorHandlingDelta",
                "telemetryCoverageDelta",
                "infrastructureDirs",
                "envVarDeps",
                "testCoverage",
                "runtimeUsageDelta",
                "hotspots",
                "verificationResults",
                "relevantDecisions",
                "observability",
                "affectedContracts",
                "aiInsights",
                "serviceMapDelta",
                "dataFlowMatches",
                "traceConfigDrift",
                "traceEnvVars",
                "sdkDependenciesDelta",
                "deployManifestChanges",
                "ciConfigChange",
                "ciPredictions",
                "knowledgeGraph",
                "analysisWarnings",
                "deadCodeFindings",
            ],
        );

        // Exact scalar values at top level
        assert_eq!(value["schemaVersion"], "v1");
        assert_eq!(value["timestampUtc"], "2023-10-27T10:00:00Z");
        assert_eq!(value["headHash"], "abc123");
        assert_eq!(value["branchName"], "main");
        assert_eq!(value["treeClean"], false);
        assert_eq!(value["riskLevel"], "high");
        assert_eq!(value["riskReasons"].as_array().unwrap().len(), 1);
        assert_eq!(value["riskReasons"][0], "reason-a");

        // changes[0] exact key set
        let changes = value["changes"].as_array().expect("changes array");
        assert_eq!(changes.len(), 1);
        let change0 = &changes[0];
        assert_exact_keys(
            change0,
            &[
                "path",
                "status",
                "oldPath",
                "isStaged",
                "symbols",
                "imports",
                "runtimeUsage",
                "analysisStatus",
                "analysisWarnings",
                "apiRoutes",
                "dataModels",
                "ciGates",
            ],
        );
        assert_eq!(change0["path"], "src/main.rs");
        assert_eq!(change0["status"], "Modified");
        assert_eq!(change0["oldPath"], "src/old.rs");
        assert_eq!(change0["isStaged"], true);

        // symbols[0] inside changes[0]
        let symbols = change0["symbols"].as_array().unwrap();
        assert_eq!(symbols.len(), 1);
        assert_exact_keys(
            &symbols[0],
            &[
                "name",
                "kind",
                "isPublic",
                "cognitiveComplexity",
                "cyclomaticComplexity",
                "lineStart",
                "lineEnd",
                "qualifiedName",
                "byteStart",
                "byteEnd",
                "entrypointKind",
                "metadata",
            ],
        );
        assert_eq!(symbols[0]["name"], "foo");
        assert_eq!(symbols[0]["kind"], "function");

        // imports inside changes[0]
        assert_exact_keys(&change0["imports"], &["importedFrom", "exportedSymbols"]);
        assert_eq!(change0["imports"]["importedFrom"][0], "dep");

        // runtimeUsage inside changes[0]
        assert_exact_keys(&change0["runtimeUsage"], &["envVars", "configKeys"]);
        assert_eq!(change0["runtimeUsage"]["envVars"][0], "FOO");

        // analysisStatus inside changes[0]
        assert_exact_keys(
            &change0["analysisStatus"],
            &["symbols", "imports", "runtimeUsage"],
        );
        assert_eq!(change0["analysisStatus"]["symbols"], "ok");

        // apiRoutes[0] inside changes[0]
        let api_routes = change0["apiRoutes"].as_array().unwrap();
        assert_eq!(api_routes.len(), 1);
        assert_exact_keys(
            &api_routes[0],
            &[
                "method",
                "pathPattern",
                "handlerSymbolName",
                "framework",
                "routeSource",
                "mountPrefix",
                "isDynamic",
                "routeConfidence",
                "evidence",
                "authRequirements",
                "schemaRefs",
                "owningService",
                "consumers",
            ],
        );

        // dataModels[0] inside changes[0]
        let data_models = change0["dataModels"].as_array().unwrap();
        assert_eq!(data_models.len(), 1);
        assert_exact_keys(
            &data_models[0],
            &["modelName", "modelKind", "confidence", "evidence"],
        );

        // ciGates[0] inside changes[0]
        let ci_gates = change0["ciGates"].as_array().unwrap();
        assert_eq!(ci_gates.len(), 1);
        assert_exact_keys(&ci_gates[0], &["platform", "jobName", "trigger"]);

        // temporalCouplings[0]
        let tc = value["temporalCouplings"].as_array().unwrap();
        assert_eq!(tc.len(), 1);
        assert_exact_keys(&tc[0], &["fileA", "fileB", "score"]);

        // structuralCouplings[0]
        let sc = value["structuralCouplings"].as_array().unwrap();
        assert_eq!(sc.len(), 1);
        assert_exact_keys(
            &sc[0],
            &["callerSymbolName", "calleeSymbolName", "callerFilePath"],
        );

        // centralityRisks[0]
        let cr = value["centralityRisks"].as_array().unwrap();
        assert_eq!(cr.len(), 1);
        assert_exact_keys(&cr[0], &["symbolName", "entrypointsReachable"]);

        // loggingCoverageDelta[0]
        let lcd = value["loggingCoverageDelta"].as_array().unwrap();
        assert_eq!(lcd.len(), 1);
        assert_exact_keys(
            &lcd[0],
            &[
                "filePath",
                "patternKind",
                "previousCount",
                "currentCount",
                "message",
            ],
        );

        // testCoverage[0]
        let test_cov = value["testCoverage"].as_array().unwrap();
        assert_eq!(test_cov.len(), 1);
        assert_exact_keys(
            &test_cov[0],
            &["changedSymbol", "changedFile", "coveringTests"],
        );
        let covering_tests = test_cov[0]["coveringTests"].as_array().unwrap();
        assert_eq!(covering_tests.len(), 1);
        assert_exact_keys(
            &covering_tests[0],
            &["testFile", "testSymbol", "confidence", "mappingKind"],
        );

        // runtimeUsageDelta[0]
        let rud = value["runtimeUsageDelta"].as_array().unwrap();
        assert_eq!(rud.len(), 1);
        assert_exact_keys(
            &rud[0],
            &[
                "filePath",
                "envVarsPreviousCount",
                "envVarsCurrentCount",
                "configKeysPreviousCount",
                "configKeysCurrentCount",
                "envVarsPrevious",
                "envVarsCurrent",
            ],
        );

        // hotspots[0]
        let hs = value["hotspots"].as_array().unwrap();
        assert_eq!(hs.len(), 1);
        assert_exact_keys(
            &hs[0],
            &[
                "path",
                "score",
                "displayScore",
                "complexity",
                "frequency",
                "centrality",
            ],
        );

        // verificationResults[0]
        let vr = value["verificationResults"].as_array().unwrap();
        assert_eq!(vr.len(), 1);
        assert_exact_keys(
            &vr[0],
            &[
                "name",
                "command",
                "exitCode",
                "stdout",
                "stderr",
                "durationMs",
                "truncated",
            ],
        );

        // relevantDecisions[0]
        let rd = value["relevantDecisions"].as_array().unwrap();
        assert_eq!(rd.len(), 1);
        assert_exact_keys(&rd[0], &["filePath", "heading", "excerpt", "similarity"]);

        // observability[0]
        let obs = value["observability"].as_array().unwrap();
        assert_eq!(obs.len(), 1);
        assert_exact_keys(
            &obs[0],
            &[
                "signal_type",
                "signal_label",
                "value",
                "severity",
                "excerpt",
                "source",
            ],
        );

        // affectedContracts[0]
        let ac = value["affectedContracts"].as_array().unwrap();
        assert_eq!(ac.len(), 1);
        assert_exact_keys(
            &ac[0],
            &[
                "endpoint_id",
                "path",
                "method",
                "summary",
                "similarity",
                "spec_file",
            ],
        );

        // aiInsights[0]
        let ai = value["aiInsights"].as_array().unwrap();
        assert_eq!(ai.len(), 1);
        assert_exact_keys(&ai[0], &["memoryId", "relevance", "content"]);

        // serviceMapDelta
        let smd = &value["serviceMapDelta"];
        assert_exact_keys(
            smd,
            &[
                "services",
                "affected_services",
                "cross_service_edges",
                "total_services",
            ],
        );
        let services = smd["services"].as_array().unwrap();
        assert_eq!(services.len(), 1);
        assert_exact_keys(
            &services[0],
            &[
                "name",
                "directory",
                "routes",
                "data_models",
                "owners",
                "runtime_name",
                "queues",
                "topics",
                "rpc_endpoints",
            ],
        );

        // dataFlowMatches[0]
        let dfm = value["dataFlowMatches"].as_array().unwrap();
        assert_eq!(dfm.len(), 1);
        assert_exact_keys(
            &dfm[0],
            &[
                "chainLabel",
                "changedNodes",
                "totalNodes",
                "changePct",
                "risk",
            ],
        );

        // traceConfigDrift[0]
        let tcd = value["traceConfigDrift"].as_array().unwrap();
        assert_eq!(tcd.len(), 1);
        assert_exact_keys(&tcd[0], &["file", "configType", "riskWeight", "isDeleted"]);

        // traceEnvVars[0]
        let tev = value["traceEnvVars"].as_array().unwrap();
        assert_eq!(tev.len(), 1);
        assert_exact_keys(&tev[0], &["varName", "pattern", "riskWeight"]);

        // sdkDependenciesDelta
        let sdd = &value["sdkDependenciesDelta"];
        assert_exact_keys(sdd, &["added", "removed", "modified"]);
        let sdk_added = sdd["added"].as_array().unwrap();
        assert_eq!(sdk_added.len(), 1);
        assert_exact_keys(&sdk_added[0], &["sdkName", "filePath", "importStatement"]);

        // deployManifestChanges[0]
        let dmc = value["deployManifestChanges"].as_array().unwrap();
        assert_eq!(dmc.len(), 1);
        assert_exact_keys(
            &dmc[0],
            &[
                "file",
                "manifestType",
                "riskTier",
                "coupledFiles",
                "highBlastResources",
            ],
        );

        // ciConfigChange
        let ccc = &value["ciConfigChange"];
        assert_exact_keys(ccc, &["knownCiFiles", "sourceChanged", "deployChanged"]);

        // ciPredictions[0]
        let cp = value["ciPredictions"].as_array().unwrap();
        assert_eq!(cp.len(), 1);
        assert_exact_keys(
            &cp[0],
            &["jobName", "platform", "failureProbability", "explanation"],
        );

        // knowledgeGraph[0]
        let kg = value["knowledgeGraph"].as_array().unwrap();
        assert_eq!(kg.len(), 1);
        assert_exact_keys(
            &kg[0],
            &[
                "sourceNode",
                "sourceCategory",
                "impactedNode",
                "impactedCategory",
                "relation",
                "pathLength",
                "reason",
            ],
        );

        // deadCodeFindings[0]
        let dcf = value["deadCodeFindings"].as_array().unwrap();
        assert_eq!(dcf.len(), 1);
        assert_exact_keys(
            &dcf[0],
            &[
                "symbolName",
                "filePath",
                "confidence",
                "factors",
                "recommendation",
            ],
        );

        // envVarDeps[0]
        let evd = value["envVarDeps"].as_array().unwrap();
        assert_eq!(evd.len(), 1);
        assert_exact_keys(&evd[0], &["varName", "declared", "evidence"]);

        // Exact field order in raw JSON for top-level object (indent = 2)
        assert_field_order(
            &json,
            2,
            &[
                "schemaVersion",
                "timestampUtc",
                "headHash",
                "branchName",
                "treeClean",
                "riskLevel",
                "riskReasons",
                "changes",
                "temporalCouplings",
                "structuralCouplings",
                "centralityRisks",
                "loggingCoverageDelta",
                "errorHandlingDelta",
                "telemetryCoverageDelta",
                "infrastructureDirs",
                "envVarDeps",
                "testCoverage",
                "runtimeUsageDelta",
                "hotspots",
                "verificationResults",
                "relevantDecisions",
                "observability",
                "affectedContracts",
                "aiInsights",
                "dataFlowMatches",
                "serviceMapDelta",
                "traceConfigDrift",
                "traceEnvVars",
                "sdkDependenciesDelta",
                "deployManifestChanges",
                "ciConfigChange",
                "ciPredictions",
                "knowledgeGraph",
                "analysisWarnings",
                "deadCodeFindings",
            ],
        );

        // Exact field order for changes[0] via isolated serialization
        let change0_json = serde_json::to_string_pretty(&packet.changes[0]).unwrap();
        assert_field_order(
            &change0_json,
            2,
            &[
                "path",
                "status",
                "oldPath",
                "isStaged",
                "symbols",
                "imports",
                "runtimeUsage",
                "analysisStatus",
                "analysisWarnings",
                "apiRoutes",
                "dataModels",
                "ciGates",
            ],
        );
    }

    /// Verify that a default/empty packet omits fields governed by skip_serializing_if
    /// and preserves fields that lack the attribute.
    #[test]
    fn test_schema_stability_golden_omitted_empty_behavior() {
        let packet = ImpactPacket::default();
        let json = serde_json::to_string_pretty(&packet).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Fields with skip_serializing_if on empty collections/options must be absent
        assert!(
            !json.contains("relevantDecisions"),
            "relevantDecisions should be omitted when empty"
        );
        assert!(
            !json.contains("observability"),
            "observability should be omitted when empty"
        );
        assert!(
            !json.contains("affectedContracts"),
            "affectedContracts should be omitted when empty"
        );
        assert!(
            !json.contains("aiInsights"),
            "aiInsights should be omitted when empty"
        );
        assert!(
            !json.contains("traceConfigDrift"),
            "traceConfigDrift should be omitted when empty"
        );
        assert!(
            !json.contains("traceEnvVars"),
            "traceEnvVars should be omitted when empty"
        );
        assert!(
            !json.contains("deployManifestChanges"),
            "deployManifestChanges should be omitted when empty"
        );
        assert!(
            !json.contains("ciPredictions"),
            "ciPredictions should be omitted when empty"
        );
        assert!(
            !json.contains("analysisWarnings"),
            "analysisWarnings should be omitted when empty"
        );
        assert!(
            !json.contains("deadCodeFindings"),
            "deadCodeFindings should be omitted when empty"
        );
        assert!(
            !json.contains("sdkDependenciesDelta"),
            "sdkDependenciesDelta should be omitted when None"
        );
        assert!(
            !json.contains("ciConfigChange"),
            "ciConfigChange should be omitted when None"
        );

        // Fields without skip_serializing_if serialize even when empty/null
        assert!(
            value.get("headHash").unwrap().is_null(),
            "headHash must be null when None"
        );
        assert!(
            value.get("branchName").unwrap().is_null(),
            "branchName must be null when None"
        );
        assert!(
            value.get("serviceMapDelta").unwrap().is_null(),
            "serviceMapDelta must be null when None"
        );
        assert!(
            value.get("loggingCoverageDelta").unwrap().is_array(),
            "loggingCoverageDelta must be present as array"
        );
        assert!(
            value.get("errorHandlingDelta").unwrap().is_array(),
            "errorHandlingDelta must be present as array"
        );
        assert!(
            value.get("telemetryCoverageDelta").unwrap().is_array(),
            "telemetryCoverageDelta must be present as array"
        );
        assert!(
            value.get("infrastructureDirs").unwrap().is_array(),
            "infrastructureDirs must be present as array"
        );
        assert!(
            value.get("envVarDeps").unwrap().is_array(),
            "envVarDeps must be present as array"
        );
        assert!(
            value.get("testCoverage").unwrap().is_array(),
            "testCoverage must be present as array"
        );
        assert!(
            value.get("runtimeUsageDelta").unwrap().is_array(),
            "runtimeUsageDelta must be present as array"
        );
        assert!(
            value.get("dataFlowMatches").unwrap().is_array(),
            "dataFlowMatches must be present as array"
        );
        assert!(
            value.get("knowledgeGraph").unwrap().is_array(),
            "knowledgeGraph must be present as array"
        );
    }

    /// Verify that missing #[serde(default)] fields fall back to defaults on deserialization.
    #[test]
    fn test_schema_stability_golden_default_fallback_semantics() {
        let minimal = r#"{
            "schemaVersion": "v1",
            "timestampUtc": "2023-10-27T10:00:00Z",
            "riskLevel": "low",
            "riskReasons": [],
            "changes": [],
            "temporalCouplings": [],
            "structuralCouplings": [],
            "centralityRisks": [],
            "hotspots": [],
            "verificationResults": []
        }"#;

        let parsed: ImpactPacket = serde_json::from_str(minimal).unwrap();
        assert_eq!(parsed.schema_version, "v1");
        assert_eq!(parsed.timestamp_utc, "2023-10-27T10:00:00Z");
        assert_eq!(parsed.risk_level, RiskLevel::Low);
        assert!(!parsed.tree_clean, "tree_clean defaults to false");
        assert!(
            parsed.logging_coverage_delta.is_empty(),
            "logging_coverage_delta defaults to empty"
        );
        assert!(
            parsed.error_handling_delta.is_empty(),
            "error_handling_delta defaults to empty"
        );
        assert!(
            parsed.telemetry_coverage_delta.is_empty(),
            "telemetry_coverage_delta defaults to empty"
        );
        assert!(
            parsed.infrastructure_dirs.is_empty(),
            "infrastructure_dirs defaults to empty"
        );
        assert!(
            parsed.env_var_deps.is_empty(),
            "env_var_deps defaults to empty"
        );
        assert!(
            parsed.test_coverage.is_empty(),
            "test_coverage defaults to empty"
        );
        assert!(
            parsed.runtime_usage_delta.is_empty(),
            "runtime_usage_delta defaults to empty"
        );
        assert!(
            parsed.data_flow_matches.is_empty(),
            "data_flow_matches defaults to empty"
        );
        assert!(
            parsed.knowledge_graph.is_empty(),
            "knowledge_graph defaults to empty"
        );
        assert!(
            parsed.relevant_decisions.is_empty(),
            "relevant_decisions defaults to empty"
        );
        assert!(
            parsed.observability.is_empty(),
            "observability defaults to empty"
        );
        assert!(
            parsed.affected_contracts.is_empty(),
            "affected_contracts defaults to empty"
        );
        assert!(
            parsed.ai_insights.is_empty(),
            "ai_insights defaults to empty"
        );
        assert!(
            parsed.trace_config_drift.is_empty(),
            "trace_config_drift defaults to empty"
        );
        assert!(
            parsed.trace_env_vars.is_empty(),
            "trace_env_vars defaults to empty"
        );
        assert!(
            parsed.deploy_manifest_changes.is_empty(),
            "deploy_manifest_changes defaults to empty"
        );
        assert!(
            parsed.ci_predictions.is_empty(),
            "ci_predictions defaults to empty"
        );
        assert!(
            parsed.analysis_warnings.is_empty(),
            "analysis_warnings defaults to empty"
        );
        assert!(
            parsed.dead_code_findings.is_empty(),
            "dead_code_findings defaults to empty"
        );
        assert!(
            parsed.sdk_dependencies_delta.is_none(),
            "sdk_dependencies_delta defaults to None"
        );
        assert!(
            parsed.ci_config_change.is_none(),
            "ci_config_change defaults to None"
        );
        assert!(
            parsed.service_map_delta.is_none(),
            "service_map_delta defaults to None"
        );
        assert!(parsed.head_hash.is_none(), "head_hash defaults to None");
        assert!(parsed.branch_name.is_none(), "branch_name defaults to None");
    }

    // --- Nested struct field-order & serde-contract coverage ---

    #[test]
    fn test_nested_api_route_field_order() {
        let route = ApiRoute {
            method: "GET".into(),
            path_pattern: "/".into(),
            handler_symbol_name: None,
            framework: "axum".into(),
            route_source: "src/main.rs".into(),
            mount_prefix: None,
            is_dynamic: false,
            route_confidence: 1.0,
            evidence: "regex".into(),
            auth_requirements: None,
            schema_refs: None,
            owning_service: None,
            consumers: None,
        };
        let json = serde_json::to_string_pretty(&route).unwrap();
        assert_field_order(
            &json,
            2,
            &[
                "method",
                "pathPattern",
                "handlerSymbolName",
                "framework",
                "routeSource",
                "mountPrefix",
                "isDynamic",
                "routeConfidence",
                "evidence",
                "authRequirements",
                "schemaRefs",
                "owningService",
                "consumers",
            ],
        );
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_exact_keys(
            &v,
            &[
                "method",
                "pathPattern",
                "handlerSymbolName",
                "framework",
                "routeSource",
                "mountPrefix",
                "isDynamic",
                "routeConfidence",
                "evidence",
                "authRequirements",
                "schemaRefs",
                "owningService",
                "consumers",
            ],
        );
    }

    #[test]
    fn test_nested_ci_gate_field_order_and_omission() {
        let gate = CIGate {
            platform: "github".into(),
            job_name: "test".into(),
            trigger: None,
            workflow_name: None,
            environment: None,
            artifacts: vec![],
            release_gates: vec![],
        };
        let json = serde_json::to_string_pretty(&gate).unwrap();
        assert_field_order(&json, 2, &["platform", "jobName", "trigger"]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_exact_keys(&v, &["platform", "jobName", "trigger"]);

        // Default fallback for omitted Vec fields
        let minimal = r#"{"platform":"github","jobName":"test","trigger":null}"#;
        let parsed: CIGate = serde_json::from_str(minimal).unwrap();
        assert!(parsed.artifacts.is_empty());
        assert!(parsed.release_gates.is_empty());
    }

    #[test]
    fn test_nested_ci_config_change_field_order_and_default() {
        let change = CiConfigChange {
            known_ci_files: vec!["a.yml".into()],
            unknown_ci_files: vec!["b.yml".into()],
            pre_commit_files: vec!["c.yml".into()],
            generated_ci_files: vec!["d.yml".into()],
            source_changed: true,
            deploy_changed: true,
        };
        let json = serde_json::to_string_pretty(&change).unwrap();
        assert_field_order(
            &json,
            2,
            &[
                "knownCiFiles",
                "unknownCiFiles",
                "preCommitFiles",
                "generatedCiFiles",
                "sourceChanged",
                "deployChanged",
            ],
        );
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_exact_keys(
            &v,
            &[
                "knownCiFiles",
                "unknownCiFiles",
                "preCommitFiles",
                "generatedCiFiles",
                "sourceChanged",
                "deployChanged",
            ],
        );

        // Default fallback
        let minimal = r#"{"sourceChanged":true}"#;
        let parsed: CiConfigChange = serde_json::from_str(minimal).unwrap();
        assert!(parsed.source_changed);
        assert!(!parsed.deploy_changed);
        assert!(parsed.known_ci_files.is_empty());
        assert!(parsed.unknown_ci_files.is_empty());
        assert!(parsed.pre_commit_files.is_empty());
        assert!(parsed.generated_ci_files.is_empty());
    }

    #[test]
    fn test_nested_hotspot_field_order_and_default() {
        let h = Hotspot {
            path: PathBuf::from("src/main.rs"),
            score: 0.75,
            display_score: 0.75,
            complexity: 5,
            frequency: 1.0,
            centrality: Some(3),
        };
        let json = serde_json::to_string_pretty(&h).unwrap();
        assert_field_order(
            &json,
            2,
            &[
                "path",
                "score",
                "displayScore",
                "complexity",
                "frequency",
                "centrality",
            ],
        );
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_exact_keys(
            &v,
            &[
                "path",
                "score",
                "displayScore",
                "complexity",
                "frequency",
                "centrality",
            ],
        );

        // Omission when centrality is None
        let h2 = Hotspot {
            path: PathBuf::from("src/main.rs"),
            score: 0.75,
            display_score: 0.75,
            complexity: 5,
            frequency: 1.0,
            centrality: None,
        };
        let json2 = serde_json::to_string(&h2).unwrap();
        let v2: serde_json::Value = serde_json::from_str(&json2).unwrap();
        assert_exact_keys(
            &v2,
            &["path", "score", "displayScore", "complexity", "frequency"],
        );

        // Default fallback for display_score and centrality
        let minimal = r#"{"path":"src/main.rs","score":0.75,"complexity":5,"frequency":1.0}"#;
        let parsed: Hotspot = serde_json::from_str(minimal).unwrap();
        assert_eq!(parsed.display_score, 0.0);
        assert_eq!(parsed.centrality, None);

        // Custom deserializer: null score defaults to 0.0
        let null_score = r#"{"path":"src/main.rs","score":null,"complexity":5,"frequency":1.0}"#;
        let parsed2: Hotspot = serde_json::from_str(null_score).unwrap();
        assert_eq!(parsed2.score, 0.0);

        // Custom deserializer: integer score accepted
        let int_score = r#"{"path":"src/main.rs","score":5,"complexity":5,"frequency":1.0}"#;
        let parsed3: Hotspot = serde_json::from_str(int_score).unwrap();
        assert_eq!(parsed3.score, 5.0);
    }

    #[test]
    fn test_nested_relevant_decision_field_order_and_omission() {
        let rd = RelevantDecision {
            file_path: PathBuf::from("a.rs"),
            heading: Some("h".into()),
            excerpt: "e".into(),
            similarity: 0.5,
            rerank_score: Some(0.2),
            staleness_days: Some(1),
            staleness_tier: Some(StalenessTier::Warning),
        };
        let json = serde_json::to_string_pretty(&rd).unwrap();
        assert_field_order(
            &json,
            2,
            &[
                "filePath",
                "heading",
                "excerpt",
                "similarity",
                "rerankScore",
                "stalenessDays",
                "stalenessTier",
            ],
        );
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_exact_keys(
            &v,
            &[
                "filePath",
                "heading",
                "excerpt",
                "similarity",
                "rerankScore",
                "stalenessDays",
                "stalenessTier",
            ],
        );

        // Omission when optional fields are None
        let rd2 = RelevantDecision {
            file_path: PathBuf::from("a.rs"),
            heading: None,
            excerpt: "e".into(),
            similarity: 0.5,
            rerank_score: None,
            staleness_days: None,
            staleness_tier: None,
        };
        let json2 = serde_json::to_string(&rd2).unwrap();
        let v2: serde_json::Value = serde_json::from_str(&json2).unwrap();
        assert_exact_keys(&v2, &["filePath", "heading", "excerpt", "similarity"]);
    }

    #[test]
    fn test_nested_deploy_manifest_change_field_order_and_default() {
        let dmc = DeployManifestChange {
            file: PathBuf::from("Dockerfile"),
            manifest_type: ManifestType::Dockerfile,
            risk_tier: 2,
            coupled_files: vec!["a.rs".into()],
            high_blast_resources: vec!["cpu".into()],
            service_name: Some("svc".into()),
            owner: Some("team".into()),
        };
        let json = serde_json::to_string_pretty(&dmc).unwrap();
        assert_field_order(
            &json,
            2,
            &[
                "file",
                "manifestType",
                "riskTier",
                "coupledFiles",
                "highBlastResources",
                "serviceName",
                "owner",
            ],
        );
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_exact_keys(
            &v,
            &[
                "file",
                "manifestType",
                "riskTier",
                "coupledFiles",
                "highBlastResources",
                "serviceName",
                "owner",
            ],
        );

        // Omission when service_name/owner are None
        let dmc2 = DeployManifestChange {
            file: PathBuf::from("Dockerfile"),
            manifest_type: ManifestType::Dockerfile,
            risk_tier: 2,
            coupled_files: vec![],
            high_blast_resources: vec![],
            service_name: None,
            owner: None,
        };
        let json2 = serde_json::to_string(&dmc2).unwrap();
        let v2: serde_json::Value = serde_json::from_str(&json2).unwrap();
        assert_exact_keys(
            &v2,
            &[
                "file",
                "manifestType",
                "riskTier",
                "coupledFiles",
                "highBlastResources",
            ],
        );

        // Default fallback for service_name and owner
        let minimal = r#"{"file":"Dockerfile","manifestType":"Dockerfile","riskTier":2,"coupledFiles":[],"highBlastResources":[]}"#;
        let parsed: DeployManifestChange = serde_json::from_str(minimal).unwrap();
        assert_eq!(parsed.service_name, None);
        assert_eq!(parsed.owner, None);
    }

    #[test]
    fn test_nested_changed_file_default_fallback() {
        let minimal = r#"{"path":"src/main.rs","status":"Modified","isStaged":true}"#;
        let parsed: ChangedFile = serde_json::from_str(minimal).unwrap();
        assert_eq!(parsed.path, PathBuf::from("src/main.rs"));
        assert_eq!(parsed.status, "Modified");
        assert!(parsed.is_staged);
        assert_eq!(parsed.old_path, None);
        assert_eq!(parsed.runtime_usage, None);
        assert_eq!(parsed.analysis_status, FileAnalysisStatus::default());
        assert!(parsed.analysis_warnings.is_empty());
        assert!(parsed.api_routes.is_empty());
        assert!(parsed.data_models.is_empty());
        assert!(parsed.ci_gates.is_empty());

        // Omission test
        let cf = ChangedFile {
            path: PathBuf::from("a.rs"),
            status: "Added".into(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: vec![],
            api_routes: vec![],
            data_models: vec![],
            ci_gates: vec![],
        };
        let json = serde_json::to_string(&cf).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_exact_keys(
            &v,
            &[
                "path",
                "status",
                "isStaged",
                "symbols",
                "imports",
                "analysisStatus",
                "analysisWarnings",
                "apiRoutes",
                "dataModels",
                "ciGates",
            ],
        );
    }

    #[test]
    fn test_nested_service_default_fallback() {
        let minimal = r#"{"name":"svc","directory":"src/svc","routes":[],"data_models":[]}"#;
        let parsed: Service = serde_json::from_str(minimal).unwrap();
        assert_eq!(parsed.name, "svc");
        assert_eq!(parsed.directory, PathBuf::from("src/svc"));
        assert!(parsed.routes.is_empty());
        assert!(parsed.data_models.is_empty());
        assert!(parsed.owners.is_empty());
        assert_eq!(parsed.runtime_name, None);
        assert!(parsed.queues.is_empty());
        assert!(parsed.topics.is_empty());
        assert!(parsed.rpc_endpoints.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::AffectedContract;
    use crate::index::symbols::Symbol;
    use crate::observability::signal::{ObservabilitySignal, SignalSeverity};
    use rstest::rstest;
    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use std::path::PathBuf;

    fn roundtrip_json<T: Serialize + DeserializeOwned + Clone + PartialEq + std::fmt::Debug>(
        value: T,
    ) -> T {
        let json = serde_json::to_string(&value).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn test_packet_serialization() {
        let mut packet = ImpactPacket {
            timestamp_utc: "2023-10-27T10:00:00Z".to_string(),
            head_hash: Some("abcdef123456".to_string()),
            branch_name: Some("main".to_string()),
            ..ImpactPacket::default()
        };
        packet.changes.push(ChangedFile {
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
        });

        let json = serde_json::to_string_pretty(&packet).unwrap();

        // Assert schema version and camelCase
        assert!(json.contains(r#""schemaVersion": "v1""#));
        assert!(json.contains(r#""timestampUtc": "2023-10-27T10:00:00Z""#));
        assert!(json.contains(r#""headHash": "abcdef123456""#));
        assert!(json.contains(r#""isStaged": true"#));
    }

    #[test]
    fn test_deterministic_sorting() {
        let mut packet = ImpactPacket {
            risk_reasons: vec!["C".to_string(), "A".to_string(), "B".to_string()],
            ..ImpactPacket::default()
        };

        packet.changes.push(ChangedFile {
            path: PathBuf::from("z.rs"),
            status: "Added".to_string(),
            old_path: None,
            is_staged: true,
            symbols: Some(vec![
                Symbol {
                    name: "foo".into(),
                    kind: crate::index::symbols::SymbolKind::Function,
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
                },
                Symbol {
                    name: "bar".into(),
                    kind: crate::index::symbols::SymbolKind::Function,
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
                },
            ]),
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        });
        packet.changes.push(ChangedFile {
            path: PathBuf::from("a.rs"),
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

        packet.finalize();

        assert_eq!(packet.risk_reasons, vec!["A", "B", "C"]);
        assert_eq!(packet.changes[0].path, PathBuf::from("a.rs"));
        assert_eq!(packet.changes[1].path, PathBuf::from("z.rs"));

        let z_symbols = packet.changes[1].symbols.as_ref().unwrap();
        assert_eq!(z_symbols[0].name, "bar");
        assert_eq!(z_symbols[1].name, "foo");
    }

    fn relevant_decisions_without_staleness() -> Vec<RelevantDecision> {
        vec![
            RelevantDecision {
                file_path: PathBuf::from("docs/guide.md"),
                heading: Some("Introduction".to_string()),
                excerpt: "This guide explains...".to_string(),
                similarity: 0.85,
                rerank_score: Some(0.92),
                staleness_days: None,
                staleness_tier: None,
            },
            RelevantDecision {
                file_path: PathBuf::from("docs/api.md"),
                heading: None,
                excerpt: "API reference section".to_string(),
                similarity: 0.6,
                rerank_score: None,
                staleness_days: None,
                staleness_tier: None,
            },
        ]
    }

    fn relevant_decisions_with_staleness_populated() -> Vec<RelevantDecision> {
        vec![RelevantDecision {
            file_path: PathBuf::from("docs/old.md"),
            heading: Some("Legacy".to_string()),
            excerpt: "Old decision".to_string(),
            similarity: 0.75,
            rerank_score: None,
            staleness_days: Some(400),
            staleness_tier: Some(StalenessTier::Warning),
        }]
    }

    fn relevant_decisions_with_staleness_none() -> Vec<RelevantDecision> {
        vec![RelevantDecision {
            file_path: PathBuf::from("docs/new.md"),
            heading: None,
            excerpt: "New decision".to_string(),
            similarity: 0.5,
            rerank_score: None,
            staleness_days: None,
            staleness_tier: None,
        }]
    }

    fn observability_packet() -> ImpactPacket {
        ImpactPacket {
            observability: vec![ObservabilitySignal::new(
                "error_rate",
                "GET /api",
                0.15,
                SignalSeverity::Critical,
                "Error rate 15%",
                "prometheus",
            )],
            changes: vec![ChangedFile {
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
            }],
            ..ImpactPacket::default()
        }
    }

    fn affected_contracts_packet() -> ImpactPacket {
        ImpactPacket {
            affected_contracts: vec![AffectedContract {
                endpoint_id: "api/openapi.json::GET::/pets".to_string(),
                path: "/pets".to_string(),
                method: "GET".to_string(),
                summary: "List all pets".to_string(),
                similarity: 0.85,
                spec_file: "api/openapi.json".to_string(),
            }],
            ..ImpactPacket::default()
        }
    }

    fn ci_config_change_packet() -> ImpactPacket {
        ImpactPacket {
            ci_config_change: Some(CiConfigChange {
                known_ci_files: vec![".github/workflows/ci.yml".to_string()],
                unknown_ci_files: vec!["ci/deploy.sh".to_string()],
                pre_commit_files: vec![".pre-commit-config.yaml".to_string()],
                generated_ci_files: vec![".github/workflows/generated.yml".to_string()],
                source_changed: true,
                deploy_changed: false,
            }),
            ..ImpactPacket::default()
        }
    }

    fn dead_code_finding_packet() -> ImpactPacket {
        ImpactPacket {
            dead_code_findings: vec![DeadCodeFinding {
                symbol_name: "unused_fn".to_string(),
                file_path: PathBuf::from("src/lib.rs"),
                confidence: 0.92,
                factors: vec![
                    ConfidenceFactor::UnreachableFromEntrypoints,
                    ConfidenceFactor::GitInactive {
                        days_since_last_commit: 120,
                    },
                    ConfidenceFactor::NoTestCoverage,
                ],
                recommendation: "Consider removing or adding tests".to_string(),
                line_start: None,
                line_end: None,
            }],
            ..ImpactPacket::default()
        }
    }

    #[rstest]
    #[case::relevant_decisions_without_staleness({
        let packet = ImpactPacket {
            relevant_decisions: relevant_decisions_without_staleness(),
            ..ImpactPacket::default()
        };
        let json = serde_json::to_string_pretty(&packet).unwrap();
        assert!(json.contains("relevantDecisions"));
        assert!(json.contains("docs/guide.md"));
        assert!(json.contains("rerankScore"));
        packet
    })]
    #[case::relevant_decisions_with_staleness_populated({
        let packet = ImpactPacket {
            relevant_decisions: relevant_decisions_with_staleness_populated(),
            ..ImpactPacket::default()
        };
        let json = serde_json::to_string_pretty(&packet).unwrap();
        assert!(json.contains("stalenessDays"));
        assert!(json.contains("stalenessTier"));
        assert!(json.contains("warning"));
        packet
    })]
    #[case::relevant_decisions_with_staleness_none({
        let packet = ImpactPacket {
            relevant_decisions: relevant_decisions_with_staleness_none(),
            ..ImpactPacket::default()
        };
        let json = serde_json::to_string_pretty(&packet).unwrap();
        assert!(!json.contains("stalenessDays"));
        assert!(!json.contains("stalenessTier"));
        packet
    })]
    #[case::observability(observability_packet())]
    #[case::affected_contracts(affected_contracts_packet())]
    #[case::ci_config_change(ci_config_change_packet())]
    #[case::dead_code_finding(dead_code_finding_packet())]
    fn packet_serialization_roundtrips(#[case] value: ImpactPacket) {
        let expected = value.clone();
        let back = roundtrip_json(value);
        assert_eq!(back, expected, "roundtrip should preserve ImpactPacket");
    }

    #[test]
    fn test_relevant_decision_empty_absent_from_json() {
        let packet = ImpactPacket::default();
        let json = serde_json::to_string_pretty(&packet).unwrap();
        assert!(!json.contains("relevantDecisions"));
    }

    #[test]
    fn test_finalize_sorts_relevant_decisions_descending() {
        let mut packet = ImpactPacket {
            relevant_decisions: vec![
                RelevantDecision {
                    file_path: PathBuf::from("docs/c.md"),
                    heading: None,
                    excerpt: "C".to_string(),
                    similarity: 0.5,
                    rerank_score: None,
                    staleness_days: None,
                    staleness_tier: None,
                },
                RelevantDecision {
                    file_path: PathBuf::from("docs/a.md"),
                    heading: None,
                    excerpt: "A".to_string(),
                    similarity: 0.9,
                    rerank_score: None,
                    staleness_days: None,
                    staleness_tier: None,
                },
                RelevantDecision {
                    file_path: PathBuf::from("docs/b.md"),
                    heading: None,
                    excerpt: "B".to_string(),
                    similarity: 0.5,
                    rerank_score: None,
                    staleness_days: None,
                    staleness_tier: None,
                },
            ],
            ..ImpactPacket::default()
        };

        packet.finalize();

        // Sorted descending by similarity, then by file_path for ties
        assert_eq!(packet.relevant_decisions[0].similarity, 0.9);
        assert_eq!(
            packet.relevant_decisions[0].file_path,
            PathBuf::from("docs/a.md")
        );
        // Tie at 0.5: b.md < c.md alphabetically
        assert_eq!(packet.relevant_decisions[1].similarity, 0.5);
        assert_eq!(
            packet.relevant_decisions[1].file_path,
            PathBuf::from("docs/b.md")
        );
        assert_eq!(packet.relevant_decisions[2].similarity, 0.5);
        assert_eq!(
            packet.relevant_decisions[2].file_path,
            PathBuf::from("docs/c.md")
        );
    }

    #[test]
    fn test_truncate_for_context_clears_relevant_decisions() {
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
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
            }],
            relevant_decisions: vec![RelevantDecision {
                file_path: PathBuf::from("docs/a.md"),
                heading: Some("Intro".to_string()),
                excerpt: "Content".to_string(),
                similarity: 0.9,
                rerank_score: None,
                staleness_days: None,
                staleness_tier: None,
            }],
            ..ImpactPacket::default()
        };

        // Truncate with a very small target to force Phase 3 clearing
        let truncated = packet.truncate_for_context(100);
        assert!(truncated);
        assert!(packet.relevant_decisions.is_empty());
    }

    #[test]
    fn test_observability_sorted_by_severity_in_finalize() {
        use crate::observability::signal::{ObservabilitySignal, SignalSeverity};

        let mut packet = ImpactPacket {
            observability: vec![
                ObservabilitySignal::new(
                    "metric",
                    "label-a",
                    1.0,
                    SignalSeverity::Normal,
                    "normal",
                    "source",
                ),
                ObservabilitySignal::new(
                    "metric",
                    "label-b",
                    1.0,
                    SignalSeverity::Critical,
                    "critical",
                    "source",
                ),
                ObservabilitySignal::new(
                    "metric",
                    "label-c",
                    1.0,
                    SignalSeverity::Warning,
                    "warning",
                    "source",
                ),
            ],
            ..ImpactPacket::default()
        };

        packet.finalize();

        assert_eq!(packet.observability[0].severity, SignalSeverity::Critical);
        assert_eq!(packet.observability[1].severity, SignalSeverity::Warning);
        assert_eq!(packet.observability[2].severity, SignalSeverity::Normal);
    }

    #[test]
    fn test_observability_cleared_in_truncate_phase_3() {
        use crate::observability::signal::{ObservabilitySignal, SignalSeverity};

        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
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
            }],
            observability: vec![ObservabilitySignal::new(
                "error_rate",
                "svc",
                0.15,
                SignalSeverity::Critical,
                "Error rate high",
                "prometheus",
            )],
            temporal_couplings: vec![TemporalCoupling {
                file_a: PathBuf::from("src/a.rs"),
                file_b: PathBuf::from("src/b.rs"),
                score: 0.9,
            }],
            ..ImpactPacket::default()
        };

        // Truncate with very small target to push through to Phase 3
        let truncated = packet.truncate_for_context(100);
        assert!(truncated);
        assert!(packet.observability.is_empty());
    }

    #[test]
    fn test_observability_empty_absent_from_json() {
        let packet = ImpactPacket::default();
        let json = serde_json::to_string_pretty(&packet).unwrap();
        assert!(!json.contains("observability"));
    }

    #[test]
    fn test_affected_contracts_empty_absent_from_json() {
        let packet = ImpactPacket::default();
        let json = serde_json::to_string_pretty(&packet).unwrap();
        assert!(!json.contains("affectedContracts"));
    }

    #[test]
    fn test_finalize_sorts_affected_contracts() {
        let mut packet = ImpactPacket {
            affected_contracts: vec![
                AffectedContract {
                    endpoint_id: "c".to_string(),
                    path: "/pets".to_string(),
                    method: "GET".to_string(),
                    summary: "".to_string(),
                    similarity: 0.5,
                    spec_file: "api.yaml".to_string(),
                },
                AffectedContract {
                    endpoint_id: "a".to_string(),
                    path: "/users".to_string(),
                    method: "POST".to_string(),
                    summary: "".to_string(),
                    similarity: 0.9,
                    spec_file: "api.yaml".to_string(),
                },
                AffectedContract {
                    endpoint_id: "b".to_string(),
                    path: "/items".to_string(),
                    method: "GET".to_string(),
                    summary: "".to_string(),
                    similarity: 0.5,
                    spec_file: "api.yaml".to_string(),
                },
            ],
            ..ImpactPacket::default()
        };

        packet.finalize();

        assert_eq!(packet.affected_contracts[0].similarity, 0.9);
        assert_eq!(packet.affected_contracts[1].similarity, 0.5);
        assert_eq!(packet.affected_contracts[2].similarity, 0.5);
        // Ties sorted by path ascending
        assert_eq!(packet.affected_contracts[1].path, "/items");
        assert_eq!(packet.affected_contracts[2].path, "/pets");
    }

    #[test]
    fn test_truncate_clears_affected_contracts() {
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
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
            }],
            affected_contracts: vec![AffectedContract {
                endpoint_id: "a".to_string(),
                path: "/pets".to_string(),
                method: "GET".to_string(),
                summary: "List pets".to_string(),
                similarity: 0.9,
                spec_file: "api.yaml".to_string(),
            }],
            temporal_couplings: vec![TemporalCoupling {
                file_a: PathBuf::from("src/a.rs"),
                file_b: PathBuf::from("src/b.rs"),
                score: 0.9,
            }],
            ..ImpactPacket::default()
        };

        let truncated = packet.truncate_for_context(100);
        assert!(truncated);
        assert!(packet.affected_contracts.is_empty());
    }

    #[test]
    fn test_truncate_clears_service_map_delta() {
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
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
            }],
            service_map_delta: Some(ServiceMapDelta {
                services: vec![
                    Service {
                        name: "users".to_string(),
                        directory: PathBuf::from("services/users"),
                        routes: vec!["/api/v1/users".to_string()],
                        data_models: vec!["User".to_string()],
                        owners: Vec::new(),
                        runtime_name: None,
                        queues: Vec::new(),
                        topics: Vec::new(),
                        rpc_endpoints: Vec::new(),
                    },
                    Service {
                        name: "orders".to_string(),
                        directory: PathBuf::from("services/orders"),
                        routes: vec!["/api/v1/orders".to_string()],
                        data_models: vec!["Order".to_string()],
                        owners: Vec::new(),
                        runtime_name: None,
                        queues: Vec::new(),
                        topics: Vec::new(),
                        rpc_endpoints: Vec::new(),
                    },
                ],
                affected_services: vec!["users".to_string(), "orders".to_string()],
                cross_service_edges: vec![("orders".to_string(), "users".to_string(), 3)],
                total_services: 2,
            }),
            temporal_couplings: vec![TemporalCoupling {
                file_a: PathBuf::from("src/a.rs"),
                file_b: PathBuf::from("src/b.rs"),
                score: 0.9,
            }],
            ..ImpactPacket::default()
        };

        let truncated = packet.truncate_for_context(100);
        assert!(truncated);
        assert!(packet.service_map_delta.is_none());
    }

    #[test]
    fn test_truncate_preserves_service_map_delta_when_budget_not_exceeded() {
        let mut packet = ImpactPacket {
            service_map_delta: Some(ServiceMapDelta {
                services: vec![Service {
                    name: "users".to_string(),
                    directory: PathBuf::from("services/users"),
                    routes: vec!["/api/v1/users".to_string()],
                    data_models: vec!["User".to_string()],
                    owners: Vec::new(),
                    runtime_name: None,
                    queues: Vec::new(),
                    topics: Vec::new(),
                    rpc_endpoints: Vec::new(),
                }],
                affected_services: vec!["users".to_string()],
                cross_service_edges: Vec::new(),
                total_services: 1,
            }),
            ..ImpactPacket::default()
        };

        let truncated = packet.truncate_for_context(1_000_000);
        assert!(!truncated);
        assert!(packet.service_map_delta.is_some());
    }

    #[test]
    fn test_finalize_sorts_data_flow_matches() {
        let mut packet = ImpactPacket {
            data_flow_matches: vec![
                DataFlowMatch {
                    chain_label: "low".to_string(),
                    changed_nodes: vec![],
                    total_nodes: 2,
                    change_pct: 0.1,
                    risk: RiskLevel::Low,
                },
                DataFlowMatch {
                    chain_label: "high".to_string(),
                    changed_nodes: vec![],
                    total_nodes: 2,
                    change_pct: 0.9,
                    risk: RiskLevel::High,
                },
                DataFlowMatch {
                    chain_label: "mid".to_string(),
                    changed_nodes: vec![],
                    total_nodes: 2,
                    change_pct: 0.5,
                    risk: RiskLevel::Medium,
                },
            ],
            ..ImpactPacket::default()
        };

        packet.finalize();

        assert_eq!(packet.data_flow_matches[0].change_pct, 0.9);
        assert_eq!(packet.data_flow_matches[1].change_pct, 0.5);
        assert_eq!(packet.data_flow_matches[2].change_pct, 0.1);
    }

    #[test]
    fn test_truncate_clears_data_flow_matches() {
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
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
            }],
            data_flow_matches: vec![DataFlowMatch {
                chain_label: "test".to_string(),
                changed_nodes: vec![],
                total_nodes: 2,
                change_pct: 0.5,
                risk: RiskLevel::Medium,
            }],
            ..ImpactPacket::default()
        };

        let truncated = packet.truncate_for_context(100);
        assert!(truncated);
        assert!(packet.data_flow_matches.is_empty());
    }

    #[test]
    fn test_finalize_sorts_deploy_manifest_changes_by_risk_tier_descending() {
        let mut packet = ImpactPacket {
            deploy_manifest_changes: vec![
                DeployManifestChange {
                    file: PathBuf::from("Dockerfile"),
                    manifest_type: ManifestType::Dockerfile,
                    risk_tier: 1,
                    coupled_files: Vec::new(),
                    high_blast_resources: Vec::new(),
                    service_name: None,
                    owner: None,
                },
                DeployManifestChange {
                    file: PathBuf::from("main.tf"),
                    manifest_type: ManifestType::Terraform,
                    risk_tier: 3,
                    coupled_files: Vec::new(),
                    high_blast_resources: Vec::new(),
                    service_name: None,
                    owner: None,
                },
                DeployManifestChange {
                    file: PathBuf::from("docker-compose.yml"),
                    manifest_type: ManifestType::DockerCompose,
                    risk_tier: 2,
                    coupled_files: Vec::new(),
                    high_blast_resources: Vec::new(),
                    service_name: None,
                    owner: None,
                },
            ],
            ..ImpactPacket::default()
        };

        packet.finalize();

        assert_eq!(packet.deploy_manifest_changes[0].risk_tier, 3);
        assert_eq!(
            packet.deploy_manifest_changes[0].file,
            PathBuf::from("main.tf")
        );
        assert_eq!(packet.deploy_manifest_changes[1].risk_tier, 2);
        assert_eq!(
            packet.deploy_manifest_changes[1].file,
            PathBuf::from("docker-compose.yml")
        );
        assert_eq!(packet.deploy_manifest_changes[2].risk_tier, 1);
        assert_eq!(
            packet.deploy_manifest_changes[2].file,
            PathBuf::from("Dockerfile")
        );
    }

    #[test]
    fn test_ci_config_change_empty_absent_from_json() {
        let packet = ImpactPacket::default();
        let json = serde_json::to_string_pretty(&packet).unwrap();
        assert!(!json.contains("ciConfigChange"));
    }

    #[test]
    fn test_truncate_clears_ci_config_change() {
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
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
            }],
            ci_config_change: Some(CiConfigChange {
                known_ci_files: vec![".github/workflows/ci.yml".to_string()],
                unknown_ci_files: Vec::new(),
                pre_commit_files: Vec::new(),
                generated_ci_files: Vec::new(),
                source_changed: false,
                deploy_changed: false,
            }),
            ..ImpactPacket::default()
        };

        let truncated = packet.truncate_for_context(100);
        assert!(truncated);
        assert!(packet.ci_config_change.is_none());
    }

    #[test]
    fn test_dead_code_findings_empty_absent_from_json() {
        let packet = ImpactPacket::default();
        let json = serde_json::to_string_pretty(&packet).unwrap();
        assert!(!json.contains("deadCodeFindings"));
    }

    #[test]
    fn test_finalize_sorts_dead_code_findings() {
        let mut packet = ImpactPacket {
            dead_code_findings: vec![
                DeadCodeFinding {
                    symbol_name: "c".to_string(),
                    file_path: PathBuf::from("src/z.rs"),
                    confidence: 0.5,
                    factors: vec![ConfidenceFactor::NoTestCoverage],
                    recommendation: "R1".to_string(),
                    line_start: None,
                    line_end: None,
                },
                DeadCodeFinding {
                    symbol_name: "a".to_string(),
                    file_path: PathBuf::from("src/a.rs"),
                    confidence: 0.9,
                    factors: vec![ConfidenceFactor::UnreachableFromEntrypoints],
                    recommendation: "R2".to_string(),
                    line_start: None,
                    line_end: None,
                },
                DeadCodeFinding {
                    symbol_name: "b".to_string(),
                    file_path: PathBuf::from("src/a.rs"),
                    confidence: 0.9,
                    factors: vec![ConfidenceFactor::NoTestCoverage],
                    recommendation: "R3".to_string(),
                    line_start: None,
                    line_end: None,
                },
            ],
            ..ImpactPacket::default()
        };

        packet.finalize();

        // Sorted by confidence descending, then path ascending, then symbol ascending
        assert!((packet.dead_code_findings[0].confidence - 0.9).abs() < 1e-6);
        assert_eq!(packet.dead_code_findings[0].symbol_name, "a");
        assert!((packet.dead_code_findings[1].confidence - 0.9).abs() < 1e-6);
        assert_eq!(packet.dead_code_findings[1].symbol_name, "b");
        assert!((packet.dead_code_findings[2].confidence - 0.5).abs() < 1e-6);
        assert_eq!(packet.dead_code_findings[2].symbol_name, "c");
    }

    #[test]
    fn test_truncate_clears_dead_code_findings() {
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
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
            }],
            dead_code_findings: vec![DeadCodeFinding {
                symbol_name: "unused".to_string(),
                file_path: PathBuf::from("src/main.rs"),
                confidence: 0.8,
                factors: vec![ConfidenceFactor::NoTestCoverage],
                recommendation: "Remove".to_string(),
                line_start: None,
                line_end: None,
            }],
            ..ImpactPacket::default()
        };

        let truncated = packet.truncate_for_context(100);
        assert!(truncated);
        assert!(packet.dead_code_findings.is_empty());
    }
}

/// Compatibility smoke-test: verify the public facade re-exports every domain type.
#[cfg(test)]
mod facade_compat_tests {
    use crate::impact::packet::*;

    #[test]
    fn test_public_facade_imports_work() {
        // Core packet metadata
        let _ = ImpactPacket::default();

        // Changed file domain
        let _ = ChangedFile::default();
        let _ = AnalysisStatus::Ok;
        let _ = FileAnalysisStatus::default();

        // Coverage domain
        let _ = CoverageDelta {
            file_path: "a.rs".to_string(),
            pattern_kind: "log".to_string(),
            previous_count: 0,
            current_count: 1,
            message: "m".to_string(),
        };
        let _ = CoveringTest {
            test_file: "t.rs".to_string(),
            test_symbol: "test".to_string(),
            confidence: 0.8,
            mapping_kind: "direct".to_string(),
        };
        let _ = TestCoverage {
            changed_symbol: "s".to_string(),
            changed_file: "f.rs".to_string(),
            covering_tests: vec![],
        };
        let _ = CallChainNode {
            symbol: "s".to_string(),
            file_path: std::path::PathBuf::from("f.rs"),
            is_data_model: false,
            is_external: false,
        };
        let _ = CallChain {
            nodes: vec![],
            has_cycle: false,
        };
        let _ = RuntimeUsageDelta {
            file_path: "f.rs".to_string(),
            env_vars_previous_count: 0,
            env_vars_current_count: 1,
            config_keys_previous_count: 0,
            config_keys_current_count: 1,
            env_vars_previous: vec![],
            env_vars_current: vec!["VAR".to_string()],
        };
        let _ = TraceConfigType::OpenTelemetryCollector;
        let _ = TraceConfigChange {
            file: std::path::PathBuf::from("otel.yml"),
            config_type: TraceConfigType::JaegerAgent,
            risk_weight: 1,
            is_deleted: false,
        };
        let _ = TraceEnvVarChange {
            var_name: "TRACE".to_string(),
            pattern: "p".to_string(),
            risk_weight: 1,
        };
        let _ = SdkDependencyDelta {
            added: vec![SdkDependency {
                sdk_name: "sdk".to_string(),
                file_path: std::path::PathBuf::from("f.rs"),
                import_statement: "use sdk;".to_string(),
            }],
            removed: vec![],
            modified: vec![],
        };
        let _ = ManifestType::Dockerfile;
        let _ = DataFlowMatch {
            chain_label: "l".to_string(),
            changed_nodes: vec![],
            total_nodes: 2,
            change_pct: 0.5,
            risk: RiskLevel::Medium,
        };
        let _ = DeployManifestChange {
            file: std::path::PathBuf::from("Dockerfile"),
            manifest_type: ManifestType::Dockerfile,
            risk_tier: 1,
            coupled_files: vec![],
            high_blast_resources: vec![],
            service_name: None,
            owner: None,
        };

        // Intelligence domain
        let _ = RelevantDecision {
            file_path: std::path::PathBuf::from("d.md"),
            heading: None,
            excerpt: "e".to_string(),
            similarity: 0.5,
            rerank_score: None,
            staleness_days: None,
            staleness_tier: None,
        };
        let _ = Hotspot {
            path: std::path::PathBuf::from("h.rs"),
            score: 0.5,
            display_score: 0.5,
            complexity: 1,
            frequency: 1.0,
            centrality: None,
        };
        let _ = StalenessTier::Warning;
        let _ = AiInsight {
            memory_id: "mid".to_string(),
            relevance: 0.5,
            content: "c".to_string(),
        };
        let _ = KGImpact {
            source_node: "s".to_string(),
            source_category: "cat".to_string(),
            impacted_node: "i".to_string(),
            impacted_category: "cat".to_string(),
            relation: "r".to_string(),
            path_length: 1,
            reason: "r".to_string(),
        };
        let _ = ConfidenceFactor::NoTestCoverage;
        let _ = ConfidenceFactor::GitInactive {
            days_since_last_commit: 30,
        };
        let _ = DeadCodeFinding {
            symbol_name: "unused".to_string(),
            file_path: std::path::PathBuf::from("u.rs"),
            confidence: 0.8,
            factors: vec![ConfidenceFactor::NoTestCoverage],
            recommendation: "del".to_string(),
            line_start: None,
            line_end: None,
        };

        // Risk domain
        let _ = RiskLevel::Low;
        let _ = TemporalCoupling {
            file_a: std::path::PathBuf::from("a.rs"),
            file_b: std::path::PathBuf::from("b.rs"),
            score: 0.5,
        };
        let _ = StructuralCoupling {
            caller_symbol_name: "x".to_string(),
            callee_symbol_name: "y".to_string(),
            caller_file_path: std::path::PathBuf::from("x.rs"),
        };
        let _ = CentralityRisk {
            symbol_name: "main".to_string(),
            entrypoints_reachable: 1,
        };
        let _ = RiskImpact {
            weight: 1,
            reasons: vec!["r".to_string()],
        };

        // Surfaces domain
        let _ = DataModel {
            model_name: "M".to_string(),
            model_kind: "struct".to_string(),
            confidence: 0.9,
            evidence: None,
        };
        let _ = ApiRoute {
            method: "GET".to_string(),
            path_pattern: "/api".to_string(),
            handler_symbol_name: None,
            framework: "axum".to_string(),
            route_source: "file".to_string(),
            mount_prefix: None,
            is_dynamic: false,
            route_confidence: 1.0,
            evidence: "e".to_string(),
            auth_requirements: None,
            schema_refs: None,
            owning_service: None,
            consumers: None,
        };
        let _ = ServiceMapDelta {
            services: vec![],
            affected_services: vec![],
            cross_service_edges: vec![],
            total_services: 0,
        };
        let _ = Service {
            name: "s".to_string(),
            directory: std::path::PathBuf::from("svc"),
            routes: vec![],
            data_models: vec![],
            owners: vec![],
            runtime_name: None,
            queues: vec![],
            topics: vec![],
            rpc_endpoints: vec![],
        };

        // Verification domain
        let _ = VerificationResult {
            name: "fmt".to_string(),
            command: "cargo fmt".to_string(),
            exit_code: 0,
            stdout: "".to_string(),
            stderr: "".to_string(),
            duration_ms: 0,
            truncated: false,
        };
        let _ = CiConfigChange {
            known_ci_files: vec![],
            unknown_ci_files: vec![],
            pre_commit_files: vec![],
            generated_ci_files: vec![],
            source_changed: false,
            deploy_changed: false,
        };
        let _ = CIPrediction {
            job_name: "ci".to_string(),
            platform: "gh".to_string(),
            failure_probability: 0.0,
            explanation: None,
        };
        let _ = CIGate {
            platform: "github".to_string(),
            job_name: "ci".to_string(),
            trigger: None,
            workflow_name: None,
            environment: None,
            artifacts: vec![],
            release_gates: vec![],
        };
    }
}
