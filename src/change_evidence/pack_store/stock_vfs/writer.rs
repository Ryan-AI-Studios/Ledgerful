//! Explicitly authorized cold and existing-WAL writer policy for the shared stock Unix VFS.
use super::{
    Activation, PATH_CAP, Scope, check_active, inner, mapping_matches, read, size, with_scope,
};
use crate::change_evidence::{
    contained::{Identity, ReadBudget},
    pack::Work,
    pack_store::capture_db::sqlite_open::cold_sqlite_header,
};
use nix::{
    errno::Errno,
    fcntl::{OFlag, OpenHow, ResolveFlag, openat, openat2},
    libc,
    sys::stat::Mode,
};
use rusqlite::{Connection, OpenFlags, config::DbConfig, ffi};
use std::{
    ffi::{CStr, c_char, c_int, c_void},
    fs::File,
    os::{
        fd::{AsRawFd, IntoRawFd},
        unix::fs::MetadataExt,
    },
    path::Path,
};

pub(super) const NAMES: [&str; 4] = [
    "ledger.db",
    "ledger.db-wal",
    "ledger.db-shm",
    "ledger.db-journal",
];

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Policy {
    ColdPreflight,
    WarmPreflight,
    AuthorizedWriter,
}

impl Policy {
    pub(super) fn preflights(self) -> bool {
        matches!(self, Self::ColdPreflight | Self::WarmPreflight)
    }
}

