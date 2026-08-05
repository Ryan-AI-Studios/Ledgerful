use clap::CommandFactory;
use miette::{IntoDiagnostic, Result};
use regex::Regex;
use rusqlite::Connection;
use std::collections::HashSet;
use std::sync::OnceLock;

use crate::search::tantivy_engine::{SearchResult, TantivySearchEngine};
use crate::state::layout::Layout;

#[derive(Debug)]
pub enum ExactIntent {
    CallersOf(String),
    CalleesOf(String),
    RouteOwner(String),
    ListRoutes,
    SymbolDefinition(String),
}

pub fn parse_intent(query: &str) -> Option<ExactIntent> {
    let lower = query.to_lowercase();
    let q = query.trim();

    // what calls X / show callers of X / who calls X
    if let Some(caps) = Regex::new(r"(?i)^(?:what|who) calls ([a-zA-Z0-9_:]+)")
        .unwrap()
        .captures(q)
    {
        return Some(ExactIntent::CallersOf(caps[1].to_string()));
    }
    if let Some(caps) = Regex::new(r"(?i)^show callers of ([a-zA-Z0-9_:]+)")
        .unwrap()
        .captures(q)
    {
        return Some(ExactIntent::CallersOf(caps[1].to_string()));
    }
    if let Some(caps) = Regex::new(r"(?i)^find callers of ([a-zA-Z0-9_:]+)")
        .unwrap()
        .captures(q)
    {
        return Some(ExactIntent::CallersOf(caps[1].to_string()));
    }

    // what does X call / show callees of X
    if let Some(caps) = Regex::new(r"(?i)^what does ([a-zA-Z0-9_:]+) call")
        .unwrap()
        .captures(q)
    {
        return Some(ExactIntent::CalleesOf(caps[1].to_string()));
    }
    if let Some(caps) = Regex::new(r"(?i)^show callees of ([a-zA-Z0-9_:]+)")
        .unwrap()
        .captures(q)
    {
        return Some(ExactIntent::CalleesOf(caps[1].to_string()));
    }

    // list route handlers / list routes
    if lower.contains("list route handlers")
        || lower.contains("list routes")
        || lower.contains("show routes")
        || lower.contains("find all axum route handlers")
        || lower.contains("what routes are defined")
    {
        return Some(ExactIntent::ListRoutes);
    }

    // which handler owns route Y
    if let Some(caps) = Regex::new(r"(?i)which handler owns route ([\w/-]+)")
        .unwrap()
        .captures(q)
    {
        return Some(ExactIntent::RouteOwner(caps[1].to_string()));
    }
    if let Some(caps) = Regex::new(r"(?i)who handles route ([\w/-]+)")
        .unwrap()
        .captures(q)
    {
        return Some(ExactIntent::RouteOwner(caps[1].to_string()));
    }
    if let Some(caps) = Regex::new(r"(?i)handler for route ([\w/-]+)")
        .unwrap()
        .captures(q)
    {
        return Some(ExactIntent::RouteOwner(caps[1].to_string()));
    }

    // where is symbol X defined
    if let Some(caps) = Regex::new(r"(?i)where is (?:symbol )?([a-zA-Z0-9_:]+) defined")
        .unwrap()
        .captures(q)
    {
        return Some(ExactIntent::SymbolDefinition(caps[1].to_string()));
    }
    if let Some(caps) = Regex::new(r"(?i)^find definition of ([a-zA-Z0-9_:]+)")
        .unwrap()
        .captures(q)
    {
        return Some(ExactIntent::SymbolDefinition(caps[1].to_string()));
    }

    // 0142: locate / find-symbol shapes → SymbolDefinition
    // (docs/operator-surface-policy.md §2 — structured sources before LLM).
    // Parse order is load-bearing: ListRoutes / callers / callees / route-owner /
    // "where is … defined" / "find definition of" run first so bare `find X`
    // never steals `find all axum route handlers` or `find callers of X`.
    // Deliberately **no** `where is X` without "defined" (false-positive magnet).
    if !has_conceptual_locate_tokens(q) {
        if let Some(sym) = parse_find_type_qualified(q) {
            return Some(ExactIntent::SymbolDefinition(sym));
        }
        if let Some(sym) = parse_locate_symbol(q) {
            return Some(ExactIntent::SymbolDefinition(sym));
        }
        if let Some(sym) = parse_bare_find_symbol(q) {
            return Some(ExactIntent::SymbolDefinition(sym));
        }
    }

    None
}

/// Whole-word conceptual / narrative tokens that must not map to locate intents.
fn has_conceptual_locate_tokens(q: &str) -> bool {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(how|why|way|best|explain|architect|architecture|implement|implementation)\b",
        )
        .ok()
    });
    re.as_ref().is_some_and(|r| r.is_match(q))
}

/// Identifier class for locate/find targets: `^[A-Za-z_][A-Za-z0-9_:]*$`.
fn is_code_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

/// Word count after stripping punctuation (for bare-find ≤8 guard).
fn locate_query_word_count(q: &str) -> usize {
    let stripped = strip_trailing_punct(q.trim());
    stripped
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
        .filter(|w| !w.is_empty())
        .count()
}

/// `find (the )?(function|fn|method|struct|type|trait|enum|symbol) X`
fn parse_find_type_qualified(q: &str) -> Option<String> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?i)^find\s+(?:the\s+)?(?:function|fn|method|struct|type|trait|enum|symbol)\s+([A-Za-z_][A-Za-z0-9_:]*)\s*[?.!]*$",
        )
        .ok()
    });
    let re = re.as_ref()?;
    let caps = re.captures(q.trim())?;
    let sym = caps.get(1)?.as_str();
    if is_code_identifier(sym) {
        Some(sym.to_string())
    } else {
        None
    }
}

/// `locate (the )?(function|…|symbol)? X`
fn parse_locate_symbol(q: &str) -> Option<String> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?i)^locate\s+(?:the\s+)?(?:(?:function|fn|method|struct|type|trait|enum|symbol)\s+)?([A-Za-z_][A-Za-z0-9_:]*)\s*[?.!]*$",
        )
        .ok()
    });
    let re = re.as_ref()?;
    let caps = re.captures(q.trim())?;
    let sym = caps.get(1)?.as_str();
    if is_code_identifier(sym) {
        Some(sym.to_string())
    } else {
        None
    }
}

