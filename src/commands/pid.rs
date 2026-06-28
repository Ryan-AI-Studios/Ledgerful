use camino::{Utf8Path, Utf8PathBuf};
use miette::{Result, miette};

/// A PID file guard that writes the current process id on creation and
/// removes the file when dropped.
pub struct PidFile {
    path: Utf8PathBuf,
}

impl PidFile {
    /// Write the current process id to `path` and return a guard.
    ///
    /// The file is created with mode `0600` on Unix and restricted ACLs on
    /// Windows so only the owner can read or write it.
    pub fn create(path: Utf8PathBuf) -> Result<Self> {
        let pid = std::process::id();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        #[cfg(unix)]
        {
            use std::fs::OpenOptions;
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;

            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(&path)
                .map_err(|e| miette!("Failed to open PID file {}: {}", path, e))?;
            write!(file, "{}", pid)
                .map_err(|e| miette!("Failed to write PID file {}: {}", path, e))?;
        }

        #[cfg(target_os = "windows")]
        {
            // Windows has no simple octal mode. The file lives inside the user-private
            // `.ledgerful/tmp` directory; rely on that directory ACL and restrict via
            // icacls best-effort.
            std::fs::write(&path, pid.to_string())
                .map_err(|e| miette!("Failed to write PID file {}: {}", path, e))?;
            let _ = restrict_file_acl(path.as_std_path());
        }

        #[cfg(not(any(unix, target_os = "windows")))]
        {
            std::fs::write(&path, pid.to_string())
                .map_err(|e| miette!("Failed to write PID file {}: {}", path, e))?;
        }

        Ok(Self { path })
    }

    /// Return true if a process with the given PID is still running and appears
    /// to belong to this executable.
    pub fn is_alive_and_ours(pid: u32) -> bool {
        Self::is_alive(pid) && Self::verify_is_our_process(pid)
    }

    /// Read the PID stored at `path`, if any.
    pub fn read(path: &Utf8Path) -> Result<Option<u32>> {
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(path)
            .map_err(|e| miette!("Failed to read PID file {}: {}", path, e))?;
        let pid: u32 = content
            .trim()
            .parse()
            .map_err(|e| miette!("Invalid PID in {}: {}", path, e))?;
        Ok(Some(pid))
    }

    /// Remove the PID file at `path`, ignoring errors.
    pub fn remove(path: &Utf8Path) {
        let _ = std::fs::remove_file(path);
    }

    /// Return true if a process with the given PID is still running.
    #[cfg(unix)]
    pub fn is_alive(pid: u32) -> bool {
        use nix::unistd::Pid;
        let pid = Pid::from_raw(pid.try_into().unwrap_or(i32::MAX));
        nix::sys::signal::kill(pid, None).is_ok()
    }

    /// Return true if a process with the given PID is still running.
    #[cfg(target_os = "windows")]
    pub fn is_alive(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, FALSE};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: OpenProcess accepts a valid PID and standard access rights.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) };
        if handle.is_null() {
            return false;
        }
        // SAFETY: handle was returned by OpenProcess and is non-null.
        unsafe {
            let _ = CloseHandle(handle);
        }
        true
    }

    /// Return true if a process with the given PID is still running.
    /// Fallback for platforms that are neither Unix nor Windows.
    #[cfg(not(any(unix, target_os = "windows")))]
    pub fn is_alive(_pid: u32) -> bool {
        false
    }

    /// Convert a signed exit code to the `u32` expected by `TerminateProcess`.
    #[cfg(target_os = "windows")]
    fn u32_exit_code(code: i32) -> u32 {
        code as u32
    }

    /// Kill the process with the given PID.
    #[cfg(unix)]
    pub fn kill(pid: u32) -> Result<()> {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        let pid_i32 = pid
            .try_into()
            .map_err(|_| miette!("PID {} exceeds platform limit", pid))?;
        kill(Pid::from_raw(pid_i32), Some(Signal::SIGKILL))
            .map_err(|e| miette!("Failed to kill process {}: {}", pid, e))?;
        Ok(())
    }

    /// Kill the process with the given PID.
    #[cfg(target_os = "windows")]
    pub fn kill(pid: u32) -> Result<()> {
        use windows_sys::Win32::Foundation::ERROR_INVALID_PARAMETER;
        use windows_sys::Win32::Foundation::{CloseHandle, FALSE, GetLastError};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_TERMINATE, TerminateProcess,
        };

        // SAFETY: OpenProcess accepts a valid PID and standard access rights.
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE, FALSE, pid) };
        if handle.is_null() {
            // SAFETY: GetLastError has no preconditions.
            let err = unsafe { GetLastError() };
            if err == ERROR_INVALID_PARAMETER {
                // Process already exited; not a hard failure.
                return Ok(());
            }
            return Err(miette!(
                "Failed to open process {} for termination (error {})",
                pid,
                err
            ));
        }

        // SAFETY: handle is non-null and was returned by OpenProcess with
        // PROCESS_TERMINATE access.
        let terminated = unsafe { TerminateProcess(handle, Self::u32_exit_code(1)) };
        let terminate_err = if terminated == 0 {
            // SAFETY: GetLastError has no preconditions.
            Some(unsafe { GetLastError() })
        } else {
            None
        };

        // SAFETY: handle was returned by OpenProcess and is non-null.
        unsafe {
            let _ = CloseHandle(handle);
        }

        if let Some(err) = terminate_err {
            return Err(miette!(
                "Failed to terminate process {} (error {})",
                pid,
                err
            ));
        }

        Ok(())
    }

    /// Fallback kill for unsupported platforms.
    #[cfg(not(any(unix, target_os = "windows")))]
    pub fn kill(pid: u32) -> Result<()> {
        Err(miette!("Cannot kill process {} on this platform", pid))
    }

    /// Verify that the process with the given PID belongs to this executable.
    #[cfg(target_os = "windows")]
    pub fn verify_is_our_process(pid: u32) -> bool {
        let Some(expected_image) = std::env::current_exe().ok().and_then(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_lowercase())
        }) else {
            return false;
        };

        let Ok(verify_output) = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/FO", "CSV", "/NH"])
            .output()
        else {
            return false;
        };
        let out_str = String::from_utf8_lossy(&verify_output.stdout);

        out_str.lines().any(|line| {
            let image = line
                .split(',')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('"');
            image.to_lowercase() == expected_image
        })
    }

    /// Verify that the process with the given PID belongs to this executable.
    ///
    /// Uses `/proc/<pid>/exe` for an exact path comparison rather than `ps`,
    /// since `ps -o comm=` truncates the command name to 15 characters on
    /// Linux (the kernel's `TASK_COMM_LEN` limit) -- long enough to break
    /// self-checks against `cargo test`'s hash-suffixed binary names.
    #[cfg(target_os = "linux")]
    pub fn verify_is_our_process(pid: u32) -> bool {
        let Ok(expected_exe) = std::env::current_exe() else {
            return false;
        };
        let Ok(actual_exe) = std::fs::read_link(format!("/proc/{}/exe", pid)) else {
            return false;
        };
        actual_exe == expected_exe
    }

    /// Verify that the process with the given PID belongs to this executable.
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    pub fn verify_is_our_process(pid: u32) -> bool {
        let Some(expected_image) = std::env::current_exe().ok().and_then(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_lowercase())
        }) else {
            return false;
        };

        let Ok(verify_output) = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
        else {
            return false;
        };
        let out_str = String::from_utf8_lossy(&verify_output.stdout);
        out_str.to_lowercase().contains(&expected_image)
    }
}

