use camino::Utf8PathBuf;
use miette::{IntoDiagnostic, Result};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::process::Command;

use crate::state::layout::Layout;

const TASK_NAME_PREFIX: &str = "LedgerfulNightlyIndex";
const SCHEDULE_HOUR: &str = "02:00";
const CRON_SCHEDULE: &str = "0 2 * * *";

/// Build a repo-scoped scheduled-task name so multiple repos on the same
/// machine do not collide (H2 from Claude cross-review). The name is the
/// fixed prefix plus a short, stable hash of the absolute repo root path,
/// keeping it a valid `schtasks` `/TN` value (alphanumeric + a few safe
/// chars) and a valid cron comment token.
///
/// Only referenced from `#[cfg(windows)]` code paths and cross-platform
/// tests, so kept non-gated with `#[allow(dead_code)]` to avoid breaking
/// the Unix clippy CI gate (matched the `cron_marker` treatment).
#[allow(dead_code)]
fn task_name(root: &Utf8PathBuf) -> String {
    let hash = short_hash(root.as_str());
    format!("{}-{}", TASK_NAME_PREFIX, hash)
}

/// Marker inserted as a trailing cron comment so `install_cron_line` /
/// `remove_cron_line` can identify *this repo's* line without touching
/// other repos' lines (H2). The marker is a `# ledgerful-nightly:<hash>`
/// comment placed on the line above the schedule line.
///
/// Only referenced from `#[cfg(unix)]` code paths, but kept non-gated so
/// cross-platform tests can assert repo-scoping behavior on every platform.
#[allow(dead_code)]
fn cron_marker(root: &Utf8PathBuf) -> String {
    let hash = short_hash(root.as_str());
    format!("# ledgerful-nightly:{}", hash)
}

/// Short, stable, lowercase hex hash of a string (first 8 chars of SHA-1).
/// Uses a tiny FNV-1a 64-bit hash to avoid pulling a crypto crate; the goal
/// is a stable per-repo discriminator, not cryptographic strength.
fn short_hash(input: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in input.as_bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
}

/// Top-level subcommands for `ledgerful schedule`.
#[derive(clap::Subcommand, Debug)]
pub enum ScheduleSubcommands {
    /// Install or uninstall a nightly `git fetch` + `ledgerful index --analyze-graph` task
    SetupNightly {
        /// Print the scheduler invocation without registering or modifying anything
        #[arg(long)]
        dry_run: bool,
        /// Remove the scheduled task instead of installing it
        #[arg(long)]
        uninstall: bool,
    },
    /// Run the nightly sequence directly (`git fetch` then `ledgerful index --analyze-graph`)
    RunNightly,
}

fn get_repo_root() -> Result<Utf8PathBuf> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let discovered = gix::discover(&current_dir).into_diagnostic()?;
    let root = discovered
        .workdir()
        .ok_or_else(|| miette::miette!("Failed to find work directory for repository"))?;
    Utf8PathBuf::from_path_buf(root.to_path_buf())
        .map_err(|_| miette::miette!("Repository root is not valid UTF-8"))
}

fn resolve_ledgerful_binary() -> Result<Utf8PathBuf> {
    let current_exe = env::current_exe().into_diagnostic()?;
    Utf8PathBuf::from_path_buf(current_exe)
        .map_err(|_| miette::miette!("Current executable path is not valid UTF-8"))
}

fn get_log_path(layout: &Layout) -> Utf8PathBuf {
    layout
        .root
        .join(".ledgerful")
        .join("logs")
        .join("nightly.log")
}

pub fn execute_setup_nightly(dry_run: bool, uninstall: bool) -> Result<()> {
    let root = get_repo_root()?;
    let layout = Layout::new(root.clone());
    let log_dir = layout.root.join(".ledgerful").join("logs");
    if !dry_run {
        fs::create_dir_all(log_dir.as_std_path()).into_diagnostic()?;
    }

    let binary = resolve_ledgerful_binary()?;

    match env::consts::OS {
        "windows" => setup_windows(&root, &binary, dry_run, uninstall),
        "macos" | "linux" => setup_unix(&root, &binary, dry_run, uninstall),
        other => Err(miette::miette!(
            "OS '{}' is not supported by schedule setup-nightly",
            other
        )),
    }
}

