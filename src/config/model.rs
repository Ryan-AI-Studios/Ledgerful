mod ask;
mod bridge;
mod coverage;
mod env;
mod federation;
mod gate;
mod gemini;
mod ledger;
mod local_model;
mod root;
mod semantic;
mod sync;
mod verify;

pub use self::ask::*;
pub use self::bridge::*;
pub use self::coverage::*;
pub use self::federation::*;
pub use self::gate::*;
pub use self::gemini::*;
pub use self::ledger::*;
pub use self::local_model::*;
pub use self::root::*;
pub use self::semantic::*;
pub use self::sync::*;
pub use self::verify::*;

// Implementation helpers stay internal (pub(crate)) so the facade does not
// leak env-resolution internals as part of the public API.
pub(crate) use self::env::read_env_key;
pub(crate) use self::env::resolve_bridge_config;
pub(crate) use self::env::resolve_local_model_config;

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Env isolation helpers
    // ------------------------------------------------------------------

    // ------------------------------------------------------------------
    // Config-level integration tests
    // ------------------------------------------------------------------

    #[test]
    fn test_config_defaults() {
        let config = Config::default();
        assert!(!config.core.strict);
        assert_eq!(config.watch.debounce_ms, 1000);
        assert!(
            config
                .watch
                .ignore_patterns
                .contains(&"target/**".to_string())
        );
        assert_eq!(config.gate.mode, "observe");
    }

    #[test]
    fn test_ask_config_default_off() {
        // DX6: auto_scan_default must default to false so the cached-packet
        // behavior is preserved unless the user opts in.
        let config = Config::default();
        assert!(!config.ask.auto_scan_default);
    }

    #[test]
    fn test_ask_config_deserialization() {
        let toml_str = r#"
            [ask]
            auto_scan_default = true
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.ask.auto_scan_default);

        // Omitting the section entirely must yield the default (false).
        let config: Config = toml::from_str("").unwrap();
        assert!(!config.ask.auto_scan_default);
    }

    #[test]
    fn test_federation_config_default_off() {
        // TA31 R2: auto_sync_siblings must default to false so the scanner's
        // existing missing-schema-skips-silently behavior is preserved
        // unless the user opts in.
        let config = Config::default();
        assert!(!config.federation.auto_sync_siblings);
    }

    #[test]
    fn test_federation_config_deserialization() {
        let toml_str = r#"
            [federation]
            auto_sync_siblings = true
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.federation.auto_sync_siblings);

        // Omitting the section entirely must yield the default (false).
        let config: Config = toml::from_str("").unwrap();
        assert!(!config.federation.auto_sync_siblings);
    }

    #[test]
    fn test_config_deserialization() {
        let toml_str = r#"
            [core]
            strict = true
            [watch]
            debounce_ms = 500
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.core.strict);
        assert_eq!(config.watch.debounce_ms, 500);
    }

    #[test]
    fn test_temporal_config_deserialization() {
        let toml_str = r#"
            [temporal]
            max_commits = 500
            max_files_per_commit = 30
            coupling_threshold = 0.5
            min_shared_commits = 4
            min_revisions = 8
            decay_half_life = 50
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.temporal.max_commits, 500);
        assert_eq!(config.temporal.max_files_per_commit, 30);
        assert!((config.temporal.coupling_threshold - 0.5).abs() < f32::EPSILON);
        assert_eq!(config.temporal.min_shared_commits, 4);
        assert_eq!(config.temporal.min_revisions, 8);
        assert_eq!(config.temporal.decay_half_life, 50);
    }

    #[test]
    fn test_verify_config_deserialization() {
        let toml_str = r#"
            [verify]
            default_timeout_secs = 120

            [[verify.steps]]
            description = "Run unit tests"
            command = "cargo test"
            timeout_secs = 60

            [[verify.steps]]
            description = "Check formatting"
            command = "cargo fmt --check"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.verify.default_timeout_secs, 120);
        assert_eq!(config.verify.steps.len(), 2);
        assert_eq!(config.verify.steps[0].description, "Run unit tests");
        assert_eq!(config.verify.steps[0].command, "cargo test");
        assert_eq!(config.verify.steps[0].timeout_secs, Some(60));
        assert_eq!(config.verify.steps[1].description, "Check formatting");
        assert_eq!(config.verify.steps[1].command, "cargo fmt --check");
        // Omitted timeout_secs should deserialize as None (uses default_timeout_secs)
        assert_eq!(config.verify.steps[1].timeout_secs, None);
        // semantic_weight not specified → uses default 0.3
        assert!((config.verify.semantic_weight - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn test_ledger_config_deserialization() {
        let toml_str = r#"
            [ledger]
            enforcement_enabled = true
            verify_to_commit = true
            auto_reconcile = false
            stale_threshold_hours = 48

            [[ledger.watcher_patterns]]
            glob = "**/Cargo.toml"
            category = "INFRA"

            [[ledger.category_mappings]]
            ledger_category = "ARCHITECTURE"
            stack_category = "BACKEND_LANG"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.ledger.enforcement_enabled);
        assert!(config.ledger.verify_to_commit);
        assert!(!config.ledger.auto_reconcile);
        assert_eq!(config.ledger.stale_threshold_hours, 48);
        assert_eq!(config.ledger.watcher_patterns.len(), 1);
        assert_eq!(config.ledger.watcher_patterns[0].glob, "**/Cargo.toml");
        assert_eq!(config.ledger.watcher_patterns[0].category, "INFRA");
        assert_eq!(config.ledger.category_mappings.len(), 1);
        assert_eq!(
            config.ledger.category_mappings[0].ledger_category,
            "ARCHITECTURE"
        );
        assert_eq!(
            config.ledger.category_mappings[0].stack_category,
            "BACKEND_LANG"
        );
    }

    #[test]
    fn test_config_includes_new_sections() {
        let config = Config::default();
        assert_eq!(config.local_model.base_url, "");
        assert_eq!(config.semantic.hnsw_rebuild_threshold(), 500);
        assert_eq!(config.docs.chunk_tokens, 512);
        assert_eq!(config.observability.error_rate_threshold, 0.05);
        assert!(config.contracts.spec_paths.is_empty());
    }

    #[test]
    fn test_local_model_config_deserialization() {
        let toml_str = r#"
            [local_model]
            base_url = "http://localhost:11434"
            embedding_model = "nomic-embed-text"
            dimensions = 768
            timeout_secs = 120
            prefer_local = true
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.local_model.base_url, "http://localhost:11434");
        assert_eq!(config.local_model.embedding_model, "nomic-embed-text");
        assert_eq!(config.local_model.dimensions, 768);
        assert_eq!(config.local_model.timeout_secs, 120);
        assert!(config.local_model.prefer_local);
        // Fields not specified should have defaults
        assert_eq!(config.local_model.context_window, 38000);
        assert_eq!(config.local_model.generation_model, "");
    }

    #[test]
    fn test_semantic_config_deserialization() {
        let toml_str = r#"
            [semantic]
            hnsw_rebuild_threshold = 64
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.semantic.hnsw_rebuild_threshold(), 64);
    }

    #[test]
    fn test_semantic_config_concurrency_toml() {
        let toml_str = r#"
            [semantic]
            concurrency = 8
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.semantic.concurrency, Some(8));
    }

    #[test]
    fn test_semantic_config_concurrency_default_none() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.semantic.concurrency, None);
    }

    #[test]
    fn test_semantic_config_default_threshold() {
        let config = Config::default();
        assert_eq!(
            config.semantic.hnsw_rebuild_threshold(),
            DEFAULT_HNSW_REBUILD_THRESHOLD
        );
    }

    #[test]
    fn test_docs_config_deserialization() {
        let toml_str = r#"
            [docs]
            include = ["README.md", "docs/"]
            chunk_tokens = 1024
            chunk_overlap = 128
            retrieval_top_k = 10
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.docs.include, vec!["README.md", "docs/"]);
        assert_eq!(config.docs.chunk_tokens, 1024);
        assert_eq!(config.docs.chunk_overlap, 128);
        assert_eq!(config.docs.retrieval_top_k, 10);
    }

    #[test]
    fn test_observability_config_deserialization() {
        let toml_str = r#"
            [observability]
            prometheus_url = "http://localhost:9090"
            error_rate_threshold = 0.1
            log_lookback_secs = 7200
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.observability.prometheus_url, "http://localhost:9090");
        assert!((config.observability.error_rate_threshold - 0.1).abs() < f32::EPSILON);
        assert_eq!(config.observability.log_lookback_secs, 7200);
        assert!(config.observability.service_map.is_empty());
        assert!(config.observability.log_paths.is_empty());
    }

    #[test]
    fn test_contracts_config_deserialization() {
        let toml_str = r#"
            [contracts]
            spec_paths = ["openapi.yaml", "proto/"]
            match_threshold = 0.7
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.contracts.spec_paths, vec!["openapi.yaml", "proto/"]);
        assert!((config.contracts.match_threshold - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn test_coverage_config_deserialization() {
        let toml_str = r#"
            [coverage]
            enabled = true
            kg_timeout_secs = 120

            [coverage.traces]
            enabled = true
            config_patterns = ["**/otel*.yaml"]
            env_var_patterns = ["OTEL_*"]

            [coverage.sdk]
            enabled = true
            patterns = ["stripe", "auth0"]
            risk_weight_new = 10
            risk_weight_modified = 5
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.coverage.enabled);
        assert_eq!(config.coverage.kg_timeout_secs, 120);
        assert!(config.coverage.traces.enabled);
        assert_eq!(
            config.coverage.traces.config_patterns,
            vec!["**/otel*.yaml"]
        );
        assert_eq!(config.coverage.traces.env_var_patterns, vec!["OTEL_*"]);
        assert!(config.coverage.sdk.enabled);
        assert_eq!(config.coverage.sdk.patterns, vec!["stripe", "auth0"]);
        assert_eq!(config.coverage.sdk.risk_weight_new, 10);
        assert_eq!(config.coverage.sdk.risk_weight_modified, 5);
        // Other sections should have defaults
        assert!(!config.coverage.services.enabled);
        assert_eq!(config.coverage.adr_staleness.threshold_days, 365);
    }

    #[test]
    fn test_dead_code_config_deserialization() {
        let toml_str = r#"
            [dead_code]
            enabled = true
            confidence_threshold = 0.8
            git_inactivity_days = 60
            reachability_weight = 2.0
            git_activity_weight = 1.5
            test_coverage_weight = 0.5
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.dead_code.enabled);
        assert!((config.dead_code.confidence_threshold - 0.8).abs() < f64::EPSILON);
        assert_eq!(config.dead_code.git_inactivity_days, 60);
        assert!((config.dead_code.reachability_weight - 2.0).abs() < f64::EPSILON);
        assert!((config.dead_code.git_activity_weight - 1.5).abs() < f64::EPSILON);
        assert!((config.dead_code.test_coverage_weight - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_config_backward_compat_without_dead_code() {
        let toml_str = r#"
            [core]
            strict = true
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.dead_code.enabled);
        assert!((config.dead_code.confidence_threshold - 0.75).abs() < f64::EPSILON);
    }

    // ------------------------------------------------------------------
    // Env precedence & alias tests (new for GF2)
    // ------------------------------------------------------------------

    #[test]
    fn test_ollama_key_alias_deserializes() {
        // The TOML key `ollama_key` is a serde alias for `ollama_cloud_api_key`.
        let toml_str = r#"
            [local_model]
            ollama_key = "alias-secret"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.local_model.ollama_cloud_api_key.as_deref(),
            Some("alias-secret")
        );
    }

    #[test]
    fn test_config_view_json_does_not_contain_sentinel_secret() {
        // Simulate what `config view --json` does: serialize to JSON then redact.
        let sentinel = "TEST-SENTINEL-SECRET-VALUE-NEVER-LEAKS";
        let config = Config {
            local_model: LocalModelConfig {
                ollama_cloud_api_key: Some(sentinel.to_string()),
                ..Default::default()
            },
            gemini: GeminiConfig {
                api_key: Some(sentinel.to_string()),
                ..Default::default()
            },
            ..Default::default()
        };

        let mut val = serde_json::to_value(&config).unwrap();
        crate::config::redact::redact_config_value(&mut val);
        let serialized = serde_json::to_string(&val).unwrap();

        assert!(
            !serialized.contains(sentinel),
            "Sentinel secret leaked in JSON output: {serialized}"
        );
    }
}
