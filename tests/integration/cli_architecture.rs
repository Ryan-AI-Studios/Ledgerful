//! Real-binary tests for the local Architecture check. Graph fixtures use the native index.

use crate::common::{git_cmd, isolated_command, nested_repo_root, setup_git_repo};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_ledgerful");
const UI: &str = "src/ui/a.rs";
const CORE: &str = "src/core/b.rs";
const EXTRA: &str = "src/core/d.rs";
const CALLER: &str = "pub fn caller() { helper(); }\n";
const HELPER: &str = "pub fn helper() {}\n";
const REVERSE: &str = "pub fn helper() { caller(); }\n";
const CONFIG: &str = r#"schema_version = 1

[[components]]
name = "ui"
paths = ["src/ui/**"]

[[components]]
name = "core"
paths = ["src/core/**"]

[[rules]]
id = "core-to-ui"
effect = "forbid"
from = "core"
to = "ui"
"#;
const UNAVAILABLE_FIELDS: &[&str] = &[
    "mapping",
    "unconstrainedCrossEdges",
    "violations",
    "cycleGroups",
    "removedViolations",
    "removedCycleGroups",
    "counts",
];

struct Fixture {
    _temp: TempDir,
    base: PathBuf,
    root: PathBuf,
    profile: PathBuf,
}

impl Fixture {
    fn empty() -> Self {
        let temp = tempfile::tempdir().unwrap();
        // The native reader verifies each component's exact spelling, so use the
        // canonical fixture path, not an 8.3 temp alias such as `RUNNER~1`.
        let base = dunce::canonicalize(temp.path()).unwrap();
        let root = nested_repo_root(&base);
        let profile = base.join("profile");
        fs::create_dir(&profile).unwrap();
        setup_git_repo(&root);
        git_cmd(&root, &["config", "core.autocrlf", "false"]);
        git_cmd(&root, &["config", "commit.gpgsign", "false"]);
        Self {
            _temp: temp,
            base,
            root,
            profile,
        }
    }

    fn indexed() -> Self {
        let fixture = Self::empty();
        fixture.write(UI, CALLER);
        fixture.write(CORE, HELPER);
        fixture.commit("forward");
        fixture.cli_ok(&["init"]);
        fixture.full_index();
        fixture.write(".ledgerful/architecture.toml", CONFIG);
        fixture.assert_edges(&[(UI, CORE, "RESOLVED", 1)]);
        fixture
    }

    fn cyclic() -> Self {
        let fixture = Self::indexed();
        fixture.make_cycle();
        fixture
    }

    fn make_cycle(&self) {
        self.write(CORE, REVERSE);
        self.commit("reverse");
        self.full_index();
        self.assert_edges(&[(CORE, UI, "RESOLVED", 1), (UI, CORE, "RESOLVED", 1)]);
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn git(&self, args: &[&str]) -> String {
        let mut command = isolated_command("git", &self.profile);
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let binary_dir = Path::new(BIN).parent().unwrap();
        let exe_path = std::env::join_paths(
            std::iter::once(binary_dir.to_path_buf()).chain(std::env::split_paths(&inherited)),
        )
        .unwrap();
        let result = command
            .current_dir(&self.root)
            .env("PATH", exe_path)
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap()
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-m", message]);
    }

    fn full_index(&self) {
        self.cli_ok(&["index", "--full"]);
    }

    fn cli_ok(&self, args: &[&str]) -> String {
        let (out, err, code) = self.cli(BIN, args);
        assert_eq!(code, 0, "{args:?}\n{out}\n{err}");
        out
    }

    fn cli(&self, binary: &str, args: &[&str]) -> (String, String, i32) {
        self.cli_with_state(binary, args, None)
    }

    fn cli_with_state(
        &self,
        binary: &str,
        args: &[&str],
        state: Option<&Path>,
    ) -> (String, String, i32) {
        let mut command = isolated_command(binary, &self.profile);
        if let Some(state) = state {
            command.env("LEDGERFUL_STATE_DIR", state);
        }
        let result = command.current_dir(&self.root).args(args).output().unwrap();
        (
            String::from_utf8(result.stdout).unwrap(),
            String::from_utf8(result.stderr).unwrap(),
            result.status.code().unwrap(),
        )
    }

    fn baseline(&self) {
        let out = self.cli_ok(&["baseline", "create", "arch0", "--json"]);
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["identity"]["inputCompleteness"]["state"], "COMPLETE");
    }

    fn check(&self, extra: &[&str], exit: i32) -> Value {
        self.check_binary(BIN, extra, exit)
    }

    fn check_binary(&self, binary: &str, extra: &[&str], exit: i32) -> Value {
        let mut args = vec!["architecture", "check", "--json"];
        args.extend_from_slice(extra);
        let (out, err, code) = self.cli(binary, &args);
        assert_eq!(code, exit, "{args:?}\n{out}\n{err}");
        if exit == 0 {
            assert!(err.is_empty(), "{err}");
        } else {
            let lines: Vec<_> = err.lines().collect();
            assert_eq!(lines.len(), 1, "{err}");
            assert!(lines[0].starts_with("architecture check failed: "), "{err}");
        }
        serde_json::from_str(&out).unwrap()
    }

