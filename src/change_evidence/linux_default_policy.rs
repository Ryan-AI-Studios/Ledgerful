//! Retain the Linux Layout lineage and fixed external policy anchors.
use super::buffer::{Reservation, ReservedBytes};
use super::contained::{
    DefaultAncestorAbsence, Identity, ReadBudget, ReadError, read_linux_discovery_home_file,
};
use super::git_config::{MAX_DISCOVERY_HOME_CONFIG_BYTES, admit_discovery_home_config};
use super::linux_proc_fds::{LinuxProcFds, attached_identity};
use super::pack::Work;
use nix::errno::Errno;
use nix::fcntl::{AtFlags, OFlag, OpenHow, ResolveFlag, open, openat, openat2};
use nix::sys::stat::{Mode, fstatat};
use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

type Result<T> = std::result::Result<T, ReadError>;
type OpenLeaf = (File, Identity);
type FixedLeaf = (Option<OpenLeaf>, [u8; 32]);

const PATH_CAP: usize = 4_096;
const COMPONENT_CAP: usize = 64;
const MAX_DISCOVERY_SYSTEM_CONFIG_BYTES: usize = 256;
const PARENT_FLAGS: OFlag = OFlag::O_RDONLY
    .union(OFlag::O_DIRECTORY)
    .union(OFlag::O_NOFOLLOW)
    .union(OFlag::O_CLOEXEC);

struct HeldChain<'a> {
    directories: Vec<File>,
    _memory: Reservation<'a>,
}

impl<'a> HeldChain<'a> {
    fn open(path: &Path, io: &ReadBudget<'_>, work: &mut Work<'a>) -> Result<Self> {
        let len = path.as_os_str().len();
        if len > PATH_CAP {
            return Err(ReadError::Path);
        }
        charge(io, work, 4 * len + 1)?;
        if path.to_str().is_none() {
            return Err(ReadError::Path);
        }
        let mut components = path.components();
        if components.next() != Some(Component::RootDir) {
            return Err(ReadError::Path);
        }
        let count = components.clone().count();
        if count > COMPONENT_CAP {
            return Err(ReadError::Path);
        }
        let bytes = (count + 1) * size_of::<File>();
        let memory = reserve(work, bytes)?;
        let mut directories = Vec::new();
        directories
            .try_reserve_exact(count + 1)
            .map_err(|_| ReadError::Allocation)?;
        if directories.capacity() != count + 1 {
            return Err(ReadError::Memory);
        }
        charge(io, work, 1)?;
        let root = File::from(open(Path::new("/"), PARENT_FLAGS, Mode::empty()).map_err(native)?);
        directories.push(root);
        for component in components {
            let Component::Normal(name) = component else {
                return Err(ReadError::Path);
            };
            if name.len() > 255 {
                return Err(ReadError::Path);
            }
            charge(io, work, 2 * name.len() + 1)?;
            let parent = directories.last().ok_or(ReadError::Unavailable)?;
            let child = openat(parent, name, PARENT_FLAGS, Mode::empty()).map_err(native)?;
            directories.push(File::from(child));
        }
        charge(io, work, 0)?;
        Ok(Self {
            directories,
            _memory: memory,
        })
    }

    fn root(&self) -> Result<&File> {
        self.directories.first().ok_or(ReadError::Unavailable)
    }

    fn tail(&self) -> Result<&File> {
        self.directories.last().ok_or(ReadError::Unavailable)
    }

