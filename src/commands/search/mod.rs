//! Codebase search command (`ledgerful search`).
//!
//! Machine output (0136):
//! - `--json` → single agent envelope (`schemaVersion: 1`)
//! - `--json-lines` → legacy NDJSON BridgeRecord stream

mod envelope;

pub use envelope::{
    HitEmit, SearchCollector, SearchEnvelope, SearchHit, SearchIndexStatus, SearchJsonMode,
    SearchSemantic,
};

use crate::commands::helpers::get_layout;
use crate::config::load::load_config;
use crate::index::staleness::AutoIndexAction;
use crate::index::warn_if_stale;
use crate::search::{RegexFilter, TantivySearchEngine, rebuild_tantivy_index};
use crate::state::storage::StorageManager;
use camino::Utf8Path;
use miette::Result;
use owo_colors::{OwoColorize, Stream, Style};
use tracing::debug;

#[derive(Debug, Clone)]
pub struct SearchArgs {
    pub query: String,
    pub regex: bool,
    pub semantic: bool,
    pub limit: usize,
    pub index: bool,
    pub json_mode: SearchJsonMode,
    pub auto_index: bool,
    pub project_id: String,
    pub hybrid: bool,
}

impl SearchArgs {
    #[inline]
    pub fn is_machine(&self) -> bool {
        self.json_mode.is_machine()
    }
}

