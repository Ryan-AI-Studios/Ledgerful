//! Codebase search command (`ledgerful search`).
//!
//! Machine output (0136):
//! - `--json` → single agent envelope (`schemaVersion: 1`)
//! - `--json-lines` → legacy NDJSON BridgeRecord stream

mod envelope;
mod retrieve;
mod trigrams;

pub use envelope::{
    HitEmit, SearchCollector, SearchEnvelope, SearchHit, SearchIndexStatus, SearchJsonMode,
    SearchSemantic,
};
pub use retrieve::{is_identifier_likely, is_regex_likely};
pub use trigrams::execute_search_trigrams;

use retrieve::{perform_search, print_search_truncation_affordance};

use crate::commands::helpers::get_layout;
use crate::config::load::load_config;
use crate::index::staleness::AutoIndexAction;
use crate::index::warn_if_stale;
use crate::search::{TantivySearchEngine, needs_format_rebuild, rebuild_tantivy_index};
use crate::state::storage::StorageManager;
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
            .cozo()
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
        let (mut results, query_succeeded) = match semantic_engine.query(
            layout.root.as_std_path(),
            &args.query,
            semantic_fetch,
        ) {
            Ok((r, filtered_foreign)) => {
                if filtered_foreign > 0 {
                    collector.set_filtered_foreign_count(filtered_foreign);
                    debug!(
                        "Semantic query filtered {filtered_foreign} foreign path hit(s) outside work root"
                    );
                }
                (r, true)
            }
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

    // 0141: format stamp (tokenizer dual-emit) forces rebuild even when schema
    // is unchanged and docs are present. Stamp check before empty-only path.
    let stamp_rebuild = needs_format_rebuild(index_path.as_std_path());

    // 0128: full FTS rebuild when auto-index ran SQLite Full/Incremental work
    // (already done above if successful), or explicit --index / empty docs /
    // format stamp mismatch. Single rebuild path (no double rebuild).
    // Never rebuild on every search when AutoIndexAction::None, docs present,
    // and stamp matches.
    let needs_fts_rebuild = args.index
        || pre_count == 0
        || (auto_index_ran_work && !fts_rebuilt_for_auto_index)
        || stamp_rebuild;

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
                // write_stamp runs inside rebuild_tantivy_index on success.
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
            // Soft concurrency (0141): stamp-driven rebuild may fail under worktree
            // writer contention. Never trust the pre-rebuild reader after failure —
            // rebuild clears the index first, so re-open from disk and only soft-
            // continue when documents remain. Empty/corrupt post-failure → hard err.
            Err(e) if stamp_rebuild && !args.index && pre_count > 0 => {
                match TantivySearchEngine::open_or_create(index_path.as_std_path()) {
                    Ok(reopened) if reopened.document_count() > 0 => {
                        if !args.is_machine() {
                            eprintln!(
                                "{} Search format-stamp rebuild failed (using on-disk index with {} docs): {e}",
                                "WARN".if_supports_color(Stream::Stderr, |s| s
                                    .style(Style::new().yellow().bold())),
                                reopened.document_count()
                            );
                        }
                        reopened
                    }
                    Ok(_) => {
                        // Cleared or empty after failed rebuild — do not pretend success.
                        return Err(e);
                    }
                    Err(_reopen_err) => return Err(e),
                }
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
    // Envelope has a single searchIndexStatus slot — do not overwrite a more
    // specific fts_rebuild_failed signal (0136 codex P2). Lines mode still
    // emits both BridgeRecords (legacy multi-line honesty).
    if pre_count == 0 {
        let skip_empty_status = args.json_mode.is_envelope() && collector.has_search_index_status();
        if !skip_empty_status {
            emit_search_index_status(&args, &mut collector, post_count);
        }
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
