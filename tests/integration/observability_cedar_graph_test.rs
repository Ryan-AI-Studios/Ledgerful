use serial_test::serial;

use crate::common::{DirGuard, git_add_and_commit, setup_git_repo};
use camino::Utf8PathBuf;
use ledgerful::commands::index::{IndexArgs, execute_index};
use ledgerful::commands::init::execute_init;
use ledgerful::config::model::{Config, ServiceDefinition};
use ledgerful::index::graph_loader::build_native_graph;
use ledgerful::index::orchestrator::{IndexStats, ProjectIndexer};
use ledgerful::state::storage::StorageManager;
use std::env;
use std::fs;
use std::process::Command;
use std::sync::OnceLock;

/// Immutable fixture path bundle for the four graph-wiring tests that use the
/// same minimal `pub fn main() {}` repo, OpenSLO and Cedar fixtures.
///
/// `StorageManager` (and the CozoDB handle it holds) is `!Sync`, so it cannot
/// live directly in a `static OnceLock`. Instead we store only the filesystem
/// paths and the cheap metadata; each test clones the SQLite/Cozo DB files
/// into its own tempdir and opens a fresh `StorageManager`.
struct SharedGraph {
    root: Utf8PathBuf,
    #[allow(dead_code)]
    _tmp: tempfile::TempDir,
    db_dir: Utf8PathBuf,
    #[allow(dead_code)]
    stats: IndexStats,
    config: Config,
}

impl SharedGraph {
    fn setup_default_config() -> Config {
        let mut config = Config::default();
        config.services.definitions = vec![ServiceDefinition {
            name: "user-service".to_string(),
            root: "src/".to_string(),
            owners: vec!["platform-team".to_string()],
            runtime_name: None,
            queues: vec![],
            topics: vec![],
            rpc_endpoints: vec![],
        }];
        config
    }

    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();

        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("lib.rs"), "pub fn main() {}").unwrap();

        let obs_dir = root.join("observability");
        fs::create_dir_all(&obs_dir).unwrap();
        let openslo_yaml = r#"
apiVersion: openslo/v1
kind: SLO
metadata:
  name: user-service-availability
  displayName: User Service Availability
  owner: platform-team
spec:
  service: user-service
  indicator:
    thresholdMetric:
      metricSource:
        metricSourceRef: prometheus
        type: prometheus
      metricQuery: sum(rate(http_requests_total{status=~"2.."}[5m]))
  objectives:
    - target: 0.999
"#;
        fs::write(obs_dir.join("slo.yaml"), openslo_yaml).unwrap();

        let policy_dir = root.join("policies");
        fs::create_dir_all(&policy_dir).unwrap();
        let cedar_policy = r#"
permit(
    principal == User::"alice",
    action == Action::"view",
    resource == Photo::"vacation.jpg"
);
"#;
        fs::write(policy_dir.join("policy.cedar"), cedar_policy).unwrap();

        let config = Self::setup_default_config();
        let db_dir = root.join(".ledgerful").join("state");
        fs::create_dir_all(&db_dir).unwrap();
        let storage = StorageManager::init(db_dir.join("ledger.db").as_std_path()).unwrap();
        let mut indexer = ProjectIndexer::new(storage, root.clone(), config.clone());
        let stats = indexer.full_index().unwrap();
        indexer.build_call_graph().unwrap();
        let cozo = indexer.cozo().expect("CozoDB should be available");
        build_native_graph(indexer.storage(), cozo, "full", &config).unwrap();
        // The storage is dropped here but the files persist in `_tmp`.
        Self {
            root,
            _tmp: tmp,
            db_dir,
            stats,
            config,
        }
    }
}

fn shared_graph() -> &'static SharedGraph {
    static SHARED: OnceLock<SharedGraph> = OnceLock::new();
    SHARED.get_or_init(SharedGraph::new)
}

