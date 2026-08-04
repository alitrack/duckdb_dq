//! BM25 (Okapi) full-text search index — hand-rolled backend (default).
//!
//! Pure-Rust implementation of the Okapi BM25 ranking function.
//! Stores an inverted index (term → doc_id → term frequency) and
//! supports incremental add/remove without full rebuild.
//!
//! Chinese/Japanese/Korean text is segmented with jieba-rs; other text
//! is split on whitespace/punctuation with optional stemming.
//!
//! Parameters: k1 = 1.2, b = 0.75 (standard).
//!
//! Functions:
//!   semantic_bm25_index_doc(doc_id, text)   → index one document
//!   semantic_bm25_remove_doc(doc_id)         → remove one document
//!   semantic_bm25_reset()                    → clear all indexes
//!   semantic_bm25_search(query, k)           → table: doc_id, score
use jieba_rs::Jieba;
use rust_stemmers::{Algorithm, Stemmer};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use once_cell::sync::Lazy;

use super::Bm25Snapshot;

// ─── Inverted index ─────────────────────────────────────────────────────

/// Per-document stats needed for BM25 scoring.
#[derive(Debug, Clone, Default)]
struct DocStats {
    /// Total term count (document length in tokens).
    len: usize,
}

/// Inverted index: term → doc_id → term frequency.
#[derive(Default)]
pub struct Bm25Index {
    /// term → (doc_id → tf)
    pub inverted: HashMap<String, BTreeMap<String, u32>>,
    /// doc_id → stats
    docs: HashMap<String, DocStats>,
    /// doc_id → raw text (kept for snapshot export/rebuild).
    doc_texts: HashMap<String, String>,
    /// Total number of documents.
    doc_count: usize,
    /// Sum of all document lengths.
    total_len: usize,
    /// Optional stemmer for language-specific stemming.
    #[allow(dead_code)]
    stemmer: Option<Stemmer>,
    /// Chinese tokenizer (lazy init).
    #[allow(dead_code)]
    jieba: Lazy<Jieba>,
}

/// BM25 parameters.
const K1: f32 = 1.2;
const B: f32 = 0.75;

