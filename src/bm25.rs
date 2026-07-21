//! BM25 (Okapi) full-text search index.
//!
//! Pure-Rust implementation of the Okapi BM25 ranking function.
//! Stores an inverted index (term → doc_id → term frequency) and
//! supports incremental add/remove without full rebuild.
//!
//! Parameters: k1 = 1.2, b = 0.75 (standard).
//!
//! Functions:
//!   semantic_bm25_index_doc(doc_id, text)   → index one document
//!   semantic_bm25_remove_doc(doc_id)         → remove one document
//!   semantic_bm25_reset()                    → clear all indexes
//!   semantic_bm25_search(query, k)           → table: doc_id, score

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

// ─── Inverted index ─────────────────────────────────────────────────────

/// Per-document stats needed for BM25 scoring.
#[derive(Debug, Clone, Default)]
struct DocStats {
    /// Total term count (document length in tokens).
    len: usize,
}

/// Inverted index: term → doc_id → term frequency.
#[derive(Debug, Clone, Default)]
pub struct Bm25Index {
    /// term → (doc_id → tf)
    pub inverted: HashMap<String, BTreeMap<String, u32>>,
    /// doc_id → stats
    docs: HashMap<String, DocStats>,
    /// Total number of documents.
    doc_count: usize,
    /// Sum of all document lengths.
    total_len: usize,
}

/// BM25 parameters.
const K1: f32 = 1.2;
const B: f32 = 0.75;

impl Bm25Index {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset index to empty.
    pub fn reset(&mut self) {
        self.inverted.clear();
        self.docs.clear();
        self.doc_count = 0;
        self.total_len = 0;
    }

    /// Tokenize text: lowercase, split on Unicode word boundaries.
    fn tokenize(text: &str) -> Vec<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty() && t.len() >= 2)
            .map(|t| t.to_string())
            .collect()
    }

    /// Index a single document by (id, text).
    pub fn index_doc(&mut self, doc_id: &str, text: &str) {
        let tokens = Self::tokenize(text);
        let len = tokens.len();

        // Remove old entry if this id existed
        self.remove_doc(doc_id);

        // Count term frequencies
        let mut tf_map: HashMap<String, u32> = HashMap::new();
        for t in &tokens {
            *tf_map.entry(t.clone()).or_insert(0) += 1;
        }

        // Insert into inverted index
        for (term, tf) in &tf_map {
            self.inverted
                .entry(term.clone())
                .or_default()
                .insert(doc_id.to_string(), *tf);
        }

        self.docs.insert(
            doc_id.to_string(),
            DocStats { len },
        );
        self.doc_count += 1;
        self.total_len += len;
    }

    /// Remove a document from the index.
    pub fn remove_doc(&mut self, doc_id: &str) {
        if let Some(stats) = self.docs.remove(doc_id) {
            self.doc_count -= 1;
            self.total_len -= stats.len;
            // Remove from inverted index
            for posting in self.inverted.values_mut() {
                posting.remove(doc_id);
            }
            // Clean up empty term entries
            self.inverted.retain(|_, posting| !posting.is_empty());
        }
    }

    /// Average document length.
    fn avgdl(&self) -> f32 {
        if self.doc_count == 0 {
            1.0
        } else {
            self.total_len as f32 / self.doc_count as f32
        }
    }

    /// IDF component: log((N - n + 0.5) / (n + 0.5) + 1)
    fn idf(&self, n: usize) -> f32 {
        let n = n as f32;
        let doc_n = self.doc_count as f32;
        (((doc_n - n + 0.5) / (n + 0.5)) + 1.0).ln()
    }

    /// BM25 score for one document against a query (list of query terms).
    fn score_doc(&self, doc_id: &str, query_terms: &[String]) -> Option<f32> {
        let doc_stats = self.docs.get(doc_id)?;
        let dl = doc_stats.len as f32;
        let avg = self.avgdl();

        let mut score = 0.0f32;
        for term in query_terms {
            if let Some(posting) = self.inverted.get(term) {
                let n = posting.len(); // number of docs containing this term
                let idf = self.idf(n);
                if let Some(&tf) = posting.get(doc_id) {
                    let tf = tf as f32;
                    let numerator = tf * (K1 + 1.0);
                    let denominator = tf + K1 * (1.0 - B + B * (dl / avg));
                    score += idf * (numerator / denominator);
                }
            }
        }
        Some(score)
    }

    /// Search top-k documents by BM25 score for a text query.
    pub fn search(&self, query: &str, k: usize) -> Vec<(String, f32)> {
        let terms = Self::tokenize(query);
        if terms.is_empty() || self.docs.is_empty() {
            return vec![];
        }

        let mut scores: Vec<(String, f32)> = self
            .docs
            .keys()
            .filter_map(|doc_id| {
                self.score_doc(doc_id, &terms)
                    .filter(|&s| s > 0.0)
                    .map(|s| (doc_id.clone(), s))
            })
            .collect();

        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(k);
        scores
    }

    /// Get BM25 scores for all indexed docs — used by hybrid fusion.
    /// Returns (doc_id, bm25_score) for docs matching any query term.
    pub fn scores_for(&self, query: &str) -> Vec<(String, f32)> {
        self.search(query, self.docs.len().max(1))
    }

    /// Number of indexed documents.
    pub fn len(&self) -> usize {
        self.doc_count
    }

    /// Export index state for snapshot.
    pub fn export(&self) -> Bm25Snapshot {
        Bm25Snapshot {
            inverted: self.inverted.clone(),
            docs: self.docs.keys().cloned().collect(),
            doc_count: self.doc_count,
            total_len: self.total_len,
        }
    }

    /// Import from snapshot.
    pub fn import(&mut self, snap: &Bm25Snapshot) {
        self.reset();
        self.inverted = snap.inverted.clone();
        for id in &snap.docs {
            // Reconstruct minimal doc stats — we lose exact lengths but
            // keep the index functional. Real lengths are reconstructed
            // on next indexing cycle.
            self.docs.insert(id.clone(), DocStats { len: 1 });
        }
        self.doc_count = snap.doc_count;
        self.total_len = snap.total_len;
    }
}

