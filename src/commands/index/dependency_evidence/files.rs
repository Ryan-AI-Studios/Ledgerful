use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::path::Path;
use std::time::SystemTime;

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct Identity(u64, u64);

#[cfg(windows)]
#[derive(Debug, PartialEq, Eq)]
struct Identity(u64, [u8; 16]);

#[cfg(not(any(unix, windows)))]
#[derive(Debug, PartialEq, Eq)]
struct Identity;

#[cfg(unix)]
fn identity(file: &File) -> io::Result<Identity> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata()?;
    Ok(Identity(meta.dev(), meta.ino()))
}

#[cfg(windows)]
fn identity(file: &File) -> io::Result<Identity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };

    let mut info = FILE_ID_INFO::default();
    // SAFETY: the file owns its live handle and info is writable for the exact ABI size.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let success = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            std::ptr::from_mut(&mut info).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if success == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Identity(info.VolumeSerialNumber, info.FileId.Identifier))
}

#[cfg(not(any(unix, windows)))]
fn identity(_: &File) -> io::Result<Identity> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(unix)]
fn open_nofollow(path: &Path) -> io::Result<(File, Vec<File>)> {
    use nix::fcntl::{OFlag, open, openat};
    use nix::sys::stat::Mode;
    use std::path::Component;

    // Pin each directory and prohibit symlinks at each step. Nonblocking
    // prevents a regular-file replacement with a FIFO from stalling open.
    let dir = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let mut parts = path.components().peekable();
    if parts.next() != Some(Component::RootDir) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut parent = open(Path::new("/"), dir, Mode::empty())?;
    if parts.peek().is_none() {
        return Ok((File::from(parent), Vec::new()));
    }
    while let Some(Component::Normal(part)) = parts.next() {
        let leaf = parts.peek().is_none();
        let flags = if leaf {
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK
        } else {
            dir
        };
        let child = openat(&parent, part, flags, Mode::empty())?;
        if leaf {
            return Ok((File::from(child), Vec::new()));
        }
        parent = child;
    }
    Err(io::ErrorKind::InvalidInput.into())
}

#[cfg(windows)]
fn open_nofollow(path: &Path) -> io::Result<(File, Vec<File>)> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::{Component, Prefix};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let mut parts = path.components().peekable();
    let drive = match parts.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
            _ => return Err(io::ErrorKind::Unsupported.into()),
        },
        _ => return Err(io::ErrorKind::InvalidInput.into()),
    };
    if parts.next() != Some(Component::RootDir) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(windows_volume_root(drive)?)?;
    check_windows_handle(&file, true)?;
    let volume = windows_final_path(
        &file,
        windows_sys::Win32::Storage::FileSystem::VOLUME_NAME_GUID,
    )?;
    let Some(guid) = volume
        .strip_prefix(r"\\?\Volume{")
        .and_then(|suffix| suffix.strip_suffix("}\\"))
    else {
        return Err(io::ErrorKind::Unsupported.into());
    };
    if guid.len() != 36
        || !guid
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        return Err(io::ErrorKind::Unsupported.into());
    }

    let mut parents = Vec::new();
    while let Some(component) = parts.next() {
        let Component::Normal(name) = component else {
            return Err(io::ErrorKind::InvalidInput.into());
        };
        let parent_component = parts.peek().is_some();
        let child = windows_open_child(&file, name, parent_component)?;
        check_windows_handle(&child, parent_component)?;
        parents.push(file);
        file = child;
    }
    Ok((file, parents))
}