    fn reject(&self, extra: &[&str], reason: &str) {
        let (out, err, result) = self.refusal(BIN, extra);
        assert_refusal(out, err, result, reason);
    }

    fn refusal(&self, binary: &str, extra: &[&str]) -> (String, String, i32) {
        let mut args = vec!["architecture", "check", "--json"];
        args.extend_from_slice(extra);
        self.cli(binary, &args)
    }

    fn db(&self, flags: OpenFlags) -> Connection {
        Connection::open_with_flags(self.db_path(), flags).unwrap()
    }

    fn db_path(&self) -> PathBuf {
        self.root.join(".ledgerful/state/ledger.db")
    }

    fn files(&self) -> BTreeMap<PathBuf, Option<blake3::Hash>> {
        let mut files = BTreeMap::new();
        fingerprint_files(&self.root, &mut files);
        files
    }

    fn assert_edges(&self, expected: &[(&str, &str, &str, u32)]) {
        // Finish with the normal writer close so this oracle does not leave
        // read-only SQLite's warm sidecars behind for the CLI under test.
        let db = self.db(OpenFlags::SQLITE_OPEN_READ_WRITE);
        let actual = grouped_edges(&db);
        assert_eq!(
            actual
                .iter()
                .map(|(a, b, state, weight)| (a.as_str(), b.as_str(), state.as_str(), *weight))
                .collect::<Vec<_>>(),
            expected,
        );
    }