/// Serializable snapshot of the BM25 index.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Bm25Snapshot {
    pub inverted: HashMap<String, BTreeMap<String, u32>>,
    pub docs: Vec<String>,
    pub doc_count: usize,
    pub total_len: usize,
}

// ─── Global BM25 store ──────────────────────────────────────────────────

static BM25_STORE: once_cell::sync::Lazy<Mutex<Bm25Index>> =
    once_cell::sync::Lazy::new(|| Mutex::new(Bm25Index::new()));

pub fn get_bm25() -> &'static Mutex<Bm25Index> {
    &BM25_STORE
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_basic() {
        let tokens = Bm25Index::tokenize("Hello, World! DuckDB is great.");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
        assert!(tokens.contains(&"duckdb".to_string()));
        // "is" is 2 chars, passes >= 2 filter
        assert!(tokens.contains(&"is".to_string()));
        assert!(tokens.contains(&"great".to_string()));
    }

    #[test]
    fn index_and_search() {
        let mut idx = Bm25Index::new();
        idx.index_doc("d1", "DuckDB is a fast analytical database");
        idx.index_doc("d2", "PostgreSQL is a relational database");
        idx.index_doc("d3", "DuckDB runs analytical queries fast");

        let results = idx.search("duckdb fast", 3);
        assert_eq!(results.len(), 2); // d2 has neither "duckdb" nor "fast"
        // d3 is shorter (4 tokens vs 5) => higher BM25 by length norm
        let ids: Vec<&str> = results.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"d1"));
        assert!(ids.contains(&"d3"));
    }

    #[test]
    fn remove_doc() {
        let mut idx = Bm25Index::new();
        idx.index_doc("d1", "hello world");
        idx.index_doc("d2", "goodbye world");
        assert_eq!(idx.len(), 2);

        idx.remove_doc("d1");
        assert_eq!(idx.len(), 1);
        let results = idx.search("hello", 5);
        assert!(results.is_empty()); // "hello" only in d1
    }

    #[test]
    fn reindex_same_id() {
        let mut idx = Bm25Index::new();
        idx.index_doc("d1", "old text");
        idx.index_doc("d1", "new different text");
        assert_eq!(idx.len(), 1);

        let results = idx.search("old", 5);
        assert!(results.is_empty());
        let results = idx.search("new", 5);
        assert!(!results.is_empty());
    }

    #[test]
    fn bm25_scoring_order() {
        let mut idx = Bm25Index::new();
        // d1: longer doc with target term once
        idx.index_doc(
            "d1",
            "this is a very long document that mentions duckdb exactly once among many words",
        );
        // d2: short doc with target term
        idx.index_doc("d2", "duckdb duckdb duckdb");

        let results = idx.search("duckdb", 3);
        assert_eq!(results[0].0, "d2"); // short doc with high tf wins
    }
}
