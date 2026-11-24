//! Contained reads and typed decoding for revision-bound evidence.
#![allow(
    dead_code,
    reason = "The byte readers precede the evidence producer wiring"
)]

pub(crate) mod attempt_projection;
mod attribute_policy;
#[cfg(any(windows, target_os = "linux"))]
mod baseline_discovery;
#[cfg(any(windows, target_os = "linux"))]
mod baseline_discovery_policy;
pub(crate) mod buffer;
pub(crate) mod config;
pub(crate) mod contained;
pub(crate) mod decode;
mod directory_rows;
mod filemode;
pub(crate) mod git;
pub(crate) mod git_config;
pub(crate) mod http_query;
pub(crate) mod http_verification;
pub(crate) mod http_worker;
mod inventory;
mod inventory_transcript;
#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
mod native_heap;
mod nested_ignore;
pub(crate) mod pack;
pub(crate) mod pack_store;
#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
mod passive_profile;
#[cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    target_env = "gnu",
    feature = "web"
))]
pub(crate) mod passive_startup;
mod root_ignore;
mod root_ignore_matcher;
pub(crate) mod scope;
mod tracked_attributes;
pub(crate) mod tree_index;

#[cfg(target_os = "linux")]
mod linux_default_policy;
#[cfg(target_os = "linux")]
mod linux_proc_fds;
#[cfg(target_os = "linux")]
mod linux_scope;
mod scope_owner;
mod witness_stream;

#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
pub(crate) unsafe fn install_stock_writer_before_sqlite() -> Result<bool, &'static str> {
    if !passive_profile::matches_before_initialization() {
        return Ok(false);
    }
    // SAFETY: the caller owns exclusive pre-init startup and the pinned
    // SQLite metadata matches.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { install_stock_hooks_before_sqlite() }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
unsafe fn install_stock_hooks_before_sqlite() -> Result<bool, &'static str> {
    use native_heap::HeapInstallError;
    use pack_store::stock_vfs::AliasInstallError;

    // SAFETY: the exact executable owns startup and has not made a VFS lookup.
    // The linked allocator's provenance is checked separately from metadata.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let prepared = match unsafe { native_heap::prepare_before_sqlite() } {
        Ok(prepared) => prepared,
        Err(HeapInstallError::Restore) => {
            return Err("native evidence startup could not restore the SQLite allocator");
        }
        Err(_) => return Ok(false),
    };
    // SAFETY: the inert forwarding table is installed. This is the first
    // initializing VFS lookup, still before the executable spawns threads.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    match unsafe { pack_store::stock_vfs::install_alias() } {
        Ok(()) => {
            // SAFETY: exact alias validation succeeded; the wrapper will stay
            // installed while only approved native scopes can activate it.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            unsafe { prepared.enable() };
            Ok(true)
        }
        // Once a VFS lookup has initialized SQLite, the forwarding allocator
        // must remain installed, but with scoped admission still disabled.
        Err(AliasInstallError::Unavailable) => Ok(false),
        Err(AliasInstallError::Restore) => {
            Err("native evidence startup could not restore the SQLite VFS")
        }
    }
}
