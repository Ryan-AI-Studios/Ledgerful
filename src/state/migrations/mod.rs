pub mod m11_to_m20;
pub mod m1_to_m10;
pub mod m21_to_m29;
pub mod m30_scip;
pub mod m31_ci_predict;
pub mod m32_symbol_metadata;
pub mod m33_intent_provenance;
pub mod m34_api_route_enrichment;
pub mod m35_adr_lifecycle;
pub mod m36_env_config_metadata;
pub mod m37_ci_deploy_enrichment;
pub mod m38_hotspot_history;
pub mod m39_ledger_neighborhood;
pub mod m40_validator_management;
pub mod m41_sync;
pub mod m42_usage_counters;
pub mod m43_ledger_author;
pub mod m44_usage_days;
pub mod m45_ledger_verification_runs_tx;
pub mod m46_hotspot_trends;
pub mod m47_project_files_git_meta;
pub mod m48_changed_files_diff_stats;
pub mod m49_project_trend_days;
pub mod m50_ledger_entry_observed;
pub mod m51_ledger_chain_hash;

use rusqlite_migration::Migrations;

pub fn get_migrations() -> Migrations<'static> {
    let mut all_m = Vec::new();
    all_m.extend(m1_to_m10::m1_to_m10());
    all_m.extend(m11_to_m20::m11_to_m20());
    all_m.extend(m21_to_m29::m21_to_m29());
    all_m.extend(m30_scip::m30_scip());
    all_m.extend(m31_ci_predict::m31_ci_predict());
    all_m.extend(m32_symbol_metadata::m32_symbol_metadata());
    all_m.extend(m33_intent_provenance::m33_intent_provenance());
    all_m.extend(m34_api_route_enrichment::m34_api_route_enrichment());
    all_m.extend(m35_adr_lifecycle::m35_adr_lifecycle());
    all_m.extend(m36_env_config_metadata::m36_env_config_metadata());
    all_m.extend(m37_ci_deploy_enrichment::m37_ci_deploy_enrichment());
    all_m.extend(m38_hotspot_history::m38_hotspot_history());
    all_m.extend(m39_ledger_neighborhood::m39_ledger_neighborhood());
    all_m.extend(m40_validator_management::m40_validator_management());
    // m41 (sync_state / tx_tombstones / entry_hlc) is registered
    // UNCONDITIONALLY — same trade-off as m42 / m44 / m45. The
    // tables and column are only used by the `sync` code paths, but
    // gating the migration on `#[cfg(feature = "sync")]` causes a
    // pre-existing "Attempt to migrate a database with a migration
    // number that is too high" failure on any binary that does NOT
    // have the `sync` feature when the on-disk DB was last written
    // by a binary that did (e.g. the dev machine's `cargo doctor`
    // run writes m41, then a subsequent `cargo nextest run --features
    // web --test integration` runs a binary without `sync` and
    // can't open the DB). The tables are empty in builds without
    // the feature, so the surface-area leak is harmless.
    all_m.extend(m41_sync::m41_sync());
    // m42 is registered unconditionally (not gated on `usage-metrics`)
    // because the `usage_counters` table already exists in any
    // pre-M7-review database that ran with the feature on. Gating
    // m42 would cause existing DBs to fail the migration
    // pre-flight check ("Attempt to migrate a database with a
    // migration number that is too high") when the binary is
    // later built without the feature. The table is empty in
    // binaries that don't include the feature.
    // The `expected_tables` test unconditionally asserts the table
    // exists; this is consistent with the unconditional registration.
    all_m.extend(m42_usage_counters::m42_usage_counters());
    all_m.extend(m43_ledger_author::m43_ledger_author());
    // m44 is a NEW migration introduced to fix the H2 regression
    // found in M7 r2 review (the `active_days_in_window` computation
    // was broken because `last_seen_day` lived on `usage_counters`
    // and was overwritten by the UPSERT). Like m42, m44 is
    // registered UNCONDITIONALLY: the `usage_days` table is created
    // on every binary to preserve backward-compat — a DB that was
    // created by a binary built with `usage-metrics` has
    // `user_version >= 44`, and a binary built without the feature
    // would otherwise fail the rusqlite_migration pre-flight check
    // ("Attempt to migrate a database with a migration number that
    // is too high") when opening that DB. The table is empty and
    // unread in builds without the feature, so the surface-area
    // leak is harmless. This is the same trade-off the r2 review
    // accepted for m42 ("M4 Deviation Verdict" in
    // `output/m7-review-2.md:162-172`).
    all_m.extend(m44_usage_days::m44_usage_days());
    // m45 is a NEW migration introduced to fix the M8 review C2 finding
    // (`fetch_verification_stats` could not JOIN `verification_runs` by
    // `tx_id` because that column didn't exist). It is registered
    // UNCONDITIONALLY for the same backward-compat reason as m42/m44: a
    // DB created by a binary built with the `sync` feature has
    // `user_version >= 45`, and a binary without the feature would
    // otherwise fail the rusqlite_migration pre-flight check
    // ("Attempt to migrate a database with a migration number that is
    // too high"). The two new columns are nullable and default to NULL,
    // so the surface-area leak is harmless in builds without `verify`.
    all_m.extend(m45_ledger_verification_runs_tx::m45_ledger_verification_runs_tx());
    // m46 adds the `hotspot_trends` table used by the post-commit hook to
    // accumulate per-file hotspot scores keyed by commit hash. It is
    // registered unconditionally so any binary that opens the DB can apply
    // the migration and keep schema_version monotonic.
    all_m.extend(m46_hotspot_trends::m46_hotspot_trends());
    // m47 adds `last_touched_at` and `last_contributor` columns to
    // `project_files` (Track TA30). Registered unconditionally for the same
    // backward-compat reason as m41/m42/m44/m45/m46: a DB created by a binary
    // that has run m47 would fail the rusqlite_migration pre-flight check on
    // a binary without the migration. Both columns are nullable and default
    // to NULL, so the surface-area leak is harmless.
    all_m.extend(m47_project_files_git_meta::m47_project_files_git_meta());
    // m48 adds nullable `additions`/`deletions` columns to `changed_files`
    // (Track 0037). Registered unconditionally so the schema version stays
    // monotonic across feature-flag combinations; the columns are nullable
    // and default to NULL, so they are harmless in builds that do not
    // populate them.
    all_m.extend(m48_changed_files_diff_stats::m48_changed_files_diff_stats());
    // m49 adds the `project_trend_days` daily rollup table (Track 0038).
    // Registered unconditionally for the same backward-compat reason as
    // m44-m48: a DB created by a binary that has run m49 would fail the
    // rusqlite_migration pre-flight check on a binary without the migration.
    // The table is empty in builds that don't populate it, so the
    // surface-area leak is harmless.
    all_m.extend(m49_project_trend_days::m49_project_trend_days());
    // m50 adds the nullable `observed` column to `ledger_entries` (Track 0050).
    // Registered unconditionally so the schema version stays monotonic across
    // feature-flag combinations; the column is nullable and defaults to NULL,
    // so it is harmless in builds that do not populate it.
    all_m.extend(m50_ledger_entry_observed::m50_ledger_entry_observed());
    // m51 adds the additive ledger chain hash schema (Track 0046): a nullable
    // `prev_hash` column on `ledger_entries` and a singleton `chain_head`
    // table. The chain lives outside the Ed25519 signing basis. Registered
    // unconditionally so the schema version stays monotonic across binaries
    // built with different feature sets; the column and table are empty in
    // builds that do not populate them, so the surface-area leak is harmless.
    all_m.extend(m51_ledger_chain_hash::m51_ledger_chain_hash());

    Migrations::new(all_m)
}