#[cfg(windows)]
fn windows_volume_root(drive: u8) -> io::Result<String> {
    use windows_sys::Win32::Storage::FileSystem::QueryDosDeviceW;

    let name = [u16::from(drive), u16::from(b':'), 0];
    let mut buffer = [0u16; 1024];
    let capacity = buffer.len() as u32;
    // SAFETY: the input is NUL terminated and the separate writable output
    // buffer holds the stated number of UTF-16 code units.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let copied = unsafe { QueryDosDeviceW(name.as_ptr(), buffer.as_mut_ptr(), capacity) };
    if copied == 0 {
        return Err(io::Error::last_os_error());
    }
    let active = buffer
        .get(..copied as usize)
        .and_then(|text| text.split(|&unit| unit == 0).next())
        .ok_or(io::ErrorKind::InvalidData)?;
    let device = String::from_utf16(active).map_err(|_| io::ErrorKind::InvalidData)?;
    let Some(volume) = device.strip_prefix(r"\Device\HarddiskVolume") else {
        return Err(io::ErrorKind::Unsupported.into());
    };
    if volume.is_empty() || !volume.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(io::ErrorKind::Unsupported.into());
    }
    // Bind the root open to the admitted local volume, even if the DOS drive
    // mapping changes between this query and the open.
    Ok(format!(r"\\?\GLOBALROOT{device}\"))
}

#[cfg(windows)]
fn check_windows_handle(file: &File, require_directory: bool) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DEVICE, FILE_ATTRIBUTE_REPARSE_POINT,
    };

    let meta = file.metadata()?;
    if meta.file_attributes() & (FILE_ATTRIBUTE_DEVICE | FILE_ATTRIBUTE_REPARSE_POINT) != 0
        || !(meta.is_dir() || !require_directory && meta.is_file())
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(())
}

#[cfg(windows)]
fn windows_open_child(
    parent: &File,
    name: &std::ffi::OsStr,
    parent_component: bool,
) -> io::Result<File> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{
        HANDLE, INVALID_HANDLE_VALUE, NTSTATUS, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE,
        RtlNtStatusToDosError, UNICODE_STRING,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_GENERIC_READ, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    #[repr(C)]
    struct ObjectAttributes {
        length: u32,
        root: HANDLE,
        name: *mut UNICODE_STRING,
        attributes: u32,
        security_descriptor: *mut core::ffi::c_void,
        security_qos: *mut core::ffi::c_void,
    }

    #[repr(C)]
    struct IoStatusBlock {
        status_or_pointer: usize,
        information: usize,
    }

    #[link(name = "ntdll")]
    // SAFETY: this declaration matches the NtCreateFile system ABI. The sole
    // caller supplies synchronous live inputs and size-checked output storage.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe extern "system" {
        fn NtCreateFile(
            file: *mut HANDLE,
            access: u32,
            attributes: *const ObjectAttributes,
            status: *mut IoStatusBlock,
            allocation_size: *const i64,
            file_attributes: u32,
            share: u32,
            disposition: u32,
            options: u32,
            ea_buffer: *const core::ffi::c_void,
            ea_length: u32,
        ) -> NTSTATUS;
    }

    let mut wide: Vec<u16> = name.encode_wide().collect();
    if wide.is_empty()
        || wide
            .iter()
            .any(|&unit| matches!(unit, 0 | 0x2f | 0x3a | 0x5c))
        || matches!(wide.last(), Some(0x20 | 0x2e))
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let length =
        u16::try_from(wide.len().saturating_mul(2)).map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut name = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: wide.as_mut_ptr(),
    };
    let attrs = ObjectAttributes {
        length: size_of::<ObjectAttributes>() as u32,
        root: parent.as_raw_handle(),
        name: &mut name,
        attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        security_descriptor: null_mut(),
        security_qos: null_mut(),
    };
    let mut handle = null_mut();
    let mut status = IoStatusBlock {
        status_or_pointer: 0,
        information: 0,
    };
    const FILE_OPEN: u32 = 1;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
    const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    // Final read handles rule out concurrent data writers through sharing.
    let sharing = FILE_SHARE_READ
        | if parent_component {
            FILE_SHARE_WRITE
        } else {
            0
        };
    // SAFETY: one separator-free UTF-16 name and its parent handle remain live,
    // with correctly sized structures and outputs for the synchronous NT call.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let result = unsafe {
        NtCreateFile(
            &mut handle,
            FILE_GENERIC_READ,
            &attrs,
            &mut status,
            null(),
            0,
            sharing,
            FILE_OPEN,
            FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
            null(),
            0,
        )
    };
    if result < 0 {
        // SAFETY: this Win32 ABI function accepts any NTSTATUS value.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let error = unsafe { RtlNtStatusToDosError(result) };
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::ErrorKind::InvalidData.into());
    }
    // SAFETY: success transferred a new owned, non-invalid file handle to us.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    Ok(unsafe { File::from_raw_handle(handle) })
}

