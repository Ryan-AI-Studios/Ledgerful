//! Run-scoped source content cache for extractors.
//!
//! Keys are normalized relative paths (`/` slashes). Never keyed by `file_id`
//! (assigned at SQLite insert, after parse). Not process-lifetime.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// Load source for `file_path`: cache hit uses the stored `Arc<str>` (no disk
/// read); miss falls back to `std::fs::read_to_string`.
pub(crate) fn load_source_content(
    cache: Option<&HashMap<String, Arc<str>>>,
    file_path: &str,
    repo_path: &Path,
) -> std::io::Result<Arc<str>> {
    let key = file_path.replace('\\', "/");
    if let Some(cache) = cache
        && let Some(content) = cache.get(&key)
    {
        return Ok(Arc::clone(content));
    }
    std::fs::read_to_string(repo_path.join(&key)).map(Arc::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::call_graph::CallGraphBuilder;
    use crate::index::data_models::DataModelExtractor;
    use crate::index::observability::ObservabilityExtractor;
    use crate::index::routes::RouteExtractor;
    use crate::index::test_mapping::TestMapper;
    use crate::state::migrations::get_migrations;
    use crate::state::storage::StorageManager;
    use rusqlite::Connection;
    use std::fs;
    use std::path::Path;

    fn in_memory_storage() -> StorageManager {
        let conn = Connection::open_in_memory().unwrap();
        let mut conn = conn;
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    const LIB_RS: &str = r#"use axum::Router;
use axum::routing::get;
use sqlx::FromRow;

#[derive(FromRow)]
pub struct User {
    pub id: i64,
    pub name: String,
}

fn helper() {}

fn caller() {
    helper();
    tracing::info!("hello");
}

fn test_inline() {
    helper();
}

async fn get_users() {}

fn app() -> Router {
    Router::new().route("/users", get(get_users))
}
"#;

    const TEST_RS: &str = r#"use crate::helper;

fn test_helper() {}
"#;

    fn seed_files_and_symbols(storage: &StorageManager) {
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            ("src/lib.rs", "Rust", "h1", 200, "OK", "2026-05-01T00:00:00Z"),
        )
        .unwrap();
        let lib_id = conn.last_insert_rowid();
        for (qn, name, kind) in [
            ("helper", "helper", "Function"),
            ("caller", "caller", "Function"),
            ("get_users", "get_users", "Function"),
            ("app", "app", "Function"),
            ("User", "User", "Struct"),
        ] {
            conn.execute(
                "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, is_public, confidence, last_indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                (lib_id, qn, name, kind, 1, 1.0, "2026-05-01T00:00:00Z"),
            )
            .unwrap();
        }

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (
                "tests/helper_test.rs",
                "Rust",
                "h2",
                80,
                "OK",
                "2026-05-01T00:00:00Z",
            ),
        )
        .unwrap();
        let test_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, is_public, confidence, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (
                test_id,
                "test_helper",
                "test_helper",
                "Function",
                1,
                1.0,
                "2026-05-01T00:00:00Z",
            ),
        )
        .unwrap();
    }

    /// In-file test Function on a product path so SAME_FILE can fire (hit only).
    fn seed_same_file_test_symbol(storage: &StorageManager) {
        let conn = storage.get_connection();
        let lib_id: i64 = conn
            .query_row(
                "SELECT id FROM project_files WHERE file_path = 'src/lib.rs'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, is_public, confidence, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (
                lib_id,
                "test_inline",
                "test_inline",
                "Function",
                1,
                1.0,
                "2026-05-01T00:00:00Z",
            ),
        )
        .unwrap();
    }

    /// DoD-1: extractors with a filled cache still extract after the source
    /// files are deleted; a miss (no cache entry, file gone) skips as today.
    #[test]
    fn content_cache_hit_skips_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src_dir = dir.path().join("src");
        let tests_dir = dir.path().join("tests");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&tests_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), LIB_RS).unwrap();
        fs::write(tests_dir.join("helper_test.rs"), TEST_RS).unwrap();

        let mut cache: HashMap<String, Arc<str>> = HashMap::new();
        cache.insert("src/lib.rs".to_string(), Arc::from(LIB_RS));
        cache.insert("tests/helper_test.rs".to_string(), Arc::from(TEST_RS));

        fs::remove_file(src_dir.join("lib.rs")).unwrap();
        fs::remove_file(tests_dir.join("helper_test.rs")).unwrap();

        let repo = dir.path().to_path_buf();
        let hit = in_memory_storage();
        seed_files_and_symbols(&hit);
        seed_same_file_test_symbol(&hit);
        assert_extractors_succeed_from_cache(&hit, repo.as_path(), &cache);

        let miss = in_memory_storage();
        seed_files_and_symbols(&miss);
        assert_extractors_miss_without_cache(&miss, repo.as_path());
    }

    fn assert_extractors_succeed_from_cache(
        storage: &StorageManager,
        repo: &Path,
        cache: &HashMap<String, Arc<str>>,
    ) {
        let cg = CallGraphBuilder::new(storage, repo.to_path_buf())
            .with_content_cache(cache)
            .build()
            .expect("call graph from cache");
        assert!(
            cg.total_edges > 0,
            "cache hit must still extract call-graph edges, got {}",
            cg.total_edges
        );

        let routes = RouteExtractor::new(storage, repo.to_path_buf())
            .with_content_cache(cache)
            .extract()
            .expect("routes from cache");
        assert!(
            routes.total_routes >= 1,
            "cache hit must still extract routes, got {}",
            routes.total_routes
        );
        assert_eq!(routes.files_skipped, 0);

        let models = DataModelExtractor::new(storage, repo.to_path_buf())
            .with_content_cache(cache)
            .extract()
            .expect("models from cache");
        assert!(
            models.total_models >= 1,
            "cache hit must still extract data models, got {}",
            models.total_models
        );
        assert_eq!(models.files_skipped, 0);

        let obs = ObservabilityExtractor::new(storage, repo.to_path_buf())
            .with_content_cache(cache)
            .extract()
            .expect("observability from cache");
        assert!(
            obs.files_processed >= 1,
            "cache hit must still walk observability files, got {}",
            obs.files_processed
        );

        let tm = TestMapper::new(storage, repo.to_path_buf())
            .with_content_cache(cache)
            .extract()
            .expect("test mapping from cache");
        assert!(
            tm.import_mappings >= 1 || tm.naming_convention_mappings >= 1,
            "cache hit must still extract import/naming mappings, stats={tm:?}"
        );
        assert!(
            tm.same_file_mappings >= 1,
            "cache hit fixture must include a same-file pair, stats={tm:?}"
        );
    }

    fn assert_extractors_miss_without_cache(storage: &StorageManager, repo: &Path) {
        let cg = CallGraphBuilder::new(storage, repo.to_path_buf())
            .build()
            .expect("call graph miss");
        assert!(
            cg.total_edges == 0,
            "miss with deleted files must not invent edges, got {}",
            cg.total_edges
        );
        assert!(
            cg.files_skipped > 0,
            "miss must skip unreadable files, skipped={}",
            cg.files_skipped
        );

        let routes = RouteExtractor::new(storage, repo.to_path_buf())
            .extract()
            .expect("routes miss");
        assert!(
            routes.files_skipped > 0,
            "miss must skip unreadable route files"
        );
        assert!(routes.partial);

        let models = DataModelExtractor::new(storage, repo.to_path_buf())
            .extract()
            .expect("models miss");
        assert!(
            models.files_skipped > 0,
            "miss must skip unreadable model files"
        );
        assert!(models.partial);

        let obs = ObservabilityExtractor::new(storage, repo.to_path_buf())
            .extract()
            .expect("observability miss");
        assert_eq!(
            obs.files_processed, 0,
            "miss with deleted files must not process observability"
        );

        let tm = TestMapper::new(storage, repo.to_path_buf())
            .extract()
            .expect("test mapping miss");
        assert_eq!(
            tm.import_mappings, 0,
            "miss with deleted files must not invent import mappings"
        );
        assert_eq!(
            tm.same_file_mappings, 0,
            "miss with deleted files must not invent same-file mappings"
        );
    }
}