pub fn execute_search(args: SearchArgs) -> Result<()> {
    let layout = get_layout()?;
    let mut collector = SearchCollector::new(
        args.json_mode,
        args.project_id.clone(),
        args.query.clone(),
        args.limit,
    );

    // Track auto-index action so FTS rebuilds only when SQLite work ran (0128).
    let mut auto_index_action = AutoIndexAction::None;

    // --- Staleness check (applies to both semantic and BM25 paths) ---
    if !args.index {
        let config = load_config(&layout)?;
        let threshold = config.index.stale_threshold_days;
        let storage_opt = StorageManager::open_read_only(&layout).ok();

        if args.auto_index {
            // Missing DB must still bootstrap under --auto-index (same as ask).
            let storage = match storage_opt {
                Some(s) => s,
                None => {
                    layout.ensure_state_dir()?;
                    StorageManager::init_with_layout(&layout)?
                }
            };
            match crate::index::staleness::try_auto_index(storage, threshold, &layout) {
                Ok((_storage, action)) => {
                    auto_index_action = action;
                }
                Err(e) => {
                    // B4 / 0136 B3: greppable remediation on stderr for human;
                    // machine modes emit **no** stdout then Err.
                    emit_auto_index_failed(&args, &e);
                    return Err(e);
                }
            }
        } else if let Some(storage) = storage_opt {
            let is_stale = warn_if_stale(&storage, threshold);
            if is_stale && !args.is_machine() && crate::util::term::is_interactive() {
                use inquire::Confirm;
                if let Ok(true) =
                    Confirm::new("Index is stale. Would you like to run auto-index now?")
                        .with_default(true)
                        .prompt()
                {
                    println!("Running auto-indexing...");
                    match crate::index::staleness::try_auto_index(storage, threshold, &layout) {
                        Ok((_storage, action)) => {
                            auto_index_action = action;
                        }
                        Err(e) => {
                            emit_auto_index_failed(&args, &e);
                            return Err(e);
                        }
                    }
                }
            }
        }
    }

    // 0128 R1: rebuild FTS *before* any semantic early-return. Semantic hits
    // must not leave BM25 content-stale after SQLite Full/Incremental work.
    let auto_index_ran_work = matches!(
        auto_index_action,
        AutoIndexAction::FullBootstrap | AutoIndexAction::Incremental { .. }
    );
    let mut fts_rebuilt_for_auto_index = false;
    if auto_index_ran_work {
        if !args.is_machine() {
            println!(
                "{} Indexing repository for search...",
                "INIT".if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold()))
            );
        }
        debug!("Post-auto-index full FTS rebuild (before semantic/BM25 query path)");
        match rebuild_tantivy_index(&layout) {
            Ok(()) => {
                fts_rebuilt_for_auto_index = true;
                if !args.is_machine() {
                    println!(
                        "{} Index built successfully.\n",
                        "DONE".if_supports_color(Stream::Stdout, |s| s
                            .style(Style::new().green().bold()))
                    );
                }
            }
            Err(e) => {
                // B4: greppable residual; continue so semantic/BM25 can still run.
                emit_fts_rebuild_failed(&args, &mut collector, /*document_count=*/ None, &e);
            }
        }
    }

    if args.semantic {
        collector.set_engine_mode("semantic");
        let config = load_config(&layout)?;
        let storage = StorageManager::open_read_only(&layout)?;
        let cozo = storage
            .cozo
            .as_ref()
            .ok_or_else(|| miette::miette!("CozoDB storage not initialized"))?;

        let semantic_engine =
            crate::semantic::SemanticDiscovery::new(config.local_model.clone(), cozo)?;

        // --- Phase 1: Readiness Check ---
        // Interactive auto-index prompt removed (0096 DoD-5): it named
        // `index --semantic` but ran incremental without semantic, and
        // re-prompted forever on repos with nothing to index. Explicit
        // state-driven warnings replace it.
        let readiness = semantic_engine.check_readiness()?;

        if args.is_machine() {
            collector.set_semantic_readiness(&readiness);
        } else {
            for msg in crate::semantic::semantic_readiness_messages(&readiness) {
                let is_error = msg.contains("dimension mismatch") || msg.contains("Dimension");
                if is_error {
                    println!(
                        "{} {}",
                        "ERROR".if_supports_color(Stream::Stdout, |s| s
                            .style(Style::new().red().bold())),
                        msg
                    );
                } else {
                    println!(
                        "{} {}",
                        "WARN".if_supports_color(Stream::Stdout, |s| s
                            .style(Style::new().yellow().bold())),
                        msg
                    );
                }
            }
        }

        debug!("Performing semantic search for: {}", args.query);
        if !args.is_machine() {
            println!("[Search Mode: Semantic]");
        }
        // On Err: print *failure* message (never Ready "no matches") and fall through.
        // On Ok([]): print empty-result once in the empty branch below.
        // Never both (P3 double-emit). JSON Err emits semantic.error / semantic_error.
        // Overfetch limit+1 so human output can show "and more" without claiming K (0100 DoD-8).
        let semantic_fetch = args.limit.saturating_add(1);
        let (mut results, query_succeeded) = match semantic_engine
            .query(&args.query, semantic_fetch)
        {
            Ok(r) => (r, true),
            Err(e) => {
                // Unconfigured / unreachable / Ready runtime failure: degrade to BM25
                // with honesty about whether the search ran or failed.
                let failure_msg = crate::semantic::semantic_query_failure_message(&readiness, &e);
                if args.is_machine() {
                    collector.set_semantic_error(failure_msg);
                } else {
                    println!(
                        "{} {}",
                        "WARN".if_supports_color(Stream::Stdout, |s| s
                            .style(Style::new().yellow().bold())),
                        failure_msg
                    );
                }
                debug!("Semantic query failed: {e}");
                (Vec::new(), false)
            }
        };

        if !results.is_empty() {
            let truncated = results.len() > args.limit;
            results.truncate(args.limit);
            collector.set_truncated(truncated);
            if args.is_machine() {
                for (path, name, offset, dist) in results {
                    let score = 1.0 - dist as f64;
                    let content = format!("{} (offset {}, dist {:.4})", name, offset, dist);
                    let bridge_content = content.clone();
                    let memory_id = format!("{}::{}", path, name);
                    collector.push_hit(HitEmit {
                        kind: "insight",
                        path,
                        line: None,
                        score: Some(score),
                        content,
                        bridge_content,
                        bridge_relevance: score,
                        bridge_memory_id: memory_id,
                    });
                }
            } else {
                println!(
                    "\n{}",
                    "Semantic Search Results:"
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
                );
                for (path, name, offset, dist) in results {
                    println!(
                        "- {} ({} at offset {}) [dist: {:.4}]",
                        name.if_supports_color(Stream::Stdout, |s| s.bold()),
                        path,
                        offset,
                        dist
                    );
                }
                if truncated {
                    print_search_truncation_affordance();
                }
                println!();
            }
            collector.finish();
            return Ok(());
        }

        // Only after a successful query that returned no hits (true empty / no-matches).
        if query_succeeded && !args.is_machine() {
            println!(
                "{} ⚠️ {}",
                "WARN".if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold())),
                crate::semantic::semantic_empty_result_message(&readiness)
            );
        }
    }

    let mut use_regex = args.regex;
    let mut use_hybrid = args.hybrid;
    if !args.semantic && !args.regex && !use_hybrid {
        if is_regex_likely(&args.query) {
            use_regex = true;
        } else if is_identifier_likely(&args.query) {
            use_hybrid = true;
        }
    }

    let engine_mode = if use_hybrid {
        "hybrid"
    } else if use_regex {
        "regex"
    } else {
        "bm25"
    };
    // Semantic fallthrough keeps semantic meta but engine_mode reflects BM25 path
    // actually used for hits (requested mode already stored if pure semantic returned).
    if !args.semantic {
        collector.set_engine_mode(engine_mode);
    } else {
        // Fallthrough after semantic empty/error: hits are non-semantic; mode still
        // names the BM25/hybrid/regex path that produces results[].kind.
        collector.set_engine_mode(engine_mode);
    }

    let index_path = layout.search_index_dir();
    let engine = TantivySearchEngine::open_or_create(index_path.as_std_path())?;

    // 0126: capture pre_count BEFORE rebuild — StreamIndexer consumes engine.
    let pre_count = engine.document_count();

    // 0128: full FTS rebuild when auto-index ran SQLite Full/Incremental work
    // (already done above if successful), or explicit --index / empty docs.
    // Never rebuild on every search when AutoIndexAction::None and docs present.
    let needs_fts_rebuild =
        args.index || pre_count == 0 || (auto_index_ran_work && !fts_rebuilt_for_auto_index);

    let engine = if needs_fts_rebuild {
        if !args.is_machine() && !fts_rebuilt_for_auto_index {
            println!(
                "{} Indexing repository for search...",
                "INIT".if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold()))
            );
        }
        debug!("Indexing repository for search...");
        match rebuild_tantivy_index(&layout) {
            Ok(()) => {
                if !args.is_machine() {
                    println!(
                        "{} Index built successfully.\n",
                        "DONE".if_supports_color(Stream::Stdout, |s| s
                            .style(Style::new().green().bold()))
                    );
                }
                let engine = TantivySearchEngine::open_or_create(index_path.as_std_path())?;
                engine.verify_index_integrity(index_path.as_std_path())?;
                debug!("Tantivy index integrity verified.");
                engine
            }
            Err(e) if auto_index_ran_work && !args.index => {
                // B4 residual (retry after early rebuild failed, or empty-doc path).
                emit_fts_rebuild_failed(&args, &mut collector, Some(pre_count), &e);
                engine
            }
            Err(e) => return Err(e),
        }
    } else if fts_rebuilt_for_auto_index {
        // Early rebuild already ran — re-open so BM25 sees fresh docs.
        let engine = TantivySearchEngine::open_or_create(index_path.as_std_path())?;
        engine.verify_index_integrity(index_path.as_std_path())?;
        engine
    } else {
        engine
    };

    let post_count = engine.document_count();

    // Empty-index honesty: do not collapse into silent no-matches for agents.
    if pre_count == 0 {
        emit_search_index_status(&args, &mut collector, post_count);
        // Still empty after rebuild: status alone is enough (skip zero-hit noise).
        if post_count == 0 {
            collector.finish();
            return Ok(());
        }
    }

    perform_search(
        engine,
        &layout.root,
        &args,
        &mut collector,
        use_regex,
        use_hybrid,
    )?;

    collector.finish();
    Ok(())
}