/// Bare `find X` / `find X in the codebase` when X is a single code identifier
/// and the query has ≤ 8 words after punctuation strip.
fn parse_bare_find_symbol(q: &str) -> Option<String> {
    let q = q.trim();
    if locate_query_word_count(q) > 8 {
        return None;
    }
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)^find\s+([A-Za-z_][A-Za-z0-9_:]*)(?:\s+in\s+the\s+codebase)?\s*[?.!]*$")
            .ok()
    });
    let re = re.as_ref()?;
    let caps = re.captures(q)?;
    let sym = caps.get(1)?.as_str();
    if is_code_identifier(sym) {
        Some(sym.to_string())
    } else {
        None
    }
}

pub fn resolve_intent(intent: &ExactIntent, conn: &Connection) -> Result<Option<String>> {
    fn normalize_symbol(s: &str) -> &str {
        s.rsplit("::").next().unwrap_or(s)
    }

    match intent {
        ExactIntent::CallersOf(target) => {
            let target_norm = normalize_symbol(target);
            let mut stmt = conn
                .prepare(
                    "SELECT DISTINCT pf.file_path, ps.symbol_name, ps.line_start 
                 FROM structural_edges ce 
                 LEFT JOIN project_symbols ps ON ce.caller_symbol_id = ps.id 
                 JOIN project_files pf ON ce.caller_file_id = pf.id 
                 LEFT JOIN project_symbols callee_ps ON ce.callee_symbol_id = callee_ps.id
                 WHERE callee_ps.symbol_name = ?1 OR ce.unresolved_callee = ?1 OR callee_ps.qualified_name LIKE ?2",
                )
                .into_diagnostic()?;

            let like_pattern = format!("%{}", target_norm);
            let rows = stmt
                .query_map([target_norm, &like_pattern], |row| {
                    let file_path: String = row.get(0)?;
                    let symbol_name: Option<String> = row.get(1)?;
                    let line_number: Option<i64> = row.get(2)?;
                    let ln = line_number
                        .map(|l| l.to_string())
                        .unwrap_or_else(|| "?".into());
                    let sym = symbol_name.unwrap_or_else(|| "<top-level>".into());
                    Ok(format!("- `{}` in {} (line {})", sym, file_path, ln))
                })
                .into_diagnostic()?;

            let mut results = Vec::new();
            for row in rows.flatten() {
                results.push(row);
            }
            if results.is_empty() {
                Ok(None)
            } else {
                results.sort();
                Ok(Some(format!(
                    "Callers of `{}`:\n{}",
                    target,
                    results.join("\n")
                )))
            }
        }
        ExactIntent::CalleesOf(target) => {
            let target_norm = normalize_symbol(target);
            let mut stmt = conn.prepare(
                "SELECT DISTINCT pf.file_path, callee_ps.symbol_name, callee_ps.line_start, ce.unresolved_callee
                 FROM structural_edges ce 
                 JOIN project_symbols ps ON ce.caller_symbol_id = ps.id 
                 LEFT JOIN project_symbols callee_ps ON ce.callee_symbol_id = callee_ps.id
                 LEFT JOIN project_files pf ON callee_ps.file_id = pf.id
                 WHERE ps.symbol_name = ?1 OR ps.qualified_name LIKE ?2"
            ).into_diagnostic()?;

            let like_pattern = format!("%{}", target_norm);
            let rows = stmt
                .query_map([target_norm, &like_pattern], |row| {
                    let file_path: Option<String> = row.get(0)?;
                    let symbol_name: Option<String> = row.get(1)?;
                    let line_number: Option<i64> = row.get(2)?;
                    let unresolved: Option<String> = row.get(3)?;

                    if let Some(s) = symbol_name {
                        let fp = file_path.unwrap_or_else(|| "unknown file".into());
                        let ln = line_number
                            .map(|l| l.to_string())
                            .unwrap_or_else(|| "?".into());
                        Ok(format!("- `{}` in {} (line {})", s, fp, ln))
                    } else if let Some(u) = unresolved {
                        Ok(format!("- `{}` (unresolved)", u))
                    } else {
                        Ok("- unknown callee".to_string())
                    }
                })
                .into_diagnostic()?;

            let mut results = Vec::new();
            for row in rows.flatten() {
                results.push(row);
            }
            if results.is_empty() {
                Ok(None)
            } else {
                results.sort();
                Ok(Some(format!(
                    "Callees of `{}`:\n{}",
                    target,
                    results.join("\n")
                )))
            }
        }
        ExactIntent::ListRoutes => {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT pf.file_path, rd.path_pattern, rd.method, rd.handler_symbol_name 
                 FROM api_routes rd 
                 JOIN project_files pf ON rd.handler_file_id = pf.id"
            ).into_diagnostic()?;

            let rows = stmt
                .query_map([], |row| {
                    let file_path: String = row.get(0)?;
                    let route_path: String = row.get(1)?;
                    let method: String = row.get(2)?;
                    let handler: Option<String> = row.get(3)?;
                    let h = handler.unwrap_or_else(|| "unknown".into());
                    Ok(format!(
                        "- `{} {}` -> `{}` in {}",
                        method, route_path, h, file_path
                    ))
                })
                .into_diagnostic()?;

            let mut results = Vec::new();
            for row in rows.flatten() {
                results.push(row);
            }
            if results.is_empty() {
                Ok(None)
            } else {
                results.sort();
                Ok(Some(format!("API Routes:\n{}", results.join("\n"))))
            }
        }
        ExactIntent::RouteOwner(route) => {
            let target_route = if !route.starts_with('/') {
                format!("/{}", route)
            } else {
                route.clone()
            };

            let mut stmt = conn
                .prepare(
                    "SELECT DISTINCT pf.file_path, rd.method, rd.handler_symbol_name 
                 FROM api_routes rd 
                 JOIN project_files pf ON rd.handler_file_id = pf.id
                 WHERE rd.path_pattern = ?1 OR rd.path_pattern LIKE ?2",
                )
                .into_diagnostic()?;

            let like_pattern = format!("%{}%", target_route);

            let rows = stmt
                .query_map([&target_route, &like_pattern], |row| {
                    let file_path: String = row.get(0)?;
                    let method: String = row.get(1)?;
                    let handler: Option<String> = row.get(2)?;
                    let h = handler.unwrap_or_else(|| "unknown".into());
                    Ok(format!(
                        "- `{} {}` -> `{}` in {}",
                        method, target_route, h, file_path
                    ))
                })
                .into_diagnostic()?;

            let mut results = Vec::new();
            for row in rows.flatten() {
                results.push(row);
            }
            if results.is_empty() {
                Ok(None)
            } else {
                results.sort();
                Ok(Some(format!(
                    "Handlers for route `{}`:\n{}",
                    route,
                    results.join("\n")
                )))
            }
        }
        ExactIntent::SymbolDefinition(symbol) => {
            let symbol_norm = normalize_symbol(symbol);
            let mut stmt = conn
                .prepare(
                    "SELECT DISTINCT pf.file_path, ps.line_start, ps.symbol_kind
                 FROM project_symbols ps 
                 JOIN project_files pf ON ps.file_id = pf.id 
                 WHERE ps.symbol_name = ?1 OR ps.qualified_name LIKE ?2",
                )
                .into_diagnostic()?;

            let like_pattern = format!("%{}", symbol_norm);
            let rows = stmt
                .query_map([symbol_norm, &like_pattern], |row| {
                    let file_path: String = row.get(0)?;
                    let line_number: Option<i64> = row.get(1)?;
                    let ln = line_number
                        .map(|l| l.to_string())
                        .unwrap_or_else(|| "?".into());
                    let kind: String = row.get(2)?;
                    Ok(format!(
                        "- `{}` is a {} defined in {} (line {})",
                        symbol, kind, file_path, ln
                    ))
                })
                .into_diagnostic()?;

            let mut results = Vec::new();
            for row in rows.flatten() {
                results.push(row);
            }
            if results.is_empty() {
                Ok(None)
            } else {
                results.sort();
                Ok(Some(format!(
                    "Definitions of `{}`:\n{}",
                    symbol,
                    results.join("\n")
                )))
            }
        }
    }
}

