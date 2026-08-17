use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum ExportCommands {
    /// Export a SOC2 evidence bundle as a zip file
    Evidence {
        /// Export profile (currently only "soc2")
        #[arg(long, default_value = "soc2")]
        profile: String,
        /// Output file path (default: ./ledgerful-soc2-evidence.zip)
        #[arg(short, long)]
        out: Option<std::path::PathBuf>,
        /// Overwrite an existing file
        #[arg(short, long)]
        force: bool,
        /// Control ID(s) to scope the export (e.g. CC8.1, CC7.*). Repeatable.
        #[arg(long = "control")]
        control: Vec<String>,
    },
    /// Export the live chain head as a thin JSON checkpoint
    Head {
        /// Output file path (default: ./ledgerful-chain-head.json).
        /// Use `-` (or `--stdout`) to write pretty JSON to stdout only.
        #[arg(short, long)]
        out: Option<std::path::PathBuf>,
        /// Overwrite an existing file (file mode only; ignored with --stdout / -o -)
        #[arg(short, long)]
        force: bool,
        /// Write pretty ChainHead JSON to stdout (no SUCCESS banner, no file).
        /// Equivalent to `-o -`. Cannot be combined with a non-dash `--out` path.
        #[arg(long)]
        stdout: bool,
    },
}

#[derive(Subcommand, Debug)]
#[cfg(feature = "web")]
pub enum WebCommands {
    /// Start the ledgerful web dashboard server
    Start(WebStartArgs),
    /// Stop a running ledgerful web dashboard server
    Stop,
    /// Show whether the ledgerful web server is running
    Status,
}

#[derive(Args, Debug)]
#[cfg(feature = "web")]
pub struct WebStartArgs {
    /// Port to listen on
    #[arg(long, short, default_value_t = 52001)]
    pub port: u16,
    /// Address to bind to
    #[arg(long, short, default_value = "127.0.0.1")]
    pub bind: String,
    /// Serve a custom SPA directory instead of the embedded dashboard
    #[arg(long)]
    pub spa_dir: Option<camino::Utf8PathBuf>,
    /// Open the dashboard in the default browser
    #[arg(long)]
    pub open: bool,
    /// Allow binding to non-loopback addresses (requires LEDGERFUL_WEB_PEER_ALLOWLIST)
    #[arg(long)]
    pub allow_public: bool,
    /// Run the server in the background
    #[arg(long)]
    pub background: bool,
    /// Print the session token to stdout (default **false**). By default the token
    /// is written to `.ledgerful/web-session-token` and only the path is printed,
    /// reducing shell-history / screen-share / CI-log leakage. Pass
    /// `--print-token=true` as an opt-in escape hatch for demos or local debugging.
    #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
    pub print_token: bool,
    /// Pre-generated session token (hidden; operator-supplied override only).
    /// Background daemonize no longer passes this on the child command line
    /// (track 0090 W2) — the parent hands the token via `LEDGERFUL_WEB_TOKEN`
    /// to avoid process-list / EDR command-line leakage. Prefer that env var
    /// or the session token file for secrets.
    #[arg(long, hide = true)]
    pub token: Option<String>,
}

