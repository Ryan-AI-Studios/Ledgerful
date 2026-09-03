use crate::commands::ask::Backend;
use clap::Args;
use std::path::PathBuf;

/// Presentation mode for `scan --impact` (0227). Unknown values are clap-rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ScanImpactMode {
    /// Docs-only lead: actionable couplings / test gaps; skip crate co-change trivia.
    Docs,
}

/// Scan git changes and identify affected symbols.
#[derive(Args, Debug)]
pub struct ScanArgs {
    /// Run impact analysis on changes
    #[arg(short, long)]
    pub impact: bool,
    /// Output a high-level summary only
    #[arg(short, long)]
    pub summary: bool,
    /// Git scan summary JSON (`kind: gitScan`). Full impact packet: `--impact --json`.
    #[arg(short, long)]
    pub json: bool,
    /// Write JSON output to file (gitScan without `--impact`; impact packet with `--impact`)
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Git ref to compare against instead of working-tree status. Used in CI.
    #[arg(long, value_name = "REF")]
    pub base_ref: Option<String>,
    /// PR-style git range, e.g. `main...HEAD` or `main..HEAD`. Mutually
    /// exclusive with --impact.
    #[arg(long, value_name = "RANGE")]
    pub pr: Option<String>,
    /// Output format for PR scan: `json` (machine-readable) or `text`
    /// (human-readable). Requires --pr.
    #[arg(long, value_name = "FORMAT")]
    pub format: Option<String>,
    /// Structural call-graph blast hop depth (default 1; max 2 on CLI).
    /// Not deploy highBlastResources. Hop > 1 only walks high-confidence edges
    /// (RESOLVED or scip:). Not a complete call graph.
    #[arg(long, value_name = "N")]
    pub blast_depth: Option<u32>,
    /// Prospective paths for impact (as if changed). Mutually exclusive with
    /// --base-ref. Comma-separated and/or repeated. Cap 50. Requires --impact.
    /// Does not rewrite latest-impact.json.
    #[arg(long, value_name = "PATH", value_delimiter = ',')]
    pub paths: Vec<String>,
    /// Include process/governance temporal couplings in risk + readSet (pathMode=all)
    #[arg(long)]
    pub include_governance: bool,
    /// Impact presentation mode. Requires `--impact`. Currently `docs` only.
    #[arg(long, value_enum, value_name = "MODE")]
    pub mode: Option<ScanImpactMode>,
    /// Expand remaining temporal couplings in docs-mode human output (default: collapsed).
    #[arg(long)]
    pub full: bool,
}

/// Analyze impact of current changes.
#[derive(Args, Debug)]
pub struct ImpactArgs {
    /// Traverse all parent commits for temporal coupling
    #[arg(long)]
    pub all_parents: bool,
    /// Output a concise summary
    #[arg(short, long)]
    pub summary: bool,
    /// Enable telemetry coverage analysis
    #[arg(long)]
    pub telemetry: bool,
    /// Run dead-code analysis on affected files
    #[arg(long)]
    pub dead_code: bool,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
    /// Write output to file
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Structural call-graph blast hop depth (default 1; max 2 on CLI).
    /// Not deploy highBlastResources. Hop > 1 only walks high-confidence edges
    /// (RESOLVED or scip:). Not a complete call graph.
    #[arg(long, value_name = "N")]
    pub blast_depth: Option<u32>,
    /// Prospective paths (as if changed). Comma-separated and/or repeated.
    /// Cap 50. In-memory only — does not rewrite latest-impact.json.
    #[arg(long, value_name = "PATH", value_delimiter = ',')]
    pub paths: Vec<String>,
    /// Include process/governance temporal couplings in risk (pathMode=all)
    #[arg(long)]
    pub include_governance: bool,
}

/// Budgeted agent change packet (impact + doctor + ledger + readSet).
#[derive(Args, Debug)]
pub struct ChangeContextArgs {
    /// Emit pure schema-v1 JSON on stdout (0093 machine mode)
    #[arg(long)]
    pub json: bool,
    /// Detail level: minimal (default) or standard
    #[arg(long, value_name = "LEVEL", default_value = "minimal")]
    pub detail: String,
    /// Cap readSet length (default 20)
    #[arg(long, value_name = "N", default_value_t = 20)]
    pub max_files: usize,
    /// Git ref for structural impact/readSet/risk (doctor+ledger stay present-tense)
    #[arg(long, value_name = "REF")]
    pub base_ref: Option<String>,
    /// Structural blast hop depth (default 1; max 2 on CLI)
    #[arg(long, value_name = "N")]
    pub blast_depth: Option<u32>,
    /// Prospective paths (as if changed). Mutually exclusive with --base-ref.
    /// Comma-separated and/or repeated. Cap 50.
    #[arg(long, value_name = "PATH", value_delimiter = ',')]
    pub paths: Vec<String>,
    /// Include process/governance temporal couplings in risk + readSet (pathMode=all)
    #[arg(long)]
    pub include_governance: bool,
}

