//! `ledgerful symbols` — scoped, index-backed symbol inventory (track 0163).
//!
//! Filters: `--path` (prefix), `--changed` (WT ∩ indexed), `--kind`, `--pub`.
//! Always limit-capped (default 200, hard max 5000). Pure `--json` schemaVersion 1.

use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::git::status::{collect_changed_files_for_filter, normalize_filter_path};
use crate::index::staleness::{try_auto_index, warn_if_stale};
use crate::index::symbols::SymbolKind;
use crate::state::storage::StorageManager;
use clap::Args;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// Default row cap when `-l/--limit` is omitted.
pub const DEFAULT_LIMIT: u64 = 200;
/// Hard maximum accepted by clap `range(1..=5000)`.
pub const MAX_LIMIT: u64 = 5000;

#[derive(Args, Debug)]
#[command(after_help = "\
Notes:
  --path is a path *prefix* match (file_path == prefix OR starts with prefix/),
  not a substring filter like `endpoints --path`.
  When prefix yields zero matches, file-form resolve applies (unique only):
  X.rs ↔ X/mod.rs, extensionless, unique suffix; ambiguous refuses.
  Class and Interface kinds are accepted but currently unpopulated by extractors
  (reserved vocabulary).
  --changed includes Deleted paths that are still in the index until re-index
  (use --auto-index to refresh).

Examples:
  ledgerful symbols --path src/verify --pub --limit 200
  ledgerful symbols --changed --json
  ledgerful symbols --kind fn --path src/cli --json
  ledgerful symbols --path src/commands/ --pub --limit 50 --json
  ledgerful symbols --path src/commands/doctor/mod.rs --limit 20
")]
pub struct SymbolsArgs {
    /// Path prefix filter (not substring). Trailing slash is trimmed.
    #[arg(long)]
    pub path: Option<String>,

    /// Only symbols whose indexed file_path is in the working-tree change set.
    /// Includes Deleted paths still present in the index until re-index.
    #[arg(long)]
    pub changed: bool,

    /// Filter by symbol kind (case-insensitive; aliases: fn, struct, enum, trait,
    /// mod/module, method, class, type, const, var, interface). Class/Interface
    /// are reserved (unpopulated today).
    #[arg(long)]
    pub kind: Option<String>,

    /// Only public symbols (`is_public = 1`)
    #[arg(long = "pub")]
    pub pub_only: bool,

    /// Maximum symbols to emit (default 200; hard max 5000)
    #[arg(
        short = 'l',
        long,
        default_value_t = DEFAULT_LIMIT,
        value_parser = clap::value_parser!(u64).range(1..=MAX_LIMIT)
    )]
    pub limit: u64,

    /// Pure machine JSON on stdout (schemaVersion 1)
    #[arg(long)]
    pub json: bool,

    /// Run incremental index when missing/stale before listing
    #[arg(long)]
    pub auto_index: bool,
}

impl SymbolsArgs {
    /// Whether `--json` is set (machine-output selection).
    pub fn wants_json(&self) -> bool {
        self.json
    }

