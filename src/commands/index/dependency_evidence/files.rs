use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::path::Path;
use std::time::SystemTime;

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct Identity(u64, u64);

#[cfg(windows)]
#[derive(Debug, PartialEq, Eq)]
struct Identity(u32, u32, u32);

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
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // The file retains ownership of the open handle through this call.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Identity(
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    ))
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
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    use std::path::{Component, Prefix};
    let mut parts = path.components();
    if !matches!(parts.next(),
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(),
            Prefix::Disk(_) | Prefix::VerbatimDisk(_) | Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _)))
        || parts.any(|part| matches!(part, Component::ParentDir)
            || matches!(part, Component::Normal(name) if name.as_encoded_bytes().contains(&b':')))
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    // Parent handles deny deletion while opening/reading the path. Open
    // reparse points themselves so a link is never followed by a child open.
    let mut parents: Vec<_> = path.ancestors().skip(1).collect();
    parents.reverse();
    let mut guards = Vec::with_capacity(parents.len());
    let open = |part: &Path| {
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(part)
    };
    for parent in parents.into_iter().filter(|part| part.has_root()) {
        let guard = open(parent)?;
        let meta = guard.metadata()?;
        if !meta.is_dir() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        guards.push(guard);
    }
    let file = open(path)?;
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok((file, guards))
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
    // The handle and the correctly sized output buffer remain live for the call.
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

/// Enumerate a directory through an already validated handle. Linux uses the
/// kernel's descriptor link, not a repository-controlled link. Windows
/// holds non-reparse handles to the directory and all ancestors during read.
#[cfg(any(target_os = "linux", windows))]
pub(super) fn read_directory<T>(
    path: &Path,
    consume: impl FnOnce(std::fs::ReadDir) -> T,
) -> io::Result<(T, Stamp)> {
    let (file, _parents) = open_nofollow(path)?;
    let before = stamp(&file)?;
    if !before.directory {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    #[cfg(target_os = "linux")]
    let opened_path = {
        use std::os::fd::AsRawFd;
        std::path::PathBuf::from("/proc/self/fd").join(file.as_raw_fd().to_string())
    };
    #[cfg(windows)]
    let opened_path = path;
    let value = consume(std::fs::read_dir(opened_path)?);
    if stamp(&file)? != before || !before.matches_path(path) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok((value, before))
}

#[cfg(not(any(target_os = "linux", windows)))]
pub(super) fn read_directory<T>(
    _: &Path,
    _: impl FnOnce(std::fs::ReadDir) -> T,
) -> io::Result<(T, Stamp)> {
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
}

pub(super) fn read_stable<T>(
    path: &Path,
    limit: u64,
    detect_overflow: bool,
    consume: impl FnOnce(&mut dyn Read) -> io::Result<T>,
) -> (Result<T, ReadFailure>, u64) {
    let (result, bytes) = read_stamped(path, limit, detect_overflow, consume);
    (result.map(|(value, _)| value), bytes)
}

pub(super) fn read_stamped<T>(
    path: &Path,
    limit: u64,
    detect_overflow: bool,
    consume: impl FnOnce(&mut dyn Read) -> io::Result<T>,
) -> (Result<(T, Stamp), ReadFailure>, u64) {
    let (mut file, _parents) = match open_nofollow(path) {
        Ok(pair) => pair,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return (Err(ReadFailure::Missing), 0);
        }
        Err(_) => return (Err(ReadFailure::Unreadable), 0),
    };
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
    let Ok(canonical) = path.canonicalize() else {
        return false;
    };
    let Some(canonical) = canonical.to_str() else {
        return false;
    };
    #[cfg(windows)]
    {
        if canonical == raw {
            return true;
        }
        if let Some(unc) = canonical.strip_prefix(r"\\?\UNC\") {
            return raw.strip_prefix(r"\\") == Some(unc);
        }
        canonical.strip_prefix(r"\\?\") == Some(raw)
    }
    #[cfg(not(windows))]
    {
        canonical == raw
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
