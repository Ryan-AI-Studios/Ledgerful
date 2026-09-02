use clap::CommandFactory;
use miette::{IntoDiagnostic, Result};
use rusqlite::Connection;
use std::collections::HashSet;

use crate::search::tantivy_engine::{SearchResult, TantivySearchEngine};
use crate::state::layout::Layout;

use super::parse::{CommandDiscoveryIntent, ExactIntent};

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
        "Provenance / pending / drift; names workRoot (cd or -C before ledger start)",
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
/// session-start workflow: AGENTS.md / CLAUDE.md `ledgerful.before` name
/// `doctor`, `audit`, and `ledger status` (aliased at the top level as
/// `status`). The AI-only skill (`docs/Ledgerful/skill.md`) Daily 5 is a
/// subset (no audit). Only the *names* are hardcoded here; the descriptions
/// shown to the user are always looked up live from `build_command_corpus()`
/// below, never duplicated here.
const REPO_HEALTH_COMMANDS: &[&str] = &["doctor", "status", "audit", "ledger status"];

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