    fn observe_ancestor_absence(
        &self,
        io: &ReadBudget<'_>,
        work: &mut Work<'_>,
    ) -> Result<Option<DefaultAncestorAbsence>> {
        // Probe only the four fixed candidates on each already held parent.
        const CANDIDATES: [&str; 4] = [".ignore", ".gitignore", ".git", ".jj"];
        let (worktree, parents) = self
            .directories
            .split_last()
            .ok_or(ReadError::Unavailable)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"ledgerful:layout-ancestor-absence:v2\0");
        charge(io, work, 48)?;
        hasher.update(&attached_identity(worktree, true)?.inventory_bytes());
        for parent in parents.iter().rev() {
            charge(io, work, 48)?;
            let before = attached_identity(parent, true)?;
            hasher.update(&before.inventory_bytes());
            for name in CANDIDATES {
                charge(io, work, name.len() + 1)?;
                match fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
                    Err(Errno::ENOENT) => (),
                    Ok(_) => return Ok(None),
                    Err(error) => return Err(native(error)),
                }
            }
            if attached_identity(parent, true)? != before {
                return Err(ReadError::Changed);
            }
        }
        charge(io, work, 32)?;
        Ok(Some(DefaultAncestorAbsence {
            digest: *hasher.finalize().as_bytes(),
            directories: parents.len(),
        }))
    }
}

pub(super) fn observe_environment<'a>(
    trusted_worktree: &Path,
    expected_worktree: &File,
    process_fds: &LinuxProcFds,
    allow_contained_cwd: bool,
    io: &mut ReadBudget<'_>,
    work: &mut Work<'a>,
) -> Result<Option<(DefaultAncestorAbsence, [u8; 32])>> {
    // Reserve for the bounded process-start environment and native path snapshots.
    const ENVIRONMENT_SCRATCH: usize = 512 * 1024;
    let _environment = reserve(work, ENVIRONMENT_SCRATCH)?;
    for name in ["GIT_CONFIG_GLOBAL", "GIT_CONFIG_SYSTEM", "XDG_CONFIG_HOME"] {
        charge(io, work, 1)?;
        if std::env::var_os(name).is_some() {
            return Ok(None);
        }
    }
    let cwd = std::env::current_dir()?;
    let trusted_bytes = trusted_worktree.as_os_str().as_encoded_bytes();
    charge(io, work, 2 * trusted_bytes.len() + 2)?;
    if !(matches_layout_cwd(trusted_worktree, &cwd)
        || (allow_contained_cwd && cwd.starts_with(trusted_worktree)))
    {
        return Ok(None);
    }

    // Establish parents of the Layout's native pathname, never parents of an
    // ambient path that only aliases the final device/inode.
    let current = HeldChain::open(trusted_worktree, io, work)?;
    charge(io, work, 32)?;
    if attached_identity(current.tail()?, true)? != attached_identity(expected_worktree, true)? {
        return Ok(None);
    }
    let Some(ancestors) = current.observe_ancestor_absence(io, work)? else {
        return Ok(None);
    };
    // Match the actual home-selection API used by ignore 0.4.33.
    #[allow(deprecated, reason = "Exact pinned default ignore home selection")]
    let home = std::env::home_dir().ok_or(ReadError::Unavailable)?;
    let home_dirs = HeldChain::open(&home, io, work)?;
    let home_bytes = home.as_os_str().as_encoded_bytes();
    charge(io, work, home_bytes.len() + 1)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ledgerful:default-ignore-global-policy:v3\0");
    hasher.update(&(home_bytes.len() as u64).to_le_bytes());
    hasher.update(home_bytes);
    let (home_config, fixed_home) = fixed_file(home_dirs.tail()?, &[], ".gitconfig", io, work)?;
    hasher.update(&fixed_home);
    if let Some((observed, observed_identity)) = home_config {
        let raw = read_fixed_config(
            process_fds,
            &observed,
            observed_identity,
            MAX_DISCOVERY_HOME_CONFIG_BYTES,
            io,
            work,
        )?;
        charge(io, work, 32 + 32 * raw.as_ref().len())?;
        let _parse = reserve(work, 128 * 1024 + 32 * raw.as_ref().len())?;
        if !admit_discovery_home_config(raw.as_ref()) {
            return Ok(None);
        }
        hasher.update(&[1]);
        hasher.update(blake3::hash(raw.as_ref()).as_bytes());
    } else {
        hasher.update(&[0]);
    }
    // Both fixed XDG fallbacks remain absence-only.
    for (anchor, directories, file) in [
        (home_dirs.tail()?, [".config", "git"].as_slice(), "config"),
        (home_dirs.tail()?, [".config", "git"].as_slice(), "ignore"),
    ] {
        let (observed, anchors) = fixed_file(anchor, directories, file, io, work)?;
        if observed.is_some() {
            return Ok(None);
        }
        hasher.update(&anchors);
    }
    let Some(system_config) = observe_system_config(current.root()?, process_fds, io, work)? else {
        return Ok(None);
    };
    hasher.update(&system_config);
    charge(io, work, 32)?;
    Ok(Some((ancestors, *hasher.finalize().as_bytes())))
}

