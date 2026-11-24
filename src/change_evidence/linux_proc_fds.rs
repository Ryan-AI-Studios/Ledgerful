//! Authenticated Linux process FD descriptors and checked regular-file reopen.
use super::contained::{Identity, ReadBudget, ReadError};
use nix::fcntl::{OFlag, OpenHow, ResolveFlag, open, openat, openat2, readlinkat};
#[cfg(target_env = "gnu")]
use nix::libc;
use nix::sys::stat::Mode;
use nix::sys::statfs::{PROC_SUPER_MAGIC, fstatfs};
use std::fs::File;
use std::io;
#[cfg(target_env = "gnu")]
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

type Result<T> = std::result::Result<T, ReadError>;
const PROC_ROOT_INODE: u64 = 1;
pub(super) const PROC_ACQUIRE_WORK: usize = 256;

pub(super) struct LinuxProcFds(File);

impl LinuxProcFds {
    pub(super) fn open(io: &ReadBudget<'_>) -> Result<Self> {
        const FLAGS: OFlag = OFlag::O_PATH
            .union(OFlag::O_DIRECTORY)
            .union(OFlag::O_NOFOLLOW)
            .union(OFlag::O_CLOEXEC);
        io.checkpoint()?;
        let root = File::from(open(Path::new("/proc"), FLAGS, Mode::empty()).map_err(native)?);
        Self::authenticate(root, io, FLAGS)
    }

    fn authenticate(root: File, io: &ReadBudget<'_>, flags: OFlag) -> Result<Self> {
        let fs = fstatfs(&root).map_err(native)?.filesystem_type();
        let metadata = root.metadata()?;
        if fs != PROC_SUPER_MAGIC || !metadata.is_dir() || metadata.ino() != PROC_ROOT_INODE {
            return Err(ReadError::Unavailable);
        }

        // The kernel publishes a relative PID token at the root's 'self' link.
        // Require proc's view of self to match the PID used for the lookup.
        io.checkpoint()?;
        let pid = std::process::id().to_string();
        let name = readlinkat(&root, "self").map_err(native)?;
        if name.as_encoded_bytes() != pid.as_bytes() {
            return Err(ReadError::Unavailable);
        }

        // Both paths are real directories in this procfs mount, never links or
        // an independently mounted directory below the validated proc root.
        let resolution = OpenHow::new().flags(flags).resolve(
            ResolveFlag::RESOLVE_BENEATH
                | ResolveFlag::RESOLVE_NO_SYMLINKS
                | ResolveFlag::RESOLVE_NO_XDEV,
        );
        io.checkpoint()?;
        let process = File::from(openat2(&root, pid.as_str(), resolution).map_err(native)?);
        let fds = File::from(openat2(&process, "fd", resolution).map_err(native)?);
        io.checkpoint()?;
        Ok(Self(fds))
    }

    pub(super) fn directory(&self) -> &File {
        &self.0
    }

    pub(super) fn reopen_regular(&self, original: &File, io: &ReadBudget<'_>) -> Result<File> {
        let number = original.as_raw_fd().to_string();
        reopen_matching_regular(
            original,
            io,
            || {
                // Following the authenticated proc magic link with O_PATH
                // obtains only a reference. This stage cannot read the file.
                openat(
                    &self.0,
                    number.as_str(),
                    OFlag::O_PATH | OFlag::O_CLOEXEC,
                    Mode::empty(),
                )
                .map(File::from)
                .map_err(native)
            },
            |referent| {
                // Use the independently checked, still-live referent FD. A
                // nonblocking open and another type/identity check precede reads.
                let pinned_number = referent.as_raw_fd().to_string();
                openat(
                    &self.0,
                    pinned_number.as_str(),
                    OFlag::O_RDONLY | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
                    Mode::empty(),
                )
                .map(File::from)
                .map_err(native)
            },
        )
    }
}