    /// Long flag names that are present (values stripped). Used for `argv_hash` shape.
    pub fn present_flag_names(&self) -> Vec<&'static str> {
        let mut f = Vec::new();
        if self.path.is_some() {
            f.push("path");
        }
        if self.changed {
            f.push("changed");
        }
        if self.kind.is_some() {
            f.push("kind");
        }
        if self.pub_only {
            f.push("pub");
        }
        if self.limit != DEFAULT_LIMIT {
            f.push("limit");
        }
        if self.json {
            f.push("json");
        }
        if self.auto_index {
            f.push("auto_index");
        }
        f
    }
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Always-present scope fields (`path` / `kind` null when unset — B3).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SymbolsScopeWire {
    pub path: Option<String>,
    pub changed: bool,
    pub kind: Option<String>,
    pub pub_only: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SymbolInventoryRow {
    pub name: String,
    pub kind: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<i64>,
    pub is_public: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualified_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

/// Additive path-identity note when `--path` zero-match fallback ran (0183).
/// Omitted when no file resolve was needed or attempted.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PathResolveNote {
    /// `resolved` | `ambiguous` | `notFound` after zero-match fallback.
    pub status: String,
    /// Stored `project_files` path when status is `resolved`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_path: Option<String>,
    /// Query string for ambiguous refuse.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Sorted candidate paths when status is `ambiguous` (capped for wire).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<String>>,
    /// True total candidate count before wire cap (honest human/JSON totals).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_total: Option<usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SymbolsJsonEnvelope {
    pub schema_version: u32,
    pub scope: SymbolsScopeWire,
    pub limit: u64,
    pub truncated: bool,
    pub result_count: usize,
    pub total_matching: usize,
    pub symbols: Vec<SymbolInventoryRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_status: Option<IndexStatus>,
    /// Present when `--path` zero-match file-identity fallback ran (0183).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_resolve: Option<PathResolveNote>,
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Normalize a `--path` prefix (D3 / M4):
/// `\`→`/`, strip leading `./` and `/`, trim trailing `/`.
/// Empty-after-trim → error.
pub fn normalize_path_prefix(raw: &str) -> Result<String> {
    let mut s = raw.replace('\\', "/");
    while let Some(stripped) = s.strip_prefix("./") {
        s = stripped.to_string();
    }
    while let Some(stripped) = s.strip_prefix('/') {
        s = stripped.to_string();
    }
    while s.ends_with('/') {
        s.pop();
    }
    if s.is_empty() {
        return Err(miette::miette!(
            "Invalid --path: empty after normalization (got {raw:?}). \
             Provide a non-empty path prefix such as `src/commands`."
        ));
    }
    Ok(s)
}

/// Parse kind aliases (case-insensitive) → canonical `SymbolKind` PascalCase (D5 / L2).
pub fn parse_kind_filter(raw: &str) -> Result<SymbolKind> {
    let lower = raw.trim().to_ascii_lowercase();
    let kind = match lower.as_str() {
        "fn" | "function" => SymbolKind::Function,
        "method" => SymbolKind::Method,
        "class" => SymbolKind::Class,
        "struct" => SymbolKind::Struct,
        "enum" => SymbolKind::Enum,
        "trait" => SymbolKind::Trait,
        "interface" => SymbolKind::Interface,
        "type" => SymbolKind::Type,
        "var" | "variable" => SymbolKind::Variable,
        "const" | "constant" => SymbolKind::Constant,
        "mod" | "module" => SymbolKind::Module,
        _ => {
            return Err(miette::miette!(
                "Unknown symbol kind {raw:?}. Accepted: Function, Method, Class, Struct, Enum, \
                 Trait, Interface, Type, Variable, Constant, Module (aliases: fn, struct, enum, \
                 trait, mod/module, method, class, type, const, var, interface)."
            ));
        }
    };
    Ok(kind)
}

/// Path membership key for filter set comparison (D4 / M3).
///
/// Always normalizes `\` → `/`. On **Windows** only, also lowercases so
/// membership is case-insensitive (LOWER both sides). On non-Windows hosts
/// (case-sensitive filesystems) the key preserves case.
fn path_membership_key(path: &str) -> String {
    let s = path.replace('\\', "/");
    if cfg!(windows) {
        s.to_ascii_lowercase()
    } else {
        s
    }
}

/// Whether `file_path` matches a normalized path prefix (equality or `prefix/…`).
///
/// Case policy matches [`path_membership_key`]: insensitive on Windows only.
fn path_matches_prefix(file_path: &str, prefix: &str) -> bool {
    let fp = path_membership_key(file_path);
    let p = path_membership_key(prefix);
    fp == p || fp.starts_with(&format!("{p}/"))
}

/// Escape SQL `LIKE` metacharacters for use with `ESCAPE '\\'`.
///
/// Order matches SQLite needs: backslash first, then `%` and `_`.
/// Same ESCAPE convention as `ledger::db::transactions::resolve_tx_id_fuzzy`.
fn escape_like_pattern(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

// ---------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct QueryFilters {
    /// Normalized path prefix (no trailing slash), when set.
    path_prefix: Option<String>,
    /// Canonical PascalCase kind string, when set.
    kind: Option<String>,
    pub_only: bool,
    /// When `Some`, restrict to these membership keys (via [`path_membership_key`]).
    /// Empty set → zero rows. Keys are lowercased on Windows only.
    changed_keys: Option<HashSet<String>>,
    limit: usize,
}

/// Internal row before wire mapping (includes id for sort stability / tests).
#[derive(Debug, Clone)]
struct QueriedSymbol {
    #[allow(dead_code)]
    id: i64,
    name: String,
    kind: String,
    path: String,
    line: Option<i64>,
    is_public: bool,
    qualified_name: Option<String>,
}

impl QueriedSymbol {
    fn into_wire(self) -> SymbolInventoryRow {
        let qn = self.qualified_name.filter(|q| !q.is_empty());
        SymbolInventoryRow {
            name: self.name,
            kind: self.kind,
            path: self.path.replace('\\', "/"),
            line: self.line,
            is_public: self.is_public,
            qualified_name: qn,
        }
    }
}

fn build_where_and_params(filters: &QueryFilters) -> (String, Vec<String>) {
    let mut where_sql = String::from(" WHERE 1=1");
    let mut params: Vec<String> = Vec::new();

    if let Some(prefix) = &filters.path_prefix {
        // D3: case-insensitive path prefix only on Windows (LOWER both sides).
        // Non-Windows: case-sensitive equality + substr prefix (not LIKE).
        // SQLite LIKE is ASCII case-insensitive by default even without LOWER, so
        // non-Windows must not use LIKE for prefix children (Codex R3 P2).
        // Windows LIKE still escapes `_`/`%` (F1 / true-prefix semantics).
        let key = if cfg!(windows) {
            prefix.to_ascii_lowercase()
        } else {
            prefix.clone()
        };
        if cfg!(windows) {
            let like_prefix = escape_like_pattern(&key);
            where_sql.push_str(
                " AND (LOWER(pf.file_path) = ? OR LOWER(pf.file_path) LIKE ? ESCAPE '\\')",
            );
            params.push(key);
            params.push(format!("{like_prefix}/%"));
        } else {
            // Case-sensitive: path == prefix OR path starts with "prefix/"
            // (bind prefix thrice: equality, length base, and "prefix/").
            where_sql.push_str(
                " AND (pf.file_path = ? OR substr(pf.file_path, 1, length(?) + 1) = ? || '/')",
            );
            params.push(key.clone());
            params.push(key.clone());
            params.push(key);
        }
    }

    if let Some(kind) = &filters.kind {
        where_sql.push_str(" AND ps.symbol_kind = ?");
        params.push(kind.clone());
    }

    if filters.pub_only {
        where_sql.push_str(" AND ps.is_public = 1");
    }

    if let Some(keys) = &filters.changed_keys {
        if keys.is_empty() {
            where_sql.push_str(" AND 1=0");
        } else {
            // Deterministic placeholder order for stable plans.
            // M3: LOWER both sides on Windows only; exact IN elsewhere.
            let mut sorted: Vec<&String> = keys.iter().collect();
            sorted.sort();
            if cfg!(windows) {
                where_sql.push_str(" AND LOWER(pf.file_path) IN (");
            } else {
                where_sql.push_str(" AND pf.file_path IN (");
            }
            for (i, _) in sorted.iter().enumerate() {
                if i > 0 {
                    where_sql.push(',');
                }
                where_sql.push('?');
            }
            where_sql.push(')');
            for k in sorted {
                params.push(k.clone());
            }
        }
    }

    (where_sql, params)
}

const ORDER_BY: &str = " ORDER BY pf.file_path ASC, \
     (ps.line_start IS NULL) ASC, \
     ps.line_start ASC, \
     ps.symbol_name ASC, \
     ps.symbol_kind ASC, \
     ps.qualified_name ASC, \
     ps.id ASC";

/// COUNT then SELECT LIMIT (M1) with total-order sort (M2).
fn query_symbols(
    conn: &rusqlite::Connection,
    filters: &QueryFilters,
) -> Result<(Vec<QueriedSymbol>, usize)> {
    let (where_sql, params) = build_where_and_params(filters);
    let params_refs: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();

    let count_sql = format!(
        "SELECT COUNT(*) FROM project_symbols ps \
         INNER JOIN project_files pf ON ps.file_id = pf.id{where_sql}"
    );
    let total: i64 = conn
        .query_row(&count_sql, params_refs.as_slice(), |row| row.get(0))
        .into_diagnostic()?;
    let total_matching = usize::try_from(total).unwrap_or(usize::MAX);

    if total_matching == 0 || filters.limit == 0 {
        return Ok((Vec::new(), total_matching));
    }

    let select_sql = format!(
        "SELECT ps.id, ps.symbol_name, ps.symbol_kind, pf.file_path, \
         ps.line_start, ps.is_public, ps.qualified_name \
         FROM project_symbols ps \
         INNER JOIN project_files pf ON ps.file_id = pf.id\
         {where_sql}{ORDER_BY} LIMIT ?"
    );

    let mut all_params: Vec<&dyn rusqlite::ToSql> = params_refs;
    let limit_i64 = i64::try_from(filters.limit).unwrap_or(i64::MAX);
    all_params.push(&limit_i64);

    let mut stmt = conn.prepare(&select_sql).into_diagnostic()?;
    let rows = stmt
        .query_map(all_params.as_slice(), |row| {
            let is_public_i: i64 = row.get(5)?;
            Ok(QueriedSymbol {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: row.get(2)?,
                path: row.get(3)?,
                line: row.get(4)?,
                is_public: is_public_i != 0,
                qualified_name: row.get(6)?,
            })
        })
        .into_diagnostic()?
        .collect::<std::result::Result<Vec<_>, _>>()
        .into_diagnostic()?;

    Ok((rows, total_matching))
}

// ---------------------------------------------------------------------------
// Envelope builders
// ---------------------------------------------------------------------------

fn empty_missing_envelope(
    path_echo: Option<String>,
    changed: bool,
    kind_echo: Option<String>,
    pub_only: bool,
    limit: u64,
) -> SymbolsJsonEnvelope {
    SymbolsJsonEnvelope {
        schema_version: 1,
        scope: SymbolsScopeWire {
            path: path_echo,
            changed,
            kind: kind_echo,
            pub_only,
        },
        limit,
        truncated: false,
        result_count: 0,
        total_matching: 0,
        symbols: Vec::new(),
        index_status: Some(IndexStatus {
            state: "missing".to_string(),
            remediation: Some("ledgerful index --incremental".to_string()),
        }),
        path_resolve: None,
    }
}

fn build_envelope_with_resolve(
    scope: SymbolsScopeWire,
    limit: u64,
    symbols: Vec<SymbolInventoryRow>,
    total_matching: usize,
    index_status: Option<IndexStatus>,
    path_resolve: Option<PathResolveNote>,
) -> SymbolsJsonEnvelope {
    let result_count = symbols.len();
    let truncated = total_matching > limit as usize;
    SymbolsJsonEnvelope {
        schema_version: 1,
        scope,
        limit,
        truncated,
        result_count,
        total_matching,
        symbols,
        index_status,
        path_resolve,
    }
}

fn print_json(envelope: &SymbolsJsonEnvelope) -> Result<()> {
    let output = serde_json::to_string_pretty(envelope).into_diagnostic()?;
    println!("{output}");
    Ok(())
}

fn print_human_path_resolve(note: &PathResolveNote) {
    match note.status.as_str() {
        "ambiguous" => {
            let query = note.query.as_deref().unwrap_or("");
            let candidates = note.candidates.as_deref().unwrap_or(&[]);
            let total = note
                .candidate_total
                .unwrap_or(candidates.len())
                .max(candidates.len());
            let show = candidates.len().min(10);
            let listed = candidates[..show].join(", ");
            let mut msg = format!("{total} indexed paths match '{query}': {listed}");
            if total > show {
                msg.push_str(&format!(", and {} more", total - show));
            }
            msg.push_str(". Provide a more specific path.");
            println!(
                "{}",
                msg.if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow()))
            );
            println!();
        }
        "resolved" => {
            if let Some(p) = note.resolved_path.as_deref() {
                println!(
                    "{}",
                    format!("Path resolved to indexed file: {p}")
                        .if_supports_color(Stream::Stdout, |s| s.dimmed())
                );
                println!();
            }
        }
        _ => {}
    }
}

