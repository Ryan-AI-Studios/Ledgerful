pub mod api;
pub mod auth;
pub mod error;
pub mod git_meta;
pub mod server;
pub mod spa_dir;
pub mod state;
pub mod types;

use crate::cli::args::{WebCommands, WebStartArgs};
use crate::commands::helpers::get_layout;
use crate::commands::pid::PidFile;
use crate::commands::web::auth::resolve_session_token;
use crate::commands::web::spa_dir::validate_spa_dir;
use crate::commands::web::state::AppState;
use camino::{Utf8Path, Utf8PathBuf};
use miette::{IntoDiagnostic, Result, miette};
use owo_colors::OwoColorize;
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;

const TOKEN_ENV_VAR: &str = "LEDGERFUL_WEB_TOKEN";
/// Comma-separated peer IPs required when `--allow-public` binds non-loopback.
const PEER_ALLOWLIST_ENV: &str = "LEDGERFUL_WEB_PEER_ALLOWLIST";

/// Dispatch a `ledgerful web` subcommand.
pub fn execute_web(command: WebCommands) -> Result<()> {
    match command {
        WebCommands::Start(args) => start_web(args),
        WebCommands::Stop => {
            let layout = get_layout()?;
            crate::commands::pid::stop(&layout.web_pid_file())
        }
        WebCommands::Status => print_web_status(),
    }
}

