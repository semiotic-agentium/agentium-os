//! Vector similarity functions for drift scoring.

/// Cosine similarity between two embedding vectors.
///
/// Returns a value in `[-1.0, 1.0]`.  Identical directions yield `1.0`;
/// orthogonal vectors yield `0.0`; opposite directions yield `-1.0`.
///
/// Returns `0.0` when either vector has zero norm (degenerate input).
///
/// # Panics
///
/// Does **not** panic.  Mismatched lengths silently truncate to the shorter
/// vector (zip semantics) — callers should ensure equal lengths.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_similarity_covers_all_geometric_cases() {
        // Identical → 1.0
        let v = vec![1.0, 2.0, 3.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);

        // Orthogonal → 0.0
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);

        // Opposite → -1.0
        let neg: Vec<f32> = v.iter().map(|x| -x).collect();
        assert!((cosine_similarity(&v, &neg) + 1.0).abs() < 1e-6);

        // Zero-norm → 0.0 (degenerate guard)
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 2.0]), 0.0);
    }
}