impl Bm25Index {
    /// Plain index (no stemmer). Public API / tests; the global store
    /// uses [`with_english_stemmer`](Bm25Index::with_english_stemmer).
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            stemmer: None,
            jieba: Lazy::new(Jieba::new),
            ..Default::default()
        }
    }

    /// New index with an English Porter stemmer.
    pub fn with_english_stemmer() -> Self {
        Self {
            stemmer: Some(Stemmer::create(Algorithm::English)),
            jieba: Lazy::new(Jieba::new),
            ..Default::default()
        }
    }

    /// Set a custom stemmer.
    pub fn set_stemmer(&mut self, s: Stemmer) {
        self.stemmer = Some(s);
    }

    /// Disable stemming.
    pub fn clear_stemmer(&mut self) {
        self.stemmer = None;
    }

    /// Reset index to empty (preserves stemmer setting).
    pub fn reset(&mut self) {
        self.inverted.clear();
        self.docs.clear();
        self.doc_texts.clear();
        self.doc_count = 0;
        self.total_len = 0;
    }

    /// Returns true if text contains CJK (Chinese/Japanese/Korean) characters.
    fn has_cjk(text: &str) -> bool {
        text.chars().any(|c| {
            matches!(c,
                '\u{4E00}'..='\u{9FFF}' | // CJK Unified Ideographs
                '\u{3400}'..='\u{4DBF}' | // CJK Extension A
                '\u{3040}'..='\u{309F}' | // Hiragana
                '\u{30A0}'..='\u{30FF}' | // Katakana
                '\u{AC00}'..='\u{D7AF}'   // Hangul Syllables
            )
        })
    }

    /// Tokenize text: lowercase, split on non-alphanumeric.
    /// For CJK text (Chinese/Japanese/Korean), uses jieba segmenter.
    /// For other text, splits on whitespace/punctuation with optional stemming.
    fn tokenize_inner(text: &str, stemmer: Option<&Stemmer>, jieba: &Jieba) -> Vec<String> {
        let mut tokens = Vec::new();

        // Split text into segments: CJK runs vs non-CJK runs
        if Self::has_cjk(text) {
            // Use jieba for CJK text
            for word in jieba.cut(text, true) {
                let w = word.trim().to_lowercase();
                if !w.is_empty() && w.len() >= 1 {
                    tokens.push(w);
                }
            }
        } else {
            // Standard tokenizer: lowercase, split on non-alphanumeric
            tokens = text
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|t| !t.is_empty() && t.len() >= 2)
                .map(|t| {
                    if let Some(s) = stemmer {
                        s.stem(t).to_string()
                    } else {
                        t.to_string()
                    }
                })
                .collect();
        }
        tokens
    }

    fn tokenize(&self, text: &str) -> Vec<String> {
        Self::tokenize_inner(text, self.stemmer.as_ref(), &self.jieba)
    }

    /// Index a single document by (id, text).
    pub fn index_doc(&mut self, doc_id: &str, text: &str) {
        let tokens = self.tokenize(text);
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
        self.doc_texts.insert(doc_id.to_string(), text.to_string());
        self.doc_count += 1;
        self.total_len += len;
    }

    /// Remove a document from the index.
    pub fn remove_doc(&mut self, doc_id: &str) {
        if let Some(stats) = self.docs.remove(doc_id) {
            self.doc_count -= 1;
            self.total_len -= stats.len;
            self.doc_texts.remove(doc_id);
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
        let terms = self.tokenize(query);
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

    /// Export index state for snapshot (doc ids + raw texts for rebuild).
    pub fn export(&self) -> Bm25Snapshot {
        let texts: Vec<(String, String)> = self
            .docs
            .keys()
            .filter_map(|id| {
                self.doc_texts
                    .get(id)
                    .map(|t| (id.clone(), t.clone()))
            })
            .collect();
        Bm25Snapshot {
            docs: texts.iter().map(|(id, _)| id.clone()).collect(),
            texts,
            doc_count: self.doc_count,
            total_len: self.total_len,
        }
    }

    /// Import from snapshot — rebuilds the index from stored texts.
    pub fn import(&mut self, snap: &Bm25Snapshot) {
        self.reset();
        for (id, text) in &snap.texts {
            self.index_doc(id, text);
        }
    }
}

// ─── Global BM25 store ──────────────────────────────────────────────────

static BM25_STORE: once_cell::sync::Lazy<Mutex<Bm25Index>> =
    once_cell::sync::Lazy::new(|| Mutex::new(Bm25Index::with_english_stemmer()));

pub fn get_bm25() -> &'static Mutex<Bm25Index> {
    &BM25_STORE
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_basic() {
        let idx = Bm25Index::new();
        let jieba = Jieba::new();
        let tokens = Bm25Index::tokenize_inner("Hello, World! DuckDB is great.", None, &jieba);
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
        assert!(tokens.contains(&"duckdb".to_string()));
        assert!(tokens.contains(&"is".to_string()));
        assert!(tokens.contains(&"great".to_string()));
    }

    #[test]
    fn tokenize_with_stemmer() {
        let idx = Bm25Index::with_english_stemmer();
        let tokens = idx.tokenize("running runs runner easily");
        // Porter stemmer: running→run, runs→run, runner→runner, easily→easili
        assert!(tokens.contains(&"run".to_string()));
        assert!(!tokens.contains(&"running".to_string()));
    }

    #[test]
    fn stemmer_search_matches_variants() {
        let mut idx = Bm25Index::with_english_stemmer();
        idx.index_doc("d1", "The database runs analytical queries");
        idx.index_doc("d2", "Running is good exercise");

        // "running" stems to "run", matches both "runs" and "running"
        let results = idx.search("running", 3);
        let ids: Vec<&str> = results.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"d1")); // "runs" → "run"
        assert!(ids.contains(&"d2")); // "running" → "run"
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
