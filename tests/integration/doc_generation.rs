use camino::Utf8Path;
use std::fs;
use tempfile::tempdir;

use crate::common::{DirGuard, setup_git_repo};

#[test]
fn test_doc_generation_integration() {
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());

    // Create source files
    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("a.rs"), "pub fn foo() {}\n").unwrap();
    fs::write(src_dir.join("b.rs"), "pub fn bar() {}\n").unwrap();

    // Pre-populate CozoDB in a scoped block so it is dropped before execute_index
    {
        let state_dir = root.join(".ledgerful").join("state");
        fs::create_dir_all(&state_dir).unwrap();
        let cozo_path = state_dir.join("ledger.cozo");
        let cozo =
            ledgerful::state::storage_cozo::CozoStorage::new(cozo_path.as_std_path()).unwrap();

        cozo.run_script(
            "?[id, label, category, risk_score, metadata] <- [
                ['src/a.rs', 'src/a.rs', 'file', 0.0, {}],
                ['src/b.rs', 'src/b.rs', 'file', 0.0, {}]
            ] :put node",
        )
        .unwrap();

        cozo.run_script(
            "?[id, file_path, qualified_name, symbol_name, symbol_kind, is_public, line_start, line_end] <- [
                [1, 'src/a.rs', 'foo', 'foo', 'fn', true, 1, 2],
                [2, 'src/b.rs', 'bar', 'bar', 'fn', true, 1, 2]
            ] :put project_symbol",
        )
        .unwrap();

        cozo.run_script(
            "?[source, target, relation, confidence, provenance_id] <- [
                ['foo', 'bar', 'calls', 1.0, 'tx1']
            ] :put edge",
        )
        .unwrap();
    } // cozo dropped here, releasing sled lock

    let _guard = DirGuard::from_utf8(root);

    let result = ledgerful::commands::index::execute_index(ledgerful::commands::index::IndexArgs {
        export_docs: true,
        ..Default::default()
    });

    assert!(result.is_ok(), "execute_index failed: {:?}", result);

    let docs_dir = root.join(".ledgerful").join("docs");
    let dep_graph = docs_dir.join("dependency_graph.md");
    let symbol_table = docs_dir.join("symbol_table.md");
    let module_summary = docs_dir.join("module_summary.md");

    assert!(dep_graph.exists(), "dependency_graph.md should exist");
    assert!(symbol_table.exists(), "symbol_table.md should exist");
    assert!(module_summary.exists(), "module_summary.md should exist");

    let dep_content = fs::read_to_string(&dep_graph).unwrap();
    let sym_content = fs::read_to_string(&symbol_table).unwrap();
    let mod_content = fs::read_to_string(&module_summary).unwrap();

    assert!(
        !dep_content.is_empty(),
        "dependency_graph.md should not be empty"
    );
    assert!(
        !sym_content.is_empty(),
        "symbol_table.md should not be empty"
    );
    assert!(
        !mod_content.is_empty(),
        "module_summary.md should not be empty"
    );

    // Verify expected content
    assert!(
        dep_content.contains("graph TD"),
        "dependency_graph should contain Mermaid header"
    );
    assert!(
        sym_content.contains("| Qualified Name |"),
        "symbol_table should contain table header"
    );
    assert!(
        mod_content.contains("src"),
        "module_summary should list src module"
    );
}
