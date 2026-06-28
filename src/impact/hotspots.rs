use crate::impact::packet::Hotspot;
use crate::impact::temporal::HistoryProvider;
use crate::state::storage::StorageManager;
use camino::Utf8PathBuf;
use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub fn normalize_score(raw_score: f64) -> f64 {
    (raw_score * 1000.0).ln_1p()
}

/// Human-readable classification of a hotspot based on its normalized
/// complexity and frequency components.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HotspotInterpretation {
    MaintenanceRisk,
    ActiveChurn,
    StableHotspot,
    LowRisk,
}

/// Score decomposition returned by the hotspot engine so callers can explain
/// the score without duplicating the math.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HotspotScoreBreakdown {
    pub complexity: u32,
    pub frequency_weight: f64,
    pub max_complexity: u32,
    pub max_frequency: f64,
    pub normalized_complexity: f64,
    pub normalized_frequency: f64,
    pub temporal_couplings: u32,
    pub base_score: f64,
    pub coupling_adjustment: f64,
    pub final_score: f64,
    pub interpretation: HotspotInterpretation,
}

/// Compute a score breakdown from already-calculated hotspot results, avoiding
/// a redundant `calculate_hotspots` call. The caller must have used a query
/// with a sufficiently large `limit` (or no `exact_file` filter) so the
/// normalization context (max_freq, max_comp) reflects the full file set.
pub fn compute_hotspot_score_breakdown_from_hotspots(
    hotspots: &[Hotspot],
    entity: &str,
    temporal_couplings: usize,
) -> Option<HotspotScoreBreakdown> {
    let entity_normalized = entity.replace('\\', "/");
    let hotspot = hotspots.iter().find(|h| {
        let lossy = h.path.to_string_lossy();
        lossy == entity || lossy.replace('\\', "/") == entity_normalized
    })?;

    let max_freq = hotspots
        .iter()
        .map(|h| h.frequency)
        .fold(f64::MIN, f64::max)
        .max(1.0);
    let max_comp = hotspots
        .iter()
        .map(|h| h.complexity)
        .max()
        .unwrap_or(0)
        .max(1) as f64;

    let f_norm = hotspot.frequency / max_freq;
    let c_norm = hotspot.complexity as f64 / max_comp;
    let base_score = f_norm * c_norm;
    let display_score = normalize_score(base_score);

    let interpretation = classify_hotspot(c_norm as f32, f_norm as f32);

    Some(HotspotScoreBreakdown {
        complexity: hotspot.complexity as u32,
        frequency_weight: hotspot.frequency,
        max_complexity: max_comp as u32,
        max_frequency: max_freq,
        normalized_complexity: c_norm,
        normalized_frequency: f_norm,
        temporal_couplings: temporal_couplings as u32,
        base_score,
        coupling_adjustment: 0.0,
        final_score: display_score,
        interpretation,
    })
}

fn classify_hotspot(c_norm: f32, f_norm: f32) -> HotspotInterpretation {
    let complexity_high = c_norm > 0.5;
    let frequency_high = f_norm > 0.5;
    match (complexity_high, frequency_high) {
        (true, false) => HotspotInterpretation::MaintenanceRisk,
        (false, true) => HotspotInterpretation::ActiveChurn,
        (true, true) => HotspotInterpretation::StableHotspot,
        (false, false) => HotspotInterpretation::LowRisk,
    }
}

#[derive(Debug, Clone, Default)]
pub struct HotspotQuery {
    pub commits: usize,
    pub days: Option<u64>,
    pub since_commit: Option<String>,
    pub limit: usize,
    pub all_parents: bool,
    pub decay_half_life: usize,
    pub dir_filter: Option<String>,
    pub lang_filter: Option<String>,
    pub exact_file: Option<String>,
    pub centrality: bool,
}

