//! Exclusive startup for the opt-in native daemon reader.
use super::native_heap::{
    HeapInstallError, prepare_before_sqlite, stock_allocator_matches_before_sqlite,
};
use super::pack_store::register_isolated_passive_pager;
use super::passive_profile;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

static BACKGROUND_PARENT: AtomicU32 = AtomicU32::new(0);
static PRIVATE_PAGER_READY: AtomicBool = AtomicBool::new(false);

fn qualified_passive_heap_bytes() -> Option<usize> {
    // Each passive snapshot reserves a slice of the shared request memory.
    // The request accounts for cumulative allocation work across both reads.
    Some(24 * 1024 * 1024)
}

pub(super) fn admitted_heap_bytes() -> Option<usize> {
    PRIVATE_PAGER_READY
        .load(Ordering::Acquire)
        .then(qualified_passive_heap_bytes)
        .flatten()
}

/// The executable has parsed an enabled web start, before it creates threads
/// or makes any SQLite call. A background parent only records its intent.
pub(crate) unsafe fn prepare_before_threads(background: bool) -> Result<(), &'static str> {
    if qualified_passive_heap_bytes().is_none() {
        return Ok(());
    }
    if background {
        if passive_profile::is_single_threaded_main() {
            BACKGROUND_PARENT.store(std::process::id(), Ordering::Release);
        }
        return Ok(());
    }
    install()
}

/// The final child cannot see any threads that existed before the first fork.
pub(crate) fn confirm_parent_before_background_fork() {
    let owner = BACKGROUND_PARENT.load(Ordering::Acquire);
    if owner != 0 && (owner != std::process::id() || !passive_profile::is_single_threaded_main()) {
        BACKGROUND_PARENT.store(0, Ordering::Release);
    }
}

/// The daemon is the final child of the single-threaded binary startup and
/// has made no SQLite calls since the fork.
pub(crate) unsafe fn install_in_background_child() -> Result<(), &'static str> {
    let parent = BACKGROUND_PARENT.load(Ordering::Acquire);
    if parent == 0 || parent == std::process::id() {
        return Ok(());
    }
    install()
}

fn install() -> Result<(), &'static str> {
    if !passive_profile::is_single_threaded_main()
        || !passive_profile::matches_before_initialization()
        || !passive_profile::linked_allocator_matches()
    {
        return Ok(());
    }
    // SAFETY: startup owns SQLite exclusively, and the audited profile
    // queries above do not initialize it.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let allocator = unsafe { stock_allocator_matches_before_sqlite() };
    match allocator {
        Ok(true) => {}
        Err(HeapInstallError::Restore) => {
            return Err("native evidence startup could not restore the SQLite allocator");
        }
        _ => return Ok(()),
    }
    // SAFETY: startup is exclusive and SQLite is still uninitialized.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let prepared = match unsafe { prepare_before_sqlite() } {
        Ok(prepared) => prepared,
        Err(HeapInstallError::Restore) => {
            return Err("native evidence startup could not restore the SQLite allocator");
        }
        Err(_) => return Ok(()),
    };
    // Register after preparing the inert forwarding allocator. Only the
    // private name is registered. Ordinary SQLite keeps its default.
    if register_isolated_passive_pager().is_ok() {
        // SAFETY: the exact private VFS is registered and startup is still exclusive.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        unsafe { prepared.enable() };
        PRIVATE_PAGER_READY.store(true, Ordering::Release);
    }
    Ok(())
}
