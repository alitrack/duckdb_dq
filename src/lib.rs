//! Semantic layer extension for DuckDB.
//!
//! Core functions:
//!   semantic_load(json_or_path)          → load MDL definition
//!   semantic_dry_plan(sql)               → expand modeled SQL (no execution)
//!   semantic_models()                    → table : list loaded models
//!   semantic_query(sql)                  → table : expanded physical SQL
//!
//! Vector search (L1 — model embeddings):
//!   semantic_index_model(name, embed)    → index a model by vector
//!   semantic_vector_search(query, k)     → table : top-k by cosine similarity
//!
//! Graph relationships (L2 — FK discovery):
//!   semantic_graph_reset()               → clear the FK graph
//!   semantic_graph_add_edge(a,b,cond)    → add a relationship edge
//!   semantic_discover_relationships(m)   → table : all models reachable from m
//!   semantic_shortest_path(a, b)         → table : JOIN path from a to b
//!
//! Ontology (L3 — class hierarchy + reasoning):
//!   semantic_class_define(name, parent)   → add class to taxonomy
//!   semantic_class_map(class, model, f?)  → map class to physical model
//!   semantic_property_define(n,d,r,m?)    → define property w/ domain+range
//!   semantic_class_query(class)           → table : expanded SQL for class
//!   semantic_class_inheritance(class)     → table : is-a chain + inherited
//!   semantic_ontology_export(format)      → scalar : OFN export

use libduckdb_sys::{
    duckdb_data_chunk, duckdb_function_info, duckdb_vector,
};
use once_cell::sync::OnceCell;
use quack_rs::connection::Connection;
use quack_rs::entry_point_v2;
use quack_rs::prelude::*;
use quack_rs::scalar::ScalarFunctionBuilder;
use quack_rs::table::TableFunctionBuilder;
use quack_rs::types::TypeId;
use quack_rs::vector::{VectorReader, VectorWriter};
use std::sync::Mutex;

mod mdl;
mod planner;
mod vectors;
mod graph;
mod ontology;
mod process;
mod persist;
mod fusion;
mod bm25;

use mdl::SemanticContext;

// ─── Global state ───────────────────────────────────────────────────────

static SEMANTIC_CTX: OnceCell<Mutex<Option<SemanticContext>>> = OnceCell::new();

fn get_ctx() -> &'static Mutex<Option<SemanticContext>> {
    SEMANTIC_CTX.get_or_init(|| Mutex::new(None))
}

// ─── semantic_load(json_or_path) → VARCHAR ──────────────────────────────

unsafe extern "C" fn semantic_load_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let reader = unsafe { VectorReader::new(input, 0) };
    let mut writer = unsafe { VectorWriter::new(output) };

    if reader.row_count() == 0 || unsafe { !reader.is_valid(0) } {
        unsafe { writer.write_str(0, "Error: no input") };
        return;
    }
    let input_str = unsafe { reader.read_str(0).to_string() };

    match mdl::load_mdl_json(&input_str) {
        Ok(ctx) => {
            let count = ctx.models.len();
            if let Ok(mut guard) = get_ctx().lock() {
                *guard = Some(ctx);
            }
            unsafe {
                writer.write_str(
                    0,
                    &format!(
                        "Loaded {} models. Use semantic_query('SELECT ...') to query.",
                        count
                    ),
                );
            }
        }
        Err(e) => {
            unsafe { writer.write_str(0, &format!("Error: {}", e)) };
        }
    }
}

// ─── semantic_dry_plan(sql) → VARCHAR ───────────────────────────────────

unsafe extern "C" fn semantic_dry_plan_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let reader = unsafe { VectorReader::new(input, 0) };
    let mut writer = unsafe { VectorWriter::new(output) };

    if reader.row_count() == 0 || unsafe { !reader.is_valid(0) } {
        unsafe { writer.write_str(0, "Error: no input") };
        return;
    }
    let sql = unsafe { reader.read_str(0).to_string() };

    let result = if let Ok(guard) = get_ctx().lock() {
        match guard.as_ref() {
            Some(ctx) => planner::expand_sql(&sql, ctx),
            None => Err("No MDL loaded. Run semantic_load() first.".into()),
        }
    } else {
        Err("Internal lock error".into())
    };

    match result {
        Ok(expanded) => unsafe { writer.write_str(0, &expanded) },
        Err(e) => unsafe { writer.write_str(0, &format!("Error: {}", e)) },
    }
}

// ─── semantic_index_model(model_name, embedding_csv) → VARCHAR ──────────

unsafe extern "C" fn semantic_index_model_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let reader = unsafe { VectorReader::new(input, 0) };
    let mut writer = unsafe { VectorWriter::new(output) };

    if reader.row_count() == 0 || unsafe { !reader.is_valid(0) } {
        unsafe { writer.write_str(0, "Error: no input") };
        return;
    }
    let name = unsafe { reader.read_str(0).to_string() };
    // param 2 is the embedding string (not directly readable with two params in a
    // single VectorReader call — we need a second reader)
    // For QuackRS scalars with 2 params, read both from the data chunk.
    let reader2 = unsafe { VectorReader::new(input, 1) };
    if unsafe { !reader2.is_valid(0) } {
        unsafe { writer.write_str(0, "Error: missing embedding") };
        return;
    }
    let embed_str = unsafe { reader2.read_str(0).to_string() };

    match vectors::parse_vec(&embed_str) {
        Ok(vec) => {
            if let Ok(mut store) = vectors::get_vector_store().lock() {
                store.index(&name, vec);
                unsafe { writer.write_str(0, &format!("Indexed model '{}'", name)) };
            } else {
                unsafe { writer.write_str(0, "Error: lock failed") };
            }
        }
        Err(e) => {
            unsafe { writer.write_str(0, &format!("Error: {}", e)) };
        }
    }
}

