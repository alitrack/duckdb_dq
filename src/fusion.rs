//! Hybrid fusion — weighted combination of dense (vector) + sparse (BM25) + graph scores.
//!
//! Merges L1 (cosine similarity), BM25 (Okapi TF-IDF), and L2 (graph distance)
//! into a single ranked result. The three-way fusion mirrors npp-kb's
//! dense+sparse+graph pattern, implemented natively in Rust.
//!
//! Function:
//!   semantic_hybrid_search(query_vec, k, dw?, gw?, bm25_query?, bw?, hub?)
//!     → table: model_name, dense_score, bm25_score, graph_score, fused_score

use crate::vectors;
use crate::graph;
use crate::bm25;
use std::collections::HashMap;

/// A fused result combining vector, BM25, and graph scores.
#[derive(Debug, Clone)]
pub struct FusedResult {
    pub model_name: String,
    pub dense_score: f32,
    pub bm25_score: f32,
    pub graph_score: f32,
    pub fused_score: f32,
}

/// Hybrid search: 3-way weighted fusion.
///
/// - `query_vec`: comma-separated vector for cosine search
/// - `k`: top-k results
/// - `dense_weight`: weight for dense score (default 0.50)
/// - `bm25_weight`: weight for BM25 score (default 0.30)
/// - `graph_weight`: weight for graph distance (default 0.20)
/// - `bm25_query`: text query for BM25 search (empty = skip BM25)
/// - `graph_hub_model`: optional hub model for graph proximity
///
/// Graph score = 1.0 / (1.0 + distance) for reachable, 0.0 for unreachable.
/// BM25 scores are normalized to [0, 1] against max in the result set.
pub fn hybrid_search(
    query_vec: &[f32],
    k: usize,
    dense_weight: f32,
    bm25_weight: f32,
    graph_weight: f32,
    bm25_query: Option<&str>,
    graph_hub_model: Option<&str>,
) -> Vec<FusedResult> {
    let store = vectors::get_vector_store().lock().unwrap();

    // 1. Dense (cosine) scores
    let dense_scores: Vec<(String, f32)> = store.search(query_vec, k * 3);

    // 2. BM25 scores (if query provided)
    let bm25_scores: HashMap<String, f32> = if let Some(q) = bm25_query {
        if let Ok(bm) = bm25::get_bm25().lock() {
            let raw = bm.scores_for(q);
            let max_score = raw.iter().map(|(_, s)| *s).fold(0.0f32, f32::max);
            raw.into_iter()
                .map(|(id, s)| {
                    let norm = if max_score > 0.0 { s / max_score } else { 0.0 };
                    (id, norm)
                })
                .collect()
        } else {
            HashMap::new()
        }
    } else {
        HashMap::new()
    };

    // 3. Graph scores
    let graph_dists: HashMap<String, f32> =
        if let (Some(hub), Ok(g)) = (graph_hub_model, graph::get_graph().lock()) {
            g.discover(hub)
                .into_iter()
                .map(|(name, dist, _)| (name, 1.0 / (1.0 + dist as f32)))
                .collect()
        } else {
            HashMap::new()
        };

    // 4. Fuse
    let mut results: Vec<FusedResult> = dense_scores
        .into_iter()
        .map(|(name, dense)| {
            let bm = bm25_scores.get(&name).copied().unwrap_or(0.0);
            let graph = graph_dists.get(&name).copied().unwrap_or(0.0);
            let fused = dense_weight * dense + bm25_weight * bm + graph_weight * graph;
            FusedResult {
                model_name: name,
                dense_score: dense,
                bm25_score: bm,
                graph_score: graph,
                fused_score: fused,
            }
        })
        .collect();

    results.sort_by(|a, b| b.fused_score.partial_cmp(&a.fused_score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(k);
    results
}