#[allow(dead_code)]
#[cfg(target_os = "windows")]
fn restrict_file_acl(path: &std::path::Path) -> Result<()> {
    use std::process::Command;

    // Remove inherited permissions and grant full control to the current user only.
    // "%USERNAME%" expands to the current user SID under icacls.
    let output = Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r", "%USERNAME%:F"])
        .output()
        .map_err(|e| miette!("Failed to run icacls on {}: {}", path.display(), e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(miette!("icacls failed for {}: {}", path.display(), stderr));
    }
    Ok(())
}

impl Drop for PidFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Stop a process identified by the PID file at `path` after verifying it
/// belongs to this executable.
pub fn stop(path: &Utf8Path) -> Result<()> {
    match PidFile::read(path)? {
        Some(pid) => {
            println!("Stopping server (PID {})...", pid);

            if !PidFile::verify_is_our_process(pid) {
                println!("Process {} is not this executable (may have exited).", pid);
                PidFile::remove(path);
                return Ok(());
            }

            if !PidFile::is_alive(pid) {
                println!("Process {} not found (already exited).", pid);
                PidFile::remove(path);
                return Ok(());
            }

            PidFile::kill(pid)?;
            println!("Server stopped.");
            PidFile::remove(path);
            Ok(())
        }
        None => {
            println!("No server PID file found. Server may not be running.");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::layout::Layout;
    use camino::Utf8Path;

    #[test]
    fn test_pid_file_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        let pid_path = layout.pid_file();

        // Clean slate
        PidFile::remove(&pid_path);
        assert!(PidFile::read(&pid_path).unwrap().is_none());

        // Write and read back
        let guard = PidFile::create(pid_path.clone()).unwrap();
        let pid = PidFile::read(&pid_path).unwrap();
        assert_eq!(pid, Some(std::process::id()));

        // Drop removes the file
        drop(guard);
        assert!(PidFile::read(&pid_path).unwrap().is_none());
    }

    #[test]
    fn test_stop_no_pid_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        let pid_path = layout.pid_file();
        PidFile::remove(&pid_path);
        // Should succeed gracefully when no server is running
        assert!(stop(&pid_path).is_ok());
    }

    #[test]
    fn test_pid_file_permissions_are_restricted() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        let pid_path = layout.pid_file();

        PidFile::remove(&pid_path);
        let _guard = PidFile::create(pid_path.clone()).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(&pid_path).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "PID file should be owner read/write only");
        }

        // On Windows we rely on the parent directory ACL and best-effort icacls.
        // The important invariant is that the file exists and contains a valid PID.
        assert!(pid_path.exists());
        assert_eq!(PidFile::read(&pid_path).unwrap(), Some(std::process::id()));
    }

    #[test]
    fn test_is_alive_and_ours_for_self() {
        let pid = std::process::id();
        assert!(PidFile::is_alive(pid));
        assert!(PidFile::verify_is_our_process(pid));
        assert!(PidFile::is_alive_and_ours(pid));
    }

    #[test]
    fn test_is_alive_for_nonexistent_pid() {
        // PID 0 is reserved/idle on both Unix and Windows and is never our process.
        assert!(!PidFile::is_alive_and_ours(0));
    }
}
