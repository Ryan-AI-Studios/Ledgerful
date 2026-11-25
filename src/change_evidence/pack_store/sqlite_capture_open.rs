use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CaptureDbUnavailable {
    UnsupportedIdentity,
    Scope,
    RevisionChanged,
    Missing,
    Identity,
    Sqlite,
    Schema,
    ForeignKeys,
}

pub(super) fn open_existing(path: &Path) -> Result<Connection, CaptureDbUnavailable> {
    if !path.is_absolute() {
        return Err(CaptureDbUnavailable::Scope);
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NOFOLLOW
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection =
        Connection::open_with_flags(path, flags).map_err(|_| CaptureDbUnavailable::Sqlite)?;
    if connection
        .is_readonly("main")
        .map_err(|_| CaptureDbUnavailable::Sqlite)?
    {
        return Err(CaptureDbUnavailable::Sqlite);
    }
    Ok(connection)
}

/// The caller holds a native writer-excluding lease and establishes that all
/// companions are absent. Writable capture uses open_existing without URI semantics.
#[cfg(any(windows, target_os = "linux"))]
pub(in crate::change_evidence::pack_store) fn cold_sqlite_header(
    header: &[u8; 100],
    file_bytes: u64,
) -> bool {
    let encoded_page_size = u16::from_be_bytes([header[16], header[17]]);
    let page_size = if encoded_page_size == 1 {
        65_536
    } else {
        u64::from(encoded_page_size)
    };
    let pages = u32::from_be_bytes([header[28], header[29], header[30], header[31]]);
    &header[..16] == b"SQLite format 3\0"
        && matches!(&header[18..20], [1, 1] | [2, 2])
        && header[21..24] == [64, 32, 32]
        // The SQLite header's in-header database size is valid only when the
        // file-change counter and the version-valid-for number match.
        && header[24..28] == header[92..96]
        && (512..=65_536).contains(&page_size)
        && page_size.is_power_of_two()
        && pages > 0
        && u64::from(pages) * page_size == file_bytes
}

#[cfg(windows)]
pub(in crate::change_evidence::pack_store) fn open_immutable_planning_reader(
    path: &Path,
) -> Result<Connection, CaptureDbUnavailable> {
    open_windows_read_only_uri(path, "?mode=ro&immutable=1", None)
}

/// For a qualified live WAL pair, pin the exact native SQLite build and use
/// the Win32 reader path that maps the existing SHM with read-only protection.
#[cfg(windows)]
pub(in crate::change_evidence::pack_store) fn open_architecture_wal_reader(
    path: &Path,
) -> Result<Connection, CaptureDbUnavailable> {
    if !qualified_windows_wal_profile() {
        return Err(CaptureDbUnavailable::Sqlite);
    }
    open_windows_read_only_uri(path, "?mode=ro&readonly_shm=1", Some("win32"))
}

#[cfg(windows)]
fn qualified_windows_wal_profile() -> bool {
    use std::ffi::CStr;
    const QUALIFIED_SOURCE: &CStr =
        c"2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76";
    // SAFETY: SQLite exposes its process-lifetime, NUL-terminated source ID.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let source = unsafe { CStr::from_ptr(rusqlite::ffi::sqlite3_sourceid()) };
    source == QUALIFIED_SOURCE && rusqlite::version_number() == 3_054_001
}

/// Retain a read handle on the same native identity as the contained entry.
/// Do not share deletion or follow reparse points.
#[cfg(windows)]
pub(in crate::change_evidence::pack_store) fn open_architecture_input(
    path: &Path,
    expected: crate::change_evidence::contained::Identity,
) -> Option<std::fs::File> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()?;
    let (volume, id) = windows_file_identity(file.as_raw_handle()).ok()?;
    let identity = crate::change_evidence::contained::Identity::from_windows_file_id(volume, id);
    (identity == expected).then_some(file)
}

/// Shared reader custody of the pinned Win32 WAL's writer, checkpoint,
/// recovery, and dead-man switch bytes. Ordinary reader slots stay available.
#[cfg(windows)]
pub(in crate::change_evidence::pack_store) struct ArchitectureWalGuard(std::fs::File);