// --- Product-docs routing (0139 Ask Docs Grounding) ---
//
// Product-usage / Daily 5 / agent-default-path questions are answered from
// the tracked skill SoT (`docs/Ledgerful/skill.md`) plus live clap `about`
// text — never free-form LLM invention. Policy:
// `docs/operator-surface-policy.md` §2 (Structured sources before LLM synthesis).
// Wire order in `execute_ask` is normative: CG-F20 → ProductDocs → CG-F31 → LLM
// so "session start commands" is not swallowed by GenericDiscovery.

/// Product-docs intent class. Daily 5 is the load-bearing first member;
/// AgentDefaultPath is a synonym that yields the same answer.
#[derive(Debug, PartialEq, Eq)]
pub enum ProductDocsIntent {
    /// "What is the Daily 5?", "agent default path", "session start commands".
    Daily5,
}

/// Locked banner for ProductDocs early-exit (DoD-1; matches CG-F20/F31 style).
pub const PRODUCT_DOCS_DAILY5_BANNER: &str = "Product-docs query resolved via skill Daily 5.";

// --- Local grounding / locate (0142 Ask Local Grounding First) ---
//
// When CG-F20 SymbolDefinition primary SQL misses, execute_ask runs secondary
// FTS (TermQuery full-id) or honest local miss — never LLM invent of
// "no codebase" while search can answer. Policy:
// `docs/operator-surface-policy.md` §2.

/// Locked banner when secondary search evidence answers a locate intent (0142).
pub const LOCAL_GROUNDING_SEARCH_BANNER: &str = "Local grounding query resolved via index/search.";

/// Locked banner when primary symbols + secondary search both miss (0142).
pub const LOCAL_GROUNDING_MISS_BANNER: &str = "Local grounding found no index/search hits.";

/// Cap for secondary search evidence lines shown to the operator.
const LOCAL_GROUNDING_SEARCH_CAP: usize = 5;

/// Max chars for a single evidence snippet line (char-boundary safe).
const LOCAL_GROUNDING_SNIPPET_CHARS: usize = 120;

/// Secondary FTS locate for SymbolDefinition when SQL symbols miss.
///
/// Opens the Tantivy index under `layout.search_index_dir()` defensively:
/// any open/search error yields an empty Vec (never panic; never hard-fail ask).
/// Prefer TermQuery on the full lowercased identifier (0141 dual-emit).
pub(crate) fn search_symbol_secondary(layout: &Layout, symbol: &str) -> Vec<SearchResult> {
    let index_path = layout.search_index_dir();
    let engine = match TantivySearchEngine::open_or_create(index_path.as_std_path()) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    engine
        .search_term_exact(symbol, LOCAL_GROUNDING_SEARCH_CAP)
        .unwrap_or_default()
}

/// Format secondary search hits into the locked evidence body (0142).
///
/// ```text
/// Search evidence for `X`:
/// - {path} (line {ln}): {snippet}
/// ```
pub(crate) fn format_search_evidence(symbol: &str, hits: &[SearchResult]) -> String {
    let mut lines = Vec::with_capacity(hits.len().saturating_add(1).min(6));
    lines.push(format!("Search evidence for `{symbol}`:"));
    for hit in hits.iter().take(LOCAL_GROUNDING_SEARCH_CAP) {
        let ln = hit
            .line_number
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".into());
        let raw = hit.snippet.as_deref().unwrap_or("");
        let snippet = crate::util::text::truncate_chars(raw, LOCAL_GROUNDING_SNIPPET_CHARS);
        lines.push(format!("- {} (line {}): {}", hit.path, ln, snippet));
    }
    lines.join("\n")
}

/// Honest local-miss body: zero hits + ledgerful search remediation (no bare grep primary).
pub(crate) fn format_local_grounding_miss(symbol: &str) -> String {
    format!(
        "Local index/search found no hits for `{symbol}`.\n\
         \n\
         Next steps:\n\
         - `ledgerful search \"{symbol}\" --auto-index`\n\
         - optional: `ledgerful index --incremental` if symbols may lag"
    )
}

/// Daily 5 argv rows (skill table). Names used for clap corpus lookup;
/// display argv tokens are skill-faithful (including flags).
const DAILY5_STEPS: &[(&str, &str, &str)] = &[
    (
        "doctor",
        "`ledgerful doctor --json`",
        "Session/env readiness (`readyForPublish`)",
    ),
    (
        "change-context",
        "`ledgerful change-context --json`",
        "Default pre-edit packet",
    ),
    (
        "ledger status",
        "`ledgerful ledger status` (`--compact` or `--json`)",
        "Provenance / pending / drift",
    ),
    (
        "search",
        "`ledgerful search …` (prefer `--auto-index` when stale)",
        "Discovery (not full impact)",
    ),
    (
        "verify",
        "`ledgerful verify --scope fast`",
        "Local gate (pre-push style); ≠ full CI; may refuse when mapping cannot scope",
    ),
];