/// B4 honesty when post-auto-index full FTS rebuild fails.
fn emit_fts_rebuild_failed(
    args: &SearchArgs,
    collector: &mut SearchCollector,
    document_count: Option<usize>,
    err: &miette::Report,
) {
    if args.is_machine() {
        collector.set_search_index_status(SearchIndexStatus {
            state: "fts_rebuild_failed".to_string(),
            document_count,
            remediation: Some("ledgerful index --incremental".to_string()),
            error: None,
        });
    } else {
        eprintln!(
            "{} Search full-text rebuild failed after auto-index: {err}. Run {} to refresh BM25.",
            "WARN".if_supports_color(Stream::Stderr, |s| s.style(Style::new().yellow().bold())),
            "ledgerful index --incremental"
                .if_supports_color(Stream::Stderr, |s| s.style(Style::new().cyan().bold()))
        );
    }
}

/// B4 / 0136 B3: SQLite auto-index (`try_auto_index`) fails before FTS.
/// Under **both** machine modes: **no** machine stdout (then caller returns Err).
fn emit_auto_index_failed(args: &SearchArgs, err: &miette::Report) {
    if args.is_machine() {
        // Fatal class: no BridgeRecord line, no partial envelope.
        return;
    }
    eprintln!(
        "{} Search auto-index failed: {err}. Run {} to refresh the index.",
        "WARN".if_supports_color(Stream::Stderr, |s| s.style(Style::new().yellow().bold())),
        "ledgerful index --incremental"
            .if_supports_color(Stream::Stderr, |s| s.style(Style::new().cyan().bold()))
    );
}