pub(super) fn observe_worktree_exclude(
    root: &File,
    io: &ReadBudget<'_>,
    work: &mut Work<'_>,
) -> Result<(bool, [u8; 32])> {
    let (observed, anchors) = fixed_file(root, &[".git", "info"], "exclude", io, work)?;
    Ok((observed.is_some(), anchors))
}

fn fixed_file(
    root: &File,
    directories: &[&str],
    leaf: &str,
    io: &ReadBudget<'_>,
    work: &mut Work<'_>,
) -> Result<FixedLeaf> {
    if directories.len() > 2 {
        return Err(ReadError::Path);
    }
    let mut held: [Option<File>; 2] = [None, None];
    let mut hasher = blake3::Hasher::new();
    charge(io, work, 64)?;
    hasher.update(b"ledgerful:fixed-ignore-path:v1\0");
    hasher.update(&identity(root, true)?.inventory_bytes());
    for (index, name) in directories.iter().enumerate() {
        let parent = if index == 0 {
            root
        } else {
            held[index - 1].as_ref().ok_or(ReadError::Unavailable)?
        };
        let Some((file, id)) = fixed_at(parent, name, true, io, work)? else {
            return Ok((None, *hasher.finalize().as_bytes()));
        };
        held[index] = Some(file);
        hasher.update(&id.inventory_bytes());
    }
    let parent = if directories.is_empty() {
        root
    } else {
        held[directories.len() - 1]
            .as_ref()
            .ok_or(ReadError::Unavailable)?
    };
    let observed = fixed_at(parent, leaf, false, io, work)?;
    if let Some((_, id)) = &observed {
        hasher.update(&id.inventory_bytes());
    }
    Ok((observed, *hasher.finalize().as_bytes()))
}

fn fixed_at(
    parent: &File,
    name: &str,
    directory: bool,
    io: &ReadBudget<'_>,
    work: &mut Work<'_>,
) -> Result<Option<OpenLeaf>> {
    charge(io, work, 32 + name.len())?;
    if !name.is_ascii() || name.len() > 10 || name.contains('/') || matches!(name, "" | "." | "..")
    {
        return Err(ReadError::Path);
    }
    let how = OpenHow::new()
        .flags(OFlag::O_PATH | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW)
        .resolve(
            ResolveFlag::RESOLVE_BENEATH
                | ResolveFlag::RESOLVE_NO_SYMLINKS
                | ResolveFlag::RESOLVE_NO_XDEV,
        );
    let descriptor = match openat2(parent, name, how) {
        Err(Errno::ENOENT) => {
            charge(io, work, 0)?;
            return Ok(None);
        }
        Err(error) => return Err(native(error)),
        Ok(descriptor) => descriptor,
    };
    let file = File::from(descriptor);
    let id = identity(&file, directory)?;
    charge(io, work, 0)?;
    Ok(Some((file, id)))
}

fn observe_system_config<'a>(
    root: &File,
    process_fds: &LinuxProcFds,
    io: &mut ReadBudget<'_>,
    work: &mut Work<'a>,
) -> Result<Option<[u8; 32]>> {
    let before = system_config_path(root, io, work)?;
    let mut stamp = blake3::Hasher::new();
    stamp.update(b"ledgerful:fixed-system-config:v1\0");
    stamp.update(&before.stamp);
    if let Some((file, id)) = &before.config {
        let raw = read_fixed_config(
            process_fds,
            file,
            *id,
            MAX_DISCOVERY_SYSTEM_CONFIG_BYTES,
            io,
            work,
        )?;
        charge(io, work, 6 * raw.as_ref().len())?;
        if !admit_hosted_system_config(raw.as_ref()) {
            return Ok(None);
        }
        stamp.update(blake3::hash(raw.as_ref()).as_bytes());
    }
    if system_config_path(root, io, work)?.stamp != before.stamp {
        return Err(ReadError::Changed);
    }
    charge(io, work, 32)?;
    Ok(Some(*stamp.finalize().as_bytes()))
}