// ─── semantic_vector_search(query_csv, k) → table ───────────────────────

#[derive(Default)]
struct VectorSearchState {
    results: Vec<(String, f32)>,
    cursor: usize,
}

// ─── semantic_graph_reset() → VARCHAR ───────────────────────────────────

unsafe extern "C" fn semantic_graph_reset_fn(
    _info: duckdb_function_info,
    _input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    if let Ok(mut g) = graph::get_graph().lock() {
        g.reset();
    }
    unsafe { VectorWriter::new(output).write_str(0, "Graph cleared"); }
}

// ─── semantic_graph_add_edge(from, to, condition) → VARCHAR ─────────────

unsafe extern "C" fn semantic_graph_add_edge_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let reader = unsafe { VectorReader::new(input, 0) };
    let reader2 = unsafe { VectorReader::new(input, 1) };
    let reader3 = unsafe { VectorReader::new(input, 2) };
    let mut writer = unsafe { VectorWriter::new(output) };
    if reader.row_count() == 0 || unsafe { !reader.is_valid(0) } {
        unsafe { writer.write_str(0, "Error: missing from") };
        return;
    }
    let from = unsafe { reader.read_str(0).to_string() };
    let to = unsafe { reader2.read_str(0).to_string() };
    let cond = unsafe { reader3.read_str(0).to_string() };
    if let Ok(mut g) = graph::get_graph().lock() {
        g.add_edge(&from, &to, &cond);
        unsafe { writer.write_str(0, &format!("{} → {}", from, to)) };
    }
}

// ─── semantic_discover_relationships(model_name) → table ────────────────

#[derive(Default)]
struct DiscoverState {
    rows: Vec<(String, i32, String)>,  // (model_name, distance, join_condition)
    cursor: usize,
}

// ─── semantic_shortest_path(from, to) → table ───────────────────────────

#[derive(Default)]
struct PathState {
    steps: Vec<(String, String)>,  // (edge_label, join_condition)
    cursor: usize,
}

// ─── semantic_query + semantic_models state ─────────────────────────────

struct QueryState {
    expanded_sql: String,
    done: bool,
}

struct ModelsState {
    models: Vec<(
        String, String, String, String, i64,
    )>,
    cursor: usize,
}

// ─── Ontology: semantic_class_define(name, parent) → VARCHAR ───────────

unsafe extern "C" fn semantic_class_define_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let reader = unsafe { VectorReader::new(input, 0) };
    let reader2 = unsafe { VectorReader::new(input, 1) };
    let mut writer = unsafe { VectorWriter::new(output) };
    if reader.row_count() == 0 || unsafe { !reader.is_valid(0) } {
        unsafe { writer.write_str(0, "Error: missing name") };
        return;
    }
    let name = unsafe { reader.read_str(0).to_string() };
    let parent = unsafe { reader2.read_str(0).to_string() };
    let parent_opt = if parent.is_empty() { None } else { Some(parent.as_str()) };
    if let Ok(mut o) = ontology::get_ontology().lock() {
        o.define_class(&name, parent_opt, "");
        unsafe { writer.write_str(0, &format!("Class '{}' defined", name)) };
    }
}

// ─── Ontology: semantic_class_map(class, model, filter?) → VARCHAR ──────

unsafe extern "C" fn semantic_class_map_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let reader = unsafe { VectorReader::new(input, 0) };
    let reader2 = unsafe { VectorReader::new(input, 1) };
    let reader3 = unsafe { VectorReader::new(input, 2) };
    let mut writer = unsafe { VectorWriter::new(output) };
    let class = unsafe { reader.read_str(0).to_string() };
    let model = unsafe { reader2.read_str(0).to_string() };
    let filter_str = unsafe { reader3.read_str(0).to_string() };
    let filter = if filter_str.is_empty() { None } else { Some(filter_str.as_str()) };
    if let Ok(mut o) = ontology::get_ontology().lock() {
        o.map_class(&class, &model, filter);
        unsafe { writer.write_str(0, &format!("{} → {}", class, model)) };
    }
}

// ─── Ontology: semantic_property_define(name, domain, range, mapping?) → VARCHAR

unsafe extern "C" fn semantic_property_define_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let r0 = unsafe { VectorReader::new(input, 0) };
    let r1 = unsafe { VectorReader::new(input, 1) };
    let r2 = unsafe { VectorReader::new(input, 2) };
    let r3 = unsafe { VectorReader::new(input, 3) };
    let mut writer = unsafe { VectorWriter::new(output) };
    let name = unsafe { r0.read_str(0).to_string() };
    let domain = unsafe { r1.read_str(0).to_string() };
    let range = unsafe { r2.read_str(0).to_string() };
    let map_str = unsafe { r3.read_str(0).to_string() };
    let mapping = if map_str.is_empty() { None } else { Some(map_str.as_str()) };
    if let Ok(mut o) = ontology::get_ontology().lock() {
        o.define_property(&name, &domain, &range, mapping);
        unsafe { writer.write_str(0, &format!("Property '{}' defined", name)) };
    }
}

// ─── Ontology: semantic_ontology_export(format) → VARCHAR ───────────────

