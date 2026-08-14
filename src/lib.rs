//! duckdb_dq — data quality assertion framework for DuckDB.
//!
//! Core functions:
//!   expect_not_null(table, col)          → table : null-check assertion result
//!   expect_unique(table, col)            → table : uniqueness assertion result
//!   expect_in_range(table, col, lo, hi)  → table : range assertion result
//!   expect_row_count_between(t, lo, hi)  → table : row-count assertion result
//!   profile_table(table)                 → table : per-column profiling (SUMMARIZE)
//!   validate_expectations(table, json)   → table : batch assertions from a JSON rule set
//!   dq_run(name, table, json)            → scalar: run rule set + persist report row
//!   dq_reports()                         → table : persisted report history
//!
//! All assertions compile to SQL and execute on a persistent secondary
//! connection, so DuckDB's vectorized engine does the counting — zero
//! per-row Rust work.

use libduckdb_sys::{duckdb_data_chunk, duckdb_function_info, duckdb_vector};
use quack_rs::connection::Connection;
use quack_rs::entry_point_v2;
use quack_rs::prelude::*;
use quack_rs::scalar::ScalarFunctionBuilder;
use quack_rs::table::{BindInfo, TableFunctionBuilder};
use quack_rs::types::TypeId;
use quack_rs::vector::{VectorReader, VectorWriter};


mod engine;
mod validate;

use engine::run_query_rows;
use validate::{AssertionResult, count_rows};

// ─── Assertion table result columns ─────────────────────────────────────
// rule, table_name, column_name, passed, row_count, failed_count, error

fn add_assertion_columns(bind: &BindInfo) {
    bind.add_result_column("rule", TypeId::Varchar)
        .add_result_column("table_name", TypeId::Varchar)
        .add_result_column("column_name", TypeId::Varchar)
        .add_result_column("passed", TypeId::Boolean)
        .add_result_column("row_count", TypeId::BigInt)
        .add_result_column("failed_count", TypeId::BigInt)
        .add_result_column("error", TypeId::Varchar);
}

#[derive(Default)]
struct AssertionState {
    results: Vec<AssertionResult>,
    cursor: usize,
}

fn write_assertion(state: &mut AssertionState, chunk: &DataChunk) -> Result<(), ExtensionError> {
    if state.cursor >= state.results.len() {
        unsafe { chunk.set_size(0) };
        return Ok(());
    }
    let r = &state.results[state.cursor];
    unsafe {
        chunk.writer(0).write_str(0, &r.rule);
        chunk.writer(1).write_str(0, &r.table);
        chunk.writer(2).write_str(0, &r.column);
        chunk.writer(3).write_bool(0, r.passed);
        chunk.writer(4).write_i64(0, r.row_count);
        chunk.writer(5).write_i64(0, r.failed_count);
        chunk.writer(6).write_str(0, &r.error);
        chunk.set_size(1);
    }
    state.cursor += 1;
    Ok(())
}

// ─── expect_not_null(table, col) ────────────────────────────────────────

fn expect_not_null_bind(bind: &BindInfo, table: &str, column: &str) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let (total, matched, err) = count_rows(table, Some(&format!("{} IS NULL", column)));
    let (row_count, failed) = if err.is_empty() { (total, matched) } else { (0, 0) };
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_not_null".into(),
            table: table.into(),
            column: column.into(),
            passed: err.is_empty() && failed == 0,
            row_count,
            failed_count: failed,
            error: err,
        }],
        cursor: 0,
    })
}

// ─── expect_unique(table, col) ──────────────────────────────────────────