    fn snapshot(&self) -> StateSnapshot {
        let (version, edges, baselines) = {
            let db = self.db(OpenFlags::SQLITE_OPEN_READ_WRITE);
            let version: u32 = db
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            let rows = db
                .prepare("SELECT name, payload_digest FROM analysis_baselines ORDER BY name")
                .unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            (version, grouped_edges(&db), rows)
        };
        StateSnapshot {
            head: self.git(&["rev-parse", "HEAD"]),
            status: self.git(&["--no-optional-locks", "status", "--porcelain=v1"]),
            files: self.files(),
            version,
            edges,
            baselines,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct StateSnapshot {
    head: String,
    status: String,
    files: BTreeMap<PathBuf, Option<blake3::Hash>>,
    version: u32,
    edges: Vec<(String, String, String, u32)>,
    baselines: Vec<(String, String)>,
}

const ARCH_STORAGE_UNAVAILABLE: &str = "ARCHITECTURE_STORAGE_UNAVAILABLE";
const WAL_SUFFIX: &str = "ledger.db-wal";
const SHM_SUFFIX: &str = "ledger.db-shm";
const JOURNAL_SUFFIX: &str = "ledger.db-journal";

fn native_storage_platform() -> bool {
    #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
    {
        // SAFETY: geteuid takes no pointers and returns this process identity.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        (unsafe { nix::libc::geteuid() }) != 0
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")))]
    {
        cfg!(windows)
    }
}

#[cfg(not(windows))]
#[test]
#[allow(non_snake_case)]
fn architecture_check_refuses_storage_on_an_unsupported_runtime__slow() {
    if native_storage_platform() {
        return;
    }
    let fixture = Fixture::indexed();
    checkpoint_fixture(&fixture);
    let before = fixture.files();
    fixture.reject(&["--fail"], ARCH_STORAGE_UNAVAILABLE);
    assert_eq!(fixture.files(), before);
}

fn checkpoint_fixture(fixture: &Fixture) {
    let db = fixture.db(OpenFlags::SQLITE_OPEN_READ_WRITE);
    let journal_mode: String = db
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "wal");
    let checkpoint: (i64, i64, i64) = db
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    assert_eq!(checkpoint, (0, 0, 0));
    db.close().unwrap();
    for name in [WAL_SUFFIX, SHM_SUFFIX, JOURNAL_SUFFIX] {
        assert!(!fixture.db_path().with_file_name(name).exists(), "{name}");
    }
}

fn create_wal_only_baseline(fixture: &Fixture) -> Connection {
    let keeper = fixture.db(OpenFlags::SQLITE_OPEN_READ_WRITE);
    keeper.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    keeper
        .set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();
    keeper
        .execute(
            "INSERT INTO analysis_baselines (
                name, baseline_schema, payload_kind, identity_json, payload_json,
                payload_digest, created_at
             )
             SELECT 'walonly', baseline_schema, payload_kind, identity_json, payload_json,
                    payload_digest, created_at
             FROM analysis_baselines WHERE name = 'arch0'",
            [],
        )
        .unwrap();
    keeper
}

#[test]
#[allow(non_snake_case)]
fn architecture_check_cold_wal_keeps_the_complete_inventory__slow() {
    if !native_storage_platform() {
        return;
    }
    let fixture = Fixture::cyclic();
    fixture.baseline();
    checkpoint_fixture(&fixture);
    let before = fixture.files();
    let verdict = fixture.check(&["--baseline", "arch0", "--fail"], 0);
    assert_eq!(fixture.files(), before);
    assert_eq!(verdict["status"], "evaluated");
    assert_eq!(verdict["observation"], "findings");
    assert_eq!(verdict["regression"], "none");
    assert_eq!(verdict["evidence"]["coverage"]["edgesResolvedCrossFile"], 2);
}

#[test]
#[allow(non_snake_case)]
fn architecture_check_reads_a_wal_only_baseline_without_file_changes__slow() {
    if !native_storage_platform() {
        return;
    }
    let fixture = Fixture::cyclic();
    fixture.baseline();
    checkpoint_fixture(&fixture);
    let main = fs::read(fixture.db_path()).unwrap();
    let keeper = create_wal_only_baseline(&fixture);
    #[cfg(not(windows))]
    keeper.close().unwrap();
    assert!(
        fs::metadata(fixture.db_path().with_file_name(WAL_SUFFIX))
            .unwrap()
            .len()
            > 32
    );
    assert_eq!(fs::read(fixture.db_path()).unwrap(), main);
    let before = fixture.files();
    let verdict = fixture.check(&["--baseline", "walonly", "--fail"], 0);
    assert_eq!(fixture.files(), before);
    assert_eq!(verdict["baseline"]["name"], "walonly");
    assert_eq!(verdict["baseline"]["status"], "comparable");
    assert_eq!(verdict["regression"], "none");
    #[cfg(not(windows))]
    let keeper = fixture.db(OpenFlags::SQLITE_OPEN_READ_WRITE);
    keeper.execute_batch("BEGIN IMMEDIATE; ROLLBACK").unwrap();
    keeper.close().unwrap();
}

#[test]
#[allow(non_snake_case)]
fn architecture_check_refuses_unqualified_sqlite_layouts_without_changes__slow() {
    if !native_storage_platform() {
        return;
    }
    let fixture = Fixture::indexed();
    checkpoint_fixture(&fixture);
    for name in [WAL_SUFFIX, SHM_SUFFIX, JOURNAL_SUFFIX] {
        let sentinel = fixture.db_path().with_file_name(name);
        fs::write(&sentinel, b"unqualified companion").unwrap();
        let before = fixture.files();
        fixture.reject(&["--fail"], ARCH_STORAGE_UNAVAILABLE);
        assert_eq!(fixture.files(), before);
        fs::remove_file(sentinel).unwrap();
    }
    let unrelated = fixture.db_path().with_file_name("unrelated.db");
    fs::hard_link(fixture.db_path(), unrelated).unwrap();
    let before = fixture.files();
    fixture.reject(&["--fail"], ARCH_STORAGE_UNAVAILABLE);
    assert_eq!(fixture.files(), before);
}

#[cfg(windows)]
#[test]
#[allow(non_snake_case)]
fn architecture_check_refuses_a_cold_database_with_a_writer_handle__slow() {
    let fixture = Fixture::indexed();
    checkpoint_fixture(&fixture);
    let writer = fs::OpenOptions::new()
        .write(true)
        .open(fixture.db_path())
        .unwrap();
    let before = fixture.files();
    fixture.reject(&["--fail"], ARCH_STORAGE_UNAVAILABLE);
    assert_eq!(fixture.files(), before);
    drop(writer);
}

#[cfg(any(
    windows,
    all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")
))]
#[test]
#[allow(non_snake_case)]
fn architecture_check_refuses_a_wal_writer_holding_the_native_lock__slow() {
    if !native_storage_platform() {
        return;
    }
    let fixture = Fixture::cyclic();
    fixture.baseline();
    checkpoint_fixture(&fixture);
    let writer = create_wal_only_baseline(&fixture);
    #[cfg(target_os = "linux")]
    let pinned = [
        fixture.db_path(),
        fixture.db_path().with_file_name(WAL_SUFFIX),
        fixture.db_path().with_file_name(SHM_SUFFIX),
    ]
    .into_iter()
    .map(|path| {
        let file = fs::File::open(&path).unwrap();
        (path, file)
    })
    .collect::<BTreeMap<_, _>>();
    #[cfg(target_os = "linux")]
    let files = || fingerprint_linux_lock_fixture(&fixture, &pinned);
    #[cfg(windows)]
    let files = || fixture.files();

    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let before = files();
    fixture.reject(
        &["--baseline", "walonly", "--fail"],
        ARCH_STORAGE_UNAVAILABLE,
    );
    assert_eq!(files(), before);
    writer.execute_batch("ROLLBACK").unwrap();
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
fn fingerprint_linux_lock_fixture(
    fixture: &Fixture,
    pinned: &BTreeMap<PathBuf, fs::File>,
) -> BTreeMap<PathBuf, Option<blake3::Hash>> {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::fs::MetadataExt;

    // Closing another descriptor for the same inode releases this process's
    // POSIX SQLite locks. Re-read through the same retained handles.
    for (path, file) in pinned {
        let named = fs::symlink_metadata(path).unwrap();
        let retained = file.metadata().unwrap();
        assert!(named.is_file());
        assert_eq!((named.dev(), named.ino()), (retained.dev(), retained.ino()));
    }
    let mut read = |path: &Path| {
        if let Some(mut file) = pinned.get(path) {
            let mut bytes = Vec::new();
            file.seek(SeekFrom::Start(0)).unwrap();
            file.read_to_end(&mut bytes).unwrap();
            blake3::hash(&bytes)
        } else {
            fingerprint_file(path)
        }
    };
    let mut files = BTreeMap::new();
    fingerprint_files_with(&fixture.root, &mut files, &mut read);
    files
}

#[cfg(windows)]
#[test]
#[allow(non_snake_case)]
fn architecture_check_refuses_non_wal_decoys_and_invalid_live_pairs__slow() {
    use std::os::windows::fs::FileExt;

    let rollback = Fixture::indexed();
    checkpoint_fixture(&rollback);
    let connection = rollback.db(OpenFlags::SQLITE_OPEN_READ_WRITE);
    let mode: String = connection
        .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "delete");
    connection.close().unwrap();
    for name in [WAL_SUFFIX, SHM_SUFFIX] {
        fs::write(rollback.db_path().with_file_name(name), b"").unwrap();
    }
    let before = rollback.files();
    rollback.reject(&["--fail"], ARCH_STORAGE_UNAVAILABLE);
    assert_eq!(rollback.files(), before);

    let live = Fixture::cyclic();
    live.baseline();
    checkpoint_fixture(&live);
    let keeper = create_wal_only_baseline(&live);
    let wal = live.db_path().with_file_name(WAL_SUFFIX);
    let shm = live.db_path().with_file_name(SHM_SUFFIX);
    let mut byte = [0];
    assert_eq!(
        fs::File::open(&wal)
            .unwrap()
            .seek_read(&mut byte, 0)
            .unwrap(),
        1
    );
    assert_live_mutation_refused(&live, &wal, 0, &[byte[0] ^ 1]);

    let shared = fs::read(&shm).unwrap();
    let published_frame = &shared[16..20];
    assert_ne!(published_frame, [0; 4]);
    // False backfill would let SQLite ignore a valid, nonempty WAL.
    assert_live_mutation_refused(&live, &shm, 96, published_frame);
    assert_ne!(&shared[136..140], [0; 4]);
    assert_live_mutation_refused(&live, &shm, 136, &[0; 4]);

    let occupied_slot = shared[16_384..32_768]
        .as_chunks::<2>()
        .0
        .iter()
        .position(|slot| *slot != [0, 0])
        .unwrap();
    assert_live_mutation_refused(&live, &shm, (16_384 + 2 * occupied_slot) as u64, &[0; 2]);
    keeper.close().unwrap();
}

