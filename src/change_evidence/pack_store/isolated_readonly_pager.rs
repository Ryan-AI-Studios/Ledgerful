//! A private SQLite pager over a caller-fenced, validated MAIN and optional WAL.
use crate::change_evidence::http_worker::{WorkerContext, WorkerError};
use rusqlite::{
    Connection, OpenFlags,
    config::DbConfig,
    ffi,
    hooks::{AuthAction, AuthContext, Authorization},
};
use std::{
    cell::{RefCell, UnsafeCell},
    ffi::{CStr, c_char, c_double, c_int, c_void},
    marker::PhantomData,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::{self, NonNull},
    slice,
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const VFS_NAME: &CStr = c"ledgerful-evidence-private-pager";
const MAIN_NAME: &CStr = c"/ledgerful-evidence-private-main";
const WAL_NAME: &CStr = c"/ledgerful-evidence-private-main-wal";
// SQLite reads one 24-byte WAL frame header and its page in one VFS call.
const MAX_READ: usize = 24 + 65_536;
static RETIREMENT_FAILED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Role {
    Main,
    Wal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InputError {
    Budget,
    Unavailable,
}

/// Present one immutable snapshot for this entire call, through checked
/// SQLite close. The caller holds the independent fence and makes a final
/// stability check before using the result. WAL length is the certified
/// committed prefix. The source must complete exact reads and account for
/// its own native I/O. The adapter additionally charges VFS copying.
pub(super) trait PagerSource {
    fn len(&self, role: Role) -> Option<u64>;
    fn charge(&mut self, units: usize) -> Result<(), InputError>;
    fn read(&mut self, role: Role, offset: u64, out: &mut [u8]) -> Result<(), InputError>;
}

#[derive(Debug)]
pub(super) enum PagerError<E> {
    Input(InputError),
    Sqlite(rusqlite::Error),
    Retirement,
    Worker(WorkerError),
    Query(E),
}

struct Active<'a> {
    id: u64,
    source: &'a mut dyn PagerSource,
    main_len: u64,
    wal_len: Option<u64>,
    main_opened: bool,
    failure: Option<InputError>,
}

impl Active<'_> {
    fn len(&self, role: Role) -> Option<u64> {
        match role {
            Role::Main => Some(self.main_len),
            Role::Wal => self.wal_len,
        }
    }

    fn meter(&mut self, units: usize) -> Result<(), InputError> {
        self.use_source(|source| source.charge(units))
    }

    fn use_source(
        &mut self,
        f: impl FnOnce(&mut dyn PagerSource) -> Result<(), InputError>,
    ) -> Result<(), InputError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        // Rust unwinding must stop at the VFS boundary.
        let result = catch_unwind(AssertUnwindSafe(|| f(self.source)))
            .unwrap_or(Err(InputError::Unavailable));
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }
}

thread_local! {
    static ACTIVE: RefCell<Option<NonNull<Active<'static>>>> = const { RefCell::new(None) };
}
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

struct ActiveSlot<'a>(PhantomData<&'a mut ()>);

impl<'a> ActiveSlot<'a> {
    fn enter(active: &'a mut Active<'_>) -> Option<Self> {
        ACTIVE
            .try_with(|slot| {
                let mut slot = slot.try_borrow_mut().ok()?;
                if slot.is_some() {
                    return None;
                }
                *slot = Some(NonNull::from(active).cast());
                Some(Self(PhantomData))
            })
            .ok()
            .flatten()
    }
}

impl Drop for ActiveSlot<'_> {
    fn drop(&mut self) {
        // Calls into SQLite have completed or unwound before scope teardown.
        let _ = ACTIVE.try_with(|slot| slot.replace(None));
    }
}