fn start_web(args: WebStartArgs) -> Result<()> {
    let layout = get_layout()?;

    if !layout.state_dir.exists() {
        return Err(miette!(
            "Ledgerful state directory not found: {}. Run 'ledgerful init' first.",
            layout.state_dir
        ));
    }

    let pid_path = layout.web_pid_file();

    if let Some(pid) = PidFile::read(&pid_path)? {
        if PidFile::is_alive_and_ours(pid) {
            return Err(miette!(
                "Another ledgerful web server is already running (PID {}). Use 'ledgerful web stop' first.",
                pid
            ));
        }
        PidFile::remove(&pid_path);
    }

    validate_bind(&args.bind, args.allow_public)?;
    let peer_allowlist = resolve_peer_allowlist(args.allow_public, &args.bind)?;
    let spa_dir = match args.spa_dir.as_ref() {
        Some(dir) => Some(validate_spa_dir(dir)?),
        None => None,
    };

    let env_token = match std::env::var(TOKEN_ENV_VAR) {
        Ok(v) => Some(v),
        Err(std::env::VarError::NotPresent) => None,
        // Non-Unicode env is treated as an explicit empty supply → refuse.
        Err(std::env::VarError::NotUnicode(_)) => Some(String::new()),
    };
    let resolution = resolve_session_token(args.token.clone(), env_token)?;
    let token = resolution.into_token();
    let base_url = format!("http://{}:{}/", args.bind, args.port);

    let token_file_path = if !args.print_token {
        Some(write_session_token_file(&layout, &token)?)
    } else {
        None
    };

    if args.background {
        return spawn_background_server(
            args,
            layout,
            &base_url,
            &token,
            token_file_path.as_deref(),
        );
    }

    let _pid_guard = PidFile::create(pid_path)?;
    let state = Arc::new(AppState::new(
        layout,
        token.clone(),
        spa_dir,
        peer_allowlist,
    ));

    println!("Starting ledgerful web at {}", base_url);
    emit_token_notice(&token, args.print_token, token_file_path.as_deref());

    if args.open {
        if let Err(e) = webbrowser::open(&base_url) {
            tracing::warn!("Failed to open browser: {}", e);
        }
        if !args.print_token
            && let Some(path) = token_file_path.as_ref()
        {
            println!(
                "Browser opened; session token is in {} (not printed).",
                path
            );
        }
    }
    println!(
        "Open {} in your browser and paste the auth token to sign in.",
        base_url
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .into_diagnostic()?;

    rt.block_on(run_server(args.bind, args.port, state))?;

    Ok(())
}

/// Format the operator-facing token notice (DoD-5). Pure so tests can assert
/// redaction without capturing process stdout.
///
/// When `print_token` is true the full token is included (legacy default).
/// When false, only the path is mentioned — the token value must never appear.
fn format_token_notice(token: &str, print_token: bool, token_file: Option<&Utf8Path>) -> String {
    if print_token {
        format!("Auth token: {token}")
    } else if let Some(path) = token_file {
        format!("Auth token written to {path}")
    } else {
        "Auth token suppressed (--print-token=false); no token file path available.".to_string()
    }
}

/// Print the token or only the file path (DoD-5).
fn emit_token_notice(token: &str, print_token: bool, token_file: Option<&Utf8Path>) {
    println!("{}", format_token_notice(token, print_token, token_file));
}

/// Write the session token to a user-only file under the state dir.
fn write_session_token_file(
    layout: &crate::state::layout::Layout,
    token: &str,
) -> Result<Utf8PathBuf> {
    layout.ensure_dir(&layout.state_dir)?;
    let path = layout.web_session_token_file();
    std::fs::write(path.as_std_path(), token.as_bytes())
        .map_err(|e| miette!("Failed to write session token file {}: {}", path, e))?;
    restrict_token_file_permissions(path.as_std_path())?;
    Ok(path)
}

/// Best-effort user-only permissions on the session token file.
///
/// Unix: `0600`. Windows: attempt a user-only DACL via `icacls`; if that fails,
/// document residual (file inherits directory ACL — same-user TCB; incidental
/// multi-user hardening is best-effort only).
fn restrict_token_file_permissions(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms)
            .map_err(|e| miette!("Failed to set 0600 on token file: {e}"))?;
    }
    #[cfg(windows)]
    {
        // Residual: full Windows ACL APIs are heavier than warranted for
        // incidental-leak defense. Prefer `icacls` to grant the current user
        // only; on failure we keep the file and warn (do not claim 0600).
        if let Ok(user) = std::env::var("USERNAME") {
            let path_str = path.to_string_lossy();
            let status = std::process::Command::new("icacls")
                .args([
                    path_str.as_ref(),
                    "/inheritance:r",
                    "/grant:r",
                    &format!("{user}:(R,W)"),
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            match status {
                Ok(s) if s.success() => {}
                _ => {
                    tracing::warn!(
                        "Could not tighten Windows ACL on session token file {}; \
                         file may inherit directory ACLs (same-user TCB residual)",
                        path.display()
                    );
                }
            }
        }
    }
    Ok(())
}

/// Resolve peer allowlist for public bind mode.
///
/// When `allow_public` is set and bind is non-loopback, require a non-empty
/// `LEDGERFUL_WEB_PEER_ALLOWLIST` of comma-separated IP addresses.
pub fn resolve_peer_allowlist(allow_public: bool, bind: &str) -> Result<Option<HashSet<IpAddr>>> {
    if !allow_public || is_loopback_bind(bind) {
        return Ok(None);
    }

    let raw = std::env::var(PEER_ALLOWLIST_ENV).map_err(|_| {
        miette!(
            "--allow-public with non-loopback bind requires {PEER_ALLOWLIST_ENV} \
             (comma-separated peer IPs). Example: set {PEER_ALLOWLIST_ENV}=203.0.113.10 \
             Public mode without an allowlist is refused (safe-by-default). \
             Fully-supported public Host/CORS rewrite is a future track."
        )
    })?;
    parse_peer_allowlist(&raw).map(Some)
}

/// Parse a comma-separated peer IP allowlist (pure; used by start + tests).
pub fn parse_peer_allowlist(raw: &str) -> Result<HashSet<IpAddr>> {
    let mut set = HashSet::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let ip: IpAddr = part.parse().map_err(|_| {
            miette!("Invalid IP in {PEER_ALLOWLIST_ENV}: '{part}'. Expected comma-separated IPs.")
        })?;
        set.insert(ip);
    }
    if set.is_empty() {
        return Err(miette!(
            "{PEER_ALLOWLIST_ENV} is empty. Provide at least one peer IP, or bind to loopback \
             without --allow-public."
        ));
    }
    Ok(set)
}

fn is_loopback_bind(bind: &str) -> bool {
    if bind == "127.0.0.1" || bind == "::1" || bind == "localhost" {
        return true;
    }
    if let Ok(addr) = bind.parse::<IpAddr>() {
        return addr.is_loopback();
    }
    false
}