#[cfg(windows)]
fn windows_final_path(file: &File, flags: u32) -> io::Result<String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;

    let mut buffer = [0u16; 4096];
    // SAFETY: the file owns its live handle and the UTF-16 buffer is writable
    // for the declared number of code units.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            flags,
        )
    } as usize;
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    let units = buffer
        .get(..length)
        .filter(|slice| slice.len() < buffer.len())
        .ok_or(io::ErrorKind::InvalidData)?;
    String::from_utf16(units).map_err(|_| io::ErrorKind::InvalidData.into())
}

#[cfg(not(any(unix, windows)))]
fn open_nofollow(_: &Path) -> io::Result<(File, Vec<File>)> {
    Err(io::ErrorKind::Unsupported.into())
}

/// The OS change timestamp catches in-place writes even when a writer resets
/// mtime. Read and reopen checks also bind the stamp to the opened file object.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Stamp {
    identity: Identity,
    modified: SystemTime,
    changed: (i64, i64),
    len: u64,
    directory: bool,
}

impl Stamp {
    pub(super) fn matches_path(&self, path: &Path) -> bool {
        open_nofollow(path)
            .and_then(|(file, _parents)| stamp(&file))
            .is_ok_and(|current| current == *self)
    }
}

#[cfg(unix)]
fn change_time(_: &File, meta: &Metadata) -> io::Result<(i64, i64)> {
    use std::os::unix::fs::MetadataExt;
    Ok((meta.ctime(), meta.ctime_nsec()))
}

#[cfg(windows)]
fn change_time(file: &File, _: &Metadata) -> io::Result<(i64, i64)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx,
    };
    let mut info = FILE_BASIC_INFO::default();
    // SAFETY: the file owns its live handle and the exact-sized output buffer
    // is writable for the whole synchronous call.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            std::ptr::from_mut(&mut info).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((info.ChangeTime, 0))
}

#[cfg(not(any(unix, windows)))]
fn change_time(_: &File, _: &Metadata) -> io::Result<(i64, i64)> {
    Err(io::ErrorKind::Unsupported.into())
}

fn stamp(file: &File) -> io::Result<Stamp> {
    let meta = file.metadata()?;
    if !meta.is_file() && !meta.is_dir() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(Stamp {
        identity: identity(file)?,
        modified: meta.modified()?,
        changed: change_time(file, &meta)?,
        len: meta.len(),
        directory: meta.is_dir(),
    })
}

pub(super) struct Entry {
    pub(super) name: std::ffi::OsString,
    pub(super) directory: bool,
    pub(super) regular: bool,
    pub(super) hidden: bool,
    pub(super) reparse: bool,
}