fn expect_unique_bind(bind: &BindInfo, table: &str, column: &str) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let sql = format!("SELECT COUNT(*), COUNT(DISTINCT {}) FROM {}", column, table);
    let (total, err1) = match run_query_rows(&sql) {
        Ok(rows) if !rows.is_empty() && rows[0].len() >= 2 => (
            rows[0][0].parse::<i64>().unwrap_or(-1),
            String::new(),
        ),
        Ok(_) => (0, format!("no rows returned for {}", sql)),
        Err(e) => (0, e.to_string()),
    };
    let dupes = if err1.is_empty() {
        match run_query_rows(&format!("SELECT COUNT(DISTINCT {}) FROM {}", column, table)) {
            Ok(rows) if !rows.is_empty() && !rows[0].is_empty() => {
                total - rows[0][0].parse::<i64>().unwrap_or(-1)
            }
            _ => -1,
        }
    } else {
        0
    };
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_unique".into(),
            table: table.into(),
            column: column.into(),
            passed: err1.is_empty() && dupes == 0,
            row_count: total,
            failed_count: dupes,
            error: err1,
        }],
        cursor: 0,
    })
}

// ─── expect_in_range(table, col, lo, hi) ────────────────────────────────

fn expect_in_range_bind(
    bind: &BindInfo,
    table: &str,
    column: &str,
    lo: f64,
    hi: f64,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let cond = format!("{} IS NULL OR {} < {} OR {} > {}", column, column, lo, column, hi);
    let (total, matched, err) = count_rows(table, Some(&cond));
    let (row_count, failed) = if err.is_empty() { (total, matched) } else { (0, 0) };
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_in_range".into(),
            table: table.into(),
            column: column.into(),
            passed: err.is_empty() && failed == 0,
            row_count,
            failed_count: failed,
            error: err,
        }],
        cursor: 0,
    })
}

// ─── expect_row_count_between(table, lo, hi) ────────────────────────────

fn expect_row_count_between_bind(
    bind: &BindInfo,
    table: &str,
    lo: i64,
    hi: i64,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let (total, _, err) = count_rows(table, None);
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_row_count_between".into(),
            table: table.into(),
            column: String::new(),
            passed: err.is_empty() && total >= lo && total <= hi,
            row_count: total,
            failed_count: if err.is_empty() && (total < lo || total > hi) { 1 } else { 0 },
            error: err,
        }],
        cursor: 0,
    })
}

// ─── profile_table(table) → SUMMARIZE ───────────────────────────────────

#[derive(Default)]
struct ProfileState {
    rows: Vec<(String, String, String, String, String, String, String)>,
    cursor: usize,
}

fn profile_table_bind(bind: &BindInfo, table: &str) -> Result<ProfileState, ExtensionError> {
    bind.add_result_column("column_name", TypeId::Varchar)
        .add_result_column("column_type", TypeId::Varchar)
        .add_result_column("count", TypeId::Varchar)
        .add_result_column("null_pct", TypeId::Varchar)
        .add_result_column("distinct_count", TypeId::Varchar)
        .add_result_column("min", TypeId::Varchar)
        .add_result_column("max", TypeId::Varchar);

    let mut rows = Vec::new();
    match run_query_rows(&format!("SUMMARIZE SELECT * FROM {}", table)) {
        Ok(out) => {
            // SUMMARIZE output columns (positional):
            // [0] column_name [1] column_type [2] min [3] max [4] approx_unique
            // [5] avg [6] std [7] q25 [8] q50 [9] q75 [10] count [11] null_percentage
            for row in out {
                let get = |i: usize| row.get(i).cloned().unwrap_or_default();
                rows.push((get(0), get(1), get(10), get(11), get(4), get(2), get(3)));
            }
        }
        Err(e) => {
            rows.push((
                format!("ERROR: {}", e),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ));
        }
    }
    Ok(ProfileState { rows, cursor: 0 })
}