fn print_human(
    symbols: &[SymbolInventoryRow],
    total_matching: usize,
    limit: u64,
    index_status: Option<&IndexStatus>,
    path_resolve: Option<&PathResolveNote>,
) {
    if let Some(note) = path_resolve {
        print_human_path_resolve(note);
    }
    if let Some(status) = index_status {
        let rem = status
            .remediation
            .as_deref()
            .unwrap_or("ledgerful index --incremental");
        println!(
            "{}",
            format!(
                "Index status: {} — run `{}` to populate symbols.",
                status.state, rem
            )
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow()))
        );
        println!();
    }

    if symbols.is_empty() {
        println!(
            "{}",
            "  No symbols matched.".if_supports_color(Stream::Stdout, |s| s.dimmed())
        );
    } else {
        // Group by path (rows already sorted by path).
        let mut by_path: BTreeMap<&str, Vec<&SymbolInventoryRow>> = BTreeMap::new();
        for s in symbols {
            by_path.entry(s.path.as_str()).or_default().push(s);
        }
        for (path, rows) in by_path {
            println!(
                "{}",
                path.if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
            );
            for r in rows {
                let line = match r.line {
                    Some(n) => format!(":{n}"),
                    None => String::new(),
                };
                let vis = if r.is_public { "pub " } else { "" };
                println!("  {vis}{} {}{line}", r.kind, r.name);
            }
            println!();
        }
    }

    let shown = symbols.len();
    let mut footer = format!("{shown} of {total_matching} symbols");
    if total_matching > limit as usize {
        footer.push_str(" (truncated; raise --limit or narrow --path / --kind / --pub)");
    }
    println!(
        "{}",
        footer.if_supports_color(Stream::Stdout, |s| s.dimmed())
    );
}