pub(super) fn read_directory<T>(
    path: &Path,
    consume: impl FnOnce(&mut dyn Iterator<Item = io::Result<Entry>>) -> T,
) -> io::Result<(T, Stamp)> {
    let (file, _parents) = open_nofollow(path)?;
    let before = stamp(&file)?;
    if !before.directory {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let value = read_directory_handle(&file, consume)?;
    if stamp(&file)? != before || !before.matches_path(path) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok((value, before))
}

#[cfg(target_os = "linux")]
fn read_directory_handle<T>(
    file: &File,
    consume: impl FnOnce(&mut dyn Iterator<Item = io::Result<Entry>>) -> T,
) -> io::Result<T> {
    use std::os::fd::AsRawFd;
    // The kernel exposes this validated descriptor in procfs.
    let path = std::path::PathBuf::from("/proc/self/fd").join(file.as_raw_fd().to_string());
    let mut entries = std::fs::read_dir(path)?.map(|entry| {
        let entry = entry?;
        let kind = entry.file_type()?;
        let name = entry.file_name();
        let hidden = name.as_encoded_bytes().starts_with(b".");
        Ok(Entry {
            name,
            directory: kind.is_dir(),
            regular: kind.is_file(),
            hidden,
            reparse: false,
        })
    });
    Ok(consume(&mut entries))
}

#[cfg(windows)]
fn read_directory_handle<T>(
    file: &File,
    consume: impl FnOnce(&mut dyn Iterator<Item = io::Result<Entry>>) -> T,
) -> io::Result<T> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_NO_MORE_FILES;
    use windows_sys::Win32::Storage::FileSystem::{
        FileFullDirectoryInfo, FileFullDirectoryRestartInfo, GetFileInformationByHandleEx,
    };

    #[repr(align(8))]
    struct DirectoryBuffer([u8; 16 * 1024]);

    let mut buffer = DirectoryBuffer([0; 16 * 1024]);
    let mut restart = true;
    let mut offset = None;
    let mut done = false;
    let entries = std::iter::from_fn(|| {
        if done {
            return None;
        }
        if offset.is_none() {
            buffer.0.fill(0);
            let class = if restart {
                FileFullDirectoryRestartInfo
            } else {
                FileFullDirectoryInfo
            };
            restart = false;
            // SAFETY: the directory handle remains live and the aligned output
            // buffer is writable for exactly the number of bytes supplied.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            let success = unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle(),
                    class,
                    buffer.0.as_mut_ptr().cast(),
                    buffer.0.len() as u32,
                )
            };
            if success == 0 {
                done = true;
                let error = io::Error::last_os_error();
                return if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                    None
                } else {
                    Some(Err(error))
                };
            }
        }
        let start = offset.unwrap_or_default();
        match parse_windows_dir_entry(&buffer.0[start..]) {
            Some((entry, next)) => {
                offset = next.map(|next| start + next);
                Some(Ok(entry))
            }
            None => {
                done = true;
                Some(Err(io::ErrorKind::InvalidData.into()))
            }
        }
    });
    // The Win32 directory classes may return these two synthetics.
    // They are not physical entries and must not consume the walk budget.
    Ok(consume(&mut entries.filter(|entry| {
        !entry
            .as_ref()
            .is_ok_and(|entry| entry.name == "." || entry.name == "..")
    })))
}

#[cfg(windows)]
fn parse_windows_dir_entry(bytes: &[u8]) -> Option<(Entry, Option<usize>)> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DEVICE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FULL_DIR_INFO,
    };

    let read_u32 = |offset| {
        let chunk: [u8; 4] = bytes.get(offset..offset + 4)?.try_into().ok()?;
        Some(u32::from_le_bytes(chunk))
    };
    let next = read_u32(std::mem::offset_of!(FILE_FULL_DIR_INFO, NextEntryOffset))? as usize;
    let attributes = read_u32(std::mem::offset_of!(FILE_FULL_DIR_INFO, FileAttributes))?;
    let length = read_u32(std::mem::offset_of!(FILE_FULL_DIR_INFO, FileNameLength))? as usize;
    let name_start = std::mem::offset_of!(FILE_FULL_DIR_INFO, FileName);
    let name_end = name_start.checked_add(length)?;
    if length == 0 || !length.is_multiple_of(2) || name_end > bytes.len() {
        return None;
    }
    if next != 0
        && (!next.is_multiple_of(8)
            || next < name_end
            || next > bytes.len().checked_sub(name_start)?)
    {
        return None;
    }
    let (units, _) = bytes[name_start..name_end].as_chunks::<2>();
    let wide: Vec<_> = units.iter().copied().map(u16::from_le_bytes).collect();
    if wide
        .iter()
        .any(|&unit| matches!(unit, 0 | 0x2f | 0x3a | 0x5c))
    {
        return None;
    }
    let name = OsString::from_wide(&wide);
    let hidden = wide.first() == Some(&0x2e) || attributes & FILE_ATTRIBUTE_HIDDEN != 0;
    let directory = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    Some((
        Entry {
            name,
            directory,
            regular: !directory && attributes & FILE_ATTRIBUTE_DEVICE == 0,
            hidden,
            reparse: attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        },
        (next != 0).then_some(next),
    ))
}