fn reopen_matching_regular(
    original: &File,
    io: &ReadBudget<'_>,
    preflight: impl FnOnce() -> Result<File>,
    readable: impl FnOnce(&File) -> Result<File>,
) -> Result<File> {
    io.checkpoint()?;
    let expected = attached_identity(original, false)?;
    let referent = preflight()?;
    if attached_identity(&referent, false)? != expected {
        return Err(ReadError::Changed);
    }
    io.checkpoint()?;
    let file = readable(&referent)?;
    if attached_identity(&file, false)? != expected {
        return Err(ReadError::Changed);
    }
    io.checkpoint()?;
    Ok(file)
}

#[derive(Eq, PartialEq)]
pub(super) struct AttachedIdentity {
    inode: Identity,
    mount_id: u64,
}

impl AttachedIdentity {
    pub(super) fn inventory_bytes(&self) -> [u8; 32] {
        let mut bytes = [0; 32];
        bytes[..24].copy_from_slice(&self.inode.inventory_bytes());
        bytes[24..].copy_from_slice(&self.mount_id.to_be_bytes());
        bytes
    }
}

pub(super) fn attached_identity(file: &File, directory: bool) -> Result<AttachedIdentity> {
    let metadata = file.metadata()?;
    if metadata.is_symlink() {
        return Err(ReadError::Redirect);
    }
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(ReadError::Kind);
    }
    Ok(AttachedIdentity {
        inode: Identity::from_linux_metadata(&metadata),
        mount_id: mount_id(file)?,
    })
}

#[cfg(target_env = "gnu")]
fn mount_id(file: &File) -> Result<u64> {
    let mut result = MaybeUninit::<libc::statx>::zeroed();
    // SAFETY: the borrowed descriptor is live. AT_EMPTY_PATH targets that
    // descriptor. The pointer names an aligned writable statx result.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let status = unsafe {
        libc::statx(
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_MNT_ID,
            result.as_mut_ptr(),
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: statx succeeded and initialized its result.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let result = unsafe { result.assume_init() };
    if result.stx_mask & libc::STATX_MNT_ID == 0 {
        return Err(ReadError::Unavailable);
    }
    Ok(result.stx_mnt_id)
}

#[cfg(not(target_env = "gnu"))]
fn mount_id(_: &File) -> Result<u64> {
    Err(ReadError::Unavailable)
}

fn native(error: nix::errno::Errno) -> ReadError {
    io::Error::from_raw_os_error(error as i32).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    #[test]
    fn ordinary_directory_cannot_authorize_proc_fd_namespace() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path();
        let root = File::from(
            open(
                path,
                OFlag::O_PATH | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .unwrap(),
        );
        let stop = AtomicBool::new(false);
        let io = ReadBudget::new(0, Instant::now() + Duration::from_secs(3), &stop);
        assert!(matches!(
            LinuxProcFds::authenticate(root, &io, OFlag::O_PATH | OFlag::O_DIRECTORY),
            Err(ReadError::Unavailable)
        ));
    }

    #[test]
    fn substituted_metadata_referent_never_enters_readable_stage() {
        let fixture = tempfile::tempdir().unwrap();
        let first = fixture.path().join("first");
        let second = fixture.path().join("second");
        std::fs::write(&first, b"first ordinary file").unwrap();
        std::fs::write(&second, b"second ordinary file").unwrap();
        let metadata_only = |path: &Path| {
            open(path, OFlag::O_PATH | OFlag::O_CLOEXEC, Mode::empty())
                .map(File::from)
                .map_err(native)
        };
        let original = metadata_only(&first).unwrap();
        let stop = AtomicBool::new(false);
        let io = ReadBudget::new(0, Instant::now() + Duration::from_secs(3), &stop);
        assert!(matches!(
            reopen_matching_regular(
                &original,
                &io,
                || metadata_only(&second),
                |_| panic!("unmatched metadata referent reached readable stage"),
            ),
            Err(ReadError::Changed)
        ));
    }
}