// 1000 ordinary SQLite 4 KiB frames and their headers fit this ceiling.
const WARM_WAL_CAP: u64 = 4 * 1024 * 1024;
const WAL_HEADER_BYTES: u64 = 32;
const WAL_FRAME_HEADER_BYTES: u64 = 24;
const SQLITE_MIN_PAGE_BYTES: u64 = 512;
#[cfg(all(target_arch = "x86_64", target_env = "gnu"))]
const WARM_HEAP_BYTES: usize = 24 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct WalEpoch {
    identity: Identity,
    bytes: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl WalEpoch {
    pub(super) fn from_pin(file: &File) -> Option<Self> {
        let info = file.metadata().ok()?;
        let bytes = info.len();
        let first_frame = WAL_HEADER_BYTES + WAL_FRAME_HEADER_BYTES + SQLITE_MIN_PAGE_BYTES;
        if !info.is_file()
            || info.nlink() != 1
            || (bytes != 0 && !(first_frame..=WARM_WAL_CAP).contains(&bytes))
        {
            return None;
        }
        Some(Self {
            identity: Identity::from_linux_file_id(info.dev(), info.ino()),
            bytes: info.len(),
            mtime: (info.mtime(), info.mtime_nsec()),
            ctime: (info.ctime(), info.ctime_nsec()),
        })
    }

    pub(super) fn is_checkpointed(self) -> bool {
        self.bytes == 0
    }

    /// Runs on the stock xOpen result while actual MAIN EXCLUSIVE still
    /// excludes concurrent WAL writers. The caller charges the original I/O.
    pub(super) fn accepts_header(self, header: [u8; WAL_HEADER_BYTES as usize]) -> bool {
        let word = |at: usize| {
            u32::from_be_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
        };
        let magic = word(0);
        let page_bytes = u64::from(word(8));
        if !matches!(magic, 0x377f0682 | 0x377f0683)
            || word(4) != 3_007_000
            || !(SQLITE_MIN_PAGE_BYTES..=65536).contains(&page_bytes)
            || !page_bytes.is_power_of_two()
            || self.is_checkpointed()
            || !(self.bytes - WAL_HEADER_BYTES).is_multiple_of(page_bytes + WAL_FRAME_HEADER_BYTES)
        {
            return false;
        }

        let mut left = 0u32;
        let mut right = 0u32;
        for pair in header[..24].as_chunks::<8>().0 {
            let endian_word = |at| {
                let word = [pair[at], pair[at + 1], pair[at + 2], pair[at + 3]];
                if magic & 1 == 0 {
                    u32::from_le_bytes(word)
                } else {
                    u32::from_be_bytes(word)
                }
            };
            left = left.wrapping_add(endian_word(0)).wrapping_add(right);
            right = right.wrapping_add(endian_word(4)).wrapping_add(left);
        }
        [left, right] == [word(24), word(28)]
    }
}

pub(super) fn qualified_shm_shape(file: &File) -> bool {
    file.metadata().is_ok_and(|info| {
        info.is_file()
            && info.nlink() == 1
            && (32 * 1024..=512 * 1024).contains(&info.len())
            && info.len().is_multiple_of(32 * 1024)
    })
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum OpenRole {
    Main,
    Wal,
    Shm,
    Directory,
    None,
}

impl OpenRole {
    fn index(self) -> Option<usize> {
        match self {
            Self::Main => Some(0),
            Self::Wal => Some(1),
            Self::Shm => Some(2),
            _ => None,
        }
    }
}

pub(super) fn safe_how(flags: OFlag) -> OpenHow {
    OpenHow::new().flags(flags | OFlag::O_CLOEXEC).resolve(
        ResolveFlag::RESOLVE_BENEATH
            | ResolveFlag::RESOLVE_NO_SYMLINKS
            | ResolveFlag::RESOLVE_NO_XDEV,
    )
}

pub(super) fn pin(state: &File, name: &str) -> Result<Option<File>, Errno> {
    match openat2(state, name, safe_how(OFlag::O_PATH | OFlag::O_NOFOLLOW)) {
        Ok(fd) => {
            let file = File::from(fd);
            let info = file.metadata().map_err(|_| Errno::EIO)?;
            if !info.is_file() || info.nlink() != 1 {
                return Err(Errno::EINVAL);
            }
            Ok(Some(file))
        }
        Err(Errno::ENOENT) => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) fn identity(file: &File) -> Option<Identity> {
    let info = file.metadata().ok()?;
    if !info.is_file() || info.nlink() != 1 {
        return None;
    }
    Some(Identity::from_linux_file_id(info.dev(), info.ino()))
}

fn pinned_open(scope: &Scope<'_>, index: usize, mode: OFlag) -> Option<std::os::fd::OwnedFd> {
    let mut number = [0u8; 16];
    let mut value = scope.pins[index].as_ref()?.as_raw_fd() as u32;
    let mut offset = number.len() - 1;
    loop {
        offset = offset.checked_sub(1)?;
        number[offset] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let name = CStr::from_bytes_with_nul(&number[offset..]).ok()?;
    openat(
        scope.owner.proc_fd(),
        name,
        mode | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .ok()
}

fn open_for_writer(
    active: &mut Activation<'_, '_>,
    path: &[u8],
    flags: c_int,
    mode: c_int,
) -> Option<std::os::fd::OwnedFd> {
    let scope = &mut active.scope;
    if !scope.check(active.io, active.work) || active.work.charge(4).is_err() {
        return None;
    }
    if active.role == OpenRole::Directory {
        let end = scope.lengths[0].checked_sub(NAMES[0].len() + 1)?;
        return (scope.policy == Policy::AuthorizedWriter
            && path == &scope.paths[0][..end]
            && flags & libc::O_ACCMODE == libc::O_RDONLY)
            .then(|| {
                openat2(
                    scope.owner.state(),
                    ".",
                    safe_how(OFlag::O_RDONLY | OFlag::O_DIRECTORY),
                )
                .ok()
            })
            .flatten();
    }
    let index = active.role.index()?;
    let readonly_wal = scope.policy == Policy::WarmPreflight && active.role == OpenRole::Wal;
    let access = if readonly_wal {
        OFlag::O_RDONLY
    } else {
        OFlag::O_RDWR
    };
    if path != &scope.paths[index][..scope.lengths[index]]
        || flags & libc::O_ACCMODE != access.bits()
        || (0..3).any(|fd| {
            // SAFETY: F_GETFD accepts any descriptor number and reports
            // EBADF for closed entries without modifying process state.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            flags < 0
        })
    {
        return None;
    }
    if scope.pins[index].is_some() {
        let allowed = match (scope.policy, active.role) {
            (_, OpenRole::Main) => flags & libc::O_CREAT == 0,
            (Policy::WarmPreflight, OpenRole::Wal) => flags & libc::O_CREAT == 0,
            (Policy::AuthorizedWriter, OpenRole::Wal | OpenRole::Shm) => true,
            _ => false,
        };
        if allowed {
            return pinned_open(scope, index, access);
        }
        return None;
    }
    if scope.policy != Policy::AuthorizedWriter || index == 0 || flags & libc::O_CREAT == 0 {
        return None;
    }
    let how = safe_how(
        OFlag::O_RDWR | OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW | OFlag::O_CREAT | OFlag::O_EXCL,
    )
    .mode(Mode::from_bits_truncate(mode as libc::mode_t & 0o777));
    let created = File::from(openat2(scope.owner.state(), NAMES[index], how).ok()?);
    let observed = pin(scope.owner.state(), NAMES[index]).ok()??;
    let matches = identity(&created)
        .zip(identity(&observed))
        .is_some_and(|(actual, pinned)| actual == pinned);
    if !matches
        || !scope
            .owner
            .0
            .take_creation(NAMES[index], active.work, active.io)
            .is_ok()
    {
        return None;
    }
    scope.pins[index] = Some(observed);
    Some(created.into())
}

pub(super) unsafe fn open(
    active: &mut Activation<'_, '_>,
    path: *const c_char,
    flags: c_int,
    mode: c_int,
) -> c_int {
    let mut opened = None;
    if !path.is_null() && active.work.charge(PATH_CAP * 2).is_ok() {
        // SAFETY: this synchronous stock SQLite callback supplies a live
        // NUL-terminated filename. Inspect at most the fixed path limit.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let length = unsafe { libc::strnlen(path, PATH_CAP) };
        if length < PATH_CAP {
            // SAFETY: strnlen just measured these live filename bytes.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            let path = unsafe { std::slice::from_raw_parts(path.cast::<u8>(), length) };
            opened = open_for_writer(active, path, flags, mode);
        }
    }
    let Some(fd) = opened else {
        active.scope.latched = true;
        // SAFETY: libc exposes the calling thread's live errno slot. Stock
        // Unix uses EISDIR to skip its READONLY fallback and reusable-FD lookup.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        unsafe { *libc::__errno_location() = libc::EISDIR };
        return -1;
    };
    match active.role {
        OpenRole::Main => active.scope.main_count += 1,
        OpenRole::Wal => active.scope.wal_count += 1,
        OpenRole::Shm => active.scope.shm_count += 1,
        _ => {}
    }
    if !active.post() {
        active.scope.latched = true;
    }
    fd.into_raw_fd()
}

pub(super) unsafe fn exclusive_readonly_main(
    file: *mut ffi::sqlite3_file,
    flags: *mut c_int,
) -> c_int {
    if file.is_null() || flags.is_null() {
        return ffi::SQLITE_CANTOPEN;
    }
    // SAFETY: this runs just after matched stock xOpen with its live Unix
    // file and SQLite's output flags. Both synchronous lock calls use only
    // that SQLite-owned descriptor, and SQLite keeps ownership on failure.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        let Some(lock) = (*file).pMethods.as_ref().and_then(|methods| methods.xLock) else {
            return ffi::SQLITE_CANTOPEN;
        };
        for level in [ffi::SQLITE_LOCK_SHARED, ffi::SQLITE_LOCK_EXCLUSIVE] {
            let result = lock(file, level);
            if result != ffi::SQLITE_OK {
                return result;
            }
        }
        *flags =
            (*flags & !(ffi::SQLITE_OPEN_READWRITE as c_int)) | ffi::SQLITE_OPEN_READONLY as c_int;
    }
    ffi::SQLITE_OK
}

pub(super) unsafe fn map_writable(
    active: *mut Activation<'_, '_>,
    file: *mut ffi::sqlite3_file,
    page: c_int,
    size: c_int,
    extend: c_int,
    out: *mut *mut c_void,
) -> c_int {
    // SAFETY: wrapper xShmMap passes the live active TLS pointer, live
    // SQLite file and writable output slot. The delegated xShmMap can
    // reenter the open hook, so no Rust reference to Activation crosses it.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        let first = (*active).scope.shm_count == 0;
        if (first && extend != 0) || !(0..16).contains(&page) || !check_active() {
            return ffi::SQLITE_IOERR_SHMMAP;
        }
        let sqlite_file = inner(file);
        let Some(method) = sqlite_file
            .as_ref()
            .and_then(|file| file.pMethods.as_ref())
            .and_then(|methods| methods.xShmMap)
        else {
            return ffi::SQLITE_IOERR_SHMMAP;
        };
        (*active).role = OpenRole::Shm;
        let result = method(sqlite_file, page, size, extend, out);
        (*active).role = OpenRole::None;
        let mapped = *out;
        if result != ffi::SQLITE_OK
            || (*active).scope.shm_count != 1
            || !check_active()
            || (!mapped.is_null() && !mapping_matches(mapped, page, size))
        {
            *out = std::ptr::null_mut();
            (*active).scope.latched = true;
            return ffi::SQLITE_IOERR_SHMMAP;
        }
    }
    ffi::SQLITE_OK
}

pub(in crate::change_evidence::pack_store) fn open_preflight<'memory, T>(
    scope: &mut Scope<'memory>,
    path: &Path,
    io: &ReadBudget<'memory>,
    work: &mut Work<'memory>,
    inspect: impl FnOnce(&mut Connection) -> T,
) -> Option<T> {
    with_preflight_reader(scope, path, io, work, |mut reader| {
        let result = inspect(&mut reader);
        (reader, result)
    })
}

pub(in crate::change_evidence::pack_store) fn with_preflight_reader<'memory, T>(
    scope: &mut Scope<'memory>,
    path: &Path,
    io: &ReadBudget<'memory>,
    work: &mut Work<'memory>,
    inspect: impl FnOnce(Connection) -> (Connection, T),
) -> Option<T> {
    let warm = scope.policy == Policy::WarmPreflight;
    if !scope.policy.preflights() {
        return None;
    }
    let existing_pair = scope.warm_epoch.is_some();
    #[cfg(all(target_arch = "x86_64", target_env = "gnu"))]
    if existing_pair {
        let heap = work.reserve_native_heap(WARM_HEAP_BYTES).ok()?;
        let result = heap.activate(|| read_preflight(scope, path, io, work, warm, inspect))?;
        // The heap covers the observation and checked close. A lower
        // level optional allocation refusal still invalidates this proof.
        return (heap.refusal().is_none()).then_some(result).flatten();
    }
    if existing_pair {
        return None;
    }
    read_preflight(scope, path, io, work, false, inspect)
}

