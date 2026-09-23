//! Shared-process `DirGuard` race pin (0433).
//!
//! The RED case without the lock is not machine-checked (inherently racy).
//! A green nextest run is not this evidence — run under `cargo test`.

use crate::common::DirGuard;
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

#[test]
fn dir_guard_two_threads_do_not_restore_deleted_tmp() {
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
        "CWD after concurrent DirGuards must still exist: {}",
        cwd.display()
    );
}
