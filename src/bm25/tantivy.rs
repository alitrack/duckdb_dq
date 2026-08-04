//! BM25 (Okapi) full-text search index — tantivy backend (`--features tantivy`).
//!
//! tantivy in-RAM index with a fixed tokenizer chain:
//! jieba (Chinese word segmentation) → LowerCaser → English stemmer.
//! Phrase queries, position data, better scaling than the hand-rolled
//! backend. SQL-facing API is identical.
//!
//! SQL-facing API is unchanged: `semantic_bm25_index_doc`, `_remove_doc`,
//! `_reset`, `_stemmer`, and `semantic_bm25_search` all keep their names.
//! Persistence stays snapshot-based (`Bm25Snapshot` stores id+text so the
//! in-RAM index can be rebuilt on restore).

use std::collections::{HashMap, HashSet};

use tantivy::collector::{DocSetCollector, TopDocs};
use tantivy::query::{AllQuery, QueryParser};
use tantivy::schema::{Field, Schema, TantivyDocument, TextOptions, Value};
use tantivy::tokenizer::{Language, LowerCaser, Stemmer, TextAnalyzer};
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, Term};
use tantivy_jieba::JiebaTokenizer;

use super::Bm25Snapshot;

/// Full-text index backed by an in-RAM tantivy index.
pub struct Bm25Index {
    index: Index,
    reader: IndexReader,
    writer: IndexWriter,
    id_field: Field,
    text_field: Field,
    /// doc ids currently indexed (drives doc_count bookkeeping).
    known: HashSet<String>,
    /// doc id → raw text length (drives total_len bookkeeping).
    lens: HashMap<String, usize>,
    doc_count: usize,
    total_len: usize,
}

impl Bm25Index {
    /// New index with the fixed tokenizer chain (jieba + lower + stemmer).
    pub fn new() -> Self {
        Self::new_internal()
    }

    /// Same as [`new`](Bm25Index::new) — the English stemmer is always on.
    pub fn with_english_stemmer() -> Self {
        Self::new_internal()
    }

    /// No-op (kept for SQL API compatibility). The tokenizer chain always
    /// includes the English stemmer; runtime switching would require
    /// rebuilding the index, which is not worth it for an embedded store.
    pub fn set_stemmer(&mut self, _s: rust_stemmers::Stemmer) {}

    /// No-op (kept for SQL API compatibility).
    pub fn clear_stemmer(&mut self) {}

    fn new_internal() -> Self {
        let mut schema_builder = Schema::builder();
        let text_opts = TextOptions::default()
            .set_indexing_options(
                tantivy::schema::TextFieldIndexing::default()
                    .set_tokenizer("jieba")
                    .set_index_option(tantivy::schema::IndexRecordOption::WithFreqsAndPositions),
            )
            .set_stored();
        schema_builder.add_text_field(
            "id",
            TextOptions::default()
                .set_indexing_options(tantivy::schema::TextFieldIndexing::default())
                .set_stored(),
        );
        schema_builder.add_text_field("text", text_opts);
        let schema = schema_builder.build();

        let index = Index::create_in_ram(schema.clone());
        index.tokenizers().register(
            "jieba",
            TextAnalyzer::builder(JiebaTokenizer::new())
                .filter(LowerCaser)
                .filter(Stemmer::new(Language::English))
                .build(),
        );

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .expect("tantivy reader");
        let writer = index.writer(50_000_000).expect("tantivy writer");

        let id_field = schema.get_field("id").expect("id field");
        let text_field = schema.get_field("text").expect("text field");

        Self {
            index,
            reader,
            writer,
            id_field,
            text_field,
            known: HashSet::new(),
            lens: HashMap::new(),
            doc_count: 0,
            total_len: 0,
        }
    }

    /// Reset index to empty.
    pub fn reset(&mut self) {
        let _ = self.writer.delete_all_documents();
        let _ = self.writer.commit();
        self.known.clear();
        self.lens.clear();
        self.doc_count = 0;
        self.total_len = 0;
    }