/// MCP stdio serve and host-platform install surface (feature `mcp`).
///
/// Bare `ledgerful mcp` (no subcommand) serves stdio. Subcommands wire agent
/// host configs for the Top-N platforms only.
#[cfg(feature = "mcp")]
#[derive(Subcommand, Debug)]
pub enum McpCommands {
    /// Run the MCP server on stdio (default when no subcommand is given)
    Serve,
    /// Merge Ledgerful into agent host MCP configs (Top-N platforms)
    Install {
        /// Platform id (repeatable). Supported: claude-code, cursor, codex, copilot.
        /// When omitted, detect from config presence / host binary on PATH.
        #[arg(
            long = "platform",
            value_name = "ID",
            action = clap::ArgAction::Append,
            value_parser = crate::commands::mcp::install::parse_platform_id
        )]
        platforms: Vec<String>,
        /// Config scope (user or project). Defaults per platform when omitted.
        #[arg(long, value_enum)]
        scope: Option<McpScope>,
        /// How to launch the MCP server
        #[arg(long, value_enum, default_value_t = McpLauncher::Auto)]
        launcher: McpLauncher,
        /// Report planned writes without mutating files
        #[arg(long)]
        dry_run: bool,
        /// Replace an existing ledgerful entry even when command/args differ
        #[arg(long)]
        force: bool,
        /// Skip creating a sibling `.bak` before writing (default: backup on)
        #[arg(long = "no-backup", default_value_t = false)]
        no_backup: bool,
        /// Emit machine-readable JSON report
        #[arg(long)]
        json: bool,
    },
    /// Remove only the ledgerful MCP server entry from host configs
    Uninstall {
        /// Platform id (repeatable). Supported: claude-code, cursor, codex, copilot.
        #[arg(
            long = "platform",
            value_name = "ID",
            action = clap::ArgAction::Append,
            value_parser = crate::commands::mcp::install::parse_platform_id
        )]
        platforms: Vec<String>,
        /// Config scope (user or project). Defaults per platform when omitted.
        #[arg(long, value_enum)]
        scope: Option<McpScope>,
        /// Report planned removals without mutating files
        #[arg(long)]
        dry_run: bool,
        /// Emit machine-readable JSON report
        #[arg(long)]
        json: bool,
    },
    /// Report ledgerful MCP entry presence across Top-N platforms (no mutation)
    Status {
        /// Emit machine-readable JSON report
        #[arg(long)]
        json: bool,
    },
}

/// MCP host config scope.
#[cfg(feature = "mcp")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum McpScope {
    User,
    Project,
}

/// MCP server launcher resolution mode.
#[cfg(feature = "mcp")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum McpLauncher {
    /// Prefer PATH binary; fall back to npx with pin-lag warning
    #[default]
    Auto,
    /// Require `ledgerful` on PATH (absolute path + args `["mcp"]`)
    Path,
    /// Use `npx -y @ledgerful/mcp-server` (Windows prefers `npx.cmd`)
    Npx,
}

#[cfg(feature = "sync")]
#[derive(Subcommand, Debug)]
pub enum SyncSubcommands {
    /// Initialize team sync for this device [Available — opt-in shared-folder v1]
    Init {
        /// Force re-initialization (overwrites key material + device_id SoT)
        #[arg(short, long)]
        force: bool,
        /// Inject a test secret (hex encoded) for non-interactive use
        #[arg(long, hide = true)]
        with_secret: Option<String>,
    },
    /// Generate or accept a pairing invite, list/revoke peers [Available]
    ///
    /// Without an invite: print a self-contained `LF-PAIR-1…` invite (requires
    /// `LEDGERFUL_SYNC_SECRET` and prior `sync init`). Accept with
    /// `sync pair <invite>` under the same team secret. Mutual accept for
    /// two-way trust (two invite/accept cycles). Never sets `[sync].enabled = true`.
    Pair {
        /// Pairing invite from peer (`LF-PAIR-1...`)
        #[arg(conflicts_with_all = ["list", "revoke"])]
        code: Option<String>,
        /// List trusted peers
        #[arg(long, conflicts_with_all = ["code", "revoke"])]
        list: bool,
        /// Revoke trust for a device_id
        #[arg(long, conflicts_with_all = ["code", "list"])]
        revoke: Option<String>,
        /// Replace existing peer pubkey on re-pair
        #[arg(long)]
        force: bool,
    },
    /// Run the sync loop [Available — opt-in; never default-on]
    Run {
        /// Run only once and exit
        #[arg(long)]
        once: bool,
    },
    /// Readiness checklist and gated enable [Available]
    ///
    /// Default: print checklist + next command (never enables, never prompts
    /// for secret). `--enable` sets `[sync].enabled=true` only when init +
    /// ≥1 peer + parseable reachable target are all green (sibling
    /// `config.toml.bak` first). See docs/team-sync.md.
    Setup {
        /// Enable sync after readiness gates pass (strict refuse matrix)
        #[arg(long)]
        enable: bool,
        /// Emit pure camelCase readiness JSON on stdout (`schemaVersion: 1`)
        #[arg(long)]
        json: bool,
    },
    /// Show team sync status + readiness next-action [Available]
    Status {
        /// Emit pure camelCase status/readiness JSON on stdout (`schemaVersion: 1`)
        #[arg(long)]
        json: bool,
    },
    /// Verify the integrity of sync bundles [Available]
    Verify {
        /// Path to the bundle file
        path: String,
    },
    /// Manage sync cursors [Available]
    Cursor {
        /// Set a specific cursor HLC
        #[arg(long)]
        set: Option<String>,
    },
    /// Show sync logs [Available]
    Log {
        /// Number of lines to tail
        #[arg(long, short)]
        tail: Option<usize>,
    },
}

