use crate::index::symbols::SymbolKind;
use crate::state::layout::Layout;
use crate::state::migrations::get_migrations;
use camino::{Utf8Path, Utf8PathBuf};
use miette::{IntoDiagnostic, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use tracing::debug;

pub struct StoredSymbol {
    pub file_path: String,
    pub name: String,
    pub kind: SymbolKind,
    pub is_public: bool,
}

pub struct StorageManager {
    pub(crate) conn: Connection,
    pub cozo: Option<crate::state::storage_cozo::CozoStorage>,
    pub(crate) is_read_only: bool,
    root_path: Utf8PathBuf,
}

impl StorageManager {
    pub fn root_path(&self) -> &Utf8Path {
        &self.root_path
    }

    /// Open write-mode storage for a path-shaped `ledger.db`.
    ///
    /// `root_path` is derived as parent of `.ledgerful` from the DB path
    /// (`…/root/.ledgerful/state/ledger.db` → `…/root`). Prefer
    /// [`Self::init_with_layout`] in production so linked worktrees keep the
    /// analysis work root while sharing main's state directory.
    pub fn init(db_path: &Path) -> Result<Self> {
        let root_path = derive_root_from_db_path(db_path)?;
        Self::init_at(db_path, root_path)
    }

    /// Open write-mode storage using a resolved [`Layout`].
    ///
    /// Opens `layout.state_subdir()/ledger.db` with the same migrations/WAL/Cozo
    /// setup as [`Self::init`], but sets `root_path` to `layout.root` (the
    /// current worktree / analysis root). Required for linked worktrees where
    /// state lives under the main checkout while analysis targets the linked tree.
    pub fn init_with_layout(layout: &Layout) -> Result<Self> {
        let db_path = layout.state_subdir().join("ledger.db");
        debug!(
            "StorageManager::init_with_layout db={:?} root={:?}",
            db_path, layout.root
        );
        Self::init_at(db_path.as_std_path(), layout.root.clone())
    }

    fn init_at(db_path: &Path, root_path: Utf8PathBuf) -> Result<Self> {
        debug!("StorageManager::init_at called with {:?}", db_path);
        // Captured BEFORE `Connection::open`, which itself creates the file:
        // this is the only reliable way to tell "brand-new project" (no
        // prior ledger.db) apart from "existing project, stale schema"
        // (Track TA31 R3 footgun #1 — a fresh DB starting at version 0 is
        // normal first-time init, not a migration the user needs telling
        // about).
        let db_existed_before_open = db_path.exists();
        let mut conn = Connection::open(db_path).into_diagnostic()?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000; PRAGMA foreign_keys = ON;",
        )
        .into_diagnostic()?;

        crate::state::migration_prompt::check_and_prompt_migration(&conn, db_existed_before_open)?;

        let migrations = get_migrations();
        migrations.to_latest(&mut conn).into_diagnostic()?;

        let cozo_path = db_path
            .parent()
            .map(|p| p.join("ledger.cozo"))
            .unwrap_or_default();
        let cozo = if !cozo_path.as_os_str().is_empty() {
            Some(crate::state::storage_cozo::CozoStorage::new(&cozo_path)?)
        } else {
            None
        };

        debug!("Initialized storage at {:?} root={:?}", db_path, root_path);
        Ok(Self {
            conn,
            cozo,
            is_read_only: false,
            root_path,
        })
    }

    pub fn root(&self) -> &Utf8Path {
        &self.root_path
    }

    pub fn get_connection(&self) -> &Connection {
        &self.conn
    }

    pub fn get_connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Explicitly shutdown the storage manager, releasing all file locks.
    pub fn shutdown(mut self) -> Result<()> {
        debug!("Shutting down StorageManager");
        if let Some(cozo) = self.cozo.take() {
            cozo.shutdown();
        }

        let conn = std::mem::replace(
            &mut self.conn,
            Connection::open_in_memory().into_diagnostic()?,
        );
        if let Err((_conn, e)) = conn.close() {
            return Err(miette::miette!("Failed to close SQLite connection: {}", e));
        }

        Ok(())
    }

    /// Open storage in read-only mode, skipping migration checks.
    /// This is a fast-path for read-only commands that do not write to storage.
    ///
    /// Uses [`Layout::state_subdir`] so linked worktrees open the shared ledger
    /// rather than inventing `{worktree}/.ledgerful`.
    ///
    /// Returns `Err` if the SQLite database file does not exist.
    pub fn open_read_only(layout: &Layout) -> Result<Self> {
        Self::open_read_only_with_options(layout, true)
    }

    /// Open storage in read-only mode, skipping migration checks and NOT opening CozoDB.
    /// This is the fastest path for commands that only need metadata or transaction status.
    pub fn open_read_only_sqlite_only(layout: &Layout) -> Result<Self> {
        Self::open_read_only_with_options(layout, false)
    }

    fn open_read_only_with_options(layout: &Layout, include_cozo: bool) -> Result<Self> {
        let db_path = layout.state_subdir().join("ledger.db");

        if !db_path.exists() {
            return Err(miette::miette!(
                "Storage not initialized at {}. Run a write command first (e.g. `ledgerful scan`).",
                db_path
            ));
        }

        tracing::debug!(
            "Opening read-only storage at {:?} (include_cozo: {})",
            db_path,
            include_cozo
        );
        // True OS-level RO flags (same pattern as open_read_only_from_path).
        // Plain Connection::open + PRAGMA journal_mode=WAL fails pure RO mounts.
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(db_path.as_std_path(), flags).into_diagnostic()?;

        // No journal_mode=WAL on RO path (would require write access).
        conn.execute_batch("PRAGMA busy_timeout = 5000; PRAGMA foreign_keys = ON;")
            .into_diagnostic()?;

        #[cfg(not(test))]
        {
            crate::state::storage::migrations::verify_schema_is_current(&conn)?;
        }

        let cozo = if include_cozo {
            let cozo_path = db_path
                .parent()
                .map(|p| p.join("ledger.cozo"))
                .unwrap_or_default();
            if cozo_path.exists() {
                Some(crate::state::storage_cozo::CozoStorage::new_read_only(
                    cozo_path.as_std_path(),
                )?)
            } else {
                None
            }
        } else {
            None
        };

        tracing::debug!("Opened read-only storage at {:?}", db_path);
        Ok(Self {
            conn,
            cozo,
            is_read_only: true,
            root_path: layout.root.clone(),
        })
    }

    pub fn init_from_conn(conn: Connection) -> Self {
        Self {
            conn,
            cozo: None,
            is_read_only: false,
            root_path: Utf8PathBuf::from("."),
        }
    }

    /// Open an arbitrary `ledger.db` path read-only, without running migrations
    /// or verifying that the schema is current. Used by the cross-repo rollup
    /// to read foreign repo DBs: a schema mismatch there is a warn-and-skip
    /// rather than a hard failure.
    ///
    /// The repo root is inferred from `db_path` as
    /// `db_path.parent().parent().parent()` (state/ → .ledgerful/ → root).
    /// If the path shape is unexpected, the root falls back to `.`.
    pub fn open_read_only_from_path(db_path: &Path) -> Result<Self> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(db_path, flags).into_diagnostic()?;

        conn.execute_batch("PRAGMA busy_timeout = 2000;")
            .into_diagnostic()?;

        let root_path = db_path
            .parent() // state/
            .and_then(|p| p.parent()) // .ledgerful/
            .and_then(|p| p.parent()) // root/
            .unwrap_or(Path::new("."));
        let root_path = Utf8PathBuf::from_path_buf(root_path.to_path_buf())
            .map_err(|_| miette::miette!("Invalid UTF-8 in root path"))?;

        Ok(Self {
            conn,
            cozo: None,
            is_read_only: true,
            root_path,
        })
    }
}