#[cfg(unix)]
fn spawn_background_server(
    args: WebStartArgs,
    layout: crate::state::layout::Layout,
    url: &str,
    token: &str,
    token_file: Option<&Utf8Path>,
) -> Result<()> {
    println!("Starting ledgerful web in the background at {}", url);
    emit_token_notice(token, args.print_token, token_file);

    let pid_path = layout.web_pid_file();
    // Remove stale PID file so the child can create a fresh one.
    PidFile::remove(&pid_path);

    // Double-fork via nix::unistd::daemon. The current process exits; the
    // daemon continues as the server.
    // Legitimate: Unix double-fork to daemonize the web server process.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    match unsafe { nix::unistd::fork() } {
        Ok(nix::unistd::ForkResult::Parent { .. }) => {
            // Parent exits immediately after the child is launched. The child
            // will write its own PID file once it starts.
            std::process::exit(0);
        }
        Ok(nix::unistd::ForkResult::Child) => {
            // Detach from controlling terminal and continue as the daemon.
            let _ = nix::unistd::setsid();
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            match unsafe { nix::unistd::fork() } {
                Ok(nix::unistd::ForkResult::Parent { .. }) => {
                    std::process::exit(0);
                }
                Ok(nix::unistd::ForkResult::Child) => {
                    // SAFETY: single-threaded child process after double-fork,
                    // no other threads can observe the env mutation.
                    // Legitimate: pass auth token to daemonized child via env.
                    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
                    unsafe {
                        std::env::set_var(TOKEN_ENV_VAR, token);
                    }
                    let _ = std::fs::write(pid_path.as_std_path(), std::process::id().to_string());
                    run_server_blocking(args, layout);
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("Failed to daemonize: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Err(e) => Err(miette!("Failed to fork background server: {}", e)),
    }
}

#[cfg(unix)]
fn run_server_blocking(args: WebStartArgs, layout: crate::state::layout::Layout) {
    let env_token = match std::env::var(TOKEN_ENV_VAR) {
        Ok(v) => Some(v),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => Some(String::new()),
    };
    let token = match resolve_session_token(args.token.clone(), env_token) {
        Ok(r) => r.into_token(),
        Err(e) => {
            eprintln!("{e:?}");
            return;
        }
    };
    let spa_dir = match args.spa_dir.as_ref() {
        Some(dir) => match validate_spa_dir(dir) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("{e:?}");
                return;
            }
        },
        None => None,
    };
    let peer_allowlist = match resolve_peer_allowlist(args.allow_public, &args.bind) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e:?}");
            return;
        }
    };
    let state = Arc::new(AppState::new(layout, token, spa_dir, peer_allowlist));
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("Failed to build tokio runtime: {}", e);
            return;
        }
    };
    let _ = rt.block_on(run_server(args.bind, args.port, state));
}

#[cfg(target_os = "windows")]
fn spawn_background_server(
    args: WebStartArgs,
    layout: crate::state::layout::Layout,
    url: &str,
    token: &str,
    token_file: Option<&Utf8Path>,
) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
    use windows_sys::Win32::System::Threading::DETACHED_PROCESS;

    println!("Starting ledgerful web in the background at {}", url);
    emit_token_notice(token, args.print_token, token_file);

    let pid_path = layout.web_pid_file();
    PidFile::remove(&pid_path);

    // Legitimate: Windows re-exec of this binary as a detached web daemon.
    // nosemgrep: rust.lang.security.current-exe.current-exe
    let current_exe = std::env::current_exe()
        .into_diagnostic()
        .map_err(|e| miette!("Failed to locate current executable: {}", e))?;
    let mut command = std::process::Command::new(current_exe);
    command.arg("web").arg("start");
    command.arg("--port").arg(args.port.to_string());
    command.arg("--bind").arg(&args.bind);
    if let Some(spa_dir) = args.spa_dir {
        command.arg("--spa-dir").arg(spa_dir.as_str());
    }
    if args.allow_public {
        command.arg("--allow-public");
    }
    // Keep hidden --token for daemonize handoff (RT-W5); no per-spawn warning.
    command.arg("--token").arg(token);
    // Preserve print_token preference for the child (child will not re-print if false
    // and token already written by parent; still avoid double-print of the secret).
    if !args.print_token {
        command.arg("--print-token").arg("false");
    }
    command.env(TOKEN_ENV_VAR, token);
    command.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);

    let _child = command
        .spawn()
        .map_err(|e| miette!("Failed to spawn background server: {}", e))?;

    // The child process will create its own PID file once it starts. Do not
    // write it here; otherwise the child sees its own PID in the file and
    // thinks another server is already running.
    Ok(())
}