#[cfg(windows)]
fn assert_live_mutation_refused(fixture: &Fixture, path: &Path, offset: u64, replacement: &[u8]) {
    use std::os::windows::fs::FileExt;

    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let initial = file.metadata().unwrap();
    let mut saved = vec![0; replacement.len()];
    assert_eq!(file.seek_read(&mut saved, offset).unwrap(), saved.len());
    assert_ne!(saved, replacement);
    assert_eq!(
        file.seek_write(replacement, offset).unwrap(),
        replacement.len()
    );
    file.set_times(fs::FileTimes::new().set_modified(initial.modified().unwrap()))
        .unwrap();
    assert_eq!(file.metadata().unwrap().len(), initial.len());
    let before = fixture.files();
    fixture.reject(
        &["--baseline", "walonly", "--fail"],
        ARCH_STORAGE_UNAVAILABLE,
    );
    assert_eq!(fixture.files(), before);
    assert_eq!(file.seek_write(&saved, offset).unwrap(), saved.len());
}

#[test]
#[allow(non_snake_case)]
fn architecture_check_reads_only_the_selected_alternate_state__slow() {
    if !native_storage_platform() {
        return;
    }
    let fixture = Fixture::cyclic();
    fixture.baseline();
    checkpoint_fixture(&fixture);
    let alternate_root = fixture.base.join("alternate");
    let alternate = alternate_root.join(".ledgerful");
    fs::create_dir_all(alternate.join("state")).unwrap();
    let alternate_db = alternate.join("state/ledger.db");
    fs::copy(fixture.db_path(), &alternate_db).unwrap();
    fixture
        .db(OpenFlags::SQLITE_OPEN_READ_WRITE)
        .pragma_update(None, "user_version", 0)
        .unwrap();

    let run = |state: &Path| {
        fixture.cli_with_state(
            BIN,
            &[
                "architecture",
                "check",
                "--baseline",
                "arch0",
                "--fail",
                "--json",
            ],
            Some(state),
        )
    };
    let mut before = fixture.files();
    fingerprint_files(&alternate_root, &mut before);
    let (out, err, exit) = run(&alternate);
    let mut after = fixture.files();
    fingerprint_files(&alternate_root, &mut after);
    assert_eq!(after, before);
    assert_eq!(exit, 0, "{out}\n{err}");
    assert!(err.is_empty(), "{err}");
    let verdict: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(verdict["observation"], "findings");
    assert_eq!(verdict["regression"], "none");
    fixture.reject(&[], "Schema mismatch detected.");

    let missing = fixture.base.join("missing").join(".ledgerful");
    let (out, err, exit) = run(&missing);
    assert!(err.contains("missing"), "{err}");
    assert_refusal(out, err, exit, ARCH_STORAGE_UNAVAILABLE);
    assert!(!missing.exists());
}

