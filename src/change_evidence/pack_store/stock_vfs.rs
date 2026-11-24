//! Scoped stock Unix VFS for explicitly authorized SQLite writers.
use nix::fcntl::{OFlag, OpenHow, ResolveFlag, openat2};
use rusqlite::ffi;
use std::{
    cell::Cell,
    ffi::{c_char, c_int, c_void},
    fs::File,
    io::Read,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::Path,
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
};

static ORIGINAL_OPEN: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_FCHMOD: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ORIGINAL_FSTAT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static INSTALL_TAINTED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn hooked_fchmod(fd: c_int, mode: nix::libc::mode_t) -> c_int {
    // SAFETY: The stored pointer has the stock fchmod signature. The original
    // arguments forward unchanged, and refusal updates only this thread's errno.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        let ptr = ORIGINAL_FCHMOD.load(Ordering::Acquire);
        if ptr.is_null() {
            *nix::libc::__errno_location() = nix::libc::EACCES;
            return -1;
        }
        let original: unsafe extern "C" fn(c_int, nix::libc::mode_t) -> c_int =
            std::mem::transmute(ptr);
        ACTIVE.with(|slot| {
            if slot.get().is_null() {
                original(fd, mode)
            } else {
                *nix::libc::__errno_location() = nix::libc::EACCES;
                -1
            }
        })
    }
}

unsafe extern "C" fn hooked_fstat(fd: c_int, out: *mut nix::libc::stat) -> c_int {
    // SAFETY: SQLite owns fd and provides the output storage for its call.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        let delegate = ORIGINAL_FSTAT.load(Ordering::Acquire);
        if delegate.is_null() {
            *nix::libc::__errno_location() = nix::libc::EBADF;
            return -1;
        }
        let original: unsafe extern "C" fn(c_int, *mut nix::libc::stat) -> c_int =
            std::mem::transmute(delegate);
        let result = original(fd, out);
        let errno = *nix::libc::__errno_location();
        ACTIVE.with(|slot| {
            let active = slot.get().cast::<Activation<'_, '_>>();
            if active.is_null() {
                return;
            }
            let Some(probe) = (*active).main_probe.as_mut() else {
                return;
            };
            if result != 0 || out.is_null() || !probe.observe(fd, &*out) {
                (*active).scope.latched = true;
            }
        });
        *nix::libc::__errno_location() = errno;
        result
    }
}

use super::linux_capture::LinuxReadScope;
use crate::change_evidence::{buffer::Reservation, contained::ReadBudget, pack::Work};
struct DirectoryWitness<'a>(LinuxReadScope<'a>);
impl<'a> DirectoryWitness<'a> {
    fn arm(root: &File, proc_fd: &File, io: &ReadBudget<'_>, work: &mut Work<'a>) -> Option<Self> {
        Some(Self(LinuxReadScope::arm(root, proc_fd, work, io).ok()?))
    }
    fn state(&self) -> &File {
        self.0.state()
    }
    fn proc_fd(&self) -> &File {
        self.0.proc_fd()
    }
    fn check(&self, work: &mut Work<'_>, io: &ReadBudget<'_>) -> bool {
        self.0.check(work, io).is_ok()
    }
    fn drain(&self, work: &mut Work<'_>, io: &ReadBudget<'_>) -> bool {
        self.0.drain(work, io).is_ok()
    }
}
#[path = "stock_vfs/writer.rs"]
pub(super) mod writer;
use writer::{NAMES, OpenRole, Policy};
const PATH_CAP: usize = 4096;
const MAPS_CAP: usize = 256 * 1024;
thread_local! {
    static ACTIVE: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
}

pub struct Scope<'m> {
    owner: DirectoryWitness<'m>,
    pins: [Option<File>; 3],
    policy: Policy,
    warm_epoch: Option<writer::WalEpoch>,
    writer_admission_spent: bool,
    paths: [[u8; PATH_CAP]; 4],
    lengths: [usize; 4],
    _memory: Reservation<'m>,
    latched: bool,
    main_count: usize,
    wal_count: usize,
    shm_count: usize,
}
struct MainFileProbe {
    identity: crate::change_evidence::contained::Identity,
    fd: Option<c_int>,
    calls: usize,
}

impl MainFileProbe {
    fn observe(&mut self, fd: c_int, stat: &nix::libc::stat) -> bool {
        // SAFETY: SQLite just called the original fstat for this live fd.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
        if self.calls >= 2
            || self.fd.is_some_and(|prior| prior != fd)
            || fd < 3
            || flags < 0
            || flags & nix::libc::O_ACCMODE != nix::libc::O_RDWR
            || crate::change_evidence::contained::Identity::from_linux_file_id(
                stat.st_dev,
                stat.st_ino,
            ) != self.identity
            || stat.st_nlink != 1
            || stat.st_mode & nix::libc::S_IFMT != nix::libc::S_IFREG
        {
            return false;
        }
        self.fd = Some(fd);
        self.calls += 1;
        true
    }
}

