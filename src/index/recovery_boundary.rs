//! Disposable commit-boundary oracles. Each test owns a `tempfile::tempdir()`.
//! A page ceiling is SQLite `max_page_count`, not a full volume and not power loss.
//!
//! Names keep the repo's `feature__condition__result` form.
#![allow(non_snake_case)]

mod env_guard {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/integration/common/env_guard.rs"
    ));
}
use env_guard::TempEnv;

use crate::config::model::Config;
use crate::index::orchestrator::lifecycle::{clear_project_data, insert_batch};
use crate::index::types::ProjectFile;
use crate::ledger::{Category, ChangeType, CommitRequest, TransactionManager, TransactionRequest};
use crate::search::{TantivySearchEngine, rebuild_tantivy_index};
use crate::semantic::chunker::AstChunk;
use crate::semantic::vector_store::VectorStore;
use crate::state::layout::{Layout, resolve_state_dir};
use crate::state::migrations::get_migrations_count;
use crate::state::storage::StorageManager;
use camino::{Utf8Path, Utf8PathBuf};
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[allow(dead_code)]
struct HomeGuard {
    userprofile: TempEnv,
    home: TempEnv,
}

impl HomeGuard {
    fn new(root: &Path) -> Self {
        let home = root.join("home");
        fs::create_dir_all(&home).expect("synthetic home inside the temp root");
        let home_s = home.to_string_lossy().into_owned();
        Self {
            userprofile: TempEnv::set("USERPROFILE", &home_s),
            home: TempEnv::set("HOME", &home_s),
        }
    }
}

fn error_chain(err: &dyn std::error::Error) -> String {
    let mut parts = Vec::new();
    let mut current = Some(err);
    while let Some(item) = current {
        parts.push(item.to_string());
        current = item.source();
    }
    parts.join(" | ")
}

fn layout_at(root: &Path) -> Layout {
    let utf8 = Utf8PathBuf::from_path_buf(root.to_path_buf()).expect("temp path is UTF-8");
    let layout = Layout::new(&utf8);
    layout.ensure_state_dir().expect("state dir");
    layout
}

fn open_storage(layout: &Layout) -> StorageManager {
    StorageManager::init_with_layout(layout).expect("init storage")
}

fn project_file(path: impl Into<String>) -> ProjectFile {
    ProjectFile {
        id: None,
        file_path: path.into(),
        language: Some("rust".to_string()),
        content_hash: None,
        git_blob_oid: None,
        file_size: Some(1),
        mtime_ns: Some(0),
        parser_version: "boundary-test".to_string(),
        parse_status: "OK".to_string(),
        last_indexed_at: "2026-10-23T00:00:00Z".to_string(),
    }
}

fn write_entity(root: &Path, relative: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("entity parent");
    }
    fs::write(&path, "fn boundary() {}\n").expect("entity file");
}

fn commit_one_ledger_row(storage: &mut StorageManager, root: &Path, entity: &str) {
    write_entity(root, entity);
    let mut tx = TransactionManager::new(storage, root.to_path_buf(), Config::default());
    let tx_id = tx
        .start_change(TransactionRequest {
            category: Category::Chore,
            entity: entity.to_string(),
            planned_action: Some("seed one committed ledger row".to_string()),
            ..Default::default()
        })
        .expect("start_change seeds one row");
    tx.commit_change(
        tx_id,
        CommitRequest {
            change_type: ChangeType::Modify,
            summary: "seed one committed ledger row".to_string(),
            reason: "recovery boundary oracle".to_string(),
            ..Default::default()
        },
        false,
    )
    .expect("commit_change seeds one row");
}

fn ledger_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM ledger_entries", [], |row| row.get(0))
        .expect("ledger_entries count")
}

fn file_count(conn: &Connection, path: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM project_files WHERE file_path = ?1",
        [path],
        |row| row.get(0),
    )
    .expect("project_files count")
}

