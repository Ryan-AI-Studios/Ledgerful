//! Impact packet, prune, bridge, and semantic/KG gather for `ask`.
//!
//! Backend validation stays in `execute.rs` (routing/readiness, not gather).

use crate::commands::ask::context::{SemanticGather, WORKING_TREE_NO_PENDING_CHANGES};
use crate::commands::ask::{
    fetch_kg_bm25, fetch_kg_neighborhood, gather_semantic_chunks, should_prune_impact,
};
use crate::config::model::Config;
use crate::impact::packet::ImpactPacket;
use crate::local_model::pruner::{self, RankedChunk};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::Result;
use owo_colors::{OwoColorize, Stream, Style};

/// Outcome of semantic gather for honest KG-fallback notes (0096).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SemanticGatherKind {
    Succeeded,
    Skipped,
    Failed,
}

/// Named gather outputs consumed by `execute_ask` after backend validation.
pub(crate) struct GatherResult {
    pub latest_packet: ImpactPacket,
    pub is_global: bool,
    #[allow(dead_code)]
    pub had_real_packet: bool,
    #[allow(dead_code)]
    pub fresh_packet: bool,
    pub pruned_for_intent: bool,
    /// Live git (or in-memory auto-scan) showed an empty change set.
    /// Honesty for the "no pending changes" prompt/stderr; not `is_global`.
    pub live_tree_clean: bool,
    pub query_string: String,
    pub relevant_chunks: Vec<RankedChunk>,
    pub semantic_gather_kind: SemanticGatherKind,
}

/// Auto-scan / latest packet / prune / QueryIntent / stale-warn / bridge
/// (today `execute.rs` :205–337).
pub(crate) fn gather_impact_and_bridge(
    storage: &StorageManager,
    layout: &Layout,
    config: &Config,
    query: &Option<String>,
    auto_scan: bool,
) -> Result<GatherResult> {
    let auto_scan_effective = auto_scan || config.ask.auto_scan_default;
    let mut live_tree_clean = false;
    let (mut latest_packet, mut is_global, had_real_packet, fresh_packet) =
        match crate::git::status::collect_changed_files_for_filter(layout) {
            Ok(v) if v.is_empty() => {
                live_tree_clean = true;
                (ImpactPacket::default(), true, false, true)
            }
            Ok(_) | Err(_) => {
                if auto_scan_effective {
                    eprintln!(
                        "{}",
                        "Auto-scanning for fresh impact context…"
                            .if_supports_color(Stream::Stderr, |s| s.cyan())
                    );
                    match crate::commands::impact::compute_impact_in_memory(storage, config) {
                        Ok(packet) => {
                            let has_changes = !packet.changes.is_empty();
                            if has_changes {
                                tracing::debug!(
                                    "ask: auto-scan produced fresh impact packet with {} changed files",
                                    packet.changes.len()
                                );
                            } else {
                                live_tree_clean = true;
                                tracing::debug!(
                                    "ask: auto-scan found clean tree — defaulting to global mode"
                                );
                            }
                            (packet, !has_changes, has_changes, true)
                        }
                        Err(e) => {
                            tracing::warn!(
                                "auto-scan failed ({e}); falling back to latest stored impact packet"
                            );
                            match storage.get_latest_packet()? {
                                Some(pkt) => (pkt, false, true, false),
                                None => {
                                    tracing::info!(
                                        "No impact report found — falling back to global knowledge retrieval mode."
                                    );
                                    (ImpactPacket::default(), true, false, false)
                                }
                            }
                        }
                    }
                } else {
                    match storage.get_latest_packet()? {
                        Some(pkt) => (pkt, false, true, false),
                        None => {
                            tracing::info!(
                                "No impact report found — falling back to global knowledge retrieval mode."
                            );
                            (ImpactPacket::default(), true, false, false)
                        }
                    }
                }
            }
        };

    if !is_global && latest_packet.changes.is_empty() {
        tracing::debug!("Latest impact packet is clean (no changes) — defaulting to global mode.");
        is_global = true;
    }

    let query_string = match query {
        Some(q) => q.clone(),
        None => {
            if is_global {
                "Give me an overview of this codebase and its key components.".to_string()
            } else {
                "Analyze the current impact and risk.".to_string()
            }
        }
    };

    let mut pruned_for_intent = false;
    if should_prune_impact(&query_string) {
        if had_real_packet && !latest_packet.changes.is_empty() {
            is_global = true;
            latest_packet = ImpactPacket::default();
            pruned_for_intent = true;
        }
        tracing::debug!("ask: impact context pruned — query classified as GlobalConceptual");
    } else {
        match crate::retrieval::query::classify_query(&query_string) {
            crate::retrieval::query::QueryIntent::DiffTask => {
                tracing::debug!("ask: impact context included — query classified as DiffTask");
            }
            crate::retrieval::query::QueryIntent::Unknown => {
                tracing::debug!(
                    "ask: intent unknown — preserving existing impact context behavior"
                );
            }
            crate::retrieval::query::QueryIntent::GlobalConceptual => {
                // Predicate should have pruned already. Safe default: keep packet.
                tracing::debug!(
                    "ask: GlobalConceptual after prune predicate false — keeping impact"
                );
            }
        }
    }

    if had_real_packet
        && !pruned_for_intent
        && !fresh_packet
        && let Some(reason) = crate::state::reports::warn_if_impact_stale(layout, config)
    {
        eprintln!(
            "{}",
            format!(
                "Warning: {reason} — using it as ask context anyway; results may not reflect the current working tree."
            ).if_supports_color(Stream::Stderr, |s| s.yellow())

        );
    }

    // Integrate external context
    if let Some(q) = query
        && let Ok(bridge_records) = crate::bridge::client::query_unified(q)
    {
        for record in bridge_records {
            if let crate::bridge::model::BridgePayload::Insight {
                memory_id,
                relevance,
                content,
            } = record.payload
            {
                // 0073 / RT-A2+A3: fence + size-cap bridge insights as data
                // (they re-enter ask context via the impact packet user prompt).
                let fenced = crate::ai::fence_bridge_insight(&content);
                latest_packet
                    .ai_insights
                    .push(crate::impact::packet::AiInsight {
                        memory_id,
                        relevance,
                        content: fenced,
                    });
            }
        }
    }

    Ok(GatherResult {
        latest_packet,
        is_global,
        had_real_packet,
        fresh_packet,
        pruned_for_intent,
        live_tree_clean,
        query_string,
        relevant_chunks: Vec::new(),
        semantic_gather_kind: SemanticGatherKind::Skipped,
    })
}

