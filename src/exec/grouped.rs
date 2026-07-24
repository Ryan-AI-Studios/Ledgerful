//! Shared process-group / Job Object spawn + bounded wait + kill.
//!
//! Extracted from the proven federation pattern (`federated/scanner.rs`) so
//! verify, validators, and federation share one implementation. On Unix the
//! child is a process-group leader; on Windows it runs in a Job Object. Timeout
//! kills the whole group so grandchildren do not leak.

use process_wrap::std::*;
use std::io;
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};
use thiserror::Error;

/// Grace period after a successful wait to best-effort reap already-exited
/// grandchildren without risking a hang on a stuck one.
const REAP_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Error)]
pub enum GroupedProcessError {
    #[error("failed to spawn process: {0}")]
    Spawn(#[source] io::Error),
    #[error("failed waiting for process: {0}")]
    Wait(#[source] io::Error),
    #[error("process timed out after {timeout:?}")]
    Timeout {
        timeout: Duration,
        /// Kill/reap diagnostics when the group kill path partially fails.
        detail: String,
    },
}

/// Spawn `command` as a process-group/job leader and wait up to `timeout`.
///
/// On success, performs a short non-blocking reap grace. On timeout, kills the
/// whole group/job and reaps.
pub fn spawn_wait_grouped(
    command: Command,
    timeout: Duration,
) -> Result<ExitStatus, GroupedProcessError> {
    let mut wrapped = CommandWrap::from(command);
    #[cfg(unix)]
    {
        wrapped.wrap(ProcessGroup::leader());
    }
    #[cfg(windows)]
    {
        wrapped.wrap(JobObject);
    }

    let mut child = wrapped.spawn().map_err(GroupedProcessError::Spawn)?;

    let status = match wait_timeout::ChildExt::wait_timeout(
        // SAFETY: `inner_child_mut()` returns a reference to the always-valid
        // inner `std::process::Child` held by the `process-wrap` wrapper. The
        // child was just spawned and not yet moved or consumed, so the inner
        // reference is valid for the duration of this `wait_timeout` call.
        // NOTE: `wait_timeout` only reaps the IMMEDIATE child. The wrapper's
        // own `wait()` is what reaps the whole process group (Unix:
        // `waitpid(-pgid)`) or Job Object (Windows: `wait_on_job`).
        // Legitimate: process-wrap API requires unsafe to access inner Child.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        unsafe { child.inner_child_mut() },
        timeout,
    )
    .map_err(GroupedProcessError::Wait)?
    {
        Some(status) => {
            // The immediate child exited. Grandchildren may still be running.
            // Use a BOUNDED non-blocking reap loop (`try_wait`) for a short
            // grace period. The wrapper's blocking `wait()` would loop until
            // ALL group members exit — right on the timeout path (after kill)
            // but would hang on the success path if a grandchild is stuck.
            let reap_start = Instant::now();
            while reap_start.elapsed() < REAP_GRACE {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => break,
                }
            }
            status
        }
        None => {
            let kill_err = child.start_kill().err();
            let reap_err = child.wait().err();
            let mut detail = String::new();
            if let Some(e) = kill_err {
                detail.push_str(&format!("process-group kill failed ({e:?}); "));
            }
            if let Some(e) = reap_err {
                detail.push_str(&format!("process reap failed ({e:?})"));
            }
            return Err(GroupedProcessError::Timeout { timeout, detail });
        }
    };

    Ok(status)
}

/// Spawn with piped stdout/stderr, wait with timeout under a process group,
/// and return exit status plus captured output. Used by verify boundary and
/// validators so timeout kills the whole process tree.
pub fn spawn_wait_grouped_captured(
    mut command: Command,
    timeout: Duration,
    max_output_bytes: usize,
) -> Result<CapturedGrouped, GroupedProcessError> {
    use std::thread;

    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    let mut wrapped = CommandWrap::from(command);
    #[cfg(unix)]
    {
        wrapped.wrap(ProcessGroup::leader());
    }
    #[cfg(windows)]
    {
        wrapped.wrap(JobObject);
    }

    let mut child = wrapped.spawn().map_err(GroupedProcessError::Spawn)?;

    // Take pipes from the inner child so concurrent readers can drain them
    // without deadlocking on a full pipe buffer.
    // Legitimate: process-wrap API requires unsafe to access inner Child.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let (stdout_handle, stderr_handle) = unsafe {
        let inner = child.inner_child_mut();
        let stdout = inner
            .stdout
            .take()
            .map(|stdout| thread::spawn(move || read_capped(stdout, max_output_bytes)));
        let stderr = inner
            .stderr
            .take()
            .map(|stderr| thread::spawn(move || read_capped(stderr, max_output_bytes)));
        (stdout, stderr)
    };

