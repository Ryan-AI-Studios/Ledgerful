use clap::Parser;
use ledgerful::cli::{self, Cli, resolve_quiet};
use miette::Result;
use tracing::Level;
use tracing_subscriber::fmt::writer::MakeWriterExt;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Build the log filter based on the verbose flag and machine mode.
///
/// - `machine = true`: force WARN so normal_layer progress INFO cannot hit
///   stderr around a machine payload (0093; successful `verify --json` empty
///   stderr). WARN/ERROR still pass (Wave 0 honesty).
/// - `verbose = true` (and not machine): use "debug" level for all crates
/// - otherwise: respect `RUST_LOG` if set, else default human floor **WARN**
///   (0154; was INFO) so timestamped tracing INFO does not treat stderr as a
///   log file on non-verbose runs.
fn build_log_filter(verbose: bool, machine: bool) -> EnvFilter {
    build_log_filter_with_env(verbose, machine, std::env::var("RUST_LOG").ok())
}

/// Injectable variant of [`build_log_filter`] for deterministic unit tests.
///
/// Pass `rust_log: None` (or empty string) to exercise the default human WARN
/// floor without mutating process environment. Non-empty values honor the
/// human-path `RUST_LOG` escape hatch.
fn build_log_filter_with_env(verbose: bool, machine: bool, rust_log: Option<String>) -> EnvFilter {
    if machine {
        // Prefer a fixed WARN floor over RUST_LOG so agents cannot accidentally
        // re-enable progress INFO via an ambient RUST_LOG=info.
        EnvFilter::new("warn,graph_builder=warn,tantivy=warn,sqlite=warn")
    } else if verbose {
        EnvFilter::new("debug")
    } else if let Some(spec) = rust_log.filter(|s| !s.is_empty()) {
        // Escape hatch: honor ambient RUST_LOG when set (human path only).
        EnvFilter::try_new(spec)
            .unwrap_or_else(|_| EnvFilter::new("warn,graph_builder=warn,tantivy=warn,sqlite=warn"))
    } else {
        // 0154: default human floor WARN (was info,…).
        EnvFilter::new("warn,graph_builder=warn,tantivy=warn,sqlite=warn")
    }
}

