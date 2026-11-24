//! Private owner for one bounded read of an existing Linux SQLite WAL store.
use super::{
    isolated_fd::{
        GuardError, MAX_ANCHOR, STACK, close_private, decimal, guard_work_error, task_fd_path,
    },
    linux_capture::LinuxReadScope,
};
use crate::change_evidence::{
    contained::{ReadBudget, ReadError},
    pack::Work,
};
use nix::{
    errno::Errno,
    fcntl::{FcntlArg, OFlag, OpenHow, ResolveFlag, fcntl, openat, openat2},
    libc,
    sys::{
        stat::Mode,
        statfs::{EXT4_SUPER_MAGIC, PROC_SUPER_MAGIC, fstatfs},
    },
    unistd::gettid,
};
use std::{
    cell::Cell,
    ffi::OsStr,
    fs::{File, Metadata},
    os::{
        fd::{AsRawFd, BorrowedFd},
        unix::{ffi::OsStrExt, fs::FileExt, fs::MetadataExt},
    },
    sync::{
        Mutex,
        atomic::{AtomicU8, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};

const MAX_READ: usize = 65_536 + 24;
const SHM_REGION: u64 = 32_768;
const MAX_SHM: u64 = 16 * 1024 * 1024;
const BUSY_LIMIT: Duration = Duration::from_millis(100);
const BUSY_POLL: Duration = Duration::from_millis(10);
// These offsets match the pinned SQLite Unix VFS.
const PENDING: libc::off_t = 0x4000_0000;
const SHARED_FIRST: libc::off_t = PENDING + 2;
const SHARED_SIZE: libc::off_t = 510;
const SHM_BASE: libc::off_t = 120;
const SHM_DMS: libc::off_t = 128;
const AVAILABLE: u8 = 0;
const ACTIVE: u8 = 1;
const POISONED: u8 = 2;
static ADMISSION: AtomicU8 = AtomicU8::new(AVAILABLE);

#[derive(Clone, Copy)]
pub(super) enum InputRole {
    Main,
    Wal,
    Shm,
}

const ROLES: [InputRole; 3] = [InputRole::Main, InputRole::Wal, InputRole::Shm];

impl InputRole {
    fn name(self) -> &'static str {
        match self {
            Self::Main => "ledger.db",
            Self::Wal => "ledger.db-wal",
            Self::Shm => "ledger.db-shm",
        }
    }

    fn accepts_len(self, len: u64) -> bool {
        match self {
            Self::Main => len >= 512,
            Self::Wal => len == 0 || len >= 32,
            Self::Shm => (SHM_REGION..=MAX_SHM).contains(&len) && len.is_multiple_of(SHM_REGION),
        }
    }
}

struct Gate {
    clean: bool,
}

impl Gate {
    fn enter() -> Result<Self, GuardError> {
        match ADMISSION.compare_exchange(AVAILABLE, ACTIVE, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => Ok(Self { clean: true }),
            Err(ACTIVE) => Err(GuardError::Busy),
            Err(_) => Err(GuardError::Unavailable),
        }
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        let state = if self.clean { AVAILABLE } else { POISONED };
        ADMISSION.store(state, Ordering::Release);
    }
}

enum Operation {
    Read {
        role: InputRole,
        offset: u64,
        len: usize,
    },
    Check,
}

enum Command {
    Run(Operation),
    Release,
}

enum Reply {
    Ready([u64; 3]),
    Read,
    Stable,
}

type Outcome = Result<Reply, GuardError>;
type ReadBuffer = Mutex<[u8; MAX_READ]>;

pub(super) struct IsolatedReadInputs<'a, 'budget> {
    tx: SyncSender<Command>,
    rx: Receiver<Outcome>,
    io: &'a ReadBudget<'budget>,
    buffer: &'a ReadBuffer,
    lengths: [u64; 3],
    failure: Option<GuardError>,
}

impl IsolatedReadInputs<'_, '_> {
    pub(super) fn len(&self, role: InputRole) -> u64 {
        self.lengths[role as usize]
    }

    pub(super) fn read_exact_at(
        &mut self,
        role: InputRole,
        offset: u64,
        out: &mut [u8],
        work: &mut Work<'_>,
    ) -> Result<(), GuardError> {
        let result = (|| {
            self.prior_result()?;
            if out.len() > MAX_READ {
                return Err(GuardError::Budget);
            }
            check_range(offset, out.len(), self.len(role))?;
            work.charge(out.len() + 1).map_err(guard_work_error)?;
            self.io.charge_native_read(out.len()).map_err(read_error)?;
            self.send(Command::Run(Operation::Read {
                role,
                offset,
                len: out.len(),
            }))?;
            let Reply::Read = self.receive()? else {
                return Err(GuardError::Unavailable);
            };
            let buffer = self.buffer.lock().map_err(|_| GuardError::Unavailable)?;
            out.copy_from_slice(&buffer[..out.len()]);
            Ok(())
        })();
        self.latch(result)
    }

    pub(super) fn check_stable(&mut self, work: &mut Work<'_>) -> Result<(), GuardError> {
        let result = (|| {
            self.prior_result()?;
            work.charge(8).map_err(guard_work_error)?;
            self.send(Command::Run(Operation::Check))?;
            match self.receive()? {
                Reply::Stable => Ok(()),
                _ => Err(GuardError::Unavailable),
            }
        })();
        self.latch(result)
    }

    fn prior_result(&self) -> Result<(), GuardError> {
        self.failure.map_or(Ok(()), Err)
    }

    fn latch(&mut self, result: Result<(), GuardError>) -> Result<(), GuardError> {
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }

    fn send(&self, command: Command) -> Result<(), GuardError> {
        checkpoint(self.io)?;
        self.tx
            .try_send(command)
            .map_err(|_| GuardError::Unavailable)
    }

    fn receive(&self) -> Outcome {
        loop {
            checkpoint(self.io)?;
            let left = self.io.deadline().saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left.min(Duration::from_millis(50))) {
                Ok(result) => {
                    checkpoint(self.io)?;
                    return result;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(GuardError::Unavailable),
            }
        }
    }
}

/// The callback uses the admitted roles while both read locks are held. The
/// result is returned only after the final scope check and confirmed close.
pub(super) fn with_isolated_readonly_inputs<'memory, T>(
    scope: &LinuxReadScope<'_>,
    io: &ReadBudget<'memory>,
    work: &mut Work<'memory>,
    f: impl FnOnce(&mut Work<'memory>, &mut IsolatedReadInputs<'_, 'memory>) -> Result<T, GuardError>,
) -> Result<T, GuardError> {
    let mut gate = Gate::enter()?;
    let bytes = STACK + std::mem::size_of::<ReadBuffer>() + 65_536;
    if work.read_limit(bytes, 0).map_err(guard_work_error)? != bytes {
        return Err(GuardError::Budget);
    }
    work.charge(8192).map_err(guard_work_error)?;
    let _memory = work
        .memory_budget()
        .reserve(bytes)
        .map_err(|_| GuardError::Budget)?;
    scope.check(work, io).map_err(scope_error)?;
    let state = scope.state();
    let proc_fd = scope.proc_fd();
    let device = state.metadata().map_err(|_| GuardError::Unavailable)?.dev();
    // Linux reports this filesystem magic for ext2, ext3, and ext4.
    if !fstatfs(state).is_ok_and(|fs| fs.filesystem_type() == EXT4_SUPER_MAGIC) {
        return Err(GuardError::Unavailable);
    }
    let Some(main) = pin_role(state, InputRole::Main, work, io)? else {
        scope.check(work, io).map_err(scope_error)?;
        checkpoint(io)?;
        return Err(GuardError::Uninitialized);
    };
    let pins = [
        main,
        pin_role(state, InputRole::Wal, work, io)?.ok_or(GuardError::Unavailable)?,
        pin_role(state, InputRole::Shm, work, io)?.ok_or(GuardError::Unavailable)?,
    ];
    let expected = [
        normal_metadata(&pins[0], InputRole::Main, device)?,
        normal_metadata(&pins[1], InputRole::Wal, device)?,
        normal_metadata(&pins[2], InputRole::Shm, device)?,
    ];
    if expected[0].ino == expected[1].ino
        || expected[0].ino == expected[2].ino
        || expected[1].ino == expected[2].ino
    {
        return Err(GuardError::Unavailable);
    }
    verify_names(scope, &expected, device, work, io)?;
    let raw = pins.each_ref().map(AsRawFd::as_raw_fd);
    let proc_raw = proc_fd.as_raw_fd();
    let mut keep = [raw[0], raw[1], raw[2], proc_raw];
    keep.sort_unstable();
    if !(0..=MAX_ANCHOR).contains(&keep[0])
        || !(0..=MAX_ANCHOR).contains(&keep[3])
        || keep.windows(2).any(|pair| pair[0] == pair[1])
    {
        return Err(GuardError::Unavailable);
    }
    let buffer = &Mutex::new([0; MAX_READ]);
    std::thread::scope(|threads| {
        let (tx, commands) = mpsc::sync_channel(1);
        let (replies, rx) = mpsc::sync_channel(1);
        let helper = std::thread::Builder::new()
            .stack_size(STACK)
            .spawn_scoped(threads, move || {
                Helper {
                    pins: raw,
                    proc_fd: proc_raw,
                    expected,
                    io,
                    buffer,
                    commands,
                    replies,
                }
                .run(keep)
            })
            .map_err(|_| GuardError::Unavailable)?;
        let mut inputs = IsolatedReadInputs {
            tx,
            rx,
            io,
            buffer,
            lengths: [0; 3],
            failure: None,
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let Reply::Ready(lengths) = inputs.receive()? else {
                return Err(GuardError::Unavailable);
            };
            inputs.lengths = lengths;
            verify_names(scope, &expected, device, work, io)?;
            let value = f(work, &mut inputs)?;
            verify_names(scope, &expected, device, work, io)?;
            inputs.check_stable(work)?;
            Ok(value)
        }));
        let _ = inputs.tx.try_send(Command::Release);
        drop(inputs);
        gate.clean = helper.join().unwrap_or(false);
        match result {
            Ok(Ok(value)) if gate.clean => {
                checkpoint(io)?;
                Ok(value)
            }
            Ok(Err(error)) if gate.clean => Err(error),
            Ok(_) => Err(GuardError::Unavailable),
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Stamp {
    fn new(meta: &Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mode: meta.mode(),
            uid: meta.uid(),
            gid: meta.gid(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        }
    }
}

fn checkpoint(io: &ReadBudget<'_>) -> Result<(), GuardError> {
    io.checkpoint().map_err(|_| GuardError::Deadline)
}

fn read_error(error: ReadError) -> GuardError {
    match error {
        ReadError::Deadline | ReadError::Interrupted => GuardError::Deadline,
        _ => GuardError::Budget,
    }
}

fn scope_error(error: &str) -> GuardError {
    match error {
        "deadline" => GuardError::Deadline,
        "work-cap" | "memory-cap" | "pack-read-cap" => GuardError::Budget,
        _ => GuardError::Unavailable,
    }
}

fn check_range(offset: u64, len: usize, file_len: u64) -> Result<(), GuardError> {
    if offset
        .checked_add(len as u64)
        .is_some_and(|end| end <= file_len)
    {
        Ok(())
    } else {
        Err(GuardError::Unavailable)
    }
}

fn pin_name(state: &File, name: &str) -> nix::Result<File> {
    let how = OpenHow::new()
        .flags(OFlag::O_PATH | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC)
        .resolve(
            ResolveFlag::RESOLVE_BENEATH
                | ResolveFlag::RESOLVE_NO_SYMLINKS
                | ResolveFlag::RESOLVE_NO_XDEV,
        );
    openat2(state, name, how).map(File::from)
}

fn pin_role(
    state: &File,
    role: InputRole,
    work: &mut Work<'_>,
    io: &ReadBudget<'_>,
) -> Result<Option<File>, GuardError> {
    checkpoint(io)?;
    work.charge(2).map_err(guard_work_error)?;
    match pin_name(state, role.name()) {
        Ok(pin) => Ok(Some(pin)),
        Err(Errno::ENOENT) => Ok(None),
        Err(_) => Err(GuardError::Unavailable),
    }
}

fn normal_metadata(file: &File, role: InputRole, device: u64) -> Result<Stamp, GuardError> {
    let meta = file.metadata().map_err(|_| GuardError::Unavailable)?;
    if !meta.is_file() || meta.nlink() != 1 || meta.dev() != device || !role.accepts_len(meta.len())
    {
        return Err(GuardError::Unavailable);
    }
    Ok(Stamp::new(&meta))
}

fn verify_names(
    scope: &LinuxReadScope<'_>,
    expected: &[Stamp; 3],
    device: u64,
    work: &mut Work<'_>,
    io: &ReadBudget<'_>,
) -> Result<(), GuardError> {
    scope.check(work, io).map_err(scope_error)?;
    let state = scope.state();
    for role in ROLES {
        let pin = pin_role(state, role, work, io)?.ok_or(GuardError::Unavailable)?;
        if normal_metadata(&pin, role, device)? != expected[role as usize] {
            return Err(GuardError::Unavailable);
        }
    }
    checkpoint(io)?;
    work.charge(1).map_err(guard_work_error)?;
    if !matches!(pin_name(state, "ledger.db-journal"), Err(Errno::ENOENT)) {
        return Err(GuardError::Unavailable);
    }
    scope.drain(work, io).map_err(scope_error)
}

// After isolation every owned descriptor is closed here, including on an
// early error. Any unconfirmed close poisons future admission.
struct PrivateFile<'a> {
    file: Option<File>,
    clean: &'a Cell<bool>,
}

impl<'a> PrivateFile<'a> {
    fn new(file: File, clean: &'a Cell<bool>) -> Self {
        Self {
            file: Some(file),
            clean,
        }
    }

    fn file(&self) -> &File {
        self.file.as_ref().expect("private file is live until drop")
    }
}

impl Drop for PrivateFile<'_> {
    fn drop(&mut self) {
        if let Some(file) = self.file.take()
            && !close_private(file)
        {
            self.clean.set(false);
        }
    }
}

fn close_range(first: u32, last: u32, flags: u32) -> bool {
    // SAFETY: caller either uses CLOSE_RANGE_UNSHARE, or has already
    // isolated this thread's file table and owns the closed range.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { libc::syscall(libc::SYS_close_range, first, last, flags) == 0 }
}

struct Helper<'a, 'budget> {
    pins: [i32; 3],
    proc_fd: i32,
    expected: [Stamp; 3],
    io: &'a ReadBudget<'budget>,
    buffer: &'a ReadBuffer,
    commands: Receiver<Command>,
    replies: SyncSender<Outcome>,
}

impl Helper<'_, '_> {
    fn run(self, keep: [i32; 4]) -> bool {
        let keep = keep.map(|fd| fd as u32);
        // CLOSE_RANGE_UNSHARE switches just this thread to a private table.
        if !close_range(keep[3] + 1, u32::MAX, libc::CLOSE_RANGE_UNSHARE) {
            return false;
        }
        let mut next = 0;
        for fd in keep {
            if next < fd && !close_range(next, fd - 1, 0) {
                return close_range(0, u32::MAX, 0);
            }
            next = fd + 1;
        }
        let clean = Cell::new(true);
        let result = self.serve(&clean);
        let closed = close_range(0, u32::MAX, 0);
        if let Err(error) = result {
            let _ = self.replies.try_send(Err(error));
        }
        clean.get() && closed
    }

    fn admit<'a>(&self, clean: &'a Cell<bool>) -> Result<[PrivateFile<'a>; 3], GuardError> {
        let until = (Instant::now() + BUSY_LIMIT).min(self.io.deadline());
        let mut result = self.try_admit(clean);
        while matches!(result, Err(GuardError::Busy)) && clean.get() {
            checkpoint(self.io)?;
            let delay = BUSY_POLL.min(until.saturating_duration_since(Instant::now()));
            if delay.is_zero() {
                break;
            }
            // A failed attempt drops every private input before waiting.
            std::thread::sleep(delay);
            checkpoint(self.io)?;
            if Instant::now() >= until {
                break;
            }
            result = self.try_admit(clean);
        }
        result
    }

    fn try_admit<'a>(&self, clean: &'a Cell<bool>) -> Result<[PrivateFile<'a>; 3], GuardError> {
        checkpoint(self.io)?;
        let files = {
            let task = proc_task_fds(self.proc_fd, clean)?;
            [
                reopen_pin(task.file(), self.pins[0], clean)?,
                reopen_pin(task.file(), self.pins[1], clean)?,
                reopen_pin(task.file(), self.pins[2], clean)?,
            ]
        };
        if !clean.get() {
            return Err(GuardError::Unavailable);
        }
        stable(&files, &self.expected)?;
        checkpoint(self.io)?;
        let [main, _, shm] = &files;
        let read_lock = libc::F_RDLCK as libc::c_short;
        set_lock(main.file(), read_lock, PENDING, 1)?;
        set_lock(main.file(), read_lock, SHARED_FIRST, SHARED_SIZE)?;
        set_lock(main.file(), libc::F_UNLCK as libc::c_short, PENDING, 1)?;
        set_lock(shm.file(), read_lock, SHM_BASE, SHM_DMS - SHM_BASE + 1)?;
        stable(&files, &self.expected)?;
        checkpoint(self.io)?;
        Ok(files)
    }

    fn serve(&self, clean: &Cell<bool>) -> Result<(), GuardError> {
        let files = self.admit(clean)?;
        let lengths = self.expected.map(|stamp| stamp.len);
        if self.replies.try_send(Ok(Reply::Ready(lengths))).is_err() {
            return Ok(());
        }
        // After Ready, keep the files and fences through terminal disposition,
        // even if servicing a command or reporting its result fails.
        let mut failure = None;
        while let Ok(Command::Run(operation)) = self.commands.recv() {
            let outcome = match failure {
                Some(error) => Err(error),
                None => self.execute(&files, operation),
            };
            if let Err(error) = outcome {
                failure = Some(error);
            }
            if self.replies.try_send(outcome).is_err() {
                failure.get_or_insert(GuardError::Unavailable);
            }
        }
        Ok(())
    }

    fn execute(&self, files: &[PrivateFile<'_>; 3], operation: Operation) -> Outcome {
        checkpoint(self.io)?;
        match operation {
            Operation::Read { role, offset, len } => {
                let i = role as usize;
                check_range(offset, len, self.expected[i].len)?;
                let mut bytes = self.buffer.lock().map_err(|_| GuardError::Unavailable)?;
                let out = bytes.get_mut(..len).ok_or(GuardError::Budget)?;
                read_exact_at(files[i].file(), offset, out, self.io)?;
                Ok(Reply::Read)
            }
            Operation::Check => {
                stable(files, &self.expected)?;
                Ok(Reply::Stable)
            }
        }
    }
}