fn with_active<T>(f: impl FnOnce(&mut Active<'_>) -> T) -> Option<T> {
    ACTIVE
        .try_with(|slot| {
            let mut slot = slot.try_borrow_mut().ok()?;
            let active = slot.as_mut()?;
            // SAFETY: the synchronous scope keeps the stack context and its
            // source live. The TLS borrow excludes reentrant mutable access.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            Some(f(unsafe { active.as_mut() }))
        })
        .ok()
        .flatten()
}

#[repr(C)]
struct PagerFile {
    base: ffi::sqlite3_file,
    active_id: u64,
    role: Role,
}

fn with_file<T>(
    file: *mut ffi::sqlite3_file,
    f: impl FnOnce(&mut Active<'_>, Role) -> T,
) -> Option<T> {
    // SAFETY: SQLite dispatches file callbacks only for files successfully
    // initialized by this VFS; the base field is at offset zero.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let file = unsafe { &*file.cast::<PagerFile>() };
    with_active(|active| (active.id == file.active_id).then(|| f(active, file.role))).flatten()
}

struct PrivateVfs(UnsafeCell<ffi::sqlite3_vfs>);

// SAFETY: only SQLite accesses this table after publication. SQLite owns
// synchronization of the registry link, and the callbacks are immutable.
// nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
unsafe impl Sync for PrivateVfs {}

static VFS: PrivateVfs = PrivateVfs(UnsafeCell::new(ffi::sqlite3_vfs {
    iVersion: 1,
    szOsFile: size_of::<PagerFile>() as c_int,
    mxPathname: 64,
    pNext: ptr::null_mut(),
    zName: VFS_NAME.as_ptr(),
    pAppData: ptr::null_mut(),
    xOpen: Some(open),
    xDelete: Some(delete),
    xAccess: Some(access),
    xFullPathname: Some(full_pathname),
    xDlOpen: Some(dl_open),
    xDlError: Some(dl_error),
    xDlSym: Some(dl_sym),
    xDlClose: Some(dl_close),
    xRandomness: Some(randomness),
    xSleep: Some(sleep),
    xCurrentTime: Some(current_time),
    xGetLastError: None,
    xCurrentTimeInt64: None,
    xSetSystemCall: None,
    xGetSystemCall: None,
    xNextSystemCall: None,
}));

static REGISTERED: OnceLock<c_int> = OnceLock::new();

pub(super) fn register_private_pager() -> rusqlite::Result<()> {
    let code = *REGISTERED.get_or_init(|| {
        // SAFETY: this table and its name live for the process. Register it
        // without changing the default used by other SQLite connections.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        unsafe {
            if !ffi::sqlite3_vfs_find(VFS_NAME.as_ptr()).is_null() {
                return ffi::SQLITE_MISUSE;
            }
            ffi::sqlite3_vfs_register(VFS.0.get(), 0)
        }
    });
    if code != ffi::SQLITE_OK {
        return Err(rusqlite::Error::SqliteFailure(ffi::Error::new(code), None));
    }
    // SAFETY: the process-lifetime private name and VFS are already initialized.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let resolved = unsafe { ffi::sqlite3_vfs_find(VFS_NAME.as_ptr()) };
    if resolved != VFS.0.get() {
        return Err(rusqlite::Error::SqliteFailure(
            ffi::Error::new(ffi::SQLITE_MISUSE),
            None,
        ));
    }
    Ok(())
}

/// Execute trusted canonical SQL synchronously. No Rust connection, statement
/// or borrowed result can leave this scope. On a database close failure this
/// fails the call; later callbacks cannot use the expired input lease.
pub(super) fn with_readonly_connection<T, E>(
    source: &mut dyn PagerSource,
    worker: Option<&WorkerContext>,
    query: impl FnOnce(&Connection) -> Result<T, E>,
) -> Result<T, PagerError<E>> {
    if RETIREMENT_FAILED.load(Ordering::Acquire) {
        return Err(PagerError::Input(InputError::Unavailable));
    }
    register_private_pager().map_err(PagerError::Sqlite)?;
    let unavailable = || PagerError::Input(InputError::Unavailable);
    let main_len = source
        .len(Role::Main)
        .filter(|&len| len >= 100 && i64::try_from(len).is_ok())
        .ok_or_else(unavailable)?;
    let wal_len = source.len(Role::Wal);
    if wal_len.is_some_and(|len| len != 0 && (len < 32 || i64::try_from(len).is_err())) {
        return Err(unavailable());
    }
    let id = NEXT_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map_err(|_| unavailable())?;
    let mut active = Active {
        id,
        source,
        main_len,
        wal_len,
        main_opened: false,
        failure: None,
    };
    active.meter(0).map_err(PagerError::Input)?;
    let slot = ActiveSlot::enter(&mut active).ok_or_else(unavailable)?;
    let result = open_and_query(wal_len.is_some(), worker, query);
    drop(slot);
    if matches!(&result, Err(PagerError::Retirement)) {
        return result;
    }
    if let Some(error) = active.failure {
        return Err(PagerError::Input(error));
    }
    active.meter(0).map_err(PagerError::Input)?;
    result
}

fn open_and_query<T, E>(
    has_wal: bool,
    worker: Option<&WorkerContext>,
    query: impl FnOnce(&Connection) -> Result<T, E>,
) -> Result<T, PagerError<E>> {
    let path = MAIN_NAME
        .to_str()
        .map_err(|_| PagerError::Input(InputError::Unavailable))?;
    let mut db = Connection::open_with_flags_and_vfs(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        VFS_NAME,
    )
    .map_err(PagerError::Sqlite)?;
    let result = catch_unwind(AssertUnwindSafe(|| {
        configure(&mut db).map_err(PagerError::Sqlite)?;
        let run = |connection: &Connection| {
            // Check that SQLite selected the intended journal profile before SQL.
            let journal_mode = connection
                .query_row("PRAGMA main.journal_mode", [], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(PagerError::Sqlite)?;
            if (journal_mode == "wal") != has_wal {
                return Err(PagerError::Input(InputError::Unavailable));
            }
            query(connection).map_err(PagerError::Query)
        };
        if let Some(worker) = worker {
            let registered = worker.register(&mut db).map_err(PagerError::Worker)?;
            run(registered.connection())
        } else {
            run(&db)
        }
    }));
    if let Err((failed, _)) = db.close() {
        // Disable future private opens before the returned connection's Drop
        // retries close. The input owner serializes the private connection lane.
        RETIREMENT_FAILED.store(true, Ordering::Release);
        drop(failed);
        return Err(PagerError::Retirement);
    }
    match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

fn configure(db: &mut Connection) -> rusqlite::Result<()> {
    // SAFETY: this connection is exclusively owned and its handle stays live
    // through both SQLite limit calls. Keep all allocation on the caller thread.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let threads = unsafe {
        ffi::sqlite3_limit(db.handle(), ffi::SQLITE_LIMIT_WORKER_THREADS, 0);
        ffi::sqlite3_limit(db.handle(), ffi::SQLITE_LIMIT_WORKER_THREADS, -1)
    };
    if threads != 0 {
        return Err(rusqlite::Error::SqliteFailure(
            ffi::Error::new(ffi::SQLITE_MISUSE),
            None,
        ));
    }
    // Set exclusive mode before the first pager transaction. The VFS lock
    // callbacks refer only to this pager. Its WAL index remains in SQLite heap.
    let exclusive = db.pragma_update_and_check(None, "locking_mode", "exclusive", |row| {
        row.get::<_, String>(0)
    })?;
    if exclusive != "exclusive"
        || !db.is_readonly("main")?
        || !db.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?
        || !db.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?
        || db.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    db.busy_timeout(Duration::ZERO)?;
    db.pragma_update(None, "query_only", true)?;
    db.authorizer(Some(authorize))
}

fn authorize(context: AuthContext<'_>) -> Authorization {
    match context.action {
        AuthAction::Attach { .. } | AuthAction::Detach { .. } => Authorization::Deny,
        AuthAction::Pragma {
            pragma_name,
            pragma_value: Some(_),
        } => {
            // The canonical ledger queries use these read-only PRAGMAs
            // through SQLite's table-valued metadata functions.
            let metadata = ["index_list", "index_xinfo", "table_list", "table_xinfo"];
            if context
                .database_name
                .is_none_or(|name| name.eq_ignore_ascii_case("main"))
                && metadata
                    .iter()
                    .any(|name| pragma_name.eq_ignore_ascii_case(name))
            {
                Authorization::Allow
            } else {
                Authorization::Deny
            }
        }
        _ => Authorization::Allow,
    }
}

static IO: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    iVersion: 1,
    xClose: Some(close),
    xRead: Some(read),
    xWrite: Some(write),
    xTruncate: Some(truncate),
    xSync: Some(sync),
    xFileSize: Some(file_size),
    xLock: Some(lock),
    xUnlock: Some(unlock),
    xCheckReservedLock: Some(reserved_lock),
    xFileControl: Some(file_control),
    xSectorSize: None,
    xDeviceCharacteristics: Some(device_characteristics),
    xShmMap: None,
    xShmLock: None,
    xShmBarrier: None,
    xShmUnmap: None,
    xFetch: None,
    xUnfetch: None,
};

fn role_for_name(name: *const c_char) -> Option<Role> {
    if name.is_null() {
        return None;
    }
    // SAFETY: SQLite supplies a nul-terminated filename to the VFS.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    match unsafe { CStr::from_ptr(name) } {
        name if name == MAIN_NAME => Some(Role::Main),
        name if name == WAL_NAME => Some(Role::Wal),
        _ => None,
    }
}

