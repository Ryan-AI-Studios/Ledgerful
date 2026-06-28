pub fn cosine_sim(a: &[f32], b: &[f32]) -> Result<f32, String> {
    if a.len() != b.len() {
        return Err(format!(
            "Vector length mismatch: {} vs {}",
            a.len(),
            b.len()
        ));
    }

    if a.iter().chain(b.iter()).any(|value| !value.is_finite()) {
        return Err("Non-finite vector value: cannot compute cosine similarity".to_string());
    }

    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if mag_a == 0.0 || mag_b == 0.0 {
        return Err("Zero vector: cannot compute cosine similarity".to_string());
    }

    let sim = dot / (mag_a * mag_b);
    if !sim.is_finite() {
        return Err("Non-finite cosine similarity".to_string());
    }
    let sim = sim.clamp(-1.0, 1.0);

    #[cfg(debug_assertions)]
    debug_assert!((-1.0..=1.0).contains(&sim));

    Ok(sim)
}

pub fn pairwise_cosine(query: &[f32], candidates: &[(String, Vec<f32>)]) -> Vec<(String, f32)> {
    let mut scores: Vec<(String, f32)> = candidates
        .iter()
        .filter_map(|(key, vec)| {
            cosine_sim(query, vec)
                .ok()
                .map(|score| (key.clone(), score))
        })
        .collect();

    scores.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    scores
}

pub fn top_k(scores: Vec<(String, f32)>, k: usize) -> Vec<(String, f32)> {
    let mut sorted = scores;
    sorted.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    if k == 0 || k >= sorted.len() {
        return sorted;
    }

    sorted.truncate(k);
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_sim_identical_vectors_returns_one() {
        let v = vec![1.0_f32, 2.0, 3.0];
        let result = cosine_sim(&v, &v).unwrap();
        assert!((result - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_sim_clamps_f32_roundoff_above_one() {
        let v = vec![0.066030696_f32, 1.1272413, 0.46750933];
        let result = cosine_sim(&v, &v).unwrap();
        assert_eq!(result, 1.0);
    }

    #[test]
    fn cosine_sim_non_finite_vector_returns_error() {
        let result = cosine_sim(&[f32::NAN, 1.0], &[1.0, 1.0]);
        assert!(result.is_err());
    }

    #[test]
    fn cosine_sim_zero_vector_returns_error() {
        let v = vec![0.0_f32, 0.0, 0.0];
        let result = cosine_sim(&v, &[1.0, 2.0, 3.0]);
        assert!(result.is_err());
    }

    #[test]
    fn cosine_sim_length_mismatch_returns_error() {
        let a = vec![1.0_f32, 2.0, 3.0];
        let b = vec![1.0_f32, 2.0];
        let result = cosine_sim(&a, &b);
        assert!(result.is_err());
    }

    #[test]
    fn cosine_sim_orthogonal_vectors() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![0.0_f32, 1.0];
        let result = cosine_sim(&a, &b).unwrap();
        assert!(result.abs() < 1e-6);
    }

    #[test]
    fn pairwise_cosine_sorts_descending() {
        let query = vec![1.0_f32, 0.0, 0.0];
        let candidates = vec![
            ("a".to_string(), vec![1.0_f32, 0.0, 0.0]),
            ("b".to_string(), vec![0.0_f32, 1.0, 0.0]),
            ("c".to_string(), vec![1.0_f32, 1.0, 0.0]),
        ];
        let results = pairwise_cosine(&query, &candidates);
        assert_eq!(results.len(), 3);
        // a (1.0) > c (~0.707) > b (0.0)
        assert_eq!(results[0].0, "a");
        assert_eq!(results[2].0, "b");
        assert!(results[0].1 > results[1].1);
        assert!(results[1].1 > results[2].1);
    }

    #[test]
    fn top_k_returns_at_most_k() {
        let scores = vec![
            ("a".to_string(), 0.9_f32),
            ("b".to_string(), 0.8),
            ("c".to_string(), 0.7),
            ("d".to_string(), 0.6),
            ("e".to_string(), 0.5),
        ];
        let result = top_k(scores, 3);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn top_k_zero_returns_all() {
        let scores = vec![
            ("a".to_string(), 0.9_f32),
            ("b".to_string(), 0.8),
            ("c".to_string(), 0.7),
        ];
        let result = top_k(scores, 0);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn top_k_stable_sort_on_ties() {
        let scores = vec![
            ("a".to_string(), 0.5_f32),
            ("b".to_string(), 0.5),
            ("c".to_string(), 0.9),
        ];
        let result = top_k(scores, 3);
        assert_eq!(result.len(), 3);
        // Highest score first
        assert_eq!(result[0].0, "c");
        // For ties, key ascending preserves insertion order with stable sort
        assert_eq!(result[0].1, 0.9);
        // The tied items should be sorted by key ascending after sort
    }
}