/// Emit human WARN / machine `searchIndexStatus` when the index was empty before
/// query (0126).
fn emit_search_index_status(args: &SearchArgs, collector: &mut SearchCollector, post_count: usize) {
    let state = if post_count == 0 {
        "empty_after_rebuild"
    } else {
        "was_empty"
    };

    if args.is_machine() {
        let remediation = if post_count == 0 {
            Some(
                "Rebuild already ran; no indexable content found (empty repo, \
                 ignore patterns, or filters). Check ignore patterns. \
                 `ledgerful index` may re-try but will not invent files."
                    .to_string(),
            )
        } else {
            None
        };
        collector.set_search_index_status(SearchIndexStatus {
            state: state.to_string(),
            document_count: Some(post_count),
            remediation,
            error: None,
        });
    } else if post_count == 0 {
        println!(
            "{} Search index empty after rebuild (0 documents). No indexable \
             content found — check ignore patterns; empty repo or filters may \
             leave the index empty.",
            "WARN".if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
        );
    } else {
        println!(
            "{} Search index was empty; rebuilt to {post_count} document(s).",
            "WARN".if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
        );
    }
}

pub fn is_regex_likely(query: &str) -> bool {
    query.chars().any(|c| {
        matches!(
            c,
            '^' | '$' | '.' | '*' | '+' | '?' | '[' | ']' | '(' | ')' | '|'
        )
    })
}

pub fn is_identifier_likely(query: &str) -> bool {
    !query.is_empty()
        && !query.contains(' ')
        && query
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == ':')
}

