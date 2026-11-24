//! Private Linux native database-name witness. It carries no SQL authority.
//! Samples a retained main file and an existing-only SQLite connection; it is
//! not a SQLite VFS or a proof over SQLite auxiliary file handles.
use super::super::buffer::Reservation;
use super::super::contained::ReadBudget;
use super::super::pack::Work;
use nix::{
    fcntl::{OFlag, OpenHow, ResolveFlag, open, openat2},
    libc,
    sys::{
        stat::{Mode, major, minor},
        statfs::{PROC_SUPER_MAGIC, fstatfs},
    },
};
use rusqlite::{Connection, FileControl};
use std::{
    ffi::CStr,
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
};
const MAX_READS: usize = 16;
const BUFFER: usize = 4096;
const EXPECTED: [&[u8]; 3] = [b".ledgerful", b"state", b"ledger.db"];
const COMPANIONS: [&str; 3] = ["ledger.db-wal", "ledger.db-shm", "ledger.db-journal"];
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Id {
    dev: u64,
    ino: u64,
}
fn id(file: &File, work: &mut Work<'_>, io: &ReadBudget<'_>) -> Result<Id, &'static str> {
    boundary(work, io)?;
    let m = file.metadata().map_err(|_| "identity")?;
    Ok(Id {
        dev: m.dev(),
        ino: m.ino(),
    })
}
fn boundary(work: &mut Work<'_>, io: &ReadBudget<'_>) -> Result<(), &'static str> {
    io.checkpoint().map_err(|_| "deadline")?;
    work.charge(1)
}
fn dir(
    parent: &File,
    name: &str,
    work: &mut Work<'_>,
    io: &ReadBudget<'_>,
) -> Result<File, &'static str> {
    boundary(work, io)?;
    let how = OpenHow::new()
        .flags(
            OFlag::O_RDONLY
                | OFlag::O_DIRECTORY
                | OFlag::O_NOFOLLOW
                | OFlag::O_CLOEXEC
                | OFlag::O_NONBLOCK,
        )
        .resolve(
            ResolveFlag::RESOLVE_BENEATH
                | ResolveFlag::RESOLVE_NO_SYMLINKS
                | ResolveFlag::RESOLVE_NO_XDEV,
        );
    openat2(parent, name, how)
        .map(File::from)
        .map_err(|_| "scope-unavailable")
}
fn path_file(
    parent: &File,
    name: &str,
    work: &mut Work<'_>,
    io: &ReadBudget<'_>,
) -> Result<Option<File>, &'static str> {
    boundary(work, io)?;
    let how = OpenHow::new()
        .flags(OFlag::O_PATH | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC)
        .resolve(
            ResolveFlag::RESOLVE_BENEATH
                | ResolveFlag::RESOLVE_NO_SYMLINKS
                | ResolveFlag::RESOLVE_NO_XDEV,
        );
    match openat2(parent, name, how) {
        Ok(fd) => Ok(Some(File::from(fd))),
        Err(nix::errno::Errno::ENOENT) => Ok(None),
        Err(_) => Err("identity"),
    }
}
fn regular(
    file: &File,
    root_dev: u64,
    work: &mut Work<'_>,
    io: &ReadBudget<'_>,
) -> Result<Id, &'static str> {
    boundary(work, io)?;
    let m = file.metadata().map_err(|_| "identity")?;
    if !m.is_file() || m.nlink() != 1 || m.dev() != root_dev {
        return Err("identity");
    }
    Ok(Id {
        dev: m.dev(),
        ino: m.ino(),
    })
}
fn readable_file(
    parent: &File,
    name: &str,
    work: &mut Work<'_>,
    io: &ReadBudget<'_>,
) -> Result<File, &'static str> {
    boundary(work, io)?;
    let how = OpenHow::new()
        .flags(OFlag::O_RDONLY | OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC)
        .resolve(
            ResolveFlag::RESOLVE_BENEATH
                | ResolveFlag::RESOLVE_NO_SYMLINKS
                | ResolveFlag::RESOLVE_NO_XDEV,
        );
    openat2(parent, name, how)
        .map(File::from)
        .map_err(|_| "identity")
}
pub(super) fn proc_anchor(
    proc_fd: &File,
    work: &mut Work<'_>,
    io: &ReadBudget<'_>,
) -> Result<(), &'static str> {
    boundary(work, io)?;
    if fstatfs(proc_fd)
        .map_err(|_| "scope-unavailable")?
        .filesystem_type()
        != PROC_SUPER_MAGIC
    {
        return Err("scope-unavailable");
    }
    boundary(work, io)?;
    let fresh = File::from(
        open(
            "/proc/self/fd",
            OFlag::O_PATH | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| "scope-unavailable")?,
    );
    boundary(work, io)?;
    if id(proc_fd, work, io)? != id(&fresh, work, io)? {
        return Err("scope-unavailable");
    }
    Ok(())
}
fn fd_path(fd: i32) -> Result<([u8; 64], usize), &'static str> {
    let mut out = [0u8; 64];
    let prefix = b"/proc/self/fd/";
    out[..prefix.len()].copy_from_slice(prefix);
    let mut num = u32::try_from(fd).map_err(|_| "watch-unavailable")?;
    let mut digits = [0u8; 10];
    let mut i = digits.len();
    loop {
        i -= 1;
        digits[i] = b'0' + (num % 10) as u8;
        num /= 10;
        if num == 0 {
            break;
        }
    }
    let end = prefix.len() + digits.len() - i;
    out[prefix.len()..end].copy_from_slice(&digits[i..]);
    Ok((out, end + 1))
}
fn parse_hex(input: &[u8]) -> Result<u64, &'static str> {
    if input.is_empty() || input.len() > 16 {
        return Err("watch-framing");
    }
    input.iter().try_fold(0u64, |n, c| {
        let digit = match c {
            b'0'..=b'9' => u64::from(*c - b'0'),
            b'a'..=b'f' => u64::from(*c - b'a' + 10),
            b'A'..=b'F' => u64::from(*c - b'A' + 10),
            _ => return Err("watch-framing"),
        };
        n.checked_mul(16)
            .and_then(|n| n.checked_add(digit))
            .ok_or("watch-framing")
    })
}
fn kernel_sdev(dev: u64) -> Result<u64, &'static str> {
    let maj = major(dev);
    let min = minor(dev);
    if maj > 0xfff || min > 0xfffff {
        return Err("watch-unavailable");
    }
    Ok((maj << 20) | min)
}
fn verify_watch_bindings(
    proc_fd: &File,
    watches: &File,
    watch_ids: &[i32; 3],
    dirs: &[Id; 3],
    work: &mut Work<'_>,
    io: &ReadBudget<'_>,
) -> Result<(), &'static str> {
    proc_anchor(proc_fd, work, io)?;
    boundary(work, io)?;
    let fdinfo = File::from(
        open(
            "/proc/self/fdinfo",
            OFlag::O_PATH | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| "watch-unavailable")?,
    );
    boundary(work, io)?;
    if fstatfs(&fdinfo)
        .map_err(|_| "watch-unavailable")?
        .filesystem_type()
        != PROC_SUPER_MAGIC
    {
        return Err("watch-unavailable");
    }
    let (path, len) = fd_path(watches.as_raw_fd())?;
    let fdname = CStr::from_bytes_with_nul(&path[b"/proc/self/fd/".len()..len])
        .map_err(|_| "watch-unavailable")?;
    boundary(work, io)?;
    let how = OpenHow::new()
        .flags(OFlag::O_RDONLY | OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC)
        .resolve(
            ResolveFlag::RESOLVE_BENEATH
                | ResolveFlag::RESOLVE_NO_SYMLINKS
                | ResolveFlag::RESOLVE_NO_XDEV,
        );
    let file = File::from(openat2(&fdinfo, fdname, how).map_err(|_| "watch-unavailable")?);
    let mut bytes = [0u8; BUFFER];
    let mut used = 0usize;
    loop {
        boundary(work, io)?;
        work.charge(BUFFER * 8)?;
        if used == bytes.len() {
            return Err("watch-cap");
        }
        // SAFETY: the procfs fdinfo descriptor and remaining fixed buffer are valid.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let n = unsafe {
            libc::read(
                file.as_raw_fd(),
                bytes[used..].as_mut_ptr().cast(),
                bytes.len() - used,
            )
        };
        if n < 0 {
            return Err("watch-unavailable");
        }
        if n == 0 {
            break;
        }
        used = used
            .checked_add(n as usize)
            .filter(|n| *n <= bytes.len())
            .ok_or("watch-cap")?;
    }
    let mut seen = [false; 3];
    for line in bytes[..used].split(|b| *b == b'\n') {
        let Some(rest) = line.strip_prefix(b"inotify ") else {
            continue;
        };
        let mut wd = None;
        let mut ino = None;
        let mut dev = None;
        for field in rest.split(|b| *b == b' ') {
            if let Some(v) = field.strip_prefix(b"wd:")
                && wd.replace(parse_hex(v)?).is_some()
            {
                return Err("watch-framing");
            }
            if let Some(v) = field.strip_prefix(b"ino:")
                && ino.replace(parse_hex(v)?).is_some()
            {
                return Err("watch-framing");
            }
            if let Some(v) = field.strip_prefix(b"sdev:")
                && dev.replace(parse_hex(v)?).is_some()
            {
                return Err("watch-framing");
            }
        }
        let wd = i32::try_from(wd.ok_or("watch-framing")?).map_err(|_| "watch-framing")?;
        let i = watch_ids
            .iter()
            .position(|v| *v == wd)
            .ok_or("watch-framing")?;
        if seen[i] || ino != Some(dirs[i].ino) || dev != Some(kernel_sdev(dirs[i].dev)?) {
            return Err("watch-unavailable");
        }
        seen[i] = true;
    }
    if !seen.into_iter().all(|b| b) {
        return Err("watch-unavailable");
    }
    proc_anchor(proc_fd, work, io)
}
pub(super) struct LinuxCaptureGuard<'a> {
    root: File,
    ledger: File,
    state: File,
    main_path: File,
    main_read: File,
    proc_fd: File,
    watches: File,
    watch_ids: [i32; 3],
    dirs: [Id; 3],
    main: Id,
    companions: [Option<File>; 3],
    companion_ids: [Option<Id>; 3],
    latched: bool,
    _scratch: Reservation<'a>,
}
impl<'a> LinuxCaptureGuard<'a> {
    /// `root` and `proc_fd` must come from the same admitted scoped observation.
    pub(super) fn arm(
        root: &File,
        proc_fd: &File,
        work: &mut Work<'a>,
        io: &ReadBudget<'_>,
    ) -> Result<Self, &'static str> {
        boundary(work, io)?;
        if work.read_limit(BUFFER, 0)? != BUFFER {
            return Err("memory-cap");
        }
        let scratch = work
            .memory_budget()
            .reserve(BUFFER)
            .map_err(|_| "memory-cap")?;
        boundary(work, io)?;
        let root = root.try_clone().map_err(|_| "scope-unavailable")?;
        boundary(work, io)?;
        let proc_fd = proc_fd.try_clone().map_err(|_| "scope-unavailable")?;
        boundary(work, io)?;
        let root_id = id(&root, work, io)?;
        let ledger = dir(&root, ".ledgerful", work, io)?;
        let state = dir(&ledger, "state", work, io)?;
        boundary(work, io)?;
        let dirs = [root_id, id(&ledger, work, io)?, id(&state, work, io)?];
        if dirs.iter().any(|x| x.dev != root_id.dev) {
            return Err("identity");
        }
        proc_anchor(&proc_fd, work, io)?;
        boundary(work, io)?;
        // SAFETY: flags are valid and returned descriptor is checked before ownership transfer.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err("watch-unavailable");
        }
        // SAFETY: fd is a newly created live descriptor with unique ownership.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let watches = unsafe { File::from_raw_fd(fd) };
        let mask = libc::IN_MOVE_SELF
            | libc::IN_DELETE_SELF
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO
            | libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_UNMOUNT
            | libc::IN_ONLYDIR;
        let mut watch_ids = [0; 3];
        for (i, f) in [&root, &ledger, &state].into_iter().enumerate() {
            boundary(work, io)?;
            let (path, len) = fd_path(f.as_raw_fd())?;
            let c = CStr::from_bytes_with_nul(&path[..len]).map_err(|_| "watch-unavailable")?;
            // SAFETY: the watch descriptor and NUL-terminated path remain live for the call.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            let wd = unsafe { libc::inotify_add_watch(watches.as_raw_fd(), c.as_ptr(), mask) };
            if wd < 0 {
                return Err("watch-unavailable");
            }
            watch_ids[i] = wd;
        }
        if watch_ids[0] == watch_ids[1]
            || watch_ids[0] == watch_ids[2]
            || watch_ids[1] == watch_ids[2]
        {
            return Err("watch-unavailable");
        }
        verify_watch_bindings(&proc_fd, &watches, &watch_ids, &dirs, work, io)?;
        let main_path = path_file(&state, "ledger.db", work, io)?.ok_or("missing")?;
        let main = regular(&main_path, root_id.dev, work, io)?;
        let main_read = readable_file(&state, "ledger.db", work, io)?;
        if regular(&main_read, root_id.dev, work, io)? != main {
            return Err("identity");
        }
        let mut this = Self {
            root,
            ledger,
            state,
            main_path,
            main_read,
            proc_fd,
            watches,
            watch_ids,
            dirs,
            main,
            companions: std::array::from_fn(|_| None),
            companion_ids: [None; 3],
            latched: false,
            _scratch: scratch,
        };
        this.initialize_companions(work, io)?;
        this.check_native(work, io, false)?;
        Ok(this)
    }
    fn drain(&self, work: &mut Work<'_>, io: &ReadBudget<'_>) -> Result<(), &'static str> {
        let mut bytes = [0u8; BUFFER];
        for _ in 0..MAX_READS {
            io.checkpoint().map_err(|_| "deadline")?;
            work.charge(BUFFER * 4)?;
            // SAFETY: the nonblocking descriptor and fixed writable buffer remain valid throughout read.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            let count = unsafe {
                libc::read(
                    self.watches.as_raw_fd(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if count < 0 {
                return if io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock {
                    Ok(())
                } else {
                    Err("watch-unavailable")
                };
            }
            if count == 0 {
                return Err("watch-unavailable");
            }
            let mut at = 0usize;
            let count = count as usize;
            while at < count {
                if count - at < 16 {
                    return Err("watch-framing");
                }
                let wd = i32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap());
                let mask = u32::from_ne_bytes(bytes[at + 4..at + 8].try_into().unwrap());
                let len = u32::from_ne_bytes(bytes[at + 12..at + 16].try_into().unwrap()) as usize;
                let size = 16usize.checked_add(len).ok_or("watch-framing")?;
                if size > count - at || !len.is_multiple_of(4) {
                    return Err("watch-framing");
                }
                if mask
                    & (libc::IN_Q_OVERFLOW
                        | libc::IN_IGNORED
                        | libc::IN_UNMOUNT
                        | libc::IN_MOVE_SELF
                        | libc::IN_DELETE_SELF)
                    != 0
                {
                    return Err("namespace-changed");
                }
                let valid = libc::IN_MOVED_FROM
                    | libc::IN_MOVED_TO
                    | libc::IN_CREATE
                    | libc::IN_DELETE
                    | libc::IN_ISDIR;
                if mask & !valid != 0 {
                    return Err("watch-framing");
                }
                let i = self
                    .watch_ids
                    .iter()
                    .position(|v| *v == wd)
                    .ok_or("watch-framing")?;
                if len == 0 {
                    return Err("watch-framing");
                }
                let name = &bytes[at + 16..at + size];
                let end = name.iter().position(|x| *x == 0).ok_or("watch-framing")?;
                if name[end..].iter().any(|x| *x != 0) || name[..end].contains(&b'/') {
                    return Err("watch-framing");
                }
                if &name[..end] == EXPECTED[i]
                    || (i == 2
                        && COMPANIONS
                            .iter()
                            .any(|expected| name[..end] == expected.as_bytes()[..]))
                {
                    return Err("namespace-changed");
                }
                at += size;
            }
        }
        Err("watch-cap")
    }
    // Admit only an already-established WAL pair; a later namespace transition
    // is never treated as the first trusted appearance of a companion.
    fn initialize_companions(
        &mut self,
        work: &mut Work<'_>,
        io: &ReadBudget<'_>,
    ) -> Result<(), &'static str> {
        for (i, name) in COMPANIONS.iter().enumerate() {
            let current = path_file(&self.state, name, work, io)?;
            let observed = match current.as_ref() {
                Some(f) => Some(regular(f, self.main.dev, work, io)?),
                None => None,
            };
            if (i < 2 && observed.is_none()) || (i == 2 && observed.is_some()) {
                return Err("companion-mode-unavailable");
            }
            self.companion_ids[i] = observed;
            self.companions[i] = current;
        }
        Ok(())
    }
    fn sample_companions(
        &self,
        work: &mut Work<'_>,
        io: &ReadBudget<'_>,
    ) -> Result<(), &'static str> {
        for (i, name) in COMPANIONS.iter().enumerate() {
            let current = path_file(&self.state, name, work, io)?;
            let observed = match current.as_ref() {
                Some(f) => Some(regular(f, self.main.dev, work, io)?),
                None => None,
            };
            if observed != self.companion_ids[i] {
                return Err("companion-changed");
            }
        }
        Ok(())
    }
    fn inner(
        &mut self,
        work: &mut Work<'_>,
        io: &ReadBudget<'_>,
        _in_transaction: bool,
    ) -> Result<(), &'static str> {
        self.drain(work, io)?;
        verify_watch_bindings(
            &self.proc_fd,
            &self.watches,
            &self.watch_ids,
            &self.dirs,
            work,
            io,
        )?;
        let ledger = dir(&self.root, ".ledgerful", work, io)?;
        let state = dir(&ledger, "state", work, io)?;
        boundary(work, io)?;
        if [
            id(&self.root, work, io)?,
            id(&ledger, work, io)?,
            id(&state, work, io)?,
        ] != self.dirs
            || id(&self.ledger, work, io)? != self.dirs[1]
            || id(&self.state, work, io)? != self.dirs[2]
        {
            return Err("scope-changed");
        }
        if regular(&self.main_path, self.main.dev, work, io)? != self.main
            || regular(&self.main_read, self.main.dev, work, io)? != self.main
        {
            return Err("identity");
        }
        let fresh = path_file(&self.state, "ledger.db", work, io)?.ok_or("missing")?;
        if regular(&fresh, self.main.dev, work, io)? != self.main {
            return Err("identity");
        }
        self.sample_companions(work, io)?;
        self.drain(work, io)?;
        Ok(())
    }
    fn check_native(
        &mut self,
        work: &mut Work<'_>,
        io: &ReadBudget<'_>,
        in_transaction: bool,
    ) -> Result<(), &'static str> {
        if self.latched {
            return Err("native-latched");
        }
        let result = self.inner(work, io, in_transaction);
        if result.is_err() {
            self.latched = true
        }
        result
    }
    pub(super) fn main_identity(&self) -> super::super::contained::Identity {
        super::super::contained::Identity::from_linux_file_id(self.main.dev, self.main.ino)
    }
    /// Latches every failure; callers must keep this owner through commit/rollback.
    pub(super) fn check(
        &mut self,
        connection: &Connection,
        work: &mut Work<'_>,
        io: &ReadBudget<'_>,
        in_transaction: bool,
    ) -> Result<(), &'static str> {
        if self.latched {
            return Err("native-latched");
        }
        let result = (|| {
            self.check_native(work, io, in_transaction)?;
            boundary(work, io)?;
            let mut moved = -1;
            connection
                .file_control(None::<&str>, FileControl::HasMoved(&mut moved))
                .map_err(|_| "sqlite-moved-unavailable")?;
            if moved != 0 {
                return Err("sqlite-moved");
            }
            self.drain(work, io)?;
            Ok(())
        })();
        if result.is_err() {
            self.latched = true
        }
        result
    }
}