fn profile_scan(state: &mut ProfileState, chunk: &DataChunk) -> Result<(), ExtensionError> {
    if state.cursor >= state.rows.len() {
        unsafe { chunk.set_size(0) };
        return Ok(());
    }
    let (n, t, c, np, d, mn, mx) = &state.rows[state.cursor];
    unsafe {
        chunk.writer(0).write_str(0, n);
        chunk.writer(1).write_str(0, t);
        chunk.writer(2).write_str(0, c);
        chunk.writer(3).write_str(0, np);
        chunk.writer(4).write_str(0, d);
        chunk.writer(5).write_str(0, mn);
        chunk.writer(6).write_str(0, mx);
        chunk.set_size(1);
    }
    state.cursor += 1;
    Ok(())
}

// ─── validate_expectations(table, json) ─────────────────────────────────

fn validate_expectations_bind(
    bind: &BindInfo,
    table: &str,
    rules_json: &str,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    Ok(AssertionState {
        results: validate::run_rules(table, rules_json),
        cursor: 0,
    })
}

// ─── dq_run(name, table, json) → scalar, persists report ───────────────

unsafe extern "C" fn dq_run_fn(
    _info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    // One reader per parameter vector — read_str(i) indexes ROWS within a
    // single vector, NOT across parameter vectors.
    let reader0 = unsafe { VectorReader::new(input, 0) };
    let reader1 = unsafe { VectorReader::new(input, 1) };
    let reader2 = unsafe { VectorReader::new(input, 2) };
    let mut writer = unsafe { VectorWriter::new(output) };
    if reader0.row_count() == 0 || unsafe { !reader0.is_valid(0) } {
        unsafe { writer.write_str(0, "Error: no input") };
        return;
    }
    let name = unsafe { reader0.read_str(0).to_string() };
    let table = unsafe { reader1.read_str(0).to_string() };
    let rules_json = unsafe { reader2.read_str(0).to_string() };

    let results = validate::run_rules(&table, &rules_json);
    let passed = results.iter().filter(|r| r.error.is_empty() && r.passed).count();
    let failed = results.iter().filter(|r| r.error.is_empty() && !r.passed).count();
    let errors = results.iter().filter(|r| !r.error.is_empty()).count();
    let total = results.len();
    let passed_all = failed == 0 && errors == 0;

    let summary = serde_json::json!({
        "total": total,
        "passed": passed,
        "failed": failed,
        "errors": errors,
    });
    let esc = |s: &str| s.replace('\'', "''");
    let insert_sql = format!(
        "CREATE TABLE IF NOT EXISTS dq_reports(name VARCHAR, table_name VARCHAR, rules VARCHAR, summary VARCHAR, passed BOOLEAN, run_at TIMESTAMP DEFAULT now()); \
         INSERT INTO dq_reports(name, table_name, rules, summary, passed) VALUES ('{}', '{}', '{}', '{}', {})",
        esc(&name),
        esc(&table),
        esc(&rules_json),
        esc(&summary.to_string()),
        if passed_all { "true" } else { "false" },
    );
    let msg = match engine::run_exec(&insert_sql) {
        Ok(()) => format!(
            "dq_run '{}': {}/{} passed, {} failed, {} errors",
            name, passed, total, failed, errors
        ),
        Err(e) => format!("dq_run '{}' failed: {}", name, e),
    };
    unsafe { writer.write_str(0, &msg) };
}

// ─── dq_reports() → table ───────────────────────────────────────────────

#[derive(Default)]
struct ReportsState {
    rows: Vec<(String, String, String, bool, String)>,
    cursor: usize,
}

fn dq_reports_bind(bind: &BindInfo) -> Result<ReportsState, ExtensionError> {
    bind.add_result_column("name", TypeId::Varchar)
        .add_result_column("table_name", TypeId::Varchar)
        .add_result_column("summary", TypeId::Varchar)
        .add_result_column("passed", TypeId::Boolean)
        .add_result_column("run_at", TypeId::Varchar);
    let mut rows = Vec::new();
    match run_query_rows("SELECT name, table_name, summary, passed, run_at::VARCHAR FROM dq_reports ORDER BY run_at DESC") {
        Ok(out) => {
            for row in out {
                let get = |i: usize| row.get(i).cloned().unwrap_or_default();
                rows.push((get(0), get(1), get(2), get(3).eq_ignore_ascii_case("true"), get(4)));
            }
        }
        Err(e) => {
            rows.push((format!("ERROR: {}", e), String::new(), String::new(), false, String::new()));
        }
    }
    Ok(ReportsState { rows, cursor: 0 })
}