    /// Index or replace one document by (id, text). Visible immediately.
    pub fn index_doc(&mut self, doc_id: &str, text: &str) {
        // Replace semantics: tombstone the old doc (idempotent), then add.
        self.writer
            .delete_term(Term::from_field_text(self.id_field, doc_id));
        let _ = self
            .writer
            .add_document(doc!(
                self.id_field => doc_id.to_string(),
                self.text_field => text.to_string(),
            ));
        let _ = self.writer.commit();

        if self.known.insert(doc_id.to_string()) {
            self.doc_count += 1;
        } else if let Some(old) = self.lens.remove(doc_id) {
            self.total_len = self.total_len.saturating_sub(old);
        }
        self.total_len += text.len();
        self.lens.insert(doc_id.to_string(), text.len());
    }

    /// Remove a document from the index.
    pub fn remove_doc(&mut self, doc_id: &str) {
        if self.known.remove(doc_id) {
            self.doc_count = self.doc_count.saturating_sub(1);
            if let Some(l) = self.lens.remove(doc_id) {
                self.total_len = self.total_len.saturating_sub(l);
            }
            self.writer
                .delete_term(Term::from_field_text(self.id_field, doc_id));
            let _ = self.writer.commit();
        }
    }

    /// Number of indexed documents.
    pub fn len(&self) -> usize {
        self.doc_count
    }

    /// Search top-k documents by BM25 score for a text query.
    pub fn search(&self, query: &str, k: usize) -> Vec<(String, f32)> {
        if self.doc_count == 0 || query.trim().is_empty() {
            return vec![];
        }
        let _ = self.reader.reload();
        let searcher = self.reader.searcher();

        let query_parser = QueryParser::for_index(&self.index, vec![self.text_field]);
        let parsed = match query_parser.parse_query(query) {
            Ok(q) => q,
            Err(_) => return vec![],
        };

        let top_docs = match searcher.search(&parsed, &TopDocs::with_limit(k).order_by_score()) {
            Ok(t) => t,
            Err(_) => return vec![],
        };

        let mut results = Vec::new();
        for (score, doc_addr) in top_docs {
            if let Ok(doc) = searcher.doc::<TantivyDocument>(doc_addr) {
                if let Some(val) = doc.get_first(self.id_field) {
                    if let Some(s) = val.as_str() {
                        results.push((s.to_string(), score));
                    }
                }
            }
        }
        results
    }

    /// Get BM25 scores for all indexed docs — used by hybrid fusion.
    /// Returns (doc_id, bm25_score) for docs matching any query term.
    pub fn scores_for(&self, query: &str) -> Vec<(String, f32)> {
        self.search(query, self.doc_count.max(1))
    }

