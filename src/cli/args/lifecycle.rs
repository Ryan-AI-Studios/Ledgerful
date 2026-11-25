use clap::{Args, Subcommand};
use std::path::PathBuf;

/// Initialize Ledgerful in the current repository.
#[derive(Args, Debug)]
pub struct InitArgs {
    /// Skip adding `.ledgerful/` to `.gitignore` (does not overwrite an existing config)
    #[arg(short, long)]
    pub force: bool,
    /// Start in enforce mode instead of the default observe mode
    #[arg(long)]
    pub enforce: bool,
    /// Fill product-default coverage/bridge keys and install nightly if missing
    #[arg(long)]
    pub operator_pack: bool,
}

/// Guided onboarding wizard (welcome → init → doctor → first scan → success).
#[derive(Args, Debug)]
pub struct SetupArgs {
    /// Skip all prompts, accept defaults (for CI/scripted use)
    #[arg(short, long)]
    pub yes: bool,
    /// Skip the first-scan step
    #[arg(long)]
    pub skip_scan: bool,
}

/// Reset Ledgerful state or configuration.
#[derive(Args, Debug)]
pub struct ResetArgs {
    /// Remove configuration file
    #[arg(long)]
    pub remove_config: bool,
    /// Remove local rules
    #[arg(long)]
    pub remove_rules: bool,
    /// Reset the ledger (history and pending transactions)
    #[arg(long)]
    pub include_ledger: bool,
    /// Remove all state and configuration (total reset)
    #[arg(long, short)]
    pub all: bool,
    /// Skip confirmation prompt
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Show what files/directories would be deleted without deleting them
    #[arg(long = "dry-run")]
    pub dry_run: bool,
}

/// Update the Ledgerful binary or migrate repository state.
#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// Perform repository state migration (re-index and schema upgrade)
    #[arg(long)]
    pub migrate: bool,
    /// Update the Ledgerful binary to the latest version
    #[arg(long)]
    pub binary: bool,
    /// Skip confirmation prompts
    #[arg(long, short)]
    pub force: bool,
    /// Force unlock CozoDB by terminating other running Ledgerful processes
    #[arg(long = "force-unlock")]
    pub force_unlock: bool,
    /// Accepted on `update --migrate` and unused (`IndexArgs.fast` stays false). Index `--fast` is a different command.
    #[arg(long)]
    pub fast: bool,
    /// Show what update actions would be performed without executing them.
    /// `--check` is an alias for `--dry-run` (preview without executing), not a
    /// version-check.
    #[arg(long = "dry-run", visible_alias = "check")]
    pub dry_run: bool,
    /// Rewrite retired Ledgerful hook commands to invoke `ledgerful`
    #[arg(long = "repair-hooks")]
    pub repair_hooks: bool,
}

