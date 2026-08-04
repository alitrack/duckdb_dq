//! BM25 (Okapi) full-text search index — two selectable backends.
//!
//! **Default (`handrolled`)** — lightweight pure-Rust inverted index with
//! jieba-rs CJK segmentation. Small dependency footprint, snapshot
//! persistence. Good for tens of thousands of docs.
//!
//! **`tantivy` feature** — tantivy engine (+ tantivy-jieba). Phrase
//! queries, position data, better scaling, same SQL API.
//!
//! Pick at build time:
//! ```text
//! cargo build --release                       # handrolled (default)
//! cargo build --release --features tantivy    # tantivy backend
//! ```
//!
//! SQL functions are identical for both:
//! `semantic_bm25_index_doc`, `_remove_doc`, `_reset`, `_stemmer`,
//! `semantic_bm25_search`.

#[cfg(feature = "tantivy")]
mod tantivy;
#[cfg(not(feature = "tantivy"))]
mod handrolled;

/// Serializable snapshot of the BM25 index — shared by both backends so
/// the persistence layer is backend-agnostic.
///
/// `texts` holds (doc_id, raw_text) pairs so the index can be rebuilt on
/// restore; `docs` is a convenience id list.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Bm25Snapshot {
    pub docs: Vec<String>,
    pub texts: Vec<(String, String)>,
    pub doc_count: usize,
    pub total_len: usize,
}

// Re-exported for downstream users of the rlib; the extension itself
// only touches `get_bm25()`.
#[allow(unused_imports)]
#[cfg(feature = "tantivy")]
pub use tantivy::{get_bm25, Bm25Index};
#[allow(unused_imports)]
#[cfg(not(feature = "tantivy"))]
pub use handrolled::{get_bm25, Bm25Index};
