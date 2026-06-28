//! Track X5 -- Security Child Node Orphan Pruning
//!
//! Tests that when Cedar policy files are deleted or absent, the principal/action/resource
//! child nodes that were previously linked via Authorizes edges are pruned from CozoDB.
//! Prior to the X5 fix, only `policy` nodes were pruned (W13); child categories persisted
//! as dangling orphans visible in `security boundaries`.
//!
//! @cg-tx: 495acfd7-5356-4041-8092-585eea54f348

use camino::Utf8PathBuf;
use ledgerful::config::model::Config;
use ledgerful::index::graph_loader::build_native_graph;
use ledgerful::index::orchestrator::ProjectIndexer;
use ledgerful::state::storage::StorageManager;
use std::fs;

struct TestHarness {
    _tmp: tempfile::TempDir,
    pub root: Utf8PathBuf,
    pub indexer: ProjectIndexer,
}

impl TestHarness {
    fn new_with_policy(policy_filename: &str, policy_content: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8 path");

        fs::create_dir_all(root.join(".git")).expect("git dir");
        fs::create_dir_all(root.join("src")).expect("src dir");
        fs::write(root.join("src").join("lib.rs"), "pub fn noop() {}").expect("lib.rs");

        let policy_dir = root.join("policies");
        fs::create_dir_all(&policy_dir).expect("policies dir");
        fs::write(policy_dir.join(policy_filename), policy_content).expect("policy file");

        let db_dir = root.join(".ledgerful").join("state");
        fs::create_dir_all(&db_dir).expect("state dir");
        let storage =
            StorageManager::init(db_dir.join("ledger.db").as_std_path()).expect("storage init");

        let config = Config::default();
        let mut indexer = ProjectIndexer::new(storage, root.clone(), config);
        indexer.full_index().expect("full_index");
        indexer.build_call_graph().expect("call_graph");

        Self {
            _tmp: tmp,
            root,
            indexer,
        }
    }

    fn build_graph(&self) {
        let cozo = self.indexer.cozo().expect("cozo available");
        let config = Config::default();
        build_native_graph(self.indexer.storage(), cozo, "full", &config)
            .expect("build_native_graph");
    }

    fn cozo(&self) -> &ledgerful::state::storage_cozo::CozoStorage {
        self.indexer.cozo().expect("cozo")
    }

    fn count_nodes_by_category(&self, category: &str) -> usize {
        let script = format!("?[id] := *node{{id, category: '{category}'}}");
        self.cozo()
            .run_script(&script)
            .map(|r| r.rows.len())
            .unwrap_or(0)
    }
}

#[test]
fn test_cedar_child_nodes_pruned_when_no_policies() {
    let cedar_policy = r#"permit(
    principal == User::"alice",
    action == Action::"view",
    resource == Photo::"vacation.jpg"
);"#;
    let harness = TestHarness::new_with_policy("photo.cedar", cedar_policy);

    // First build: policy is on disk -- all child nodes should be present.
    harness.build_graph();
    assert_eq!(
        harness.count_nodes_by_category("principal"),
        1,
        "Before deletion: principal node should exist"
    );
    assert_eq!(
        harness.count_nodes_by_category("action"),
        1,
        "Before deletion: action node should exist"
    );
    assert_eq!(
        harness.count_nodes_by_category("resource"),
        1,
        "Before deletion: resource node should exist"
    );

    // Act: delete the cedar file so no valid policies remain on disk.
    fs::remove_file(harness.root.join("policies").join("photo.cedar")).expect("remove policy file");

    // Second build: no cedar files on disk -- child nodes must be pruned.
    harness.build_graph();

    assert_eq!(
        harness.count_nodes_by_category("principal"),
        0,
        "After deletion: principal orphan should be pruned"
    );
    assert_eq!(
        harness.count_nodes_by_category("action"),
        0,
        "After deletion: action orphan should be pruned"
    );
    assert_eq!(
        harness.count_nodes_by_category("resource"),
        0,
        "After deletion: resource orphan should be pruned"
    );
    assert_eq!(
        harness.count_nodes_by_category("policy"),
        0,
        "After deletion: policy node should also be pruned (W13)"
    );
}