fn clone_storage_from_shared() -> (StorageManager, Utf8PathBuf, Config) {
    let shared = shared_graph();
    let root = shared.root.clone();
    let src_db = shared.db_dir.join("ledger.db");
    let tmp = tempfile::tempdir().unwrap();
    let target_dir = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf())
        .unwrap()
        .join(".ledgerful")
        .join("state");
    fs::create_dir_all(&target_dir).unwrap();
    let target_db = target_dir.join("ledger.db");
    fs::copy(&src_db, &target_db).unwrap();
    // `ledger.cozo` is a CozoDB sqlite database file, not a directory.
    let src_cozo = shared.db_dir.join("ledger.cozo");
    let target_cozo = target_dir.join("ledger.cozo");
    if src_cozo.exists() {
        fs::copy(&src_cozo, &target_cozo).unwrap();
    }
    let storage = StorageManager::init(target_db.as_std_path()).unwrap();
    (storage, root, shared.config.clone())
}

#[allow(dead_code)]
fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = match path.file_name() {
            Some(n) => n,
            None => continue,
        };
        let target = dst.join(file_name);
        if path.is_dir() {
            copy_dir_all(&path, &target)?;
        } else {
            fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

/// Helper for tests with a unique fixture (different OpenSLO service / no Cedar)
/// that still need a fresh index. This avoids duplicating the indexer wiring.
///
/// Returns the `ProjectIndexer` so the caller can insert test data into the
/// SQLite tables BEFORE calling `build_native_graph` (edges that depend on
/// `api_routes` or `adr_metadata` rows must be present before the graph build).
/// Call `build_obs_graph(indexer, config)` after inserting data.
struct ObsIndexer {
    indexer: ProjectIndexer,
}

fn new_indexer_for_obs_test(
    config: &Config,
    db_path: &std::path::Path,
    root: Utf8PathBuf,
) -> ObsIndexer {
    let storage = StorageManager::init(db_path).unwrap();
    let mut indexer = ProjectIndexer::new(storage, root, config.clone());
    indexer.full_index().unwrap();
    indexer.build_call_graph().unwrap();
    ObsIndexer { indexer }
}

fn build_obs_graph(obs: ObsIndexer, config: &Config) -> StorageManager {
    let storage = obs.indexer.storage();
    let cozo = storage.cozo.as_ref().expect("CozoDB should be available");
    build_native_graph(storage, cozo, "full", config).unwrap();
    obs.indexer.into_storage()
}

#[test]
fn test_observability_and_cedar_graph_wiring() {
    let (storage, _root, _config) = clone_storage_from_shared();

    let cozo = storage.cozo.as_ref().expect("CozoDB should be available");

    // 5. Query and verify OpenSLO nodes and edges
    // Verify Slo node exists
    let res_slo = cozo.run_script(
        "?[id, label] := *node{id, label, category: 'slo'}, id = 'urn:ledgerful:slo:user-service-availability'"
    ).unwrap();
    assert_eq!(res_slo.rows.len(), 1, "SLO node should be inserted");

    // Verify Metric node exists
    let res_metric = cozo.run_script(
        "?[id, label] := *node{id, label, category: 'metric'}, id = 'urn:ledgerful:metric:user-service-availability-threshold'"
    ).unwrap();
    assert_eq!(res_metric.rows.len(), 1, "Metric node should be inserted");

    // Verify Service node exists
    let res_service = cozo.run_script(
        "?[id, label] := *node{id, label, category: 'service'}, id = 'urn:ledgerful:service:user-service'"
    ).unwrap();
    assert_eq!(res_service.rows.len(), 1, "Service node should be inserted");

    // Verify Owner node exists
    let res_owner = cozo.run_script(
        "?[id, label] := *node{id, label, category: 'role'}, id = 'urn:ledgerful:role:platform-team'"
    ).unwrap();
    assert_eq!(res_owner.rows.len(), 1, "Owner node should be inserted");

    // Verify SLO Monitors Service edge exists
    let res_monitors = cozo
        .run_script(
            "?[src, tgt] := *edge{source: src, target: tgt, relation: 'monitors'}, \
         src = 'urn:ledgerful:slo:user-service-availability', \
         tgt = 'urn:ledgerful:service:user-service'",
        )
        .unwrap();
    assert_eq!(
        res_monitors.rows.len(),
        1,
        "SLO monitors Service edge should exist"
    );

    // Verify SLO DependsOn Metric edge exists
    let res_depends = cozo
        .run_script(
            "?[src, tgt] := *edge{source: src, target: tgt, relation: 'depends_on'}, \
         src = 'urn:ledgerful:slo:user-service-availability', \
         tgt = 'urn:ledgerful:metric:user-service-availability-threshold'",
        )
        .unwrap();
    assert_eq!(
        res_depends.rows.len(),
        1,
        "SLO depends_on Metric edge should exist"
    );

    // Verify Owner Owns Service edge exists
    let res_owns_svc = cozo
        .run_script(
            "?[src, tgt] := *edge{source: src, target: tgt, relation: 'owns'}, \
         src = 'urn:ledgerful:role:platform-team', \
         tgt = 'urn:ledgerful:service:user-service'",
        )
        .unwrap();
    assert_eq!(
        res_owns_svc.rows.len(),
        1,
        "Owner owns Service edge should exist"
    );

    // Verify Owner Owns SLO edge exists
    let res_owns_slo = cozo
        .run_script(
            "?[src, tgt] := *edge{source: src, target: tgt, relation: 'owns'}, \
         src = 'urn:ledgerful:role:platform-team', \
         tgt = 'urn:ledgerful:slo:user-service-availability'",
        )
        .unwrap();
    assert_eq!(
        res_owns_slo.rows.len(),
        1,
        "Owner owns SLO edge should exist"
    );

    // 6. Query and verify Cedar Policy nodes and edges
    // Verify Policy node exists
    let res_policy = cozo
        .run_script("?[id, label] := *node{id, label, category: 'policy'}")
        .unwrap();
    assert_eq!(res_policy.rows.len(), 1, "Policy node should be inserted");

    // Verify Principal node exists
    let res_principal = cozo.run_script(
        "?[id, label] := *node{id, label, category: 'principal'}, id = 'urn:ledgerful:principal:User::\"alice\"'"
    ).unwrap();
    assert_eq!(
        res_principal.rows.len(),
        1,
        "Principal node should be inserted"
    );

    // Verify Action node exists
    let res_action = cozo.run_script(
        "?[id, label] := *node{id, label, category: 'action'}, id = 'urn:ledgerful:action:Action::\"view\"'"
    ).unwrap();
    assert_eq!(res_action.rows.len(), 1, "Action node should be inserted");

    // Verify Resource node exists
    let res_resource = cozo.run_script(
        "?[id, label] := *node{id, label, category: 'resource'}, id = 'urn:ledgerful:resource:Photo::\"vacation.jpg\"'"
    ).unwrap();
    assert_eq!(
        res_resource.rows.len(),
        1,
        "Resource node should be inserted"
    );

    // Verify Policy Authorizes Principal edge exists
    let res_auth_p = cozo
        .run_script(
            "?[src, tgt] := *edge{source: src, target: tgt, relation: 'authorizes'}, \
         tgt = 'urn:ledgerful:principal:User::\"alice\"'",
        )
        .unwrap();
    assert_eq!(
        res_auth_p.rows.len(),
        1,
        "Policy authorizes Principal edge should exist"
    );

    // Verify Policy Authorizes Action edge exists
    let res_auth_a = cozo
        .run_script(
            "?[src, tgt] := *edge{source: src, target: tgt, relation: 'authorizes'}, \
         tgt = 'urn:ledgerful:action:Action::\"view\"'",
        )
        .unwrap();
    assert_eq!(
        res_auth_a.rows.len(),
        1,
        "Policy authorizes Action edge should exist"
    );

    // Verify Policy Authorizes Resource edge exists
    let res_auth_r = cozo
        .run_script(
            "?[src, tgt] := *edge{source: src, target: tgt, relation: 'authorizes'}, \
         tgt = 'urn:ledgerful:resource:Photo::\"vacation.jpg\"'",
        )
        .unwrap();
    assert_eq!(
        res_auth_r.rows.len(),
        1,
        "Policy authorizes Resource edge should exist"
    );
}

#[test]
fn test_obs_node_source_file_in_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();

    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "pub fn main() {}").unwrap();

    let obs_dir = root.join("observability");
    fs::create_dir_all(&obs_dir).unwrap();
    let openslo_yaml = r#"
