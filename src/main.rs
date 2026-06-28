use clap::Parser;
use ledgerful::cli::{self, Cli};
use miette::Result;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Build the log filter based on the verbose flag.
///
/// - `verbose = true`: use "debug" level for all crates
/// - `verbose = false`: respect `RUST_LOG` if set, otherwise apply the quiet
///   default that silences noisy third-party crates (graph_builder, tantivy,
///   sqlite) to WARN while keeping everything else at INFO.
fn build_log_filter(verbose: bool) -> EnvFilter {
    if verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info,graph_builder=warn,tantivy=warn,sqlite=warn"))
    }
}

fn run() -> Result<()> {
    // Intercept "help" and "version" subcommands ONLY if they are the first
    // positional argument to unify behavior without breaking legitimate
    // positional values or subcommand args later in the string.
    let args: Vec<String> = std::env::args().collect();
    let mut transformed = Vec::with_capacity(args.len());

    if args.len() > 1 && args[1] == "help" {
        // [exe, help, cmd1, cmd2] -> [exe, cmd1, cmd2, --help]
        transformed.push(args[0].clone());
        for arg in args.iter().skip(2) {
            transformed.push(arg.clone());
        }
        transformed.push("--help".to_string());
    } else if args.len() > 1 && args[1] == "version" {
        // [exe, version, ...] -> [exe, --version, ...]
        transformed.push(args[0].clone());
        transformed.push("--version".to_string());
        for arg in args.iter().skip(2) {
            transformed.push(arg.clone());
        }
    } else {
        transformed = args;
    }

    let args = transformed;
    // Parse CLI args once here so we can read the verbose flag before
    // initializing the logger.  cli::run_with(cli) reuses the parsed struct,
    // avoiding a second parse.
    let cli_args = Cli::parse_from(args);
    // TA19: the global `-v` flag must not enable debug-level tracing for
    // `config diff` output. It still controls tracing for every other command.
    let effective_verbose = cli_args.verbose
        && !matches!(
            &cli_args.command,
            ledgerful::cli::args::Commands::Config {
                command: ledgerful::cli::args::ConfigCommands::Diff { .. },
            }
        );

    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(build_log_filter(effective_verbose))
        .init();

    // H4: Sweep for stale shadow-copy binaries left over from a prior update
    // attempt (e.g. `ledgerful.old.exe` next to the current executable).
    sweep_stale_old_binaries();

    cli::run_with(cli_args)?;

    Ok(())
}

/// Remove `<exe_name>.old.*.exe` files adjacent to the current executable.
/// These are left when a previous `update --binary` was interrupted.
/// Only files whose prefix matches the *current* binary name are removed so
/// that shadow copies from unrelated binaries are not accidentally deleted.
/// Errors are silently ignored — this is best-effort cleanup.
#[cfg(target_os = "windows")]
fn sweep_stale_old_binaries() {
    if let Ok(current) = std::env::current_exe()
        && let Some(dir) = current.parent()
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        // Derive the expected prefix, e.g. "ledgerful.old." from "ledgerful.exe".
        let prefix = current
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|stem| format!("{stem}.old."))
            .unwrap_or_else(|| "ledgerful.old.".to_string());

        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(prefix.as_str()) && n.ends_with(".exe"))
                .unwrap_or(false)
            {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn sweep_stale_old_binaries() {}

fn main() {
    // Windows debug builds with many clap subcommands can overflow the default
    // 1 MiB stack. Run the application logic in a thread with a larger stack.
    let result = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .spawn(run)
        .expect("Failed to spawn main thread")
        .join()
        .expect("Main thread panicked");

    if let Err(e) = result {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn cli_has_verbose_flag() {
        let args =
            ledgerful::cli::Cli::try_parse_from(["ledgerful", "--verbose", "doctor"]).unwrap();
        assert!(args.verbose);
        let args_short =
            ledgerful::cli::Cli::try_parse_from(["ledgerful", "-v", "doctor"]).unwrap();
        assert!(args_short.verbose);
    }

    #[test]
    fn cli_verbose_default_is_false() {
        let args = ledgerful::cli::Cli::try_parse_from(["ledgerful", "doctor"]).unwrap();
        assert!(!args.verbose);
    }

    #[test]
    fn build_log_filter_verbose_does_not_panic() {
        let _f = build_log_filter(true);
    }

    #[test]
    fn build_log_filter_quiet_does_not_panic() {
        let _f = build_log_filter(false);
    }
}
