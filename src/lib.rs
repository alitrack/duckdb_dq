use duckdb::core::{DataChunkHandle, LogicalType, LogicalTypeId};
use duckdb::vtab::{
    duckdb_free, BindInfo, FunctionInfo, InitResult, TableFunction, TableFunctionInfo,
};
use duckdb::*;
use std::collections::HashMap;
use std::sync::Mutex;

mod mdl;
mod planner;

use mdl::{load_mdl_json, Model, SemanticContext};

/// Global semantic context — stores MDL state per DuckDB connection via extension state.
/// Each DuckDB connection gets its own SemanticContext stored in the extension's
/// file_system or through the `semantic_load` function's side effect.
struct SemanticState {
    ctx: Option<SemanticContext>,
}

// DuckDB extension registration entry point.
// Functions registered:
//   semantic_load(path_or_json)  → load MDL definitions
//   semantic_models()            → table function: list all loaded models
//   semantic_dry_plan(sql)       → expand modeled SQL (no execution)
//   semantic_query(sql)          → table function: expand + execute

#[no_mangle]
pub extern "C" fn semantic_init(db: &duckdb::Database) {
    // Register scalar and table functions
    let _ = db.register_scalar_function("semantic_load", semantic_load);
    let _ = db.register_table_function("semantic_models", semantic_models_tf());
    let _ = db.register_scalar_function("semantic_dry_plan", semantic_dry_plan);
    let _ = db.register_table_function("semantic_query", semantic_query_tf());
}

#[no_mangle]
pub extern "C" fn semantic_version() -> *const u8 {
    concat!("semantic v", env!("CARGO_PKG_VERSION"), "\0").as_ptr()
}

// ─── semantic_load(path_or_json) ────────────────────────────────────────

fn semantic_load(args: &[Varchar]) -> String {
    let input = args[0].as_str();
    match load_mdl_json(input) {
        Ok(ctx) => {
            // Store in thread-local or global state (MVP: parse, return model count)
            format!("Loaded {} models", ctx.models.len())
        }
        Err(e) => format!("Error: {}", e),
    }
}

// ─── semantic_models() → table function ─────────────────────────────────

fn semantic_models_tf() -> TableFunction {
    struct ModelsTF;
    impl TableFunctionInfo for ModelsTF {
        fn name(&self) -> &str {
            "semantic_models"
        }
        fn bind(&self, _bind: &BindInfo) -> InitResult {
            Ok(Box::new(()))
        }
        fn schema(&self) -> Vec<(String, LogicalType)> {
            vec![
                ("name".into(), LogicalType::Varchar),
                ("catalog".into(), LogicalType::Varchar),
                ("schema_name".into(), LogicalType::Varchar),
                ("table_name".into(), LogicalType::Varchar),
                ("column_count".into(), LogicalType::Integer),
            ]
        }
        fn scan(&self, _init: &dyn FunctionInfo, output: &mut DataChunkHandle) {
            // MVP: return empty. After state management, iterate ctx.models.
            output.set_len(0);
        }
    }
    TableFunction::new("semantic_models", ModelsTF)
}

// ─── semantic_dry_plan(sql) ────────────────────────────────────────────

fn semantic_dry_plan(args: &[Varchar]) -> String {
    let sql = args[0].as_str();
    match planner::expand_sql(sql) {
        Ok(expanded) => expanded,
        Err(e) => format!("Error: {}", e),
    }
}

// ─── semantic_query(sql) → table function ───────────────────────────────

fn semantic_query_tf() -> TableFunction {
    struct QueryTF;
    impl TableFunctionInfo for QueryTF {
        fn name(&self) -> &str {
            "semantic_query"
        }
        fn bind(&self, _bind: &BindInfo) -> InitResult {
            Ok(Box::new(()))
        }
        fn schema(&self) -> Vec<(String, LogicalType)> {
            // Dynamic schema — return placeholder
            vec![("result".into(), LogicalType::Varchar)]
        }
        fn scan(&self, _init: &dyn FunctionInfo, output: &mut DataChunkHandle) {
            output.set_len(0);
        }
    }
    TableFunction::new("semantic_query", QueryTF)
}