fn proc_task_fds(proc_fd: i32, clean: &Cell<bool>) -> Result<PrivateFile<'_>, GuardError> {
    // SAFETY: the parent retains this authenticated procfs fd through the join
    // and this thread has kept the private-table entry at the same number.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let anchor = unsafe { BorrowedFd::borrow_raw(proc_fd) };
    let flags = OFlag::O_PATH | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let no_cross = ResolveFlag::RESOLVE_NO_XDEV | ResolveFlag::RESOLVE_NO_SYMLINKS;
    let parent = openat2(anchor, "..", OpenHow::new().flags(flags).resolve(no_cross))
        .map(|fd| PrivateFile::new(File::from(fd), clean))
        .map_err(|_| GuardError::Unavailable)?;
    if !fstatfs(parent.file()).is_ok_and(|fs| fs.filesystem_type() == PROC_SUPER_MAGIC) {
        return Err(GuardError::Unavailable);
    }
    let mut path = [0; 64];
    let len = task_fd_path(gettid().as_raw() as u64, &mut path).ok_or(GuardError::Unavailable)?;
    openat2(
        parent.file(),
        OsStr::from_bytes(&path[..len]),
        OpenHow::new()
            .flags(flags)
            .resolve(no_cross | ResolveFlag::RESOLVE_BENEATH),
    )
    .map(|fd| PrivateFile::new(File::from(fd), clean))
    .map_err(|_| GuardError::Unavailable)
}

