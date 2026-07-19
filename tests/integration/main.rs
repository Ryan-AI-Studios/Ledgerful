mod adr_lifecycle;
mod ask_auto_scan;
mod ask_kg_fallback;
mod ask_structural_queries;
mod bridge_ask_tests;
mod bridge_export_tests;
mod bridge_import_tests;
mod bridge_ipc_tests;
mod bridge_lineage_tests;
mod bridge_notify_tests;
mod bridge_query_tests;
mod bridge_tests;
mod cargo_lock_orphan_pruning;
mod cedar_orphan_pruning;
mod ci_prediction;
mod cli_ask;
mod cli_audit;
mod cli_binary;
mod cli_config;
mod cli_config_set;
mod cli_dead_code;
mod cli_dead_code_prune;
mod cli_dependencies;
mod cli_doctor;
mod cli_dx1_prompts;
mod cli_dx7_config_hints;
mod cli_federate;
mod cli_hook_repair;
mod cli_hotspots;
mod cli_hotspots_explain;
mod cli_impact;
mod cli_index;
mod cli_init;
mod cli_ledger_graph;
mod cli_migration_prompt;
mod cli_reset;
mod cli_scan;
mod cli_search;
mod cli_services_diff_messaging;
mod cli_setup;
mod cli_sparse_empty_states;
mod cli_surfaces;
mod cli_update;
#[cfg(feature = "usage-metrics")]
mod cli_usage;
mod cli_verify;
mod cli_verify_explain_test_mapping;
mod cli_verify_rules;
mod cli_verify_stacks;
mod cli_viz;
mod cli_watch;
mod common;
mod complexity_scoring;
mod cozo_schema_migration;
mod cozo_vector_ops;
mod cozodb_integrity;
mod cross_platform_doctor;
mod crypto_key_migrate;
mod daemon_lifecycle;
mod demo_command;
mod doc_generation;
mod dump_rust_tree;
#[cfg(feature = "export")]
mod export_cli_parity;
#[cfg(feature = "export")]
mod export_control_tests;
mod federated_discovery;
mod gate_mode;
mod hook_commit_msg;
mod hook_post_commit;
mod hotspot_ranking;
mod impact_verify_pipeline;
mod incremental_graph_consistency;
mod latest_impact_freshness;
mod ledger_adr;
mod ledger_bulk;
mod ledger_chain_hash;
mod ledger_cli_parsing;
mod ledger_crypto;
mod ledger_drift;
mod ledger_enforcement;
mod ledger_enforcement_gate;
mod ledger_export_public;
mod ledger_federation;
mod ledger_git_commit;
mod ledger_graph_edges;
mod ledger_lifecycle;
mod ledger_provenance;
mod ledger_re_sign;
mod ledger_search;
mod ledger_signature_fix;
mod m33_migration;
mod milestone_j_remediation;
mod narrative_golden;
mod observability_cedar_graph_test;
mod path_security;
mod persistence;
mod platform_windows;
mod platform_wsl;
mod policy_check;
mod policy_integration;
mod predictor;
mod repro_w2_extraction;
mod risk_analysis;
mod rollup;
mod rust_parser_modular;
mod scan_pr_tests;
mod scip_integration;
mod search_performance;
mod semantic_search;
mod tantivy_hardening;
mod temporal_coupling;
mod test_mapping_graph;
mod track_f_repro;
mod track_k4_services;
mod track_ta34;
mod track_z2_repro;
mod track_z3_repro;
mod track_z4_repro;
mod track_z5_repro;
mod track_z6_repro;
mod watch_graph_sync;

#[test]
fn test_integration_harness_init() {
    // Basic test to ensure the harness compiles and runs.
    let init = true;
    assert!(init);
}

#[cfg(feature = "sync")]
mod sync_apply;
#[cfg(feature = "sync")]
mod sync_bundle;
#[cfg(feature = "sync")]
mod sync_crypto;
#[cfg(feature = "sync")]
mod sync_extract;
#[cfg(feature = "sync")]
mod sync_hlc;
#[cfg(feature = "sync")]
mod sync_init;
#[cfg(feature = "sync")]
mod sync_transport;
mod timings;
mod track_ta12;

#[cfg(feature = "mcp")]
mod mcp_server;

mod cli_tests_command;
#[cfg(feature = "web")]
mod openapi_contract;
#[cfg(feature = "web")]
mod web_api;
#[cfg(feature = "web")]
mod web_security;
