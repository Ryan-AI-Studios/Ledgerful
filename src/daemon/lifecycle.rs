use miette::{IntoDiagnostic, Result, miette};
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use sysinfo::{Pid, System};
use tracing::{info, warn};

pub struct DaemonLifecycle {
    pid_file: PathBuf,
    parent_pid: Option<u32>,
}

impl DaemonLifecycle {
    pub fn new(root: &Path, parent_pid: Option<u32>) -> Self {
        let pid_file = root.join(".ledgerful").join("daemon.pid");
        let parent_pid = parent_pid.or_else(current_parent_pid);
        Self {
            pid_file,
            parent_pid,
        }
    }

    pub fn setup(&self) -> Result<()> {
        if let Some(parent) = self.pid_file.parent() {
            fs::create_dir_all(parent).into_diagnostic()?;
        }

        if self.pid_file.exists() {
            let content = fs::read_to_string(&self.pid_file).into_diagnostic()?;
            if let Ok(pid) = content.trim().parse::<u32>() {
                if self.is_process_alive(pid) {
                    return Err(miette!(
                        "Daemon is already running (PID: {}). Stop it first or delete {}",
                        pid,
                        self.pid_file.display()
                    ));
                } else {
                    warn!("Stale PID file found (PID: {}). Cleaning up.", pid);
                    self.cleanup()?;
                }
            }
        }

        let current_pid = process::id();
        fs::write(&self.pid_file, current_pid.to_string()).into_diagnostic()?;
        info!("Daemon started with PID: {}", current_pid);
        Ok(())
    }

    pub fn cleanup(&self) -> Result<()> {
        if self.pid_file.exists() {
            fs::remove_file(&self.pid_file).into_diagnostic()?;
            info!("Cleaned up PID file: {}", self.pid_file.display());
        }
        Ok(())
    }

    pub fn is_process_alive(&self, pid: u32) -> bool {
        let mut sys = System::new_all();
        sys.refresh_all();
        sys.process(Pid::from(pid as usize)).is_some()
    }

    pub fn check_parent_alive(&self) -> bool {
        if let Some(ppid) = self.parent_pid {
            self.is_process_alive(ppid)
        } else {
            true
        }
    }
}

fn current_parent_pid() -> Option<u32> {
    let mut sys = System::new_all();
    sys.refresh_all();
    let current = sys.process(Pid::from(process::id() as usize))?;
    current.parent().map(|pid| pid.as_u32())
}