unsafe extern "C" fn semantic_ontology_export_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let reader = unsafe { VectorReader::new(input, 0) };
    let mut writer = unsafe { VectorWriter::new(output) };
    let fmt = if reader.row_count() > 0 && unsafe { reader.is_valid(0) } {
        unsafe { reader.read_str(0).to_string() }
    } else {
        "ofn".to_string()
    };
    if let Ok(o) = ontology::get_ontology().lock() {
        let text = match fmt.as_str() {
            "ofn" => o.export_ofn(),
            _ => format!("Unsupported format: {}. Use 'ofn'.", fmt),
        };
        unsafe { writer.write_str(0, &text) };
    }
}

// ─── Ontology table states ──────────────────────────────────────────────

#[derive(Default)]
struct ClassQueryState {
    sql: String,
    done: bool,
}

#[derive(Default)]
struct InheritanceState {
    rows: Vec<(String, i32, String, String)>, // class, depth, kind, detail
    cursor: usize,
}

// ─── Process Context: semantic_pattern_add(name, steps, domain, desc) → VARCHAR

unsafe extern "C" fn semantic_pattern_add_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let r0 = unsafe { VectorReader::new(input, 0) };
    let r1 = unsafe { VectorReader::new(input, 1) };
    let r2 = unsafe { VectorReader::new(input, 2) };
    let r3 = unsafe { VectorReader::new(input, 3) };
    let mut writer = unsafe { VectorWriter::new(output) };
    let name = unsafe { r0.read_str(0).to_string() };
    let steps_json = unsafe { r1.read_str(0).to_string() };
    let domain = unsafe { r2.read_str(0).to_string() };
    let desc = unsafe { r3.read_str(0).to_string() };

    // Parse steps: "customers,orders,order_items" or JSON array
    let step_names: Vec<String> = if steps_json.starts_with('[') {
        serde_json::from_str::<Vec<String>>(&steps_json).unwrap_or_default()
    } else {
        steps_json.split(',').map(|s| s.trim().to_string()).collect()
    };

    let steps: Vec<process::PatternStep> = step_names
        .iter()
        .enumerate()
        .map(|(i, n)| process::PatternStep {
            model_name: n.clone(),
            order: i as i32,
            notes: String::new(),
        })
        .collect();

    let pattern = process::WorkflowPattern {
        name, description: desc, domain,
        steps, frequency: 1,
        source: "manual".into(),
    };

    if let Ok(mut store) = process::get_store().lock() {
        store.add_pattern(pattern);
        unsafe { writer.write_str(0, "Pattern added") };
    }
}

// ─── Process Context: semantic_process_context(model_name) → table ──────

#[derive(Default)]
struct ProcessCtxState {
    rows: Vec<(String, String, String)>, // (kind, key, value)
    cursor: usize,
}

// ─── Process Context: semantic_pattern_search(query, k) → table ─────────

#[derive(Default)]
struct PatternSearchState {
    rows: Vec<(String, String, String, String)>, // name, domain, steps, desc
    cursor: usize,
}

// ─── Process Context: semantic_discover_patterns() → table ──────────────

#[derive(Default)]
struct DiscoverPatternsState {
    rows: Vec<(String, i32, String)>, // path, frequency, type
    cursor: usize,
}

// ─── Persistence: semantic_save(path) → VARCHAR ────────────────────────

unsafe extern "C" fn semantic_save_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let reader = unsafe { VectorReader::new(input, 0) };
    let mut writer = unsafe { VectorWriter::new(output) };
    if reader.row_count() == 0 { unsafe { writer.write_str(0, "Error: no path") }; return; }
    let path = unsafe { reader.read_str(0).to_string() };
    match persist::capture() {
        Ok(snap) => {
            match serde_json::to_string_pretty(&snap) {
                Ok(json) => {
                    if let Err(e) = std::fs::write(&path, &json) {
                        unsafe { writer.write_str(0, &format!("Error: {}", e)) };
                    } else {
                        unsafe { writer.write_str(0, &format!("Saved to {}", path)) };
                    }
                }
                Err(e) => unsafe { writer.write_str(0, &format!("Error: {}", e)) },
            }
        }
        Err(e) => unsafe { writer.write_str(0, &format!("Error: {}", e)) },
    }
}

// ─── Persistence: semantic_restore(path) → VARCHAR ──────────────────────

unsafe extern "C" fn semantic_restore_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let reader = unsafe { VectorReader::new(input, 0) };
    let mut writer = unsafe { VectorWriter::new(output) };
    if reader.row_count() == 0 { unsafe { writer.write_str(0, "Error: no path") }; return; }
    let path = unsafe { reader.read_str(0).to_string() };
    match std::fs::read_to_string(&path) {
        Ok(json) => {
            match serde_json::from_str::<persist::Snapshot>(&json) {
                Ok(snap) => match persist::restore(&snap) {
                    Ok(msg) => unsafe { writer.write_str(0, &msg) },
                    Err(e) => unsafe { writer.write_str(0, &format!("Error: {}", e)) },
                },
                Err(e) => unsafe { writer.write_str(0, &format!("Error: {}", e)) },
            }
        }
        Err(e) => unsafe { writer.write_str(0, &format!("Error: {}", e)) },
    }
}

// ─── BM25 scalars ──────────────────────────────────────────────────────

unsafe extern "C" fn semantic_bm25_index_doc_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let r0 = unsafe { VectorReader::new(input, 0) };
    let r1 = unsafe { VectorReader::new(input, 1) };
    let mut writer = unsafe { VectorWriter::new(output) };
    if r0.row_count() == 0 || unsafe { !r0.is_valid(0) } {
        unsafe { writer.write_str(0, "Error: missing doc_id") };
        return;
    }
    let doc_id = unsafe { r0.read_str(0).to_string() };
    let text = unsafe { r1.read_str(0).to_string() };
    if let Ok(mut bm) = crate::bm25::get_bm25().lock() {
        bm.index_doc(&doc_id, &text);
        unsafe { writer.write_str(0, &format!("Indexed '{}' ({} docs total)", doc_id, bm.len())) };
    }
}