fn product_docs_trigger() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(daily\s*5|daily\s*five|agent\s+default\s+path|session\s+start\s+commands)\b",
        )
        .ok()
    })
    .as_ref()
}

fn product_docs_impl_exclude() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    // Only true implementation words — do not exclude natural product
    // phrasing like "how does Daily 5 work".
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(compute|computes|calculation|calculations|calculate|calculates|score|scores|internal|internals|implement|implements|implementation|algorithm)\b",
        )
        .ok()
    })
    .as_ref()
}

/// Strip trailing punctuation so "Daily 5?" / "Daily 5!" still match product phrases.
fn strip_trailing_punct(s: &str) -> &str {
    s.trim_end_matches(|c: char| {
        matches!(
            c,
            '?' | '!' | '.' | ',' | ';' | ':' | '\'' | '"' | '”' | '’'
        )
    })
}

/// Recognizes product-docs / Daily 5 / agent-default-path phrasing.
/// Conservative: returns `None` for implementation questions, pure CG-F20
/// structural shapes, and plain command-discovery without product phrases.
///
/// Policy: `docs/operator-surface-policy.md` §2 (Structured sources before LLM synthesis).
pub fn parse_product_docs_intent(query: &str) -> Option<ProductDocsIntent> {
    let q = strip_trailing_punct(query.trim());
    if q.is_empty() {
        return None;
    }

    // Implementation-flavored questions fall through even if they mention Daily 5.
    if product_docs_impl_exclude().is_some_and(|re| re.is_match(q)) {
        return None;
    }

    if product_docs_trigger().is_some_and(|re| re.is_match(q)) {
        return Some(ProductDocsIntent::Daily5);
    }

    None
}

/// Builds the skill-grounded Daily 5 answer. Prefers live clap `about` text
/// from `corpus` when the qualified name matches; else skill role text.
/// Never lists CG-F31 REPO_HEALTH (different product set).
pub fn build_daily5_answer(corpus: &[CommandSurface]) -> String {
    let mut lines = Vec::with_capacity(DAILY5_STEPS.len());
    for (i, (qualified_name, display_argv, skill_role)) in DAILY5_STEPS.iter().enumerate() {
        let role = corpus
            .iter()
            .find(|c| c.qualified_name == *qualified_name)
            .map(|c| c.about.as_str())
            .unwrap_or(skill_role);
        lines.push(format!("{}. {} — {}", i + 1, display_argv, role));
    }

    format!(
        "Daily 5 (agent default path) — scannable day-to-day subset from skill:\n\
         {}\n\n\
         Honesty: step 5 may refuse when `test_mapping` cannot scope (not a surprise full suite). \
         doctor ≠ verify ≠ full CI. Escalate `scan --impact` is **not** Daily 5 \
         (B2 only: readSetCapped / high multi-module risk / unclear public API / user DoD).",
        lines.join("\n")
    )
}

// --- CG-F31: command-discovery / repo-health routing ---
//
// `ask_routing` already short-circuits structural code questions (CG-F20,
// above) before any LLM backend is consulted. CG-F31 extends the same
// "answer deterministically from indexed/structural metadata before the LLM"
// principle to a different intent class: operator questions about *which CLI
// command* to run (repo health, command discovery), as opposed to questions
// about the implementation. The corpus below is always built live from the
// active `clap::Command` tree (requirement #8) so descriptions can't go
// stale; only the *topic -> command name* curation is hardcoded, never the
// descriptive text shown to the user.

/// One flattened entry from the live clap command tree: a qualified
/// subcommand path (e.g. `"hotspots trend"`) and its `about` text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSurface {
    pub qualified_name: String,
    pub about: String,
}

/// Curated list of command names that answer "how do I check repo health /
/// current state" style questions. Grounded in this repo's own documented
/// session-start workflow: CLAUDE.md's `ledgerful.before` list and
/// `.agents/skills/ledgerful/SKILL.md`'s "Default Workflow" both name
/// `doctor`, `audit`, and `ledger status` (aliased at the top level as
/// `status`) as the canonical repo-health/session-start commands. Only the
/// *names* are hardcoded here; the descriptions shown to the user are always
/// looked up live from `build_command_corpus()` below, never duplicated here.
const REPO_HEALTH_COMMANDS: &[&str] = &["doctor", "status", "audit", "ledger status"];

/// Distinguishes the two command-discovery answer shapes CG-F31 handles.
#[derive(Debug, PartialEq, Eq)]
pub enum CommandDiscoveryIntent {
    /// "what commands show repo health?" and similar phrasings: answer from
    /// the curated `REPO_HEALTH_COMMANDS` list.
    RepoHealth,
    /// A generic "what command does/shows X" question that doesn't match the
    /// repo-health topic; answered via keyword overlap against the full
    /// live corpus.
    GenericDiscovery,
}

/// Recursively flattens the active clap `Command` tree (built from `Cli`,
/// the real CLI definition) into qualified `(name, about)` pairs. Nested
/// subcommands are qualified with a space, e.g. parent `hotspots` + child
/// `trend` -> `"hotspots trend"`. Entries with no `about` text, and any
/// hidden subcommands (e.g. a synthetic `help` subcommand, were one
/// injected), are skipped.
pub fn build_command_corpus() -> Vec<CommandSurface> {
    let root = <crate::cli::Cli as CommandFactory>::command();
    let mut out = Vec::new();
    collect_subcommands(&root, "", &mut out);
    out
}

fn collect_subcommands(cmd: &clap::Command, prefix: &str, out: &mut Vec<CommandSurface>) {
    for sub in cmd.get_subcommands() {
        if sub.is_hide_set() {
            continue;
        }
        let name = sub.get_name();
        if name == "help" {
            continue;
        }
        let qualified_name = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix} {name}")
        };

        if let Some(about) = sub.get_about() {
            out.push(CommandSurface {
                qualified_name: qualified_name.clone(),
                about: about.to_string(),
            });
        }

        collect_subcommands(sub, &qualified_name, out);
    }
}