/// One-shot agent session briefing (git + ledger + doctor + change-context + hotspots).
#[derive(Args, Debug)]
pub struct SessionArgs {
    /// Emit schema-v1 JSON envelope (`kind: session`) on stdout (0093 machine mode)
    #[arg(long)]
    pub json: bool,
}

/// Clap bag for `search` (query tokens + flags). Not
/// [`crate::commands::search::SearchArgs`] (execution DTO).
#[derive(Args, Debug)]
pub struct SearchCliArgs {
    /// Query words (unquoted multi-word OK). Flags may appear before or after.
    #[arg(value_name = "QUERY", num_args = 1.., required = true)]
    pub query: Vec<String>,
    /// Use regular expression search
    #[arg(short, long)]
    pub regex: bool,
    /// Use semantic search (requires local model and indexed snippets)
    #[arg(short, long)]
    pub semantic: bool,
    /// Limit the number of results
    #[arg(short, long, default_value_t = 10)]
    pub limit: usize,
    /// Force re-index before searching
    #[arg(short, long)]
    pub index: bool,
    /// Output a single JSON envelope for agents (`schemaVersion` 1)
    #[arg(long, conflicts_with = "json_lines")]
    pub json: bool,
    /// Output NDJSON BridgeRecord lines (legacy; pre-0136 `--json` behavior)
    #[arg(long, conflicts_with = "json")]
    pub json_lines: bool,
    /// Automatically run incremental index before searching if the index is stale
    #[arg(long)]
    pub auto_index: bool,
    /// Use hybrid search (combines regex and BM25 results)
    #[arg(long)]
    pub hybrid: bool,
}

/// Ask Gemini or a local model for assistance based on the current context.
#[derive(Args, Debug)]
pub struct AskArgs {
    /// Query words (unquoted multi-word OK). Prefer flags before words.
    /// Flags (e.g. --semantic, --backend) must precede unquoted query words;
    /// post-query flags are treated as query text.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    pub query: Vec<String>,
    /// Use semantic search for code snippets instead of full impact context
    #[arg(long, short)]
    pub semantic: bool,
    /// Maximum number of code snippets to include in context
    #[arg(long, short, default_value_t = 10)]
    pub limit: usize,
    /// Gemini interaction mode
    #[arg(long, short, default_value = "analyze")]
    pub mode: crate::gemini::modes::GeminiMode,
    /// Enable narrative mode (Senior Architect summary)
    #[arg(long)]
    pub narrative: bool,
    /// Backend to use (local, gemini, ollama-cloud, openrouter, or auto)
    #[arg(long)]
    pub backend: Option<Backend>,
    /// Automatically run incremental index before querying if the index is stale
    #[arg(long)]
    pub auto_index: bool,
    /// Per-request timeout in seconds for LLM backend calls.
    /// When omitted: Local uses `local_model.timeout_secs` (default 300; cold load
    /// headroom); Gemini uses `gemini.timeout_secs` / 120-class starter; other cloud
    /// providers default short (15s). Explicit value always wins for all backends.
    #[arg(long)]
    pub timeout: Option<u64>,
    /// Disable Knowledge Graph BM25 fallback when semantic index is empty
    #[arg(long)]
    pub no_kg_fallback: bool,
    /// Compute a fresh ImpactPacket in-memory from the live working tree
    /// instead of reading the cached packet, and suppress the stale-impact
    /// warning. Equivalent to running `scan --impact` before `ask` but
    /// without writing the report. See also `[ask].auto_scan_default`.
    #[arg(long)]
    pub auto_scan: bool,
}