fn grouped_edges(db: &Connection) -> Vec<(String, String, String, u32)> {
    db.prepare(
        "SELECT cf.file_path, df.file_path, se.resolution_status, COUNT(*)
         FROM structural_edges se JOIN project_files cf ON cf.id = se.caller_file_id
         LEFT JOIN project_files df ON df.id = se.callee_file_id
         GROUP BY 1, 2, 3 ORDER BY 1, 2, 3",
    )
    .unwrap()
    .query_map([], |row| {
        // Use an empty destination for a resolver row without a target.
        Ok((
            row.get(0)?,
            row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            row.get(2)?,
            row.get(3)?,
        ))
    })
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

fn fingerprint_files(path: &Path, result: &mut BTreeMap<PathBuf, Option<blake3::Hash>>) {
    fingerprint_files_with(path, result, &mut fingerprint_file);
}

fn fingerprint_files_with(
    path: &Path,
    result: &mut BTreeMap<PathBuf, Option<blake3::Hash>>,
    read: &mut impl FnMut(&Path) -> blake3::Hash,
) {
    if path.is_dir() {
        result.insert(path.to_path_buf(), None);
        for entry in fs::read_dir(path).unwrap() {
            fingerprint_files_with(&entry.unwrap().path(), result, read);
        }
    } else {
        result.insert(path.to_path_buf(), Some(read(path)));
    }
}

fn fingerprint_file(path: &Path) -> blake3::Hash {
    match fs::read(path) {
        Ok(bytes) => blake3::hash(&bytes),
        #[cfg(windows)]
        Err(error)
            if error.raw_os_error() == Some(33)
                && path.file_name().is_some_and(|name| name == SHM_SUFFIX) =>
        {
            fingerprint_locked_fixture_shm(path)
        }
        Err(error) => panic!("could not fingerprint {}: {error}", path.display()),
    }
}

#[cfg(windows)]
fn fingerprint_locked_fixture_shm(path: &Path) -> blake3::Hash {
    use std::{os::windows::io::AsRawHandle, ptr, slice};
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Memory::{
            CreateFileMappingW, FILE_MAP_READ, MapViewOfFile, PAGE_READONLY, UnmapViewOfFile,
        },
    };

    let file = fs::File::open(path).unwrap();
    let size = usize::try_from(file.metadata().unwrap().len()).unwrap();
    assert!((32 * 1024..=512 * 1024).contains(&size));
    // Windows direct reads fail across a held byte lock. The fixture writer
    // is paused, and the CLI is between observations. A bounded read-only
    // mapping can sample the complete SHM bytes without opening SQLite.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        let mapping = CreateFileMappingW(
            file.as_raw_handle(),
            ptr::null(),
            PAGE_READONLY,
            0,
            0,
            ptr::null(),
        );
        assert!(!mapping.is_null());
        let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, size);
        assert_ne!(CloseHandle(mapping), 0);
        assert!(!view.Value.is_null());
        let hash = blake3::hash(slice::from_raw_parts(view.Value.cast::<u8>(), size));
        assert_ne!(UnmapViewOfFile(view), 0);
        hash
    }
}

fn assert_refusal(out: String, err: String, exit: i32, reason: &str) {
    assert_eq!(exit, 1, "{out}\n{err}");
    assert!(out.is_empty(), "{out}");
    assert!(err.contains(reason), "expected {reason}: {err}");
}

fn assert_unknown_current(value: &Value) {
    assert_eq!(value["status"], "unknown");
    assert_eq!(value["observation"], "unknown");
    for key in UNAVAILABLE_FIELDS {
        assert!(value.get(key).is_none(), "{key} must be absent");
    }
}

#[test]
fn architecture_check_missing_config_exits_nonzero_without_envelope() {
    let fixture = Fixture::empty();
    fixture.reject(&[], "ARCHITECTURE_CONFIG_NOT_FOUND");
    let (out, err, exit) = fixture.refusal(BIN, &["--config", "missing-rules.toml"]);
    assert!(err.contains("missing-rules.toml"), "{err}");
    assert_refusal(out, err, exit, "ARCHITECTURE_CONFIG_NOT_FOUND");
    assert!(!fixture.root.join(".ledgerful").exists());
}

#[test]
fn architecture_check_invalid_config_exits_nonzero_without_envelope() {
    let fixture = Fixture::empty();
    let config_path = ".ledgerful/architecture.toml";
    let sentinel = "synthetic-private-config-value";
    for (contents, reason) in [
        (
            format!("schema_version = 1\nvalue = \"{sentinel}\n"),
            "invalid TOML at line 2, column ",
        ),
        (
            format!("{CONFIG}\n{sentinel} = \"{sentinel}\"\n"),
            "unknown rule key",
        ),
    ] {
        fixture.write(config_path, &contents);
        let (out, err, exit) = fixture.refusal(BIN, &[]);
        assert!(!err.contains(sentinel), "{err}");
        assert!(err.contains(reason), "{err}");
        assert_refusal(out, err, exit, "ARCHITECTURE_CONFIG_INVALID");
    }
    assert!(!fixture.root.join(".ledgerful/state").exists());
}