/// Recognizes operator-intent command-discovery phrasing. Returns `None`
/// (graceful low-confidence fallback, spec requirement #7) when the query
/// doesn't look like a command-discovery question at all -- in particular,
/// implementation-flavored questions ("how does X work", "what does
/// calculate_hotspots do") must not match.
pub fn parse_command_discovery_intent(query: &str) -> Option<CommandDiscoveryIntent> {
    let q = query.trim();

    // If the query is implementation-flavored, it must fall through
    let impl_keywords = Regex::new(
        r"(?i)\b(compute|computes|calculation|calculations|calculate|calculates|score|scores|internal|internals|implement|implements|implementation|work|works|code|structure|algorithm)\b"
    ).unwrap();
    if impl_keywords.is_match(q) {
        return None;
    }

    let repo_health_trigger = Regex::new(
        r"(?i)what\s+command|which\s+command|how\s+do\s+i\s+check|how\s+can\s+i\s+check",
    )
    .unwrap();
    // The repo/project/CLI qualifier is mandatory here (no trailing `?`):
    // without it, "status"/"health" alone match any implementation-flavored
    // question (e.g. "how do I check the status of my database connection"),
    // which must fall through to `None` per spec requirement #7. The
    // `\bcommands?\b` alternative mirrors `discovery_shape` below, so "what
    // commands show repo health" style phrasing that mentions "command(s)"
    // still qualifies even without an explicit repo-qualifier word.
    let repo_health_topic = Regex::new(
        r"(?i)\b(repo|repository|project|ledgerful|ledgerful|cli)\b\s*(health|status|current\s+state|project\s+status)|\bcommands?\b.*\b(health|status|current\s+state|project\s+status)\b",
    )
    .unwrap();

    if repo_health_trigger.is_match(q) && repo_health_topic.is_match(q) {
        return Some(CommandDiscoveryIntent::RepoHealth);
    }

    // Generic command-discovery shape: must explicitly be about commands
    // (contains "command"/"commands") and phrased as a discovery question
    // ("what/which command(s) show/does/handles/runs ..."). This is
    // intentionally conservative: plain "how does X work" or "what does X
    // do" (without the word "command") must fall through unaffected so the
    // CG-F20 structural path and narrative/implementation questions are
    // unaffected.
    let mentions_command = Regex::new(r"(?i)\bcommands?\b").unwrap();
    let discovery_shape = Regex::new(
        r"(?i)\b(what|which|how)\b.*\bcommands?\b|\bcommands?\b.*\b(show|shows|does|do|handle|handles|run|runs|list|lists)\b",
    )
    .unwrap();

    if mentions_command.is_match(q) && discovery_shape.is_match(q) {
        return Some(CommandDiscoveryIntent::GenericDiscovery);
    }

    None
}

const DISCOVERY_STOPWORDS: &[&str] = &[
    "what", "which", "command", "commands", "show", "shows", "does", "do", "the", "a", "an", "i",
    "how", "is", "are", "to", "for", "of", "in", "on", "can", "me", "my", "this", "that", "with",
];

fn content_words(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !DISCOVERY_STOPWORDS.contains(w))
        .map(|w| w.to_string())
        .collect()
}