fn reports_scan(state: &mut ReportsState, chunk: &DataChunk) -> Result<(), ExtensionError> {
    if state.cursor >= state.rows.len() {
        unsafe { chunk.set_size(0) };
        return Ok(());
    }
    let (n, t, s, p, at) = &state.rows[state.cursor];
    unsafe {
        chunk.writer(0).write_str(0, n);
        chunk.writer(1).write_str(0, t);
        chunk.writer(2).write_str(0, s);
        chunk.writer(3).write_bool(0, *p);
        chunk.writer(4).write_str(0, at);
        chunk.set_size(1);
    }
    state.cursor += 1;
    Ok(())
}

// ─── Registration ───────────────────────────────────────────────────────

fn register(con: &Connection) -> Result<(), ExtensionError> {
    let raw_con = con.as_raw_connection();

    // Persistent secondary connection for query execution
    engine::init_early(con);

    // expect_not_null(table, col)
    let tf = TableFunctionBuilder::new("expect_not_null")
        .param(TypeId::Varchar).param(TypeId::Varchar)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let column = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
            expect_not_null_bind(bind, &table, &column)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // expect_unique(table, col)
    let tf = TableFunctionBuilder::new("expect_unique")
        .param(TypeId::Varchar).param(TypeId::Varchar)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let column = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
            expect_unique_bind(bind, &table, &column)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // expect_in_range(table, col, lo, hi)
    let tf = TableFunctionBuilder::new("expect_in_range")
        .param(TypeId::Varchar).param(TypeId::Varchar)
        .param(TypeId::Double).param(TypeId::Double)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let column = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
            let lo = unsafe { bind.get_parameter_value(2) }.as_f64_or(f64::MIN);
            let hi = unsafe { bind.get_parameter_value(3) }.as_f64_or(f64::MAX);
            expect_in_range_bind(bind, &table, &column, lo, hi)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // expect_row_count_between(table, lo, hi)
    let tf = TableFunctionBuilder::new("expect_row_count_between")
        .param(TypeId::Varchar).param(TypeId::BigInt).param(TypeId::BigInt)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let lo = unsafe { bind.get_parameter_value(1) }.as_i64_or(0);
            let hi = unsafe { bind.get_parameter_value(2) }.as_i64_or(i64::MAX);
            expect_row_count_between_bind(bind, &table, lo, hi)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // profile_table(table)
    let tf = TableFunctionBuilder::new("profile_table")
        .param(TypeId::Varchar)
        .with_state::<ProfileState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            profile_table_bind(bind, &table)
        })
        .scan(profile_scan).build()?;
    unsafe { let _ = con.register_table(tf); }

    // validate_expectations(table, json)
    let tf = TableFunctionBuilder::new("validate_expectations")
        .param(TypeId::Varchar).param(TypeId::Varchar)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let rules = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
            validate_expectations_bind(bind, &table, &rules)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // dq_run(name, table, json) → scalar
    unsafe {
        ScalarFunctionBuilder::new("dq_run")
            .param(TypeId::Varchar).param(TypeId::Varchar).param(TypeId::Varchar)
            .returns(TypeId::Varchar)
            .function(dq_run_fn).register(raw_con)?;
    }

    // dq_reports() → table
    let tf = TableFunctionBuilder::new("dq_reports")
        .with_state::<ReportsState, _>(|bind| dq_reports_bind(bind))
        .scan(reports_scan).build()?;
    unsafe { let _ = con.register_table(tf); }

    Ok(())
}

entry_point_v2!(dq_init_c_api, |con| register(con));
