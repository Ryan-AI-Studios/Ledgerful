use camino::Utf8PathBuf;
use ledgerful::config::model::Config;
use ledgerful::index::graph_loader::build_native_graph;
use ledgerful::index::languages::Language;
use ledgerful::index::orchestrator::{ProjectIndexer, SUPPORTED_EXTENSIONS};
use ledgerful::state::storage::StorageManager;
use std::fs;

use crate::common::setup_git_repo;

const USER_JAVA: &str = r#"package com.example;
public class User {
  public void create(boolean a, boolean b) {
    if (a && b) {
      save();
    }
  }
  private void save() {}
}
"#;

const STORE_JAVA: &str = r#"package com.example;
public class Store {
  public void save() {}
  public void save(int code) {}
  public void callOn(Store obj) {
    obj.save();
    save();
  }
}
"#;

const WIDGET_JAVA: &str = r#"package com.example;
public class Widget {
  Widget() {}
  void make() {
    new Widget();
    new Ghost();
    new Dual();
  }
}
class Dual {
  Dual() {}
  Dual(int x) {}
}
"#;

#[test]
fn java_index_user_and_store_fixture() {
    assert!(
        SUPPORTED_EXTENSIONS.contains(&"java"),
        "SUPPORTED_EXTENSIONS must include java"
    );

    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    setup_git_repo(root.as_std_path());
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("User.java"), USER_JAVA).unwrap();
    fs::write(src.join("Store.java"), STORE_JAVA).unwrap();
    fs::write(src.join("Widget.java"), WIDGET_JAVA).unwrap();

    let state = root.join(".ledgerful_java").join("state");
    fs::create_dir_all(&state).unwrap();
    let db = state.join("ledger.db");
    let storage = StorageManager::init(db.as_std_path()).unwrap();
    let mut indexer = ProjectIndexer::new(storage, root.clone(), Config::default());
    indexer.full_index().expect("full_index java fixture");
    indexer.build_call_graph().expect("build_call_graph");
    {
        let cozo = indexer.cozo().expect("CozoDB should be available");
        build_native_graph(indexer.storage(), cozo, "full", &Config::default())
            .expect("build_native_graph");

        let nodes = cozo
            .run_script("?[id, label] := *node{id, label}")
            .expect("query nodes");
        let create_urn = "urn:ledgerful:symbol:com.example.User.create";
        let hit = nodes.rows.iter().any(|row| {
            matches!(
                (row.first(), row.get(1)),
                (
                    Some(cozo::DataValue::Str(id)),
                    Some(cozo::DataValue::Str(label))
                ) if &**id == create_urn && &**label == "create"
            )
        });
        assert!(
            hit,
            "Cozo *node must contain {create_urn} label create; rows={:?}",
            nodes.rows
        );
    }

    let conn = indexer.storage().get_connection();
    let java_lang = format!("{:?}", Language::Java);
    let mut files = conn
        .prepare("SELECT file_path, language, parse_status FROM project_files ORDER BY file_path")
        .unwrap();
    let file_rows: Vec<(String, String, String)> = files
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    drop(files);
    assert!(
        file_rows.iter().any(|(p, lang, status)| {
            p.ends_with("User.java") && lang == &java_lang && status == "OK"
        }),
        "User.java language={java_lang} parse_status=OK; got {file_rows:?}"
    );
    assert!(
        file_rows.iter().any(|(p, lang, status)| {
            p.ends_with("Store.java") && lang == &java_lang && status == "OK"
        }),
        "Store.java language={java_lang} parse_status=OK; got {file_rows:?}"
    );

    let mut symbols = conn
        .prepare(
            "SELECT file_id, qualified_name, symbol_name, symbol_kind, is_public, line_start, byte_start \
             FROM project_symbols ORDER BY id",
        )
        .unwrap();
    let symbol_rows: Vec<(i64, String, String, String, i32, i32, i32)> = symbols
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    drop(symbols);

    let mut by_file: std::collections::BTreeMap<i64, Vec<_>> = std::collections::BTreeMap::new();
    for row in &symbol_rows {
        by_file.entry(row.0).or_default().push(row.clone());
    }
    for (file_id, rows) in &by_file {
        let mut expected = rows.clone();
        expected.sort_by(|a, b| {
            a.2.cmp(&b.2)
                .then(a.3.cmp(&b.3))
                .then(a.1.cmp(&b.1))
                .then(a.5.cmp(&b.5))
                .then(a.6.cmp(&b.6))
        });
        assert_eq!(
            rows, &expected,
            "file_id {file_id} insertion order must match §5.1 sort (name, kind, QN, line, byte); got {rows:?}"
        );
    }

    assert!(
        symbol_rows
            .iter()
            .any(|(_, qn, _, kind, pub_, _, _)| qn == "com.example.User"
                && kind == "Class"
                && *pub_ != 0)
    );
    assert!(symbol_rows.iter().any(|(_, qn, name, kind, pub_, _, _)| {
        qn == "com.example.User.create" && name == "create" && kind == "Method" && *pub_ != 0
    }));
    assert!(symbol_rows.iter().any(|(_, qn, name, kind, pub_, _, _)| {
        qn == "com.example.User.save" && name == "save" && kind == "Method" && *pub_ == 0
    }));

    let store_saves: Vec<_> = symbol_rows
        .iter()
        .filter(|(_, qn, name, kind, _, _, _)| {
            qn == "com.example.Store.save" && name == "save" && kind == "Method"
        })
        .collect();
    assert_eq!(
        store_saves.len(),
        1,
        "overload collapse to one Store.save row; got {symbol_rows:?}"
    );
    assert_eq!(
        store_saves[0].5, 4,
        "later save(int) span wins the upsert; row={:?}",
        store_saves[0]
    );

    let mut edges = conn
        .prepare(
            "SELECT caller.qualified_name, se.resolution_status, se.unresolved_callee, \
                    callee.qualified_name \
             FROM structural_edges se \
             JOIN project_symbols caller ON se.caller_symbol_id = caller.id \
             LEFT JOIN project_symbols callee ON se.callee_symbol_id = callee.id",
        )
        .unwrap();
    let edge_rows: Vec<(String, String, Option<String>, Option<String>)> = edges
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    drop(edges);

    assert!(
        edge_rows.iter().any(|(caller, status, _, callee)| {
            caller == "com.example.User.create"
                && status == "RESOLVED"
                && callee.as_deref() == Some("com.example.User.save")
        }),
        "create → save must resolve; edges={edge_rows:?}"
    );
    assert!(
        edge_rows.iter().any(|(caller, status, unresolved, _)| {
            caller == "com.example.Store.callOn"
                && status == "UNRESOLVED"
                && unresolved.as_deref() == Some("obj.save")
        }),
        "obj.save() stays UNRESOLVED; edges={edge_rows:?}"
    );
    assert!(
        edge_rows.iter().any(|(caller, status, unresolved, _)| {
            caller == "com.example.Store.callOn"
                && status == "UNRESOLVED"
                && unresolved.as_deref() == Some("save.overload")
        }),
        "bare overload save() stays UNRESOLVED; edges={edge_rows:?}"
    );
    assert!(
        edge_rows.iter().any(|(caller, status, _, callee)| {
            caller == "com.example.Widget.make"
                && status == "RESOLVED"
                && callee.as_deref() == Some("com.example.Widget.<init>")
        }),
        "unique new Widget() must persist-graph resolve; edges={edge_rows:?}"
    );
    assert!(
        edge_rows.iter().any(|(caller, status, unresolved, _)| {
            caller == "com.example.Widget.make"
                && status == "UNRESOLVED"
                && unresolved.as_deref() == Some("new.Ghost")
        }),
        "new Ghost() with no same-file type stays UNRESOLVED; edges={edge_rows:?}"
    );
    assert!(
        edge_rows.iter().any(|(caller, status, unresolved, _)| {
            caller == "com.example.Widget.make"
                && status == "UNRESOLVED"
                && unresolved.as_deref() == Some("new.Dual")
        }),
        "two-constructor new Dual() stays UNRESOLVED; edges={edge_rows:?}"
    );
}
