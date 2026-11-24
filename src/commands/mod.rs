pub mod ask;
pub mod ask_routing;
pub mod baseline;
pub mod bench;
pub mod bridge;
pub mod change_context;
pub(crate) mod change_evidence;
pub mod config;
pub mod config_verify;
pub mod configure;
#[cfg(feature = "daemon")]
pub mod daemon;
pub mod data_models;
pub mod dead_code;
pub mod demo;
pub mod dependencies;
pub mod deploy;
pub mod doctor;
pub mod dx1_templates;
pub mod endpoints;
pub mod federate;
pub mod gate;
pub mod graph_export;
pub mod graph_path;
pub mod helpers;
pub mod hook_commit_msg;
pub mod hook_post_commit;
pub mod hook_repair;
pub mod hook_sidecar;
pub mod hook_template;
pub mod hotspots;
pub mod impact;
pub mod index;
pub mod init;
pub mod intent;
pub mod ledger;
pub mod ledger_adopt;
pub mod ledger_adr;
pub mod ledger_audit;
pub mod ledger_diagnose;
pub mod ledger_graph;
pub mod ledger_re_sign;
pub mod ledger_register;
pub mod ledger_search;
pub mod ledger_stack;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod observability;
/// Compiled performance budget table + `bench --check` (0564).
pub mod perf_budget;
#[cfg(any(feature = "viz-server", feature = "web"))]
pub mod pid;
/// CI policy gate (`ledgerful policy check`). Distinct from `crate::policy`
/// (tech-stack `rules.toml` matching).
pub mod policy_check;
pub mod release;
pub mod reset;
pub mod review;
pub mod scan;
pub mod scan_pr;
pub mod schedule;
pub mod search;
pub mod security;
pub mod services_diff;
pub mod session;
pub mod setup;
pub mod surfaces;
pub mod symbols;
#[cfg(feature = "sync")]
pub mod sync;
pub mod test_mapping;
pub mod timings;
pub mod update;
#[cfg(feature = "usage-metrics")]
pub mod usage;
pub mod verify;
pub mod viz;
#[cfg(feature = "viz-server")]
pub mod viz_server;
pub mod watch;
#[cfg(feature = "web")]
pub mod web;

use miette::Diagnostic;
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
pub enum CommandError {
    #[error("Failed to discover repository root")]
    RepoDiscoveryFailed,

    #[error("I/O error during command execution")]
    IoError(#[from] std::io::Error),

    #[error("Verification failed: {0}")]
    Verify(String),
}

#[cfg(test)]
mod baseline_consumer_contract {
    use super::baseline::{
        BaselineError, BaselineIdentity, BaselineScope, Comparability, CompletenessState,
        DeltaClass, EdgeCoverage, EdgeSetDelta, FileEdge, FileEdgeSet, InputCompleteness,
        compare_edge_sets, extract_file_edge_set, load_baseline,
    };
    use crate::config::model::Config;
    use crate::state::{migrations::get_migrations, storage::StorageManager};

    #[test]
    fn baseline_consumer_seam_verifies_digest_and_exports_fields() {
        let _: fn(&StorageManager, &Config) -> miette::Result<FileEdgeSet> = extract_file_edge_set;
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let original = FileEdgeSet {
            identity: BaselineIdentity {
                baseline_schema: 1,
                payload_kind: "fileDependencyEdges".into(),
                scope: BaselineScope::repo(),
                parser_version: "1".into(),
                edge_sources: vec!["native".into()],
                ledgerful_version: "build".into(),
                index_head_hash: None,
                input_digest: "input".into(),
                input_completeness: InputCompleteness {
                    state: CompletenessState::Complete,
                    reasons: vec![],
                },
            },
            coverage: EdgeCoverage {
                edges_resolved_cross_file: 1,
                ..EdgeCoverage::default()
            },
            edges: vec![FileEdge {
                src_path: "a.rs".into(),
                dst_path: "b.rs".into(),
                weight: 1,
            }],
        };
        super::baseline::create_baseline(&storage, "consumer", &original).unwrap();
        let loaded = load_baseline(&storage, "consumer").unwrap();
        assert_eq!(loaded, original);
        let mut changed = loaded.clone();
        changed.edges[0].weight += 1;
        let delta: EdgeSetDelta = compare_edge_sets(&loaded, &changed);
        assert!(matches!(delta.status, Comparability::Comparable));
        assert_eq!(delta.items.unwrap()[0].class, DeltaClass::Worsened);
        storage.get_connection().execute_batch(
            "DROP TRIGGER analysis_baselines_no_update;
             UPDATE analysis_baselines SET payload_digest = 'blake3:invalid' WHERE name = 'consumer';"
        ).unwrap();
        let error = load_baseline(&storage, "consumer").unwrap_err();
        assert!(matches!(error.downcast_ref::<BaselineError>(),
            Some(BaselineError::DigestMismatch { name }) if name == "consumer"));
    }
}