#[test]
#[allow(non_snake_case)]
fn architecture_check_acyclic_fixture_is_evaluated_with_no_cycle_groups__slow() {
    if !native_storage_platform() {
        return;
    }
    let fixture = Fixture::indexed();
    let value = fixture.check(&[], 0);
    assert_eq!(value["status"], "evaluated");
    assert_eq!(value["cycleGroups"], json!([]));
    assert_eq!(value["observation"], "noneObserved");
    assert_eq!(value["regression"], "notEvaluated");
    assert_eq!(value["evidence"]["coverage"]["edgesUnresolved"], 0);
    assert_eq!(value["config"]["source"], "local");

    let control = format!("{CALLER}pub fn intra_caller() {{}}\npub fn intra_helper() {{}}\n");
    fixture.write(UI, &control);
    fixture.commit("same-file control");
    fixture.full_index();
    let before = fixture.check(&[], 0);
    fixture.assert_edges(&[(UI, CORE, "RESOLVED", 1)]);
    fixture.write(
        UI,
        &control.replace("intra_caller() {}", "intra_caller() { intra_helper(); }"),
    );
    fixture.commit("same-file call");
    fixture.full_index();
    let after = fixture.check(&[], 0);
    fixture.assert_edges(&[(UI, CORE, "RESOLVED", 1), (UI, UI, "RESOLVED", 1)]);
    assert_eq!(before["evidence"]["coverage"]["edgesResolvedIntraFile"], 0);
    assert_eq!(after["evidence"]["coverage"]["edgesResolvedIntraFile"], 1);
    assert_eq!(before["mapping"], after["mapping"]);
    assert_eq!(after["mapping"]["selfEdges"], 0);
}

#[test]
#[allow(non_snake_case)]
fn architecture_check_reverse_call_creates_cycle_group_with_witnesses__slow() {
    if !native_storage_platform() {
        return;
    }
    let fixture = Fixture::cyclic();
    let value = fixture.check(&[], 0);
    assert_eq!(value["observation"], "findings");
    assert_eq!(
        value["violations"],
        json!([{
            "ruleId": "core-to-ui", "srcComponent": "core", "dstComponent": "ui",
            "srcPath": CORE, "dstPath": UI, "weight": 1
        }])
    );
    assert_eq!(
        value["cycleGroups"],
        json!([{
            "members": ["core", "ui"],
            "componentEdges": [
                { "src": "core", "dst": "ui", "weight": 1, "witnessCount": 1,
                  "witnessTruncated": false, "witnesses": [{"srcPath": CORE, "dstPath": UI, "weight": 1}] },
                { "src": "ui", "dst": "core", "weight": 1, "witnessCount": 1,
                  "witnessTruncated": false, "witnesses": [{"srcPath": UI, "dstPath": CORE, "weight": 1}] }
            ]
        }])
    );
}

#[test]
#[allow(non_snake_case)]
fn architecture_check_baseline_unchanged_is_existing_and_regression_none__slow() {
    if !native_storage_platform() {
        return;
    }
    let fixture = Fixture::cyclic();
    fixture.baseline();
    let value = fixture.check(&["--baseline", "arch0", "--fail"], 0);
    assert_eq!(value["baseline"]["status"], "comparable");
    assert_eq!(value["observation"], "findings");
    assert_eq!(value["regression"], "none");
    assert_eq!(value["violations"][0]["class"], "existing");
    assert_eq!(value["violations"][0]["baselineWeight"], 1);
    assert_eq!(value["cycleGroups"][0]["class"], "unchanged");
    assert_eq!(value["counts"]["violations"]["existing"], 1);
    assert_eq!(value["counts"]["cycleGroups"]["unchanged"], 1);
}

#[test]
#[allow(non_snake_case)]
fn architecture_check_new_forbidden_call_is_regression_present__slow() {
    if !native_storage_platform() {
        return;
    }
    let fixture = Fixture::cyclic();
    fixture.baseline();
    fixture.write(EXTRA, "pub fn additional() { caller(); }\n");
    fixture.commit("one more forbidden path pair");
    fixture.full_index();
    fixture.assert_edges(&[
        (CORE, UI, "RESOLVED", 1),
        (EXTRA, UI, "RESOLVED", 1),
        (UI, CORE, "RESOLVED", 1),
    ]);

    fixture.write(
        ".ledgerful/config.toml",
        r#"
[[verify.steps]]
description = "unused empty step"
command = ""

[[verify.steps]]
description = "unused zero-timeout step"
command = "echo unused"
timeout_secs = 0
"#,
    );
    let advisory = fixture.check(&["--baseline", "arch0"], 0);
    let failed = fixture.check(&["--baseline", "arch0", "--fail"], 1);
    assert_eq!(advisory, failed);
    assert_eq!(advisory["regression"], "present");
    assert_eq!(advisory["counts"]["violations"]["new"], 1);
    assert_eq!(advisory["violations"][1]["srcPath"], EXTRA);
    assert_eq!(advisory["violations"][1]["class"], "new");
}

#[test]
#[allow(non_snake_case)]
fn architecture_check_uncommitted_edit_without_reindex_is_unknown__slow() {
    if !native_storage_platform() {
        return;
    }
    let fixture = Fixture::indexed();
    fixture.baseline();
    fixture.write(CORE, REVERSE);
    let value = fixture.check(&[], 0);
    assert_unknown_current(&value);
    assert_eq!(value["statusReasons"], json!(["CONTENT_DRIFT"]));
    let compared = fixture.check(&["--baseline", "arch0", "--fail"], 1);
    assert_unknown_current(&compared);
    assert_eq!(compared["baseline"]["status"], "incomparable");
    assert_eq!(
        compared["baseline"]["incomparableReasons"],
        json!(["CURRENT_INCOMPLETE"])
    );
    assert_eq!(compared["regression"], "unknown");
}