fn pragma_i64(conn: &Connection, name: &str) -> i64 {
    conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
        .unwrap_or_else(|err| panic!("read PRAGMA {name}: {err}"))
}

fn set_page_ceiling(conn: &Connection) -> usize {
    let page_count = pragma_i64(conn, "page_count");
    let page_size = pragma_i64(conn, "page_size");
    assert!(
        page_count > 0,
        "page_count must be positive before the ceiling"
    );
    assert!(
        page_size > 0,
        "page_size must be positive before the ceiling"
    );
    let applied: i64 = conn
        .query_row(
            &format!("PRAGMA max_page_count = {page_count}"),
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|err| panic!("set max_page_count: {err}"));
    assert_eq!(
        applied, page_count,
        "SQLite page ceiling must equal the current page_count"
    );
    usize::try_from(page_count)
        .expect("page_count fits")
        .checked_mul(usize::try_from(page_size).expect("page_size fits"))
        .and_then(|bytes| bytes.checked_add(1))
        .expect("payload length fits")
}

fn checkpoint_truncate(conn: &Connection) {
    let busy: i64 = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
        .expect("wal_checkpoint");
    assert_eq!(busy, 0, "wal_checkpoint(TRUNCATE) must finish");
}

fn add_one_document(engine: &TantivySearchEngine) {
    let schema = engine.schema();
    let path_field = schema.get_field("path").expect("path field");
    let content_field = schema.get_field("content").expect("content field");
    let line_count_field = schema.get_field("line_count").expect("line_count field");
    let trigrams_field = schema.get_field("trigrams").expect("trigrams field");
    let content = "fn boundary_probe() {}";
    let trigrams = crate::search::trigram::extract_trigrams(content)
        .into_iter()
        .collect::<Vec<_>>()
        .join(" ");
    let mut writer = engine.get_writer(15_000_000).expect("get_writer");
    let mut doc = tantivy::TantivyDocument::default();
    doc.add_text(path_field, "src/boundary.rs");
    doc.add_text(content_field, content);
    doc.add_u64(line_count_field, 1);
    doc.add_text(trigrams_field, &trigrams);
    writer.add_document(doc).expect("add document");
    writer.commit().expect("tantivy commit");
    engine.reload_reader().expect("reload_reader");
}

fn delete_store_files(index_dir: &Path) {
    let mut removed = 0usize;
    for entry in fs::read_dir(index_dir).expect("read search index") {
        let entry = entry.expect("index entry");
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".store") {
            fs::remove_file(entry.path()).expect("delete .store");
            removed += 1;
        }
    }
    assert!(removed > 0, "expected at least one Tantivy .store file");
}

fn git(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git available")
}