unsafe extern "C" fn semantic_bm25_remove_doc_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let reader = unsafe { VectorReader::new(input, 0) };
    let mut writer = unsafe { VectorWriter::new(output) };
    if reader.row_count() == 0 || unsafe { !reader.is_valid(0) } {
        unsafe { writer.write_str(0, "Error: missing doc_id") };
        return;
    }
    let doc_id = unsafe { reader.read_str(0).to_string() };
    if let Ok(mut bm) = crate::bm25::get_bm25().lock() {
        bm.remove_doc(&doc_id);
        unsafe { writer.write_str(0, &format!("Removed '{}' ({} docs remain)", doc_id, bm.len())) };
    }
}

unsafe extern "C" fn semantic_bm25_reset_fn(
    _info: duckdb_function_info,
    _input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    if let Ok(mut bm) = crate::bm25::get_bm25().lock() {
        bm.reset();
    }
    unsafe { VectorWriter::new(output).write_str(0, "BM25 index cleared"); }
}

unsafe extern "C" fn semantic_bm25_stemmer_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let reader = unsafe { VectorReader::new(input, 0) };
    let mut writer = unsafe { VectorWriter::new(output) };
    let lang = if reader.row_count() > 0 && unsafe { reader.is_valid(0) } {
        unsafe { reader.read_str(0).to_string() }
    } else {
        "english".to_string()
    };
    let algorithm = match lang.as_str() {
        "arabic" => rust_stemmers::Algorithm::Arabic,
        "danish" => rust_stemmers::Algorithm::Danish,
        "dutch" => rust_stemmers::Algorithm::Dutch,
        "english" => rust_stemmers::Algorithm::English,
        "french" => rust_stemmers::Algorithm::French,
        "german" => rust_stemmers::Algorithm::German,
        "greek" => rust_stemmers::Algorithm::Greek,
        "hungarian" => rust_stemmers::Algorithm::Hungarian,
        "italian" => rust_stemmers::Algorithm::Italian,
        "norwegian" => rust_stemmers::Algorithm::Norwegian,
        "portuguese" => rust_stemmers::Algorithm::Portuguese,
        "romanian" => rust_stemmers::Algorithm::Romanian,
        "russian" => rust_stemmers::Algorithm::Russian,
        "spanish" => rust_stemmers::Algorithm::Spanish,
        "swedish" => rust_stemmers::Algorithm::Swedish,
        "tamil" => rust_stemmers::Algorithm::Tamil,
        "turkish" => rust_stemmers::Algorithm::Turkish,
        "none" => {
            if let Ok(mut bm) = crate::bm25::get_bm25().lock() {
                bm.clear_stemmer();
            }
            unsafe { writer.write_str(0, "Stemmer disabled"); }
            return;
        }
        _ => {
            unsafe { writer.write_str(0, &format!("Unknown language: {}. Use 'none' to disable.", lang)) };
            return;
        }
    };
    if let Ok(mut bm) = crate::bm25::get_bm25().lock() {
        bm.set_stemmer(rust_stemmers::Stemmer::create(algorithm));
        unsafe { writer.write_str(0, &format!("Stemmer set to {}", lang)); }
    }
}

// ─── BM25 search state ──────────────────────────────────────────────────

#[derive(Default)]
struct Bm25SearchState {
    results: Vec<(String, f32)>,
    cursor: usize,
}

// ─── Hybrid fusion state ────────────────────────────────────────────────

#[derive(Default)]
struct HybridState {
    results: Vec<(String, f32, f32, f32, f32)>, // name, dense, bm25, graph, fused
    cursor: usize,
}

// ─── Extension registration ─────────────────────────────────────────────