unsafe extern "C" fn open(
    _: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    // SAFETY: even on failure SQLite expects a valid file prefix with a null
    // method table. The allocation is owned and sized by SQLite.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        ptr::write(
            file,
            ffi::sqlite3_file {
                pMethods: ptr::null(),
            },
        )
    };
    let Some(role) = role_for_name(name) else {
        return ffi::SQLITE_CANTOPEN;
    };
    let id = with_active(|active| match role {
        Role::Main
            if flags == ffi::SQLITE_OPEN_MAIN_DB | ffi::SQLITE_OPEN_READONLY
                && !active.main_opened =>
        {
            active.main_opened = true;
            Some(active.id)
        }
        Role::Wal
            if flags
                == ffi::SQLITE_OPEN_WAL | ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE
                && active.main_opened
                && active.wal_len.is_some() =>
        {
            // This is SQLite's standard WAL open request. Serve the existing
            // validated prefix read-only. There is no create capability.
            Some(active.id)
        }
        _ => None,
    })
    .flatten();
    let Some(active_id) = id else {
        return ffi::SQLITE_CANTOPEN;
    };
    // SAFETY: the allocation has room for PagerFile and lives through xClose.
    // The output flags are optional and SQLite provides writable storage.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        ptr::write(
            file.cast(),
            PagerFile {
                base: ffi::sqlite3_file { pMethods: &IO },
                active_id,
                role,
            },
        );
        if !out_flags.is_null() {
            out_flags.write(ffi::SQLITE_OPEN_READONLY);
        }
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn read(
    file: *mut ffi::sqlite3_file,
    out: *mut c_void,
    amount: c_int,
    offset: i64,
) -> c_int {
    let (Ok(size), Ok(start)) = (usize::try_from(amount), u64::try_from(offset)) else {
        return ffi::SQLITE_IOERR_READ;
    };
    if out.is_null() || size > MAX_READ || offset.checked_add(i64::from(amount)).is_none() {
        return ffi::SQLITE_IOERR_READ;
    }
    with_file(file, |active, role| {
        let Some(len) = active.len(role) else {
            return ffi::SQLITE_IOERR_READ;
        };
        let present = len.saturating_sub(start).min(size as u64) as usize;
        if active.meter(size + present).is_err() {
            return ffi::SQLITE_IOERR_READ;
        }
        // SAFETY: SQLite supplies amount bytes of writable, exclusive storage.
        // Initialize the whole range before borrowing it as bytes. On a short
        // read SQLite requires the unread tail to contain zeroes.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        unsafe { out.cast::<u8>().write_bytes(0, size) };
        if present != 0 {
            // SAFETY: the full range was initialized, and this exact prefix
            // fits in the SQLite destination and the certified source extent.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            let destination = unsafe { slice::from_raw_parts_mut(out.cast::<u8>(), present) };
            if active
                .use_source(|source| source.read(role, start, destination))
                .is_err()
            {
                return ffi::SQLITE_IOERR_READ;
            }
        }
        if present != size {
            return ffi::SQLITE_IOERR_SHORT_READ;
        }
        ffi::SQLITE_OK
    })
    .unwrap_or(ffi::SQLITE_IOERR_READ)
}