fn assert_git(cwd: &Path, args: &[&str]) {
    let out = git(cwd, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_git_repo(main: &Path) {
    fs::create_dir_all(main).expect("main repo dir");
    assert_git(main, &["init", "-b", "main"]);
    assert_git(main, &["config", "user.email", "t@example.com"]);
    assert_git(main, &["config", "user.name", "t"]);
    let _ = git(main, &["config", "core.hooksPath", ".git/hooks"]);
    fs::write(main.join("README"), "x\n").expect("readme");
    assert_git(main, &["add", "README"]);
    assert_git(main, &["commit", "-m", "init"]);
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn assert_inside_temp(path: &Path, temp_root: &Path) {
    let path = canonical(path);
    let root = canonical(temp_root);
    assert!(
        path.starts_with(&root),
        "state path {} must stay inside {}",
        path.display(),
        root.display()
    );
}

struct WorktreePair {
    tmp: tempfile::TempDir,
    main: PathBuf,
    linked: PathBuf,
}

fn worktree_pair() -> WorktreePair {
    let tmp = tempfile::tempdir().expect("tempdir");
    let main = tmp.path().join("main");
    init_git_repo(&main);
    let linked = tmp.path().join("linked");
    let out = git(
        &main,
        &["worktree", "add", linked.to_str().expect("utf-8"), "HEAD"],
    );
    assert!(
        out.status.success(),
        "worktree add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    WorktreePair { tmp, main, linked }
}

#[test]
#[serial_test::serial(env)]
fn recovery_boundary__page_ceiling_batch__seed_remains() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::new(dir.path());
    let layout = layout_at(dir.path());
    let mut storage = open_storage(&layout);
    let seed = "src/seed.rs";
    insert_batch(&mut storage, &[project_file(seed)], &[vec![]], &[vec![]])
        .expect("seed insert_batch");
    commit_one_ledger_row(&mut storage, dir.path(), "src/ledger_seed.rs");
    let payload_len = set_page_ceiling(storage.get_connection());
    let second = "b".repeat(payload_len);
    let err = insert_batch(
        &mut storage,
        &[project_file(second.clone())],
        &[vec![]],
        &[vec![]],
    )
    .expect_err("page-ceiling insert must fail");
    let text = error_chain(err.as_ref());
    assert!(
        text.contains("database or disk is full"),
        "SQLite max_page_count ceiling (not a full volume) must report database or disk is full; got {text}"
    );
    let conn = storage.get_connection();
    assert_eq!(file_count(conn, seed), 1, "seed path remains");
    assert_eq!(file_count(conn, &second), 0, "ceiling path is absent");
    assert_eq!(ledger_count(conn), 1, "ledger_entries count is exactly 1");
}

#[test]
#[serial_test::serial(env)]
fn recovery_boundary__uncommitted_drop__seed_remains() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::new(dir.path());
    let layout = layout_at(dir.path());
    let mut storage = open_storage(&layout);
    let seed = "src/seed.rs";
    insert_batch(&mut storage, &[project_file(seed)], &[vec![]], &[vec![]])
        .expect("seed insert_batch");
    commit_one_ledger_row(&mut storage, dir.path(), "src/ledger_seed.rs");
    {
        let tx = storage
            .get_connection_mut()
            .unchecked_transaction()
            .expect("unchecked_transaction");
        tx.execute(
            "INSERT INTO project_files (file_path, language, content_hash, git_blob_oid, file_size, mtime_ns, parser_version, parse_status, last_indexed_at) VALUES (?1, 'rust', NULL, NULL, 1, 0, 'boundary-test', 'OK', '2026-10-23T00:00:00Z')",
            ["src/dropped.rs"],
        )
        .expect("insert dropped path");
        drop(tx);
    }
    storage.shutdown().expect("shutdown");
    let storage = open_storage(&layout);
    let conn = storage.get_connection();
    assert_eq!(file_count(conn, seed), 1, "seed path remains after reopen");
    assert_eq!(
        file_count(conn, "src/dropped.rs"),
        0,
        "dropped path is absent"
    );
    assert_eq!(ledger_count(conn), 1, "ledger_entries count is exactly 1");
}

#[test]
fn recovery_boundary__busy_lock__seed_remains_then_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let layout = layout_at(dir.path());
    let mut storage = open_storage(&layout);
    assert_eq!(
        pragma_i64(storage.get_connection(), "busy_timeout"),
        5000,
        "production init_at busy_timeout stays 5000"
    );
    let seed = "src/seed.rs";
    insert_batch(&mut storage, &[project_file(seed)], &[vec![]], &[vec![]])
        .expect("seed insert_batch");
    let db_path = layout.state_subdir().join("ledger.db");
    let holder = Connection::open(db_path.as_std_path()).expect("holder connection");
    holder
        .execute_batch("BEGIN IMMEDIATE")
        .expect("holder BEGIN IMMEDIATE");
    storage
        .get_connection_mut()
        .execute_batch("PRAGMA busy_timeout = 0;")
        .expect("busy_timeout 0 on the writer only");
    let err = insert_batch(
        &mut storage,
        &[project_file("src/locked.rs")],
        &[vec![]],
        &[vec![]],
    )
    .expect_err("locked insert must fail");
    let text = error_chain(err.as_ref());
    assert!(
        text.contains("database is locked"),
        "busy_timeout 0 under BEGIN IMMEDIATE must report database is locked; got {text}"
    );
    holder.execute_batch("ROLLBACK").expect("holder rollback");
    let conn = storage.get_connection();
    assert_eq!(file_count(conn, seed), 1, "seed path remains");
    assert_eq!(
        file_count(conn, "src/locked.rs"),
        0,
        "locked path is absent"
    );
    let selected: String = conn
        .query_row(
            "SELECT file_path FROM project_files WHERE file_path = ?1",
            [seed],
            |row| row.get(0),
        )
        .expect("select seed after release");
    assert_eq!(selected, seed, "select after rollback returns the seed");
}

#[test]
#[serial_test::serial(env)]
fn recovery_boundary__clear_derived__ledger_row_remains() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::new(dir.path());
    let layout = layout_at(dir.path());
    let mut storage = open_storage(&layout);
    commit_one_ledger_row(&mut storage, dir.path(), "src/ledger_seed.rs");
    insert_batch(
        &mut storage,
        &[project_file("src/derived.rs")],
        &[vec![]],
        &[vec![]],
    )
    .expect("derived file");
    clear_project_data(&mut storage).expect("clear_project_data");
    let conn = storage.get_connection();
    let files: i64 = conn
        .query_row("SELECT COUNT(*) FROM project_files", [], |row| row.get(0))
        .expect("project_files count");
    assert_eq!(files, 0, "project_files count is 0");
    assert_eq!(ledger_count(conn), 1, "ledger_entries count is exactly 1");
}