struct Activation<'a, 'm> {
    scope: &'a mut Scope<'m>,
    io: &'a ReadBudget<'m>,
    work: &'a mut Work<'m>,
    write_admitted: bool,
    role: OpenRole,
    main_probe: Option<MainFileProbe>,
}
impl Activation<'_, '_> {
    fn post(&mut self) -> bool {
        if !self.scope.owner.drain(self.work, self.io) {
            self.scope.latched = true;
            return false;
        }
        true
    }
}
impl<'m> Scope<'m> {
    pub(super) fn cold(
        root: &File,
        proc_fd: &File,
        state: &Path,
        io: &ReadBudget<'m>,
        work: &mut Work<'m>,
        expected_ledger: crate::change_evidence::contained::Identity,
    ) -> Option<Self> {
        // SAFETY: geteuid takes no pointers and returns the calling process identity.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        if unsafe { nix::libc::geteuid() } == 0 {
            return None;
        }
        let base = state.as_os_str().as_bytes();
        if base.len() > PATH_CAP - 32 {
            return None;
        }
        work.charge(base.len().checked_mul(4)?.checked_add(PATH_CAP * 4)?)
            .ok()?;
        io.checkpoint().ok()?;
        let owner = DirectoryWitness::arm(root, proc_fd, io, work)?;
        if expected_ledger != owner.0.ledger_identity() {
            return None;
        }
        let bytes = std::mem::size_of::<Self>()
            .checked_mul(2)?
            .checked_add(16384)?;
        if work.read_limit(bytes, 0).ok()? != bytes {
            return None;
        }
        let memory = work.memory_budget().reserve(bytes).ok()?;
        work.charge(8).ok()?;
        let pins = [
            writer::pin(owner.state(), NAMES[0]).ok()?,
            writer::pin(owner.state(), NAMES[1]).ok()?,
            writer::pin(owner.state(), NAMES[2]).ok()?,
        ];
        let has_wal = pins[1].is_some();
        if pins[0].is_none()
            || has_wal != pins[2].is_some()
            || writer::pin(owner.state(), NAMES[3]).ok()?.is_some()
        {
            return None;
        }
        let (policy, warm_epoch) = if has_wal {
            let wal = writer::WalEpoch::from_pin(pins[1].as_ref()?)?;
            if !writer::qualified_shm_shape(pins[2].as_ref()?) {
                return None;
            }
            let policy = if wal.is_checkpointed() {
                Policy::ColdPreflight
            } else {
                Policy::WarmPreflight
            };
            (policy, Some(wal))
        } else {
            (Policy::ColdPreflight, None)
        };
        let names = NAMES;
        let mut paths = [[0u8; PATH_CAP]; 4];
        let mut lengths = [0usize; 4];
        for i in 0..4 {
            paths[i][..base.len()].copy_from_slice(base);
            let mut at = base.len();
            if !base.ends_with(b"/") {
                paths[i][at] = b'/';
                at += 1
            }
            paths[i][at..at + names[i].len()].copy_from_slice(names[i].as_bytes());
            lengths[i] = at + names[i].len();
        }
        let mut s = Self {
            owner,
            pins,
            policy,
            warm_epoch,
            writer_admission_spent: false,
            paths,
            lengths,
            _memory: memory,
            latched: false,
            main_count: 0,
            wal_count: 0,
            shm_count: 0,
        };
        if !s.check(io, work) {
            return None;
        }
        Some(s)
    }
    pub(super) fn main_identity(&self) -> Option<crate::change_evidence::contained::Identity> {
        self.pins[0].as_ref().and_then(writer::identity)
    }
    fn check(&mut self, io: &ReadBudget<'m>, work: &mut Work<'m>) -> bool {
        if self.latched {
            return false;
        }
        if io.checkpoint().is_err() || !self.owner.check(work, io) {
            self.latched = true;
            return false;
        }
        for (i, name) in NAMES.iter().enumerate() {
            if work.charge(3).is_err() {
                self.latched = true;
                return false;
            }
            let Ok(path) = writer::pin(self.owner.state(), name) else {
                self.latched = true;
                return false;
            };
            let pinned = self.pins.get(i).and_then(Option::as_ref);
            let current = match (path.as_ref(), pinned) {
                (Some(path), Some(pinned)) => writer::identity(path)
                    .zip(writer::identity(pinned))
                    .is_some_and(|(path, pinned)| path == pinned),
                (None, None) => true,
                _ => false,
            };
            if !current {
                self.latched = true;
                return false;
            }
        }
        if self.warm_epoch.is_some_and(|epoch| {
            work.charge(1).is_err()
                || self.pins[1].as_ref().and_then(writer::WalEpoch::from_pin) != Some(epoch)
        }) || !self.owner.drain(work, io)
        {
            self.latched = true;
            return false;
        }
        true
    }
}
fn hook_intact() -> bool {
    let b = BASE.load(Ordering::Acquire);
    let a = ALIAS.load(Ordering::Acquire);
    if b.is_null() || a.is_null() || INSTALL_TAINTED.load(Ordering::Acquire) {
        return false;
    }
    // SAFETY: Lookup must return both installed process-lifetime identities before
    // the stock VFS method table is dereferenced.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        if ffi::sqlite3_vfs_find(c"unix".as_ptr()) != b
            || ffi::sqlite3_vfs_find(c"ledgerful-scoped-proof".as_ptr()) != a
        {
            return false;
        }
        let Some(get) = (*b).xGetSystemCall else {
            return false;
        };
        get(b, c"open".as_ptr()).map(|f| f as usize) == Some(hooked_open as *const () as usize)
            && get(b, c"fchmod".as_ptr()).map(|f| f as usize)
                == Some(hooked_fchmod as *const () as usize)
            && get(b, c"fstat".as_ptr()).map(|f| f as usize)
                == Some(hooked_fstat as *const () as usize)
    }
}
pub fn with_scope<'m, T>(
    scope: &mut Scope<'m>,
    io: &ReadBudget<'m>,
    work: &mut Work<'m>,
    f: impl FnOnce() -> T,
) -> Option<T> {
    ACTIVE.with(|c| {
        if !c.get().is_null() || work.charge(1).is_err() || !hook_intact() {
            return None;
        }
        // The stock PRNG may open system entropy before first xOpen.
        // Prime it outside the scoped namespace.
        let mut seed = 0u8;
        // SAFETY: the writable byte lives through the synchronous SQLite call.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        unsafe { ffi::sqlite3_randomness(1, (&mut seed as *mut u8).cast()) };
        let mut activation = Activation {
            scope,
            io,
            work,
            write_admitted: false,
            role: OpenRole::None,
            main_probe: None,
        };
        let activation_ptr = std::ptr::addr_of_mut!(activation);
        c.set(activation_ptr.cast());
        struct Reset<'a>(&'a Cell<*mut c_void>);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.set(std::ptr::null_mut())
            }
        }
        let reset = Reset(c);
        let result = f();
        drop(reset);
        (!activation.write_admitted
            && activation.scope.check(activation.io, activation.work)
            && hook_intact())
        .then_some(result)
    })
}