#[cfg(windows)]
fn setup_windows(
    root: &Utf8PathBuf,
    binary: &Utf8PathBuf,
    dry_run: bool,
    uninstall: bool,
) -> Result<()> {
    let log_path = root.join(".ledgerful").join("logs").join("nightly.log");
    let task_name = task_name(root);

    if uninstall {
        let args = ["/Delete", "/TN", task_name.as_str(), "/F"];
        if dry_run {
            println!("DRY-RUN: schtasks.exe {}", args.join(" "));
            return Ok(());
        }
        return run_schtasks(&args);
    }

    // Route through the pure syntax helper so production and tests share one
    // code path (M3 from Claude cross-review).
    let arg_strings = windows_schtasks_args(binary, &log_path, &task_name);
    let args: Vec<&str> = arg_strings.iter().map(|s| s.as_str()).collect();
    let task_command = format!("\"{}\" schedule run-nightly >\"{}\" 2>&1", binary, log_path);

    if dry_run {
        println!("DRY-RUN: schtasks.exe {}", args.join(" "));
        println!("Task command: {}", task_command);
        return Ok(());
    }

    run_schtasks(&args)
}

#[cfg(not(windows))]
fn setup_windows(
    _root: &Utf8PathBuf,
    _binary: &Utf8PathBuf,
    _dry_run: bool,
    _uninstall: bool,
) -> Result<()> {
    Err(miette::miette!(
        "Windows scheduler setup is only available on Windows"
    ))
}

#[cfg(windows)]
fn run_schtasks(args: &[&str]) -> Result<()> {
    let status = Command::new("schtasks.exe")
        .args(args)
        .status()
        .into_diagnostic()?;
    if status.success() {
        Ok(())
    } else {
        Err(miette::miette!(
            "schtasks.exe exited with status {}",
            status.code().unwrap_or(-1)
        ))
    }
}

#[cfg(not(unix))]
fn setup_unix(
    _root: &Utf8PathBuf,
    _binary: &Utf8PathBuf,
    _dry_run: bool,
    _uninstall: bool,
) -> Result<()> {
    Err(miette::miette!(
        "Unix scheduler setup is only available on Unix"
    ))
}

#[cfg(unix)]
fn setup_unix(
    root: &Utf8PathBuf,
    binary: &Utf8PathBuf,
    dry_run: bool,
    uninstall: bool,
) -> Result<()> {
    let log_path = root.join(".ledgerful").join("logs").join("nightly.log");
    let marker = cron_marker(root);
    // Route through the pure syntax helper so production and tests share one
    // code path (M3 from Claude cross-review).
    let cron_line = unix_cron_line(binary, root, &log_path, &marker);

    if uninstall {
        return remove_cron_line(&marker, &cron_line, dry_run);
    }

    if dry_run {
        println!("DRY-RUN: crontab entry:");
        println!("{}", marker);
        println!("{}", cron_line);
        return Ok(());
    }

    install_cron_line(&marker, &cron_line)
}