#[test]
#[serial_test::serial(env)]
fn recovery_boundary__page_ceiling_ledger__pending_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::new(dir.path());
    let layout = layout_at(dir.path());
    let mut storage = open_storage(&layout);
    write_entity(dir.path(), "src/first.rs");
    write_entity(dir.path(), "src/second.rs");
    let mut tx = TransactionManager::new(&mut storage, dir.path().to_path_buf(), Config::default());
    let first_id = tx
        .start_change(TransactionRequest {
            category: Category::Chore,
            entity: "src/first.rs".to_string(),
            planned_action: Some("first commit".to_string()),
            ..Default::default()
        })
        .expect("first start_change");
    tx.commit_change(
        first_id,
        CommitRequest {
            change_type: ChangeType::Modify,
            summary: "first committed row".to_string(),
            reason: "seed".to_string(),
            ..Default::default()
        },
        false,
    )
    .expect("first commit_change");
    let second_id = tx
        .start_change(TransactionRequest {
            category: Category::Chore,
            entity: "src/second.rs".to_string(),
            planned_action: Some("second commit".to_string()),
            ..Default::default()
        })
        .expect("second start_change is Ok before the ceiling");
    let prev_before: Option<String> = tx
        .get_connection()
        .query_row(
            "SELECT prev_hash FROM ledger_entries ORDER BY id ASC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("prev_hash before ceiling");
    let payload_len = set_page_ceiling(tx.get_connection());
    let err = tx
        .commit_change(
            second_id.clone(),
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: "s".repeat(payload_len),
                reason: "ceiling".to_string(),
                ..Default::default()
            },
            false,
        )
        .expect_err("ceiling commit must fail");
    let text = error_chain(&err);
    assert!(
        text.contains("database or disk is full"),
        "SQLite max_page_count ceiling (not a full volume) must report database or disk is full; got {text}"
    );
    let conn = tx.get_connection();
    assert_eq!(ledger_count(conn), 1, "ledger_entries count is exactly 1");
    let status: String = conn
        .query_row(
            "SELECT status FROM transactions WHERE tx_id = ?1",
            [&second_id],
            |row| row.get(0),
        )
        .expect("second transaction status");
    assert_eq!(status, "PENDING", "second transaction stays PENDING");
    let prev_after: Option<String> = conn
        .query_row(
            "SELECT prev_hash FROM ledger_entries ORDER BY id ASC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("prev_hash after ceiling");
    assert_eq!(prev_before, prev_after, "first row prev_hash is unchanged");
}