/// Four-state verbosity for the `cli_summary` product layer (0093 + 0100).
///
/// | State | Level | Per-entry `debug!` | Aggregate `info!` |
/// |---|---|---|---|
/// | Default | `INFO` | hidden | shown |
/// | `--verbose` | `DEBUG` | shown | shown |
/// | `--quiet` | `INFO` | hidden | shown |
/// | Machine | `WARN` | hidden | hidden |
///
/// Precedence: machine → `WARN` (wins over everything); else if `verbose` →
/// `DEBUG` (explicit `-v` wins over quiet); else → `INFO` (default and quiet
/// are equivalent for signatures). Track 0100 moved the product default from
/// `DEBUG` to `INFO`; `--verbose` restores per-entry detail.
fn summary_layer_max_level(machine: bool, quiet: bool, verbose: bool) -> Level {
    if machine {
        Level::WARN
    } else if verbose {
        Level::DEBUG
    } else {
        // quiet and default both INFO — quiet is kept as a flag for agents
        // that already set LEDGERFUL_QUIET=1 (no behaviour change for them).
        let _ = quiet;
        Level::INFO
    }
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

    // 0131: honour NO_COLOR / FORCE_COLOR / CLICOLOR_FORCE before any product
    // colour emit. Belt-and-suspenders for if_supports_color + test baseline.
    ledgerful::output::color::init_color_support();

    // TA19: the global `-v` flag must not enable debug-level tracing for
    // `config diff` output. It still controls tracing for every other command.
    let effective_verbose = cli_args.verbose
        && !matches!(
            &cli_args.command,
            ledgerful::cli::args::Commands::Config {
                command: ledgerful::cli::args::ConfigCommands::Diff { .. },
            }
        );

    // 0093/0100: four-state `cli_summary` filter. Machine mode (`--json` / mcp /
    // scan --format json) filters to WARN so human product lines cannot land
    // on stdout around a machine payload. Default/quiet are INFO (aggregate
    // only); `--verbose` restores DEBUG per-entry detail. Machine also raises
    // normal_layer to WARN so progress INFO cannot pollute stderr on success.
    let machine = cli_args.command.is_machine_output();
    let summary_max =
        summary_layer_max_level(machine, resolve_quiet(cli_args.quiet), effective_verbose);

    let normal_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(tracing_subscriber::filter::filter_fn(move |meta| {
            meta.target() != "cli_summary"
        }))
        .with_filter(build_log_filter(effective_verbose, machine));

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
        let code = ledgerful::output::requested_exit::take_requested_exit_code().unwrap_or(1);
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
        let _f = build_log_filter(true, false);
    }

    #[test]
    fn build_log_filter_quiet_does_not_panic() {
        let _f = build_log_filter(false, false);
    }

    #[test]
    fn build_log_filter_default_human_is_warn_floor() {
        // 0154 C1: inject None so ambient RUST_LOG cannot flaky the default floor.
        let filter = build_log_filter_with_env(false, false, None);
        let rendered = format!("{filter:?}");
        assert!(
            rendered.contains("warn") || rendered.to_lowercase().contains("warn"),
            "default human filter must be WARN-based; got {rendered}"
        );
        // Must not retain the pre-0154 default info floor as the primary directive.
        assert!(
            !rendered.contains("info,") && !rendered.starts_with("info"),
            "default human must not use INFO floor; got {rendered}"
        );
    }

    #[test]
    fn build_log_filter_verbose_is_debug_based() {
        // 0154 C6 / DoD-4 filter-level: -v selects fixed debug.
        let filter = build_log_filter_with_env(true, false, None);
        let rendered = format!("{filter:?}");
        assert!(
            rendered.contains("debug") || rendered.to_lowercase().contains("debug"),
            "verbose filter must be DEBUG-based; got {rendered}"
        );
    }

    #[test]
    fn build_log_filter_rust_log_escape_hatch_constructs() {
        // Human path still honors RUST_LOG when set (escape hatch without -v).
        let filter = build_log_filter_with_env(false, false, Some("info".into()));
        let rendered = format!("{filter:?}");
        assert!(
            rendered.contains("info") || rendered.to_lowercase().contains("info"),
            "RUST_LOG=info escape hatch must construct; got {rendered}"
        );
    }

    #[test]
    fn build_log_filter_machine_forces_warn() {
        // Machine mode must silence normal_layer INFO regardless of ambient RUST_LOG.
        let filter = build_log_filter(false, true);
        // EnvFilter Debug form includes the directive string we set.
        let rendered = format!("{filter:?}");
        assert!(
            rendered.contains("warn") || rendered.to_lowercase().contains("warn"),
            "machine filter must be WARN-based; got {rendered}"
        );
        // Verbose must not override machine (fixed WARN directive, not debug).
        let filter_v = build_log_filter(true, true);
        let rendered_v = format!("{filter_v:?}");
        assert!(
            rendered_v.contains("warn") || rendered_v.to_lowercase().contains("warn"),
            "machine must win over verbose (WARN floor); got {rendered_v}"
        );
        assert!(
            !rendered_v.contains("debug") && !rendered_v.to_lowercase().contains("debug"),
            "machine must not select DEBUG when verbose; got {rendered_v}"
        );
        // Machine also wins over with_env verbose injection.
        let filter_mv = build_log_filter_with_env(true, true, Some("debug".into()));
        let rendered_mv = format!("{filter_mv:?}");
        assert!(
            rendered_mv.contains("warn") || rendered_mv.to_lowercase().contains("warn"),
            "machine must win over verbose+RUST_LOG; got {rendered_mv}"
        );
    }

    #[test]
    fn summary_layer_max_level_four_states() {
        // Default → INFO (summary-first; hides per-entry debug!).
        assert_eq!(summary_layer_max_level(false, false, false), Level::INFO);
        // Quiet → INFO (same as default for signatures).
        assert_eq!(summary_layer_max_level(false, true, false), Level::INFO);
        // Verbose → DEBUG (restores per-entry detail).
        assert_eq!(summary_layer_max_level(false, false, true), Level::DEBUG);
        // Explicit -v wins over quiet.
        assert_eq!(summary_layer_max_level(false, true, true), Level::DEBUG);
        // Machine → WARN.
        assert_eq!(summary_layer_max_level(true, false, false), Level::WARN);
        // Machine wins over quiet and verbose.
        assert_eq!(summary_layer_max_level(true, true, false), Level::WARN);
        assert_eq!(summary_layer_max_level(true, false, true), Level::WARN);
        assert_eq!(summary_layer_max_level(true, true, true), Level::WARN);
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
        let summary_max = summary_layer_max_level(true, false, false); // machine

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

    /// DoD-5 / F3: under the machine `cli_summary` filter, VALID-style detail
    /// (`debug!`) is dropped; INVALID / required-UNSIGNED are raw `eprintln!`
    /// outside the subscriber and therefore cannot be filtered away.
    #[test]
    fn machine_mode_drops_valid_detail_not_raw_invalid() {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let stdout_buf = BufWriter::default();
        let stderr_buf = BufWriter::default();
        let stdout_capture = Arc::clone(&stdout_buf.buf);
        let stderr_capture = Arc::clone(&stderr_buf.buf);

        let writer = stderr_buf.with_max_level(Level::WARN).or_else(stdout_buf);
        let summary_max = summary_layer_max_level(true, false, false); // machine = WARN

        let layer = fmt::layer()
            .with_writer(writer)
            .without_time()
            .with_target(false)
            .with_level(false)
            .with_filter(tracing_subscriber::filter::filter_fn(move |meta| {
                meta.target() == "cli_summary" && *meta.level() <= summary_max
            }));

        let _guard = tracing_subscriber::registry().with(layer).set_default();

        // VALID detail (production uses debug! on cli_summary).
        tracing::debug!(target: "cli_summary", "  [VALID] TX abcdef01");
        // Aggregate summary (production uses info! on cli_summary).
        tracing::info!(
            target: "cli_summary",
            "Signature verification summary: 1 valid, 0 invalid"
        );
        // Machine still allows warn! diagnostics on stderr.
        tracing::warn!(target: "cli_summary", "observe would-block");

        let stdout = String::from_utf8_lossy(&stdout_capture.lock().unwrap()).to_string();
        let stderr = String::from_utf8_lossy(&stderr_capture.lock().unwrap()).to_string();

        assert!(
            !stdout.contains("[VALID]") && !stderr.contains("[VALID]"),
            "VALID detail must not appear under machine filter; stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            !stdout.contains("Signature verification summary")
                && !stderr.contains("Signature verification summary"),
            "aggregate info! must not appear under machine; stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            stderr.contains("observe would-block"),
            "warn! still reaches stderr under machine; stderr={stderr:?}"
        );

        // Structural guarantee for INVALID / required UNSIGNED: they use raw
        // `eprintln!`, which never enters the tracing subscriber. Prove the
        // emit decision helper marks them filter-immune.
        use ledgerful::commands::verify::sig_entry_stream;
        use ledgerful::ledger::crypto::SignatureTrustStatus;
        assert!(
            sig_entry_stream(SignatureTrustStatus::Invalid, false).is_raw_stderr(),
            "INVALID must bypass cli_summary filter"
        );
        assert!(
            sig_entry_stream(SignatureTrustStatus::Unsigned, true).is_raw_stderr(),
            "required UNSIGNED must bypass cli_summary filter"
        );
        assert!(
            !sig_entry_stream(SignatureTrustStatus::ValidTrusted, false).is_raw_stderr(),
            "VALID must go through cli_summary (debug) and be filterable"
        );
        assert!(
            !sig_entry_stream(SignatureTrustStatus::Unsigned, false).is_raw_stderr(),
            "optional UNSIGNED (SKIP) must go through cli_summary (debug)"
        );
    }
}
