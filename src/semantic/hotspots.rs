use crate::state::storage_cozo::CozoStorage;
use crate::util::path::{display_path_under_work_root, path_is_under_work_root};
use cozo::{DataValue, Num};
use miette::Result;
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SemanticMatch {
    pub file1: String,
    pub name1: String,
    pub offset1: usize,
    pub file2: String,
    pub name2: String,
    pub offset2: usize,
    pub similarity: f32,
}

/// Find high-similarity snippet pairs under `work_root` (0152 B2-H).
///
/// Drops pairs where either path fails under-work-root (legacy absolute foreign).
/// Absolute-under-root legacy keys are rewritten to relative for display.
pub fn find_semantic_hotspots(
    storage: &CozoStorage,
    work_root: &Path,
    threshold: f32,
) -> Result<Vec<SemanticMatch>> {
    // Find snippets with high cosine similarity (> threshold).
    // Similarity = 1.0 - Cosine Distance.
    // We use a self-join on snippet_embedding.
    // Note: snippet_embedding is a key-value relation, so we use {{...}} syntax.
    let script = format!(
        "?[f1, n1, o1, f2, n2, o2, similarity] := 
            *snippet_embedding{{file_path: f1, name: n1, line_offset: o1, embedding: v1}},
            *snippet_embedding{{file_path: f2, name: n2, line_offset: o2, embedding: v2}},
            f1 < f2,
            dist = cos_dist(v1, v2),
            similarity = 1.0 - dist,
            similarity > {threshold}
        ?[f1, n1, o1, f2, n2, o2, similarity] := 
            *snippet_embedding{{file_path: f1, name: n1, line_offset: o1, embedding: v1}},
            *snippet_embedding{{file_path: f2, name: n2, line_offset: o2, embedding: v2}},
            f1 == f2,
            o1 < o2,
            dist = cos_dist(v1, v2),
            similarity = 1.0 - dist,
            similarity > {threshold}",
        threshold = threshold
    );

    let res = storage.run_script(&script)?;
    let mut results = Vec::new();
    for row in res.rows {
        if let (
            Some(DataValue::Str(f1)),
            Some(DataValue::Str(n1)),
            Some(DataValue::Num(Num::Int(o1))),
            Some(DataValue::Str(f2)),
            Some(DataValue::Str(n2)),
            Some(DataValue::Num(Num::Int(o2))),
            Some(DataValue::Num(num)),
        ) = (
            row.first(),
            row.get(1),
            row.get(2),
            row.get(3),
            row.get(4),
            row.get(5),
            row.get(6),
        ) {
            // B2-H: drop pairs with any foreign-absolute path.
            if !path_is_under_work_root(work_root, f1.as_ref())
                || !path_is_under_work_root(work_root, f2.as_ref())
            {
                continue;
            }
            let file1 = display_path_under_work_root(work_root, f1.as_ref())
                .unwrap_or_else(|| f1.to_string());
            let file2 = display_path_under_work_root(work_root, f2.as_ref())
                .unwrap_or_else(|| f2.to_string());
            let sim = match num {
                Num::Float(f) => *f as f32,
                Num::Int(i) => *i as f32,
            };
            results.push(SemanticMatch {
                file1,
                name1: n1.to_string(),
                offset1: *o1 as usize,
                file2,
                name2: n2.to_string(),
                offset2: *o2 as usize,
                similarity: sim,
            });
        }
    }
    // Deterministic emit order.
    results.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file1.cmp(&b.file1))
            .then_with(|| a.file2.cmp(&b.file2))
            .then_with(|| a.name1.cmp(&b.name1))
            .then_with(|| a.name2.cmp(&b.name2))
    });
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cozo::{DataValue, ScriptMutability};
    use std::collections::BTreeMap;

    fn plant(storage: &CozoStorage, file_path: &str, name: &str, embedding: Vec<f32>) {
        // Normalize for cos_dist.
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        let emb: Vec<f32> = embedding.iter().map(|x| x / norm).collect();
        let mut params = BTreeMap::new();
        params.insert(
            "data".to_string(),
            DataValue::from(vec![DataValue::List(Box::new(vec![
                DataValue::from(file_path),
                DataValue::from(name),
                DataValue::from(0_i64),
                DataValue::Vec(Box::new(cozo::Vector::F32(emb.into()))),
            ]))]),
        );
        storage
            .run_script(
                ":create snippet_embedding {file_path,name,line_offset=>embedding:<F32; 3>}",
            )
            .ok();
        storage
            .run_script_with_params(
                "?[file_path, name, line_offset, embedding] <- $data :put snippet_embedding",
                params,
                ScriptMutability::Mutable,
            )
            .expect("plant");
    }

    #[test]
    fn hotspots_drop_foreign_absolute_pairs() {
        let storage = CozoStorage::new_in_memory().expect("cozo");
        let dir_a = tempfile::tempdir().expect("A");
        let dir_b = tempfile::tempdir().expect("B");
        let root_a = dir_a.path();
        let poison_b = dir_b
            .path()
            .join("poison.rs")
            .to_string_lossy()
            .replace('\\', "/");

        plant(&storage, "src/a.rs", "fn_a", vec![1.0, 0.0, 0.0]);
        plant(&storage, "src/b.rs", "fn_b", vec![1.0, 0.0, 0.0]);
        plant(&storage, &poison_b, "fn_poison", vec![1.0, 0.0, 0.0]);

        let matches = find_semantic_hotspots(&storage, root_a, 0.5).expect("hotspots");
        assert!(
            matches
                .iter()
                .all(|m| !m.file1.contains("poison") && !m.file2.contains("poison")),
            "foreign absolute pairs must be dropped: {matches:?}"
        );
        assert!(
            matches
                .iter()
                .any(|m| (m.file1 == "src/a.rs" && m.file2 == "src/b.rs")
                    || (m.file1 == "src/b.rs" && m.file2 == "src/a.rs")),
            "relative under-root pair should remain: {matches:?}"
        );
    }
}