#[derive(Subcommand, Debug)]
pub enum InternalCommands {
    /// Internal git hook command for commit message validation
    #[command(name = "hook-commit-msg")]
    HookCommitMsg {
        /// The file containing the commit message
        msg_file: PathBuf,
    },
    /// Internal git hook command for post-commit processing
    #[command(name = "hook-post-commit")]
    HookPostCommit,
}

#[derive(Subcommand, Debug)]
#[cfg(feature = "usage-metrics")]
pub enum UsageCommands {
    /// Enable anonymous usage metrics
    Enable,
    /// Disable anonymous usage metrics
    Disable,
    /// Show usage metrics status
    Status,
    /// Show the exact payload that would be sent
    ShowPayload,
}

/// Local-only per-command timing analysis (Track 0043; `--global` is Track 0044).
#[derive(Args, Debug)]
pub struct TimingsCliArgs {
    /// Aggregate command timings across all discovered repos on disk (0044)
    #[arg(long)]
    pub global: bool,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
    /// Show the top N commands by total time (default 20)
    #[arg(long)]
    pub top: Option<u32>,
    /// Limit analysis to the last N days (default 30)
    #[arg(long)]
    pub days: Option<u32>,
    /// Write output to PATH (JSON for summary; collapsed stacks for --flame)
    #[arg(long, value_name = "PATH")]
    pub export: Option<PathBuf>,
    /// Show aggregated inner-span breakdown
    #[arg(long)]
    pub inner: bool,
    /// Filter --inner / --flame to a specific command name
    #[arg(long, value_name = "NAME")]
    pub command: Option<String>,
    /// Emit Brendan Gregg collapsed-stack text (speedscope-compatible)
    #[arg(long)]
    pub flame: bool,
    /// One-sentence explanation for a command (with week-over-week delta)
    #[arg(long, value_name = "COMMAND")]
    pub explain: Option<String>,
    /// Delete old timing rows (use with --older-than)
    #[arg(long)]
    pub prune: bool,
    /// Age threshold for --prune, e.g. 90d or 30d
    #[arg(long, value_name = "Nd")]
    pub older_than: Option<String>,
    /// Re-enable local self-timing capture
    #[arg(long, conflicts_with = "opt_out")]
    pub opt_in: bool,
    /// Disable local self-timing capture (writes self_timing = false)
    #[arg(long, conflicts_with = "opt_in")]
    pub opt_out: bool,
}

/// Generate a disposable demonstration repo.
#[derive(Args, Debug)]
pub struct DemoArgs {
    /// Keep the demo repo and openable DEMO evidence zip after completion (required for the golden-path walkthrough; default: clean up)
    #[arg(short, long)]
    pub keep: bool,
    /// Output directory for the demo repo (default: ./ledgerful-demo)
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Overwrite a non-empty target directory
    #[arg(short, long)]
    pub force: bool,
}

/// High-performance trigram-based search (low-level, hidden).
#[derive(Args, Debug)]
pub struct SearchTrigramsArgs {
    /// Trigrams to search for (space separated)
    pub trigrams: Vec<String>,
    /// Limit results
    #[arg(long, short, default_value_t = 100)]
    pub limit: usize,
}