#[test]
#[serial_test::serial(env)]
fn recovery_boundary__tantivy_clear_stop__empty_index_ledger_remains() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::new(dir.path());
    let layout = layout_at(dir.path());
    let mut storage = open_storage(&layout);
    commit_one_ledger_row(&mut storage, dir.path(), "src/ledger_seed.rs");
    fs::write(dir.path().join("src").join("probe.rs"), "fn probe() {}\n").expect("probe file");
    let index_dir = layout.search_index_dir();
    {
        let engine = TantivySearchEngine::open_or_create(index_dir.as_std_path()).expect("open");
        add_one_document(&engine);
        assert_eq!(
            engine.document_count(),
            1,
            "document_count is 1 before clear"
        );
        assert!(
            engine.segment_count().expect("segment_count") > 0,
            "segment_count is greater than 0 before clear"
        );
        engine.clear().expect("clear");
    }
    {
        let engine = TantivySearchEngine::open_or_create(index_dir.as_std_path()).expect("reopen");
        assert_eq!(
            engine.document_count(),
            0,
            "document_count is 0 after clear and reopen"
        );
    }
    rebuild_tantivy_index(&layout).expect("rebuild_tantivy_index");
    {
        let engine =
            TantivySearchEngine::open_or_create(index_dir.as_std_path()).expect("after rebuild");
        assert!(
            engine.document_count() > 0,
            "rebuild of a one-file repo makes document_count greater than 0"
        );
    }
    assert_eq!(
        ledger_count(storage.get_connection()),
        1,
        "ledger_entries count stays exactly 1"
    );
}

#[test]
#[serial_test::serial(env)]
fn recovery_boundary__missing_segment__open_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::new(dir.path());
    let layout = layout_at(dir.path());
    let mut storage = open_storage(&layout);
    commit_one_ledger_row(&mut storage, dir.path(), "src/ledger_seed.rs");
    let index_dir = layout.search_index_dir();
    {
        let engine = TantivySearchEngine::open_or_create(index_dir.as_std_path()).expect("open");
        add_one_document(&engine);
        assert!(
            engine.document_count() > 0,
            "committed document is visible before the segment is removed"
        );
    }
    delete_store_files(index_dir.as_std_path());
    let opened = TantivySearchEngine::open_or_create(index_dir.as_std_path());
    let err = match opened {
        Ok(_) => panic!("open_or_create must fail when .store is missing"),
        Err(err) => err,
    };
    let text = error_chain(err.as_ref());
    assert!(
        text.contains("Files does not exist") && text.contains(".store"),
        "cross-platform open must report Files does not exist for the missing .store; got {text}"
    );
    assert_eq!(
        ledger_count(storage.get_connection()),
        1,
        "ledger_entries count is exactly 1"
    );
}

#[cfg(unix)]
#[test]
#[serial_test::serial(env)]
fn recovery_boundary__missing_segment__integrity_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::new(dir.path());
    let layout = layout_at(dir.path());
    let mut storage = open_storage(&layout);
    commit_one_ledger_row(&mut storage, dir.path(), "src/ledger_seed.rs");
    let index_dir = layout.search_index_dir();
    let engine = TantivySearchEngine::open_or_create(index_dir.as_std_path()).expect("open");
    add_one_document(&engine);
    delete_store_files(index_dir.as_std_path());
    let err = engine
        .verify_index_integrity(index_dir.as_std_path())
        .expect_err("integrity must fail while the engine is still open");
    let text = error_chain(err.as_ref());
    assert!(
        text.contains("Tantivy segment file missing"),
        "unix in-process integrity error on {} must contain Tantivy segment file missing; got {text}",
        std::env::consts::OS
    );
    assert_eq!(
        ledger_count(storage.get_connection()),
        1,
        "ledger_entries count is exactly 1"
    );
}

