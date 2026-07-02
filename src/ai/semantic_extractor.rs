use crate::config::model::{GeminiConfig, LocalModelConfig};
use crate::local_model::client::{
    ChatMessage, CompletionOptions, complete_with_first_byte_timeout, gemini_complete,
    is_first_byte_timeout_error,
};
use crate::state::storage_cozo::CozoStorage;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// When set, the full prompt and raw LLM response are dumped to this directory
/// for inspection (one file per chunk attempt).
const DEBUG_DUMP_ENV: &str = "LEDGERFUL_DEBUG_SEMANTIC";

#[derive(Debug, Clone)]
pub struct SemanticNode {
    pub id: String,
    pub label: String,
    pub category: String,
    pub source_file: String,
    pub source_location: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SemanticEdge {
    pub source: String,
    pub target: String,
    pub relation: String,
    pub confidence: f64,
}

#[derive(Debug, Clone)]
pub struct ExtractionResult {
    pub nodes: Vec<SemanticNode>,
    pub edges: Vec<SemanticEdge>,
    pub input_tokens: usize,
    pub output_tokens: usize,
}

#[derive(Debug, Clone)]
pub struct SemanticExtractorConfig {
    pub max_tokens_per_chunk: usize,
    pub model_context_window: usize,
    pub overlap_chars: usize,
    pub max_retries: usize,
    pub enable_adaptive_recursion: bool,
    /// When true, use Gemini API instead of the local model for extraction.
    pub fast: bool,
}

impl Default for SemanticExtractorConfig {
    fn default() -> Self {
        Self {
            max_tokens_per_chunk: 24000,
            model_context_window: 8192,
            overlap_chars: 1000,
            max_retries: 3,
            enable_adaptive_recursion: true,
            fast: false,
        }
    }
}

pub struct SemanticExtractor {
    config: SemanticExtractorConfig,
}

#[derive(Debug, Deserialize, Serialize)]
struct LlmNode {
    id: String,
    label: String,
    category: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct LlmEdge {
    source: String,
    target: String,
    relation: String,
    confidence: f64,
}

#[derive(Debug, Deserialize, Serialize)]
struct LlmResponse {
    nodes: Vec<LlmNode>,
    edges: Vec<LlmEdge>,
}

const EXTRACTION_PROMPT: &str = r#"Analyze the following source code and extract semantic nodes and edges.

Return ONLY valid JSON matching this exact schema:

{
  "nodes": [
    {"id": "qualified::name", "label": "brief semantic description", "category": "function_concept|data_model|business_logic|infrastructure|utility"}
  ],
  "edges": [
    {"source": "id1", "target": "id2", "relation": "depends_on|implements|orchestrates|reads_from|calls", "confidence": 0.95}
  ]
}

Categories:
- function_concept: A function, method, or callable concept
- data_model: A struct, enum, type alias, or database schema
- business_logic: Core domain logic, rules, or workflows
- infrastructure: Configuration, build scripts, deployment, or tooling
- utility: Helper functions, formatting, logging, or generic utilities

Relations:
- depends_on: One concept depends on another
- implements: A concept implements an interface or trait
- orchestrates: A concept coordinates or manages other concepts
- reads_from: A concept reads data from another
- calls: A function or method calls another

Source code:
```"#;

impl SemanticExtractor {
    pub fn new(config: SemanticExtractorConfig) -> Self {
        Self { config }
    }

    pub fn extract_from_file(
        &self,
        path: &Path,
        content: &str,
        local_model_config: &LocalModelConfig,
        gemini_config: &GeminiConfig,
    ) -> Result<ExtractionResult, String> {
        // Dynamically adjust based on the model's reported context window
        let max_input_tokens = if local_model_config.context_window >= 64000 {
            24000 // High-fidelity window for large models
        } else if local_model_config.context_window >= 16000 {
            8000
        } else {
            4000
        };

        let max_chars = max_input_tokens * 4;
        let chunks = if content.len() <= max_chars {
            vec![content.to_string()]
        } else {
            chunk_content(content, max_chars, self.config.overlap_chars)
        };

        let mut all_nodes = Vec::new();
        let mut all_edges = Vec::new();
        let mut total_input_tokens = 0;
        let mut total_output_tokens = 0;

        for chunk in chunks {
            let chunk_input_tokens = chunk.chars().count().div_ceil(4);
            total_input_tokens += chunk_input_tokens;

            match self.call_llm(path, &chunk, local_model_config, gemini_config) {
                Ok((partial, output_tokens)) => {
                    total_output_tokens += output_tokens;
                    all_nodes.extend(partial.nodes);
                    all_edges.extend(partial.edges);
                }
                Err(e) => {
                    warn!(
                        "LLM extraction failed for chunk in {}: {}",
                        path.display(),
                        e
                    );
                }
            }
        }

        let (nodes, edges) = deduplicate(all_nodes, all_edges);
        Ok(ExtractionResult {
            nodes,
            edges,
            input_tokens: total_input_tokens,
            output_tokens: total_output_tokens,
        })
    }

    pub fn extract_batch(
        &self,
        files: Vec<(PathBuf, String)>,
        local_model_config: &LocalModelConfig,
        gemini_config: &GeminiConfig,
    ) -> Result<ExtractionResult, String> {
        let mut all_nodes = Vec::new();
        let mut all_edges = Vec::new();
        let mut total_input_tokens = 0;
        let mut total_output_tokens = 0;
        let n = files.len();

        for (i, (path, content)) in files.iter().enumerate() {
            let file_label = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(path.to_str().unwrap_or("<unknown>"));
            let byte_len = content.len();
            let start = std::time::Instant::now();
            info!(
                "Semantic extraction [{}/{}]: processing {} ({} bytes)...",
                i + 1,
                n,
                file_label,
                byte_len,
            );
            let result =
                self.extract_from_file(path, content, local_model_config, gemini_config)?;
            let elapsed = start.elapsed();
            total_input_tokens += result.input_tokens;
            total_output_tokens += result.output_tokens;
            let node_count = result.nodes.len();
            let edge_count = result.edges.len();
            all_nodes.extend(result.nodes);
            all_edges.extend(result.edges);
            info!(
                "Semantic extraction [{}/{}]: completed {} in {:.1}s ({} nodes, {} edges from this file)",
                i + 1,
                n,
                file_label,
                elapsed.as_secs_f64(),
                node_count,
                edge_count,
            );
        }

        let (nodes, edges) = deduplicate(all_nodes, all_edges);
        Ok(ExtractionResult {
            nodes,
            edges,
            input_tokens: total_input_tokens,
            output_tokens: total_output_tokens,
        })
    }

    pub fn ingest_into_cozo(
        result: &ExtractionResult,
        cozo: &CozoStorage,
        provenance_id: &str,
    ) -> miette::Result<()> {
        let mut node_batch = Vec::new();
        for node in &result.nodes {
            let metadata = json!({
                "source_file": node.source_file,
                "source_location": node.source_location
            });
            node_batch.push(json!([
                node.id.clone(),
                node.label.clone(),
                node.category.clone(),
                0.0,
                metadata
            ]));
        }

        if !node_batch.is_empty() {
            let script = "?[id, label, category, risk_score, metadata] <- $batch :put node";
            let mut params = std::collections::BTreeMap::new();
            params.insert(
                "batch".to_string(),
                cozo::DataValue::from(serde_json::Value::Array(node_batch)),
            );
            cozo.run_script_with_params(script, params, cozo::ScriptMutability::Mutable)?;
        }

        let mut edge_batch = Vec::new();
        for edge in &result.edges {
            edge_batch.push(json!([
                edge.source.clone(),
                edge.target.clone(),
                edge.relation.clone(),
                edge.confidence,
                provenance_id
            ]));
        }

        if !edge_batch.is_empty() {
            let script =
                "?[source, target, relation, confidence, provenance_id] <- $batch :put edge";
            let mut params = std::collections::BTreeMap::new();
            params.insert(
                "batch".to_string(),
                cozo::DataValue::from(serde_json::Value::Array(edge_batch)),
            );
            cozo.run_script_with_params(script, params, cozo::ScriptMutability::Mutable)?;
        }

        Ok(())
    }

    fn call_llm(
        &self,
        path: &Path,
        chunk: &str,
        local_model_config: &LocalModelConfig,
        gemini_config: &GeminiConfig,
    ) -> Result<(ExtractionResult, usize), String> {
        let system_msg = ChatMessage {
            role: "system".to_string(),
            content: "You are a semantic code analysis engine that returns only JSON.".to_string(),
        };

        // Derive output budget from the model's actual context window.
        // Reserve ~60% for input+prompts, ~40% for output JSON.
        // The hardcoded default was 8192 — too small for 64K models processing
        // symbol-dense files like model.rs (130+ functions).
        let max_output_tokens = std::cmp::max(
            8192,
            local_model_config.context_window.saturating_mul(2) / 5,
        );
        let options = CompletionOptions {
            max_tokens: max_output_tokens,
            temperature: 0.1,
        };

        // Semantic extraction can require many output tokens at ~20ms/token on GPU,
        // so use a generous read timeout (10 min) to prevent premature cut-off once
        // the model starts streaming. The first-byte timeout below bounds the time
        // we wait for the server to begin the response.
        let effective_timeout = std::cmp::max(600, local_model_config.timeout_secs);
        // Short budget for the server to begin responding; prevents stalling on an
        // accept-then-hang server.
        let first_byte_secs = 15u64;

        let debug_dir = std::env::var(DEBUG_DUMP_ENV).ok();

        let mut last_error = String::new();
        let mut attempt = 0;
        let mut current_chunk = chunk.to_string();

        while attempt < self.config.max_retries {
            attempt += 1;
            let prompt = format!("{}{}\n```", EXTRACTION_PROMPT, current_chunk);