pub fn calculate_hotspots(
    storage: &StorageManager,
    history_provider: &dyn HistoryProvider,
    query: &HotspotQuery,
) -> Result<Vec<Hotspot>> {
    let history = history_provider
        .get_history(
            query.commits,
            query.days,
            query.since_commit.clone(),
            query.all_parents,
        )
        .map_err(|e| miette::miette!("Git history error: {e}"))?;

    let mut frequency_map: HashMap<Utf8PathBuf, f64> = HashMap::new();
    let mut total_eligible_commits = 0;

    let half_life = query.decay_half_life as f64;

    for (idx, commit_set) in history.iter().enumerate() {
        if commit_set.is_merge || commit_set.files.is_empty() {
            continue;
        }
        total_eligible_commits += 1;

        // Exponential decay: most recent commit (idx 0) gets weight 1.0
        let weight = if half_life > 0.0 {
            (2.0_f64).powf(-(idx as f64) / half_life)
        } else {
            1.0
        };

        for file in &commit_set.files {
            // Apply filtering during crawl
            let path_str = file.as_str();

            if query.exact_file.as_ref().is_some_and(|f| path_str != f) {
                continue;
            }

            if query
                .dir_filter
                .as_ref()
                .is_some_and(|dir| !path_str.starts_with(dir))
            {
                continue;
            }

            if query
                .lang_filter
                .as_ref()
                .is_some_and(|lang| !path_str.ends_with(&format!(".{lang}")))
            {
                continue;
            }

            *frequency_map.entry(file.clone()).or_default() += weight;
        }
    }

    if total_eligible_commits == 0 {
        return Ok(Vec::new());
    }

    let file_paths: Vec<String> = frequency_map.keys().map(|p| p.to_string()).collect();
    let file_complexities = query_file_complexities(storage, &file_paths)?;

    let mut hotspots = Vec::new();

    // Find max frequency for normalization
    let max_freq = frequency_map
        .values()
        .cloned()
        .fold(f64::MIN, f64::max)
        .max(1.0) as f32;
    // Find max complexity for normalization; clamp to 1 to avoid division by zero when all files have 0 complexity
    let max_comp = file_complexities
        .values()
        .max()
        .cloned()
        .unwrap_or(0)
        .max(1) as f32;

    for (path, freq) in frequency_map {
        let path_str = path.to_string();
        let complexity = file_complexities.get(&path_str).cloned().unwrap_or(0);

        // Scoring:
        // Normalized Frequency (0-1) * Normalized Complexity (0-1)
        // Multiplication surfaces the "worst of both worlds" more effectively than addition.
        let f_norm = freq as f32 / max_freq;
        let c_norm = complexity as f32 / max_comp;

        let score = f_norm * c_norm;

        let display_score = normalize_score(score as f64) as f32;
        hotspots.push(Hotspot {
            path: path.into(),
            score,
            display_score,
            complexity,
            frequency: freq,
            centrality: None,
        });
    }

    if query.centrality {
        crate::index::centrality::enrich_hotspots_with_centrality(&mut hotspots, storage)?;
    }

    // Deterministic sorting: Score (desc) then Path (asc)
    hotspots.sort_unstable_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });
    hotspots.truncate(query.limit);

    Ok(hotspots)
}