#[cfg(unix)]
fn get_current_crontab() -> Result<String> {
    let output = Command::new("crontab")
        .arg("-l")
        .output()
        .into_diagnostic()?;
    // `crontab -l` returns exit code 1 when the user has no crontab yet.
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(miette::miette!(
            "crontab -l failed with status {}",
            output.status.code().unwrap_or(-1)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(unix)]
fn install_cron_line(marker: &str, new_entry: &str) -> Result<()> {
    let existing = get_current_crontab()?;
    // Drop this repo's prior marker+line (if any) without touching other
    // repos' lines. The marker is repo-scoped (H2 from Claude cross-review).
    let mut lines: Vec<String> = Vec::new();
    let mut skip_next = false;
    for line in existing.lines() {
        if line.trim() == marker {
            // Skip the marker and the schedule line that follows it.
            skip_next = true;
            continue;
        }
        if skip_next {
            skip_next = false;
            continue;
        }
        lines.push(line.to_string());
    }
    // Append the new marker + schedule line as a single block.
    for piece in new_entry.split('\n') {
        lines.push(piece.to_string());
    }

    let mut crontab = String::new();
    for line in lines {
        writeln!(crontab, "{}", line).into_diagnostic()?;
    }

    let mut child = Command::new("crontab")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .into_diagnostic()?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin.write_all(crontab.as_bytes()).into_diagnostic()?;
    }
    let status = child.wait().into_diagnostic()?;
    if status.success() {
        Ok(())
    } else {
        Err(miette::miette!(
            "crontab install failed with status {}",
            status.code().unwrap_or(-1)
        ))
    }
}

#[cfg(unix)]
fn remove_cron_line(marker: &str, _new_entry: &str, dry_run: bool) -> Result<()> {
    let existing = get_current_crontab()?;
    // Remove only this repo's marker+line pair (H2).
    let mut lines: Vec<String> = Vec::new();
    let mut skip_next = false;
    for line in existing.lines() {
        if line.trim() == marker {
            skip_next = true;
            continue;
        }
        if skip_next {
            skip_next = false;
            continue;
        }
        lines.push(line.to_string());
    }

    if dry_run {
        println!("DRY-RUN: crontab would be:");
        for line in &lines {
            println!("{}", line);
        }
        return Ok(());
    }

    let mut crontab = String::new();
    for line in lines {
        writeln!(crontab, "{}", line).into_diagnostic()?;
    }

    let mut child = Command::new("crontab")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .into_diagnostic()?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin.write_all(crontab.as_bytes()).into_diagnostic()?;
    }
    let status = child.wait().into_diagnostic()?;
    if status.success() {
        Ok(())
    } else {
        Err(miette::miette!(
            "crontab remove failed with status {}",
            status.code().unwrap_or(-1)
        ))
    }
}

pub fn execute_run_nightly() -> Result<()> {
    let root = get_repo_root()?;
    let layout = Layout::new(root);
    let log_path = get_log_path(&layout);
    let log_dir = log_path
        .parent()
        .ok_or_else(|| miette::miette!("Log path has no parent directory"))?
        .to_path_buf();
    fs::create_dir_all(log_dir.as_std_path()).into_diagnostic()?;

    append_log(&log_path, "--- Nightly run started ---").into_diagnostic()?;

    // 1. git fetch
    let fetch_status = Command::new("git")
        .args(["fetch"])
        .current_dir(layout.root.as_std_path())
        .status()
        .into_diagnostic()?;
    append_log(
        &log_path,
        &format!(
            "git fetch finished with status {}",
            fetch_status.code().unwrap_or(-1)
        ),
    )
    .into_diagnostic()?;

    // 2. ledgerful index --analyze-graph
    let index_status = Command::new(resolve_ledgerful_binary()?.as_std_path())
        .args(["index", "--analyze-graph"])
        .current_dir(layout.root.as_std_path())
        .status()
        .into_diagnostic()?;
    append_log(
        &log_path,
        &format!(
            "ledgerful index --analyze-graph finished with status {}",
            index_status.code().unwrap_or(-1)
        ),
    )
    .into_diagnostic()?;

    append_log(&log_path, "--- Nightly run finished ---").into_diagnostic()?;

    if fetch_status.success() && index_status.success() {
        Ok(())
    } else {
        Err(miette::miette!(
            "Nightly sequence failed: git fetch {:?}, index {:?}",
            fetch_status.code(),
            index_status.code()
        ))
    }
}

fn append_log(path: &Utf8PathBuf, message: &str) -> std::io::Result<()> {
    use std::io::Write;
    let timestamp = chrono::Local::now().to_rfc3339();
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.as_std_path())?;
    let mut buf = String::with_capacity(timestamp.len() + message.len() + 5);
    let _ = write!(buf, "[{}] {}", timestamp, message);
    writeln!(file, "{}", buf)
}

// ── Pure syntax helpers for deterministic dry-run testing ────────────────────

/// Build the `schtasks /Create` argument vector for the current repo.
///
/// This is kept pure (no OS calls, no env beyond the inputs) so tests can
/// assert the generated syntax on every platform. The `task_name` is
/// repo-scoped by the caller (see `task_name(root)`) so multiple repos on
/// the same machine do not collide.
pub fn windows_schtasks_args(
    binary: &Utf8PathBuf,
    log_path: &Utf8PathBuf,
    task_name: &str,
) -> Vec<String> {
    let task_command = format!("\"{}\" schedule run-nightly >\"{}\" 2>&1", binary, log_path);
    vec![
        "/Create".to_string(),
        "/TN".to_string(),
        task_name.to_string(),
        "/TR".to_string(),
        task_command,
        "/SC".to_string(),
        "DAILY".to_string(),
        "/ST".to_string(),
        SCHEDULE_HOUR.to_string(),
        "/F".to_string(),
    ]
}