            // Debug dump: write the full prompt before sending
            if let Some(ref dir) = debug_dir {
                let stem = path.file_stem().unwrap_or_default();
                let dump_path = PathBuf::from(dir).join(format!(
                    "{}_attempt{}_prompt.txt",
                    stem.to_string_lossy(),
                    attempt
                ));
                let _ = fs::write(&dump_path, prompt.as_str());
                info!(target: "semantic_debug", "Wrote prompt to {}", dump_path.display());
            }

            let messages = vec![
                system_msg.clone(),
                ChatMessage {
                    role: "user".to_string(),
                    content: prompt,
                },
            ];

            let llm_result = if self.config.fast {
                info!(
                    "Semantic extraction [{}/{}]: using Gemini (fast mode)...",
                    attempt, self.config.max_retries,
                );
                match gemini_complete(gemini_config, &messages, &options) {
                    Ok(res) => Ok(res),
                    Err(e) => {
                        warn!(
                            "Gemini extraction failed: {}. Falling back to local model...",
                            e
                        );
                        complete_with_first_byte_timeout(
                            local_model_config,
                            &messages,
                            &options,
                            Some(effective_timeout),
                            Some(first_byte_secs),
                        )
                    }
                }
            } else {
                complete_with_first_byte_timeout(
                    local_model_config,
                    &messages,
                    &options,
                    Some(effective_timeout),
                    Some(first_byte_secs),
                )
            };

