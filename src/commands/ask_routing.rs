use clap::CommandFactory;
use miette::{IntoDiagnostic, Result};
use regex::Regex;
use rusqlite::Connection;
use std::collections::HashSet;

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

    None
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
