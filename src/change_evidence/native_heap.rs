//! Allocation admission for scoped work using the pinned stock SQLite heap.
use super::buffer::{MemoryBudget, MemoryLimit, Reservation};
use rusqlite::ffi;
use std::cell::Cell;
use std::ffi::{CStr, c_int, c_void};
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

// Allowance used by the native Linux x86_64 glibc feasibility control. This
// meters low-level SQLite allocations, including full reallocations, not RSS.
const ALLOCATION_OVERHEAD: usize = 4096 + 64;
const SOURCE_ID: &CStr =
    c"2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NativeHeapRefusal {
    Capacity,
    Cancelled,
    Deadline,
}

struct Credit<'a> {
    remaining: Cell<usize>,
    refusal: Cell<Option<NativeHeapRefusal>>,
    interrupted: &'a AtomicBool,
    deadline: Instant,
}

impl Credit<'_> {
    fn refusal(&self) -> Option<NativeHeapRefusal> {
        let current = self.refusal.get();
        if current.is_some() {
            return current;
        }
        let interruption = if self.interrupted.load(Ordering::Acquire) {
            Some(NativeHeapRefusal::Cancelled)
        } else if Instant::now() >= self.deadline {
            Some(NativeHeapRefusal::Deadline)
        } else {
            None
        };
        self.refusal.set(interruption);
        interruption
    }

    fn charge(&self, bytes: c_int) -> bool {
        if self.refusal().is_some() {
            return false;
        }
        let charge = usize::try_from(bytes)
            .ok()
            .filter(|bytes| *bytes > 0)
            .and_then(|bytes| bytes.checked_add(ALLOCATION_OVERHEAD));
        let Some(remaining) = charge.and_then(|bytes| self.remaining.get().checked_sub(bytes))
        else {
            self.refusal.set(Some(NativeHeapRefusal::Capacity));
            return false;
        };
        self.remaining.set(remaining);
        true
    }
}

thread_local! {
    static ACTIVE: Cell<*const c_void> = const { Cell::new(ptr::null()) };
}

/// Owns the reservation from before the first SQLite open through connection
/// close. Every activation spends from the same credit.
pub(super) struct NativeHeapTicket<'a> {
    credit: Credit<'a>,
    _reservation: Reservation<'a>,
}

impl<'a> NativeHeapTicket<'a> {
    pub(super) fn reserve(
        memory: &'a MemoryBudget,
        bytes: usize,
        deadline: Instant,
        interrupted: &'a AtomicBool,
    ) -> Result<Self, MemoryLimit> {
        let reservation = memory.reserve(bytes)?;
        Ok(Self {
            credit: Credit {
                remaining: Cell::new(bytes),
                refusal: Cell::new(None),
                interrupted,
                deadline,
            },
            _reservation: reservation,
        })
    }

    /// Enter once on the SQLite caller thread. An exhausted ticket may still
    /// activate for cleanup. The caller checks `refusal()` before releasing a
    /// successful result because SQLite can handle some OOM failures internally.
    pub(super) fn activate<T>(&self, f: impl FnOnce() -> T) -> Option<T> {
        if !ENABLED.load(Ordering::Acquire) {
            return None;
        }
        ACTIVE
            .try_with(|slot| {
                if !slot.get().is_null() {
                    return None;
                }
                struct Reset<'a>(&'a Cell<*const c_void>);
                impl Drop for Reset<'_> {
                    fn drop(&mut self) {
                        self.0.set(ptr::null());
                    }
                }
                slot.set(ptr::from_ref(&self.credit).cast());
                let _reset = Reset(slot);
                Some(f())
            })
            .ok()
            .flatten()
    }

    #[cfg(feature = "web")]
    pub(super) fn remaining_credit(&self) -> usize {
        self.credit.remaining.get()
    }

    pub(super) fn refusal(&self) -> Option<NativeHeapRefusal> {
        self.credit.refusal()
    }
}

fn admit(bytes: c_int) -> bool {
    ACTIVE
        .try_with(|slot| {
            let credit = slot.get().cast::<Credit<'_>>();
            if credit.is_null() {
                return true;
            }
            // The activation holds a shared borrow until the TLS slot is
            // cleared. Only this thread can reach the credit and its Cells.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            unsafe { (&*credit).charge(bytes) }
        })
        .unwrap_or(true)
}

struct Delegates {
    allocate: unsafe extern "C" fn(c_int) -> *mut c_void,
    reallocate: unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_void,
}

static ORIGINAL: OnceLock<Delegates> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn allocate(bytes: c_int) -> *mut c_void {
    let Some(original) = ORIGINAL.get().filter(|_| admit(bytes)) else {
        return ptr::null_mut();
    };
    // SQLite supplies the original allocator's request size.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { (original.allocate)(bytes) }
}

unsafe extern "C" fn reallocate(old: *mut c_void, bytes: c_int) -> *mut c_void {
    let Some(original) = ORIGINAL.get().filter(|_| admit(bytes)) else {
        return ptr::null_mut();
    };
    // SQLite supplies the original allocator's pointer and rounded size.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { (original.reallocate)(old, bytes) }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HeapInstallError {
    Unsupported,
    AlreadyAttempted,
    Configure,
    Restore,
}

/// A forwarding allocator installed with request activation still disabled.
/// Dropping this value leaves the allocator installed and inert.
#[must_use]
pub(super) struct PreparedHeap {
    original: ffi::sqlite3_mem_methods,
}