            match llm_result {
                Ok(response) => {
                    // Debug dump: write the raw LLM response
                    if let Some(ref dir) = debug_dir {
                        let stem = path.file_stem().unwrap_or_default();
                        let dump_path = PathBuf::from(dir).join(format!(
                            "{}_attempt{}_response.txt",
                            stem.to_string_lossy(),
                            attempt
                        ));
                        let _ = fs::write(&dump_path, &response);
                        info!(target: "semantic_debug", "Wrote response to {}", dump_path.display());
                    }

                    let output_tokens = response.chars().count().div_ceil(4);

                    // Strip code fences before the truncation check so that
                    // markdown-wrapped valid JSON is not falsely flagged.
                    let cleaned = response.trim();
                    let cleaned = if cleaned.starts_with("```json") {
                        cleaned
                            .trim_start_matches("```json")
                            .trim_end_matches("```")
                            .trim()
                    } else if cleaned.starts_with("```") {
                        cleaned
                            .trim_start_matches("```")
                            .trim_end_matches("```")
                            .trim()
                    } else {
                        cleaned
                    };

                    if self.config.enable_adaptive_recursion
                        && (!cleaned.ends_with('}') && !cleaned.ends_with(']'))
                    {
                        warn!(
                            "LLM response appears truncated ({} bytes received, does not end with '}}' or ']'), retrying with smaller chunk ({} → {})",
                            response.len(),
                            current_chunk.len(),
                            current_chunk.len() / 2,
                        );
                        if current_chunk.len() > 1000 {
                            current_chunk = current_chunk[..current_chunk.len() / 2].to_string();
                            continue;
                        }
                    }
                    match parse_llm_response(&response, path) {
                        Ok((nodes, edges)) => {
                            let partial = ExtractionResult {
                                nodes,
                                edges,
                                input_tokens: current_chunk.chars().count().div_ceil(4),
                                output_tokens,
                            };
                            return Ok((partial, output_tokens));
                        }
                        Err(e) => {
                            warn!("Failed to parse LLM JSON response: {}", e);
                            last_error = e;
                        }
                    }
                }
                Err(e) => {
                    last_error = e.clone();
                    // Fail fast for connection-level or first-byte failures: looping on a
                    // down/hung model would otherwise stall for max_retries * timeout.
                    if is_first_byte_timeout_error(&e)
                        || e.contains("is unreachable")
                        || e.contains("not configured")
                        || e.contains("not reachable")
                    {
                        warn!(
                            "Semantic extraction [{}/{}]: model unavailable ({}); aborting retries",
                            attempt, self.config.max_retries, e
                        );
                        break;
                    }
                    if e.contains("503") || e.contains("rate limited") {
                        std::thread::sleep(std::time::Duration::from_secs(1));
                    }
                }
            }
        }