apiVersion: openslo/v1
kind: SLO
metadata:
  name: checkout-latency
  displayName: Checkout Latency SLO
spec:
  service: checkout-service
  indicator:
    thresholdMetric:
      metricSource:
        type: prometheus
      metricQuery: histogram_quantile(0.99, http_request_duration_seconds_bucket)
"#;
    fs::write(obs_dir.join("checkout.yaml"), openslo_yaml).unwrap();

    let mut config = Config::default();
    config.services.definitions = vec![ServiceDefinition {
        name: "checkout-service".to_string(),
        root: "src/".to_string(),
        owners: vec![],
        runtime_name: None,
        queues: vec![],
        topics: vec![],
        rpc_endpoints: vec![],
    }];

    let db_dir = root.join(".ledgerful").join("state");
    fs::create_dir_all(&db_dir).unwrap();
    let obs = new_indexer_for_obs_test(
        &config,
        db_dir.join("ledger.db").as_std_path(),
        root.clone(),
    );
    let storage = build_obs_graph(obs, &config);
    let cozo = storage.cozo.as_ref().expect("CozoDB should be available");

    // The SLO node metadata must include source_file so observability diff can match it
    let res = cozo
        .run_script(
            "?[id, metadata] := *node{id, metadata, category: 'slo'}, \
             id = 'urn:ledgerful:slo:checkout-latency'",
        )
        .unwrap();
    assert_eq!(res.rows.len(), 1, "SLO node should exist");

    let meta = &res.rows[0][1];
    let meta_json = if let cozo::DataValue::Json(j) = meta {
        j.clone()
    } else {
        panic!("metadata should be JSON");
    };
    let source_file = meta_json
        .get("source_file")
        .and_then(|v| v.as_str())
        .expect("source_file must be present in SLO node metadata");
    assert_eq!(
        source_file, "observability/checkout.yaml",
        "source_file must be repo-relative so it matches git diff paths; got: {source_file}"
    );
}