fn reopen_pin<'a>(
    task: &File,
    pin: i32,
    clean: &'a Cell<bool>,
) -> Result<PrivateFile<'a>, GuardError> {
    let mut digits = [0; 20];
    let name = decimal(pin as u64, &mut digits).ok_or(GuardError::Unavailable)?;
    // The only followed magic link is the retained pin in this private table.
    openat(
        task,
        OsStr::from_bytes(name),
        OFlag::O_RDONLY | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC | OFlag::O_NOATIME,
        Mode::empty(),
    )
    .map(|fd| PrivateFile::new(File::from(fd), clean))
    .map_err(|_| GuardError::Unavailable)
}

fn lock_range(kind: libc::c_short, start: libc::off_t, len: libc::off_t) -> libc::flock {
    libc::flock {
        l_type: kind,
        l_whence: libc::SEEK_SET as libc::c_short,
        l_start: start,
        l_len: len,
        l_pid: 0,
    }
}

fn set_lock(
    file: &File,
    kind: libc::c_short,
    start: libc::off_t,
    len: libc::off_t,
) -> Result<(), GuardError> {
    let lock = lock_range(kind, start, len);
    fcntl(file, FcntlArg::F_OFD_SETLK(&lock))
        .map(|_| ())
        .map_err(|error| match error {
            Errno::EACCES | Errno::EAGAIN if kind == libc::F_RDLCK as libc::c_short => {
                GuardError::Busy
            }
            _ => GuardError::Unavailable,
        })
}