        Err(format!(
            "LLM extraction failed after {} attempts: {}",
            self.config.max_retries, last_error
        ))
    }
}

fn parse_llm_response(
    response: &str,
    path: &Path,
) -> Result<(Vec<SemanticNode>, Vec<SemanticEdge>), String> {
    let cleaned = response.trim();
    let cleaned = if cleaned.starts_with("```json") {
        cleaned
            .trim_start_matches("```json")
            .trim_end_matches("```")
            .trim()
    } else if cleaned.starts_with("```") {
        cleaned
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim()
    } else {
        cleaned
    };

    let parsed: LlmResponse =
        serde_json::from_str(cleaned).map_err(|e| format!("JSON parse error: {}", e))?;

    let nodes: Vec<SemanticNode> = parsed
        .nodes
        .into_iter()
        .map(|n| SemanticNode {
            id: n.id,
            label: n.label,
            category: n.category,
            source_file: path.to_string_lossy().to_string(),
            source_location: None,
        })
        .collect();

    let edges: Vec<SemanticEdge> = parsed
        .edges
        .into_iter()
        .map(|e| SemanticEdge {
            source: e.source,
            target: e.target,
            relation: e.relation,
            confidence: e.confidence.clamp(0.0, 1.0),
        })
        .collect();

    Ok((nodes, edges))
}

fn chunk_content(content: &str, max_chars: usize, overlap_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < content.len() {
        let end = (start + max_chars).min(content.len());
        let end = content.floor_char_boundary(end);
        let chunk = content[start..end].to_string();
        chunks.push(chunk);
        if end >= content.len() {
            break;
        }
        let next_start = end.saturating_sub(overlap_chars);
        let mut next_start = content.floor_char_boundary(next_start);
        if next_start <= start {
            next_start = content.floor_char_boundary(start + 1);
        }
        if next_start >= content.len() {
            break;
        }
        start = next_start;
    }
    chunks
}