unsafe extern "C" fn hooked_open(path: *const c_char, flags: c_int, mode: c_int) -> c_int {
    // SAFETY: the installed delegate has the pinned stock callback signature.
    // The writer activation is live on this thread only during with_scope.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        let p = ORIGINAL_OPEN.load(Ordering::Acquire);
        if p.is_null() {
            *nix::libc::__errno_location() = nix::libc::EACCES;
            return -1;
        }
        let original: unsafe extern "C" fn(*const c_char, c_int, c_int) -> c_int =
            std::mem::transmute(p);
        ACTIVE.with(|slot| {
            let context = slot.get();
            if context.is_null() {
                return original(path, flags, mode);
            }
            writer::open(
                &mut *context.cast::<Activation<'_, '_>>(),
                path,
                flags,
                mode,
            )
        })
    }
}
fn number(input: &[u8], radix: u32) -> Option<u64> {
    if input.is_empty() {
        return None;
    }
    let mut value = 0u64;
    for b in input {
        let digit = match *b {
            b'0'..=b'9' => (*b - b'0') as u32,
            b'a'..=b'f' => (*b - b'a' + 10) as u32,
            b'A'..=b'F' => (*b - b'A' + 10) as u32,
            _ => return None,
        };
        if digit >= radix {
            return None;
        }
        value = value.checked_mul(radix as u64)?.checked_add(digit as u64)?;
    }
    Some(value)
}
fn map_line(
    line: &[u8],
    address: usize,
    page: c_int,
    size: c_int,
    dev: u64,
    ino: u64,
) -> Option<bool> {
    let mut fields = line
        .split(|c| c.is_ascii_whitespace())
        .filter(|x| !x.is_empty());
    let range = fields.next()?;
    let perms = fields.next()?;
    let offset = number(fields.next()?, 16)?;
    let device = fields.next()?;
    let inode = number(fields.next()?, 10)?;
    let split = range.iter().position(|c| *c == b'-')?;
    let start = usize::try_from(number(&range[..split], 16)?).ok()?;
    let end = usize::try_from(number(&range[split + 1..], 16)?).ok()?;
    if address < start || address >= end {
        return Some(false);
    }
    if address.checked_add(size as usize)? > end {
        return Some(false);
    }
    let colon = device.iter().position(|c| *c == b':')?;
    let major = number(&device[..colon], 16)?;
    let minor = number(&device[colon + 1..], 16)?;
    let wanted = (page as u64).checked_mul(size as u64)?;
    Some(
        perms.len() == 4
            && perms[0] == b'r'
            && perms[3] == b's'
            && major == nix::sys::stat::major(dev)
            && minor == nix::sys::stat::minor(dev)
            && inode == ino
            && offset.checked_add((address - start) as u64) == Some(wanted),
    )
}
fn mapping_matches(address: *mut c_void, page: c_int, size: c_int) -> bool {
    ACTIVE.with(|slot| {
        let p = slot.get();
        if p.is_null() || address.is_null() || page < 0 || size <= 0 {
            return false;
        }
        // SAFETY: the scoped caller retains this activation in this thread's
        // TLS. Identity and budget checks do not call back into SQLite.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let g = unsafe { &mut *p.cast::<Activation<'_, '_>>() };
        if !g.scope.check(g.io, g.work) || g.work.charge(2).is_err() {
            return false;
        }
        let Some(meta) = g.scope.pins[2].as_ref().and_then(|pin| pin.metadata().ok()) else {
            return false;
        };
        let how = OpenHow::new()
            .flags(OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW)
            .resolve(ResolveFlag::RESOLVE_NO_XDEV | ResolveFlag::RESOLVE_NO_SYMLINKS);
        let Ok(fd) = openat2(g.scope.owner.proc_fd(), "../maps", how) else {
            return false;
        };
        let mut maps = File::from(fd);
        let Ok(fs) = nix::sys::statfs::fstatfs(&maps) else {
            return false;
        };
        if fs.filesystem_type() != nix::sys::statfs::PROC_SUPER_MAGIC {
            return false;
        }
        let mut chunk = [0u8; 4096];
        let mut header = [0u8; 256];
        let mut used = 0usize;
        let mut total = 0usize;
        let mut found = false;
        loop {
            if g.io.checkpoint().is_err() || g.work.charge(1).is_err() {
                return false;
            }
            let Ok(n) = maps.read(&mut chunk) else {
                return false;
            };
            if n == 0 {
                break;
            }
            let Some(next) = total.checked_add(n) else {
                return false;
            };
            total = next;
            if total > MAPS_CAP || g.work.charge(n.saturating_mul(4)).is_err() {
                return false;
            }
            for &b in &chunk[..n] {
                if b == b'\n' {
                    if let Some(true) = map_line(
                        &header[..used],
                        address as usize,
                        page,
                        size,
                        meta.dev(),
                        meta.ino(),
                    ) {
                        if found {
                            return false;
                        }
                        found = true;
                    }
                    used = 0;
                } else if used < header.len() {
                    header[used] = b;
                    used += 1;
                }
            }
        }
        found && used == 0 && g.post()
    })
}
static BASE: AtomicPtr<ffi::sqlite3_vfs> = AtomicPtr::new(std::ptr::null_mut());
static ALIAS: AtomicPtr<ffi::sqlite3_vfs> = AtomicPtr::new(std::ptr::null_mut());
#[repr(C)]
struct Wrapper {
    base: ffi::sqlite3_file,
    inner: *mut ffi::sqlite3_file,
    policy: Policy,
    role: OpenRole,
}
unsafe fn inner(f: *mut ffi::sqlite3_file) -> *mut ffi::sqlite3_file {
    // SAFETY: The repr(C) Wrapper is live and begins with f. shim_open initializes
    // the stock-file pointer before publishing the callback table.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { (*(f.cast::<Wrapper>())).inner }
}
macro_rules! delegate {($name:ident,$member:ident,$($arg:ident:$ty:ty),*)=>{unsafe extern "C" fn $name(f:*mut ffi::sqlite3_file,$($arg:$ty),*)->c_int{ unsafe {let q=inner(f);if q.is_null()||(*q).pMethods.is_null(){return ffi::SQLITE_IOERR}match (*(*q).pMethods).$member{Some(cb)=>cb(q,$($arg),*),None=>ffi::SQLITE_IOERR}}}};}
delegate!(read_delegate,xRead,b:*mut c_void,n:c_int,o:i64);
delegate!(write_delegate,xWrite,b:*const c_void,n:c_int,offset:i64);
delegate!(truncate_delegate,xTruncate,len:i64);
delegate!(sync_delegate,xSync,flags:c_int);
delegate!(size_delegate,xFileSize,p:*mut i64);
delegate!(lock_delegate,xLock,n:c_int);
delegate!(unlock_delegate,xUnlock,n:c_int);
delegate!(reserved,xCheckReservedLock,p:*mut c_int);
delegate!(control_delegate,xFileControl,n:c_int,p:*mut c_void);
delegate!(shm_lock_delegate,xShmLock,off:c_int,n:c_int,flags:c_int);
delegate!(shm_unmap_delegate,xShmUnmap,n:c_int);

