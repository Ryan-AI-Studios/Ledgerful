use clap::Parser;
use ledgerful::cli::{self, Cli};
use miette::Result;
use tracing::Level;
use tracing_subscriber::fmt::writer::MakeWriterExt;
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

/// Three-state verbosity for the `cli_summary` product layer (track 0093).
///
/// - Default (`DEBUG`): per-entry detail and aggregate both visible.
/// - Quiet (`INFO`): hide per-entry `debug!` detail; keep aggregate `info!`.
/// - Machine (`WARN`): no human-facing `cli_summary` line reaches stdout.
///
/// Machine wins over quiet when both are selected.
fn summary_layer_max_level(machine: bool, quiet: bool) -> Level {
    if machine {
        Level::WARN
    } else if quiet {
        Level::INFO
    } else {
        Level::DEBUG
    }
}

/// `true` when `--quiet`/`-q` or `LEDGERFUL_QUIET=1` (or `true`) is set.
fn resolve_quiet(cli_quiet: bool) -> bool {
    if cli_quiet {
        return true;
    }
    std::env::var("LEDGERFUL_QUIET")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn run() -> Result<()> {
    // Intercept "help" and "version" subcommands ONLY if they are the first
    // positional argument to unify behavior without breaking legitimate
    // positional values or subcommand args later in the string.
    // Legitimate: CLI argv for help/version rewrite only — not an auth boundary.
    // nosemgrep: rust.lang.security.args.args
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

    // 0093: three-state `cli_summary` filter. Machine mode (`--json` / mcp /
    // scan --format json) filters to WARN so human product lines cannot land
    // on stdout around a machine payload. Quiet hides per-entry DEBUG detail.
    let summary_max = summary_layer_max_level(
        cli_args.command.is_machine_output(),
        resolve_quiet(cli_args.quiet),
    );

    let normal_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(tracing_subscriber::filter::filter_fn(move |meta| {
            meta.target() != "cli_summary"
        }))
        .with_filter(build_log_filter(effective_verbose));

    // Level-split writer (0093 DoD-4): product `info!` → stdout; diagnostic
    // `warn!`/`error!` → stderr. A blanket stdout writer would break
    // `ledger status --json` on the observe would-block path (spec §2.3).
    let summary_layer = fmt::layer()
        .with_writer(
            std::io::stderr
                .with_max_level(Level::WARN)
                .or_else(std::io::stdout),
        )
        .without_time()
        .with_target(false)
        .with_level(false)
        .with_filter(tracing_subscriber::filter::filter_fn(move |meta| {
            meta.target() == "cli_summary" && *meta.level() <= summary_max
        }));

    // Track 0043: local-only TimingLayer buffers span closes in memory;
    // TimedCommand (in dispatch) flushes one SQLite batch on drop.
    #[cfg(feature = "self-timing")]
    {
        let timing_layer = ledgerful::observability::self_timing::TimingLayer::new();
        tracing_subscriber::registry()
            .with(normal_layer)
            .with(summary_layer)
            .with(timing_layer)
            .init();
    }
    #[cfg(not(feature = "self-timing"))]
    {
        tracing_subscriber::registry()
            .with(normal_layer)
            .with(summary_layer)
            .init();
    }

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
    // Legitimate: self-path for sweeping leftover update shadow-copy binaries.
    // nosemgrep: rust.lang.security.current-exe.current-exe
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
        // 0072: signature verify may request distinct exit codes (e.g. UNSIGNED=3).
        let code = ledgerful::commands::verify::sig_exit::take_requested_exit_code().unwrap_or(1);
        std::process::exit(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

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
    fn cli_has_quiet_flag() {
        let args = ledgerful::cli::Cli::try_parse_from(["ledgerful", "--quiet", "doctor"]).unwrap();
        assert!(args.quiet);
        let args_short =
            ledgerful::cli::Cli::try_parse_from(["ledgerful", "-q", "doctor"]).unwrap();
        assert!(args_short.quiet);
    }

    #[test]
    fn build_log_filter_verbose_does_not_panic() {
        let _f = build_log_filter(true);
    }

    #[test]
    fn build_log_filter_quiet_does_not_panic() {
        let _f = build_log_filter(false);
    }

    #[test]
    fn summary_layer_max_level_three_states() {
        assert_eq!(summary_layer_max_level(false, false), Level::DEBUG);
        assert_eq!(summary_layer_max_level(false, true), Level::INFO);
        assert_eq!(summary_layer_max_level(true, false), Level::WARN);
        // Machine wins over quiet.
        assert_eq!(summary_layer_max_level(true, true), Level::WARN);
    }

    /// Buffer make-writer for stream-routing tests (DoD-4).
    #[derive(Clone, Default)]
    struct BufWriter {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl<'a> MakeWriter<'a> for BufWriter {
        type Writer = BufGuard;

        fn make_writer(&'a self) -> Self::Writer {
            BufGuard {
                buf: Arc::clone(&self.buf),
            }
        }
    }

    struct BufGuard {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for BufGuard {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.buf
                .lock()
                .map_err(|e| io::Error::other(e.to_string()))?
                .extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn summary_layer_routes_info_to_stdout_warn_error_to_stderr() {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let stdout_buf = BufWriter::default();
        let stderr_buf = BufWriter::default();
        let stdout_capture = Arc::clone(&stdout_buf.buf);
        let stderr_capture = Arc::clone(&stderr_buf.buf);

        // Level-split: events at WARN and above → stderr buffer; else → stdout.
        let writer = stderr_buf.with_max_level(Level::WARN).or_else(stdout_buf);

        let layer = fmt::layer()
            .with_writer(writer)
            .without_time()
            .with_target(false)
            .with_level(false)
            .with_filter(tracing_subscriber::filter::filter_fn(|meta| {
                meta.target() == "cli_summary"
            }));

        let _guard = tracing_subscriber::registry().with(layer).set_default();

        tracing::info!(target: "cli_summary", "info-product-line");
        tracing::warn!(target: "cli_summary", "warn-diagnostic-line");
        tracing::error!(target: "cli_summary", "error-diagnostic-line");

        let stdout = String::from_utf8_lossy(&stdout_capture.lock().unwrap()).to_string();
        let stderr = String::from_utf8_lossy(&stderr_capture.lock().unwrap()).to_string();

        assert!(
            stdout.contains("info-product-line"),
            "info! must land on stdout; got stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            !stdout.contains("warn-diagnostic-line") && !stdout.contains("error-diagnostic-line"),
            "warn/error must not land on stdout; got stdout={stdout:?}"
        );
        assert!(
            stderr.contains("warn-diagnostic-line"),
            "warn! must land on stderr; got stderr={stderr:?}"
        );
        assert!(
            stderr.contains("error-diagnostic-line"),
            "error! must land on stderr; got stderr={stderr:?}"
        );
        assert!(
            !stderr.contains("info-product-line"),
            "info! must not land on stderr; got stderr={stderr:?}"
        );
    }

    #[test]
    fn machine_mode_drops_cli_summary_info() {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let stdout_buf = BufWriter::default();
        let stderr_buf = BufWriter::default();
        let stdout_capture = Arc::clone(&stdout_buf.buf);
        let stderr_capture = Arc::clone(&stderr_buf.buf);

        let writer = stderr_buf.with_max_level(Level::WARN).or_else(stdout_buf);
        let summary_max = summary_layer_max_level(true, false); // machine

        let layer = fmt::layer()
            .with_writer(writer)
            .without_time()
            .with_target(false)
            .with_level(false)
            .with_filter(tracing_subscriber::filter::filter_fn(move |meta| {
                meta.target() == "cli_summary" && *meta.level() <= summary_max
            }));

        let _guard = tracing_subscriber::registry().with(layer).set_default();

        // Injected from the test itself — structural guarantee (DoD-4b).
        tracing::info!(target: "cli_summary", "injected-info-must-not-appear");
        tracing::warn!(target: "cli_summary", "injected-warn-on-stderr");

        let stdout = String::from_utf8_lossy(&stdout_capture.lock().unwrap()).to_string();
        let stderr = String::from_utf8_lossy(&stderr_capture.lock().unwrap()).to_string();

        assert!(
            !stdout.contains("injected-info-must-not-appear"),
            "machine mode must drop info! from both streams; stdout={stdout:?}"
        );
        assert!(
            !stderr.contains("injected-info-must-not-appear"),
            "machine mode must drop info!; stderr={stderr:?}"
        );
        assert!(
            stderr.contains("injected-warn-on-stderr"),
            "machine mode still routes warn! to stderr; stderr={stderr:?}"
        );
        assert!(
            stdout.is_empty() || !stdout.contains("injected"),
            "stdout must stay clean for machine payloads; stdout={stdout:?}"
        );
    }
}