/// Builds the grounded, deterministic answer text for a recognized
/// command-discovery intent. Always looks up descriptions live from
/// `corpus` (built from the active CLI definition) rather than any
/// hardcoded paraphrase.
pub fn build_command_discovery_answer(
    intent: &CommandDiscoveryIntent,
    query: &str,
    corpus: &[CommandSurface],
) -> Option<String> {
    match intent {
        CommandDiscoveryIntent::RepoHealth => {
            let mut lines = Vec::new();
            for name in REPO_HEALTH_COMMANDS {
                if let Some(entry) = corpus.iter().find(|c| c.qualified_name == *name) {
                    lines.push(format!(
                        "- `{}` - {} (matches your question about checking repo health/status at session start.)",
                        entry.qualified_name, entry.about
                    ));
                }
            }
            if lines.is_empty() {
                None
            } else {
                Some(format!(
                    "Commands that show repo health / current state:\n{}",
                    lines.join("\n")
                ))
            }
        }
        CommandDiscoveryIntent::GenericDiscovery => {
            let query_words = content_words(query);
            if query_words.is_empty() {
                return None;
            }

            let mut scored: Vec<(usize, &CommandSurface)> = corpus
                .iter()
                .map(|entry| {
                    let entry_words =
                        content_words(&format!("{} {}", entry.qualified_name, entry.about));
                    let overlap = query_words.intersection(&entry_words).count();
                    (overlap, entry)
                })
                .filter(|(overlap, _)| *overlap > 0)
                .collect();

            if scored.is_empty() {
                return None;
            }

            scored.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then_with(|| a.1.qualified_name.cmp(&b.1.qualified_name))
            });
            let top: Vec<String> = scored
                .into_iter()
                .take(5)
                .map(|(_, entry)| format!("- `{}` - {}", entry.qualified_name, entry.about))
                .collect();

            Some(format!(
                "Commands that may answer this:\n{}",
                top.join("\n")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::storage::StorageManager;
    use tempfile::tempdir;

    #[test]
    fn test_parse_intent_recognizes_callers_phrasings() {
        assert!(matches!(
            parse_intent("what calls remove_snippets_for_files"),
            Some(ExactIntent::CallersOf(t)) if t == "remove_snippets_for_files"
        ));
        assert!(matches!(
            parse_intent("who calls remove_snippets_for_files"),
            Some(ExactIntent::CallersOf(t)) if t == "remove_snippets_for_files"
        ));
        assert!(matches!(
            parse_intent("show callers of remove_snippets_for_files"),
            Some(ExactIntent::CallersOf(t)) if t == "remove_snippets_for_files"
        ));
    }

    #[test]
    fn test_parse_intent_recognizes_callees_and_routes() {
        assert!(matches!(
            parse_intent("what does execute_ask call"),
            Some(ExactIntent::CalleesOf(t)) if t == "execute_ask"
        ));
        assert!(matches!(
            parse_intent("list route handlers"),
            Some(ExactIntent::ListRoutes)
        ));
        assert!(matches!(
            parse_intent("which handler owns route /api/users"),
            Some(ExactIntent::RouteOwner(t)) if t == "/api/users"
        ));
        assert!(matches!(
            parse_intent("where is symbol execute_ask defined"),
            Some(ExactIntent::SymbolDefinition(t)) if t == "execute_ask"
        ));
    }

    #[test]
    fn test_parse_intent_returns_none_for_narrative_questions() {
        assert!(parse_intent("what should I refactor in this module?").is_none());
        assert!(parse_intent("give me an overview of this codebase").is_none());
    }

    // --- 0142 local grounding / locate parse ---

    #[test]
    fn test_parse_intent_local_grounding_find_locate_positives() {
        let positives = [
            ("find the function verify_step_key", "verify_step_key"),
            ("find verify_step_key", "verify_step_key"),
            ("locate verify_step_key", "verify_step_key"),
            ("find the fn foo", "foo"),
            ("find FooBar", "FooBar"),
            ("locate the function verify_step_key", "verify_step_key"),
            ("find symbol run_with", "run_with"),
            ("find verify_step_key in the codebase", "verify_step_key"),
            ("find the method process_batch", "process_batch"),
            ("locate struct Layout", "Layout"),
        ];
        for (q, expected) in positives {
            assert!(
                matches!(
                    parse_intent(q),
                    Some(ExactIntent::SymbolDefinition(ref t)) if t == expected
                ),
                "expected SymbolDefinition({expected}) for: {q}, got {:?}",
                parse_intent(q)
            );
        }
    }

    #[test]
    fn test_parse_intent_local_grounding_negatives() {
        // No `where is X` without "defined"
        assert!(parse_intent("where is the config").is_none());
        assert!(parse_intent("where is the README").is_none());
        assert!(parse_intent("where is Cargo.toml").is_none());

        // Conceptual / narrative
        assert!(parse_intent("how does verify_step_key work").is_none());
        assert!(parse_intent("find the best way to handle errors").is_none());
        assert!(parse_intent("explain the architecture of indexing").is_none());
        assert!(parse_intent("how to implement verify_step_key").is_none());
        assert!(parse_intent("why does verify_step_key exist").is_none());
        assert!(parse_intent("find the architecture overview").is_none());
        assert!(parse_intent("find the best implementation").is_none());

        // ProductDocs / CG-F31 phrases must not parse as SymbolDefinition (wire owns them).
        assert!(parse_intent("What is the Daily 5?").is_none());
        assert!(parse_intent("what are the session start commands?").is_none());
        assert!(parse_intent("what commands show repo health").is_none());
    }

    #[test]
    fn test_parse_intent_local_grounding_list_routes_collision() {
        // ListRoutes must win over bare find (parse order load-bearing).
        assert!(
            matches!(
                parse_intent("find all axum route handlers"),
                Some(ExactIntent::ListRoutes)
            ),
            "expected ListRoutes, got {:?}",
            parse_intent("find all axum route handlers")
        );
    }

    #[test]
    fn test_local_grounding_banners_locked_strings() {
        assert_eq!(
            LOCAL_GROUNDING_SEARCH_BANNER,
            "Local grounding query resolved via index/search."
        );
        assert_eq!(
            LOCAL_GROUNDING_MISS_BANNER,
            "Local grounding found no index/search hits."
        );
    }

    #[test]
    fn test_format_local_grounding_miss_body_remediation() {
        let body = format_local_grounding_miss("verify_step_key");
        assert!(
            body.contains("no hits for `verify_step_key`"),
            "must state local zero hits:\n{body}"
        );
        assert!(
            body.contains("ledgerful search \"verify_step_key\" --auto-index")
                || body.contains("ledgerful search") && body.contains("--auto-index"),
            "must recommend ledgerful search --auto-index:\n{body}"
        );
        assert!(
            body.contains("ledgerful search"),
            "must name ledgerful search:\n{body}"
        );
        let lower = body.to_lowercase();
        // Banned as primary advice: bare grep/rg without ledgerful search.
        // Presence of ledgerful search is required; bare-grep-primary is not allowed.
        assert!(
            !lower.contains("use grep")
                && !lower.contains("use rg")
                && !lower.contains("run grep")
                && !lower.contains("run rg"),
            "must not recommend bare grep/rg as primary:\n{body}"
        );
    }

    #[test]
    fn test_format_search_evidence_body_shape() {
        let hits = vec![SearchResult {
            path: "src/verify/probability.rs".into(),
            line_count: 10,
            score: 1.0,
            snippet: Some("fn verify_step_key() { /* body */ }".into()),
            highlight_ranges: None,
            line_number: Some(45),
        }];
        let body = format_search_evidence("verify_step_key", &hits);
        assert!(
            body.starts_with("Search evidence for `verify_step_key`:"),
            "header:\n{body}"
        );
        assert!(
            body.contains("- src/verify/probability.rs (line 45):"),
            "path+line:\n{body}"
        );
        assert!(body.contains("verify_step_key"), "snippet:\n{body}");

        // Missing line → `?`
        let hits_no_line = vec![SearchResult {
            path: "src/foo.rs".into(),
            line_count: 1,
            score: 1.0,
            snippet: Some("x".into()),
            highlight_ranges: None,
            line_number: None,
        }];
        let body2 = format_search_evidence("x", &hits_no_line);
        assert!(
            body2.contains("(line ?):"),
            "missing line must use `?`:\n{body2}"
        );

        // Snippet truncated ≤ 120 chars
        let long = "a".repeat(200);
        let hits_long = vec![SearchResult {
            path: "p.rs".into(),
            line_count: 1,
            score: 1.0,
            snippet: Some(long),
            highlight_ranges: None,
            line_number: Some(1),
        }];
        let body3 = format_search_evidence("a", &hits_long);
        let snippet_part = body3
            .lines()
            .nth(1)
            .and_then(|l| l.split(": ").nth(1))
            .unwrap_or("");
        assert!(
            snippet_part.chars().count() <= 120,
            "snippet must be ≤120 chars, got {}",
            snippet_part.chars().count()
        );
    }

    #[test]
    fn test_residual_wording_forbids_overclaim_phrases() {
        // Global empty-chunk note (execute.rs) and CodebaseFocus oracle (local_model).
        let execute_src = include_str!("ask/execute.rs");
        let context_src = include_str!("../local_model/context.rs");
        assert!(
            execute_src.contains("no retrieved snippets for this query"),
            "execute.rs must use residual snippets wording"
        );
        assert!(
            !execute_src.contains("no project context available"),
            "execute.rs must not overclaim no project context"
        );
        assert!(
            context_src.contains("answering without retrieved snippets"),
            "local_model context must use residual snippets wording"
        );
        assert!(
            !context_src.contains("answering without codebase context"),
            "local_model context must not overclaim without codebase context"
        );
    }

    #[test]
    fn test_search_symbol_secondary_term_exact_format() {
        // tempdir + TantivySearchEngine + index_doc (not seeded_storage alone).
        use crate::search::trigram::extract_trigrams;
        use tantivy::TantivyDocument;

        let tmp = tempdir().expect("tempdir");
        let root = camino::Utf8Path::from_path(tmp.path()).expect("utf8");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("state dir");

        let engine = TantivySearchEngine::open_or_create(layout.search_index_dir().as_std_path())
            .expect("engine");
        {
            let schema = engine.schema();
            let path_field = schema.get_field("path").expect("path");
            let content_field = schema.get_field("content").expect("content");
            let line_count_field = schema.get_field("line_count").expect("line_count");
            let trigrams_field = schema.get_field("trigrams").expect("trigrams");
            let content = "fn verify_step_key() { /* 0142 secondary */ }";
            let tgrams_str = extract_trigrams(content)
                .into_iter()
                .collect::<Vec<_>>()
                .join(" ");
            let mut writer = engine.get_writer(15_000_000).expect("writer");
            let mut doc = TantivyDocument::default();
            doc.add_text(path_field, "src/verify/probability.rs");
            doc.add_text(content_field, content);
            doc.add_u64(line_count_field, 1);
            doc.add_text(trigrams_field, &tgrams_str);
            writer.add_document(doc).expect("add");
            writer.commit().expect("commit");
            engine.reload_reader().expect("reload");
        }

        let hits = search_symbol_secondary(&layout, "verify_step_key");
        assert!(
            !hits.is_empty(),
            "secondary TermQuery must hit dual-emitted full id"
        );
        let body = format_search_evidence("verify_step_key", &hits);
        assert!(
            body.contains("Search evidence for `verify_step_key`:"),
            "header:\n{body}"
        );
        assert!(body.contains("src/verify/probability.rs"), "path:\n{body}");
        assert!(
            body.lines()
                .any(|l| l.starts_with("- ") && l.contains("(line ")),
            "evidence lines:\n{body}"
        );
    }

    // --- 0139 ProductDocs / Daily 5 tests ---

    #[test]
    fn test_parse_product_docs_intent_recognizes_daily5_phrasings() {
        let positives = [
            "What is the Daily 5?",
            "what is the daily 5",
            "daily five",
            "What is Daily Five?",
            "agent default path",
            "What is the agent default path?",
            "session start commands",
            "what are the session start commands?",
            "Daily 5!",
            "tell me about the daily 5.",
        ];
        for q in positives {
            assert_eq!(
                parse_product_docs_intent(q),
                Some(ProductDocsIntent::Daily5),
                "expected ProductDocs for: {q}"
            );
        }
    }

    #[test]
    fn test_parse_product_docs_intent_ignores_non_product_queries() {
        // Implementation (no product phrase, or impl keywords)
        assert_eq!(
            parse_product_docs_intent("how does doctor compute readiness"),
            None
        );
        assert_eq!(
            parse_product_docs_intent("how does the Daily 5 compute scores"),
            None
        );
        // Pure CG-F20 structural shapes
        assert_eq!(parse_product_docs_intent("what calls execute_ask"), None);
        assert_eq!(
            parse_product_docs_intent("where is symbol execute_ask defined"),
            None
        );
        // Plain command-discovery without product phrases → leave for CG-F31
        assert_eq!(
            parse_product_docs_intent("what command shows hotspots"),
            None
        );
        assert_eq!(
            parse_product_docs_intent("what commands show repo health?"),
            None
        );
        // Random narrative / daily noise without product phrase
        assert_eq!(
            parse_product_docs_intent("what should I refactor in this module?"),
            None
        );
        assert_eq!(
            parse_product_docs_intent("daily standup notes from git"),
            None
        );
    }

    #[test]
    fn test_product_docs_wins_session_start_overlap_with_generic_discovery() {
        // Wire-order regression (AI3): this phrase matches GenericDiscovery
        // in isolation, but ProductDocs must own it when both parse.
        let q = "what are the session start commands?";
        assert_eq!(
            parse_product_docs_intent(q),
            Some(ProductDocsIntent::Daily5),
            "ProductDocs must match session-start synonym"
        );
        assert_eq!(
            parse_command_discovery_intent(q),
            Some(CommandDiscoveryIntent::GenericDiscovery),
            "GenericDiscovery may still match in isolation — wire order wins in execute_ask"
        );
    }

    #[test]
    fn test_build_daily5_answer_contains_required_argv_tokens() {
        let corpus = build_command_corpus();
        let answer = build_daily5_answer(&corpus);

        for required in [
            "doctor --json",
            "change-context --json",
            "ledger status",
            "search",
            "verify --scope fast",
        ] {
            assert!(
                answer.contains(required),
                "Daily 5 answer missing required token `{required}`:\n{answer}"
            );
        }
    }

    #[test]
    fn test_build_daily5_answer_excludes_banned_flags_and_framing() {
        let corpus = build_command_corpus();
        let answer = build_daily5_answer(&corpus);
        let lower = answer.to_lowercase();

        for banned in [
            "--machine-output",
            "--json-lines",
            "--narrative",
            "--mode",
            "--semantic",
            "--auto-scan",
        ] {
            assert!(
                !answer.contains(banned),
                "Daily 5 answer must not contain banned flag `{banned}`:\n{answer}"
            );
        }
        assert!(
            !lower.contains("top 5 findings"),
            "must not redefine Daily 5 as topFindings / top 5 findings:\n{answer}"
        );
        // Bare --json is legitimate on doctor/change-context — do not ban it.
        assert!(
            answer.contains("--json"),
            "Daily 5 must retain skill --json on doctor/change-context:\n{answer}"
        );
    }

    #[test]
    fn test_daily5_skill_sot_contains_five_argv_substrings() {
        // Tracked SoT only (docs/Ledgerful/skill.md). Path relative to this
        // source file: src/commands/ask_routing.rs → ../../docs/Ledgerful/skill.md
        let skill = include_str!("../../docs/Ledgerful/skill.md");
        for token in [
            "doctor --json",
            "change-context --json",
            "ledger status",
            "search",
            "verify --scope fast",
        ] {
            assert!(
                skill.contains(token),
                "skill.md missing Daily 5 argv substring `{token}`"
            );
        }
    }

    #[test]
    fn test_product_docs_daily5_banner_locked_string() {
        assert_eq!(
            PRODUCT_DOCS_DAILY5_BANNER,
            "Product-docs query resolved via skill Daily 5."
        );
    }

    // --- CG-F31 tests ---

    #[test]
    fn test_build_command_corpus_includes_known_health_commands() {
        let corpus = build_command_corpus();
        let names: Vec<&str> = corpus.iter().map(|c| c.qualified_name.as_str()).collect();
        assert!(names.contains(&"doctor"), "expected doctor in corpus");
        assert!(names.contains(&"status"), "expected status in corpus");
        assert!(names.contains(&"audit"), "expected audit in corpus");
        assert!(
            names.contains(&"ledger status"),
            "expected nested ledger status in corpus"
        );
        assert!(
            names.contains(&"hotspots trend"),
            "expected nested hotspots trend in corpus"
        );
        assert!(
            !names.contains(&"help"),
            "synthetic help subcommand must not leak into corpus"
        );
    }

    #[test]
    fn test_parse_command_discovery_intent_recognizes_repo_health_phrasings() {
        assert_eq!(
            parse_command_discovery_intent("what commands show repo health?"),
            Some(CommandDiscoveryIntent::RepoHealth)
        );
        assert_eq!(
            parse_command_discovery_intent("how do i check repo health"),
            Some(CommandDiscoveryIntent::RepoHealth)
        );
        assert_eq!(
            parse_command_discovery_intent("what command shows project status"),
            Some(CommandDiscoveryIntent::RepoHealth)
        );
        assert_eq!(
            parse_command_discovery_intent("which command shows the current state of the repo"),
            Some(CommandDiscoveryIntent::RepoHealth)
        );
    }

    #[test]
    fn test_parse_command_discovery_intent_recognizes_generic_discovery() {
        assert_eq!(
            parse_command_discovery_intent("what command shows hotspots"),
            Some(CommandDiscoveryIntent::GenericDiscovery)
        );
        assert_eq!(
            parse_command_discovery_intent("which command lists dependencies"),
            Some(CommandDiscoveryIntent::GenericDiscovery)
        );
    }

    #[test]
    fn test_parse_command_discovery_intent_ignores_implementation_questions() {
        assert_eq!(
            parse_command_discovery_intent("how does calculate_hotspots compute scores"),
            None
        );
        assert_eq!(
            parse_command_discovery_intent("how does the embedding pipeline work"),
            None
        );
        assert_eq!(
            parse_command_discovery_intent("what calls execute_ask"),
            None
        );
        assert_eq!(
            parse_command_discovery_intent("what should I refactor in this module?"),
            None
        );
        assert_eq!(
            parse_command_discovery_intent("give me an overview of this codebase"),
            None
        );
    }

    #[test]
    fn test_parse_command_discovery_intent_ignores_unqualified_status_questions() {
        // Regression for CG-F31 review finding: the bare words "status"/
        // "health" must not trigger `RepoHealth` without a repo/project/CLI
        // qualifier (or an explicit "command(s)" mention). These are
        // "how do I check ..." questions, which would have falsely matched
        // the old optional-qualifier topic regex.
        assert_eq!(
            parse_command_discovery_intent("how do I check the status of my database connection"),
            None
        );
        assert_eq!(
            parse_command_discovery_intent("how do I check if the status field is set"),
            None
        );
    }

    #[test]
    fn test_build_command_discovery_answer_repo_health_mentions_canonical_commands() {
        let corpus = build_command_corpus();
        let answer = build_command_discovery_answer(
            &CommandDiscoveryIntent::RepoHealth,
            "what commands show repo health?",
            &corpus,
        )
        .expect("expected a grounded repo-health answer");

        assert!(answer.contains("doctor"), "got: {answer}");
        assert!(answer.contains("status"), "got: {answer}");
        assert!(answer.contains("audit"), "got: {answer}");
    }

    #[test]
    fn test_build_command_discovery_answer_generic_returns_none_for_low_confidence() {
        let corpus = build_command_corpus();
        let answer = build_command_discovery_answer(
            &CommandDiscoveryIntent::GenericDiscovery,
            "zzzznonsensequery1234",
            &corpus,
        );
        assert!(answer.is_none());
    }

    fn seeded_storage() -> StorageManager {
        let tmp = tempdir().unwrap();
        let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES \
             (1, 'src/state/storage_cozo.rs', '2026-01-01T00:00:00Z'), \
             (2, 'src/index/incremental.rs', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO project_symbols (id, file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at) VALUES \
             (1, 1, 'remove_snippets_for_files', 'remove_snippets_for_files', 'Function', '2026-01-01T00:00:00Z'), \
             (2, 2, 'reindex_file', 'reindex_file', 'Function', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO structural_edges (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id) \
             VALUES (2, 2, 1, 1)",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO api_routes (method, path_pattern, handler_symbol_name, handler_file_id, framework, last_indexed_at) \
             VALUES ('GET', '/api/users', 'reindex_file', 2, 'axum', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        storage
    }

    #[test]
    fn test_resolve_intent_finds_real_caller() {
        let storage = seeded_storage();
        let resolved = resolve_intent(
            &ExactIntent::CallersOf("remove_snippets_for_files".to_string()),
            storage.get_connection(),
        )
        .unwrap();

        let text = resolved.expect("expected a caller to be found");
        assert!(text.contains("reindex_file"), "got: {text}");
        assert!(
            text.contains("src/index/incremental.rs"),
            "expected a file citation: {text}"
        );
    }

    #[test]
    fn test_resolve_intent_finds_route_owner() {
        let storage = seeded_storage();
        let resolved = resolve_intent(
            &ExactIntent::RouteOwner("/api/users".to_string()),
            storage.get_connection(),
        )
        .unwrap();

        let text = resolved.expect("expected a route owner to be found");
        assert!(text.contains("reindex_file"), "got: {text}");
        assert!(
            text.contains("src/index/incremental.rs"),
            "expected a file citation: {text}"
        );
    }

    #[test]
    fn test_resolve_intent_returns_none_for_unknown_symbol() {
        let storage = seeded_storage();
        let resolved = resolve_intent(
            &ExactIntent::CallersOf("nonexistent_symbol_xyz".to_string()),
            storage.get_connection(),
        )
        .unwrap();

        assert!(resolved.is_none());
    }
}