fn check_active() -> bool {
    ACTIVE.with(|slot| {
        // SAFETY: The current thread owns the activation. Scope::check does not call
        // back into the SQLite method table.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let Some(g) = (unsafe { slot.get().cast::<Activation<'_, '_>>().as_mut() }) else {
            return false;
        };
        g.scope.check(g.io, g.work)
    })
}
pub(super) fn capture_check(connection: &rusqlite::Connection) -> bool {
    if !check_active() {
        return false;
    }
    let mut moved = -1;
    connection
        .file_control(None::<&str>, rusqlite::FileControl::HasMoved(&mut moved))
        .is_ok()
        && moved == 0
}
pub(super) fn capture_admit(connection: &rusqlite::Connection) -> bool {
    // SAFETY: an active, caller-owned connection supplies a live handle. This
    // queries its actual transaction state without stepping SQLite statements.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    if unsafe { ffi::sqlite3_txn_state(connection.handle(), c"main".as_ptr()) }
        != ffi::SQLITE_TXN_WRITE
        || !capture_check(connection)
    {
        return false;
    }
    ACTIVE.with(|slot| {
        let activation = slot.get().cast::<Activation<'_, '_>>();
        if activation.is_null() {
            return false;
        }
        // SAFETY: the synchronous writer owns this thread-local activation.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let active = unsafe { &mut *activation };
        if active.scope.policy != Policy::AuthorizedWriter || active.scope.writer_admission_spent {
            return false;
        }
        // BEGIN now holds the stock WAL writer lock. The one admitted
        // transaction can change the WAL after the captured epoch check.
        active.scope.warm_epoch = None;
        active.scope.writer_admission_spent = true;
        active.write_admitted = true;
        true
    })
}

fn charge_active(units: usize) -> bool {
    ACTIVE.with(|slot| {
        let p = slot.get();
        if p.is_null() {
            return false;
        }
        // SAFETY: the TLS slot holds the active stack activation until reset.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let g = unsafe { &mut *p.cast::<Activation<'_, '_>>() };
        !g.scope.latched && g.io.checkpoint().is_ok() && g.work.charge(units).is_ok()
    })
}

fn charge_active_native_read(bytes: usize) -> bool {
    ACTIVE.with(|slot| {
        let p = slot.get();
        if p.is_null() {
            return false;
        }
        // SAFETY: the TLS slot holds the active stack activation until reset.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        let g = unsafe { &mut *p.cast::<Activation<'_, '_>>() };
        !g.scope.latched && g.io.charge_native_read(bytes).is_ok()
    })
}

