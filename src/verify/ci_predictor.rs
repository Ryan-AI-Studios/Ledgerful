use crate::config::model::LocalModelConfig;
use crate::embed::client::embed_long_text;
use crate::embed::embed_and_store;
use crate::verify::semantic_predictor::TestStatus;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::warn;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CIJobOutcome {
    pub job_name: String,
    pub platform: String,
    pub ci_file_path: String,
    pub commit_hash: String,
    pub status: TestStatus,
    pub duration_ms: u64,
}

pub fn record_ci_outcomes(
    conn: &Connection,
    embed_config: &LocalModelConfig,
    outcomes: &[CIJobOutcome],
    diff_text: &str,
) -> Result<usize, String> {
    if embed_config.base_url.is_empty() {
        warn!("CI prediction: base_url is empty; skipping outcome recording");
        return Ok(0);
    }

    if diff_text.is_empty() {
        warn!("CI prediction: diff_text is empty; skipping outcome recording");
        return Ok(0);
    }

    if outcomes.is_empty() {
        return Ok(0);
    }

    let commit_hash = &outcomes[0].commit_hash;

    embed_and_store(embed_config, conn, "ci_diff", commit_hash, diff_text)?;

    let embedding_id: i64 = conn
        .query_row(
            "SELECT id FROM embeddings WHERE entity_type = 'ci_diff' AND entity_id = ?1 AND model_name = ?2",
            rusqlite::params![commit_hash, embed_config.embedding_model],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;

    for outcome in outcomes {
        // Get ci_file_id from project_files
        let ci_file_id: i64 = conn
            .query_row(
                "SELECT id FROM project_files WHERE file_path = ?1",
                rusqlite::params![outcome.ci_file_path],
                |row| row.get(0),
            )
            .map_err(|e| {
                format!(
                    "CI file not found in project_files: {}. Error: {}",
                    outcome.ci_file_path, e
                )
            })?;

        conn.execute(
            "INSERT INTO ci_outcome_history (diff_embedding_id, ci_file_id, job_name, platform, outcome, commit_hash) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                embedding_id,
                ci_file_id,
                outcome.job_name,
                outcome.platform,
                outcome.status.as_str(),
                outcome.commit_hash
            ],
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(outcomes.len())
}

/// Load diff embeddings for CI.
fn load_ci_diff_embeddings(
    conn: &Connection,
    model_name: &str,
) -> Result<Vec<(i64, String, Vec<f32>)>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, entity_id, vector FROM embeddings WHERE entity_type = 'ci_diff' AND model_name = ?1",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map(rusqlite::params![model_name], |row| {
            let id: i64 = row.get(0)?;
            let entity_id: String = row.get(1)?;
            let blob: Vec<u8> = row.get(2)?;
            Ok((id, entity_id, blob))
        })
        .map_err(|e| e.to_string())?;

    let mut results = Vec::new();
    for row in rows {
        let (id, entity_id, blob) = row.map_err(|e| e.to_string())?;
        if blob.len() % 4 != 0 {
            continue;
        }
        let floats: Vec<f32> = blob
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();
        results.push((id, entity_id, floats));
    }

    Ok(results)
}

pub fn query_similar_ci_outcomes(
    conn: &Connection,
    embed_config: &LocalModelConfig,
    diff_text: &str,
    top_k: usize,
) -> Result<Vec<(CIJobOutcome, f32)>, String> {
    if embed_config.base_url.is_empty() {
        return Ok(Vec::new());
    }

    if diff_text.is_empty() {
        return Ok(Vec::new());
    }

    let diff_embeddings = load_ci_diff_embeddings(conn, &embed_config.embedding_model)?;

    if diff_embeddings.is_empty() {
        return Ok(Vec::new());
    }

    let query_vec = embed_long_text(embed_config, diff_text)?;

    let mut scored: Vec<(i64, f32)> = diff_embeddings
        .iter()
        .filter_map(|(id, _entity_id, vec)| {
            cosine_sim(&query_vec, vec).ok().map(|score| (*id, score))
        })
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    if top_k < scored.len() {
        scored.truncate(top_k);
    }

    let mut results = Vec::new();
    for (embedding_id, similarity) in &scored {
        let mut stmt = conn
            .prepare(
                "SELECT pf.file_path, h.job_name, h.platform, h.outcome, h.commit_hash \
                 FROM ci_outcome_history h \
                 JOIN project_files pf ON h.ci_file_id = pf.id \
                 WHERE h.diff_embedding_id = ?1",
            )
            .map_err(|e| e.to_string())?;

        let outcome_rows = stmt
            .query_map(rusqlite::params![embedding_id], |row| {
                let ci_file_path: String = row.get(0)?;
                let job_name: String = row.get(1)?;
                let platform: String = row.get(2)?;
                let outcome: String = row.get(3)?;
                let commit_hash: String = row.get(4)?;
                Ok((ci_file_path, job_name, platform, outcome, commit_hash))
            })
            .map_err(|e| e.to_string())?;

        for outcome_row in outcome_rows {
            let (ci_file_path, job_name, platform, outcome_str, commit_hash) =
                outcome_row.map_err(|e| e.to_string())?;
            let status = match outcome_str.as_str() {
                "pass" => TestStatus::Passed,
                "fail" => TestStatus::Failed,
                _ => TestStatus::Skipped,
            };
            results.push((
                CIJobOutcome {
                    job_name,
                    platform,
                    ci_file_path,
                    commit_hash,
                    status,
                    duration_ms: 0,
                },
                *similarity,
            ));
        }
    }

    Ok(results)
}

fn cosine_sim(a: &[f32], b: &[f32]) -> Result<f32, String> {
    if a.len() != b.len() {
        return Err("Vectors have different lengths".to_string());
    }
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return Ok(0.0);
    }
    Ok(dot / (norm_a.sqrt() * norm_b.sqrt()))
}

pub fn compute_ci_failure_scores(similar_outcomes: &[(CIJobOutcome, f32)]) -> HashMap<String, f64> {
    let mut job_scores: HashMap<String, (f64, usize)> = HashMap::new();

    for (outcome, sim) in similar_outcomes {
        if outcome.status == TestStatus::Failed {
            let entry = job_scores
                .entry(outcome.job_name.clone())
                .or_insert((0.0, 0));
            entry.0 += *sim as f64;
            entry.1 += 1;
        }
    }

    job_scores
        .into_iter()
        .map(|(job, (sum, count))| (job, sum / count as f64))
        .collect()
}