#[test]
#[allow(non_snake_case)]
fn architecture_check_unknown_baseline_name_exits_nonzero__slow() {
    if !native_storage_platform() {
        return;
    }
    let fixture = Fixture::indexed();
    fixture.reject(&["--baseline", "nosuchname"], "BASELINE_NOT_FOUND");
}

#[test]
#[allow(non_snake_case)]
fn architecture_check_fail_without_baseline_exit_codes__slow() {
    if !native_storage_platform() {
        return;
    }
    let fixture = Fixture::indexed();
    assert_eq!(fixture.check(&["--fail"], 0)["observation"], "noneObserved");
    fixture.make_cycle();
    assert_eq!(fixture.check(&["--fail"], 1)["observation"], "findings");
}

fn schema_refusal(version: usize, diagnostic: &str) {
    let fixture = Fixture::indexed();
    fixture.baseline();
    {
        let db = fixture.db(OpenFlags::SQLITE_OPEN_READ_WRITE);
        db.pragma_update(None, "user_version", version as u32)
            .unwrap();
    }
    let before = fixture.snapshot();
    assert_eq!(before.version as usize, version);
    let files = fixture.files();
    fixture.reject(&[], diagnostic);
    assert_eq!(fixture.files(), files);
    fixture.reject(&["--baseline", "arch0"], diagnostic);
    assert_eq!(fixture.files(), files);
    assert_eq!(fixture.snapshot(), before);
}

#[test]
#[allow(non_snake_case)]
fn architecture_check_refuses_older_database_schema__slow() {
    if !native_storage_platform() {
        return;
    }
    schema_refusal(
        ledgerful::state::migrations::get_migrations_count() - 1,
        "Schema mismatch detected.",
    );
}

#[test]
#[allow(non_snake_case)]
fn architecture_check_refuses_newer_database_schema__slow() {
    if !native_storage_platform() {
        return;
    }
    schema_refusal(
        ledgerful::state::migrations::get_migrations_count() + 1,
        "BASELINE_SCHEMA_TOO_NEW",
    );
}

fn alter_fixture_baseline(fixture: &Fixture, update: impl FnOnce(&Connection)) {
    let mut connection = fixture.db(OpenFlags::SQLITE_OPEN_READ_WRITE);
    let transaction = connection.transaction().unwrap();
    let update_trigger: String = transaction
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name = 'analysis_baselines_no_update'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    transaction
        .execute_batch("DROP TRIGGER analysis_baselines_no_update")
        .unwrap();
    update(&transaction);
    transaction.execute_batch(&update_trigger).unwrap();
    transaction.commit().unwrap();
}