unsafe extern "C" fn read(f: *mut ffi::sqlite3_file, b: *mut c_void, n: c_int, o: i64) -> c_int {
    // SAFETY: SQLite supplies its active file and output range to the delegate.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        if n < 0
            || !charge_active((n as usize).saturating_add(1))
            || !charge_active_native_read(n as usize)
        {
            return ffi::SQLITE_INTERRUPT;
        }
        let rc = read_delegate(f, b, n, o);
        if !charge_active(0) {
            return ffi::SQLITE_INTERRUPT;
        }
        rc
    }
}
unsafe extern "C" fn sync(f: *mut ffi::sqlite3_file, flags: c_int) -> c_int {
    // SAFETY: SQLite supplies the live wrapper and xSync flags. may_write
    // proves the current thread owns an admitted activation. The stock call
    // may use the directory hook, so no Rust borrow spans that call.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        if !may_write(f) {
            return ffi::SQLITE_READONLY;
        }
        ACTIVE.with(|slot| {
            let raw = slot.get().cast::<Activation<'_, '_>>();
            (*raw).role = OpenRole::Directory;
            let result = sync_delegate(f, flags);
            (*raw).role = OpenRole::None;
            if check_active() {
                result
            } else {
                ffi::SQLITE_IOERR_FSYNC
            }
        })
    }
}
unsafe extern "C" fn size(f: *mut ffi::sqlite3_file, p: *mut i64) -> c_int {
    // SAFETY: SQLite supplies the live wrapper and writable i64 output slot that
    // the stock xFileSize callback receives unchanged.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        if !charge_active(1) {
            return ffi::SQLITE_INTERRUPT;
        }
        let rc = size_delegate(f, p);
        if !charge_active(0) {
            return ffi::SQLITE_INTERRUPT;
        }
        rc
    }
}
unsafe extern "C" fn unlock(f: *mut ffi::sqlite3_file, n: c_int) -> c_int {
    // SAFETY: SQLite supplies a live wrapper whose stock file remains valid
    // through xUnlock delegation.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        if (*f.cast::<Wrapper>()).policy == Policy::WarmPreflight {
            // The wrapper obtained exclusive stock MAIN ownership before exposing
            // the file. Keep it until stock xClose releases the physical lock.
            return ffi::SQLITE_OK;
        }
        unlock_delegate(f, n)
    }
}
unsafe extern "C" fn lock(f: *mut ffi::sqlite3_file, n: c_int) -> c_int {
    // SAFETY: SQLite supplies a live wrapper and lock level. The stock xLock runs
    // only after the role and budget checks.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        let policy = (*f.cast::<Wrapper>()).policy;
        if n > ffi::SQLITE_LOCK_SHARED
            && policy != Policy::AuthorizedWriter
            && !(policy == Policy::WarmPreflight && n == ffi::SQLITE_LOCK_EXCLUSIVE)
        {
            return ffi::SQLITE_READONLY;
        }
        if !charge_active(1) {
            return ffi::SQLITE_INTERRUPT;
        }
        lock_delegate(f, n)
    }
}
unsafe extern "C" fn control(f: *mut ffi::sqlite3_file, n: c_int, p: *mut c_void) -> c_int {
    // SAFETY: MMAP_SIZE supplies a writable aligned i64. HAS_MOVED forwards its
    // original argument; other opcodes never dereference it.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        if n == ffi::SQLITE_FCNTL_MMAP_SIZE {
            if p.is_null() {
                return ffi::SQLITE_MISUSE;
            }
            *p.cast::<i64>() = 0;
            return ffi::SQLITE_OK;
        }
        if n == ffi::SQLITE_FCNTL_HAS_MOVED {
            if !charge_active(1) {
                return ffi::SQLITE_INTERRUPT;
            }
            return control_delegate(f, n, p);
        }
        ffi::SQLITE_NOTFOUND
    }
}
unsafe extern "C" fn fetch(
    _: *mut ffi::sqlite3_file,
    _: i64,
    _: c_int,
    p: *mut *mut c_void,
) -> c_int {
    // SAFETY: SQLite supplies the writable void** output for xFetch. A null
    // mapping sends the pager down its ordinary read path.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        if p.is_null() {
            return ffi::SQLITE_MISUSE;
        }
        *p = std::ptr::null_mut();
        ffi::SQLITE_OK
    }
}
unsafe extern "C" fn unfetch(_: *mut ffi::sqlite3_file, _: i64, _: *mut c_void) -> c_int {
    ffi::SQLITE_OK
}
unsafe extern "C" fn close(f: *mut ffi::sqlite3_file) -> c_int {
    // SAFETY: SQLite calls this with the initialized wrapper it previously
    // opened; its inner file remains live until delegating xClose.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        let q = inner(f);
        if q.is_null() || (*q).pMethods.is_null() {
            return ffi::SQLITE_IOERR;
        }
        let rc = match (*(*q).pMethods).xClose {
            Some(cb) => cb(q),
            None => ffi::SQLITE_IOERR,
        };
        (*f).pMethods = std::ptr::null();
        rc
    }
}
unsafe fn may_write(f: *mut ffi::sqlite3_file) -> bool {
    // SAFETY: SQLite supplies the initialized wrapper. The current thread owns
    // the activation and checks it before delegating a native write.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        let file = &*f.cast::<Wrapper>();
        file.role == OpenRole::Wal
            && file.policy == Policy::AuthorizedWriter
            && ACTIVE.with(|slot| {
                let Some(active) = slot.get().cast::<Activation<'_, '_>>().as_mut() else {
                    return false;
                };
                active.write_admitted && active.scope.check(active.io, active.work)
            })
    }
}
unsafe extern "C" fn write(
    f: *mut ffi::sqlite3_file,
    b: *const c_void,
    n: c_int,
    offset: i64,
) -> c_int {
    // SAFETY: SQLite retains its wrapper and input buffer through xWrite.
    // Only the admitted stock WAL receives this operation.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        if n < 0 || !may_write(f) || !charge_active((n as usize).saturating_add(1)) {
            return ffi::SQLITE_IOERR_WRITE;
        }
        write_delegate(f, b, n, offset)
    }
}
unsafe extern "C" fn truncate(f: *mut ffi::sqlite3_file, len: i64) -> c_int {
    // SAFETY: SQLite supplies this live wrapper and the xTruncate arguments.
    // Only the admitted WAL receives the call.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        if !may_write(f) {
            return ffi::SQLITE_IOERR_TRUNCATE;
        }
        truncate_delegate(f, len)
    }
}
unsafe extern "C" fn sector(f: *mut ffi::sqlite3_file) -> c_int {
    // SAFETY: The live wrapper retains the stock file, whose method table is
    // checked before querying its sector size.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        let q = inner(f);
        if q.is_null() || (*q).pMethods.is_null() {
            return 0;
        }
        match (*(*q).pMethods).xSectorSize {
            Some(cb) => cb(q),
            None => 0,
        }
    }
}
unsafe extern "C" fn device(f: *mut ffi::sqlite3_file) -> c_int {
    // SAFETY: The live wrapper retains the stock file, whose method table is
    // checked before querying its capabilities.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        let q = inner(f);
        if q.is_null() || (*q).pMethods.is_null() {
            return 0;
        }
        let bits = match (*(*q).pMethods).xDeviceCharacteristics {
            Some(cb) => cb(q),
            None => 0,
        };
        if (*f.cast::<Wrapper>()).policy == Policy::ColdPreflight {
            bits | ffi::SQLITE_IOCAP_IMMUTABLE as c_int
        } else {
            bits
        }
    }
}
unsafe extern "C" fn shm_map(
    f: *mut ffi::sqlite3_file,
    page: c_int,
    size: c_int,
    extend: c_int,
    out: *mut *mut c_void,
) -> c_int {
    // SAFETY: SQLite supplies this live wrapper and its mutable result slot.
    // The scoped writer function owns the live activation for this call.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        if out.is_null() {
            return ffi::SQLITE_IOERR;
        }
        *out = std::ptr::null_mut();
        if size != 32768 {
            return ffi::SQLITE_IOERR;
        }
        ACTIVE.with(|slot| {
            let raw = slot.get().cast::<Activation<'_, '_>>();
            if raw.is_null() || (*f.cast::<Wrapper>()).policy != Policy::AuthorizedWriter {
                return ffi::SQLITE_IOERR_SHMMAP;
            }
            writer::map_writable(raw, f, page, size, extend, out)
        })
    }
}
unsafe extern "C" fn shm_lock(
    f: *mut ffi::sqlite3_file,
    off: c_int,
    n: c_int,
    flags: c_int,
) -> c_int {
    // SAFETY: the registered I/O table receives the live wrapper and exact
    // stock lock arguments. Its caller owns the thread-local activation.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        let policy = (*f.cast::<Wrapper>()).policy;
        if flags & ffi::SQLITE_SHM_EXCLUSIVE as c_int != 0 {
            if policy != Policy::AuthorizedWriter {
                return ffi::SQLITE_READONLY;
            }
            // The pinned stock pager releases exclusive WAL lock byte 0 at
            // the end of its write transaction, on commit and on rollback.
            if off == 0 && n > 0 && flags & ffi::SQLITE_SHM_UNLOCK as c_int != 0 {
                ACTIVE.with(|slot| {
                    if let Some(active) = slot.get().cast::<Activation<'_, '_>>().as_mut() {
                        active.write_admitted = false;
                    }
                });
            }
        }
        shm_lock_delegate(f, off, n, flags)
    }
}
unsafe extern "C" fn barrier(f: *mut ffi::sqlite3_file) {
    // SAFETY: SQLite retains this wrapper for the callback. The inner stock
    // file and its methods remain live until xClose; the barrier is void.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        let q = inner(f);
        if !q.is_null()
            && !(*q).pMethods.is_null()
            && let Some(cb) = (*(*q).pMethods).xShmBarrier
        {
            cb(q)
        }
    }
}
unsafe extern "C" fn shm_unmap(f: *mut ffi::sqlite3_file, _: c_int) -> c_int {
    // SAFETY: SQLite supplies its live initialized wrapper. The stock method
    // detaches this handle without deleting the shared sidecar.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { shm_unmap_delegate(f, 0) }
}
static METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    iVersion: 3,
    xClose: Some(close),
    xRead: Some(read),
    xWrite: Some(write),
    xTruncate: Some(truncate),
    xSync: Some(sync),
    xFileSize: Some(size),
    xLock: Some(lock),
    xUnlock: Some(unlock),
    xCheckReservedLock: Some(reserved),
    xFileControl: Some(control),
    xSectorSize: Some(sector),
    xDeviceCharacteristics: Some(device),
    xShmMap: Some(shm_map),
    xShmLock: Some(shm_lock),
    xShmBarrier: Some(barrier),
    xShmUnmap: Some(shm_unmap),
    xFetch: Some(fetch),
    xUnfetch: Some(unfetch),
};
unsafe extern "C" fn no_delete(_: *mut ffi::sqlite3_vfs, _: *const c_char, _: c_int) -> c_int {
    ffi::SQLITE_READONLY
}
unsafe extern "C" fn shim_open(
    _: *mut ffi::sqlite3_vfs,
    z: *const c_char,
    f: *mut ffi::sqlite3_file,
    flags: c_int,
    out: *mut c_int,
) -> c_int {
    // SAFETY: SQLite supplies this VFS's sized and aligned file storage.
    // The wrapper prefix and stock tail remain live through the native call.
    // Only a validated stock file receives the registered callback table.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        if f.is_null() {
            return ffi::SQLITE_CANTOPEN;
        }
        (*f).pMethods = std::ptr::null();
        let base = BASE.load(Ordering::Acquire);
        if base.is_null() {
            return ffi::SQLITE_CANTOPEN;
        }
        let Some(cb) = (*base).xOpen else {
            return ffi::SQLITE_CANTOPEN;
        };
        ACTIVE.with(|slot| {
            let raw = slot.get();
            if raw.is_null() {
                return ffi::SQLITE_CANTOPEN;
            }
            let ctx = raw.cast::<Activation<'_, '_>>();
            let is_wal = flags & ffi::SQLITE_OPEN_WAL as c_int != 0;
            let is_main = flags & ffi::SQLITE_OPEN_MAIN_DB as c_int != 0;
            if !is_wal && !is_main {
                return ffi::SQLITE_CANTOPEN;
            }
            let policy = (*ctx).scope.policy;
            if (is_main && flags & ffi::SQLITE_OPEN_READWRITE as c_int == 0)
                || (is_wal && policy == Policy::ColdPreflight)
            {
                return ffi::SQLITE_CANTOPEN;
            }
            let before = if is_wal {
                (*ctx).scope.wal_count
            } else {
                (*ctx).scope.main_count
            };
            let w = f.cast::<Wrapper>();
            let q = w.add(1).cast::<ffi::sqlite3_file>();
            (*w).inner = q;
            (*w).policy = policy;
            (*w).role = if is_wal {
                OpenRole::Wal
            } else {
                OpenRole::Main
            };
            let selected = if is_wal && policy == Policy::WarmPreflight {
                (flags & !(ffi::SQLITE_OPEN_READWRITE as c_int | ffi::SQLITE_OPEN_CREATE as c_int))
                    | ffi::SQLITE_OPEN_READONLY as c_int
            } else {
                flags
            };
            if is_main {
                if z.is_null()
                    || (*ctx).work.charge(PATH_CAP + 8).is_err()
                    || (*ctx).main_probe.is_some()
                {
                    return ffi::SQLITE_CANTOPEN;
                }
                let length = nix::libc::strnlen(z, PATH_CAP);
                if length >= PATH_CAP
                    || length != (*ctx).scope.lengths[0]
                    || std::slice::from_raw_parts(z.cast::<u8>(), length)
                        != &(&(*ctx).scope.paths[0])[..length]
                {
                    return ffi::SQLITE_CANTOPEN;
                }
                let Some(identity) = (*ctx).scope.main_identity() else {
                    return ffi::SQLITE_CANTOPEN;
                };
                (*ctx).main_probe = Some(MainFileProbe {
                    identity,
                    fd: None,
                    calls: 0,
                });
            }
            (*ctx).role = (*w).role;
            let rc = cb(base, z, q, selected, out);
            let actual_main = (*ctx).main_probe.take();
            (*ctx).role = OpenRole::None;
            if rc != ffi::SQLITE_OK {
                return rc;
            }
            let after = if is_wal {
                (*ctx).scope.wal_count
            } else {
                (*ctx).scope.main_count
            };
            if (*ctx).scope.latched
                || (is_wal && after == before)
                || (is_main && !actual_main.is_some_and(|probe| probe.calls == 2))
                || (*q).pMethods.is_null()
                || (*(*q).pMethods).iVersion < 3
                || (is_main && (*(*q).pMethods).xShmMap.is_none())
            {
                if !(*q).pMethods.is_null()
                    && let Some(close) = (*(*q).pMethods).xClose
                {
                    close(q);
                }
                return ffi::SQLITE_CANTOPEN;
            }
            if is_main && policy.preflights() {
                let rc = writer::exclusive_readonly_main(q, out);
                if rc != ffi::SQLITE_OK {
                    if let Some(close) = (*(*q).pMethods).xClose {
                        close(q);
                    }
                    return rc;
                }
            }
            if is_wal && policy == Policy::WarmPreflight {
                let mut header = [0; 32];
                let valid = read(f, header.as_mut_ptr().cast(), header.len() as c_int, 0)
                    == ffi::SQLITE_OK
                    && (*ctx)
                        .scope
                        .warm_epoch
                        .is_some_and(|epoch| epoch.accepts_header(header));
                if !valid {
                    if let Some(close) = (*(*q).pMethods).xClose {
                        close(q);
                    }
                    (*ctx).scope.latched = true;
                    return ffi::SQLITE_CANTOPEN;
                }
            }
            // SQLite can reuse a deferred native fd without calling its
            // Unix open hook. The mandatory fstats observed this fd too.
            if is_main && after == before {
                (*ctx).scope.main_count += 1;
            }
            (*f).pMethods = &METHODS;
            ffi::SQLITE_OK
        })
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::change_evidence) enum AliasInstallError {
    Unavailable,
    Restore,
}