fn read_preflight<'memory, T>(
    scope: &mut Scope<'memory>,
    path: &Path,
    io: &ReadBudget<'memory>,
    work: &mut Work<'memory>,
    warm: bool,
    inspect: impl FnOnce(Connection) -> (Connection, T),
) -> Option<T> {
    let previous_wal_opens = scope.wal_count;
    let previous_shm_opens = scope.shm_count;
    let result = with_scope(scope, io, work, || {
        let reader = open_connection(path)?;
        let ready = || {
            if !reader.is_readonly("main").ok()? || !valid_header(&reader) {
                return None;
            }
            if !warm {
                return Some(());
            }
            // SAFETY: the caller owns this live private connection and the
            // synchronous call limits workers before any statement runs.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            unsafe { ffi::sqlite3_limit(reader.handle(), ffi::SQLITE_LIMIT_WORKER_THREADS, 0) };
            if !suppress_checkpoints(&reader) {
                return None;
            }
            let mode: String = reader
                .query_row("PRAGMA locking_mode=EXCLUSIVE", [], |row| row.get(0))
                .ok()?;
            if mode != "exclusive" || !check_active() {
                return None;
            }
            Some(())
        };
        if ready().is_none() {
            reader.close().ok()?;
            return None;
        }
        let (reader, result) = inspect(reader);
        reader.close().ok()?;
        Some(result)
    })??;
    if scope.shm_count != previous_shm_opens
        || scope.wal_count != previous_wal_opens + usize::from(warm)
    {
        return None;
    }
    Some(result)
}