#[test]
#[serial_test::serial(env)]
fn recovery_boundary__bad_embedding_dimension__ledger_remains() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::new(dir.path());
    let layout = layout_at(dir.path());
    let mut storage = open_storage(&layout);
    commit_one_ledger_row(&mut storage, dir.path(), "src/ledger_seed.rs");
    let cozo = storage.cozo().expect("cozo opened with the ledger");
    let store = VectorStore::new_without_hnsw(cozo, 3).expect("vector store dim 3");
    let chunk = AstChunk {
        file_path: "t.rs".to_string(),
        name: "boundary_fn".to_string(),
        qualified_name: None,
        kind: crate::index::symbols::SymbolKind::Function,
        content: "fn boundary_fn() {}".to_string(),
        docstring: None,
        range: (0, 10),
        lines: (1, 1),
        offset: 0,
    };
    let err = store
        .index_chunks(vec![chunk], vec![vec![1.0, 0.0, 0.0, 0.0]])
        .expect_err("dimension mismatch must fail before a put");
    let text = error_chain(err.as_ref());
    assert!(
        text.contains("Embedding dimension mismatch"),
        "expected Embedding dimension mismatch; got {text}"
    );
    assert_eq!(
        ledger_count(storage.get_connection()),
        1,
        "ledger_entries count is exactly 1"
    );
}

#[test]
#[serial_test::serial(env)]
fn recovery_boundary__garbage_sqlite__bytes_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::new(dir.path());
    let layout = layout_at(dir.path());
    let mut storage = open_storage(&layout);
    commit_one_ledger_row(&mut storage, dir.path(), "src/ledger_seed.rs");
    checkpoint_truncate(storage.get_connection());
    let db_path = layout.state_subdir().join("ledger.db");
    storage.shutdown().expect("shutdown");
    let _ = fs::remove_file(layout.state_subdir().join("ledger.db-wal"));
    let _ = fs::remove_file(layout.state_subdir().join("ledger.db-shm"));
    let garbage = b"not-a-sqlite-database";
    fs::write(db_path.as_std_path(), garbage).expect("overwrite ledger.db");
    let err = match StorageManager::init_with_layout(&layout) {
        Ok(_) => panic!("garbage sqlite must fail init"),
        Err(err) => err,
    };
    let text = error_chain(err.as_ref());
    assert!(
        text.contains("file is not a database"),
        "garbage ledger.db must report file is not a database; got {text}"
    );
    let bytes = fs::read(db_path.as_std_path()).expect("read overwritten db");
    assert!(
        !bytes.starts_with(b"SQLite format 3"),
        "overwritten ledger.db must not be replaced with a SQLite header"
    );
    assert_eq!(bytes, garbage, "overwritten ledger.db bytes stay unchanged");
}

#[test]
#[serial_test::serial(env)]
fn recovery_boundary__user_version_too_high__row_remains() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::new(dir.path());
    let layout = layout_at(dir.path());
    let mut storage = open_storage(&layout);
    commit_one_ledger_row(&mut storage, dir.path(), "src/ledger_seed.rs");
    let target = i64::try_from(get_migrations_count()).expect("migration count") + 1;
    storage
        .get_connection()
        .execute_batch(&format!("PRAGMA user_version = {target};"))
        .expect("set user_version");
    assert_eq!(
        pragma_i64(storage.get_connection(), "user_version"),
        target,
        "user_version was stored"
    );
    checkpoint_truncate(storage.get_connection());
    let db_path = layout.state_subdir().join("ledger.db");
    storage.shutdown().expect("shutdown");
    let err = match StorageManager::init_with_layout(&layout) {
        Ok(_) => panic!("too-high user_version must fail init"),
        Err(err) => err,
    };
    let text = error_chain(err.as_ref());
    assert!(
        text.contains("Attempt to migrate a database with a migration number that is too high"),
        "too-high user_version display; got {text}"
    );
    let conn = Connection::open(db_path.as_std_path()).expect("raw open without migrations");
    assert_eq!(
        ledger_count(&conn),
        1,
        "a connection that does not migrate still reads the one ledger row"
    );
}

