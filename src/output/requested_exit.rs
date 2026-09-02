//! Process exit-code request for CLI paths that return `Result<()>`.
//!
//! First-write-wins. `main` takes the code once after a miette error so
//! would-block paths can exit 2 without a full `ExitCode` refactor (0072
//! signatures; 0223 pending-entity collision).

use std::sync::atomic::{AtomicI32, Ordering};

static REQUESTED: AtomicI32 = AtomicI32::new(0);

/// Record a non-zero exit code for the CLI process (first-write-wins).
pub fn request_exit(code: i32) {
    let _ = REQUESTED.compare_exchange(0, code, Ordering::SeqCst, Ordering::SeqCst);
}

/// Take the requested exit code (if any) and reset. Used by `main`.
pub fn take_requested_exit_code() -> Option<i32> {
    let c = REQUESTED.swap(0, Ordering::SeqCst);
    if c == 0 { None } else { Some(c) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_resets_and_first_write_wins() {
        let _ = take_requested_exit_code();
        request_exit(2);
        request_exit(1);
        assert_eq!(take_requested_exit_code(), Some(2));
        assert_eq!(take_requested_exit_code(), None);
    }
}