pub(crate) fn query_file_complexities(
    storage: &StorageManager,
    file_paths: &[String],
) -> Result<HashMap<String, i32>> {
    let mut file_complexities = HashMap::new();
    let conn = storage.get_connection();

    // 1. Primary Lookup: symbols table (impact-time data)
    if storage.table_exists("symbols")? {
        for chunk in file_paths.chunks(999) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let query = format!(
                "SELECT file_path, MAX(IFNULL(cognitive_complexity, 0), IFNULL(cyclomatic_complexity, 0)) as max_comp
                 FROM symbols
                 WHERE file_path IN ({})
                 GROUP BY file_path",
                placeholders
            );

            let mut stmt = conn.prepare(&query).into_diagnostic()?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(chunk), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)?))
                })
                .into_diagnostic()?;

            for row in rows {
                let (path, comp) = row.into_diagnostic()?;
                file_complexities.insert(path, comp);
            }
        }
    }

    // 2. Identify gaps (missing from primary)
    // NOTE: Primary data takes precedence. If a file is in the symbols table but has 0 complexity,
    // we respect that measurement and do NOT fall back to background index data.
    let gaps: Vec<String> = file_paths
        .iter()
        .filter(|path| !file_complexities.contains_key(*path))
        .cloned()
        .collect();

    if gaps.is_empty() {
        return Ok(file_complexities);
    }

    // 3. Fallback: project_symbols (background index data)
    // We must ensure BOTH project_symbols and project_files exist before joining.
    if !storage.table_exists("project_symbols")? || !storage.table_exists("project_files")? {
        tracing::debug!("Background index tables not available, skipping fallback");
        return Ok(file_complexities);
    }

    // Batch query gaps from project_symbols.
    // Formula: MAX(cognitive_complexity, cyclomatic_complexity) to match primary formula.
    for chunk in gaps.chunks(999) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let query = format!(
            "SELECT pf.file_path, MAX(IFNULL(ps.cognitive_complexity, 0), IFNULL(ps.cyclomatic_complexity, 0)) as max_comp
             FROM project_symbols ps
             JOIN project_files pf ON ps.file_id = pf.id
             WHERE pf.file_path IN ({})
             GROUP BY pf.file_path",
            placeholders
        );

        let mut stmt = conn.prepare(&query).into_diagnostic()?;
        let fallback_rows = stmt
            .query_map(rusqlite::params_from_iter(chunk), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)?))
            })
            .into_diagnostic()?;

        for row in fallback_rows {
            let (path, comp) = row.into_diagnostic()?;
            if comp > 0 {
                file_complexities.insert(path, comp);
            }
        }
    }

    Ok(file_complexities)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_scale_zero() {
        assert_eq!(normalize_score(0.0), 0.0);
    }

    #[test]
    fn log_scale_positive() {
        assert!(normalize_score(1.0) > 0.0);
    }

    #[test]
    fn log_scale_compresses_outlier() {
        let high = normalize_score(0.135);
        let low = normalize_score(0.006);
        // The raw ratio is 22.5x; after log normalization should be < 5x
        assert!(
            high / low < 5.0,
            "expected log normalization to compress 22x gap; got ratio {:.2}",
            high / low
        );
    }

    #[test]
    fn sort_order_preserved() {
        // ln_1p is monotonic — sort by raw == sort by display
        let scores = vec![0.135_f64, 0.006, 0.05, 0.0];
        let mut by_raw = scores.clone();
        by_raw.sort_by(|a, b| b.partial_cmp(a).unwrap());
        let mut by_display = scores.clone();
        by_display.sort_by(|a, b| {
            normalize_score(*b)
                .partial_cmp(&normalize_score(*a))
                .unwrap()
        });
        assert_eq!(by_raw, by_display, "sort order must be identical");
    }

    #[test]
    fn test_hotspots_uses_symbols_when_available() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE symbols (file_path TEXT, cognitive_complexity INTEGER, cyclomatic_complexity INTEGER)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO symbols (file_path, cognitive_complexity, cyclomatic_complexity) VALUES ('a.rs', 5, 3)",
            [],
        ).unwrap();

        let storage = StorageManager::init_from_conn(conn);
        let result = query_file_complexities(&storage, &["a.rs".to_string()]).unwrap();
        assert_eq!(result.get("a.rs"), Some(&5));
    }

    #[test]
    fn test_hotspots_falls_back_to_project_symbols() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE symbols (file_path TEXT, cognitive_complexity INTEGER, cyclomatic_complexity INTEGER)",
            [],
        ).unwrap();
        conn.execute(
            "CREATE TABLE project_files (id INTEGER PRIMARY KEY, file_path TEXT, parse_status TEXT, last_indexed_at TEXT)",
            [],
        ).unwrap();
        conn.execute(
            "CREATE TABLE project_symbols (file_id INTEGER, cognitive_complexity INTEGER, cyclomatic_complexity INTEGER)",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO project_files (id, file_path) VALUES (1, 'b.rs')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project_symbols (file_id, cognitive_complexity) VALUES (1, 10)",
            [],
        )
        .unwrap();

        let storage = StorageManager::init_from_conn(conn);
        let result = query_file_complexities(&storage, &["b.rs".to_string()]).unwrap();
        assert_eq!(result.get("b.rs"), Some(&10));
    }

    #[test]
    fn test_hotspots_prefers_symbols_over_project_symbols() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE symbols (file_path TEXT, cognitive_complexity INTEGER, cyclomatic_complexity INTEGER)",
            [],
        ).unwrap();
        conn.execute(
            "CREATE TABLE project_files (id INTEGER PRIMARY KEY, file_path TEXT, parse_status TEXT, last_indexed_at TEXT)",
            [],
        ).unwrap();
        conn.execute(
            "CREATE TABLE project_symbols (file_id INTEGER, cognitive_complexity INTEGER, cyclomatic_complexity INTEGER)",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO symbols (file_path, cognitive_complexity) VALUES ('a.rs', 5)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project_files (id, file_path) VALUES (1, 'a.rs')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project_symbols (file_id, cognitive_complexity) VALUES (1, 10)",
            [],
        )
        .unwrap();

        let storage = StorageManager::init_from_conn(conn);
        let result = query_file_complexities(&storage, &["a.rs".to_string()]).unwrap();
        assert_eq!(result.get("a.rs"), Some(&5));
    }

    #[test]
    fn test_hotspots_primary_zero_wins_precedence() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE symbols (file_path TEXT, cognitive_complexity INTEGER, cyclomatic_complexity INTEGER)",
            [],
        ).unwrap();
        conn.execute(
            "CREATE TABLE project_files (id INTEGER PRIMARY KEY, file_path TEXT, parse_status TEXT, last_indexed_at TEXT)",
            [],
        ).unwrap();
        conn.execute(
            "CREATE TABLE project_symbols (file_id INTEGER, cognitive_complexity INTEGER, cyclomatic_complexity INTEGER)",
            [],
        )
        .unwrap();

        // Primary has the file but complexity is 0
        conn.execute(
            "INSERT INTO symbols (file_path, cognitive_complexity) VALUES ('a.rs', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project_files (id, file_path) VALUES (1, 'a.rs')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project_symbols (file_id, cognitive_complexity) VALUES (1, 10)",
            [],
        )
        .unwrap();

        let storage = StorageManager::init_from_conn(conn);
        let result = query_file_complexities(&storage, &["a.rs".to_string()]).unwrap();
        // Should be 0, because it was FOUND in primary (precedence rule)
        assert_eq!(result.get("a.rs"), Some(&0));
    }

    #[test]
    fn test_hotspots_graceful_degradation_without_project_symbols() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE symbols (file_path TEXT, cognitive_complexity INTEGER, cyclomatic_complexity INTEGER)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO symbols (file_path, cognitive_complexity) VALUES ('a.rs', 5)",
            [],
        )
        .unwrap();

        let storage = StorageManager::init_from_conn(conn);
        let result = query_file_complexities(&storage, &["a.rs".to_string()]).unwrap();
        assert_eq!(result.get("a.rs"), Some(&5));
        // No crash even though project_symbols table is missing
    }
}