pub fn get_migrations_count() -> usize {
    let mut count = 0;
    count += m1_to_m10::m1_to_m10().len();
    count += m11_to_m20::m11_to_m20().len();
    count += m21_to_m29::m21_to_m29().len();
    count += m30_scip::m30_scip().len();
    count += m31_ci_predict::m31_ci_predict().len();
    count += m32_symbol_metadata::m32_symbol_metadata().len();
    count += m33_intent_provenance::m33_intent_provenance().len();
    count += m34_api_route_enrichment::m34_api_route_enrichment().len();
    count += m35_adr_lifecycle::m35_adr_lifecycle().len();
    count += m36_env_config_metadata::m36_env_config_metadata().len();
    count += m37_ci_deploy_enrichment::m37_ci_deploy_enrichment().len();
    count += m38_hotspot_history::m38_hotspot_history().len();
    count += m39_ledger_neighborhood::m39_ledger_neighborhood().len();
    count += m40_validator_management::m40_validator_management().len();
    // m41 is counted unconditionally — see the matching comment in
    // `get_migrations` for the rationale.
    count += m41_sync::m41_sync().len();
    // m42 is counted unconditionally — see the matching comment in
    // `get_migrations` for the rationale.
    count += m42_usage_counters::m42_usage_counters().len();
    count += m43_ledger_author::m43_ledger_author().len();
    // m44 is counted unconditionally — see the matching comment in
    // `get_migrations` for the rationale.
    count += m44_usage_days::m44_usage_days().len();
    // m45 is counted unconditionally — see the matching comment in
    // `get_migrations` for the rationale.
    count += m45_ledger_verification_runs_tx::m45_ledger_verification_runs_tx().len();
    count += m46_hotspot_trends::m46_hotspot_trends().len();
    count += m47_project_files_git_meta::m47_project_files_git_meta().len();
    count += m48_changed_files_diff_stats::m48_changed_files_diff_stats().len();
    count += m49_project_trend_days::m49_project_trend_days().len();
    count += m50_ledger_entry_observed::m50_ledger_entry_observed().len();
    // m51 is counted unconditionally — see the matching comment in `get_migrations`.
    count += m51_ledger_chain_hash::m51_ledger_chain_hash().len();

    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn test_migrations_validate() {
        let migrations = get_migrations();
        migrations.validate().unwrap();
    }

    #[test]
    fn test_all_tables_exist() {
        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = get_migrations();
        migrations.to_latest(&mut conn).unwrap();

        let expected_tables = {
            let mut tables = vec![
                "snapshots",
                "batches",
                "changed_files",
                "verification_runs",
                "verification_results",
                "symbols",
                "federated_links",
                "federated_dependencies",
                "transactions",
                "ledger_entries",
                "ledger_fts",
                "tech_stack",
                "commit_validators",
                "category_stack_mappings",
                "watcher_patterns",
                "token_provenance",
                "project_files",
                "index_metadata",
                "project_symbols",
                "project_docs",
                "project_topology",
                "structural_edges",
                "api_routes",
                "data_models",
                "symbol_centrality",
                "observability_patterns",
                "test_mapping",
                "ci_gates",
                "env_declarations",
                "env_references",
                "embeddings",
                "doc_chunks",
                "api_endpoints",
                "test_outcome_history",
                "observability_snapshots",
                "scip_indices",
                "ci_outcome_history",
                "hotspot_history",
                "temporal_coupling_history",
                "transaction_links",
                "usage_counters",
                "hotspot_trends",
                "project_trend_days",
                "chain_head",
            ];

            // `sync_state` and `tx_tombstones` are registered
            // unconditionally (m41) — see the matching comment in
            // `get_migrations`. The tables are harmless in builds
            // without the `sync` feature, and asserting they exist
            // unconditionally keeps the schema-validation consistent
            // with the registration.
            tables.push("sync_state");
            tables.push("tx_tombstones");

            // `usage_days` is registered unconditionally (m44) —
            // see the M4-deviation comment in `get_migrations`. The
            // table is harmless in builds without the feature, and
            // asserting it exists unconditionally keeps the
            // schema-validation consistent with the registration.
            tables.push("usage_days");

            // `project_trend_days` is registered unconditionally (m49) — see
            // the matching comment in `get_migrations`. The table is harmless in
            // builds without the `web` feature, and asserting it exists
            // unconditionally keeps the schema-validation consistent with the
            // registration.
            tables.push("project_trend_days");

            tables
        };

        for table in &expected_tables {
            let count: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "Table {} should exist", table);
        }

        let observed_exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM pragma_table_info('ledger_entries') WHERE name='observed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            observed_exists, 1,
            "ledger_entries.observed column should exist"
        );

        let prev_hash_exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM pragma_table_info('ledger_entries') WHERE name='prev_hash'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            prev_hash_exists, 1,
            "ledger_entries.prev_hash column should exist"
        );
    }

    #[test]
    #[cfg(feature = "sync")]
    fn test_sync_tables_created_on_migration() {
        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = get_migrations();
        migrations.to_latest(&mut conn).unwrap();

        let sync_state_exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='sync_state'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sync_state_exists, 1);

        let tx_tombstones_exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='tx_tombstones'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tx_tombstones_exists, 1);
    }

    #[test]
    fn test_sync_migration_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = get_migrations();

        // Apply once
        migrations.to_latest(&mut conn).unwrap();

        // Apply again
        migrations.to_latest(&mut conn).unwrap();

        // Should still be valid
        migrations.validate().unwrap();
    }
}