#[derive(Subcommand, Debug)]
pub enum GateCommands {
    /// Show or set the gate mode
    #[command(visible_alias = "status")]
    Mode {
        /// Set mode: observe or enforce
        mode: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ArchitectureCommands {
    /// Report observed dependency boundaries and component cycles
    Check {
        /// Read this architecture rules file instead of .ledgerful/architecture.toml
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
        /// Compare against this named analysis baseline
        #[arg(long, value_name = "NAME")]
        baseline: Option<String>,
        /// Write the architectureCheck JSON envelope
        #[arg(long)]
        json: bool,
        /// Exit 1 on findings or unknown evidence, or on present or unknown regression with a baseline
        #[arg(long)]
        fail: bool,
    },
    /// Estimate how one additional resolved call would affect boundaries and cycles
    #[command(
        after_help = "Prepare without --context, then replay the same request with the returned token.\n\
        Output is always JSON with permission unknown. Independently check the actual edit.\n\
        The measured engine-size R2 graph exceeds the 25,000 raw-row ceiling and returns unknown.\n\
        Raw observations can repeat a pair and are counted separately from projected pairs."
    )]
    Propose {
        /// Repository-relative path of the calling file
        #[arg(long, value_name = "PATH", allow_hyphen_values = true)]
        from: String,
        /// Repository-relative path of the called file
        #[arg(long, value_name = "PATH", allow_hyphen_values = true)]
        to: String,
        /// Hypothetical observation kind (fileCall)
        #[arg(long, value_name = "KIND", allow_hyphen_values = true)]
        kind: String,
        /// Full lowercase object ID of the current committed and indexed HEAD
        #[arg(long, value_name = "FULL-HEAD")]
        head: String,
        /// Observed context token from preparing the same proposal
        #[arg(long, value_name = "TOKEN")]
        context: Option<String>,
        /// Write the architectureProposal JSON envelope (also the default)
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum PolicyCommands {
    /// Evaluate declared policy against PR/diff/ledger state
    #[command(visible_alias = "evaluate")]
    Check {
        /// PR-style git range, e.g. `main...HEAD` or `main..HEAD`
        #[arg(long, value_name = "RANGE")]
        pr: Option<String>,
        /// Risk threshold that fails the check: off | low | medium | high
        /// (overrides config `rules.fail_on` for this run)
        #[arg(long, value_name = "LEVEL")]
        fail_on: Option<String>,
        /// Trusted policy file path (org/CI). When set, this path is used
        /// instead of base-branch or working-tree policy resolution.
        #[arg(long, value_name = "PATH")]
        policy: Option<PathBuf>,
        /// Output format: `json` (machine contract) or `text` (human report).
        /// Default: text.
        #[arg(long, value_name = "FORMAT")]
        format: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
#[command(after_help = "\
Examples:
  ledgerful config view                        Show resolved configuration
  ledgerful config show                        Alias for `config view`
  ledgerful config view --section local_model
  ledgerful config verify                      Verify config and environment health
  ledgerful config verify --verbose            Include default rows
  ledgerful config set coverage.enabled=true   Set a configuration value
  ledgerful config diff                        Show declared vs inferred config
")]
pub enum ConfigCommands {
    /// Verify current configuration and environment health
    #[command(after_help = "\
Examples:
  ledgerful config verify --verbose            Include default rows
")]
    Verify {
        /// Output results as JSON
        #[arg(long)]
        json: bool,
        /// Filter by specific section name (e.g. backend, semantic)
        #[arg(long, short)]
        section: Option<String>,
        /// Include defaults that are normally hidden
        #[arg(long, short)]
        verbose: bool,
    },
    /// View resolved project configuration
    #[command(visible_alias = "show")]
    #[command(after_help = "\
Examples:
  ledgerful config view --section local_model
  ledgerful config view --section local_model --key prefer_local
")]
    View {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Filter view by section (e.g. local_model)
        #[arg(long, short)]
        section: Option<String>,
        /// Filter view by key within section (requires --section, or searches top-level)
        #[arg(long, short)]
        key: Option<String>,
    },
    /// Manage environment and config schemas
    Schema {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show differences between declared and inferred config
    Diff {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Show all env vars including internal ones (no filtering)
        #[arg(long)]
        show_internal: bool,
    },
    /// Set a configuration value in .ledgerful/config.toml by dotted key
    /// (e.g. `coverage.services.enabled=true`). Preserves comments and
    /// formatting. Value is parsed as TOML (bool/int/float/string/array);
    /// an unquoted bareword that is not valid TOML is stored as a string.
    Set {
        /// Dotted key and TOML value, e.g. `coverage.services.enabled=true`
        key_value: String,
    },
    /// Remove an array-of-tables entry from .ledgerful/config.toml by
    /// indexed key (e.g. `ask.providers.priority[1]`).
    Unset {
        /// Dotted key with array index, e.g. `ask.providers.priority[1]`
        key: String,
    },
}

/// Subcommands of `ledgerful release`. Bare `release` defaults to [`Self::Pins`].
#[derive(Subcommand, Debug)]
pub enum ReleaseCommands {
    /// Diff GitHub Latest vs packaging templates, brew/scoop remotes, and npm engine pin
    Pins,
}
