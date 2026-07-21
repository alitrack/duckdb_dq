//! Vector store for semantic model/column embeddings.
//!
//! Supports:
//!   semantic_index_model(model_name, embedding_csv) → index a model by vector
//!   semantic_vector_search(query_csv, k)             → cosine similarity search

use std::collections::HashMap;
use std::sync::Mutex;

/// Parses "0.1,0.2,-0.3" → Vec<f32>
pub fn parse_vec(s: &str) -> Result<Vec<f32>, String> {
    s.split(',')
        .map(|p| {
            p.trim()
                .parse::<f32>()
                .map_err(|e| format!("invalid float '{}': {}", p, e))
        })
        .collect()
}

/// Cosine similarity between two vectors.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (dot, na, nb) = a
        .iter()
        .zip(b.iter())
        .fold((0.0f32, 0.0f32, 0.0f32), |(d, na, nb), (x, y)| {
            (d + x * y, na + x * x, nb + y * y)
        });
    let denom = (na * nb).sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

/// In-memory vector index per model name.
pub struct VectorStore {
    /// model_name → embedding
    pub embeddings: HashMap<String, Vec<f32>>,
}

impl VectorStore {
    pub fn new() -> Self {
        Self {
            embeddings: HashMap::new(),
        }
    }

    /// Index a model by name + embedding.
    pub fn index(&mut self, model_name: &str, vec: Vec<f32>) {
        self.embeddings.insert(model_name.to_string(), vec);
    }

    /// Search top-k models by cosine similarity to query vector.
    pub fn search(&self, query: &[f32], k: usize) -> Vec<(String, f32)> {
        let mut scores: Vec<(String, f32)> = self
            .embeddings
            .iter()
            .map(|(name, v)| (name.clone(), cosine(query, v)))
            .collect();
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(k);
        scores
    }}

/// Global vector store (one per extension load).
static VECTOR_STORE: once_cell::sync::Lazy<Mutex<VectorStore>> =
    once_cell::sync::Lazy::new(|| Mutex::new(VectorStore::new()));

pub fn get_vector_store() -> &'static Mutex<VectorStore> {
    &VECTOR_STORE
}