    let status = match wait_timeout::ChildExt::wait_timeout(
        // Legitimate: process-wrap API requires unsafe to access inner Child.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        unsafe { child.inner_child_mut() },
        timeout,
    )
    .map_err(GroupedProcessError::Wait)?
    {
        Some(status) => {
            let reap_start = Instant::now();
            while reap_start.elapsed() < REAP_GRACE {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => break,
                }
            }
            status
        }
        None => {
            let kill_err = child.start_kill().err();
            let reap_err = child.wait().err();
            let _ = join_reader(stdout_handle);
            let _ = join_reader(stderr_handle);
            let mut detail = String::new();
            if let Some(e) = kill_err {
                detail.push_str(&format!("process-group kill failed ({e:?}); "));
            }
            if let Some(e) = reap_err {
                detail.push_str(&format!("process reap failed ({e:?})"));
            }
            return Err(GroupedProcessError::Timeout { timeout, detail });
        }
    };

    let stdout = join_reader(stdout_handle);
    let stderr = join_reader(stderr_handle);
    let truncated = stdout.truncated || stderr.truncated;

    Ok(CapturedGrouped {
        status,
        stdout: stdout.output,
        stderr: stderr.output,
        truncated,
    })
}

#[derive(Debug)]
pub struct CapturedGrouped {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

#[derive(Debug, Default)]
struct CapturedOutput {
    output: String,
    truncated: bool,
}

fn read_capped(mut reader: impl std::io::Read, max_output_bytes: usize) -> CapturedOutput {
    let mut output = Vec::new();
    let mut truncated = false;
    let mut buffer = [0_u8; 8192];

    loop {
        let bytes_read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(bytes_read) => bytes_read,
            Err(_) => break,
        };

        let remaining = max_output_bytes.saturating_sub(output.len());
        if remaining > 0 {
            let bytes_to_store = bytes_read.min(remaining);
            output.extend_from_slice(&buffer[..bytes_to_store]);
        }
        if bytes_read > remaining {
            truncated = true;
        }
    }

    CapturedOutput {
        output: String::from_utf8_lossy(&output).to_string(),
        truncated,
    }
}

fn join_reader(reader: Option<std::thread::JoinHandle<CapturedOutput>>) -> CapturedOutput {
    reader
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn grouped_fast_success() {
        let cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/c", "exit", "0"]);
            c
        } else {
            Command::new("true")
        };
        let status = spawn_wait_grouped(cmd, Duration::from_secs(5)).expect("spawn");
        assert!(status.success());
    }

    #[test]
    fn grouped_timeout_kills() {
        let cmd = if cfg!(windows) {
            let mut c = Command::new("ping");
            c.args(["-n", "30", "127.0.0.1"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg("30");
            c
        };
        let start = Instant::now();
        let err = spawn_wait_grouped(cmd, Duration::from_millis(400)).unwrap_err();
        assert!(
            matches!(err, GroupedProcessError::Timeout { .. }),
            "expected timeout, got {err:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timeout path too slow: {:?}",
            start.elapsed()
        );
    }

    /// Best-effort grandchild reaping: spawn a shell that backgrounds a long
    /// sleep, then timeout-kill the group. Platform-gated; OS scheduling may
    /// leave orphans briefly, so we only assert the parent path returns Timeout.
    #[test]
    fn grouped_timeout_on_shell_with_child() {
        let cmd = if cfg!(windows) {
            // cmd starts ping as a child; Job Object should cover it.
            let mut c = Command::new("cmd");
            c.args(["/C", "ping -n 60 127.0.0.1"]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", "sleep 60"]);
            c
        };
        let err = spawn_wait_grouped(cmd, Duration::from_millis(500)).unwrap_err();
        assert!(matches!(err, GroupedProcessError::Timeout { .. }));
    }
}