#[test]
fn recovery_boundary__failed_migration_sql__version_stays() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("throwaway.db");
    let mut conn = Connection::open(&path).expect("throwaway connection");
    let migrations = rusqlite_migration::Migrations::new(vec![
        rusqlite_migration::M::up(
            "CREATE TABLE boundary_probe (id INTEGER PRIMARY KEY); INSERT INTO boundary_probe (id) VALUES (1);",
        ),
        rusqlite_migration::M::up("NOT VALID SQL"),
    ]);
    let err = migrations
        .to_latest(&mut conn)
        .expect_err("invalid second step must fail to_latest");
    let text = error_chain(&err);
    assert!(
        text.contains("NOT VALID SQL"),
        "failed migration display must contain NOT VALID SQL; got {text}"
    );
    assert_eq!(
        pragma_i64(&conn, "user_version"),
        0,
        "user_version stays 0 because the batch rolls back"
    );
    let present: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'boundary_probe'",
            [],
            |row| row.get(0),
        )
        .expect("sqlite_master");
    assert_eq!(present, 0, "boundary_probe is absent after rollback");
}

#[test]
#[serial_test::serial(env)]
fn recovery_boundary__shared_worktree__second_writer_busy() {
    let pair = worktree_pair();
    let _clear = TempEnv::remove("LEDGERFUL_STATE_DIR");
    let main_repo = gix::discover(&pair.main).expect("main repo");
    let linked_repo = gix::discover(&pair.linked).expect("linked repo");
    let main_state = resolve_state_dir(&main_repo).expect("main state");
    let linked_state = resolve_state_dir(&linked_repo).expect("linked state");
    fs::create_dir_all(main_state.as_std_path()).expect("state dir exists for compare");
    assert_eq!(
        canonical(main_state.as_std_path()),
        canonical(linked_state.as_std_path()),
        "both resolve_state_dir results are the main checkout .ledgerful"
    );
    assert_eq!(
        canonical(main_state.as_std_path()),
        canonical(&pair.main.join(".ledgerful")),
        "shared state is the main checkout .ledgerful"
    );
    assert_inside_temp(main_state.as_std_path(), pair.tmp.path());
    let main_utf8 = Utf8Path::from_path(&pair.main).expect("utf-8");
    let layout = Layout::from_roots(main_utf8, &main_state);
    layout.ensure_state_dir().expect("ensure state");
    let sentinel = "src/sentinel.rs";
    let mut first = open_storage(&layout);
    insert_batch(&mut first, &[project_file(sentinel)], &[vec![]], &[vec![]]).expect("sentinel");
    first.shutdown().expect("close cozo before the second init");
    let mut second = StorageManager::init_with_layout(&layout)
        .expect("second init_with_layout is Ok while no BEGIN IMMEDIATE is held");
    let db_path = layout.state_subdir().join("ledger.db");
    let holder = Connection::open(db_path.as_std_path()).expect("holder");
    holder
        .execute_batch("BEGIN IMMEDIATE")
        .expect("BEGIN IMMEDIATE");
    second
        .get_connection_mut()
        .execute_batch("PRAGMA busy_timeout = 0;")
        .expect("busy_timeout 0");
    let err = insert_batch(
        &mut second,
        &[project_file("src/other.rs")],
        &[vec![]],
        &[vec![]],
    )
    .expect_err("second writer must fail");
    let text = error_chain(err.as_ref());
    assert!(
        text.contains("database is locked"),
        "shared worktree writer must report database is locked; got {text}"
    );
    holder.execute_batch("ROLLBACK").expect("rollback");
    let conn = second.get_connection();
    assert_eq!(file_count(conn, sentinel), 1, "sentinel remains");
    assert_eq!(file_count(conn, "src/other.rs"), 0, "other path is absent");
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM project_files", [], |row| row.get(0))
        .expect("project_files");
    assert_eq!(total, 1, "the sentinel is the only row of its kind");
}

