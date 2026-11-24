use super::*;
use crate::commands::baseline::{BaselineScope, FileEdge, InputCompleteness, compare_edge_sets};
use crate::state::storage::change_evidence::writes::ScopeExpectation;
use crate::state::{
    migrations::get_migrations, storage::change_evidence::resolve_or_provision_write_tx,
};

const BASE: &str = "1111111111111111111111111111111111111111";
const HEAD: &str = "2222222222222222222222222222222222222222";
const TIME: &str = "2026-11-23T00:00:00Z";

fn side(head: &str, edge: bool) -> FileEdgeSet {
    FileEdgeSet {
        identity: BaselineIdentity {
            baseline_schema: BASELINE_SCHEMA,
            payload_kind: PAYLOAD_KIND.to_string(),
            scope: BaselineScope::repo(),
            edge_sources: vec!["native".to_string()],
            ledgerful_version: "0.2.18 (abcdef0)".to_string(),
            parser_version: "1".to_string(),
            index_head_hash: Some(head.to_string()),
            input_digest: content_digest("fixture inputs"),
            input_completeness: InputCompleteness::complete(),
        },
        coverage: EdgeCoverage::default(),
        edges: if edge {
            vec![FileEdge {
                src_path: "src/a.rs".to_string(),
                dst_path: "src/b.rs".to_string(),
                weight: 1,
            }]
        } else {
            Vec::new()
        },
    }
}

fn evidence(head: &str) -> PreparedEvidence {
    let baseline = side(BASE, false);
    let current = side(head, true);
    let delta = compare_edge_sets(&baseline, &current);
    let comparison = PreparedComparison::from_delta(&baseline, &current, delta).unwrap();
    let index = IndexGeneration {
        head_hash: Some(head.to_string()),
        parser_version: Some("1".to_string()),
        last_indexed_at: Some(TIME.to_string()),
    };
    PreparedEvidence {
        git: PreparedGit::Unavailable,
        comparison,
        index_marker: Some(PreparedIndexMarker::from_generation(&index, 7, [3; 32]).unwrap()),
    }
}

fn put(
    conn: &Connection,
    scope: uuid::Uuid,
    head: &str,
    digest: &str,
    evidence: &PreparedEvidence,
) {
    let raw = serde_json::to_string(evidence).unwrap();
    let body_digest = content_digest(&raw);
    conn.execute(
            "INSERT INTO change_evidence_prepared
             (repo_scope,head,base,baseline_name,baseline_digest,generation,payload_version,
              payload_json,payload_digest,captured_at)
             VALUES (?1,?2,?3,'fixture',?4,1,1,?5,?6,?7)
             ON CONFLICT(repo_scope,head,base,baseline_name,baseline_digest)
             DO UPDATE SET payload_json=excluded.payload_json,payload_digest=excluded.payload_digest",
            rusqlite::params![scope.to_string(), head, BASE, digest, raw, body_digest, TIME],
        ).unwrap();
}

#[test]
fn verifies_real_prepared_comparator_and_rejects_rehashed_contradictions() {
    let mut conn = Connection::open_in_memory().unwrap();
    get_migrations().to_latest(&mut conn).unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let guard = [8; 32];
    let scope = resolve_or_provision_write_tx(&tx, &guard, TIME).unwrap();
    let ownership = WriteScope {
        guard: &guard,
        expected: ScopeExpectation::Existing(scope),
    };
    let digest = content_digest("strict baseline");
    let key = ExactPreparedKey::new(HEAD, BASE, "fixture", &digest).unwrap();

    let actual = evidence(HEAD);
    put(&tx, scope, HEAD, &digest, &actual);
    assert!(read_prepared_in_tx(&tx, ownership, &key).unwrap().is_some());

    let mut invalid = evidence(HEAD);
    let PreparedComparison::Comparable(value) = &mut invalid.comparison else {
        unreachable!()
    };
    value.sample = ComparisonSample {
        complete: false,
        omitted: 1,
    };
    put(&tx, scope, HEAD, &digest, &invalid);
    assert!(matches!(
        read_prepared_in_tx(&tx, ownership, &key),
        Err(E::Corrupt)
    ));

    let mut invalid = evidence(HEAD);
    let PreparedComparison::Comparable(value) = &mut invalid.comparison else {
        unreachable!()
    };
    value.items[0].class = DeltaClass::Worsened;
    value.items[0].baseline_weight = Some(1);
    value.items[0].current_weight = Some(2);
    put(&tx, scope, HEAD, &digest, &invalid);
    assert!(matches!(
        read_prepared_in_tx(&tx, ownership, &key),
        Err(E::Corrupt)
    ));

    let mut invalid = evidence(HEAD);
    let PreparedComparison::Comparable(value) = &mut invalid.comparison else {
        unreachable!()
    };
    value.regression = Regression::None;
    put(&tx, scope, HEAD, &digest, &invalid);
    assert!(matches!(
        read_prepared_in_tx(&tx, ownership, &key),
        Err(E::Corrupt)
    ));
    tx.commit().unwrap();
}