#[cfg(not(any(unix, target_os = "windows")))]
fn spawn_background_server(
    _args: WebStartArgs,
    _layout: crate::state::layout::Layout,
    _url: &str,
    _token: &str,
    _token_file: Option<&Utf8Path>,
) -> Result<()> {
    Err(miette!("Background mode is not supported on this platform"))
}

fn print_web_status() -> Result<()> {
    let layout = get_layout()?;
    let pid_path = layout.web_pid_file();

    match PidFile::read(&pid_path)? {
        Some(pid) if PidFile::is_alive_and_ours(pid) => {
            println!("ledgerful web is running (PID {})", pid);
            println!("PID file: {}", pid_path);
        }
        Some(pid) => {
            println!(
                "ledgerful web is not running (stale PID {} in {})",
                pid, pid_path
            );
        }
        None => {
            println!("ledgerful web is not running (no PID file at {})", pid_path);
        }
    }

    Ok(())
}

async fn run_server(bind: String, port: u16, state: Arc<AppState>) -> Result<()> {
    let router = server::router(state);
    server::serve(router, bind, port).await
}

fn validate_bind(bind: &str, allow_public: bool) -> Result<()> {
    if is_loopback_bind(bind) {
        return Ok(());
    }

    if allow_public {
        print_public_warning(bind);
        return Ok(());
    }

    Err(miette!(
        "Server must bind to a loopback address unless --allow-public is set. Got: {}",
        bind
    ))
}

fn print_public_warning(bind: &str) {
    let msg = format!(
        "WARNING: binding ledgerful web to non-loopback address {}",
        bind
    );
    let line: String = msg.chars().map(|_| '=').collect();

    eprintln!("{}", line.red());
    eprintln!("{}", msg.red());
    eprintln!(
        "{}",
        "This exposes the dashboard to anyone who can reach the bind address.".red()
    );
    eprintln!(
        "{}",
        format!(
            "Require {PEER_ALLOWLIST_ENV} (peer IPs). Host validation is a rebinding defense only, not a network ACL."
        )
        .red()
    );
    eprintln!(
        "{}",
        "Prefer LEDGERFUL_WEB_TOKEN / the session token file over shell history.".red()
    );
    eprintln!("{}", line.red());
}

#[cfg(test)]
mod tests {
    use super::*;