struct SystemConfigPath {
    // Hold both original descriptors through the final path observation.
    _etc: Option<File>,
    config: Option<OpenLeaf>,
    stamp: [u8; 32],
}

fn system_config_path(
    root: &File,
    io: &ReadBudget<'_>,
    work: &mut Work<'_>,
) -> Result<SystemConfigPath> {
    let mut stamp = blake3::Hasher::new();
    stamp.update(b"ledgerful:fixed-system-config-path:v1\0");
    record_system_anchor(root, true, &mut stamp, io, work)?;
    let etc = fixed_at(root, "etc", true, io, work)?;
    stamp.update(&[u8::from(etc.is_some())]);
    let config = if let Some((etc, _)) = &etc {
        record_system_anchor(etc, true, &mut stamp, io, work)?;
        fixed_at(etc, "gitconfig", false, io, work)?
    } else {
        None
    };
    stamp.update(&[u8::from(config.is_some())]);
    if let Some((file, _)) = &config {
        record_system_anchor(file, false, &mut stamp, io, work)?;
    }
    Ok(SystemConfigPath {
        _etc: etc.map(|(file, _)| file),
        config,
        stamp: *stamp.finalize().as_bytes(),
    })
}

fn record_system_anchor(
    file: &File,
    directory: bool,
    stamp: &mut blake3::Hasher,
    io: &ReadBudget<'_>,
    work: &mut Work<'_>,
) -> Result<()> {
    charge(io, work, 128)?;
    let meta = file.metadata()?;
    if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        return Err(ReadError::Unavailable);
    }
    stamp.update(&Identity::from_linux_metadata(&meta).inventory_bytes());
    stamp.update(&meta.mode().to_be_bytes());
    stamp.update(&meta.uid().to_be_bytes());
    stamp.update(&meta.gid().to_be_bytes());
    if !directory {
        if meta.nlink() != 1 {
            return Err(ReadError::Redirect);
        }
        stamp.update(&meta.len().to_be_bytes());
        stamp.update(&meta.mtime().to_be_bytes());
        stamp.update(&meta.mtime_nsec().to_be_bytes());
        stamp.update(&meta.ctime().to_be_bytes());
        stamp.update(&meta.ctime_nsec().to_be_bytes());
    }
    Ok(())
}

fn admit_hosted_system_config(bytes: &[u8]) -> bool {
    // The pinned ignore reader scans for excludesfile. Recognize the Ubuntu
    // defaults as inert discovery inputs. The values never enter an ownership
    // check or command execution.
    let Some(rest) = bytes.strip_prefix(b"[safe]\n        directory = *\n") else {
        return false;
    };
    if rest.is_empty() {
        return true;
    }
    let Some(lfs) = rest
        .strip_prefix(b"[filter \"lfs\"]\n")
        .and_then(|lines| lines.strip_suffix(b"\n"))
    else {
        return false;
    };
    // The stock Git LFS installer emits these exact keys in arbitrary order.
    let expected: [&[u8]; 4] = [
        b"\tclean = git-lfs clean -- %f",
        b"\tsmudge = git-lfs smudge -- %f",
        b"\tprocess = git-lfs filter-process",
        b"\trequired = true",
    ];
    let mut seen = [false; 4];
    for line in lfs.split(|&byte| byte == b'\n') {
        let Some(index) = expected.iter().position(|&entry| entry == line) else {
            return false;
        };
        if std::mem::replace(&mut seen[index], true) {
            return false;
        }
    }
    seen.into_iter().all(|present| present)
}