/// Infer repo root from a conventional `…/root/.ledgerful/state/ledger.db` path.
fn derive_root_from_db_path(db_path: &Path) -> Result<Utf8PathBuf> {
    let root_path = db_path
        .parent() // state/
        .and_then(|p| p.parent()) // .ledgerful/
        .and_then(|p| p.parent()) // root/
        .unwrap_or(Path::new("."));
    Utf8PathBuf::from_path_buf(root_path.to_path_buf())
        .map_err(|_| miette::miette!("Invalid UTF-8 in root path"))
}

#[cfg(test)]
pub(crate) fn in_memory_storage() -> StorageManager {
    let conn = Connection::open_in_memory().unwrap();
    let mut conn = conn;
    get_migrations().to_latest(&mut conn).unwrap();
    StorageManager {
        conn,
        cozo: None,
        is_read_only: false,
        root_path: Utf8PathBuf::from("."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::{ChangedFile, FileAnalysisStatus, ImpactPacket};
    use std::path::PathBuf;

    #[test]
    fn test_save_changed_files() {
        let storage = in_memory_storage();
        let packet = ImpactPacket {
            head_hash: Some("abc".to_string()),
            changes: vec![ChangedFile {
                path: PathBuf::from("src/main.rs"),
                status: "Added".to_string(),
                old_path: None,
                is_staged: true,

                symbols: None,
                imports: None,
                runtime_usage: None,
                analysis_status: FileAnalysisStatus::default(),
                analysis_warnings: Vec::new(),
                api_routes: Vec::new(),
                data_models: Vec::new(),
                ci_gates: Vec::new(),
            }],
            ..Default::default()
        };
        storage.save_packet(&packet).unwrap();

        let snapshot_id = storage.conn.last_insert_rowid();
        storage
            .save_changed_files(snapshot_id, &packet.changes)
            .unwrap();
    }

    #[test]
    fn read_only_skips_migrations() {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();

        // Create an empty SQLite file (no migrations have run)
        let db_path = layout.state_subdir().join("ledger.db");
        let conn = Connection::open(db_path.as_std_path()).unwrap();
        let initial_version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(initial_version, 0, "fresh db should have user_version=0");
        drop(conn);

        // Call open_read_only — in RED phase this delegates to init which
        // runs migrations, so the test will fail. In GREEN phase it skips
        // migrations and the test passes.
        let storage = StorageManager::open_read_only(&layout).unwrap();

        // Verify no migrations ran — user_version should still be 0
        let version: i64 = storage
            .get_connection()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 0, "open_read_only should not run migrations");
    }

    #[test]
    fn read_only_fails_on_missing_db() {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        // Do NOT create an SQLite file

        // In RED phase open_read_only delegates to init which creates the
        // file via Connection::open, so the test fails. In GREEN phase
        // open_read_only checks path existence first and returns Err.
        let result = StorageManager::open_read_only(&layout);
        assert!(
            result.is_err(),
            "open_read_only should fail without a db file"
        );
    }

    #[test]
    fn open_read_only_uses_sqlite_open_read_only_flags() {
        // True SQLITE_OPEN_READ_ONLY path: create via write init, reopen RO,
        // and still query. Optional: mark file RO on Windows and reopen.
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();

        let write = StorageManager::init_with_layout(&layout).unwrap();
        write
            .get_connection()
            .execute(
                "CREATE TABLE IF NOT EXISTS _ro_flag_probe (k TEXT PRIMARY KEY)",
                [],
            )
            .unwrap();
        write
            .get_connection()
            .execute("INSERT INTO _ro_flag_probe (k) VALUES ('ok')", [])
            .unwrap();
        let _ = write.shutdown();

        let db_path = layout.state_subdir().join("ledger.db");
        assert!(db_path.exists());

        let read = StorageManager::open_read_only(&layout).unwrap();
        assert!(read.is_read_only);
        let got: String = read
            .get_connection()
            .query_row("SELECT k FROM _ro_flag_probe WHERE k = 'ok'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(got, "ok");

        // SQLITE_OPEN_READ_ONLY must reject mutations (proves true RO flags).
        let insert_err = read
            .get_connection()
            .execute("INSERT INTO _ro_flag_probe (k) VALUES ('should_fail')", []);
        assert!(
            insert_err.is_err(),
            "INSERT under open_read_only must fail (SQLITE_OPEN_READ_ONLY)"
        );
        let create_err = read.get_connection().execute(
            "CREATE TABLE IF NOT EXISTS _ro_write_probe (k TEXT PRIMARY KEY)",
            [],
        );
        assert!(
            create_err.is_err(),
            "CREATE TABLE under open_read_only must fail (SQLITE_OPEN_READ_ONLY)"
        );
        let _ = read.shutdown();

        // Windows: mark the DB file read-only at OS level and ensure RO open still works.
        #[cfg(windows)]
        {
            let meta = std::fs::metadata(db_path.as_std_path()).unwrap();
            let mut perms = meta.permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(db_path.as_std_path(), perms).unwrap();

            let read2 = StorageManager::open_read_only(&layout).unwrap();
            assert!(read2.is_read_only);
            let got2: String = read2
                .get_connection()
                .query_row("SELECT k FROM _ro_flag_probe WHERE k = 'ok'", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(got2, "ok");
            let _ = read2.shutdown();

            // Clear readonly so tempdir cleanup succeeds (Windows-only test path).
            let meta = std::fs::metadata(db_path.as_std_path()).unwrap();
            let mut perms = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            {
                perms.set_readonly(false);
            }
            std::fs::set_permissions(db_path.as_std_path(), perms).unwrap();
        }
    }

    #[test]
    fn init_with_layout_keeps_work_root_when_state_is_shared() {
        // Linked-worktree shape: work_root ≠ parent-of-state, but both open
        // the same absolute ledger.db and write-mode root_path stays work_root.
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        let linked = tmp.path().join("linked");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::create_dir_all(&linked).unwrap();

        let work = Utf8PathBuf::from_path_buf(linked.clone()).unwrap();
        let state = Utf8PathBuf::from_path_buf(main.join(".ledgerful")).unwrap();
        let layout = Layout::from_roots(&work, &state);
        layout.ensure_state_dir().unwrap();

        let db_expected = layout.state_subdir().join("ledger.db");
        let write = StorageManager::init_with_layout(&layout).unwrap();
        assert_eq!(
            write.root_path(),
            layout.root.as_path(),
            "write-mode root_path must be the analysis work root, not state parent"
        );
        assert_ne!(
            write.root_path().as_str().replace('\\', "/"),
            state.parent().unwrap().as_str().replace('\\', "/"),
            "root_path must not collapse to main when state is under main"
        );

        // Trivial row so both layouts share the same physical DB.
        write
            .get_connection()
            .execute(
                "CREATE TABLE IF NOT EXISTS _wt_probe (k TEXT PRIMARY KEY, v TEXT)",
                [],
            )
            .unwrap();
        write
            .get_connection()
            .execute(
                "INSERT INTO _wt_probe (k, v) VALUES ('shared', 'from-linked')",
                [],
            )
            .unwrap();
        let _ = write.shutdown();

        let main_layout =
            Layout::from_roots(Utf8PathBuf::from_path_buf(main.clone()).unwrap(), &state);
        assert_eq!(
            main_layout.state_subdir().join("ledger.db"),
            db_expected,
            "main and linked layouts must resolve the same ledger.db path"
        );
        let read = StorageManager::open_read_only(&main_layout).unwrap();
        assert_eq!(
            read.root_path(),
            main_layout.root.as_path(),
            "read-only root_path follows the layout that opened it"
        );
        let v: String = read
            .get_connection()
            .query_row("SELECT v FROM _wt_probe WHERE k = 'shared'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(v, "from-linked");
    }

    #[test]
    fn init_path_shaped_still_derives_root_from_db() {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let db_path = layout.state_subdir().join("ledger.db");
        let storage = StorageManager::init(db_path.as_std_path()).unwrap();
        assert_eq!(storage.root_path(), layout.root.as_path());
    }
}
