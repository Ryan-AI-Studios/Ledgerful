//! Inert, pre-initialization pinned SQLite build-profile check.
use rusqlite::ffi;
use std::ffi::CStr;

const SOURCE: &[u8] =
    b"2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76";
const OPTIONS: &[&[u8]] = &[
    b"ATOMIC_INTRINSICS=1",
    b"DEFAULT_AUTOVACUUM",
    b"DEFAULT_CACHE_SIZE=-2000",
    b"DEFAULT_FILE_FORMAT=4",
    b"DEFAULT_FOREIGN_KEYS",
    b"DEFAULT_JOURNAL_SIZE_LIMIT=-1",
    b"DEFAULT_MMAP_SIZE=0",
    b"DEFAULT_PAGE_SIZE=4096",
    b"DEFAULT_PCACHE_INITSZ=20",
    b"DEFAULT_RECURSIVE_TRIGGERS",
    b"DEFAULT_SECTOR_SIZE=4096",
    b"DEFAULT_SYNCHRONOUS=2",
    b"DEFAULT_WAL_AUTOCHECKPOINT=1000",
    b"DEFAULT_WAL_SYNCHRONOUS=2",
    b"DEFAULT_WORKER_THREADS=0",
    b"DIRECT_OVERFLOW_READ",
    b"ENABLE_API_ARMOR",
    b"ENABLE_COLUMN_METADATA",
    b"ENABLE_DBSTAT_VTAB",
    b"ENABLE_FTS3",
    b"ENABLE_FTS3_PARENTHESIS",
    b"ENABLE_FTS5",
    b"ENABLE_LOAD_EXTENSION",
    b"ENABLE_RTREE",
    b"ENABLE_STAT4",
    b"HAVE_ISNAN",
    b"MALLOC_SOFT_LIMIT=1024",
    b"MAX_ATTACHED=10",
    b"MAX_COLUMN=2000",
    b"MAX_COMPOUND_SELECT=500",
    b"MAX_DEFAULT_PAGE_SIZE=8192",
    b"MAX_EXPR_DEPTH=1000",
    b"MAX_FUNCTION_ARG=1000",
    b"MAX_LENGTH=1000000000",
    b"MAX_LIKE_PATTERN_LENGTH=50000",
    b"MAX_MMAP_SIZE=0x7fff0000",
    b"MAX_PAGE_COUNT=0xfffffffe",
    b"MAX_PAGE_SIZE=65536",
    b"MAX_SCHEMA=10000000",
    b"MAX_SQL_LENGTH=1000000000",
    b"MAX_TRIGGER_DEPTH=1000",
    b"MAX_VARIABLE_NUMBER=32766",
    b"MAX_VDBE_OP=250000000",
    b"MAX_WORKER_THREADS=8",
    b"MUTEX_PTHREADS",
    b"SOUNDEX",
    b"SYSTEM_MALLOC",
    b"TEMP_STORE=1",
    b"THREADSAFE=1",
    b"USE_URI",
];

/// These SQLite identity getters do not initialize the pinned SQLite build.
/// Caller must still establish trusted exclusive pre-thread startup and the
/// final linked provider before installing allocator or VFS hooks.
pub(super) fn matches_before_initialization() -> bool {
    if !cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "gnu",
        target_pointer_width = "64"
    )) {
        return false;
    }
    // SAFETY: getauxval reads the current process auxiliary vector; SQLite
    // returns static identity strings or null at the end of its options.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        if nix::libc::getauxval(nix::libc::AT_PAGESZ) != 4096 {
            return false;
        }
        let source = ffi::sqlite3_sourceid();
        if source.is_null()
            || CStr::from_ptr(source).to_bytes() != SOURCE
            || ffi::sqlite3_libversion_number() != 3_054_001
            || ffi::sqlite3_threadsafe() != 1
        {
            return false;
        }
        let mut found = 0usize;
        let mut compiler = false;
        for i in 0..=OPTIONS.len() + 1 {
            let item = ffi::sqlite3_compileoption_get(i as i32);
            if item.is_null() {
                return found == OPTIONS.len() && compiler;
            }
            let bytes = CStr::from_ptr(item).to_bytes();
            if bytes.starts_with(b"COMPILER=") {
                if compiler || bytes.len() == b"COMPILER=".len() {
                    return false;
                }
                compiler = true;
            } else {
                if OPTIONS.get(found).copied() != Some(bytes) {
                    return false;
                }
                found += 1;
            }
        }
        false
    }
}

#[cfg(feature = "web")]
pub(super) fn is_single_threaded_main() -> bool {
    let pid = std::process::id();
    if u32::try_from(nix::unistd::gettid().as_raw()).ok() != Some(pid) {
        return false;
    }
    let Ok(mut tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return false;
    };
    tasks
        .next()
        .and_then(Result::ok)
        .is_some_and(|task| task.file_name() == pid.to_string().as_str())
        && tasks.next().is_none()
}

#[cfg(feature = "web")]
pub(super) fn linked_allocator_matches() -> bool {
    use nix::libc;
    use std::ffi::c_void;
    use std::mem::MaybeUninit;

    fn image(address: *const c_void) -> Option<*mut c_void> {
        let mut info = MaybeUninit::<libc::Dl_info>::uninit();
        // SAFETY: dladdr inspects a function address without dereferencing it
        // and writes Dl_info only on success.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        unsafe {
            if libc::dladdr(address, info.as_mut_ptr()) == 0 {
                return None;
            }
            let base = info.assume_init().dli_fbase;
            (!base.is_null()).then_some(base)
        }
    }

    let Some(executable) = image(linked_allocator_matches as *const c_void) else {
        return false;
    };
    if ![
        ffi::sqlite3_config as *const c_void,
        ffi::sqlite3_sourceid as *const c_void,
        ffi::sqlite3_open_v2 as *const c_void,
        ffi::sqlite3_vfs_find as *const c_void,
    ]
    .into_iter()
    .all(|symbol| image(symbol) == Some(executable))
    {
        return false;
    }

    // SAFETY: the names are static C strings. Compare only symbol addresses
    // from the already loaded libc and release the loader's handle afterward.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe {
        let handle = libc::dlopen(
            c"libc.so.6".as_ptr(),
            libc::RTLD_LAZY | libc::RTLD_LOCAL | libc::RTLD_NOLOAD,
        );
        if handle.is_null() {
            return false;
        }
        let matches = [
            (c"malloc", libc::malloc as *const c_void),
            (c"free", libc::free as *const c_void),
            (c"realloc", libc::realloc as *const c_void),
            (
                c"malloc_usable_size",
                libc::malloc_usable_size as *const c_void,
            ),
        ]
        .into_iter()
        .all(|(name, address)| libc::dlsym(handle, name.as_ptr()).cast_const() == address);
        libc::dlclose(handle) == 0 && matches
    }
}