#[cfg(not(any(target_os = "linux", windows)))]
fn read_directory_handle<T>(
    _: &File,
    _: impl FnOnce(&mut dyn Iterator<Item = io::Result<Entry>>) -> T,
) -> io::Result<T> {
    Err(io::ErrorKind::Unsupported.into())
}

pub(super) fn path_is_missing(path: &Path) -> bool {
    open_nofollow(path).is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
}

pub(super) enum ReadFailure {
    Missing,
    Unreadable,
    Unstable,
    Capped,
    Spelling,
}

pub(super) fn read_stable<T>(
    path: &Path,
    limit: u64,
    detect_overflow: bool,
    consume: impl FnOnce(&mut dyn Read) -> io::Result<T>,
) -> (Result<T, ReadFailure>, u64) {
    let (result, bytes) = read_stamped_from(path, limit, detect_overflow, true, consume);
    (result.map(|(value, _)| value), bytes)
}

pub(super) fn read_stamped<T>(
    path: &Path,
    limit: u64,
    detect_overflow: bool,
    consume: impl FnOnce(&mut dyn Read) -> io::Result<T>,
) -> (Result<(T, Stamp), ReadFailure>, u64) {
    read_stamped_from(path, limit, detect_overflow, false, consume)
}

fn read_stamped_from<T>(
    path: &Path,
    limit: u64,
    detect_overflow: bool,
    require_canonical: bool,
    consume: impl FnOnce(&mut dyn Read) -> io::Result<T>,
) -> (Result<(T, Stamp), ReadFailure>, u64) {
    #[cfg(windows)]
    if require_canonical && !windows_artifact_path_is_safe(path) {
        return (Err(ReadFailure::Spelling), 0);
    }
    let (mut file, _parents) = match open_nofollow(path) {
        Ok(pair) => pair,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return (Err(ReadFailure::Missing), 0);
        }
        Err(_) => return (Err(ReadFailure::Unreadable), 0),
    };
    if require_canonical && !canonical_file_spelling(&file, path) {
        return (Err(ReadFailure::Spelling), 0);
    }
    let Ok(before) = stamp(&file) else {
        return (Err(ReadFailure::Unreadable), 0);
    };
    if before.directory {
        return (Err(ReadFailure::Unreadable), 0);
    }
    if !detect_overflow && before.len > limit {
        return (Err(ReadFailure::Capped), 0);
    }
    let read_limit = limit + u64::from(detect_overflow);
    let mut input = Read::by_ref(&mut file).take(read_limit);
    let value = consume(&mut input);
    let read = read_limit - input.limit();
    if read > limit {
        return (Err(ReadFailure::Capped), read);
    }
    let Ok(value) = value else {
        return (Err(ReadFailure::Unreadable), read);
    };
    if read == before.len
        && stamp(&file).is_ok_and(|after| after == before)
        && before.matches_path(path)
        && (!require_canonical || canonical_file_spelling(&file, path))
    {
        (Ok((value, before)), read)
    } else {
        (Err(ReadFailure::Unstable), read)
    }
}