fn register(con: &Connection) -> Result<(), ExtensionError> {
    let raw_con = con.as_raw_connection();

    // ── Core scalars ──
    unsafe {
        ScalarFunctionBuilder::new("semantic_load")
            .param(TypeId::Varchar).returns(TypeId::Varchar)
            .function(semantic_load_fn).register(raw_con)?;
    }
    unsafe {
        ScalarFunctionBuilder::new("semantic_dry_plan")
            .param(TypeId::Varchar).returns(TypeId::Varchar)
            .function(semantic_dry_plan_fn).register(raw_con)?;
    }

    // ── L1: Vector scalars ──
    unsafe {
        ScalarFunctionBuilder::new("semantic_index_model")
            .param(TypeId::Varchar).param(TypeId::Varchar).returns(TypeId::Varchar)
            .function(semantic_index_model_fn).register(raw_con)?;
    }

    // ── L1: Vector search table ──
    let vs_tf = TableFunctionBuilder::new("semantic_vector_search")
        .param(TypeId::Varchar).param(TypeId::Integer)
        .with_state::<VectorSearchState, _>(|bind| {
            bind.add_result_column("model_name", TypeId::Varchar);
            bind.add_result_column("score", TypeId::Float);

            if bind.parameter_count() >= 2 {
                let query_str = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default();
                let k_val = unsafe { bind.get_parameter_value(1) };
                let k: usize = k_val.as_i64_or(5) as usize;

                match vectors::parse_vec(&query_str) {
                    Ok(qvec) => {
                        let store = vectors::get_vector_store().lock().map_err(|_e| "lock")?;
                        Ok(VectorSearchState { results: store.search(&qvec, k), cursor: 0 })
                    }
                    Err(_e) => {
                        Ok(VectorSearchState { results: vec![], cursor: 0 })
                    }
                }
            } else {
                Ok(VectorSearchState::default())
            }
        })
        .scan(|state, chunk| {
            if state.cursor >= state.results.len() {
                unsafe { chunk.set_size(0) };
                return Ok(());
            }
            let (name, score) = &state.results[state.cursor];
            unsafe {
                chunk.writer(0).write_str(0, name);
                chunk.writer(1).write_f32(0, *score);
                chunk.set_size(1);
            }
            state.cursor += 1;
            Ok(())
        })
        .build()?;
    unsafe { let _ = con.register_table(vs_tf); }

    // ── L2: Graph scalars ──
    unsafe {
        ScalarFunctionBuilder::new("semantic_graph_reset")
            .returns(TypeId::Varchar)
            .function(semantic_graph_reset_fn).register(raw_con)?;
    }
    unsafe {
        ScalarFunctionBuilder::new("semantic_graph_add_edge")
            .param(TypeId::Varchar).param(TypeId::Varchar).param(TypeId::Varchar)
            .returns(TypeId::Varchar)
            .function(semantic_graph_add_edge_fn).register(raw_con)?;
    }

    // ── L2: discover_relationships table ──
    let disc_tf = TableFunctionBuilder::new("semantic_discover_relationships")
        .param(TypeId::Varchar)
        .with_state::<DiscoverState, _>(|bind| {
            bind.add_result_column("target_model", TypeId::Varchar);
            bind.add_result_column("distance", TypeId::Integer);
            bind.add_result_column("join_condition", TypeId::Varchar);

            if bind.parameter_count() >= 1 {
                let model = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default();
                let g = graph::get_graph().lock().map_err(|e| e.to_string())?;
                let rows = g.discover(&model);
                Ok(DiscoverState { rows, cursor: 0 })
            } else {
                Ok(DiscoverState::default())
            }
        })
        .scan(|state, chunk| {
            if state.cursor >= state.rows.len() {
                unsafe { chunk.set_size(0) };
                return Ok(());
            }
            let (name, dist, cond) = &state.rows[state.cursor];
            unsafe {
                chunk.writer(0).write_str(0, name);
                chunk.writer(1).write_i64(0, *dist as i64);
                chunk.writer(2).write_str(0, cond);
                chunk.set_size(1);
            }
            state.cursor += 1;
            Ok(())
        })
        .build()?;
    unsafe { let _ = con.register_table(disc_tf); }

    // ── L2: shortest_path table ──
    let path_tf = TableFunctionBuilder::new("semantic_shortest_path")
        .param(TypeId::Varchar).param(TypeId::Varchar)
        .with_state::<PathState, _>(|bind| {
            bind.add_result_column("edge", TypeId::Varchar);
            bind.add_result_column("join_condition", TypeId::Varchar);

            if bind.parameter_count() >= 2 {
                let from = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default();
                let to = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default();
                let g = graph::get_graph().lock().map_err(|e| e.to_string())?;
                let steps = g.shortest_path(&from, &to);
                Ok(PathState { steps, cursor: 0 })
            } else {
                Ok(PathState::default())
            }
        })
        .scan(|state, chunk| {
            if state.cursor >= state.steps.len() {
                unsafe { chunk.set_size(0) };
                return Ok(());
            }
            let (edge, cond) = &state.steps[state.cursor];
            unsafe {
                chunk.writer(0).write_str(0, edge);
                chunk.writer(1).write_str(0, cond);
                chunk.set_size(1);
            }
            state.cursor += 1;
            Ok(())
        })
        .build()?;
    unsafe { let _ = con.register_table(path_tf); }

    // ── L3: Ontology scalars ──
    unsafe {
        ScalarFunctionBuilder::new("semantic_class_define")
            .param(TypeId::Varchar).param(TypeId::Varchar).returns(TypeId::Varchar)
            .function(semantic_class_define_fn).register(raw_con)?;
    }
    unsafe {
        ScalarFunctionBuilder::new("semantic_class_map")
            .param(TypeId::Varchar).param(TypeId::Varchar).param(TypeId::Varchar).returns(TypeId::Varchar)
            .function(semantic_class_map_fn).register(raw_con)?;
    }
    unsafe {
        ScalarFunctionBuilder::new("semantic_property_define")
            .param(TypeId::Varchar).param(TypeId::Varchar).param(TypeId::Varchar).param(TypeId::Varchar)
            .returns(TypeId::Varchar)
            .function(semantic_property_define_fn).register(raw_con)?;
    }
    unsafe {
        ScalarFunctionBuilder::new("semantic_ontology_export")
            .param(TypeId::Varchar).returns(TypeId::Varchar)
            .function(semantic_ontology_export_fn).register(raw_con)?;
    }

    // ── L3: semantic_class_query(class_name) → table ──
    let cq_tf = TableFunctionBuilder::new("semantic_class_query")
        .param(TypeId::Varchar)
        .with_state::<ClassQueryState, _>(|bind| {
            bind.add_result_column("expanded_sql", TypeId::Varchar);
            if bind.parameter_count() >= 1 {
                let class = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default();
                let o = ontology::get_ontology().lock().map_err(|_e| "lock")?;
                match o.class_query_sql(&class) {
                    Ok(sql) => Ok(ClassQueryState { sql, done: false }),
                    Err(e) => Ok(ClassQueryState { sql: format!("Error: {}", e), done: false }),
                }
            } else {
                Ok(ClassQueryState::default())
            }
        })
        .scan(|state, chunk| {
            if state.done { unsafe { chunk.set_size(0) }; return Ok(()); }
            unsafe { chunk.writer(0).write_str(0, &state.sql); chunk.set_size(1); }
            state.done = true;
            Ok(())
        }).build()?;
    unsafe { let _ = con.register_table(cq_tf); }

    // ── L3: semantic_class_inheritance(class_name) → table ──
    let inh_tf = TableFunctionBuilder::new("semantic_class_inheritance")
        .param(TypeId::Varchar)
        .with_state::<InheritanceState, _>(|bind| {
            bind.add_result_column("class", TypeId::Varchar);
            bind.add_result_column("depth", TypeId::Integer);
            bind.add_result_column("kind", TypeId::Varchar);
            bind.add_result_column("detail", TypeId::Varchar);
            if bind.parameter_count() >= 1 {
                let class = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default();
                let o = ontology::get_ontology().lock().map_err(|_e| "lock")?;
                let mut rows = Vec::new();
                // Self
                rows.push((class.clone(), 0, "self".into(), "—".into()));
                // Ancestors
                for (depth, a) in o.ancestors(&class).iter().enumerate() {
                    rows.push((a.clone(), (depth + 1) as i32, "is-a".into(), format!("← {}", class)));
                }
                // Descendants
                for d in o.descendants(&class) {
                    rows.push((d, -1, "subclass".into(), format!("{} →", class)));
                }
                // Inherited properties
                for p in o.inherited_properties(&class) {
                    rows.push((p.name, 0, "property".into(), format!("{} → {}", p.domain, p.range)));
                }
                // Inherited mapping
                if let Some(m) = o.inherited_mapping(&class) {
                    let filter = m.filter.unwrap_or_default();
                    rows.push((m.class_name, 0, "mapping".into(), format!("→ {} WHERE {}", m.model_name, filter)));
                }
                Ok(InheritanceState { rows, cursor: 0 })
            } else {
                Ok(InheritanceState::default())
            }
        })
        .scan(|state, chunk| {
            if state.cursor >= state.rows.len() { unsafe { chunk.set_size(0) }; return Ok(()); }
            let (cls, dep, kind, det) = &state.rows[state.cursor];
            unsafe {
                chunk.writer(0).write_str(0, cls); chunk.writer(1).write_i64(0, *dep as i64);
                chunk.writer(2).write_str(0, kind); chunk.writer(3).write_str(0, det);
                chunk.set_size(1);
            }
            state.cursor += 1;
            Ok(())
        }).build()?;
    unsafe { let _ = con.register_table(inh_tf); }

    // ── L4: Process Context scalars ──
    unsafe {
        ScalarFunctionBuilder::new("semantic_pattern_add")
            .param(TypeId::Varchar).param(TypeId::Varchar)
            .param(TypeId::Varchar).param(TypeId::Varchar)
            .returns(TypeId::Varchar)
            .function(semantic_pattern_add_fn).register(raw_con)?;
    }

    // ── L4: semantic_process_context(model_name) → table ──
    let pc_tf = TableFunctionBuilder::new("semantic_process_context")
        .param(TypeId::Varchar)
        .with_state::<ProcessCtxState, _>(|bind| {
            bind.add_result_column("kind", TypeId::Varchar);
            bind.add_result_column("key", TypeId::Varchar);
            bind.add_result_column("value", TypeId::Varchar);
            if bind.parameter_count() >= 1 {
                let model = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default();
                let store = process::get_store().lock().map_err(|_e| "lock")?;
                let mut rows = Vec::new();

                // Hub detection: check graph connectivity
                if let Ok(g) = graph::get_graph().lock() {
                    let related = g.discover(&model);
                    let degree = related.len();
                    let is_hub = degree >= 3;
                    rows.push(("hub".into(), model.clone(), format!("degree={}, is_hub={}", degree, is_hub)));
                }

                // Pattern matches
                for p in store.patterns_for(&model) {
                    let steps = p.node_ids().join(" → ");
                    rows.push(("pattern".into(), p.name.clone(), steps));
                    // Co-occurring models
                    for s in &p.steps {
                        if s.model_name != model {
                            rows.push(("co_occurring".into(), model.clone(), s.model_name.clone()));
                        }
                    }
                }

                if rows.is_empty() {
                    rows.push(("info".into(), "no_context".into(), "No patterns or graph edges found".into()));
                }
                Ok(ProcessCtxState { rows, cursor: 0 })
            } else {
                Ok(ProcessCtxState::default())
            }
        })
        .scan(|state, chunk| {
            if state.cursor >= state.rows.len() { unsafe { chunk.set_size(0) }; return Ok(()); }
            let (k, key, val) = &state.rows[state.cursor];
            unsafe {
                chunk.writer(0).write_str(0, k); chunk.writer(1).write_str(0, key);
                chunk.writer(2).write_str(0, val); chunk.set_size(1);
            }
            state.cursor += 1; Ok(())
        }).build()?;
    unsafe { let _ = con.register_table(pc_tf); }

    // ── L4: semantic_pattern_search(query, k) → table ──
    let ps_tf = TableFunctionBuilder::new("semantic_pattern_search")
        .param(TypeId::Varchar).param(TypeId::Integer)
        .with_state::<PatternSearchState, _>(|bind| {
            bind.add_result_column("name", TypeId::Varchar);
            bind.add_result_column("domain", TypeId::Varchar);
            bind.add_result_column("steps", TypeId::Varchar);
            bind.add_result_column("description", TypeId::Varchar);
            if bind.parameter_count() >= 2 {
                let query = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default();
                let k_val = unsafe { bind.get_parameter_value(1) };
                let k = k_val.as_i64_or(5) as usize;
                let store = process::get_store().lock().map_err(|_e| "lock")?;
                let matches = store.search(&query, k);
                let rows: Vec<_> = matches.iter().map(|p| {
                    (p.name.clone(), p.domain.clone(), p.node_ids().join(","), p.description.clone())
                }).collect();
                Ok(PatternSearchState { rows, cursor: 0 })
            } else {
                Ok(PatternSearchState::default())
            }
        })
        .scan(|state, chunk| {
            if state.cursor >= state.rows.len() { unsafe { chunk.set_size(0) }; return Ok(()); }
            let (n, d, s, desc) = &state.rows[state.cursor];
            unsafe {
                chunk.writer(0).write_str(0, n); chunk.writer(1).write_str(0, d);
                chunk.writer(2).write_str(0, s); chunk.writer(3).write_str(0, desc);
                chunk.set_size(1);
            }
            state.cursor += 1; Ok(())
        }).build()?;
    unsafe { let _ = con.register_table(ps_tf); }

    // ── L4: semantic_discover_patterns() → table ──
    let dp_tf = TableFunctionBuilder::new("semantic_discover_patterns")
        .with_state::<DiscoverPatternsState, _>(|bind| {
            bind.add_result_column("path", TypeId::Varchar);
            bind.add_result_column("frequency", TypeId::Integer);
            bind.add_result_column("type", TypeId::Varchar);
            let mut rows = Vec::new();
            // Mine paths from FK graph
            if let Ok(g) = graph::get_graph().lock() {
                // Build edge list from graph concepts
                let edges: Vec<(String, String)> = g.edges().into_iter().map(|(a, b, _)| (a, b)).collect();
                let paths = process::PatternDiscovery::frequent_paths(&edges, &g, 3, 10);
                for (path, freq) in paths {
                    rows.push((path.join(" → "), freq, "frequent-path".into()));
                }
            }
            Ok(DiscoverPatternsState { rows, cursor: 0 })
        })
        .scan(|state, chunk| {
            if state.cursor >= state.rows.len() { unsafe { chunk.set_size(0) }; return Ok(()); }
            let (p, f, t) = &state.rows[state.cursor];
            unsafe {
                chunk.writer(0).write_str(0, p); chunk.writer(1).write_i64(0, *f as i64);
                chunk.writer(2).write_str(0, t); chunk.set_size(1);
            }
            state.cursor += 1; Ok(())
        }).build()?;
    unsafe { let _ = con.register_table(dp_tf); }

    // ── P0: Persistence scalars ──
    unsafe {
        ScalarFunctionBuilder::new("semantic_save")
            .param(TypeId::Varchar).returns(TypeId::Varchar)
            .function(semantic_save_fn).register(raw_con)?;
    }
    unsafe {
        ScalarFunctionBuilder::new("semantic_restore")
            .param(TypeId::Varchar).returns(TypeId::Varchar)
            .function(semantic_restore_fn).register(raw_con)?;
    }

    // ── BM25 scalars ──
    unsafe {
        ScalarFunctionBuilder::new("semantic_bm25_index_doc")
            .param(TypeId::Varchar).param(TypeId::Varchar).returns(TypeId::Varchar)
            .function(semantic_bm25_index_doc_fn).register(raw_con)?;
    }
    unsafe {
        ScalarFunctionBuilder::new("semantic_bm25_remove_doc")
            .param(TypeId::Varchar).returns(TypeId::Varchar)
            .function(semantic_bm25_remove_doc_fn).register(raw_con)?;
    }
    unsafe {
        ScalarFunctionBuilder::new("semantic_bm25_reset")
            .returns(TypeId::Varchar)
            .function(semantic_bm25_reset_fn).register(raw_con)?;
    }
    unsafe {
        ScalarFunctionBuilder::new("semantic_bm25_stemmer")
            .param(TypeId::Varchar).returns(TypeId::Varchar)
            .function(semantic_bm25_stemmer_fn).register(raw_con)?;
    }

    // ── BM25: semantic_bm25_search(query, k) → table ──
    let bm_tf = TableFunctionBuilder::new("semantic_bm25_search")
        .param(TypeId::Varchar).param(TypeId::Integer)
        .with_state::<Bm25SearchState, _>(|bind| {
            bind.add_result_column("doc_id", TypeId::Varchar);
            bind.add_result_column("bm25_score", TypeId::Float);
            if bind.parameter_count() >= 2 {
                let query = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default();
                let k_val = unsafe { bind.get_parameter_value(1) };
                let k = k_val.as_i64_or(5) as usize;
                let bm = crate::bm25::get_bm25().lock().map_err(|_e| "lock")?;
                Ok(Bm25SearchState { results: bm.search(&query, k), cursor: 0 })
            } else {
                Ok(Bm25SearchState::default())
            }
        })
        .scan(|state, chunk| {
            if state.cursor >= state.results.len() { unsafe { chunk.set_size(0) }; return Ok(()); }
            let (id, score) = &state.results[state.cursor];
            unsafe {
                chunk.writer(0).write_str(0, id);
                chunk.writer(1).write_f32(0, *score);
                chunk.set_size(1);
            }
            state.cursor += 1; Ok(())
        }).build()?;
    unsafe { let _ = con.register_table(bm_tf); }

    // ── P0: semantic_hybrid_search(query_vec, k, dw?, gw?, bq?, bw?, hub?) → table ──
    let hy_tf = TableFunctionBuilder::new("semantic_hybrid_search")
        .param(TypeId::Varchar).param(TypeId::Integer)
        .param(TypeId::Float).param(TypeId::Float)
        .param(TypeId::Varchar).param(TypeId::Float)
        .param(TypeId::Varchar)
        .with_state::<HybridState, _>(|bind| {
            bind.add_result_column("model_name", TypeId::Varchar);
            bind.add_result_column("dense_score", TypeId::Float);
            bind.add_result_column("bm25_score", TypeId::Float);
            bind.add_result_column("graph_score", TypeId::Float);
            bind.add_result_column("fused_score", TypeId::Float);
            if bind.parameter_count() >= 2 {
                let qv = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default();
                let k_val = unsafe { bind.get_parameter_value(1) };
                let k = k_val.as_i64_or(5) as usize;
                let dw = if bind.parameter_count() >= 3 {
                    unsafe { bind.get_parameter_value(2) }.as_f64_or(0.50) as f32
                } else { 0.50 };
                let gw = if bind.parameter_count() >= 4 {
                    unsafe { bind.get_parameter_value(3) }.as_f64_or(0.20) as f32
                } else { 0.20 };
                let bq = if bind.parameter_count() >= 5 {
                    let s = unsafe { bind.get_parameter_value(4) }.as_str().unwrap_or_default();
                    if s.is_empty() { None } else { Some(s) }
                } else { None };
                let bw = if bind.parameter_count() >= 6 {
                    unsafe { bind.get_parameter_value(5) }.as_f64_or(0.30) as f32
                } else { 0.30 };
                let hub = if bind.parameter_count() >= 7 {
                    let s = unsafe { bind.get_parameter_value(6) }.as_str().unwrap_or_default();
                    if s.is_empty() { None } else { Some(s) }
                } else { None };
                match vectors::parse_vec(&qv) {
                    Ok(qvec) => {
                        let results: Vec<_> = fusion::hybrid_search(
                            &qvec, k, dw, bw, gw, bq.as_deref(), hub.as_deref(),
                        )
                        .into_iter()
                        .map(|r| (r.model_name, r.dense_score, r.bm25_score, r.graph_score, r.fused_score))
                        .collect();
                        Ok(HybridState { results, cursor: 0 })
                    }
                    Err(_) => Ok(HybridState::default()),
                }
            } else { Ok(HybridState::default()) }
        })
        .scan(|state, chunk| {
            if state.cursor >= state.results.len() { unsafe { chunk.set_size(0) }; return Ok(()); }
            let (n, ds, bs, gs, fs) = &state.results[state.cursor];
            unsafe {
                chunk.writer(0).write_str(0, n); chunk.writer(1).write_f32(0, *ds);
                chunk.writer(2).write_f32(0, *bs); chunk.writer(3).write_f32(0, *gs);
                chunk.writer(4).write_f32(0, *fs);
                chunk.set_size(1);
            }
            state.cursor += 1; Ok(())
        }).build()?;
    unsafe { let _ = con.register_table(hy_tf); }

    // ── Core tables: semantic_query + semantic_models ──
    let query_tf = TableFunctionBuilder::new("semantic_query")
        .param(TypeId::Varchar)
        .with_state::<QueryState, _>(|bind| {
            bind.add_result_column("expanded_sql", TypeId::Varchar);
            if bind.parameter_count() >= 1 {
                let sql_text = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default();
                if let Ok(guard) = get_ctx().lock() {
                    if let Some(ctx) = guard.as_ref() {
                        match planner::expand_sql(&sql_text, ctx) {
                            Ok(e) => return Ok(QueryState { expanded_sql: e, done: false }),
                            Err(e) => return Ok(QueryState { expanded_sql: format!("Error: {}", e), done: false }),
                        }
                    }
                }
            }
            Ok(QueryState { expanded_sql: "Error: No MDL loaded".into(), done: false })
        })
        .scan(|state, chunk| {
            if state.done { unsafe { chunk.set_size(0) }; return Ok(()); }
            unsafe { chunk.writer(0).write_str(0, &state.expanded_sql); chunk.set_size(1); }
            state.done = true;
            Ok(())
        }).build()?;
    unsafe { let _ = con.register_table(query_tf); }

    let models_tf = TableFunctionBuilder::new("semantic_models")
        .with_state::<ModelsState, _>(|bind| {
            bind.add_result_column("name", TypeId::Varchar);
            bind.add_result_column("catalog", TypeId::Varchar);
            bind.add_result_column("schema_name", TypeId::Varchar);
            bind.add_result_column("table_name", TypeId::Varchar);
            bind.add_result_column("column_count", TypeId::Integer);
            let models = get_ctx().lock().ok().and_then(|g| g.clone()).map(|ctx| {
                ctx.models.iter().map(|m| {
                    let (cat, sch, tbl) = m.table_reference.as_ref().map(|tr| {
                        (tr.catalog.clone().unwrap_or_default(), tr.schema.clone().unwrap_or_default(), tr.table.clone())
                    }).unwrap_or_default();
                    (m.name.clone(), cat, sch, tbl, m.columns.len() as i64)
                }).collect()
            }).unwrap_or_default();
            Ok(ModelsState { models, cursor: 0 })
        })
        .scan(|state, chunk| {
            if state.cursor >= state.models.len() { unsafe { chunk.set_size(0) }; return Ok(()); }
            let (n, c, s, t, cc) = &state.models[state.cursor];
            unsafe {
                chunk.writer(0).write_str(0, n); chunk.writer(1).write_str(0, c);
                chunk.writer(2).write_str(0, s); chunk.writer(3).write_str(0, t);
                chunk.writer(4).write_i64(0, *cc); chunk.set_size(1);
            }
            state.cursor += 1;
            Ok(())
        }).build()?;
    unsafe { let _ = con.register_table(models_tf); }

    Ok(())
}

entry_point_v2!(semantic_init_c_api, |con| register(con));