#[test]
fn saved_incomparable_refuses_current_recheck_after_index_replacement() {
    let mut conn = Connection::open_in_memory().unwrap();
    get_migrations().to_latest(&mut conn).unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let guard = [8; 32];
    let scope = resolve_or_provision_write_tx(&tx, &guard, TIME).unwrap();
    let ownership = WriteScope {
        guard: &guard,
        expected: ScopeExpectation::Existing(scope),
    };
    let digest = content_digest("strict baseline");
    let key = ExactPreparedKey::new(HEAD, BASE, "fixture", &digest).unwrap();
    tx.execute(
        "INSERT INTO index_metadata(key,value) VALUES ('head_hash',?1),('parser_version','1'),('last_indexed_at',?2)",
        (BASE, TIME),
    ).unwrap();

    let baseline = side(BASE, false);
    let mut current = side(BASE, false);
    current.identity.input_completeness =
        InputCompleteness::incomplete(vec!["CONTENT_DRIFT".into()]);
    let delta = compare_edge_sets(&baseline, &current);
    let comparison = PreparedComparison::from_delta(&baseline, &current, delta).unwrap();
    assert!(matches!(comparison, PreparedComparison::Incomparable(_)));
    let generation = read_index_generation(&tx).unwrap();
    let marker = PreparedIndexMarker::from_generation(&generation, 7, [3; 32]).unwrap();
    let evidence = PreparedEvidence {
        git: PreparedGit::Unavailable,
        comparison,
        index_marker: Some(marker),
    };
    put(&tx, scope, HEAD, &digest, &evidence);
    let observed = read_prepared_in_tx(&tx, ownership, &key).unwrap().unwrap();
    assert!(matches!(
        recheck_prepared_in_tx(&tx, ownership, &key, &observed),
        Ok(PreparedRecheck::Current)
    ));
    tx.execute(
        "UPDATE index_metadata SET value=?1 WHERE key='head_hash'",
        [HEAD],
    )
    .unwrap();
    assert!(matches!(
        recheck_prepared_in_tx(&tx, ownership, &key, &observed),
        Ok(PreparedRecheck::IndexChanged)
    ));
    tx.commit().unwrap();
}

#[test]
fn prepared_lookup_keeps_full_commits_with_the_same_prefix_distinct() {
    const SAME_PREFIX: &str = "2222222233333333333333333333333333333333";
    let mut conn = Connection::open_in_memory().unwrap();
    get_migrations().to_latest(&mut conn).unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let guard = [8; 32];
    let scope = resolve_or_provision_write_tx(&tx, &guard, TIME).unwrap();
    let ownership = WriteScope {
        guard: &guard,
        expected: ScopeExpectation::Existing(scope),
    };
    let digest = content_digest("strict baseline");
    let other = ExactPreparedKey::new(SAME_PREFIX, BASE, "fixture", &digest).unwrap();
    put(&tx, scope, HEAD, &digest, &evidence(HEAD));
    assert!(
        read_prepared_in_tx(&tx, ownership, &other)
            .unwrap()
            .is_none()
    );
    put(&tx, scope, SAME_PREFIX, &digest, &evidence(SAME_PREFIX));
    for head in [HEAD, SAME_PREFIX] {
        let key = ExactPreparedKey::new(head, BASE, "fixture", &digest).unwrap();
        let record = read_prepared_in_tx(&tx, ownership, &key).unwrap().unwrap();
        let PreparedComparison::Comparable(result) = record.evidence.comparison else {
            panic!("the exact prepared record should contain its own comparison");
        };
        assert_eq!(
            result.current_identity.index_head_hash.as_deref(),
            Some(head)
        );
    }
    tx.commit().unwrap();
}

#[test]
fn partial_git_reason_preserves_existing_payloads_and_round_trips_new_ones() {
    let old = r#"{"status":"partial","files":[],"total":1,"omitted":1}"#;
    let parsed: PreparedGit = serde_json::from_str(old).unwrap();
    assert_eq!(serde_json::to_string(&parsed).unwrap(), old);

    let new = r#"{"status":"partial","files":[],"total":1,"omitted":1,"reasonCode":"unsupported-tree-path"}"#;
    let parsed: PreparedGit = serde_json::from_str(new).unwrap();
    assert_eq!(serde_json::to_string(&parsed).unwrap(), new);
    parsed.check().unwrap();
}