fn perform_search(
    engine: TantivySearchEngine,
    root: &Utf8Path,
    args: &SearchArgs,
    collector: &mut SearchCollector,
    use_regex: bool,
    use_hybrid: bool,
) -> Result<()> {
    // Overfetch by one so human path can emit a truncation affordance without
    // claiming an exact "K more" total (0100 DoD-7). Envelope uses the same
    // signal for `truncated`.
    let overfetch = args.limit.saturating_add(1);

    if use_hybrid {
        if !args.is_machine() {
            println!("[Search Mode: Hybrid]");
        }
        let filter = RegexFilter::new(&engine);
        let regex_matches = filter
            .search(root, &args.query, overfetch)
            .unwrap_or_default();
        let bm25_results = engine.search(&args.query, overfetch).unwrap_or_default();

        struct MergedResult {
            path: String,
            line_number: Option<usize>,
            /// Plain fragment for JSON and for emphasis application at print time.
            content: String,
            /// Byte ranges into `content` for gated owo_colors emphasis (human only).
            highlight_ranges: Vec<(usize, usize)>,
            score: Option<f32>,
            is_regex: bool,
        }

        let mut merged: std::collections::HashMap<(String, Option<usize>), MergedResult> =
            std::collections::HashMap::new();

        for r in bm25_results {
            // Seed from plain fragment (not pre-rendered highlighted). Emphasis
            // is applied only on the human path; machine content stays plain (DoD-5).
            let content = r.snippet.clone().unwrap_or_default();
            let highlight_ranges = r.highlight_ranges.unwrap_or_default();
            merged.insert(
                (r.path.clone(), r.line_number),
                MergedResult {
                    path: r.path,
                    line_number: r.line_number,
                    content,
                    highlight_ranges,
                    score: Some(r.score),
                    is_regex: false,
                },
            );
        }

        for m in regex_matches {
            let key = (m.path.clone(), Some(m.line_number));
            let mut boost = 5.0;
            if m.path.ends_with(".rs")
                || m.path.ends_with(".ts")
                || m.path.ends_with(".js")
                || m.path.ends_with(".go")
                || m.path.ends_with(".py")
            {
                boost += 10.0;
            }
            if let Some(existing) = merged.get_mut(&key) {
                // Boost existing BM25 score
                let current_score = existing.score.unwrap_or(0.0);
                existing.score = Some(current_score + boost);
                existing.is_regex = true;
            } else {
                merged.insert(
                    key,
                    MergedResult {
                        path: m.path,
                        line_number: Some(m.line_number),
                        content: m.content,
                        highlight_ranges: Vec::new(),
                        score: Some(boost),
                        is_regex: true,
                    },
                );
            }
        }

        let mut merged_results: Vec<MergedResult> = merged.into_values().collect();
        merged_results.sort_by(|a, b| {
            let score_a = a.score.unwrap_or(0.0);
            let score_b = b.score.unwrap_or(0.0);
            score_b
                .partial_cmp(&score_a)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let truncated = merged_results.len() > args.limit;
        merged_results.truncate(args.limit);
        collector.set_truncated(truncated);

        if merged_results.is_empty() {
            handle_fuzzy_fallback(&engine, args, collector);
        } else if args.is_machine() {
            for res in merged_results {
                let kind = if res.is_regex {
                    "regex_match"
                } else {
                    "bm25_match"
                };
                let score = res.score.map(|s| s as f64);
                let bridge_content = if res.is_regex {
                    format!(
                        "{}:{}: {}",
                        res.path,
                        res.line_number.unwrap_or(0),
                        res.content
                    )
                } else {
                    format!("{} ({})", res.path, res.content)
                };
                let memory_id = if let Some(line) = res.line_number {
                    format!("{}::{}", res.path, line)
                } else {
                    res.path.clone()
                };
                let relevance = res.score.unwrap_or(1.0) as f64;
                collector.push_hit(HitEmit {
                    kind,
                    path: res.path,
                    line: res.line_number,
                    score,
                    content: res.content,
                    bridge_content,
                    bridge_relevance: relevance,
                    bridge_memory_id: memory_id,
                });
            }
        } else {
            println!(
                "\n{}",
                "Hybrid Search Results (BM25 + Regex):"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
            );
            for res in merged_results {
                let line_info = if let Some(line) = res.line_number {
                    format!(
                        ":{}",
                        line.to_string()
                            .if_supports_color(Stream::Stdout, |s| s.yellow())
                    )
                } else {
                    String::new()
                };
                let source_label = if res.is_regex {
                    "[Regex]"
                        .if_supports_color(Stream::Stdout, |s| s.magenta())
                        .to_string()
                } else {
                    "[BM25]"
                        .if_supports_color(Stream::Stdout, |s| s.green())
                        .to_string()
                };
                let score_info = if let Some(score) = res.score {
                    format!(" [score: {:.2}]", score)
                } else {
                    String::new()
                };
                // Apply emphasis at print time via if_supports_color (0131 colour
                // gate). Under NO_COLOR / non-TTY, escapes are suppressed.
                let display = emphasize_snippet(&res.content, &res.highlight_ranges);
                println!(
                    "{} {}{} {}",
                    source_label,
                    format!(
                        "{}{}",
                        res.path.if_supports_color(Stream::Stdout, |s| s.cyan()),
                        line_info
                    )
                    .if_supports_color(Stream::Stdout, |s| s.bold()),
                    score_info.if_supports_color(Stream::Stdout, |s| s.yellow()),
                    display.trim()
                );
            }
            if truncated {
                print_search_truncation_affordance();
            }
            println!();
        }
    } else if use_regex {
        if !args.is_machine() {
            println!("[Search Mode: Regex]");
        }
        let filter = RegexFilter::new(&engine);
        let mut matches = filter.search(root, &args.query, overfetch)?;
        let truncated = matches.len() > args.limit;
        matches.truncate(args.limit);
        collector.set_truncated(truncated);

        if args.is_machine() {
            for m in matches {
                let bridge_content = format!("{}:{}: {}", m.path, m.line_number, m.content);
                let memory_id = format!("{}::{}", m.path, m.line_number);
                collector.push_hit(HitEmit {
                    kind: "regex_match",
                    path: m.path,
                    line: Some(m.line_number),
                    score: Some(1.0),
                    content: m.content,
                    bridge_content,
                    bridge_relevance: 1.0,
                    bridge_memory_id: memory_id,
                });
            }
        } else {
            println!(
                "\n{}",
                "Regex Search Results:"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
            );
            if matches.is_empty() {
                println!("No matches found.");
                println!(
                    "{} Check your regex syntax or run {} if changes are missing.",
                    "HINT".if_supports_color(Stream::Stdout, |s| s
                        .style(Style::new().yellow().bold())),
                    "ledgerful index"
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold()))
                );
            } else {
                for m in matches {
                    println!(
                        "{}:{}: {}",
                        m.path.if_supports_color(Stream::Stdout, |s| s.cyan()),
                        m.line_number
                            .to_string()
                            .if_supports_color(Stream::Stdout, |s| s.yellow()),
                        m.content.trim()
                    );
                }
                if truncated {
                    print_search_truncation_affordance();
                }
            }
            println!();
        }
    } else {
        if !args.is_machine() {
            println!("[Search Mode: BM25]");
        }
        let mut results = engine.search(&args.query, overfetch)?;
        let truncated = results.len() > args.limit;
        results.truncate(args.limit);
        collector.set_truncated(truncated);

        if results.is_empty() {
            handle_fuzzy_fallback(&engine, args, collector);
        } else if args.is_machine() {
            for r in results {
                let content = r.snippet.unwrap_or_default();
                let bridge_content = format!("{} ({})", r.path, content);
                let memory_id = r.path.clone();
                let score = r.score as f64;
                collector.push_hit(HitEmit {
                    kind: "bm25_match",
                    path: r.path,
                    line: r.line_number,
                    score: Some(score),
                    content,
                    bridge_content,
                    bridge_relevance: score,
                    bridge_memory_id: memory_id,
                });
            }
        } else {
            println!(
                "\n{}",
                "Ranked Search Results (BM25):"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
            );
            for r in results {
                let line_info = if let Some(line) = r.line_number {
                    format!(
                        ":{}",
                        line.to_string()
                            .if_supports_color(Stream::Stdout, |s| s.yellow())
                    )
                } else {
                    String::new()
                };
                println!(
                    "{} [score: {:.2}]",
                    format!(
                        "{}{}",
                        r.path.if_supports_color(Stream::Stdout, |s| s.cyan()),
                        line_info
                    )
                    .if_supports_color(Stream::Stdout, |s| s.bold()),
                    r.score.if_supports_color(Stream::Stdout, |s| s.yellow())
                );
                if let Some(snippet) = r.snippet {
                    let ranges = r.highlight_ranges.as_deref().unwrap_or(&[]);
                    let display = emphasize_snippet(&snippet, ranges);
                    println!("  {}", display.trim());
                }
            }
            if truncated {
                print_search_truncation_affordance();
            }
            println!();
        }
    }

    Ok(())
}

