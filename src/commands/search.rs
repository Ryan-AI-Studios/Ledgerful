use crate::bridge::model::{BridgeDirection, BridgePayload, BridgeRecord, Privacy};
use crate::commands::helpers::get_layout;
use crate::config::load::load_config;
use crate::index::staleness::AutoIndexAction;
use crate::index::warn_if_stale;
use crate::search::{RegexFilter, TantivySearchEngine, rebuild_tantivy_index};
use crate::state::storage::StorageManager;
use camino::Utf8Path;
use miette::Result;
use owo_colors::OwoColorize;
use tracing::debug;

#[derive(Debug, Clone)]
pub struct SearchArgs {
    pub query: String,
    pub regex: bool,
    pub semantic: bool,
    pub limit: usize,
    pub index: bool,
    pub json: bool,
    pub auto_index: bool,
    pub project_id: String,
    pub hybrid: bool,
}

pub fn execute_search(args: SearchArgs) -> Result<()> {
    let layout = get_layout()?;

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
            let (_storage, action) =
                crate::index::staleness::try_auto_index(storage, threshold, &layout)?;
            auto_index_action = action;
        } else if let Some(storage) = storage_opt {
            let is_stale = warn_if_stale(&storage, threshold);
            if is_stale && !args.json && crate::util::term::is_interactive() {
                use inquire::Confirm;
                if let Ok(true) =
                    Confirm::new("Index is stale. Would you like to run auto-index now?")
                        .with_default(true)
                        .prompt()
                {
                    println!("Running auto-indexing...");
                    let (_storage, action) =
                        crate::index::staleness::try_auto_index(storage, threshold, &layout)?;
                    auto_index_action = action;
                }
            }
        }
    }

    if args.semantic {
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

        if args.json {
            let record = BridgeRecord {
                bridge_version: BridgeRecord::VERSION.to_string(),
                direction: BridgeDirection::Outbound,
                timestamp: chrono::Utc::now(),
                parent_hash: None,
                project_id: args.project_id.clone(),
                session_id: None,
                tx_id: None,
                record_kind: "semantic_readiness".to_string(),
                payload: BridgePayload::Insight {
                    memory_id: "readiness".to_string(),
                    relevance: 1.0,
                    content: serde_json::to_string(&readiness).unwrap_or_default(),
                },
                privacy: Privacy::ProjectLocal,
            };
            println!("{}", serde_json::to_string(&record).unwrap_or_default());
        } else {
            for msg in crate::semantic::semantic_readiness_messages(&readiness) {
                let is_error = msg.contains("dimension mismatch") || msg.contains("Dimension");
                if is_error {
                    println!("{} {}", "ERROR".red().bold(), msg);
                } else {
                    println!("{} {}", "WARN".yellow().bold(), msg);
                }
            }
        }

        debug!("Performing semantic search for: {}", args.query);
        if !args.json {
            println!("[Search Mode: Semantic]");
        }
        // On Err: print *failure* message (never Ready "no matches") and fall through.
        // On Ok([]): print empty-result once in the empty branch below.
        // Never both (P3 double-emit). JSON Err emits record_kind "semantic_error".
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
                if args.json {
                    let record = BridgeRecord {
                        bridge_version: BridgeRecord::VERSION.to_string(),
                        direction: BridgeDirection::Outbound,
                        timestamp: chrono::Utc::now(),
                        parent_hash: None,
                        project_id: args.project_id.clone(),
                        session_id: None,
                        tx_id: None,
                        record_kind: "semantic_error".to_string(),
                        payload: BridgePayload::Insight {
                            memory_id: "semantic_error".to_string(),
                            relevance: 0.0,
                            content: failure_msg,
                        },
                        privacy: Privacy::ProjectLocal,
                    };
                    println!("{}", serde_json::to_string(&record).unwrap_or_default());
                } else {
                    println!("{} {}", "WARN".yellow().bold(), failure_msg);
                }
                debug!("Semantic query failed: {e}");
                (Vec::new(), false)
            }
        };

        if !results.is_empty() {
            let truncated = results.len() > args.limit;
            results.truncate(args.limit);
            if args.json {
                for (path, name, offset, dist) in results {
                    let record = BridgeRecord {
                        bridge_version: BridgeRecord::VERSION.to_string(),
                        direction: BridgeDirection::Outbound,
                        timestamp: chrono::Utc::now(),
                        parent_hash: None,
                        project_id: args.project_id.clone(),
                        session_id: None,
                        tx_id: None,
                        record_kind: "insight".to_string(),
                        payload: BridgePayload::Insight {
                            memory_id: format!("{}::{}", path, name),
                            relevance: 1.0 - dist as f64,
                            content: format!("{} (offset {}, dist {:.4})", name, offset, dist),
                        },
                        privacy: Privacy::ProjectLocal,
                    };
                    println!("{}", serde_json::to_string(&record).unwrap_or_default());
                }
            } else {
                println!("\n{}", "Semantic Search Results:".bold().cyan());
                for (path, name, offset, dist) in results {
                    println!(
                        "- {} ({} at offset {}) [dist: {:.4}]",
                        name.bold(),
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
            return Ok(());
        }

        // Only after a successful query that returned no hits (true empty / no-matches).
        if query_succeeded && !args.json {
            println!(
                "{} ⚠️ {}",
                "WARN".yellow().bold(),
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

    let index_path = layout.search_index_dir();
    let engine = TantivySearchEngine::open_or_create(index_path.as_std_path())?;

    // 0126: capture pre_count BEFORE rebuild — StreamIndexer consumes engine.
    let pre_count = engine.document_count();

    // 0128: full FTS rebuild when auto-index ran SQLite Full/Incremental work,
    // or existing paths (explicit --index / empty document_count). Never rebuild
    // on every search when AutoIndexAction::None and docs already present.
    let auto_index_ran_work = matches!(
        auto_index_action,
        AutoIndexAction::FullBootstrap | AutoIndexAction::Incremental { .. }
    );
    let needs_fts_rebuild = args.index || pre_count == 0 || auto_index_ran_work;

    let engine = if needs_fts_rebuild {
        if !args.json {
            println!("{} Indexing repository for search...", "INIT".cyan().bold());
        }
        debug!("Indexing repository for search...");
        match rebuild_tantivy_index(&layout) {
            Ok(()) => {
                if !args.json {
                    println!("{} Index built successfully.\n", "DONE".green().bold());
                }
                let engine = TantivySearchEngine::open_or_create(index_path.as_std_path())?;
                engine.verify_index_integrity(index_path.as_std_path())?;
                debug!("Tantivy index integrity verified.");
                engine
            }
            Err(e) if auto_index_ran_work && !args.index => {
                // B4: greppable residual after failed post-auto-index FTS rebuild.
                if args.json {
                    let content = serde_json::json!({
                        "state": "fts_rebuild_failed",
                        "document_count": pre_count,
                        "remediation": "ledgerful index --incremental",
                    });
                    let record = BridgeRecord {
                        bridge_version: BridgeRecord::VERSION.to_string(),
                        direction: BridgeDirection::Outbound,
                        timestamp: chrono::Utc::now(),
                        parent_hash: None,
                        project_id: args.project_id.clone(),
                        session_id: None,
                        tx_id: None,
                        record_kind: "search_index_status".to_string(),
                        payload: BridgePayload::Insight {
                            memory_id: "search_index_status".to_string(),
                            relevance: 0.0,
                            content: serde_json::to_string(&content).unwrap_or_default(),
                        },
                        privacy: Privacy::ProjectLocal,
                    };
                    println!("{}", serde_json::to_string(&record).unwrap_or_default());
                } else {
                    eprintln!(
                        "{} Search full-text rebuild failed after auto-index: {e}. Run {} to refresh BM25.",
                        "WARN".yellow().bold(),
                        "ledgerful index --incremental".cyan().bold()
                    );
                }
                engine
            }
            Err(e) => return Err(e),
        }
    } else {
        engine
    };

    let post_count = engine.document_count();

    // Empty-index honesty: do not collapse into silent no-matches for agents.
    if pre_count == 0 {
        emit_search_index_status(&args, post_count);
        // Still empty after rebuild: status alone is enough (skip zero-hit noise).
        if post_count == 0 {
            return Ok(());
        }
    }

    perform_search(engine, &layout.root, &args, use_regex, use_hybrid)?;

    Ok(())
}

/// Emit human WARN / JSON `search_index_status` when the index was empty before
/// query (0126). Mirror `semantic_readiness` Insight construction.
fn emit_search_index_status(args: &SearchArgs, post_count: usize) {
    let state = if post_count == 0 {
        "empty_after_rebuild"
    } else {
        "was_empty"
    };

    if args.json {
        let mut content = serde_json::json!({
            "state": state,
            "document_count": post_count,
        });
        if post_count == 0 {
            // B2 honesty: rebuild already ran; do not only say "run index".
            content["remediation"] = serde_json::Value::String(
                "Rebuild already ran; no indexable content found (empty repo, \
                 ignore patterns, or filters). Check ignore patterns. \
                 `ledgerful index` may re-try but will not invent files."
                    .to_string(),
            );
        }
        let record = BridgeRecord {
            bridge_version: BridgeRecord::VERSION.to_string(),
            direction: BridgeDirection::Outbound,
            timestamp: chrono::Utc::now(),
            parent_hash: None,
            project_id: args.project_id.clone(),
            session_id: None,
            tx_id: None,
            record_kind: "search_index_status".to_string(),
            payload: BridgePayload::Insight {
                memory_id: "search_index_status".to_string(),
                relevance: 0.0,
                content: serde_json::to_string(&content).unwrap_or_default(),
            },
            privacy: Privacy::ProjectLocal,
        };
        println!("{}", serde_json::to_string(&record).unwrap_or_default());
    } else if post_count == 0 {
        println!(
            "{} Search index empty after rebuild (0 documents). No indexable \
             content found — check ignore patterns; empty repo or filters may \
             leave the index empty.",
            "WARN".yellow().bold()
        );
    } else {
        println!(
            "{} Search index was empty; rebuilt to {post_count} document(s).",
            "WARN".yellow().bold()
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
    use_regex: bool,
    use_hybrid: bool,
) -> Result<()> {
    // Overfetch by one so human path can emit a truncation affordance without
    // claiming an exact "K more" total (0100 DoD-7). JSON still truncates to
    // `args.limit` with no new fields.
    let overfetch = args.limit.saturating_add(1);

    if use_hybrid {
        if !args.json {
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
            /// Byte ranges into `content` for ungated owo_colors emphasis (human only).
            highlight_ranges: Vec<(usize, usize)>,
            score: Option<f32>,
            is_regex: bool,
        }

        let mut merged: std::collections::HashMap<(String, Option<usize>), MergedResult> =
            std::collections::HashMap::new();

        for r in bm25_results {
            // Seed from plain fragment (not pre-rendered highlighted). Emphasis
            // is applied only on the human path; --json stays plain (DoD-5).
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

        if merged_results.is_empty() {
            handle_fuzzy_fallback(&engine, args);
        } else {
            if args.json {
                for res in merged_results {
                    let record = BridgeRecord {
                        bridge_version: BridgeRecord::VERSION.to_string(),
                        direction: BridgeDirection::Outbound,
                        timestamp: chrono::Utc::now(),
                        parent_hash: None,
                        project_id: args.project_id.clone(),
                        session_id: None,
                        tx_id: None,
                        record_kind: if res.is_regex {
                            "regex_match".to_string()
                        } else {
                            "bm25_match".to_string()
                        },
                        payload: BridgePayload::Insight {
                            memory_id: if let Some(line) = res.line_number {
                                format!("{}::{}", res.path, line)
                            } else {
                                res.path.clone()
                            },
                            relevance: res.score.unwrap_or(1.0) as f64,
                            content: if res.is_regex {
                                format!(
                                    "{}:{}: {}",
                                    res.path,
                                    res.line_number.unwrap_or(0),
                                    res.content
                                )
                            } else {
                                format!("{} ({})", res.path, res.content)
                            },
                        },
                        privacy: Privacy::ProjectLocal,
                    };
                    println!("{}", serde_json::to_string(&record).unwrap_or_default());
                }
            } else {
                println!(
                    "\n{}",
                    "Hybrid Search Results (BM25 + Regex):".bold().cyan()
                );
                for res in merged_results {
                    let line_info = if let Some(line) = res.line_number {
                        format!(":{}", line.to_string().yellow())
                    } else {
                        String::new()
                    };
                    let source_label = if res.is_regex {
                        "[Regex]".magenta().to_string()
                    } else {
                        "[BM25]".green().to_string()
                    };
                    let score_info = if let Some(score) = res.score {
                        format!(" [score: {:.2}]", score)
                    } else {
                        String::new()
                    };
                    // Apply emphasis at print time (ungated owo_colors, like neighbours).
                    // Do NOT assert absence of escapes in human stdout tests — a pipe is
                    // not a TTY and nothing gates colour (spec §2.4).
                    let display = emphasize_snippet(&res.content, &res.highlight_ranges);
                    println!(
                        "{} {}{} {}",
                        source_label,
                        format!("{}{}", res.path.cyan(), line_info).bold(),
                        score_info.yellow(),
                        display.trim()
                    );
                }
                if truncated {
                    print_search_truncation_affordance();
                }
                println!();
            }
        }
    } else if use_regex {
        if !args.json {
            println!("[Search Mode: Regex]");
        }
        let filter = RegexFilter::new(&engine);
        let mut matches = filter.search(root, &args.query, overfetch)?;
        let truncated = matches.len() > args.limit;
        matches.truncate(args.limit);

        if args.json {
            for m in matches {
                let record = BridgeRecord {
                    bridge_version: BridgeRecord::VERSION.to_string(),
                    direction: BridgeDirection::Outbound,
                    timestamp: chrono::Utc::now(),
                    parent_hash: None,
                    project_id: args.project_id.clone(),
                    session_id: None,
                    tx_id: None,
                    record_kind: "regex_match".to_string(),
                    payload: BridgePayload::Insight {
                        memory_id: format!("{}::{}", m.path, m.line_number),
                        relevance: 1.0,
                        content: format!("{}:{}: {}", m.path, m.line_number, m.content),
                    },
                    privacy: Privacy::ProjectLocal,
                };
                println!("{}", serde_json::to_string(&record).unwrap_or_default());
            }
        } else {
            println!("\n{}", "Regex Search Results:".bold().cyan());
            if matches.is_empty() {
                println!("No matches found.");
                println!(
                    "{} Check your regex syntax or run {} if changes are missing.",
                    "HINT".yellow().bold(),
                    "ledgerful index".cyan().bold()
                );
            } else {
                for m in matches {
                    println!(
                        "{}:{}: {}",
                        m.path.cyan(),
                        m.line_number.to_string().yellow(),
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
        if !args.json {
            println!("[Search Mode: BM25]");
        }
        let mut results = engine.search(&args.query, overfetch)?;
        let truncated = results.len() > args.limit;
        results.truncate(args.limit);

        if results.is_empty() {
            handle_fuzzy_fallback(&engine, args);
        } else {
            if args.json {
                for r in results {
                    let record = BridgeRecord {
                        bridge_version: BridgeRecord::VERSION.to_string(),
                        direction: BridgeDirection::Outbound,
                        timestamp: chrono::Utc::now(),
                        parent_hash: None,
                        project_id: args.project_id.clone(),
                        session_id: None,
                        tx_id: None,
                        record_kind: "bm25_match".to_string(),
                        payload: BridgePayload::Insight {
                            memory_id: r.path.clone(),
                            relevance: r.score as f64,
                            content: format!("{} ({})", r.path, r.snippet.unwrap_or_default()),
                        },
                        privacy: Privacy::ProjectLocal,
                    };
                    println!("{}", serde_json::to_string(&record).unwrap_or_default());
                }
            } else {
                println!("\n{}", "Ranked Search Results (BM25):".bold().cyan());
                for r in results {
                    let line_info = if let Some(line) = r.line_number {
                        format!(":{}", line.to_string().yellow())
                    } else {
                        String::new()
                    };
                    println!(
                        "{} [score: {:.2}]",
                        format!("{}{}", r.path.cyan(), line_info).bold(),
                        owo_colors::OwoColorize::yellow(&r.score)
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
    }

    Ok(())
}

/// Human-only truncation affordance (0100 DoD-7). No exact remaining count —
/// engines do not always return a total. JSON paths never call this.
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
        out.push_str(&piece.bold().to_string());
        last = end;
    }
    if last <= fragment.len() && fragment.is_char_boundary(last) {
        out.push_str(&fragment[last..]);
    }
    out
}

fn handle_fuzzy_fallback(engine: &TantivySearchEngine, args: &SearchArgs) {
    if !args.json {
        println!("{}", "Falling back to fuzzy search...".yellow());
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
        if !args.json {
            println!("No matches found.");
            println!(
                "{} No exact symbols found. Try {} or {}.",
                "HINT:".yellow().bold(),
                "--regex".cyan().bold(),
                "ledgerful index".cyan().bold()
            );
            println!(
                "      Alternatively, try semantic search instead: {}",
                format!("ledgerful ask \"{}\"", args.query).cyan()
            );
        }
    } else {
        let truncated = fuzzy_matches.len() > args.limit;
        fuzzy_matches.truncate(args.limit);
        if args.json {
            for m in fuzzy_matches {
                let record = BridgeRecord {
                    bridge_version: BridgeRecord::VERSION.to_string(),
                    direction: BridgeDirection::Outbound,
                    timestamp: chrono::Utc::now(),
                    parent_hash: None,
                    project_id: args.project_id.clone(),
                    session_id: None,
                    tx_id: None,
                    record_kind: "fuzzy_match".to_string(),
                    payload: BridgePayload::Insight {
                        memory_id: format!("{}::{}", m.path, m.line_number.unwrap_or(1)),
                        relevance: m.score as f64,
                        content: format!(
                            "{}:{}: {}",
                            m.path,
                            m.line_number.unwrap_or(1),
                            m.snippet.as_deref().unwrap_or_default()
                        ),
                    },
                    privacy: Privacy::ProjectLocal,
                };
                println!("{}", serde_json::to_string(&record).unwrap_or_default());
            }
        } else {
            println!("\n{}", "Fuzzy Search Results:".bold().cyan());
            for m in fuzzy_matches {
                let line_info = if let Some(line) = m.line_number {
                    format!(":{}", line.to_string().yellow())
                } else {
                    String::new()
                };
                println!(
                    "{} [score: {:.2}]",
                    format!("{}{}", m.path.cyan(), line_info).bold(),
                    owo_colors::OwoColorize::yellow(&m.score)
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