/// Install the scoped writer VFS after inert heap preparation.
/// Caller owns the process's exclusive startup and matched profile.
pub(in crate::change_evidence) unsafe fn install_alias() -> Result<(), AliasInstallError> {
    use AliasInstallError::{Restore, Unavailable};
    // SAFETY: exclusive startup prevents concurrent VFS use or mutation.
    // The pinned SQLite VFS owns all returned tables and named callback slots.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        if !ALIAS.load(Ordering::Acquire).is_null() {
            return Err(Unavailable);
        }
        let default_vfs = ffi::sqlite3_vfs_find(std::ptr::null());
        if default_vfs.is_null()
            || !ffi::sqlite3_vfs_find(c"ledgerful-scoped-proof".as_ptr()).is_null()
        {
            return Err(Unavailable);
        }
        let b = ffi::sqlite3_vfs_find(c"unix".as_ptr());
        if b.is_null()
            || b != default_vfs
            || (*b).iVersion != 3
            || (*b).szOsFile != 120
            || (*b).mxPathname != 512
            || (*b).xOpen.is_none()
            || (*b).xAccess.is_none()
            || (*b).xDelete.is_none()
            || (*b).xFullPathname.is_none()
        {
            return Err(Unavailable);
        }
        let (Some(get), Some(set)) = ((*b).xGetSystemCall, (*b).xSetSystemCall) else {
            return Err(Unavailable);
        };
        let (Some(old_open), Some(old_chmod), Some(old_fstat)) = (
            get(b, c"open".as_ptr()),
            get(b, c"fchmod".as_ptr()),
            get(b, c"fstat".as_ptr()),
        ) else {
            return Err(Unavailable);
        };
        // Only used at trusted, single-threaded startup. In this pinned SQLite,
        // named reset establishes the original default on first use.
        for (name, old) in [
            (c"open", old_open),
            (c"fchmod", old_chmod),
            (c"fstat", old_fstat),
        ] {
            let rc = set(b, name.as_ptr(), None);
            let reset = get(b, name.as_ptr()).map(|f| f as usize);
            if rc != ffi::SQLITE_OK || reset != Some(old as usize) {
                let restored = set(b, name.as_ptr(), Some(old)) == ffi::SQLITE_OK
                    && get(b, name.as_ptr()).map(|f| f as usize) == Some(old as usize);
                if !restored {
                    INSTALL_TAINTED.store(true, Ordering::Release);
                    return Err(Restore);
                }
                return Err(Unavailable);
            }
        }
        if old_open as usize == hooked_open as *const () as usize
            || old_chmod as usize == hooked_fchmod as *const () as usize
            || old_fstat as usize == hooked_fstat as *const () as usize
        {
            return Err(Unavailable);
        }
        let Some(sz) = (*b)
            .szOsFile
            .checked_add(std::mem::size_of::<Wrapper>() as c_int)
        else {
            return Err(Unavailable);
        };
        ORIGINAL_OPEN.store(old_open as *mut c_void, Ordering::Release);
        ORIGINAL_FCHMOD.store(old_chmod as *mut c_void, Ordering::Release);
        ORIGINAL_FSTAT.store(old_fstat as *mut c_void, Ordering::Release);
        BASE.store(b, Ordering::Release);
        let mut alias = Box::new(std::ptr::read(b));
        alias.zName = c"ledgerful-scoped-proof".as_ptr();
        alias.pNext = std::ptr::null_mut();
        alias.szOsFile = sz;
        alias.xOpen = Some(shim_open);
        alias.xDelete = Some(no_delete);
        let alias = Box::into_raw(alias);
        if ffi::sqlite3_vfs_register(alias, 0) != ffi::SQLITE_OK {
            let lookup = ffi::sqlite3_vfs_find(c"ledgerful-scoped-proof".as_ptr());
            if lookup.is_null() {
                drop(Box::from_raw(alias));
                if ffi::sqlite3_vfs_find(std::ptr::null()) == b {
                    return Err(Unavailable);
                }
            }
            INSTALL_TAINTED.store(true, Ordering::Release);
            return Err(Restore);
        }
        let open_hook: unsafe extern "C" fn() = std::mem::transmute(
            hooked_open as unsafe extern "C" fn(*const c_char, c_int, c_int) -> c_int,
        );
        let chmod_hook: unsafe extern "C" fn() = std::mem::transmute(
            hooked_fchmod as unsafe extern "C" fn(c_int, nix::libc::mode_t) -> c_int,
        );
        let fstat_hook: unsafe extern "C" fn() = std::mem::transmute(
            hooked_fstat as unsafe extern "C" fn(c_int, *mut nix::libc::stat) -> c_int,
        );
        let success = ffi::sqlite3_vfs_find(std::ptr::null()) == b
            && ffi::sqlite3_vfs_find(c"unix".as_ptr()) == b
            && ffi::sqlite3_vfs_find(c"ledgerful-scoped-proof".as_ptr()) == alias
            && set(b, c"open".as_ptr(), Some(open_hook)) == ffi::SQLITE_OK
            && set(b, c"fchmod".as_ptr(), Some(chmod_hook)) == ffi::SQLITE_OK
            && set(b, c"fstat".as_ptr(), Some(fstat_hook)) == ffi::SQLITE_OK
            && get(b, c"open".as_ptr()).map(|f| f as usize) == Some(open_hook as usize)
            && get(b, c"fchmod".as_ptr()).map(|f| f as usize) == Some(chmod_hook as usize)
            && get(b, c"fstat".as_ptr()).map(|f| f as usize) == Some(fstat_hook as usize);
        if !success {
            let _ = set(b, c"fstat".as_ptr(), Some(old_fstat));
            let _ = set(b, c"fchmod".as_ptr(), Some(old_chmod));
            let _ = set(b, c"open".as_ptr(), Some(old_open));
            let restored = get(b, c"open".as_ptr()).map(|f| f as usize) == Some(old_open as usize)
                && get(b, c"fchmod".as_ptr()).map(|f| f as usize) == Some(old_chmod as usize)
                && get(b, c"fstat".as_ptr()).map(|f| f as usize) == Some(old_fstat as usize);
            let unregistered = ffi::sqlite3_vfs_unregister(alias) == ffi::SQLITE_OK;
            if unregistered {
                drop(Box::from_raw(alias));
            }
            if !restored
                || !unregistered
                || ffi::sqlite3_vfs_find(std::ptr::null()) != b
                || !ffi::sqlite3_vfs_find(c"ledgerful-scoped-proof".as_ptr()).is_null()
            {
                INSTALL_TAINTED.store(true, Ordering::Release);
                return Err(Restore);
            }
            return Err(Unavailable);
        }
        ALIAS.store(alias, Ordering::Release);
        Ok(())
    }
}