/// Human-only truncation affordance (0100 DoD-7). No exact remaining count —
/// engines do not always return a total. Machine paths never call this.
fn print_search_truncation_affordance() {
    println!("… and more results (use --limit N to see more)");
}

/// Apply bold emphasis to byte ranges in a plain snippet via owo_colors.
/// Ranges that are out of bounds or mid-character are skipped.
fn emphasize_snippet(fragment: &str, ranges: &[(usize, usize)]) -> String {
    if ranges.is_empty() {
        return fragment.to_string();
    }
    let mut sorted: Vec<(usize, usize)> = ranges
        .iter()
        .copied()
        .filter(|(s, e)| {
            *s <= *e
                && *e <= fragment.len()
                && fragment.is_char_boundary(*s)
                && fragment.is_char_boundary(*e)
        })
        .collect();
    sorted.sort_by_key(|(s, e)| (*s, *e));

    let mut out = String::new();
    let mut last = 0usize;
    for (start, end) in sorted {
        if start < last {
            continue;
        }
        out.push_str(&fragment[last..start]);
        let piece = &fragment[start..end];
        out.push_str(
            &piece
                .if_supports_color(Stream::Stdout, |s| s.bold())
                .to_string(),
        );
        last = end;
    }
    if last <= fragment.len() && fragment.is_char_boundary(last) {
        out.push_str(&fragment[last..]);
    }
    out
}

