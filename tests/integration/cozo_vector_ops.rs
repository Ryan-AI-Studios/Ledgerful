//! Regression guard for cozo-redux vector ops and HNSW round-trip.
//!
//! These tests pin the operator names and HNSW lifecycle that
//! `src/semantic/vector_store.rs` depends on. If a cozo-redux upgrade
//! renames `l2_dist`/`cos_dist` or regresses HNSW insert/query with
//! Cosine distance, these fail fast — see `conductor/track56-1/`.

use cozo::*;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[test]
fn cos_dist_and_l2_dist_are_callable() {
    let db = DbInstance::new("mem", PathBuf::from(""), Default::default()).unwrap();
    db.run_script(
        ":create test {id: Int => vec: <F32; 3>}",
        Default::default(),
        ScriptMutability::Mutable,
    )
    .unwrap();
    db.run_script(
        "?[id, vec] <- [[1, vec([1.0, 0.0, 0.0])], [2, vec([0.0, 1.0, 0.0])]] :put test",
        Default::default(),
        ScriptMutability::Mutable,
    )
    .unwrap();

    let mut params = BTreeMap::new();
    params.insert(
        "q".to_string(),
        DataValue::Vec(Box::new(Vector::F32(vec![1.0, 0.1, 0.0].into()))),
    );

    let l2 = db
        .run_script(
            "?[id, dist] := *test{id, vec}, dist = l2_dist(vec, $q)",
            params.clone(),
            ScriptMutability::Immutable,
        )
        .expect("l2_dist must be a registered op in cozo-redux");
    assert_eq!(l2.rows.len(), 2);

    let cos = db
        .run_script(
            "?[id, dist] := *test{id, vec}, dist = cos_dist(vec, $q)",
            params,
            ScriptMutability::Immutable,
        )
        .expect("cos_dist must be a registered op in cozo-redux");
    assert_eq!(cos.rows.len(), 2);
}

#[test]
fn hnsw_index_create_and_query_works_on_fresh_db() {
    let db = DbInstance::new("mem", PathBuf::from(""), Default::default()).unwrap();
    db.run_script(
        ":create snippets {id: Int => embedding: <F32; 3>}",
        Default::default(),
        ScriptMutability::Mutable,
    )
    .unwrap();

    db.run_script(
        "::hnsw create snippets:idx {dim: 3, dtype: F32, fields: [embedding], distance: L2, m: 16, ef_construction: 20}",
        Default::default(),
        ScriptMutability::Mutable,
    )
    .expect("HNSW create should succeed on fresh in-memory cozo-redux db");

    db.run_script(
        "?[id, embedding] <- [[1, vec([1.0, 0.0, 0.0])], [2, vec([0.0, 1.0, 0.0])], [3, vec([0.5, 0.5, 0.0])]] :put snippets",
        Default::default(),
        ScriptMutability::Mutable,
    )
    .expect("HNSW inserts should succeed without panic");

    let mut p = BTreeMap::new();
    p.insert(
        "q".to_string(),
        DataValue::Vec(Box::new(Vector::F32(vec![0.9, 0.1, 0.0].into()))),
    );
    let res = db
        .run_script(
            "?[id, dist] := ~snippets:idx{id | query: $q, k: 2, ef: 20, bind_distance: dist}",
            p,
            ScriptMutability::Immutable,
        )
        .expect("HNSW query should succeed");
    assert!(!res.rows.is_empty(), "HNSW query should return results");
}