pub(super) struct LinuxReadScope<'a> {
    root: File,
    ledger: File,
    state: File,
    proc_fd: File,
    watches: File,
    watch_ids: [i32; 3],
    dirs: [Id; 3],
    _scratch: Reservation<'a>,
}
impl<'a> LinuxReadScope<'a> {
    pub(super) fn arm(
        root: &File,
        proc_fd: &File,
        work: &mut Work<'a>,
        io: &ReadBudget<'_>,
    ) -> Result<Self, &'static str> {
        boundary(work, io)?;
        if work.read_limit(BUFFER, 0)? != BUFFER {
            return Err("memory-cap");
        }
        let scratch = work
            .memory_budget()
            .reserve(BUFFER)
            .map_err(|_| "memory-cap")?;
        boundary(work, io)?;
        let root = root.try_clone().map_err(|_| "scope-unavailable")?;
        boundary(work, io)?;
        let proc_fd = proc_fd.try_clone().map_err(|_| "scope-unavailable")?;
        boundary(work, io)?;
        let root_id = id(&root, work, io)?;
        let ledger = dir(&root, ".ledgerful", work, io)?;
        let state = dir(&ledger, "state", work, io)?;
        boundary(work, io)?;
        let dirs = [root_id, id(&ledger, work, io)?, id(&state, work, io)?];
        if dirs.iter().any(|x| x.dev != root_id.dev) {
            return Err("identity");
        }
        proc_anchor(&proc_fd, work, io)?;
        boundary(work, io)?;
        // SAFETY: flags are valid and returned descriptor is checked before ownership transfer.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err("watch-unavailable");
        }
        // SAFETY: fd is a newly created live descriptor with unique ownership.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let watches = unsafe { File::from_raw_fd(fd) };
        let mask = libc::IN_MOVE_SELF
            | libc::IN_DELETE_SELF
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO
            | libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_UNMOUNT
            | libc::IN_ONLYDIR;
        let mut watch_ids = [0; 3];
        for (i, f) in [&root, &ledger, &state].into_iter().enumerate() {
            boundary(work, io)?;
            let (path, len) = fd_path(f.as_raw_fd())?;
            let c = CStr::from_bytes_with_nul(&path[..len]).map_err(|_| "watch-unavailable")?;
            // SAFETY: the watch descriptor and NUL-terminated path remain live for the call.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            let wd = unsafe { libc::inotify_add_watch(watches.as_raw_fd(), c.as_ptr(), mask) };
            if wd < 0 {
                return Err("watch-unavailable");
            }
            watch_ids[i] = wd;
        }
        if watch_ids[0] == watch_ids[1]
            || watch_ids[0] == watch_ids[2]
            || watch_ids[1] == watch_ids[2]
        {
            return Err("watch-unavailable");
        }
        verify_watch_bindings(&proc_fd, &watches, &watch_ids, &dirs, work, io)?;
        let scope = Self {
            root,
            ledger,
            state,
            proc_fd,
            watches,
            watch_ids,
            dirs,
            _scratch: scratch,
        };
        scope.check(work, io)?;
        Ok(scope)
    }
    pub(super) fn ledger_identity(&self) -> super::super::contained::Identity {
        super::super::contained::Identity::from_linux_file_id(self.dirs[1].dev, self.dirs[1].ino)
    }
    pub(super) fn drain(
        &self,
        work: &mut Work<'_>,
        io: &ReadBudget<'_>,
    ) -> Result<(), &'static str> {
        self.drain_events(None, work, io)
    }
    pub(super) fn take_creation(
        &self,
        expected: &str,
        work: &mut Work<'_>,
        io: &ReadBudget<'_>,
    ) -> Result<(), &'static str> {
        self.drain_events(Some(expected.as_bytes()), work, io)
    }
    fn drain_events(
        &self,
        expected: Option<&[u8]>,
        work: &mut Work<'_>,
        io: &ReadBudget<'_>,
    ) -> Result<(), &'static str> {
        let mut observed = false;
        let mut bytes = [0u8; BUFFER];
        for _ in 0..MAX_READS {
            io.checkpoint().map_err(|_| "deadline")?;
            work.charge(BUFFER * 4)?;
            // SAFETY: the nonblocking descriptor and fixed writable buffer remain valid throughout read.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            let count = unsafe {
                libc::read(
                    self.watches.as_raw_fd(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if count < 0 {
                return if io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock
                    && observed == expected.is_some()
                {
                    Ok(())
                } else {
                    Err("watch-unavailable")
                };
            }
            if count == 0 {
                return Err("watch-unavailable");
            }
            let mut at = 0usize;
            let count = count as usize;
            while at < count {
                if count - at < 16 {
                    return Err("watch-framing");
                }
                let wd = i32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap());
                let mask = u32::from_ne_bytes(bytes[at + 4..at + 8].try_into().unwrap());
                let len = u32::from_ne_bytes(bytes[at + 12..at + 16].try_into().unwrap()) as usize;
                let size = 16usize.checked_add(len).ok_or("watch-framing")?;
                if size > count - at || !len.is_multiple_of(4) {
                    return Err("watch-framing");
                }
                if mask
                    & (libc::IN_Q_OVERFLOW
                        | libc::IN_IGNORED
                        | libc::IN_UNMOUNT
                        | libc::IN_MOVE_SELF
                        | libc::IN_DELETE_SELF)
                    != 0
                {
                    return Err("namespace-changed");
                }
                let valid = libc::IN_MOVED_FROM
                    | libc::IN_MOVED_TO
                    | libc::IN_CREATE
                    | libc::IN_DELETE
                    | libc::IN_ISDIR;
                if mask & !valid != 0 {
                    return Err("watch-framing");
                }
                let i = self
                    .watch_ids
                    .iter()
                    .position(|v| *v == wd)
                    .ok_or("watch-framing")?;
                if len == 0 {
                    return Err("watch-framing");
                }
                let name = &bytes[at + 16..at + size];
                let end = name.iter().position(|x| *x == 0).ok_or("watch-framing")?;
                if name[end..].iter().any(|x| *x != 0) || name[..end].contains(&b'/') {
                    return Err("watch-framing");
                }
                if &name[..end] == EXPECTED[i]
                    || (i == 2
                        && COMPANIONS
                            .iter()
                            .any(|expected| name[..end] == expected.as_bytes()[..]))
                {
                    if !observed
                        && i == 2
                        && mask == libc::IN_CREATE
                        && expected == Some(&name[..end])
                    {
                        observed = true;
                    } else {
                        return Err("namespace-changed");
                    }
                }
                at += size;
            }
        }
        Err("watch-cap")
    }
    pub(super) fn state(&self) -> &File {
        &self.state
    }
    pub(super) fn proc_fd(&self) -> &File {
        &self.proc_fd
    }
    pub(super) fn check(
        &self,
        work: &mut Work<'_>,
        io: &ReadBudget<'_>,
    ) -> Result<(), &'static str> {
        self.drain(work, io)?;
        verify_watch_bindings(
            &self.proc_fd,
            &self.watches,
            &self.watch_ids,
            &self.dirs,
            work,
            io,
        )?;
        let ledger = dir(&self.root, ".ledgerful", work, io)?;
        let state = dir(&ledger, "state", work, io)?;
        if [
            id(&self.root, work, io)?,
            id(&ledger, work, io)?,
            id(&state, work, io)?,
        ] != self.dirs
            || id(&self.ledger, work, io)? != self.dirs[1]
            || id(&self.state, work, io)? != self.dirs[2]
        {
            return Err("scope-changed");
        }
        self.drain(work, io)
    }
}