#[test]
fn test_cedar_child_nodes_preserved_for_live_policy() {
    let cedar_policy_a = r#"permit(
    principal == User::"alice",
    action == Action::"read",
    resource == Doc::"report.pdf"
);"#;
    let cedar_policy_b = r#"permit(
    principal == User::"bob",
    action == Action::"write",
    resource == Doc::"notes.txt"
);"#;

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8 path");

    fs::create_dir_all(root.join(".git")).expect("git dir");
    fs::create_dir_all(root.join("src")).expect("src dir");
    fs::write(root.join("src").join("lib.rs"), "pub fn noop() {}").expect("lib.rs");

    let policy_dir = root.join("policies");
    fs::create_dir_all(&policy_dir).expect("policies dir");
    fs::write(policy_dir.join("policy_a.cedar"), cedar_policy_a).expect("policy_a");
    fs::write(policy_dir.join("policy_b.cedar"), cedar_policy_b).expect("policy_b");

    let db_dir = root.join(".ledgerful").join("state");
    fs::create_dir_all(&db_dir).expect("state dir");
    let storage =
        StorageManager::init(db_dir.join("ledger.db").as_std_path()).expect("storage init");

    let config = Config::default();
    let mut indexer = ProjectIndexer::new(storage, root.clone(), config);
    indexer.full_index().expect("full_index");
    indexer.build_call_graph().expect("call_graph");

    let cozo = indexer.cozo().expect("cozo");
    build_native_graph(indexer.storage(), cozo, "full", &Config::default())
        .expect("first build_native_graph");

    // Verify both child nodes exist before deletion.
    let count_principal = |id: &str| -> usize {
        let q = format!("?[x] := *node{{id: x, category: 'principal'}}, x = '{id}'");
        cozo.run_script(&q).map(|r| r.rows.len()).unwrap_or(0)
    };
    let count_action = |id: &str| -> usize {
        let q = format!("?[x] := *node{{id: x, category: 'action'}}, x = '{id}'");
        cozo.run_script(&q).map(|r| r.rows.len()).unwrap_or(0)
    };
    let count_resource = |id: &str| -> usize {
        let q = format!("?[x] := *node{{id: x, category: 'resource'}}, x = '{id}'");
        cozo.run_script(&q).map(|r| r.rows.len()).unwrap_or(0)
    };

    assert_eq!(
        count_principal("urn:ledgerful:principal:User::\"alice\""),
        1,
        "Alice principal initially"
    );
    assert_eq!(
        count_principal("urn:ledgerful:principal:User::\"bob\""),
        1,
        "Bob principal initially"
    );

    // Act: delete policy_b.cedar -- bob's child nodes become orphans.
    fs::remove_file(policy_dir.join("policy_b.cedar")).expect("remove policy_b");

    build_native_graph(indexer.storage(), cozo, "full", &Config::default())
        .expect("second build_native_graph");

    // Alice's child nodes must survive (policy_a is still live).
    assert_eq!(
        count_principal("urn:ledgerful:principal:User::\"alice\""),
        1,
        "Alice principal survives"
    );
    assert_eq!(
        count_action("urn:ledgerful:action:Action::\"read\""),
        1,
        "Alice action survives"
    );
    assert_eq!(
        count_resource("urn:ledgerful:resource:Doc::\"report.pdf\""),
        1,
        "Alice resource survives"
    );

    // Bob's child nodes must be pruned (policy_b was deleted).
    assert_eq!(
        count_principal("urn:ledgerful:principal:User::\"bob\""),
        0,
        "Bob principal pruned"
    );
    assert_eq!(
        count_action("urn:ledgerful:action:Action::\"write\""),
        0,
        "Bob action pruned"
    );
    assert_eq!(
        count_resource("urn:ledgerful:resource:Doc::\"notes.txt\""),
        0,
        "Bob resource pruned"
    );
}