pub(super) fn canonical_spelling(path: &Path) -> bool {
    let Some(raw) = path.to_str().filter(|_| path.is_absolute()) else {
        return false;
    };
    #[cfg(windows)]
    {
        windows_artifact_path_is_safe(path)
            && open_nofollow(path)
                .is_ok_and(|(file, _parents)| windows_canonical_spelling(&file, raw))
    }
    #[cfg(not(windows))]
    {
        path.canonicalize()
            .is_ok_and(|canonical| canonical.to_str() == Some(raw))
    }
}

#[cfg(windows)]
fn windows_artifact_path_is_safe(path: &Path) -> bool {
    use std::path::Component;

    path.components().all(|part| match part {
        Component::Normal(name) => name
            .to_str()
            .is_some_and(windows_artifact_component_is_safe),
        _ => true,
    })
}

#[cfg(windows)]
fn windows_artifact_component_is_safe(name: &str) -> bool {
    // A physical NTFS entry can have a name that Win32 treats as a DOS device.
    // Reject that ambiguous spelling before opening either accepted path form.
    let basename = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    let upper = basename.to_ascii_uppercase();
    let device = matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        upper.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    });
    !device && !name.chars().any(|character| character.is_ascii_control())
}

fn canonical_file_spelling(_file: &File, path: &Path) -> bool {
    #[cfg(windows)]
    {
        path.to_str()
            .is_some_and(|raw| windows_canonical_spelling(_file, raw))
    }
    #[cfg(not(windows))]
    {
        canonical_spelling(path)
    }
}