unsafe extern "C" fn file_size(file: *mut ffi::sqlite3_file, out: *mut i64) -> c_int {
    with_file(file, |active, role| {
        let len = active.len(role).and_then(|len| i64::try_from(len).ok());
        let Some(len) = len else {
            return ffi::SQLITE_IOERR_FSTAT;
        };
        if active.meter(1).is_err() {
            return ffi::SQLITE_IOERR_FSTAT;
        }
        // SAFETY: SQLite supplies a writable sqlite3_int64 output.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        unsafe { out.write(len) };
        ffi::SQLITE_OK
    })
    .unwrap_or(ffi::SQLITE_IOERR_FSTAT)
}

unsafe extern "C" fn close(_: *mut ffi::sqlite3_file) -> c_int {
    // The file owns no descriptors, locks, mappings or Rust references.
    ffi::SQLITE_OK
}

unsafe extern "C" fn lock(file: *mut ffi::sqlite3_file, level: c_int) -> c_int {
    match with_file(file, |_, role| role) {
        Some(Role::Main)
            if (ffi::SQLITE_LOCK_SHARED..=ffi::SQLITE_LOCK_EXCLUSIVE).contains(&level) =>
        {
            ffi::SQLITE_OK
        }
        _ => ffi::SQLITE_IOERR_LOCK,
    }
}

unsafe extern "C" fn unlock(_: *mut ffi::sqlite3_file, level: c_int) -> c_int {
    match level {
        ffi::SQLITE_LOCK_NONE | ffi::SQLITE_LOCK_SHARED => ffi::SQLITE_OK,
        _ => ffi::SQLITE_IOERR_UNLOCK,
    }
}