// ---------------------------------------------------------------------------
// Execute
// ---------------------------------------------------------------------------

pub fn execute_symbols(args: SymbolsArgs) -> Result<()> {
    // Resolve path prefix early (fail before storage).
    let path_prefix = match &args.path {
        Some(raw) => Some(normalize_path_prefix(raw)?),
        None => None,
    };
    let kind_canonical = match &args.kind {
        Some(raw) => Some(parse_kind_filter(raw)?),
        None => None,
    };
    let kind_echo = kind_canonical.as_ref().map(|k| k.as_str().to_string());
    let path_echo = path_prefix.clone();
    let limit = args.limit;

    let layout = get_layout()?;
    let config = load_ledger_config(&layout)?;
    let threshold_days = config.index.stale_threshold_days;

    // --changed: validate git / collect WT set BEFORE H1 missing-DB envelope
    // (Codex P2-2 / D4). Non-git must fail closed even when ledger.db is absent —
    // never return the empty indexStatus:missing success envelope for an
    // invalid --changed context.
    // Empty change set → empty inventory exit 0 (after DB open / H1).
    //
    // Collect the full WT key set first (no path filter). Path ∩ changed is
    // applied after DB open so 0183 file-form resolve can widen `X.rs` →
    // `X/mod.rs` when the raw prefix matches nothing in the change set.
    let raw_changed_keys: Option<HashSet<String>> = if args.changed {
        let wt_changes = collect_changed_files_for_filter(&layout).map_err(|e| {
            miette::miette!(
                "symbols --changed requires a git repository with a readable working tree: {e}"
            )
        })?;
        let mut keys: HashSet<String> = HashSet::new();
        let mut insert_path = |path: &std::path::Path| {
            let normalized = normalize_filter_path(path);
            keys.insert(path_membership_key(&normalized));
        };
        for c in &wt_changes {
            // Includes Deleted — still indexed until re-index (M5).
            insert_path(Path::new(&c.path));
            // Renames: index still has old_path until re-index; include both sides
            // so --changed does not under-report (Codex R2 P2).
            if let crate::git::ChangeType::Renamed { old_path } = &c.change_type {
                insert_path(old_path.as_path());
            }
        }
        Some(keys)
    } else {
        None
    };

    // H1: missing DB / open_read_only Err without successful auto-index → empty + indexStatus.
    let storage = if args.auto_index {
        let storage = match StorageManager::open_read_only(&layout) {
            Ok(s) => s,
            Err(_) => {
                // Bootstrap missing DB under --auto-index (dead-code pattern).
                layout.ensure_state_dir()?;
                StorageManager::init_with_layout(&layout)?
            }
        };
        // Fatal auto-index under --json: no partial machine stdout (Err before print).
        let (s, _) = try_auto_index(storage, threshold_days, &layout)?;
        s
    } else {
        match StorageManager::open_read_only(&layout) {
            Ok(s) => {
                if !args.json {
                    let _ = warn_if_stale(&s, threshold_days);
                }
                s
            }
            Err(e) => {
                // H1 honesty: only a truly missing ledger.db maps to
                // indexStatus.state=missing + empty envelope. Other open
                // failures (corrupt/permission) propagate (F3).
                let db_path = layout.state_subdir().join("ledger.db");
                if !db_path.exists() {
                    let envelope = empty_missing_envelope(
                        path_echo,
                        args.changed,
                        kind_echo,
                        args.pub_only,
                        limit,
                    );
                    if args.json {
                        return print_json(&envelope);
                    }
                    print_human(
                        &[],
                        0,
                        limit,
                        envelope.index_status.as_ref(),
                        envelope.path_resolve.as_ref(),
                    );
                    return Ok(());
                }
                return Err(e);
            }
        }
    };

    let conn = storage.get_connection();

    // Path ∩ changed (A1-BS2) + 0183 file-form zero-match resolve.
    // When --changed: SQL uses membership IN only (path applied via keys).
    // When path only: SQL uses path_prefix; resolve re-runs query on stored path.
    use crate::util::path_entity::{IndexedFileResolve, resolve_indexed_file_path};

    let mut path_resolve: Option<PathResolveNote> = None;
    let query_path_prefix: Option<String> = if args.changed {
        None
    } else {
        path_prefix.clone()
    };

    // --changed + --path: first-pass path ∩ changed (exact/prefix only).
    // File-form resolve runs on **zero-match** after the first query so renames
    // that leave a non-empty key set under the raw path (new name) still widen
    // to the indexed alias (old name still in index until re-index).
    let mut changed_keys = if let Some(ref prefix) = path_prefix
        && let Some(ref all_keys) = raw_changed_keys
    {
        Some(
            all_keys
                .iter()
                .filter(|k| path_matches_prefix(k, prefix))
                .cloned()
                .collect::<HashSet<_>>(),
        )
    } else {
        raw_changed_keys.clone()
    };

    let filters = QueryFilters {
        path_prefix: query_path_prefix.clone(),
        kind: kind_echo.clone(),
        pub_only: args.pub_only,
        changed_keys: changed_keys.clone(),
        limit: usize::try_from(limit).unwrap_or(usize::MAX),
    };

    let (rows, total_matching) = query_symbols(conn, &filters)?;

    // 0183 B2: zero-match file-identity fallback (path-only and --changed+path).
    // Reuses existing query path after resolve (0183-D). Successful non-empty
    // inventories stay as-is.
    let (rows, total_matching) = if total_matching == 0
        && let Some(ref raw_path) = path_prefix
        && path_resolve.is_none()
    {
        match resolve_indexed_file_path(conn, raw_path) {
            IndexedFileResolve::Unique { stored_path, .. } if stored_path != *raw_path => {
                if args.changed {
                    // Re-intersect full WT keys against resolved indexed path.
                    let filtered = match &raw_changed_keys {
                        Some(all_keys) => all_keys
                            .iter()
                            .filter(|k| path_matches_prefix(k, &stored_path))
                            .cloned()
                            .collect::<HashSet<_>>(),
                        None => HashSet::new(),
                    };
                    changed_keys = Some(filtered);
                    let resolved_filters = QueryFilters {
                        path_prefix: None,
                        kind: kind_echo.clone(),
                        pub_only: args.pub_only,
                        changed_keys: changed_keys.clone(),
                        limit: usize::try_from(limit).unwrap_or(usize::MAX),
                    };
                    let (r2, t2) = query_symbols(conn, &resolved_filters)?;
                    path_resolve = Some(PathResolveNote {
                        status: "resolved".to_string(),
                        resolved_path: Some(stored_path),
                        query: None,
                        candidates: None,
                        candidate_total: None,
                    });
                    (r2, t2)
                } else {
                    let resolved_filters = QueryFilters {
                        path_prefix: Some(stored_path.clone()),
                        kind: kind_echo.clone(),
                        pub_only: args.pub_only,
                        changed_keys: changed_keys.clone(),
                        limit: usize::try_from(limit).unwrap_or(usize::MAX),
                    };
                    let (r2, t2) = query_symbols(conn, &resolved_filters)?;
                    path_resolve = Some(PathResolveNote {
                        status: "resolved".to_string(),
                        resolved_path: Some(stored_path),
                        query: None,
                        candidates: None,
                        candidate_total: None,
                    });
                    (r2, t2)
                }
            }
            IndexedFileResolve::Ambiguous { query, candidates } => {
                let total = candidates.len();
                let show = total.min(20);
                path_resolve = Some(PathResolveNote {
                    status: "ambiguous".to_string(),
                    resolved_path: None,
                    query: Some(query),
                    candidates: Some(candidates[..show].to_vec()),
                    candidate_total: Some(total),
                });
                // Keep zero-row inventory; refuse silent pick (do not re-query).
                (rows, total_matching)
            }
            _ => (rows, total_matching),
        }
    } else {
        (rows, total_matching)
    };

    let symbols: Vec<SymbolInventoryRow> = rows.into_iter().map(QueriedSymbol::into_wire).collect();

    let envelope = build_envelope_with_resolve(
        SymbolsScopeWire {
            path: path_echo,
            changed: args.changed,
            kind: kind_echo,
            pub_only: args.pub_only,
        },
        limit,
        symbols,
        total_matching,
        None,
        path_resolve,
    );

    if args.json {
        print_json(&envelope)
    } else {
        print_human(
            &envelope.symbols,
            envelope.total_matching,
            envelope.limit,
            envelope.index_status.as_ref(),
            envelope.path_resolve.as_ref(),
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use rusqlite::Connection;

    fn in_memory_storage() -> StorageManager {
        let conn = Connection::open_in_memory().unwrap();
        let mut conn = conn;
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    fn seed_file(conn: &Connection, id: i64, path: &str) {
        conn.execute(
            "INSERT INTO project_files (id, file_path, last_indexed_at) \
             VALUES (?1, ?2, '2026-01-01T00:00:00Z')",
            rusqlite::params![id, path],
        )
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn seed_symbol(
        conn: &Connection,
        id: i64,
        file_id: i64,
        name: &str,
        kind: &str,
        line: Option<i64>,
        is_public: bool,
        qualified: &str,
    ) {
        conn.execute(
            "INSERT INTO project_symbols \
             (id, file_id, qualified_name, symbol_name, symbol_kind, is_public, \
              line_start, last_indexed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '2026-01-01T00:00:00Z')",
            rusqlite::params![
                id,
                file_id,
                qualified,
                name,
                kind,
                if is_public { 1 } else { 0 },
                line
            ],
        )
        .unwrap();
    }

    fn seed_fixture(conn: &Connection) {
        seed_file(conn, 1, "src/commands/foo.rs");
        seed_file(conn, 2, "src/commands/bar.rs");
        seed_file(conn, 3, "src/cli/args.rs");
        seed_file(conn, 4, "tests/integration/cli.rs");

        // 5 under src/commands (for limit/truncated)
        seed_symbol(conn, 1, 1, "alpha", "Function", Some(10), true, "alpha");
        seed_symbol(conn, 2, 1, "beta", "Function", Some(20), false, "beta");
        seed_symbol(conn, 3, 2, "gamma", "Struct", Some(5), true, "gamma");
        seed_symbol(conn, 4, 2, "delta", "Function", Some(15), true, "delta");
        seed_symbol(conn, 5, 1, "epsilon", "Module", None, true, "epsilon");
        // outside path
        seed_symbol(
            conn,
            6,
            3,
            "cli_main",
            "Function",
            Some(1),
            true,
            "cli_main",
        );
        seed_symbol(conn, 7, 4, "test_a", "Function", Some(1), false, "test_a");
        // M2 tiebreak: same file, same line, same name, different qualified_name
        seed_symbol(conn, 8, 1, "twin", "Function", Some(99), true, "a::twin");
        seed_symbol(conn, 9, 1, "twin", "Function", Some(99), true, "b::twin");
    }

    #[test]
    fn normalize_path_backslash_and_trailing_slash() {
        assert_eq!(
            normalize_path_prefix(r"src\commands\").unwrap(),
            "src/commands"
        );
        assert_eq!(
            normalize_path_prefix("./src/commands/").unwrap(),
            "src/commands"
        );
        assert_eq!(
            normalize_path_prefix("/src/commands").unwrap(),
            "src/commands"
        );
        assert_eq!(
            normalize_path_prefix("src/commands").unwrap(),
            "src/commands"
        );
    }

    #[test]
    fn normalize_path_empty_after_trim_errors() {
        assert!(normalize_path_prefix("/").is_err());
        assert!(normalize_path_prefix("./").is_err());
        assert!(normalize_path_prefix("///").is_err());
        assert!(normalize_path_prefix("").is_err());
    }

    #[test]
    fn parse_kind_aliases_canonical() {
        assert_eq!(parse_kind_filter("fn").unwrap(), SymbolKind::Function);
        assert_eq!(parse_kind_filter("Function").unwrap(), SymbolKind::Function);
        assert_eq!(parse_kind_filter("FN").unwrap(), SymbolKind::Function);
        assert_eq!(parse_kind_filter("mod").unwrap(), SymbolKind::Module);
        assert_eq!(parse_kind_filter("module").unwrap(), SymbolKind::Module);
        assert_eq!(parse_kind_filter("const").unwrap(), SymbolKind::Constant);
        assert_eq!(parse_kind_filter("var").unwrap(), SymbolKind::Variable);
        assert_eq!(parse_kind_filter("struct").unwrap(), SymbolKind::Struct);
        assert_eq!(parse_kind_filter("class").unwrap(), SymbolKind::Class);
        assert_eq!(
            parse_kind_filter("interface").unwrap(),
            SymbolKind::Interface
        );
        assert!(parse_kind_filter("unknown").is_err());
        assert!(parse_kind_filter("").is_err());
    }

    #[test]
    fn path_prefix_and_trailing_slash_match() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();
        seed_fixture(conn);

        let filters = QueryFilters {
            path_prefix: Some("src/commands".into()),
            kind: None,
            pub_only: false,
            changed_keys: None,
            limit: 200,
        };
        let (rows, total) = query_symbols(conn, &filters).unwrap();
        // 5 under path + 2 twins = 7
        assert_eq!(total, 7);
        assert_eq!(rows.len(), 7);
        assert!(rows.iter().all(|r| r.path.starts_with("src/commands")));
    }

    #[test]
    fn kind_filter_and_pub() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();
        seed_fixture(conn);

        let filters = QueryFilters {
            path_prefix: Some("src/commands".into()),
            kind: Some("Function".into()),
            pub_only: true,
            changed_keys: None,
            limit: 200,
        };
        let (rows, total) = query_symbols(conn, &filters).unwrap();
        // alpha, delta, twin a, twin b (beta private, gamma Struct, epsilon Module)
        assert_eq!(total, 4);
        assert!(rows.iter().all(|r| r.kind == "Function" && r.is_public));
    }

    #[test]
    fn limit_truncates_with_true_count() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();
        seed_fixture(conn);

        let filters = QueryFilters {
            path_prefix: Some("src/commands".into()),
            kind: None,
            pub_only: false,
            changed_keys: None,
            limit: 2,
        };
        let (rows, total) = query_symbols(conn, &filters).unwrap();
        assert_eq!(total, 7);
        assert_eq!(rows.len(), 2);

        let envelope = build_envelope_with_resolve(
            SymbolsScopeWire {
                path: Some("src/commands".into()),
                changed: false,
                kind: None,
                pub_only: false,
            },
            2,
            rows.into_iter().map(QueriedSymbol::into_wire).collect(),
            total,
            None,
            None,
        );
        assert!(envelope.truncated);
        assert_eq!(envelope.result_count, 2);
        assert_eq!(envelope.total_matching, 7);
        assert_eq!(envelope.schema_version, 1);
    }

    #[test]
    fn sort_tiebreak_qualified_name_then_id() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();
        seed_fixture(conn);

        let filters = QueryFilters {
            path_prefix: Some("src/commands/foo.rs".into()),
            kind: Some("Function".into()),
            pub_only: false,
            changed_keys: None,
            limit: 200,
        };
        let (rows, _) = query_symbols(conn, &filters).unwrap();
        let twins: Vec<_> = rows.iter().filter(|r| r.name == "twin").collect();
        assert_eq!(twins.len(), 2);
        assert_eq!(twins[0].qualified_name.as_deref(), Some("a::twin"));
        assert_eq!(twins[1].qualified_name.as_deref(), Some("b::twin"));
        assert!(twins[0].id < twins[1].id);
    }

    #[test]
    fn changed_keys_filter_intersection() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();
        seed_fixture(conn);

        let mut keys = HashSet::new();
        keys.insert(path_membership_key("src/cli/args.rs"));
        // path ∩ changed: only cli when path is src/cli
        let filters = QueryFilters {
            path_prefix: None,
            kind: None,
            pub_only: false,
            changed_keys: Some(keys),
            limit: 200,
        };
        let (rows, total) = query_symbols(conn, &filters).unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows[0].name, "cli_main");
    }

    #[test]
    fn empty_changed_keys_yield_zero() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();
        seed_fixture(conn);

        let filters = QueryFilters {
            path_prefix: None,
            kind: None,
            pub_only: false,
            changed_keys: Some(HashSet::new()),
            limit: 200,
        };
        let (rows, total) = query_symbols(conn, &filters).unwrap();
        assert_eq!(total, 0);
        assert!(rows.is_empty());
    }

    #[test]
    fn line_omitted_when_null_in_json() {
        let row = SymbolInventoryRow {
            name: "epsilon".into(),
            kind: "Module".into(),
            path: "src/commands/foo.rs".into(),
            line: None,
            is_public: true,
            qualified_name: Some("epsilon".into()),
        };
        let v = serde_json::to_value(&row).unwrap();
        assert!(v.get("line").is_none());
        assert_eq!(v["isPublic"], true);
    }

    #[test]
    fn missing_envelope_has_index_status() {
        let env = empty_missing_envelope(None, false, None, false, 200);
        let v = serde_json::to_value(&env).unwrap();
        assert_eq!(v["schemaVersion"], 1);
        assert_eq!(v["symbols"].as_array().unwrap().len(), 0);
        assert_eq!(v["indexStatus"]["state"], "missing");
        assert!(
            v["indexStatus"]["remediation"]
                .as_str()
                .unwrap()
                .contains("index")
        );
        assert_eq!(v["scope"]["path"], serde_json::Value::Null);
        assert_eq!(v["scope"]["kind"], serde_json::Value::Null);
        assert_eq!(v["scope"]["changed"], false);
        assert_eq!(v["scope"]["pubOnly"], false);
    }

    #[test]
    fn path_membership_key_normalizes_separators() {
        // Always `\` → `/`; already-lowercase paths are stable on every OS.
        assert_eq!(
            path_membership_key(r"src\commands\foo.rs"),
            "src/commands/foo.rs"
        );
    }

    /// D3/D4/M3 platform policy: case-insensitive membership on Windows only.
    #[test]
    fn path_membership_key_platform_case_policy() {
        let mixed = path_membership_key(r"Src\Commands\Foo.rs");
        if cfg!(windows) {
            assert_eq!(mixed, "src/commands/foo.rs");
        } else {
            assert_eq!(mixed, "Src/Commands/Foo.rs");
        }
    }

    #[test]
    fn path_matches_prefix_equality_and_child() {
        assert!(path_matches_prefix("src/commands/foo.rs", "src/commands"));
        assert!(path_matches_prefix("src/commands", "src/commands"));
        assert!(!path_matches_prefix("src/commandx/foo.rs", "src/commands"));
        assert!(!path_matches_prefix("src/cli/args.rs", "src/commands"));
    }

    #[cfg(windows)]
    #[test]
    fn path_matches_prefix_case_insensitive_on_windows() {
        assert!(path_matches_prefix("Src/Commands/foo.rs", "src/commands"));
        assert!(path_matches_prefix("src/commands/foo.rs", "Src/Commands"));
    }

    #[cfg(not(windows))]
    #[test]
    fn path_matches_prefix_case_sensitive_on_unix() {
        assert!(!path_matches_prefix("Src/Commands/foo.rs", "src/commands"));
        assert!(path_matches_prefix("src/commands/foo.rs", "src/commands"));
    }

    /// Path-prefix SQL follows the same Windows-only case fold as membership keys.
    #[test]
    fn path_prefix_sql_case_policy_matches_platform() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();
        seed_file(conn, 1, "src/commands/foo.rs");
        seed_symbol(conn, 1, 1, "alpha", "Function", Some(1), true, "alpha");

        let filters = QueryFilters {
            path_prefix: Some("Src/Commands".into()),
            kind: None,
            pub_only: false,
            changed_keys: None,
            limit: 200,
        };
        let (rows, total) = query_symbols(conn, &filters).unwrap();
        if cfg!(windows) {
            assert_eq!(total, 1, "Windows path prefix is case-insensitive");
            assert_eq!(rows[0].name, "alpha");
        } else {
            assert_eq!(total, 0, "non-Windows path prefix is case-sensitive");
        }
    }

    #[test]
    fn escape_like_pattern_metacharacters() {
        assert_eq!(escape_like_pattern("src/dead_code"), r"src/dead\_code");
        assert_eq!(escape_like_pattern("a%b"), r"a\%b");
        assert_eq!(escape_like_pattern(r"a\b"), r"a\\b");
        assert_eq!(escape_like_pattern(r"a\_%"), r"a\\\_\%");
    }

    /// F1: `_` in a path prefix must not act as a single-char LIKE wildcard.
    /// `src/a_b` must not over-match `src/axb/...`; `src/foo_bar` must not
    /// match `src/fooXbar/...`.
    #[test]
    fn path_prefix_underscore_is_literal_not_like_wildcard() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();

        seed_file(conn, 1, "src/a_b/x.rs");
        seed_file(conn, 2, "src/axb/y.rs");
        seed_file(conn, 3, "src/foo_bar/z.rs");
        seed_file(conn, 4, "src/fooXbar/w.rs");
        seed_symbol(conn, 1, 1, "under_a", "Function", Some(1), true, "under_a");
        seed_symbol(conn, 2, 2, "wild_a", "Function", Some(1), true, "wild_a");
        seed_symbol(conn, 3, 3, "under_f", "Function", Some(1), true, "under_f");
        seed_symbol(conn, 4, 4, "wild_f", "Function", Some(1), true, "wild_f");

        let filters = QueryFilters {
            path_prefix: Some("src/a_b".into()),
            kind: None,
            pub_only: false,
            changed_keys: None,
            limit: 200,
        };
        let (rows, total) = query_symbols(conn, &filters).unwrap();
        assert_eq!(total, 1, "src/a_b must not match src/axb via LIKE _");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "under_a");
        assert!(rows[0].path.starts_with("src/a_b"));

        let filters = QueryFilters {
            path_prefix: Some("src/foo_bar".into()),
            kind: None,
            pub_only: false,
            changed_keys: None,
            limit: 200,
        };
        let (rows, total) = query_symbols(conn, &filters).unwrap();
        assert_eq!(
            total, 1,
            "src/foo_bar must not match src/fooXbar via LIKE _"
        );
        assert_eq!(rows[0].name, "under_f");
        assert!(rows[0].path.starts_with("src/foo_bar"));
    }

    /// 0183 B2: file-form `pkg.rs` with only `pkg/mod.rs` indexed → resolve then
    /// re-query existing symbols path (non-empty).
    #[test]
    fn zero_match_file_form_resolves_to_mod_rs() {
        use crate::util::path_entity::{IndexedFileResolve, resolve_indexed_file_path};

        let storage = in_memory_storage();
        let conn = storage.get_connection();
        seed_file(conn, 1, "src/pkg/mod.rs");
        seed_symbol(conn, 1, 1, "mod_fn", "Function", Some(1), true, "mod_fn");

        let filters = QueryFilters {
            path_prefix: Some("src/pkg.rs".into()),
            kind: None,
            pub_only: false,
            changed_keys: None,
            limit: 200,
        };
        let (_rows, total) = query_symbols(conn, &filters).unwrap();
        assert_eq!(total, 0, "prefix alone must miss file-form .rs vs mod.rs");

        match resolve_indexed_file_path(conn, "src/pkg.rs") {
            IndexedFileResolve::Unique { stored_path, .. } => {
                assert_eq!(stored_path, "src/pkg/mod.rs");
                let resolved = QueryFilters {
                    path_prefix: Some(stored_path),
                    kind: None,
                    pub_only: false,
                    changed_keys: None,
                    limit: 200,
                };
                let (rows2, total2) = query_symbols(conn, &resolved).unwrap();
                assert_eq!(total2, 1);
                assert_eq!(rows2[0].name, "mod_fn");
            }
            other => panic!("expected Unique alias resolve, got {other:?}"),
        }
    }

    /// 0183: successful dir prefix must not need file resolve (zero-match guard).
    #[test]
    fn dir_prefix_with_children_stays_non_empty_without_file_resolve() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();
        seed_file(conn, 1, "src/pkg/mod.rs");
        seed_file(conn, 2, "src/pkg/sub.rs");
        seed_symbol(conn, 1, 1, "a", "Function", Some(1), true, "a");
        seed_symbol(conn, 2, 2, "b", "Function", Some(1), true, "b");

        let filters = QueryFilters {
            path_prefix: Some("src/pkg".into()),
            kind: None,
            pub_only: false,
            changed_keys: None,
            limit: 200,
        };
        let (rows, total) = query_symbols(conn, &filters).unwrap();
        assert_eq!(total, 2);
        assert_eq!(rows.len(), 2);
    }

    /// 0183 DoD-2: ambiguous suffix refuses (multi mod.rs).
    #[test]
    fn zero_match_ambiguous_suffix_refuses() {
        use crate::util::path_entity::{IndexedFileResolve, resolve_indexed_file_path};

        let storage = in_memory_storage();
        let conn = storage.get_connection();
        seed_file(conn, 1, "src/a/mod.rs");
        seed_file(conn, 2, "src/b/mod.rs");
        seed_symbol(conn, 1, 1, "a", "Function", Some(1), true, "a");
        seed_symbol(conn, 2, 2, "b", "Function", Some(1), true, "b");

        match resolve_indexed_file_path(conn, "mod.rs") {
            IndexedFileResolve::Ambiguous { candidates, .. } => {
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }
}