#[test]
#[serial_test::serial(env)]
fn recovery_boundary__state_override__separate_db() {
    let pair = worktree_pair();
    let _clear = TempEnv::remove("LEDGERFUL_STATE_DIR");
    let main_repo = gix::discover(&pair.main).expect("main repo");
    let linked_repo = gix::discover(&pair.linked).expect("linked repo");
    let main_state = resolve_state_dir(&main_repo).expect("main state");
    fs::create_dir_all(main_state.as_std_path()).expect("main state dir");
    assert_inside_temp(main_state.as_std_path(), pair.tmp.path());
    let main_utf8 = Utf8Path::from_path(&pair.main).expect("utf-8");
    let main_layout = Layout::from_roots(main_utf8, &main_state);
    main_layout.ensure_state_dir().expect("ensure main state");
    let sentinel = "src/sentinel.rs";
    let mut main_storage = open_storage(&main_layout);
    insert_batch(
        &mut main_storage,
        &[project_file(sentinel)],
        &[vec![]],
        &[vec![]],
    )
    .expect("main sentinel");
    main_storage.shutdown().expect("shutdown main");

    let override_dir = pair.tmp.path().join("custom-state");
    fs::create_dir_all(&override_dir).expect("override dir");
    let override_s = override_dir.to_string_lossy().into_owned();
    let _override = TempEnv::set("LEDGERFUL_STATE_DIR", &override_s);
    let resolved = resolve_state_dir(&linked_repo).expect("override resolve");
    assert_eq!(
        canonical(resolved.as_std_path()),
        canonical(&override_dir),
        "resolve_state_dir is the absolute override inside the temp root"
    );
    assert_inside_temp(resolved.as_std_path(), pair.tmp.path());
    let linked_utf8 = Utf8Path::from_path(&pair.linked).expect("utf-8");
    let override_layout = Layout::from_roots(linked_utf8, &resolved);
    override_layout.ensure_state_dir().expect("ensure override");
    let override_storage = open_storage(&override_layout);
    assert_eq!(
        file_count(override_storage.get_connection(), sentinel),
        0,
        "the new ledger.db does not contain the main sentinel"
    );
}

#[cfg(unix)]
#[test]
fn recovery_boundary__readonly_db_file__write_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let layout = layout_at(dir.path());
    let mut storage = open_storage(&layout);
    let seed = "src/seed.rs";
    insert_batch(&mut storage, &[project_file(seed)], &[vec![]], &[vec![]])
        .expect("seed insert_batch");
    checkpoint_truncate(storage.get_connection());
    storage.shutdown().expect("drop the writer");
    let read = StorageManager::open_read_only(&layout).expect("open_read_only is Ok");
    let mut read = read;
    let err = insert_batch(
        &mut read,
        &[project_file("src/readonly.rs")],
        &[vec![]],
        &[vec![]],
    )
    .expect_err("insert on a read-only manager must fail");
    let text = error_chain(err.as_ref());
    assert!(
        text.contains("readonly"),
        "readonly write on {} must contain readonly; got {text}",
        std::env::consts::OS
    );
    let selected: String = read
        .get_connection()
        .query_row(
            "SELECT file_path FROM project_files WHERE file_path = ?1",
            [seed],
            |row| row.get(0),
        )
        .expect("select seed on the same connection");
    assert_eq!(selected, seed, "seed row is unchanged");
}