    mod env_guard {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/integration/common/env_guard.rs"
        ));
    }
    use env_guard::TempEnv;

    #[test]
    fn peer_allowlist_required_for_public_non_loopback() {
        // Pure parse tests — env-dependent refuse is covered by parse emptiness.
        let err = parse_peer_allowlist("").unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("empty") || msg.contains("PEER"), "{msg}");
    }

    #[test]
    fn peer_allowlist_parses_ips() {
        let set = parse_peer_allowlist("203.0.113.10, 2001:db8::1").unwrap();
        assert!(set.contains(&"203.0.113.10".parse().unwrap()));
        assert!(set.contains(&"2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn peer_allowlist_rejects_garbage() {
        let err = parse_peer_allowlist("not-an-ip").unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("Invalid") || msg.contains("IP"), "{msg}");
    }

    #[test]
    fn loopback_bind_detection() {
        assert!(is_loopback_bind("127.0.0.1"));
        assert!(is_loopback_bind("::1"));
        assert!(is_loopback_bind("localhost"));
        assert!(!is_loopback_bind("0.0.0.0"));
        assert!(!is_loopback_bind("192.168.1.1"));
    }

    #[test]
    #[serial_test::serial(env)]
    fn public_non_loopback_without_allowlist_refuses() {
        // Isolate process env so a developer/CI LEDGERFUL_WEB_PEER_ALLOWLIST
        // cannot turn this refuse path into a false pass.
        let _clear = TempEnv::remove(PEER_ALLOWLIST_ENV);
        // Combined with DoD-1 (blank token refuse), the 0.0.0.0 + Host:127.0.0.1
        // + blank-token footgun is closed: public bind needs allowlist, blank
        // token never starts.
        assert!(
            resolve_peer_allowlist(true, "0.0.0.0").is_err(),
            "public bind without allowlist env must refuse"
        );
        assert!(resolve_peer_allowlist(false, "0.0.0.0").is_ok());
        assert!(resolve_peer_allowlist(true, "127.0.0.1").is_ok());
    }

    #[test]
    #[serial_test::serial(env)]
    fn public_non_loopback_with_allowlist_accepts() {
        let _set = TempEnv::set(PEER_ALLOWLIST_ENV, "203.0.113.10");
        let set = resolve_peer_allowlist(true, "0.0.0.0").unwrap();
        let set = set.expect("allowlist present");
        assert!(set.contains(&"203.0.113.10".parse().unwrap()));
    }

    #[test]
    fn write_session_token_file_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let layout =
            crate::state::layout::Layout::new(camino::Utf8Path::from_path(tmp.path()).unwrap());
        std::fs::create_dir_all(layout.state_dir.as_std_path()).unwrap();
        let token = "a".repeat(64);
        let path = write_session_token_file(&layout, &token).unwrap();
        assert!(path.as_str().ends_with("web-session-token"));
        let read = std::fs::read_to_string(path.as_std_path()).unwrap();
        assert_eq!(read, token);
    }

    #[test]
    fn format_token_notice_print_true_includes_token() {
        let token = "a".repeat(64);
        let notice = format_token_notice(&token, true, None);
        assert!(notice.contains(&token), "default print must show token");
        assert!(notice.starts_with("Auth token:"));
    }

    #[test]
    fn format_token_notice_print_false_redacts_token_and_shows_path() {
        let token = "b".repeat(64);
        let path = Utf8Path::new("/tmp/.ledgerful/web-session-token");
        let notice = format_token_notice(&token, false, Some(path));
        assert!(
            !notice.contains(&token),
            "print-token=false must not include the token value: {notice}"
        );
        assert!(
            notice.contains("web-session-token"),
            "print-token=false must report the file path: {notice}"
        );
        assert!(notice.contains("Auth token written to"));
    }

    #[test]
    fn format_token_notice_print_false_without_path_still_redacts() {
        let token = "c".repeat(64);
        let notice = format_token_notice(&token, false, None);
        assert!(!notice.contains(&token));
        assert!(!notice.contains("Auth token: c"));
    }

    #[test]
    fn open_path_message_reports_token_file_when_suppressed() {
        // Mirrors the --open branch in start_web when print_token is false.
        let path = Utf8Path::new("C:\\repo\\.ledgerful\\web-session-token");
        let msg = format!(
            "Open {} in your browser. Sign in with the token from {}.",
            "http://127.0.0.1:52001/", path
        );
        assert!(msg.contains("web-session-token"));
        assert!(!msg.contains("Auth token:"));
    }

    #[cfg(unix)]
    #[test]
    fn write_session_token_file_sets_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let layout =
            crate::state::layout::Layout::new(camino::Utf8Path::from_path(tmp.path()).unwrap());
        std::fs::create_dir_all(layout.state_dir.as_std_path()).unwrap();
        let path = write_session_token_file(&layout, &"d".repeat(64)).unwrap();
        let mode = std::fs::metadata(path.as_std_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "Unix token file must be user-only 0600");
    }

    #[test]
    #[serial_test::serial(env)]
    fn combined_footgun_blank_token_and_public_both_refuse() {
        // DoD-1 + DoD-3: neither blank token nor public-without-allowlist starts.
        let _clear = TempEnv::remove(PEER_ALLOWLIST_ENV);
        assert!(resolve_session_token(Some(String::new()), None).is_err());
        assert!(resolve_session_token(None, Some(String::new())).is_err());
        assert!(resolve_peer_allowlist(true, "0.0.0.0").is_err());
    }
}