/// Health check for Ledgerful and local model stack.
#[derive(Args, Debug)]
pub struct DoctorArgs {
    /// Emit pure schema-v1 JSON on stdout (severity, readyForPublish, findings)
    #[arg(long)]
    pub json: bool,
    /// Refresh stale Ledgerful marker-bounded hook blocks to current product templates
    #[arg(long = "apply-hook-refresh", group = "fix_or_refresh")]
    pub apply_hook_refresh: bool,
    /// Pin observe-mode signing remediations (keys / min_sig_version / phantom ack).
    /// Never runs `ledger re-sign --all`. Conflicts with `--apply-hook-refresh`.
    #[arg(long, conflicts_with = "apply_hook_refresh", group = "fix_or_refresh")]
    pub fix: bool,
    /// Confirm `--fix` mutations (non-interactive). Requires `--fix`.
    #[arg(long, requires = "fix")]
    pub yes: bool,
    /// With `--fix` or `--apply-hook-refresh`, report would-apply without writing
    #[arg(long = "dry-run", requires = "fix_or_refresh")]
    pub dry_run: bool,
    /// Expand collapsed hygiene findings (optional/info) in human output
    #[arg(long)]
    pub full: bool,
}

/// Run verification plan (predictive Bayesian testing).
#[derive(Args, Debug)]
pub struct VerifyArgs {
    /// Optional specific command or step to run
    pub command: Option<String>,
    /// Transaction ID to associate with this verification run
    #[arg(long)]
    pub tx_id: Option<String>,
    /// Timeout in seconds
    #[arg(long, short, default_value_t = 600)]
    pub timeout: u64,
    /// Disable Bayesian failure prediction
    #[arg(long)]
    pub no_predict: bool,
    /// Explain failure probability via local LLM for a specific entity
    #[arg(long)]
    pub explain: bool,
    /// Entity path for verification explanation (use with --explain; does not narrow executed steps)
    #[arg(long, short)]
    pub entity: Option<String>,
    /// Show detailed health of the verification system
    #[arg(long)]
    pub health: bool,
    /// Mathematically verify all transaction signatures in the ledger
    #[arg(long)]
    pub signatures: bool,
    /// Verify ledger chain continuity end-to-end (requires --signatures or
    /// validates the chain linkage separately)
    #[arg(long)]
    pub chain: bool,
    /// Compare the live chain against a retained checkpoint (SOC2 zip or bare
    /// chain_head.json). Default: live must extend or equal the export head
    /// (ancestor/prefix). Use `--exact` for snapshot equality (freeze check).
    #[arg(long, value_name = "PATH")]
    pub against_export: Option<std::path::PathBuf>,
    /// With `--against-export`, require full head equality (latest/genesis/length)
    /// instead of checkpoint (extends-or-equals) semantics.
    #[arg(long)]
    pub exact: bool,
    /// Treat unsigned LOCAL rows as failures even when require_signing is false
    #[arg(long = "strict-signatures")]
    pub strict_signatures: bool,
    /// Show the verification plan without executing any commands
    #[arg(long)]
    pub dry_run: bool,
    /// Verification scope: `fast` or `full` (default: `full`).
    /// `fast` always runs fmt + clippy; only test selection is scoped via
    /// `test_mapping`. When mapping cannot scope, refuses (exit ≠ 0) rather
    /// than surprise-running full — use `--allow-full-fallback` for the
    /// old full path, or `--scope full` / SharedInfra. Pre-push uses
    /// `fast`. See `docs/verify-performance.md`.
    #[arg(long, default_value = "full")]
    pub scope: crate::verify::plan::VerifyScope,
    /// Refresh empty / unverifiable `test_mapping` before scoped selection
    /// on `--scope fast`. Head-lag (index head behind packet/HEAD) auto-
    /// repairs once without this flag — see `docs/verify-performance.md`.
    /// On success scopes; if still cannot scope, refuses (unless
    /// `--allow-full-fallback`). Opt-in for Empty / PacketHeadMissing.
    #[arg(long)]
    pub auto_index: bool,
    /// When `--scope fast` cannot map tests, run the full suite with an
    /// announcement (0061 behavior) instead of refusing. Default off so
    /// agents never surprise multi-minute hangs. Pre-push does not set this.
    #[arg(long)]
    pub allow_full_fallback: bool,
    /// Emit a versioned machine-readable result object on stdout (schema
    /// version 1). Selects machine mode so human `cli_summary` lines cannot
    /// precede or follow the JSON. See `docs/agent-output-contract.md`.
    #[arg(long)]
    pub json: bool,
}

/// Ledger pending/drift JSON (`--json`) and compact (`--compact`).
/// Not a full alias of `ledger status`.
#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Output as JSON (same payload as `ledger status --json`)
    #[arg(long)]
    pub json: bool,
    /// One-line pending/drift summary (same as `ledger status --compact`)
    #[arg(long)]
    pub compact: bool,
}
