use super::super::category_parser::{CATEGORY_LONG_HELP, CategoryValueParser};
use crate::ledger::types::Category;
use clap::{Args, Subcommand};
use std::path::PathBuf;

/// Perform a holistic project audit or history for an entity.
#[derive(Args, Debug)]
pub struct AuditArgs {
    /// Entity path to audit (e.g. src/main.rs)
    #[arg(short, long, conflicts_with = "pos_entity")]
    pub entity: Option<String>,
    /// Entity path to audit (positional fallback)
    #[arg(hide = true)]
    pub pos_entity: Option<String>,
    /// Include unaudited drift in the report
    #[arg(long, short)]
    pub include_unaudited: bool,
    /// Maximum number of entries to display
    #[arg(long, short, default_value_t = 10)]
    pub limit: usize,
    /// Offset for pagination
    #[arg(long, default_value_t = 0)]
    pub offset: usize,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand, Debug)]
pub enum IntentCommands {
    /// Launch the interactive intent confirmation UI with mock data
    Demo,
}

#[derive(Subcommand, Debug)]
pub enum AdrSubcommands {
    /// Export MADR files from ledger history
    Export {
        /// Output path for ADR files
        #[arg(short, long, alias = "output-dir", default_value = "docs/adr")]
        output: String,
        /// Filter entries from the last N days
        #[arg(short, long)]
        days: Option<u64>,
    },
    /// Update lifecycle status of an ADR
    UpdateStatus {
        /// ADR ID (transaction ID or prefix)
        adr_id: String,
        /// New status
        #[arg(value_enum)]
        status: crate::ledger::types::AdrStatus,
    },
    /// Link an ADR as superseding another
    Link {
        /// Current ADR ID
        adr_id: String,
        /// ID of the ADR being superseded
        #[arg(short, long)]
        supersedes: String,
    },
    /// Record a review for an ADR
    Review {
        /// ADR ID
        adr_id: String,
        /// Optional review notes
        #[arg(short, long)]
        message: Option<String>,
    },
    /// List all ADRs in the ledger
    List,
}

#[derive(Subcommand, Debug)]
pub enum ValidatorSubcommands {
    /// List all registered commit validators
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Enable a commit validator
    Enable {
        /// Name of the validator
        name: String,
    },
    /// Disable a commit validator
    Disable {
        /// Name of the validator
        name: String,
    },
    /// Remove a commit validator from the registry
    Remove {
        /// Name of the validator
        name: String,
    },
    /// Check validator executables and report health
    Doctor,
}

#[derive(Subcommand, Debug)]
pub enum RegisterCommands {
    /// Register a forbidden term (tech stack enforcement)
    Rule {
        /// Forbidden term or technology name
        term: String,
        /// Ledger category for the rule (ARCHITECTURE, FEATURE, BUGFIX, REFACTOR, INFRA, SECURITY, TOOLING, DOCS, CHORE)
        #[arg(short, long, value_parser = CategoryValueParser, long_help = CATEGORY_LONG_HELP)]
        category: Category,
        /// Reason for prohibition
        #[arg(short, long)]
        reason: String,
    },
    /// Register a commit validator script
    Validator {
        /// Name of the validator
        name: String,
        /// Command to execute (supports {entity} placeholder)
        #[arg(short = 'x', long)]
        command: String,
        /// Category this validator applies to (or 'ALL')
        #[arg(short, long)]
        category: String,
        /// Timeout in seconds
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },
}

#[derive(Subcommand, Debug)]
#[command(after_help = "\
Tips:
  ledger list            Alias for `ledger status` (active transactions)
  ledger history <q>     Alias for `ledger search <q>` (FTS; query required)
  Use `ledger status --all` for chronological list; `ledger search <q>` for FTS.