fn valid_header(connection: &Connection) -> bool {
    let mut file: *mut ffi::sqlite3_file = std::ptr::null_mut();
    // SAFETY: SQLite writes the borrowed MAIN file pointer to a live output
    // slot; the owning Connection remains live while the pointer is used.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let is_wrapped = unsafe {
        ffi::sqlite3_file_control(
            connection.handle(),
            c"main".as_ptr(),
            ffi::SQLITE_FCNTL_FILE_POINTER,
            (&mut file as *mut *mut ffi::sqlite3_file).cast(),
        ) == ffi::SQLITE_OK
            && !file.is_null()
            && (*file).pMethods == &super::METHODS
    };
    if !is_wrapped {
        return false;
    }
    let mut header = [0u8; 100];
    let mut bytes = 0i64;
    // SAFETY: the method table above identified our wrapper. These calls
    // write only the fixed header and length slots before returning.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    if unsafe {
        read(file, header.as_mut_ptr().cast(), 100, 0) != ffi::SQLITE_OK
            || size(file, &mut bytes) != ffi::SQLITE_OK
    } {
        return false;
    }
    header[18..20] == [2, 2]
        && u64::try_from(bytes).is_ok_and(|length| cold_sqlite_header(&header, length))
}

fn open_connection(path: &Path) -> Option<Connection> {
    Connection::open_with_flags_and_vfs(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        "ledgerful-scoped-proof",
    )
    .ok()
}

