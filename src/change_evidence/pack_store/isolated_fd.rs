//! Bounded helper primitives for private Linux file ownership.

use std::{fs::File, os::fd::IntoRawFd};

pub(super) const STACK: usize = 256 * 1024;
pub(super) const MAX_ANCHOR: i32 = 4095;

#[derive(Clone, Copy, Debug)]
pub(super) enum GuardError {
    Unavailable,
    Uninitialized,
    Busy,
    Deadline,
    Budget,
}

pub(super) fn guard_work_error(error: &str) -> GuardError {
    if error == "deadline" {
        GuardError::Deadline
    } else {
        GuardError::Budget
    }
}

pub(super) fn decimal(mut n: u64, out: &mut [u8; 20]) -> Option<&[u8]> {
    let mut at = out.len();
    loop {
        at = at.checked_sub(1)?;
        out[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            return Some(&out[at..]);
        }
    }
}
pub(super) fn task_fd_path(tid: u64, out: &mut [u8; 64]) -> Option<usize> {
    let mut n = [0u8; 20];
    let digits = decimal(tid, &mut n)?;
    let prefix = b"task/";
    let suffix = b"/fd";
    let end = prefix
        .len()
        .checked_add(digits.len())?
        .checked_add(suffix.len())?;
    out.get_mut(..prefix.len())?.copy_from_slice(prefix);
    out.get_mut(prefix.len()..prefix.len() + digits.len())?
        .copy_from_slice(digits);
    out.get_mut(prefix.len() + digits.len()..end)?
        .copy_from_slice(suffix);
    Some(end)
}

pub(super) fn close_private(file: File) -> bool {
    let fd = file.into_raw_fd() as u32;
    // SAFETY: this number belongs solely to the isolated helper table; no
    // further open is attempted before the close result is observed.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { nix::libc::syscall(nix::libc::SYS_close_range, fd, fd, 0u32) == 0 }
}