impl PreparedHeap {
    /// Restore the exact original table before SQLite or any worker starts.
    ///
    /// # Safety
    /// The same exclusive pre-initialization startup boundary still applies.
    pub(super) unsafe fn restore_before_sqlite(self) -> Result<(), HeapInstallError> {
        restore(self.original)
    }

    /// Commit one process-wide activation permission after startup review.
    ///
    /// # Safety
    /// The caller has verified the final allocator, VFS and worker profile.
    /// The SQLite allocator must stay installed for the process lifetime.
    pub(super) unsafe fn enable(self) {
        ENABLED.store(true, Ordering::Release);
    }
}

fn methods() -> Option<ffi::sqlite3_mem_methods> {
    let mut table = MaybeUninit::<ffi::sqlite3_mem_methods>::uninit();
    // Invoked only from the exclusive startup path. SQLite initializes the
    // C table on a successful GETMALLOC and will not retain the output pointer.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        if ffi::sqlite3_config(ffi::SQLITE_CONFIG_GETMALLOC, table.as_mut_ptr()) != ffi::SQLITE_OK {
            return None;
        }
        Some(table.assume_init())
    }
}

fn method_addresses(table: &ffi::sqlite3_mem_methods) -> [*const (); 8] {
    [
        table.xMalloc.map_or(ptr::null(), |f| f as _),
        table.xFree.map_or(ptr::null(), |f| f as _),
        table.xRealloc.map_or(ptr::null(), |f| f as _),
        table.xSize.map_or(ptr::null(), |f| f as _),
        table.xRoundup.map_or(ptr::null(), |f| f as _),
        table.xInit.map_or(ptr::null(), |f| f as _),
        table.xShutdown.map_or(ptr::null(), |f| f as _),
        table.pAppData.cast_const().cast(),
    ]
}

fn set_methods(mut table: ffi::sqlite3_mem_methods) -> bool {
    // The exclusive startup caller supplies the actual SQLite ABI table.
    // SQLite copies it synchronously without retaining the pointer.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        ffi::sqlite3_config(ffi::SQLITE_CONFIG_MALLOC, ptr::from_mut(&mut table)) == ffi::SQLITE_OK
    }
}

fn restore(original: ffi::sqlite3_mem_methods) -> Result<(), HeapInstallError> {
    if set_methods(original)
        && methods()
            .is_some_and(|current| method_addresses(&current) == method_addresses(&original))
    {
        return Ok(());
    }
    Err(HeapInstallError::Restore)
}

/// Check the compiled allocator through the pre-initialization SQLite configuration.
///
/// # Safety
/// Call from exclusive startup before any other thread or SQLite initializer.
#[cfg(feature = "web")]
pub(super) unsafe fn stock_allocator_matches_before_sqlite() -> Result<bool, HeapInstallError> {
    let original = methods().ok_or(HeapInstallError::Configure)?;
    let unset = ffi::sqlite3_mem_methods {
        xMalloc: None,
        xFree: None,
        xRealloc: None,
        xSize: None,
        xRoundup: None,
        xInit: None,
        xShutdown: None,
        pAppData: ptr::null_mut(),
    };
    // In the pinned SQLite, SETMALLOC copies this table without initializing
    // the allocator. GETMALLOC sees the null xMalloc and selects the compiled
    // default. Compare every callback and pAppData to the original table.
    if !set_methods(unset) {
        return Err(HeapInstallError::Configure);
    }
    if methods().is_some_and(|default| method_addresses(&default) == method_addresses(&original)) {
        return Ok(true);
    }
    restore(original)?;
    Ok(false)
}

/// Stage the forwarding table without enabling allocation admission.
///
/// # Safety
/// Call only once from exclusive process startup before any SQLite
/// initialization, including VFS lookup, or other threads. A later failure
/// may restore only under the same boundary. Once SQLite initializes, leave
/// the wrapper inert on failure without changing its callbacks.
pub(super) unsafe fn prepare_before_sqlite() -> Result<PreparedHeap, HeapInstallError> {
    // These version APIs read static build identity without initializing SQLite.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let accepted = unsafe {
        let source = ffi::sqlite3_sourceid();
        ffi::sqlite3_libversion_number() == 3_054_001
            && ffi::sqlite3_threadsafe() == 1
            && !source.is_null()
            && CStr::from_ptr(source) == SOURCE_ID
    };
    if !accepted {
        return Err(HeapInstallError::Unsupported);
    }
    let original = methods().ok_or(HeapInstallError::Configure)?;
    let (Some(malloc), Some(realloc)) = (original.xMalloc, original.xRealloc) else {
        return Err(HeapInstallError::Unsupported);
    };
    if method_addresses(&original)[..7]
        .iter()
        .any(|address| address.is_null())
        || std::ptr::fn_addr_eq(
            malloc,
            allocate as unsafe extern "C" fn(c_int) -> *mut c_void,
        )
        || std::ptr::fn_addr_eq(
            realloc,
            reallocate as unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_void,
        )
    {
        return Err(HeapInstallError::Unsupported);
    }
    ORIGINAL
        .set(Delegates {
            allocate: malloc,
            reallocate: realloc,
        })
        .map_err(|_| HeapInstallError::AlreadyAttempted)?;
    let mut forwarded = original;
    forwarded.xMalloc = Some(allocate);
    forwarded.xRealloc = Some(reallocate);
    if !set_methods(forwarded) {
        return Err(HeapInstallError::Configure);
    }
    if !methods().is_some_and(|current| method_addresses(&current) == method_addresses(&forwarded))
    {
        restore(original)?;
        return Err(HeapInstallError::Configure);
    }
    Ok(PreparedHeap { original })
}