#[cfg(windows)]
impl ArchitectureWalGuard {
    // The qualified sqlite3.c asserts writer/checkpoint/recovery at
    // 120/121/122. WIN_SHM_DMS = WIN_SHM_BASE + SQLITE_SHM_NLOCK = 128.
    const WRITER_FIRST: u32 = 120;
    const WRITER_COUNT: u32 = 3;
    const DEAD_MAN_SWITCH: u32 = 128;

    pub(in crate::change_evidence::pack_store) fn acquire(file: std::fs::File) -> Option<Self> {
        use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
        use windows_sys::Win32::Storage::FileSystem::LOCKFILE_EXCLUSIVE_LOCK;

        if !qualified_windows_wal_profile() {
            return None;
        }
        Self::lock(&file, Self::WRITER_FIRST, Self::WRITER_COUNT, 0).ok()?;
        // A live SQLite owner must already hold the DMS. Retain a shared
        // DMS through close so SQLite cannot enter first-owner recovery.
        match Self::lock(&file, Self::DEAD_MAN_SWITCH, 1, LOCKFILE_EXCLUSIVE_LOCK) {
            Ok(()) => {
                let _ = Self::unlock(&file, Self::DEAD_MAN_SWITCH, 1);
                return None;
            }
            Err(error) if error.raw_os_error() == i32::try_from(ERROR_LOCK_VIOLATION).ok() => {}
            Err(_) => return None,
        }
        Self::lock(&file, Self::DEAD_MAN_SWITCH, 1, 0).ok()?;
        Some(Self(file))
    }

    pub(in crate::change_evidence::pack_store) fn shm(&self) -> &std::fs::File {
        &self.0
    }

    pub(in crate::change_evidence::pack_store) fn release(self) -> bool {
        let dms = Self::unlock(&self.0, Self::DEAD_MAN_SWITCH, 1);
        let writer = Self::unlock(&self.0, Self::WRITER_FIRST, Self::WRITER_COUNT);
        dms && writer
    }