unsafe extern "C" fn reserved_lock(file: *mut ffi::sqlite3_file, out: *mut c_int) -> c_int {
    if with_file(file, |_, role| role) != Some(Role::Main) {
        return ffi::SQLITE_IOERR_CHECKRESERVEDLOCK;
    }
    // SAFETY: SQLite supplies a writable C int. There are no competing
    // connections to this private file or rollback-journal recovery.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { out.write(0) };
    ffi::SQLITE_OK
}

unsafe extern "C" fn access(
    _: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    mode: c_int,
    out: *mut c_int,
) -> c_int {
    let exists = matches!(mode, ffi::SQLITE_ACCESS_EXISTS | ffi::SQLITE_ACCESS_READ)
        && role_for_name(name)
            .is_some_and(|role| with_active(|active| active.len(role).is_some()).unwrap_or(false));
    // SAFETY: SQLite supplies a writable C int for the access result.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { out.write(c_int::from(exists)) };
    ffi::SQLITE_OK
}

unsafe extern "C" fn full_pathname(
    _: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    capacity: c_int,
    out: *mut c_char,
) -> c_int {
    let path = MAIN_NAME.to_bytes_with_nul();
    if role_for_name(name) != Some(Role::Main)
        || !usize::try_from(capacity).is_ok_and(|capacity| capacity >= path.len())
    {
        return ffi::SQLITE_CANTOPEN;
    }
    // SAFETY: SQLite supplies capacity bytes of writable storage. The source
    // is a separate static buffer containing the terminating nul.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { ptr::copy_nonoverlapping(path.as_ptr().cast(), out, path.len()) };
    ffi::SQLITE_OK
}

unsafe extern "C" fn write(_: *mut ffi::sqlite3_file, _: *const c_void, _: c_int, _: i64) -> c_int {
    ffi::SQLITE_READONLY
}

unsafe extern "C" fn truncate(_: *mut ffi::sqlite3_file, _: i64) -> c_int {
    ffi::SQLITE_READONLY
}

unsafe extern "C" fn sync(_: *mut ffi::sqlite3_file, _: c_int) -> c_int {
    ffi::SQLITE_READONLY
}

unsafe extern "C" fn delete(_: *mut ffi::sqlite3_vfs, _: *const c_char, _: c_int) -> c_int {
    ffi::SQLITE_READONLY
}

unsafe extern "C" fn file_control(_: *mut ffi::sqlite3_file, _: c_int, _: *mut c_void) -> c_int {
    ffi::SQLITE_NOTFOUND
}

unsafe extern "C" fn device_characteristics(_: *mut ffi::sqlite3_file) -> c_int {
    // Do not advertise SQLITE_IOCAP_IMMUTABLE. SQLite would skip WAL recovery.
    0
}

unsafe extern "C" fn dl_open(_: *mut ffi::sqlite3_vfs, _: *const c_char) -> *mut c_void {
    ptr::null_mut()
}

unsafe extern "C" fn dl_error(_: *mut ffi::sqlite3_vfs, size: c_int, out: *mut c_char) {
    if size > 0 {
        // SAFETY: SQLite supplies at least one writable byte when size > 0.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        unsafe { out.write(0) };
    }
}

unsafe extern "C" fn dl_sym(
    _: *mut ffi::sqlite3_vfs,
    _: *mut c_void,
    _: *const c_char,
) -> Option<unsafe extern "C" fn(*mut ffi::sqlite3_vfs, *mut c_void, *const c_char)> {
    None
}

unsafe extern "C" fn dl_close(_: *mut ffi::sqlite3_vfs, _: *mut c_void) {}

unsafe extern "C" fn randomness(_: *mut ffi::sqlite3_vfs, _: c_int, _: *mut c_char) -> c_int {
    // SQLite seeds its global PRNG through the default VFS.
    0
}

unsafe extern "C" fn sleep(_: *mut ffi::sqlite3_vfs, _: c_int) -> c_int {
    // The private pager has no competing reader and refuses busy retries.
    0
}

unsafe extern "C" fn current_time(_: *mut ffi::sqlite3_vfs, out: *mut c_double) -> c_int {
    let unix_seconds = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_secs_f64(),
        Err(error) => -error.duration().as_secs_f64(),
    };
    // SAFETY: SQLite supplies a writable double. SQLite uses Julian days.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { out.write(2_440_587.5 + unix_seconds / 86_400.0) };
    ffi::SQLITE_OK
}