fn read_fixed_config<'a>(
    process_fds: &LinuxProcFds,
    observed: &File,
    expected: Identity,
    limit: usize,
    io: &mut ReadBudget<'_>,
    work: &mut Work<'a>,
) -> Result<ReservedBytes<'a>> {
    charge(io, work, 64)?;
    if observed.metadata()?.nlink() != 1 {
        return Err(ReadError::Redirect);
    }
    let _scratch = reserve(work, 128)?;
    let file = process_fds.reopen_regular(observed, io)?;
    if identity(&file, false)? != expected {
        return Err(ReadError::Changed);
    }
    let raw = read_linux_discovery_home_file(&file, io, work, limit)?;
    if identity(&file, false)? != expected {
        return Err(ReadError::Changed);
    }
    Ok(raw)
}

fn matches_layout_cwd(trusted: &Path, cwd: &Path) -> bool {
    trusted.as_os_str().as_encoded_bytes() == cwd.as_os_str().as_encoded_bytes()
}

fn identity(file: &File, directory: bool) -> Result<Identity> {
    let meta = file.metadata()?;
    if meta.is_symlink() {
        return Err(ReadError::Redirect);
    }
    if (directory && !meta.is_dir()) || (!directory && !meta.is_file()) {
        return Err(ReadError::Kind);
    }
    Ok(Identity::from_linux_metadata(&meta))
}

fn charge(io: &ReadBudget<'_>, work: &mut Work<'_>, units: usize) -> Result<()> {
    io.checkpoint()?;
    work.charge(units).map_err(|_| ReadError::Work)
}

fn reserve<'a>(work: &mut Work<'a>, bytes: usize) -> Result<Reservation<'a>> {
    if work.read_limit(bytes, 0).map_err(|_| ReadError::Work)? != bytes {
        return Err(ReadError::Memory);
    }
    work.memory_budget()
        .reserve(bytes)
        .map_err(|_| ReadError::Memory)
}

fn native(error: Errno) -> ReadError {
    match error {
        Errno::ELOOP | Errno::EXDEV => ReadError::Redirect,
        Errno::EINVAL | Errno::ENOSYS => ReadError::Unavailable,
        _ => io::Error::from_raw_os_error(error as i32).into(),
    }
}

#[cfg(test)]
mod tests {
    use super::admit_hosted_system_config;

    const SAFE: &[u8] = b"[safe]\n        directory = *\n";

    #[test]
    fn recognizes_hosted_system_config_defaults() {
        assert!(admit_hosted_system_config(SAFE));
        for filter in [
            &b"[filter \"lfs\"]\n\tclean = git-lfs clean -- %f\n\tsmudge = git-lfs smudge -- %f\n\tprocess = git-lfs filter-process\n\trequired = true\n"[..],
            &b"[filter \"lfs\"]\n\tprocess = git-lfs filter-process\n\trequired = true\n\tsmudge = git-lfs smudge -- %f\n\tclean = git-lfs clean -- %f\n"[..],
        ] {
            assert!(admit_hosted_system_config(&[SAFE, filter].concat()));
        }
    }

    #[test]
    fn refuses_other_system_config_contents() {
        assert!(!admit_hosted_system_config(b""));
        for extra in [
            SAFE,
            b"# comment\n",
            b"[include]\n\tpath = other-config\n",
            b"[core]\n\texcludesfile = global-ignore\n",
            b"[filter \"lfs\"]\n\trequired = true\n",
            b"[filter \"lfs\"]\n\tclean = git-lfs clean -- %f\n\tsmudge = git-lfs smudge -- %f\n\tprocess = git-lfs filter-process\n\tprocess = git-lfs filter-process\n",
            b"[filter \"lfs\"]\n\tclean = git-lfs clean -- %f\n\tsmudge = git-lfs smudge -- %f\n\tprocess = git-lfs filter-process\n\trequired = false\n",
        ] {
            assert!(!admit_hosted_system_config(&[SAFE, extra].concat()));
        }
    }
}