    /// Export index state for snapshot (id + raw text pairs).
    pub fn export(&self) -> Bm25Snapshot {
        let _ = self.reader.reload();
        let searcher = self.reader.searcher();

        let mut texts = Vec::new();
        if let Ok(hits) = searcher.search(&AllQuery, &DocSetCollector) {
            for doc_addr in hits {
                if let Ok(doc) = searcher.doc::<TantivyDocument>(doc_addr) {
                    let id = doc
                        .get_first(self.id_field)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let text = doc
                        .get_first(self.text_field)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    texts.push((id, text));
                }
            }
        }

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

impl Default for Bm25Index {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Global BM25 store ──────────────────────────────────────────────────

static BM25_STORE: once_cell::sync::Lazy<std::sync::Mutex<Bm25Index>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(Bm25Index::with_english_stemmer()));

pub fn get_bm25() -> &'static std::sync::Mutex<Bm25Index> {
    &BM25_STORE
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze(text: &str) -> Vec<String> {
        let mut analyzer = TextAnalyzer::builder(JiebaTokenizer::new())
            .filter(LowerCaser)
            .filter(Stemmer::new(Language::English))
            .build();
        let mut stream = analyzer.token_stream(text);
        let mut out = Vec::new();
        while stream.advance() {
            out.push(stream.token().text.to_string());
        }
        out
    }

    #[test]
    fn tokenize_basic() {
        let tokens = analyze("Hello, World! DuckDB is great.");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
        assert!(tokens.contains(&"duckdb".to_string()));
    }

    #[test]
    fn tokenize_with_stemmer() {
        let tokens = analyze("running runs runner easily");
        // Porter stemmer: running→run, runs→run
        assert!(tokens.contains(&"run".to_string()));
        assert!(!tokens.contains(&"running".to_string()));
    }

    #[test]
    fn chinese_word_segmentation() {
        let tokens = analyze("量子计算加速药物研发");
        assert!(tokens.contains(&"量子".to_string()), "tokens: {tokens:?}");
        assert!(tokens.contains(&"药物".to_string()), "tokens: {tokens:?}");
    }

    #[test]
    fn stemmer_search_matches_variants() {
        let mut idx = Bm25Index::new();
        idx.index_doc("d1", "The database runs analytical queries");
        idx.index_doc("d2", "Running is good exercise");

        // "running" stems to "run", matches both "runs" and "running"
        let results = idx.search("running", 3);
        let ids: Vec<&str> = results.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"d1"), "d1 has 'runs'→run: {results:?}");
        assert!(ids.contains(&"d2"), "d2 has 'running'→run: {results:?}");
    }

    #[test]
    fn index_and_search() {
        let mut idx = Bm25Index::new();
        idx.index_doc("d1", "DuckDB is a fast analytical database");
        idx.index_doc("d2", "PostgreSQL is a relational database");
        idx.index_doc("d3", "DuckDB runs analytical queries fast");

        let results = idx.search("duckdb fast", 3);
        assert_eq!(results.len(), 2, "d2 has neither duckdb nor fast: {results:?}");
        let ids: Vec<&str> = results.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"d1"));
        assert!(ids.contains(&"d3"));
    }

    #[test]
    fn chinese_search() {
        let mut idx = Bm25Index::new();
        idx.index_doc("doc1", "量子计算加速药物研发，超导比特是核心技术");
        idx.index_doc("doc2", "DuckDB 数据库分析引擎，支持 SQL 与全文检索");

        let hits = idx.search("量子", 5);
        assert_eq!(hits.len(), 1, "「量子」应命中 doc1: {hits:?}");
        assert_eq!(hits[0].0, "doc1");

        let hits = idx.search("数据库", 5);
        assert_eq!(hits.len(), 1, "「数据库」应命中 doc2: {hits:?}");
        assert_eq!(hits[0].0, "doc2");
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
        assert!(results.is_empty());
    }

    #[test]
    fn reindex_same_id() {
        let mut idx = Bm25Index::new();
        idx.index_doc("d1", "old text");
        idx.index_doc("d1", "new different text");
        assert_eq!(idx.len(), 1);

        let results = idx.search("old", 5);
        assert!(results.is_empty(), "stale doc must be replaced: {results:?}");
        let results = idx.search("new", 5);
        assert!(!results.is_empty());
    }

    #[test]
    fn bm25_scoring_order() {
        let mut idx = Bm25Index::new();
        idx.index_doc(
            "d1",
            "this is a very long document that mentions duckdb exactly once among many words",
        );
        idx.index_doc("d2", "duckdb duckdb duckdb");

        let results = idx.search("duckdb", 3);
        assert_eq!(results[0].0, "d2", "short doc with high tf wins: {results:?}");
    }

    #[test]
    fn snapshot_roundtrip() {
        let mut idx = Bm25Index::new();
        idx.index_doc("a", "量子计算与药物研发");
        idx.index_doc("b", "DuckDB analytical engine");

        let snap = idx.export();
        assert_eq!(snap.doc_count, 2);

        let mut idx2 = Bm25Index::new();
        idx2.import(&snap);
        assert_eq!(idx2.len(), 2);
        assert!(!idx2.search("量子", 5).is_empty());
        assert!(!idx2.search("duckdb", 5).is_empty());
    }
}