fn deduplicate(
    nodes: Vec<SemanticNode>,
    edges: Vec<SemanticEdge>,
) -> (Vec<SemanticNode>, Vec<SemanticEdge>) {
    let mut seen_nodes: HashSet<String> = HashSet::new();
    let mut deduped_nodes = Vec::new();
    for node in nodes {
        if seen_nodes.insert(node.id.clone()) {
            deduped_nodes.push(node);
        }
    }
    deduped_nodes.sort_by(|a, b| a.id.cmp(&b.id));

    let mut seen_edges: HashSet<(String, String, String)> = HashSet::new();
    let mut deduped_edges = Vec::new();
    for edge in edges {
        let key = (
            edge.source.clone(),
            edge.target.clone(),
            edge.relation.clone(),
        );
        if seen_edges.insert(key) {
            deduped_edges.push(edge);
        }
    }
    deduped_edges.sort_by(|a, b| {
        (&a.source, &a.target, &a.relation).cmp(&(&b.source, &b.target, &b.relation))
    });

    (deduped_nodes, deduped_edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunking_splits_long_content() {
        let content = "A".repeat(100);
        let chunks = chunk_content(&content, 10, 5);
        assert!(
            chunks.len() > 1,
            "Expected multiple chunks, got {}",
            chunks.len()
        );
    }

    #[test]
    fn test_prompt_includes_schema() {
        assert!(EXTRACTION_PROMPT.contains("\"nodes\""));
        assert!(EXTRACTION_PROMPT.contains("\"edges\""));
        assert!(EXTRACTION_PROMPT.contains("\"id\""));
        assert!(EXTRACTION_PROMPT.contains("\"label\""));
        assert!(EXTRACTION_PROMPT.contains("\"category\""));
        assert!(EXTRACTION_PROMPT.contains("\"source\""));
        assert!(EXTRACTION_PROMPT.contains("\"target\""));
        assert!(EXTRACTION_PROMPT.contains("\"relation\""));
        assert!(EXTRACTION_PROMPT.contains("\"confidence\""));
        assert!(EXTRACTION_PROMPT.contains("function_concept"));
        assert!(EXTRACTION_PROMPT.contains("depends_on"));
    }

    #[test]
    fn test_deduplicate_nodes_and_edges() {
        let nodes = vec![
            SemanticNode {
                id: "a".to_string(),
                label: "A".to_string(),
                category: "x".to_string(),
                source_file: "f".to_string(),
                source_location: None,
            },
            SemanticNode {
                id: "a".to_string(),
                label: "A2".to_string(),
                category: "x".to_string(),
                source_file: "f".to_string(),
                source_location: None,
            },
            SemanticNode {
                id: "b".to_string(),
                label: "B".to_string(),
                category: "y".to_string(),
                source_file: "f".to_string(),
                source_location: None,
            },
        ];
        let edges = vec![
            SemanticEdge {
                source: "a".to_string(),
                target: "b".to_string(),
                relation: "calls".to_string(),
                confidence: 0.9,
            },
            SemanticEdge {
                source: "a".to_string(),
                target: "b".to_string(),
                relation: "calls".to_string(),
                confidence: 0.8,
            },
            SemanticEdge {
                source: "b".to_string(),
                target: "c".to_string(),
                relation: "reads".to_string(),
                confidence: 0.7,
            },
        ];
        let (deduped_nodes, deduped_edges) = deduplicate(nodes, edges);
        assert_eq!(deduped_nodes.len(), 2);
        assert_eq!(deduped_edges.len(), 2);
        assert_eq!(deduped_nodes[0].id, "a");
        assert_eq!(deduped_nodes[1].id, "b");
    }

    #[test]
    fn test_ingest_into_cozo() {
        use std::path::PathBuf;
        let cozo = CozoStorage::new(&PathBuf::from("")).unwrap();
        let result = ExtractionResult {
            nodes: vec![
                SemanticNode {
                    id: "node1".to_string(),
                    label: "Node 1".to_string(),
                    category: "function_concept".to_string(),
                    source_file: "test.rs".to_string(),
                    source_location: None,
                },
                SemanticNode {
                    id: "node2".to_string(),
                    label: "Node 2".to_string(),
                    category: "data_model".to_string(),
                    source_file: "test.rs".to_string(),
                    source_location: Some("line 5".to_string()),
                },
            ],
            edges: vec![SemanticEdge {
                source: "node1".to_string(),
                target: "node2".to_string(),
                relation: "calls".to_string(),
                confidence: 0.95,
            }],
            input_tokens: 100,
            output_tokens: 50,
        };
        SemanticExtractor::ingest_into_cozo(&result, &cozo, "tx_test").unwrap();

        let res = cozo.run_script("?[id] := *node{id: id}").unwrap();
        assert_eq!(res.rows.len(), 2);

        let res = cozo
            .run_script("?[source, target] := *edge{source: source, target: target}")
            .unwrap();
        assert_eq!(res.rows.len(), 1);
    }
}