    fn lock(file: &std::fs::File, offset: u32, count: u32, mode: u32) -> std::io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{LOCKFILE_FAIL_IMMEDIATELY, LockFileEx};
        let mut location = Self::location(offset);
        // SAFETY: the synchronous file handle and OVERLAPPED stay live through
        // the fail-immediate byte-range lock call.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        if unsafe {
            LockFileEx(
                file.as_raw_handle(),
                mode | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                count,
                0,
                &mut location,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn unlock(file: &std::fs::File, offset: u32, count: u32) -> bool {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
        let mut location = Self::location(offset);
        // SAFETY: the live handle and range match its earlier lock.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        unsafe { UnlockFileEx(file.as_raw_handle(), 0, count, 0, &mut location) != 0 }
    }

    fn location(offset: u32) -> windows_sys::Win32::System::IO::OVERLAPPED {
        use windows_sys::Win32::System::IO::{OVERLAPPED, OVERLAPPED_0, OVERLAPPED_0_0};
        OVERLAPPED {
            Anonymous: OVERLAPPED_0 {
                Anonymous: OVERLAPPED_0_0 {
                    Offset: offset,
                    OffsetHigh: 0,
                },
            },
            ..OVERLAPPED::default()
        }
    }
}

#[cfg(windows)]
fn open_windows_read_only_uri(
    path: &Path,
    options: &str,
    vfs: Option<&str>,
) -> Result<Connection, CaptureDbUnavailable> {
    use std::fmt::Write as _;
    let text = path.to_str().ok_or(CaptureDbUnavailable::Scope)?;
    let text = text.strip_prefix(r"\\?\").unwrap_or(text);
    let [drive, b':', b'\\' | b'/', ..] = text.as_bytes() else {
        return Err(CaptureDbUnavailable::Scope);
    };
    if !drive.is_ascii_alphabetic() {
        return Err(CaptureDbUnavailable::Scope);
    }
    let mut uri = String::from("file:///");
    for (position, byte) in text.bytes().enumerate() {
        match byte {
            b'\\' | b'/' => uri.push('/'),
            b':' if position == 1 => uri.push(':'),
            b if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') => {
                uri.push(char::from(b))
            }
            b => write!(&mut uri, "%{b:02X}").map_err(|_| CaptureDbUnavailable::Scope)?,
        }
    }
    uri.push_str(options);
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NOFOLLOW
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let db = match vfs {
        Some(vfs) => Connection::open_with_flags_and_vfs(uri, flags, vfs),
        None => Connection::open_with_flags(uri, flags),
    }
    .map_err(|_| CaptureDbUnavailable::Sqlite)?;
    if !db
        .is_readonly("main")
        .map_err(|_| CaptureDbUnavailable::Sqlite)?
    {
        return Err(CaptureDbUnavailable::Sqlite);
    }
    Ok(db)
}

pub(super) fn qualify_schema(
    connection: &Connection,
    expected_version: i64,
) -> Result<(), CaptureDbUnavailable> {
    // Check identity before this function, since the first SQLite read can
    // attach an existing WAL. Never request a journal-mode change.
    connection
        .busy_timeout(Duration::from_millis(100))
        .map_err(|_| CaptureDbUnavailable::Sqlite)?;
    let version = pragma_int(connection, "user_version")?;
    if version != expected_version {
        return Err(CaptureDbUnavailable::Schema);
    }
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(|_| CaptureDbUnavailable::ForeignKeys)?;
    if pragma_int(connection, "foreign_keys")? != 1 {
        return Err(CaptureDbUnavailable::ForeignKeys);
    }
    Ok(())
}

fn pragma_int(connection: &Connection, pragma: &str) -> Result<i64, CaptureDbUnavailable> {
    connection
        .pragma_query_value(None, pragma, |row| row.get(0))
        .map_err(|_| CaptureDbUnavailable::Sqlite)
}

#[cfg(windows)]
pub(super) fn windows_main_identity(
    connection: &Connection,
) -> Result<(u64, [u8; 16]), CaptureDbUnavailable> {
    use rusqlite::ffi;
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};

    let mut handle: HANDLE = std::ptr::null_mut();
    // SAFETY: the connection is live and exclusively used on this thread;
    // WIN32_GET_HANDLE writes one HANDLE to our initialized output slot. The
    // handle remains SQLite-owned for the lifetime of the connection.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let status = unsafe {
        ffi::sqlite3_file_control(
            connection.handle(),
            c"main".as_ptr(),
            ffi::SQLITE_FCNTL_WIN32_GET_HANDLE,
            (&mut handle as *mut HANDLE).cast(),
        )
    };
    if status != ffi::SQLITE_OK || handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(CaptureDbUnavailable::UnsupportedIdentity);
    }
    windows_file_identity(handle)
}

#[cfg(windows)]
fn windows_file_identity(
    handle: windows_sys::Win32::Foundation::HANDLE,
) -> Result<(u64, [u8; 16]), CaptureDbUnavailable> {
    use std::mem::{MaybeUninit, size_of};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
    };
    let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    let mut id = MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: both calls borrow the live SQLite-owned HANDLE for the duration
    // of this function and receive valid output buffers of the specified sizes.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let ok = unsafe {
        GetFileInformationByHandle(handle, info.as_mut_ptr()) != 0
            && GetFileInformationByHandleEx(
                handle,
                FileIdInfo,
                id.as_mut_ptr().cast(),
                size_of::<FILE_ID_INFO>() as u32,
            ) != 0
    };
    if !ok {
        return Err(CaptureDbUnavailable::Identity);
    }
    // SAFETY: both native calls succeeded and initialized their output buffers.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let (info, id) = unsafe { (info.assume_init(), id.assume_init()) };
    if info.nNumberOfLinks != 1
        || info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
    {
        return Err(CaptureDbUnavailable::Identity);
    }
    Ok((id.VolumeSerialNumber, id.FileId.Identifier))
}