/// Build the Unix crontab line for the current repo. The `marker` is a
/// repo-scoped comment (`# ledgerful-nightly:<hash>`) placed on the line
/// above the schedule line so `install_cron_line` / `remove_cron_line` can
/// identify *this repo's* entry without touching other repos' lines.
pub fn unix_cron_line(
    binary: &Utf8PathBuf,
    root: &Utf8PathBuf,
    log_path: &Utf8PathBuf,
    marker: &str,
) -> String {
    format!(
        "{}\n{} cd {} && \"{}\" schedule run-nightly >>\"{}\" 2>&1",
        marker, CRON_SCHEDULE, root, binary, log_path
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use tempfile::tempdir;

    #[test]
    fn test_windows_schtasks_args_syntax() {
        let tmp = tempdir().unwrap();
        let binary = Utf8PathBuf::from_path_buf(tmp.path().join("ledgerful.exe")).unwrap();
        let log = Utf8PathBuf::from_path_buf(
            tmp.path()
                .join(".ledgerful")
                .join("logs")
                .join("nightly.log"),
        )
        .unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let task_name = task_name(&root);
        let args = windows_schtasks_args(&binary, &log, &task_name);

        assert_eq!(args[0], "/Create");
        assert_eq!(args[1], "/TN");
        assert_eq!(args[2], task_name);
        assert!(args[2].starts_with("LedgerfulNightlyIndex-"));
        assert_eq!(args[3], "/TR");
        assert!(args[4].contains("ledgerful.exe\" schedule run-nightly"));
        assert!(args[4].contains("nightly.log"));
        assert_eq!(args[5], "/SC");
        assert_eq!(args[6], "DAILY");
        assert_eq!(args[7], "/ST");
        assert_eq!(args[8], "02:00");
        assert_eq!(args[9], "/F");
    }

    #[test]
    fn test_unix_cron_line_syntax() {
        let tmp = tempdir().unwrap();
        let binary = Utf8PathBuf::from_path_buf(tmp.path().join("ledgerful")).unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let log = Utf8PathBuf::from_path_buf(
            tmp.path()
                .join(".ledgerful")
                .join("logs")
                .join("nightly.log"),
        )
        .unwrap();
        let marker = cron_marker(&root);
        let entry = unix_cron_line(&binary, &root, &log, &marker);

        // The entry is a marker comment followed by the schedule line.
        assert!(entry.starts_with(&marker));
        assert!(marker.starts_with("# ledgerful-nightly:"));
        let schedule_line = entry.lines().nth(1).expect("schedule line");
        assert!(schedule_line.starts_with("0 2 * * * "));
        assert!(schedule_line.contains("cd "));
        assert!(
            schedule_line.contains("ledgerful\" schedule run-nightly"),
            "got: {}",
            schedule_line
        );
        assert!(schedule_line.contains(">>\""));
        assert!(schedule_line.contains("2>&1"));
    }

    #[test]
    fn test_task_name_is_repo_scoped_and_stable() {
        let root_a = Utf8PathBuf::from("/repos/project-a");
        let root_b = Utf8PathBuf::from("/repos/project-b");
        let name_a = task_name(&root_a);
        let name_b = task_name(&root_b);
        assert_ne!(
            name_a, name_b,
            "different repos must produce different task names (H2)"
        );
        assert_eq!(
            task_name(&root_a),
            task_name(&root_a),
            "same repo must produce a stable task name"
        );
        assert!(name_a.starts_with("LedgerfulNightlyIndex-"));
    }

    #[test]
    fn test_cron_marker_is_repo_scoped_and_stable() {
        let root_a = Utf8PathBuf::from("/repos/project-a");
        let root_b = Utf8PathBuf::from("/repos/project-b");
        let marker_a = cron_marker(&root_a);
        let marker_b = cron_marker(&root_b);
        assert_ne!(marker_a, marker_b, "different repos need different markers");
        assert_eq!(cron_marker(&root_a), cron_marker(&root_a));
        assert!(marker_a.starts_with("# ledgerful-nightly:"));
    }
}
