pub mod api;
pub mod auth;
pub mod error;
pub mod git_meta;
pub mod server;
pub mod state;
pub mod types;

use crate::cli::args::{WebCommands, WebStartArgs};
use crate::commands::helpers::get_layout;
use crate::commands::pid::PidFile;
use crate::commands::web::auth::generate_token;
use crate::commands::web::state::AppState;
use miette::{IntoDiagnostic, Result, miette};
use owo_colors::OwoColorize;
use std::sync::Arc;

const TOKEN_ENV_VAR: &str = "LEDGERFUL_WEB_TOKEN";

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

    let token = args
        .token
        .clone()
        .or_else(|| std::env::var(TOKEN_ENV_VAR).ok())
        .unwrap_or_else(generate_token);
    let base_url = format!("http://{}:{}/", args.bind, args.port);

    if args.background {
        return spawn_background_server(args, layout, &base_url, &token);
    }

    let _pid_guard = PidFile::create(pid_path)?;
    let state = Arc::new(AppState::new(layout, token.clone(), args.spa_dir));

    println!("Starting ledgerful web at {}", base_url);
    println!("Auth token: {}", token);

    if args.open
        && let Err(e) = webbrowser::open(&base_url)
    {
        tracing::warn!("Failed to open browser: {}", e);
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

#[cfg(unix)]
fn spawn_background_server(
    args: WebStartArgs,
    layout: crate::state::layout::Layout,
    url: &str,
    token: &str,
) -> Result<()> {
    println!("Starting ledgerful web in the background at {}", url);
    println!("Auth token: {}", token);

    let pid_path = layout.web_pid_file();
    // Remove stale PID file so the child can create a fresh one.
    PidFile::remove(&pid_path);

    // Double-fork via nix::unistd::daemon. The current process exits; the
    // daemon continues as the server.
    match unsafe { nix::unistd::fork() } {
        Ok(nix::unistd::ForkResult::Parent { .. }) => {
            // Parent exits immediately after the child is launched. The child
            // will write its own PID file once it starts.
            std::process::exit(0);
        }
        Ok(nix::unistd::ForkResult::Child) => {
            // Detach from controlling terminal and continue as the daemon.
            let _ = nix::unistd::setsid();
            match unsafe { nix::unistd::fork() } {
                Ok(nix::unistd::ForkResult::Parent { .. }) => {
                    std::process::exit(0);
                }
                Ok(nix::unistd::ForkResult::Child) => {
                    // SAFETY: single-threaded child process after double-fork,
                    // no other threads can observe the env mutation.
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
    let token = std::env::var(TOKEN_ENV_VAR)
        .ok()
        .or(args.token)
        .unwrap_or_else(generate_token);
    let state = Arc::new(AppState::new(layout, token, args.spa_dir));
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
) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
    use windows_sys::Win32::System::Threading::DETACHED_PROCESS;

    println!("Starting ledgerful web in the background at {}", url);
    println!("Auth token: {}", token);

    let pid_path = layout.web_pid_file();
    PidFile::remove(&pid_path);

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
    if bind == "127.0.0.1" || bind == "::1" || bind == "localhost" {
        return Ok(());
    }

    if let Ok(addr) = bind.parse::<std::net::IpAddr>()
        && addr.is_loopback()
    {
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
        "Keep the token secret, or restrict network access with a firewall.".red()
    );
    eprintln!("{}", line.red());
}