")]
pub enum LedgerCommands {
    /// Start a new change transaction
    Start {
        /// Entity path to track
        entity: String,
        /// Category (ARCHITECTURE, FEATURE, BUGFIX, REFACTOR, INFRA, SECURITY, TOOLING, DOCS, CHORE)
        #[arg(short, long, value_parser = CategoryValueParser, long_help = CATEGORY_LONG_HELP)]
        category: Category,
        /// Intent message for the change
        #[arg(short, long)]
        message: String,
    },
    /// Finalize and commit a change transaction
    Commit {
        /// Transaction ID to commit (optional, defaults to current)
        tx_id: Option<String>,
        /// Summary of the change
        #[arg(short, long)]
        summary: String,
        /// Reason for the change (Architecture Decision)
        #[arg(short, long)]
        reason: String,
        /// Mark as a breaking change
        #[arg(long)]
        breaking: bool,
        /// Bypass verification gate enforcement
        #[arg(long)]
        force: bool,
        /// Create a git commit after the ledger commit succeeds
        #[arg(long)]
        with_git: bool,
        /// Override the generated git commit message
        #[arg(long, requires = "with_git")]
        git_message: Option<String>,
        /// Skip adding a git Signed-off-by trailer
        #[arg(long, requires = "with_git")]
        no_signoff: bool,
        /// Print the git commit command without executing it
        #[arg(long, requires = "with_git")]
        dry_run: bool,
    },
    /// Roll back an active transaction
    Rollback {
        /// Transaction ID to rollback (optional, defaults to current)
        tx_id: Option<String>,
        /// Reason for the rollback
        #[arg(short, long)]
        reason: String,
    },
    /// Record a surgical atomic change without a full session
    Atomic {
        /// Entity path
        entity: String,
        /// Category (ARCHITECTURE, FEATURE, BUGFIX, REFACTOR, INFRA, SECURITY, TOOLING, DOCS, CHORE)
        #[arg(short, long, value_parser = CategoryValueParser, long_help = CATEGORY_LONG_HELP)]
        category: Category,
        /// Summary
        #[arg(short, long)]
        summary: String,
        /// Reason
        #[arg(short, long)]
        reason: String,
        /// Bypass verification gate enforcement
        #[arg(long)]
        force: bool,
    },
    /// Show pending/drift status; names workRoot (cwd or -C)
    #[command(visible_alias = "list")]
    Status {
        /// Show all historical transactions
        #[arg(short, long)]
        all: bool,
        /// Filter status by entity path
        #[arg(short, long)]
        entity: Option<String>,
        /// Output a compact view
        #[arg(short, long)]
        compact: bool,
        /// Exit non-zero on would-block conditions (enforce: 1; observe: 0 unless --strict-observe-signal)
        #[arg(long)]
        exit_code: bool,
        /// In observe mode with --exit-code, exit 2 for would-block conditions (distinct from enforce's 1)
        #[arg(long = "strict-observe-signal")]
        strict_observe_signal: bool,
        /// Perform signature verification and exit with 1 if signatures are invalid
        #[arg(long = "verify-signatures")]
        verify_signatures: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Aggregate posture across all discovered repos on disk
        #[arg(long)]
        global: bool,
        /// Scope the global rollup to a single repo path
        #[arg(long, requires = "global")]
        repo: Option<String>,
        /// Force a fresh walk of the configured roots
        #[arg(long, requires = "global")]
        reindex: bool,
        /// Disable the global rollup view in user config
        #[arg(long, conflicts_with = "opt_in", requires = "global")]
        opt_out: bool,
        /// Re-enable the global rollup view in user config
        #[arg(long, conflicts_with = "opt_out", requires = "global")]
        opt_in: bool,
    },
    /// Recover a promote-failed or HEAD-matching orphan sidecar
    RecoverOrphan {
        /// Promote the PENDING orphan to COMMITTED (Unverified) and drop the sidecar
        #[arg(long, conflicts_with = "abandon")]
        promote: bool,
        /// Abandon the orphan: write a MAINTENANCE row with --reason, rollback pending, drop sidecar
        #[arg(long, conflicts_with = "promote")]
        abandon: bool,
        /// Required with --abandon: durable reason (never silent delete)
        #[arg(long, short)]
        reason: Option<String>,
    },
    /// Register a new tech stack rule or commit validator
    Register {
        #[command(subcommand)]
        command: RegisterCommands,
    },
    /// Show active tech stack enforcement rules
    Stack {
        /// Optional ledger category filter (positional; same aliases as --category)
        #[arg(value_parser = CategoryValueParser, long_help = CATEGORY_LONG_HELP)]
        category: Option<Category>,
    },
    /// Architectural Decision Records (MADR format)
    Adr {
        #[command(subcommand)]
        command: AdrSubcommands,
    },
    /// Manage commit validators
    Validator {
        #[command(subcommand)]
        command: ValidatorSubcommands,
    },
    /// Show the entity graph neighborhood governed by a transaction
    Graph(crate::commands::ledger_graph::LedgerGraphArgs),
    /// Full-text search across ledger history
    #[command(visible_alias = "history")]
    Search {
        /// Search query
        query: String,
        /// Filter by category (canonical or alias; case-insensitive)
        #[arg(short, long, value_parser = CategoryValueParser, long_help = CATEGORY_LONG_HELP)]
        category: Option<Category>,
        /// Number of days to look back
        #[arg(short, long)]
        days: Option<u64>,
        /// Filter by breaking changes only
        #[arg(short, long)]
        breaking: bool,
        /// Limit results
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
        /// Offset for pagination
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Include ROLLBACK entries (omitted by default; ranked after non-rollback)
        #[arg(long)]
        include_rollback: bool,
    },
    /// Reconcile detected drift with a transaction or pattern
    Reconcile {
        /// Transaction ID to associate drift with
        #[arg(short, long)]
        tx_id: Option<String>,
        /// File pattern to reconcile (glob)
        #[arg(short, long)]
        pattern: Option<String>,
        /// Reconcile all current drift
        #[arg(long)]
        all: bool,
        /// Reason for reconciliation
        #[arg(short, long)]
        reason: Option<String>,
    },
    /// Adopt drift as a new committed transaction
    Adopt {
        /// File pattern to adopt
        #[arg(short, long)]
        pattern: Option<String>,
        /// Adopt all current drift
        #[arg(long)]
        all: bool,
        /// Category for the new transaction (ARCHITECTURE, FEATURE, BUGFIX, …)
        #[arg(short, long, value_parser = CategoryValueParser, long_help = CATEGORY_LONG_HELP)]
        category: Category,
        /// Summary for the new transaction
        #[arg(short, long)]
        summary: String,
        /// Reason for the new transaction
        #[arg(short, long)]
        reason: String,
    },
    /// Perform a holistic project audit or history for an entity
    Audit {
        /// Entity path to audit (e.g. src/main.rs)
        #[arg(short, long, conflicts_with = "pos_entity")]
        entity: Option<String>,
        /// Entity path to audit (positional fallback)
        #[arg(hide = true)]
        pos_entity: Option<String>,
        /// Include unaudited drift in the report
        #[arg(long, short)]
        include_unaudited: bool,
        /// Maximum number of entries to display
        #[arg(long, short, default_value_t = 10)]
        limit: usize,
        /// Offset for pagination
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Add a lightweight note/lesson to a transaction for an entity
    Note {
        /// Entity path
        entity: String,
        /// The note content
        #[arg(required_unless_present = "message")]
        note: Option<String>,
        /// The note content (takes precedence over positional note)
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Re-sign ledger entries (upgrade legacy signatures and/or repair invalid ones)
    ReSign {
        /// Re-sign a single transaction by id or prefix
        #[arg(short, long, conflicts_with_all = ["all_invalid", "all"])]
        tx: Option<String>,
        /// Re-sign all entries whose stored signatures fail verification (key-repair)
        #[arg(long, conflicts_with_all = ["tx", "all"])]
        all_invalid: bool,
        /// Upgrade LOCAL entries with sig_version below current, and repair invalid signatures
        #[arg(long, conflicts_with_all = ["tx", "all_invalid"])]
        all: bool,
        /// Preview candidates and keys that would be used; do not mutate
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Skip interactive confirmation and proceed with backup + re-sign
        #[arg(long)]
        yes: bool,
    },
    /// Garbage collect orphaned or stale ledger entries
    Gc {
        /// Remove PENDING transactions older than TTL
        #[arg(long)]
        stale: bool,
        /// Remove transactions with no corresponding git commit
        #[arg(long)]
        orphans: bool,
        /// Time-to-live for PENDING transactions in hours (used with --stale)
        #[arg(long, default_value_t = 72)]
        ttl_hours: u64,
        /// Force removal without confirmation
        #[arg(short, long)]
        force: bool,
        /// Show what would be removed without actually deleting it
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
    /// Resume a pending transaction by ID or the most recent pending transaction
    Resume {
        /// Transaction ID to resume (optional, defaults to most recent pending)
        #[arg(short, long = "tx", value_name = "TX_ID")]
        tx_id: Option<String>,
    },
    /// Export committed ledger entries as a stable JSON provenance artifact
    ExportProvenance {
        /// Output path for the JSON provenance file (default: stdout)
        #[arg(short, long, value_name = "PATH")]
        out_path: Option<PathBuf>,
        /// Overwrite an existing output file
        #[arg(short, long)]
        force: bool,
    },
    /// Export a redacted, cryptographically verifiable public ledger bundle
    ExportPublic {
        /// Output directory for the bundle files
        #[arg(short, long, value_name = "DIR")]
        output: PathBuf,
        /// Sign the manifest with the bot keypair
        #[arg(long)]
        sign: bool,
        /// Override the bot key directory (holds bot key, bot public key, and pseudonym secret)
        #[arg(long, value_name = "PATH")]
        key: Option<PathBuf>,
    },
    /// Repair a stale post-commit hook sidecar after a crash
    HookRepair {
        /// Roll back the stale transaction and remove the sidecar
        #[arg(short, long)]
        force: bool,
    },
}