fn corrupt_payload_with_correct_digest(db: &Connection) {
    let malformed = "{malformed";
    let identity: String = db
        .query_row(
            "SELECT identity_json FROM analysis_baselines WHERE name = 'arch0'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut digest = blake3::Hasher::new();
    digest.update(identity.as_bytes());
    digest.update(&[0]);
    digest.update(malformed.as_bytes());
    db.execute(
        "UPDATE analysis_baselines SET payload_json = ?1, payload_digest = ?2 WHERE name = 'arch0'",
        rusqlite::params![malformed, format!("blake3:{}", digest.finalize().to_hex())],
    )
    .unwrap();
}

fn distort_digest(db: &Connection) {
    db.execute(
        "UPDATE analysis_baselines SET payload_digest = 'different' WHERE name = 'arch0'",
        [],
    )
    .unwrap();
}

fn increase_row_schema(db: &Connection) {
    db.execute(
        "UPDATE analysis_baselines SET baseline_schema = 99 WHERE name = 'arch0'",
        [],
    )
    .unwrap();
}

#[test]
#[allow(non_snake_case)]
fn architecture_check_strict_baseline_refusal_matrix__slow() {
    if !native_storage_platform() {
        return;
    }
    type BaselineMutation = fn(&Connection);
    let cases: [(&str, BaselineMutation); 3] = [
        ("BASELINE_DIGEST_MISMATCH", distort_digest),
        ("SCHEMA_MISMATCH", increase_row_schema),
        ("BASELINE_CORRUPT", corrupt_payload_with_correct_digest),
    ];
    for (reason, mutation) in cases {
        let fixture = Fixture::cyclic();
        fixture.baseline();
        alter_fixture_baseline(&fixture, mutation);

        let before = fixture.files();
        let value = fixture.check(&["--baseline", "arch0"], 0);
        assert_eq!(fixture.files(), before);
        assert_eq!(fixture.check(&["--baseline", "arch0", "--fail"], 1), value);
        assert_eq!(fixture.files(), before);
        assert_eq!(value["baseline"]["status"], "incomparable");
        assert_eq!(value["baseline"]["incomparableReasons"], json!([reason]));
        assert_eq!(value["regression"], "unknown");
        assert_eq!(value["observation"], "findings");
        assert!(value["violations"][0].get("class").is_none());
        assert!(value["cycleGroups"][0].get("class").is_none());
        for key in ["counts", "removedViolations", "removedCycleGroups"] {
            assert!(value.get(key).is_none());
        }

        if reason == "BASELINE_CORRUPT" {
            fixture.write(CORE, HELPER);
            let before = fixture.files();
            let value = fixture.check(&["--baseline", "arch0"], 0);
            assert_eq!(fixture.files(), before);
            assert_unknown_current(&value);
            assert_eq!(
                value["baseline"]["incomparableReasons"],
                json!([reason, "CURRENT_INCOMPLETE"])
            );
        }
    }

    let fixture = Fixture::indexed();
    fixture.reject(&["--baseline", "nosuchname"], "BASELINE_NOT_FOUND");
    fixture.reject(&["--baseline", "UPPER"], "BASELINE_NAME_INVALID");
    fixture
        .db(OpenFlags::SQLITE_OPEN_READ_WRITE)
        .execute_batch("DROP TABLE analysis_baselines")
        .unwrap();
    fixture.reject(
        &["--baseline", "arch0"],
        "no such table: analysis_baselines",
    );
}

#[test]
#[allow(non_snake_case)]
fn architecture_check_preserves_repository_and_state__slow() {
    if !native_storage_platform() {
        return;
    }
    let fixture = Fixture::cyclic();
    fixture.baseline();
    let ready = fixture.snapshot();
    let before = fixture.files();
    assert_eq!(
        fixture.check(&["--baseline", "arch0"], 0)["status"],
        "evaluated"
    );
    assert_eq!(fixture.files(), before);
    assert_eq!(fixture.snapshot(), ready);

    fixture.write(CORE, HELPER);
    let stale = fixture.snapshot();
    let before = fixture.files();
    assert_unknown_current(&fixture.check(&["--baseline", "arch0"], 0));
    assert_eq!(fixture.files(), before);
    assert_eq!(fixture.snapshot(), stale);

    fixture.write(CORE, REVERSE);
    alter_fixture_baseline(&fixture, distort_digest);
    let incomparable = fixture.snapshot();
    let before = fixture.files();
    assert_eq!(
        fixture.check(&["--baseline", "arch0"], 0)["baseline"]["status"],
        "incomparable"
    );
    assert_eq!(fixture.files(), before);
    assert_eq!(fixture.snapshot(), incomparable);

    #[cfg(windows)]
    check_windows_startup_sentinels();
}

#[cfg(windows)]
fn check_windows_startup_sentinels() {
    for should_succeed in [false, true] {
        let fixture = if should_succeed {
            Fixture::indexed()
        } else {
            Fixture::empty()
        };
        if !should_succeed {
            fixture.write(".gitignore", "# original fixture ignore\n");
        }
        fixture.write(".changeguard/legacy.marker", "legacy state sentinel\n");
        fixture.write(
            "bin/ledgerful.old.architecture-fixture.exe",
            "adjacent sentinel\n",
        );
        let binary = fixture.root.join("bin/ledgerful.exe");
        fs::copy(BIN, &binary).unwrap();
        let sentinel_paths = [
            fixture.root.join(".gitignore"),
            fixture.root.join(".changeguard/legacy.marker"),
            fixture
                .root
                .join("bin/ledgerful.old.architecture-fixture.exe"),
        ];
        let before = sentinel_paths
            .each_ref()
            .map(|file| fs::read(file).unwrap());
        let status = fixture.git(&["--no-optional-locks", "status", "--porcelain=v1"]);
        if should_succeed {
            assert_eq!(
                fixture.check_binary(binary.to_str().unwrap(), &[], 0)["observation"],
                "noneObserved"
            );
        } else {
            let (out, err, exit) = fixture.refusal(binary.to_str().unwrap(), &[]);
            assert_refusal(out, err, exit, "ARCHITECTURE_CONFIG_NOT_FOUND");
            assert!(!fixture.root.join(".ledgerful").exists());
        }
        let after = sentinel_paths
            .each_ref()
            .map(|file| fs::read(file).unwrap());
        assert_eq!(before, after);
        assert_eq!(
            fixture.git(&["--no-optional-locks", "status", "--porcelain=v1"]),
            status
        );
    }
}

#[test]
#[allow(non_snake_case)]
fn architecture_check_all_unmapped_fails_closed__slow() {
    if !native_storage_platform() {
        return;
    }
    let fixture = Fixture::indexed();
    fixture.baseline();
    fixture.write(
        ".ledgerful/architecture.toml",
        &CONFIG.replace("src/", "not-matching/"),
    );
    fixture.assert_edges(&[(UI, CORE, "RESOLVED", 1)]);

    let current = fixture.check(&[], 0);
    assert_eq!(current["status"], "evaluated");
    assert_eq!(current["mapping"]["crossComponentEdges"], 0);
    assert_eq!(current["mapping"]["unmappedEdges"], 1);
    assert_eq!(current["observation"], "unknown");
    assert_eq!(
        current["observationReasons"],
        json!(["NO_MAPPED_CROSS_COMPONENT_EDGES"])
    );
    assert_eq!(fixture.check(&["--fail"], 1), current);

    let compared = fixture.check(&["--baseline", "arch0"], 0);
    assert_eq!(compared["baseline"]["status"], "comparable");
    assert_eq!(compared["regression"], "unknown");
    assert_eq!(
        compared["regressionReasons"],
        json!(["NO_MAPPED_CROSS_COMPONENT_EDGES"])
    );
    assert_eq!(
        fixture.check(&["--baseline", "arch0", "--fail"], 1),
        compared
    );
}
