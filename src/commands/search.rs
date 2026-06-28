use crate::bridge::model::{BridgeDirection, BridgePayload, BridgeRecord, Privacy};
use crate::commands::helpers::get_layout;
use crate::config::load::load_config;
use crate::index::warn_if_stale;
use crate::search::{RegexFilter, StreamIndexer, TantivySearchEngine};
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

    // --- Staleness check (applies to both semantic and BM25 paths) ---
    if !args.index {
        let config = load_config(&layout)?;
        let storage_opt = StorageManager::open_read_only(&layout.root).ok();

        if let Some(storage) = storage_opt {
            let threshold = config.index.stale_threshold_days;
            if args.auto_index {
                crate::index::staleness::try_auto_index(storage, threshold)?;
            } else {
                let is_stale = warn_if_stale(&storage, threshold);
                if is_stale && !args.json && crate::util::term::is_interactive() {
                    use inquire::Confirm;
                    if let Ok(true) =
                        Confirm::new("Index is stale. Would you like to run auto-index now?")
                            .with_default(true)
                            .prompt()
                    {
                        println!("Running auto-indexing...");
                        crate::index::staleness::try_auto_index(storage, threshold)?;
                    }
                }
            }
        }
    }

    if args.semantic {
        let config = load_config(&layout)?;
        let storage = StorageManager::open_read_only(&layout.root)?;
        let cozo = storage
            .cozo
            .as_ref()
            .ok_or_else(|| miette::miette!("CozoDB storage not initialized"))?;

        let semantic_engine =
            crate::semantic::SemanticDiscovery::new(config.local_model.clone(), cozo)?;

        // --- Phase 1: Readiness Check ---
        let mut readiness = semantic_engine.check_readiness()?;
        if readiness.vector_count == 0
            && !args.auto_index
            && !args.json
            && crate::util::term::is_interactive()
        {
            use inquire::Confirm;
            if let Ok(true) = Confirm::new(
                "Semantic index is empty. Would you like to run 'ledgerful index --semantic' now?",
            )
            .with_default(true)
            .prompt()
            {
                println!("Running semantic indexing...");
                crate::commands::index::execute_index(crate::commands::index::IndexArgs {
                    incremental: true,
                    check: false,
                    strict: false,
                    json: false,
                    analyze_graph: false,
                    docs: false,
                    contracts: false,
                    semantic: false,
                    scip: None,
                    auto_scip: false,
                    export_docs: false,

                    doc_type: None,
                    concurrency: None,
                    semantic_dry_run: None,
                    fast: false,
                    repair_metadata: false,
                    dry_run: false,
                    yes: false,
                })?;
                readiness = semantic_engine.check_readiness()?;
            }
        }

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
            if !readiness.endpoint_available {
                println!(
                    "{} Local embedding endpoint unreachable. Check your model server.",
                    "WARN".yellow().bold()
                );
            }
            if readiness.vector_count == 0 {
                println!(
                    "{} Semantic index is empty. Run {} to populate.",
                    "WARN".yellow().bold(),
                    "ledgerful index --semantic".cyan().bold()
                );
            }
            if readiness.dimension_mismatch {
                println!(
                    "{} Model/Index dimension mismatch ({} vs {}). Run {} to fix.",
                    "ERROR".red().bold(),
                    readiness.model_name,
                    readiness.dimensions,
                    "ledgerful update --migrate".cyan().bold()
                );
            }
        }

        debug!("Performing semantic search for: {}", args.query);
        if !args.json {
            println!("[Search Mode: Semantic]");
        }
        let results = semantic_engine.query(&args.query, args.limit)?;

        if !results.is_empty() {
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
                println!();
            }
            return Ok(());
        }

        if !args.json {
            if readiness.vector_count == 0 {
                println!(
                    "{} ⚠️ Semantic index empty. Showing BM25 results. Run 'ledgerful index --semantic' to populate.",
                    "WARN".yellow().bold()
                );
            } else {
                println!(
                    "{} ⚠️ No relevant code snippets found semantically. Showing BM25 results.",
                    "WARN".yellow().bold()
                );
            }
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

    if args.index || engine.document_count() == 0 {
        if !args.json {
            println!("{} Indexing repository for search...", "INIT".cyan().bold());
        }
        debug!("Indexing repository for search...");
        {
            engine.clear()?;
            let indexer = StreamIndexer::new(engine);
            indexer.index_repository(&layout.root)?;
        }

        if !args.json {
            println!("{} Index built successfully.\n", "DONE".green().bold());
        }

        let engine = TantivySearchEngine::open_or_create(index_path.as_std_path())?;
        engine.verify_index_integrity(index_path.as_std_path())?;
        debug!("Tantivy index integrity verified.");

        perform_search(engine, &layout.root, &args, use_regex, use_hybrid)?;
    } else {
        perform_search(engine, &layout.root, &args, use_regex, use_hybrid)?;
    }

    Ok(())
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
    if use_hybrid {
        if !args.json {
            println!("[Search Mode: Hybrid]");
        }
        let filter = RegexFilter::new(&engine);
        let regex_matches = filter
            .search(root, &args.query, args.limit)
            .unwrap_or_default();
        let bm25_results = engine.search(&args.query, args.limit).unwrap_or_default();

        struct MergedResult {
            path: String,
            line_number: Option<usize>,
            content: String,
            score: Option<f32>,
            is_regex: bool,
        }

        let mut merged: std::collections::HashMap<(String, Option<usize>), MergedResult> =
            std::collections::HashMap::new();

        for r in bm25_results {
            merged.insert(
                (r.path.clone(), r.line_number),
                MergedResult {
                    path: r.path,
                    line_number: r.line_number,
                    content: r
                        .highlighted
                        .clone()
                        .or_else(|| r.snippet.clone())
                        .unwrap_or_default(),
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
                    println!(
                        "{} {}{} {}",
                        source_label,
                        format!("{}{}", res.path.cyan(), line_info).bold(),
                        score_info.yellow(),
                        res.content.trim()
                    );
                }
                println!();
            }
        }
    } else if use_regex {
        if !args.json {
            println!("[Search Mode: Regex]");
        }
        let filter = RegexFilter::new(&engine);
        let matches = filter.search(root, &args.query, args.limit)?;

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
            }
            println!();
        }
    } else {
        if !args.json {
            println!("[Search Mode: BM25]");
        }
        let results = engine.search(&args.query, args.limit)?;

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
                    if let Some(snippet) = r.highlighted {
                        println!("  {}", snippet.trim());
                    }
                }
                println!();
            }
        }
    }

    Ok(())
}

fn handle_fuzzy_fallback(engine: &TantivySearchEngine, args: &SearchArgs) {
    if !args.json {
        println!("{}", "Falling back to fuzzy search...".yellow());
    }
    let fuzzy_matches = match engine.search_fuzzy(&args.query, args.limit) {
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
                if let Some(snippet) = m.highlighted {
                    println!("  {}", snippet.trim());
                }
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
