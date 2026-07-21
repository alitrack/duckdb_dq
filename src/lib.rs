//! Semantic layer extension for DuckDB.
//!
//! Functions:
//!   semantic_load(json_or_path)  → load MDL definition
//!   semantic_dry_plan(sql)       → expand modeled SQL (no execution)
//!   semantic_models()            → table function: list loaded models available

use libduckdb_sys::{duckdb_connection, duckdb_data_chunk, duckdb_function_info, duckdb_vector};
use once_cell::sync::OnceCell;
use quack_rs::connection::Connection;
use quack_rs::{entry_point, entry_point_v2};
use quack_rs::prelude::*;
use quack_rs::scalar::ScalarFunctionBuilder;
use quack_rs::table::TableFunctionBuilder;
use quack_rs::types::TypeId;
use quack_rs::vector::{VectorReader, VectorWriter};
use std::sync::Mutex;

mod mdl;
mod planner;

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
            unsafe { writer.write_str(0, &format!("Loaded {} models", count)) };
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

// ─── semantic_models() → table function ─────────────────────────────────
// Uses TypedTableFunctionBuilder for closure-based bind/scan.

struct ModelsState {
    models: Vec<(
        String, // name
        String, // catalog
        String, // schema
        String, // table
        i64,    // column_count
    )>,
    cursor: usize,
}

// ─── Extension registration ─────────────────────────────────────────────

fn register(con: &Connection) -> Result<(), ExtensionError> {
    let raw_con = con.as_raw_connection();

    // Scalar: semantic_load(json_or_path)
    unsafe {
        ScalarFunctionBuilder::new("semantic_load")
            .param(TypeId::Varchar)
            .returns(TypeId::Varchar)
            .function(semantic_load_fn)
            .register(raw_con)?;
    }

    // Scalar: semantic_dry_plan(sql)
    unsafe {
        ScalarFunctionBuilder::new("semantic_dry_plan")
            .param(TypeId::Varchar)
            .returns(TypeId::Varchar)
            .function(semantic_dry_plan_fn)
            .register(raw_con)?;
    }

    // Table: semantic_models()
    let models_tf = TableFunctionBuilder::new("semantic_models")
        .with_state::<ModelsState, _>(|bind| {
            bind.add_result_column("name", TypeId::Varchar);
            bind.add_result_column("catalog", TypeId::Varchar);
            bind.add_result_column("schema_name", TypeId::Varchar);
            bind.add_result_column("table_name", TypeId::Varchar);
            bind.add_result_column("column_count", TypeId::Integer);

            let models: Vec<_> = get_ctx()
                .lock()
                .ok()
                .and_then(|g| g.clone())
                .map(|ctx| {
                    ctx.models
                        .iter()
                        .map(|m| {
                            let (cat, sch, tbl) = m
                                .table_reference
                                .as_ref()
                                .map(|tr| {
                                    (
                                        tr.catalog.clone().unwrap_or_default(),
                                        tr.schema.clone().unwrap_or_default(),
                                        tr.table.clone(),
                                    )
                                })
                                .unwrap_or_default();
                            (
                                m.name.clone(),
                                cat,
                                sch,
                                tbl,
                                m.columns.len() as i64,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();

            Ok(ModelsState {
                models,
                cursor: 0,
            })
        })
        .scan(|state, chunk| {
            if state.cursor >= state.models.len() {
                unsafe { chunk.set_size(0) };
                return Ok(());
            }

            let remaining = state.models.len() - state.cursor;
            let batch = remaining.min(2048);

            let (name, catalog, schema, table, col_count) =
                &state.models[state.cursor];
            unsafe {
                chunk.writer(0).write_str(0, name);
                chunk.writer(1).write_str(0, catalog);
                chunk.writer(2).write_str(0, schema);
                chunk.writer(3).write_str(0, table);
                chunk.writer(4).write_i64(0, *col_count);
                chunk.set_size(1);
            }
            state.cursor += 1;

            Ok(())
        })
        .build()?;
    unsafe { con.register_table(models_tf) };

    Ok(())
}

entry_point_v2!(semantic_init_c_api, |con| register(con));