/// Semantic readiness WARN + KG fallback (today `execute.rs` :393–513).
#[allow(clippy::too_many_arguments)]
pub(crate) fn gather_semantic_and_kg(
    gathered: &mut GatherResult,
    storage: &StorageManager,
    layout: &Layout,
    config: &Config,
    semantic: bool,
    auto_index: bool,
    limit: usize,
    no_kg_fallback: bool,
) {
    // 0096 DoD-5: removed interactive `index --semantic` prompt (same defect as
    // search — named semantic index, ran non-semantic incremental; re-prompted
    // forever on empty repos). State-driven warnings replace it.
    if semantic
        && !auto_index
        && let Some(cozo) = storage.cozo()
        && let Ok(semantic_engine) =
            crate::semantic::SemanticDiscovery::new(config.local_model.clone(), cozo)
        && let Ok(readiness) = semantic_engine.check_readiness()
    {
        for msg in crate::semantic::semantic_readiness_messages(&readiness) {
            eprintln!(
                "{} {}",
                "WARN".if_supports_color(Stream::Stderr, |s| s.style(Style::new().yellow().bold())),
                msg
            );
        }
    }

    if gathered.pruned_for_intent {
        eprintln!(
            "{}",
            "[Global Mode] Conceptual query — querying the full Knowledge Graph (active diff context pruned for intent).".if_supports_color(Stream::Stderr, |s| s.cyan())

        );
    } else if gathered.live_tree_clean && !gathered.pruned_for_intent {
        eprintln!(
            "{}",
            format!(
                "[Global Mode] {WORKING_TREE_NO_PENDING_CHANGES} — querying the full Knowledge Graph for context."
            )
            .if_supports_color(Stream::Stderr, |s| s.cyan())
        );
    }

    // DoD-4/8: never treat embed/query Err as "no semantic matches".
    // Track gather kind (without holding chunks) for honest KG-fallback notes.
    let (mut relevant_chunks, semantic_gather_kind) = match gather_semantic_chunks(
        storage,
        layout.root.as_std_path(),
        &gathered.query_string,
        limit,
        &config.local_model,
        gathered.is_global,
    ) {
        SemanticGather::Chunks(chunks) => (chunks, SemanticGatherKind::Succeeded),
        SemanticGather::Skipped { reason } => {
            tracing::warn!("Semantic context skipped: {reason}");
            // Readiness messages already cover NotConfigured; keep a debug trail only.
            (Vec::new(), SemanticGatherKind::Skipped)
        }
        SemanticGather::Failed { reason } => {
            tracing::warn!("Semantic context failed: {reason}");
            eprintln!(
                "{} Semantic search failed (continuing with non-semantic context): {}",
                "WARN".if_supports_color(Stream::Stderr, |s| s.style(Style::new().yellow().bold())),
                reason
            );
            (Vec::new(), SemanticGatherKind::Failed)
        }
    };

    if relevant_chunks.is_empty() {
        relevant_chunks = pruner::query_relevant_chunks(
            &gathered.query_string,
            &config.local_model,
            storage.get_connection(),
            limit,
            config.local_model.chunk_min_similarity,
            config.local_model.chunk_dedup_threshold,
        )
        .unwrap_or_else(|e| {
            tracing::warn!("Chunk retrieval failed: {e}, proceeding without chunks");
            Vec::new()
        });

        // KG Fallback logic — wording must not claim "index empty" on failure/skip.
        if gathered.is_global
            && relevant_chunks.is_empty()
            && !no_kg_fallback
            && let Some(cozo) = storage.cozo()
            && let Some(kg_bm25_context) = fetch_kg_bm25(cozo, &gathered.query_string, limit)
        {
            let note = match semantic_gather_kind {
                SemanticGatherKind::Failed => {
                    "Note: semantic search failed — using KG text search for context"
                }
                SemanticGatherKind::Skipped => {
                    "Note: semantic search did not run — using KG text search for context"
                }
                SemanticGatherKind::Succeeded => {
                    "Note: semantic index empty — using KG text search for context"
                }
            };
            eprintln!("{}", note.if_supports_color(Stream::Stderr, |s| s.yellow()));
            relevant_chunks.push(pruner::RankedChunk {
                source: "Knowledge Graph (BM25)".to_string(),
                content: kg_bm25_context,
                score: 1.0,
            });
        }

        // CR7: Apply KG neighborhood to pruner fallback chunks as well.
        if gathered.is_global
            && !relevant_chunks.is_empty()
            && let Some(cozo) = storage.cozo()
        {
            let syms = relevant_chunks.iter().filter_map(|c| {
                let path = std::path::Path::new(&c.source);
                path.file_stem()?.to_str()
            });
            if let Some(kg_ctx) = fetch_kg_neighborhood(cozo, syms) {
                relevant_chunks.push(pruner::RankedChunk {
                    source: "Knowledge Graph".to_string(),
                    content: kg_ctx,
                    score: 1.0,
                });
            }
        }
    }

    gathered.relevant_chunks = relevant_chunks;
    gathered.semantic_gather_kind = semantic_gather_kind;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::ask::context::{WORKING_TREE_NO_PENDING_CHANGES, build_ask_user_prompt};
    use crate::impact::packet::ChangedFile;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::tempdir;

    fn git(cwd: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git command")
    }

    fn init_repo_with_commit(dir: &Path) {
        assert!(git(dir, &["init", "-b", "main"]).status.success());
        assert!(
            git(dir, &["config", "user.email", "test@example.com"])
                .status
                .success()
        );
        assert!(git(dir, &["config", "user.name", "test"]).status.success());
        fs::write(dir.join("tracked.txt"), "hello\n").expect("write tracked");
        assert!(git(dir, &["add", "."]).status.success());
        assert!(git(dir, &["commit", "-m", "init"]).status.success());
    }

    fn dirty_cached_packet() -> ImpactPacket {
        ImpactPacket {
            tree_clean: false,
            head_hash: Some("deadbeef".to_string()),
            branch_name: Some("old-branch".to_string()),
            changes: vec![ChangedFile {
                path: std::path::PathBuf::from("src/old.rs"),
                status: "Modified".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn gather_with_saved_packet(
        dir: &Path,
        packet: Option<&ImpactPacket>,
        query: &str,
        auto_scan: bool,
    ) -> GatherResult {
        let root = camino::Utf8Path::from_path(dir).expect("utf8 path");
        let layout = Layout::new(root);
        layout.ensure_state_dir().expect("state dir");
        let db_path = layout.state_subdir().join("ledger.db");
        let storage = StorageManager::init(db_path.as_std_path()).expect("storage init");
        if let Some(pkt) = packet {
            storage.save_packet(pkt).expect("save_packet");
        }
        let config = Config::default();
        let gathered = gather_impact_and_bridge(
            &storage,
            &layout,
            &config,
            &Some(query.to_string()),
            auto_scan,
        )
        .expect("gather");
        storage.shutdown().expect("shutdown");
        gathered
    }

    #[test]
    fn gather_live_tree_clean_wall_overrides_dirty_cached_packet() {
        let dir = tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());
        let gathered = gather_with_saved_packet(
            dir.path(),
            Some(&dirty_cached_packet()),
            "what is change-context",
            false,
        );
        assert!(gathered.is_global);
        assert!(gathered.live_tree_clean);
        assert!(gathered.latest_packet.changes.is_empty());
        assert!(!gathered.pruned_for_intent);

        let gathered_auto = gather_with_saved_packet(
            dir.path(),
            Some(&dirty_cached_packet()),
            "what is change-context",
            true,
        );
        assert!(gathered_auto.live_tree_clean);
        assert!(gathered_auto.is_global);
        assert!(gathered_auto.latest_packet.changes.is_empty());
    }

    #[test]
    fn gather_watch_ignored_target_dirt_is_live_clean() {
        let dir = tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());
        let target = dir.path().join("target");
        fs::create_dir_all(&target).expect("mkdir target");
        fs::write(target.join("artifact.o"), "obj\n").expect("write target artifact");
        let gathered = gather_with_saved_packet(
            dir.path(),
            Some(&dirty_cached_packet()),
            "what is change-context",
            false,
        );
        assert!(
            gathered.live_tree_clean,
            "watch-ignored target/ dirt must still take the live-clean wall"
        );
        assert!(gathered.is_global);
        assert!(gathered.latest_packet.changes.is_empty());
        assert!(!gathered.pruned_for_intent);
    }

    #[test]
    fn gather_git_err_does_not_take_live_clean_wall() {
        let dir = tempdir().expect("tempdir");
        let gathered = gather_with_saved_packet(
            dir.path(),
            Some(&dirty_cached_packet()),
            "what is change-context",
            false,
        );
        assert!(
            !gathered.live_tree_clean,
            "git discovery Err must not take the live-clean wall"
        );
        assert!(!gathered.latest_packet.changes.is_empty());
        assert!(!gathered.is_global);
    }

    #[test]
    fn gather_dirty_tree_empty_snapshot_is_not_live_clean() {
        let dir = tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());
        fs::write(dir.path().join("tracked.txt"), "dirty\n").expect("dirty write");
        let gathered = gather_with_saved_packet(dir.path(), None, "what is change-context", false);
        assert!(
            !gathered.live_tree_clean,
            "dirty tree + empty/missing snapshot must not claim live-clean"
        );
        let prompt = build_ask_user_prompt(
            &gathered.query_string,
            gathered.is_global,
            false,
            &gathered.latest_packet,
            gathered.live_tree_clean,
        );
        assert!(
            !prompt.contains(WORKING_TREE_NO_PENDING_CHANGES),
            "dirty-empty-snapshot prompt must not include the no-pending constant: {prompt}"
        );
    }

    #[test]
    fn gather_dirty_tree_loads_cached_packet() {
        let dir = tempdir().expect("tempdir");
        init_repo_with_commit(dir.path());
        fs::write(dir.path().join("tracked.txt"), "dirty\n").expect("dirty write");
        let gathered = gather_with_saved_packet(
            dir.path(),
            Some(&dirty_cached_packet()),
            "what is change-context",
            false,
        );
        assert!(!gathered.live_tree_clean);
        assert!(!gathered.is_global);
        assert_eq!(gathered.latest_packet.changes.len(), 1);
        assert!(!gathered.pruned_for_intent);
    }
}
