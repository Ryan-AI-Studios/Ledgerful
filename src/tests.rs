use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

/// RAII CWD guard for unit / lib tests.
///
/// Holds a process-wide lock for the guard lifetime because `cargo test
/// --lib` is shared-process. `cargo nextest` is process-per-test, so the
/// lock is inert there. Poison is recovered via `into_inner`. This lock
/// is not shared with the integration `DirGuard` (different binaries)
/// and does not cover residual `CwdGuard` / unguarded test `set_current_dir`.
///
/// `DirGuard` is `!Send` (`MutexGuard`). Construct it on the thread that
/// holds it.
pub struct DirGuard {
    original: PathBuf,
    _lock: MutexGuard<'static, ()>,
}

fn dir_guard_cwd_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

impl DirGuard {
    pub fn new(dir: &Path) -> Self {
        let lock = dir_guard_cwd_lock()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();
        Self {
            original,
            _lock: lock,
        }
    }
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
    }
}

#[cfg(test)]
mod dir_guard_lock_tests {
    use super::DirGuard;
    use std::thread;
    use std::time::Duration;
    use tempfile::tempdir;

    /// Two threads + two tempdirs must not restore a deleted path as CWD.
    /// The RED case without the lock is not machine-checked (inherently racy).
    #[test]
    fn test_dir_guard_lib_two_threads_do_not_restore_deleted_tmp() {
        let a = tempdir().unwrap();
        let b = tempdir().unwrap();
        let pa = a.path().to_path_buf();
        let pb = b.path().to_path_buf();

        let t1 = thread::spawn(move || {
            let _guard = DirGuard::new(&pa);
            thread::sleep(Duration::from_millis(20));
        });
        let t2 = thread::spawn(move || {
            let _guard = DirGuard::new(&pb);
            thread::sleep(Duration::from_millis(20));
        });
        t1.join().expect("dir-guard thread A");
        t2.join().expect("dir-guard thread B");

        let cwd = std::env::current_dir().unwrap();
        assert!(
            cwd.exists(),
            "CWD after concurrent lib DirGuards must still exist: {}",
            cwd.display()
        );
    }
}
