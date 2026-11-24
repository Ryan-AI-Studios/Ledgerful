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
    uri.push_str("?mode=ro&immutable=1");
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NOFOLLOW
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let db = Connection::open_with_flags(uri, flags).map_err(|_| CaptureDbUnavailable::Sqlite)?;
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
    use std::mem::{MaybeUninit, size_of};
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
    };

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