fn require_stock_owner(shm: &File) -> Result<(), GuardError> {
    let mut conflict = lock_range(libc::F_WRLCK as libc::c_short, SHM_DMS, 1);
    fcntl(shm, FcntlArg::F_OFD_GETLK(&mut conflict)).map_err(|_| GuardError::Unavailable)?;
    match i32::from(conflict.l_type) {
        libc::F_RDLCK if conflict.l_pid > 0 => Ok(()),
        libc::F_WRLCK => Err(GuardError::Busy),
        _ => Err(GuardError::Unavailable),
    }
}

fn stable(files: &[PrivateFile<'_>; 3], expected: &[Stamp; 3]) -> Result<(), GuardError> {
    for role in ROLES {
        let i = role as usize;
        if normal_metadata(files[i].file(), role, expected[i].dev)? != expected[i] {
            return Err(GuardError::Unavailable);
        }
    }
    require_stock_owner(files[InputRole::Shm as usize].file())
}

fn read_exact_at(
    file: &File,
    mut offset: u64,
    mut out: &mut [u8],
    io: &ReadBudget<'_>,
) -> Result<(), GuardError> {
    while !out.is_empty() {
        checkpoint(io)?;
        let count = file
            .read_at(out, offset)
            .map_err(|_| GuardError::Unavailable)?;
        if count == 0 {
            return Err(GuardError::Unavailable);
        }
        offset += count as u64;
        out = &mut out[count..];
    }
    checkpoint(io)
}