pub(in crate::change_evidence::pack_store) fn open_authorized<'memory>(
    scope: &mut Scope<'memory>,
    path: &Path,
    io: &ReadBudget<'memory>,
    work: &mut Work<'memory>,
    verify: impl FnOnce(&Connection) -> bool,
) -> Option<Connection> {
    if !scope.policy.preflights() || !scope.check(io, work) {
        return None;
    }
    let existing_pair = scope.warm_epoch.is_some();
    let previous_wal = scope.wal_count;
    let previous_shm = scope.shm_count;
    scope.policy = Policy::AuthorizedWriter;
    let connection = with_scope(scope, io, work, || {
        let conn = open_connection(path)?;
        if conn.is_readonly("main").ok()? {
            return None;
        }
        if !suppress_checkpoints(&conn) || !verify(&conn) {
            return None;
        }
        Some(conn)
    })??;
    if scope.wal_count != previous_wal + 1
        || scope.shm_count != previous_shm + 1
        || !scope.check(io, work)
    {
        return None;
    }
    if existing_pair {
        return Some(connection);
    }
    work.charge(3).ok()?;
    let dir = File::from(
        openat2(
            scope.owner.state(),
            ".",
            safe_how(OFlag::O_RDONLY | OFlag::O_DIRECTORY),
        )
        .ok()?,
    );
    if dir.sync_all().is_err() || !scope.check(io, work) {
        return None;
    }
    Some(connection)
}

fn suppress_checkpoints(connection: &Connection) -> bool {
    if !matches!(
        connection.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true),
        Ok(true)
    ) {
        return false;
    }
    // SAFETY: the caller owns this live connection and sets the stock
    // per-connection autocheckpoint before the first WAL access.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { ffi::sqlite3_wal_autocheckpoint(connection.handle(), 0) == ffi::SQLITE_OK }
}