fn handle_fuzzy_fallback(
    engine: &TantivySearchEngine,
    args: &SearchArgs,
    collector: &mut SearchCollector,
) {
    if !args.is_machine() {
        println!(
            "{}",
            "Falling back to fuzzy search...".if_supports_color(Stream::Stdout, |s| s.yellow())
        );
    }
    // Overfetch limit+1 for human truncation affordance (0100 DoD-8; codex R1).
    let fuzzy_fetch = args.limit.saturating_add(1);
    let mut fuzzy_matches = match engine.search_fuzzy(&args.query, fuzzy_fetch) {
        Ok(matches) => matches,
        Err(e) => {
            tracing::warn!("Fuzzy search fallback failed: {}", e);
            Vec::new()
        }
    };

    if fuzzy_matches.is_empty() {
        if !args.is_machine() {
            println!("No matches found.");
            println!(
                "{} No exact symbols found. Try {} or {}.",
                "HINT:"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold())),
                "--regex"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold())),
                "ledgerful index"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold()))
            );
            println!(
                "      Alternatively, try semantic search instead: {}",
                format!("ledgerful ask \"{}\"", args.query)
                    .if_supports_color(Stream::Stdout, |s| s.cyan())
            );
        }
        // Envelope: empty results still finish with resultCount 0 (caller finishes).
    } else {
        let truncated = fuzzy_matches.len() > args.limit;
        fuzzy_matches.truncate(args.limit);
        collector.set_truncated(truncated);
        if args.is_machine() {
            for m in fuzzy_matches {
                let content = m.snippet.as_deref().unwrap_or_default().to_string();
                let line = m.line_number;
                let bridge_content = format!("{}:{}: {}", m.path, line.unwrap_or(1), content);
                let memory_id = format!("{}::{}", m.path, line.unwrap_or(1));
                let score = m.score as f64;
                collector.push_hit(HitEmit {
                    kind: "fuzzy_match",
                    path: m.path,
                    line,
                    score: Some(score),
                    content,
                    bridge_content,
                    bridge_relevance: score,
                    bridge_memory_id: memory_id,
                });
            }
        } else {
            println!(
                "\n{}",
                "Fuzzy Search Results:"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
            );
            for m in fuzzy_matches {
                let line_info = if let Some(line) = m.line_number {
                    format!(
                        ":{}",
                        line.to_string()
                            .if_supports_color(Stream::Stdout, |s| s.yellow())
                    )
                } else {
                    String::new()
                };
                println!(
                    "{} [score: {:.2}]",
                    format!(
                        "{}{}",
                        m.path.if_supports_color(Stream::Stdout, |s| s.cyan()),
                        line_info
                    )
                    .if_supports_color(Stream::Stdout, |s| s.bold()),
                    m.score.if_supports_color(Stream::Stdout, |s| s.yellow())
                );
                if let Some(snippet) = m.snippet {
                    let ranges = m.highlight_ranges.as_deref().unwrap_or(&[]);
                    let display = emphasize_snippet(&snippet, ranges);
                    println!("  {}", display.trim());
                }
            }
            if truncated {
                print_search_truncation_affordance();
            }
            println!();
        }
    }
}

pub fn execute_search_trigrams(trigrams: Vec<String>, limit: usize) -> Result<()> {
    let layout = get_layout()?;
    let index_path = layout.search_index_dir();
    let engine = TantivySearchEngine::open_or_create(index_path.as_std_path())?;
    let results = engine.search_trigrams(&trigrams, limit)?;
    for path in results {
        println!("{path}");
    }
    Ok(())
}