#[cfg(windows)]
fn windows_canonical_spelling(file: &File, raw: &str) -> bool {
    windows_final_path(file, 0)
        .is_ok_and(|canonical| canonical == raw || canonical.strip_prefix(r"\\?\") == Some(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[cfg(windows)]
    #[test]
    fn dependency_evidence_windows_enumerates_retained_directory_across_pages() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        std::fs::create_dir(root.join("nested")).unwrap();
        for index in 0..400 {
            File::create(root.join(format!("source-{index:04}.rs"))).unwrap();
        }
        let (entries, stamp) =
            read_directory(root, |entries| entries.collect::<io::Result<Vec<_>>>()).unwrap();
        let entries = entries.unwrap();
        assert_eq!(entries.len(), 401);
        assert!(stamp.matches_path(root));
        let names: std::collections::BTreeSet<_> = entries
            .into_iter()
            .filter(|entry| entry.regular && !entry.hidden && !entry.reparse)
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names.len(), 400);
        assert!(names.contains(std::ffi::OsStr::new("source-0000.rs")));
        assert!(names.contains(std::ffi::OsStr::new("source-0399.rs")));
    }

    #[cfg(windows)]
    #[test]
    fn dependency_evidence_windows_rejects_malformed_directory_records() {
        use windows_sys::Win32::Storage::FileSystem::FILE_FULL_DIR_INFO;

        let mut record = [0u8; 160];
        let name = std::mem::offset_of!(FILE_FULL_DIR_INFO, FileName);
        let length = std::mem::offset_of!(FILE_FULL_DIR_INFO, FileNameLength);
        record[length..length + 4].copy_from_slice(&2u32.to_le_bytes());
        record[name..name + 2].copy_from_slice(&u16::from(b'x').to_le_bytes());
        let (entry, next) = parse_windows_dir_entry(&record).unwrap();
        assert_eq!(entry.name, "x");
        assert_eq!(next, None);

        for offset in [1u32, 64, 160] {
            record[..4].copy_from_slice(&offset.to_le_bytes());
            assert!(parse_windows_dir_entry(&record).is_none());
        }
        record[..4].fill(0);
        for size in [0u32, 1, 100] {
            record[length..length + 4].copy_from_slice(&size.to_le_bytes());
            assert!(parse_windows_dir_entry(&record).is_none());
        }
    }

    #[cfg(windows)]
    #[test]
    fn dependency_evidence_windows_declines_a_leaf_with_an_existing_writer() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().canonicalize().unwrap();
        let path = root.join("fixture.scip");
        let mut writer = File::create(&path).unwrap();
        writer.write_all(b"fixture").unwrap();
        let (result, read) =
            read_stamped(&path, 1024, false, |input| io::copy(input, &mut io::sink()));
        assert_eq!(read, 0);
        assert!(matches!(result, Err(ReadFailure::Unreadable)));
        drop(writer);
        let (result, read) =
            read_stable(&path, 1024, false, |input| io::copy(input, &mut io::sink()));
        assert_eq!(read, 7);
        assert!(matches!(result, Ok(7)));
    }

    #[cfg(windows)]
    #[test]
    fn dependency_evidence_windows_declines_ambiguous_dos_artifact_components() {
        for name in [
            "CON",
            "nul.scip",
            "LPT9.dat",
            "aux .log",
            "CoM¹.scip",
            "ConOut$.txt",
            "a\t.scip",
        ] {
            assert!(!windows_artifact_component_is_safe(name), "{name}");
        }
        for name in ["console.scip", "COM0.scip", "LPT10.scip", "normal.scip"] {
            assert!(windows_artifact_component_is_safe(name), "{name}");
        }
        assert!(!windows_artifact_path_is_safe(Path::new(
            r"C:\NUL\source.scip"
        )));
        assert!(!windows_artifact_path_is_safe(Path::new(
            r"\\?\C:\CON.scip"
        )));
    }

    #[rstest::rstest]
    #[case(100 * 1024 * 1024, false)]
    #[case(100 * 1024 * 1024 + 1, true)]
    fn dependency_evidence_artifact_boundary(#[case] bytes: u64, #[case] capped: bool) {
        let repo = tempfile::tempdir().unwrap();
        let path = repo.path().join("fixture.scip");
        File::create(&path).unwrap().set_len(bytes).unwrap();
        let (result, read) = read_stable(&path, 100 * 1024 * 1024, true, |input| {
            io::copy(input, &mut io::sink())
        });
        assert_eq!(read, bytes);
        if capped {
            assert!(matches!(result, Err(ReadFailure::Capped)));
        } else {
            assert!(matches!(result, Ok(n) if n == bytes));
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn dependency_evidence_detects_mid_read_change() {
        let repo = tempfile::tempdir().unwrap();
        let path = repo.path().join("fixture.scip");
        std::fs::write(&path, b"old payload").unwrap();
        let (result, read) = read_stable(&path, 100, true, |input| {
            let mut bytes = Vec::new();
            input.read_to_end(&mut bytes)?;
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)?
                .write_all(b"changed")?;
            Ok(bytes)
        });
        assert_eq!(read, b"old payload".len() as u64);
        assert!(matches!(result, Err(ReadFailure::Unstable)));
    }

    #[test]
    fn dependency_evidence_source_never_reads_past_budget() {
        let repo = tempfile::tempdir().unwrap();
        let path = repo.path().join("source.rs");
        File::create(&path)
            .unwrap()
            .set_len(8 * 1024 * 1024 + 1)
            .unwrap();
        let (result, read) = read_stable(&path, 8 * 1024 * 1024, false, |input| {
            io::copy(input, &mut io::sink())
        });
        assert_eq!(read, 0);
        assert!(matches!(result, Err(ReadFailure::Capped)));
    }

    #[cfg(unix)]
    #[test]
    fn dependency_evidence_refuses_linked_source_and_linked_parent() {
        use std::os::unix::fs::symlink;
        let repo = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("outside.rs"), b"must not be read").unwrap();
        symlink(
            outside.path().join("outside.rs"),
            repo.path().join("leaf.rs"),
        )
        .unwrap();
        symlink(outside.path(), repo.path().join("subdir")).unwrap();
        for path in [
            repo.path().join("leaf.rs"),
            repo.path().join("subdir/outside.rs"),
        ] {
            let (result, bytes) =
                read_stable(&path, 100, false, |input| io::copy(input, &mut io::sink()));
            assert_eq!(bytes, 0);
            assert!(matches!(result, Err(ReadFailure::Unreadable)));
        }
    }
}