#[test]
fn test_policy_adr_security_cross_link() {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();

    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "pub fn main() {}").unwrap();

    // Cedar policy fixture
    let policy_dir = root.join("policies");
    fs::create_dir_all(&policy_dir).unwrap();
    let cedar_policy = r#"
permit(
    principal == User::"alice",
    action == Action::"read",
    resource == Resource::"data"
);
"#;
    fs::write(policy_dir.join("auth.cedar"), cedar_policy).unwrap();

    let config = Config::default();

    let db_dir = root.join(".ledgerful").join("state");
    fs::create_dir_all(&db_dir).unwrap();
    let mut obs = new_indexer_for_obs_test(
        &config,
        db_dir.join("ledger.db").as_std_path(),
        root.clone(),
    );

    // Insert an ADR whose summary contains "security" — should link to the policy via Governs
    {
        let conn = obs.indexer.storage_mut().get_connection_mut();
        conn.execute(
            "INSERT INTO transactions (tx_id, status, category, entity, entity_normalized, session_id, source, started_at) \
             VALUES ('adr-tx-001', 'COMMITTED', 'ARCHITECTURE', 'src/auth.rs', 'src/auth.rs', 'test-session', 'CLI', '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO ledger_entries (tx_id, category, entity, entity_normalized, change_type, summary, reason, committed_at) \
             VALUES ('adr-tx-001', 'ARCHITECTURE', 'src/auth.rs', 'src/auth.rs', 'MODIFY', \
             'Adopt JWT-based security policy for all API endpoints', \
             'Compliance requirement', '2026-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO adr_metadata (adr_id, status, last_updated_at) \
             VALUES ('adr-tx-001', 'accepted', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    }

    let storage = build_obs_graph(obs, &config);
    let cozo = storage.cozo.as_ref().expect("CozoDB should be available");

    // ADR node should exist with label containing "security"
    let adr_res = cozo
        .run_script("?[id, label] := *node{id, label, category: 'adr'}")
        .unwrap();
    assert_eq!(adr_res.rows.len(), 1, "ADR node should be inserted");
    let adr_label = if let cozo::DataValue::Str(l) = &adr_res.rows[0][1] {
        l.to_string()
    } else {
        panic!("label should be string");
    };
    assert!(
        adr_label.to_lowercase().contains("security"),
        "ADR label should contain 'security', got: {adr_label}"
    );

    // Policy â†’ ADR Governs edge must exist
    let governs_res = cozo
        .run_script(
            "?[src, tgt] := *edge{source: src, target: tgt, relation: 'governs'}, \
             tgt = 'urn:ledgerful:adr:adr-tx-001'",
        )
        .unwrap();
    assert_eq!(
        governs_res.rows.len(),
        1,
        "Policy should have a Governs edge to the security ADR"
    );
}

#[test]
fn test_policy_endpoint_cross_link() {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();

    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "pub fn main() {}").unwrap();

    // Cedar policy that mentions the /api/users endpoint in its raw text
    let policy_dir = root.join("policies");
    fs::create_dir_all(&policy_dir).unwrap();
    let cedar_policy = r#"
// protects /api/users resource
permit(
    principal == User::"alice",
    action == Action::"GET",
    resource == Resource::"/api/users"
);
"#;
    fs::write(policy_dir.join("users.cedar"), cedar_policy).unwrap();

    let config = Config::default();

    let db_dir = root.join(".ledgerful").join("state");
    fs::create_dir_all(&db_dir).unwrap();
    let mut obs = new_indexer_for_obs_test(
        &config,
        db_dir.join("ledger.db").as_std_path(),
        root.clone(),
    );

    // Insert an api_routes row whose path_pattern appears in the policy raw text
    {
        let conn = obs.indexer.storage_mut().get_connection_mut();
        conn.execute(
            "INSERT OR IGNORE INTO project_files (file_path, language, last_indexed_at) \
             VALUES ('src/lib.rs', 'rust', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let file_id: i64 = conn
            .query_row(
                "SELECT id FROM project_files WHERE file_path = 'src/lib.rs'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO api_routes (method, path_pattern, framework, handler_file_id, last_indexed_at) \
             VALUES ('GET', '/api/users', 'axum', ?1, '2026-01-01T00:00:00Z')",
            [file_id],
        )
        .unwrap();
    }

    let storage = build_obs_graph(obs, &config);
    let cozo = storage.cozo.as_ref().expect("CozoDB should be available");

    // Policy â†’ endpoint ProtectedBy edge must exist
    let protected_res = cozo
        .run_script(
            "?[src, tgt] := *edge{source: src, target: tgt, relation: 'protected_by'}, \
             tgt = 'urn:ledgerful:endpoint:GET:/api/users'",
        )
        .unwrap();
    assert_eq!(
        protected_res.rows.len(),
        1,
        "Policy should have a ProtectedBy edge to the /api/users endpoint"
    );
}

#[test]
fn test_dogfood_fixtures_syntax() {
    let cargo_manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map(Utf8PathBuf::from)
        .unwrap_or_else(|_| Utf8PathBuf::from("."));

    // Path to the fixtures
    let slo_path = cargo_manifest_dir.join("tests/fixtures/observability/dogfood_slo.yaml");
    let policy_path = cargo_manifest_dir.join("tests/fixtures/policies/dogfood_policy.cedar");

    // Read the slo fixture
    let slo_content = fs::read_to_string(&slo_path)
        .unwrap_or_else(|e| panic!("Failed to read slo fixture at {}: {}", slo_path, e));

    // Verify it parses correctly using parse_openslo
    let slos = ledgerful::observability::openslo::parse_openslo(&slo_content)
        .expect("dogfood_slo.yaml should parse correctly as OpenSLO");
    assert_eq!(
        slos.len(),
        2,
        "Expected 2 OpenSLO entities (Service and SLO)"
    );

    let service = slos
        .iter()
        .find(|s| s.kind == "Service")
        .expect("Service should be present");
    assert_eq!(service.name, "dogfood-service");

    let slo = slos
        .iter()
        .find(|s| s.kind == "SLO")
        .expect("SLO should be present");
    assert_eq!(slo.name, "dogfood-slo");
    assert_eq!(slo.service_name, Some("dogfood-service".to_string()));

    // Verify SLO contains objectives and indicator
    assert!(slo.indicator.is_some(), "SLO must have an indicator");
    let indicator = slo.indicator.as_ref().unwrap();
    assert!(
        indicator.threshold_metric.is_some(),
        "Indicator must contain a thresholdMetric"
    );

    assert_eq!(slo.metrics.len(), 1, "Expected 1 metric in the SLO");
    assert_eq!(slo.metrics[0].name, "dogfood-slo-threshold");
    assert_eq!(
        slo.metrics[0].query,
        "sum(rate(dogfood_requests_total{status=~\"2..\"}[5m]))"
    );
    assert_eq!(slo.metrics[0].source, "prometheus");

    // Read the policy fixture
    let policy_content = fs::read_to_string(&policy_path)
        .unwrap_or_else(|e| panic!("Failed to read policy fixture at {}: {}", policy_path, e));

    // Verify it parses correctly using CedarImporter
    let importer = ledgerful::policy::cedar::CedarImporter::new();
    let policies = importer.parse(&policy_content);
    assert_eq!(policies.len(), 1, "Expected 1 Cedar policy");
    assert_eq!(policies[0].effect, "permit");
    assert_eq!(policies[0].principal, Some("User::\"admin\"".to_string()));
    assert_eq!(policies[0].action, Some("Action::\"access\"".to_string()));
    assert_eq!(
        policies[0].resource,
        Some("Feature::\"dogfood\"".to_string())
    );
}

/// End-to-end regression test for the documented "Dogfood Fixture Smoke
/// Recipes" in docs/operator-surface-policy.md (CG-F29).
///
/// `test_dogfood_fixtures_syntax` above only proves the fixtures are
/// syntactically valid OpenSLO/Cedar by calling the parsers directly â€” it
/// never runs the actual documented CLI path. An external review (codex
/// findings #1) found that following the documented recipe in a fresh
/// checkout can still yield an empty `observability coverage` /
/// `security boundaries` result, because the recipe's fixture source paths
/// are relative to the Ledgerful repo root and silently resolve to nothing
/// when run from a different working directory.
///
/// This test exercises the real, compiled `ledgerful` binary against a
/// fresh temp git repo, using the exact documented steps: `ledgerful init`,
/// copy the checked-in dogfood fixtures into `observability/` and
/// `policies/`, `ledgerful index --analyze-graph`, then assert that both
/// `observability coverage --json` and `security boundaries --json` report
/// genuinely populated (non-empty-state) results.
#[test]
fn test_scan_impact_auto_analyzes_graph_for_openslo_change() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "pub fn main() {}").unwrap();
    git_add_and_commit(root, "initial");

    let cargo_manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map(Utf8PathBuf::from)
        .unwrap_or_else(|_| Utf8PathBuf::from("."));
    let slo_src = cargo_manifest_dir.join("tests/fixtures/observability/dogfood_slo.yaml");
    assert!(
        slo_src.exists(),
        "dogfood SLO fixture must exist at {slo_src}"
    );

    let obs_dir = root.join("observability");
    fs::create_dir_all(&obs_dir).unwrap();
    fs::copy(slo_src.as_std_path(), obs_dir.join("dogfood_slo.yaml")).unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    // Initialize storage first so `scan --impact` can read and auto-analyze.
    let init_output = Command::new(ledgerful_bin)
        .args(["init"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        init_output.status.success(),
        "`init` failed: {:?}",
        String::from_utf8_lossy(&init_output.stderr)
    );

    // Run `scan --impact` with an unstaged OpenSLO change and no prior index.
    let scan_output = Command::new(ledgerful_bin)
        .args(["scan", "--impact"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        scan_output.status.success(),
        "`scan --impact` failed: {:?}",
        String::from_utf8_lossy(&scan_output.stderr)
    );
    let scan_stdout = String::from_utf8_lossy(&scan_output.stdout);
    assert!(
        scan_stdout.contains("Auto-triggering graph analysis")
            || scan_stdout.contains("observability"),
        "expected scan --impact to mention observability/graph analysis, got: {scan_stdout}"
    );

    // `observability diff` should now populate correctly without requiring a
    // manual `index --analyze-graph`.
    let diff_output = Command::new(ledgerful_bin)
        .args(["observability", "diff", "--json"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        diff_output.status.success(),
        "`observability diff --json` failed: {:?}",
        String::from_utf8_lossy(&diff_output.stderr)
    );
    let diff_stdout = String::from_utf8_lossy(&diff_output.stdout);
    let diff_json: serde_json::Value = serde_json::from_str(&diff_stdout)
        .unwrap_or_else(|e| panic!("diff output was not valid JSON: {e}\n{diff_stdout}"));

    let changed = diff_json["changed"].as_array().unwrap_or_else(|| {
        panic!("expected `changed` array in observability diff output, got: {diff_stdout}")
    });
    assert!(
        !changed.is_empty(),
        "expected observability diff to report changed SLOs after auto-analysis, got: {diff_stdout}"
    );
    assert!(
        diff_json.get("emptyReason").is_none(),
        "expected observability diff to be populated (no emptyReason), got: {diff_stdout}"
    );
}

#[test]
fn test_scan_impact_skips_auto_analysis_for_non_observability_change() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "pub fn main() {}").unwrap();
    git_add_and_commit(root, "initial");

    fs::write(root.join("README.md"), "# modified").unwrap();

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    let init_output = Command::new(ledgerful_bin)
        .args(["init"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        init_output.status.success(),
        "`init` failed: {:?}",
        String::from_utf8_lossy(&init_output.stderr)
    );

    let scan_output = Command::new(ledgerful_bin)
        .args(["scan", "--impact"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        scan_output.status.success(),
        "`scan --impact` failed: {:?}",
        String::from_utf8_lossy(&scan_output.stderr)
    );
    let scan_stdout = String::from_utf8_lossy(&scan_output.stdout);
    assert!(
        !scan_stdout.contains("Auto-triggering graph analysis"),
        "expected no auto-analysis for README change, got: {scan_stdout}"
    );
}

#[serial(cwd)]
#[test]
fn test_dogfood_recipe_end_to_end_populates_observability_and_security() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Resolve fixture source paths robustly, independent of the test
    // process's current working directory â€” this is the same robust
    // pattern used by `test_dogfood_fixtures_syntax` above.
    let cargo_manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map(Utf8PathBuf::from)
        .unwrap_or_else(|_| Utf8PathBuf::from("."));
    let slo_src = cargo_manifest_dir.join("tests/fixtures/observability/dogfood_slo.yaml");
    let policy_src = cargo_manifest_dir.join("tests/fixtures/policies/dogfood_policy.cedar");
    assert!(
        slo_src.exists(),
        "dogfood SLO fixture must exist at {slo_src}"
    );
    assert!(
        policy_src.exists(),
        "dogfood Cedar fixture must exist at {policy_src}"
    );

    setup_git_repo(root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("lib.rs"), "pub fn main() {}").unwrap();
    git_add_and_commit(root, "initial");

    // Step 1 of the documented recipe: `ledgerful init`, then copy the
    // checked-in dogfood fixtures into the active scanning directories.
    // Use `std::fs::copy` with the CARGO_MANIFEST_DIR-resolved absolute
    // source paths rather than a shell command, so this test is immune to
    // the exact CWD-fragility class of bug the recipe documentation has.
    {
        let _guard = DirGuard::new(root);
        execute_init(false).unwrap();

        let obs_dir = root.join("observability");
        let policy_dir = root.join("policies");
        fs::create_dir_all(&obs_dir).unwrap();
        fs::create_dir_all(&policy_dir).unwrap();
        fs::copy(slo_src.as_std_path(), obs_dir.join("dogfood_slo.yaml")).unwrap();
        fs::copy(
            policy_src.as_std_path(),
            policy_dir.join("dogfood_policy.cedar"),
        )
        .unwrap();

        // Step 2 of the documented recipe.
        execute_index(IndexArgs {
            analyze_graph: true,
            ..Default::default()
        })
        .unwrap();
    }

    let ledgerful_bin = env!("CARGO_BIN_EXE_ledgerful");

    // Step 3 of the documented recipe: `ledgerful observability coverage`.
    let coverage_output = Command::new(ledgerful_bin)
        .args(["observability", "coverage", "--json"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        coverage_output.status.success(),
        "`observability coverage --json` failed: {:?}",
        String::from_utf8_lossy(&coverage_output.stderr)
    );
    let coverage_stdout = String::from_utf8_lossy(&coverage_output.stdout);
    let coverage_json: serde_json::Value = serde_json::from_str(&coverage_stdout)
        .unwrap_or_else(|e| panic!("coverage output was not valid JSON: {e}\n{coverage_stdout}"));

    // `format_json_empty_state` (src/output/empty.rs) emits a bare JSON array
    // when populated, and an object wrapping `results`/`emptyReason`/`message`
    // when empty. Asserting the top-level shape is a bare, non-empty array is
    // exactly how to distinguish "populated" from the empty state.
    let coverage_array = coverage_json.as_array().unwrap_or_else(|| {
        panic!(
            "expected `observability coverage --json` to return a populated bare JSON array \
             (not the wrapped empty-state object), got: {coverage_stdout}"
        )
    });
    assert!(
        !coverage_array.is_empty(),
        "expected `observability coverage --json` to be non-empty after following the \
         documented dogfood recipe, got: {coverage_stdout}"
    );
    assert_eq!(
        coverage_array[0]["service"], "Service: dogfood-service",
        "expected the dogfood Service node to appear in coverage output, got: {coverage_stdout}"
    );
    assert_eq!(
        coverage_array[0]["slo_count"], 1,
        "expected the dogfood SLO to be counted, got: {coverage_stdout}"
    );
    assert_eq!(
        coverage_array[0]["metric_count"], 1,
        "expected the dogfood metric to be counted, got: {coverage_stdout}"
    );

    // Step 3 continued: `ledgerful security boundaries`.
    let boundaries_output = Command::new(ledgerful_bin)
        .args(["security", "boundaries", "--json"])
        .current_dir(root)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .unwrap();
    assert!(
        boundaries_output.status.success(),
        "`security boundaries --json` failed: {:?}",
        String::from_utf8_lossy(&boundaries_output.stderr)
    );
    let boundaries_stdout = String::from_utf8_lossy(&boundaries_output.stdout);
    let boundaries_json: serde_json::Value = serde_json::from_str(&boundaries_stdout)
        .unwrap_or_else(|e| {
            panic!("boundaries output was not valid JSON: {e}\n{boundaries_stdout}")
        });

    // `security boundaries --json` wraps its payload in a top-level object
    // with `meta`/`boundaries` and, only in the empty case, additional
    // `emptyReason`/`message` keys (see execute_boundaries in
    // src/commands/security.rs). Populated output has non-empty
    // `boundaries.auth_nodes` and omits `emptyReason`.
    assert!(
        boundaries_json.get("emptyReason").is_none(),
        "expected `security boundaries --json` to be populated (no emptyReason) after \
         following the documented dogfood recipe, got: {boundaries_stdout}"
    );
    let auth_nodes = boundaries_json["boundaries"]["auth_nodes"]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "expected `boundaries.auth_nodes` to be a populated array, got: {boundaries_stdout}"
            )
        });
    assert!(
        !auth_nodes.is_empty(),
        "expected `security boundaries --json` auth_nodes to be non-empty after following \
         the documented dogfood recipe, got: {boundaries_stdout}"
    );

    let policy_node = auth_nodes
        .iter()
        .find(|n| n["category"] == "policy")
        .unwrap_or_else(|| {
            panic!("expected a policy node in security boundaries output, got: {boundaries_stdout}")
        });
    assert!(
        policy_node["id"]
            .as_str()
            .unwrap_or_default()
            .contains("policy"),
        "expected the dogfood Cedar policy node id to reference 'policy', got: {boundaries_stdout}"
    );
}
