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

// ─── expect_accepted_values(table, col, 'a,b,c') ────────────────────────
// Fails on NULL or values outside the allowed set.

fn expect_accepted_values_bind(
    bind: &BindInfo,
    table: &str,
    column: &str,
    values_csv: &str,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let values: Vec<&str> = values_csv.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if values.is_empty() {
        return Ok(AssertionState {
            results: vec![AssertionResult {
                rule: "expect_accepted_values".into(),
                table: table.into(),
                column: column.into(),
                passed: false,
                row_count: 0,
                failed_count: 0,
                error: "values list is empty".into(),
            }],
            cursor: 0,
        });
    }
    let quoted: Vec<String> = values.iter().map(|v| format!("'{}'", v.replace('\'', "''"))).collect();
    let cond = format!("{} IS NULL OR {} NOT IN ({})", column, column, quoted.join(", "));
    let (total, matched, err) = count_rows(table, Some(&cond));
    let (row_count, failed) = if err.is_empty() { (total, matched) } else { (0, 0) };
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_accepted_values".into(),
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

// ─── expect_match_regex(table, col, pattern) ────────────────────────────
// Fails on NULL or values not matching the regex (DuckDB regexp_matches).

fn expect_match_regex_bind(
    bind: &BindInfo,
    table: &str,
    column: &str,
    pattern: &str,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let pat = pattern.replace('\'', "''");
    let cond = format!(
        "{} IS NULL OR NOT regexp_matches(CAST({} AS VARCHAR), '{}')",
        column, column, pat
    );
    let (total, matched, err) = count_rows(table, Some(&cond));
    let (row_count, failed) = if err.is_empty() { (total, matched) } else { (0, 0) };
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_match_regex".into(),
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

// ─── expect_relationship(table, col, to_table, to_col) ──────────────────
// Fails on values in col that have no matching key in to_table.to_col
// (orphan / broken foreign-key check).

fn expect_relationship_bind(
    bind: &BindInfo,
    table: &str,
    column: &str,
    to_table: &str,
    to_col: &str,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let cond = format!(
        "{} IS NOT NULL AND {} NOT IN (SELECT {} FROM {})",
        column, column, to_col, to_table
    );
    let (total, matched, err) = count_rows(table, Some(&cond));
    let (row_count, failed) = if err.is_empty() { (total, matched) } else { (0, 0) };
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_relationship".into(),
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

// ─── Statistical metric assertions ──────────────────────────────────────
// expect_min_between / max / mean / stddev / sum / distinct_count:
// compile to SELECT COUNT(*), {agg}(col) FROM table, then compare the
// aggregate against [lo, hi]. NULL aggregate (empty table) → fail.

fn metric_between_bind(
    bind: &BindInfo,
    rule: &str,
    table: &str,
    column: &str,
    metric_expr: &str,
    lo: f64,
    hi: f64,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let sql = format!("SELECT COUNT(*), {} FROM {}", metric_expr, table);
    let (total, val, err) = match run_query_rows(&sql) {
        Ok(rows) if !rows.is_empty() && rows[0].len() >= 2 => {
            let total = rows[0][0].parse::<i64>().unwrap_or(-1);
            let v = rows[0][1].parse::<f64>().unwrap_or(f64::NAN);
            (total, v, String::new())
        }
        Ok(_) => (0, f64::NAN, "no rows returned".into()),
        Err(e) => (0, f64::NAN, e.to_string()),
    };
    let passed = err.is_empty() && !val.is_nan() && val >= lo && val <= hi;
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: rule.into(),
            table: table.into(),
            column: column.into(),
            passed,
            row_count: total,
            failed_count: if err.is_empty() && !passed { 1 } else { 0 },
            error: err,
        }],
        cursor: 0,
    })
}

// ─── expect_column_type(table, col, expected_type) ──────────────────────
// Compares against duckdb_columns() logical type, case-insensitively.

fn expect_column_type_bind(
    bind: &BindInfo,
    table: &str,
    column: &str,
    expected: &str,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let esc = |s: &str| s.replace('\'', "''");
    let sql = format!(
        "SELECT data_type FROM duckdb_columns() WHERE table_name = '{}' AND column_name = '{}'",
        esc(table),
        esc(column)
    );
    let (actual, err) = match run_query_rows(&sql) {
        Ok(rows) if !rows.is_empty() && !rows[0].is_empty() => (rows[0][0].clone(), String::new()),
        Ok(_) => (String::new(), format!("column {}.{} not found", table, column)),
        Err(e) => (String::new(), e.to_string()),
    };
    let norm = |s: &str| s.trim().to_uppercase();
    let passed = err.is_empty() && norm(&actual) == norm(expected);
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_column_type".into(),
            table: table.into(),
            column: column.into(),
            passed,
            row_count: 0,
            failed_count: if err.is_empty() && !passed { 1 } else { 0 },
            error: if err.is_empty() && !passed {
                format!("actual type {} != expected {}", actual, expected)
            } else {
                err
            },
        }],
        cursor: 0,
    })
}

// ─── expect_table_column_count_between(table, lo, hi) ───────────────────
// Asserts the number of columns in the table is within [lo, hi].

fn expect_table_column_count_between_bind(
    bind: &BindInfo,
    table: &str,
    lo: i64,
    hi: i64,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let esc = table.replace('\'', "''");
    let sql = format!(
        "SELECT COUNT(*) FROM duckdb_columns() WHERE table_name = '{}'",
        esc
    );
    let (ncols, err) = match run_query_rows(&sql) {
        Ok(rows) if !rows.is_empty() && !rows[0].is_empty() => (
            rows[0][0].parse::<i64>().unwrap_or(-1),
            String::new(),
        ),
        Ok(_) => (0, "no rows returned".into()),
        Err(e) => (0, e.to_string()),
    };
    let passed = err.is_empty() && ncols >= lo && ncols <= hi;
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_table_column_count_between".into(),
            table: table.into(),
            column: String::new(),
            passed,
            row_count: ncols,
            failed_count: if err.is_empty() && !passed { 1 } else { 0 },
            error: err,
        }],
        cursor: 0,
    })
}

// ─── Proportion assertions ──────────────────────────────────────────────
// expect_null_proportion_between(table, col, lo, hi): NULL ratio in [lo,hi]
// expect_unique_proportion_between(table, col, lo, hi): distinct ratio in [lo,hi]
// expect_quantile_between(table, col, q, lo, hi): quantile_cont(col, q) in [lo,hi]

fn ratio_between_bind(
    bind: &BindInfo,
    rule: &str,
    table: &str,
    column: &str,
    is_null: bool,
    lo: f64,
    hi: f64,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let expr = if is_null {
        format!("SUM(CASE WHEN {} IS NULL THEN 1 ELSE 0 END)::DOUBLE / COUNT(*)", column)
    } else {
        format!("COUNT(DISTINCT {})::DOUBLE / COUNT(*)", column)
    };
    let sql = format!("SELECT COUNT(*), {} FROM {}", expr, table);
    let (total, ratio, err) = match run_query_rows(&sql) {
        Ok(rows) if !rows.is_empty() && rows[0].len() >= 2 => {
            let total = rows[0][0].parse::<i64>().unwrap_or(-1);
            let v = rows[0][1].parse::<f64>().unwrap_or(f64::NAN);
            (total, v, String::new())
        }
        Ok(_) => (0, f64::NAN, "no rows returned".into()),
        Err(e) => (0, f64::NAN, e.to_string()),
    };
    let passed = err.is_empty() && !ratio.is_nan() && ratio >= lo && ratio <= hi;
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: rule.into(),
            table: table.into(),
            column: column.into(),
            passed,
            row_count: total,
            failed_count: if err.is_empty() && !passed { 1 } else { 0 },
            error: err,
        }],
        cursor: 0,
    })
}

fn quantile_between_bind(
    bind: &BindInfo,
    table: &str,
    column: &str,
    q: f64,
    lo: f64,
    hi: f64,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let sql = format!(
        "SELECT COUNT(*), quantile_cont({}, {}) FROM {}",
        column, q, table
    );
    let (total, val, err) = match run_query_rows(&sql) {
        Ok(rows) if !rows.is_empty() && rows[0].len() >= 2 => {
            let total = rows[0][0].parse::<i64>().unwrap_or(-1);
            let v = rows[0][1].parse::<f64>().unwrap_or(f64::NAN);
            (total, v, String::new())
        }
        Ok(_) => (0, f64::NAN, "no rows returned".into()),
        Err(e) => (0, f64::NAN, e.to_string()),
    };
    let passed = err.is_empty() && !val.is_nan() && val >= lo && val <= hi;
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_quantile_between".into(),
            table: table.into(),
            column: column.into(),
            passed,
            row_count: total,
            failed_count: if err.is_empty() && !passed { 1 } else { 0 },
            error: err,
        }],
        cursor: 0,
    })
}

// ─── expect_columns_unique_together(table, col1, col2, ...) ─────────────
// Asserts the combined tuple (col1, col2, ...) has no duplicates.
// Accepts 2..=4 columns.

fn columns_unique_together_bind(
    bind: &BindInfo,
    table: &str,
    cols: &[String],
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let cols_sql = cols.join(", ");
    let sql = format!(
        "SELECT COUNT(*), COUNT(DISTINCT ({})::VARCHAR) FROM {}",
        cols_sql, table
    );
    let (total, dupes, err) = match run_query_rows(&sql) {
        Ok(rows) if !rows.is_empty() && rows[0].len() >= 2 => {
            let total = rows[0][0].parse::<i64>().unwrap_or(-1);
            let distinct = rows[0][1].parse::<i64>().unwrap_or(-1);
            (total, total - distinct, String::new())
        }
        Ok(_) => (0, 0, "no rows returned".into()),
        Err(e) => (0, 0, e.to_string()),
    };
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_columns_unique_together".into(),
            table: table.into(),
            column: cols.join(","),
            passed: err.is_empty() && dupes == 0,
            row_count: total,
            failed_count: if err.is_empty() { dupes } else { 0 },
            error: err,
        }],
        cursor: 0,
    })
}

// ─── GX-parity assertions ──────────────────────────────────────────────
// expect_column_length_between(table, col, lo, hi): every non-null
//   LENGTH(col) within [lo, hi] (GX: expect_column_value_lengths_to_be_between)
// expect_null_count_between(table, col, lo, hi): NULL row count in [lo, hi]
// expect_row_count_to_equal(table, n): exact row count match

fn column_length_between_bind(
    bind: &BindInfo,
    table: &str,
    column: &str,
    lo: i64,
    hi: i64,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let sql = format!(
        "SELECT COUNT(*), COUNT({}), MIN(LENGTH({})), MAX(LENGTH({})) FROM {}",
        column, column, column, table
    );
    let (total, checked, min_len, max_len, err) = match run_query_rows(&sql) {
        Ok(rows) if !rows.is_empty() && rows[0].len() >= 4 => {
            let total = rows[0][0].parse::<i64>().unwrap_or(-1);
            let checked = rows[0][1].parse::<i64>().unwrap_or(-1);
            let min_len = rows[0][2].parse::<i64>().unwrap_or(i64::MAX);
            let max_len = rows[0][3].parse::<i64>().unwrap_or(i64::MIN);
            (total, checked, min_len, max_len, String::new())
        }
        Ok(_) => (0, 0, i64::MAX, i64::MIN, "no rows returned".into()),
        Err(e) => (0, 0, i64::MAX, i64::MIN, e.to_string()),
    };
    let passed = err.is_empty() && min_len >= lo && max_len <= hi;
    let failed = if err.is_empty() {
        let bad_lo = if min_len < lo { min_len } else { 0 };
        let bad_hi = if max_len > hi { max_len } else { 0 };
        bad_lo + bad_hi
    } else {
        0
    };
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_column_length_between".into(),
            table: table.into(),
            column: column.into(),
            passed,
            row_count: total,
            failed_count: failed,
            error: if err.is_empty() && !passed {
                format!("length range [{}, {}] not within [{}, {}] (checked {} rows)", min_len, max_len, lo, hi, checked)
            } else {
                err
            },
        }],
        cursor: 0,
    })
}

fn null_count_between_bind(
    bind: &BindInfo,
    table: &str,
    column: &str,
    lo: i64,
    hi: i64,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let sql = format!(
        "SELECT COUNT(*), COUNT(*) - COUNT({}) FROM {}",
        column, table
    );
    let (total, nulls, err) = match run_query_rows(&sql) {
        Ok(rows) if !rows.is_empty() && rows[0].len() >= 2 => {
            let total = rows[0][0].parse::<i64>().unwrap_or(-1);
            let nulls = rows[0][1].parse::<i64>().unwrap_or(-1);
            (total, nulls, String::new())
        }
        Ok(_) => (0, 0, "no rows returned".into()),
        Err(e) => (0, 0, e.to_string()),
    };
    let passed = err.is_empty() && nulls >= lo && nulls <= hi;
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_null_count_between".into(),
            table: table.into(),
            column: column.into(),
            passed,
            row_count: total,
            failed_count: if err.is_empty() && !passed { nulls } else { 0 },
            error: if err.is_empty() && !passed {
                format!("null count {} not within [{}, {}]", nulls, lo, hi)
            } else {
                err
            },
        }],
        cursor: 0,
    })
}

fn row_count_to_equal_bind(
    bind: &BindInfo,
    table: &str,
    expected: i64,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let (total, err) = match run_query_rows(&format!("SELECT COUNT(*) FROM {}", table)) {
        Ok(rows) if !rows.is_empty() && !rows[0].is_empty() => (
            rows[0][0].parse::<i64>().unwrap_or(-1),
            String::new(),
        ),
        Ok(_) => (0, "no rows returned".into()),
        Err(e) => (0, e.to_string()),
    };
    let passed = err.is_empty() && total == expected;
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_row_count_to_equal".into(),
            table: table.into(),
            column: String::new(),
            passed,
            row_count: total,
            failed_count: if err.is_empty() && !passed { 1 } else { 0 },
            error: if err.is_empty() && !passed {
                format!("row count {} != expected {}", total, expected)
            } else {
                err
            },
        }],
        cursor: 0,
    })
}

// ─── expect_custom_sql(table, sql) ──────────────────────────────────────
// Fails on any row returned by the user-supplied WHERE clause.
// The SQL receives {table} placeholder substitution.
fn expect_custom_sql_bind(
    bind: &BindInfo,
    table: &str,
    where_sql: &str,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let sql = where_sql.replace("{table}", table);
    let (total, matched, err) = count_rows(table, Some(&sql));
    let (row_count, failed) = if err.is_empty() { (total, matched) } else { (0, 0) };
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_custom_sql".into(),
            table: table.into(),
            column: String::new(),
            passed: err.is_empty() && failed == 0,
            row_count,
            failed_count: failed,
            error: err,
        }],
        cursor: 0,
    })
}

// ─── GX parity batch 2 ─────────────────────────────────────────────────
// expect_not_in_set(table, col, 'a,b,c'): no value in the comma set
// expect_not_match_regex(table, col, pattern): no value matches
// expect_match_date_format(table, col, format): every non-null value parses
// expect_sorted(table, col, 'asc'|'desc'): column ordered
// expect_median_between(table, col, lo, hi): median_cont in range

fn not_in_set_bind(
    bind: &BindInfo,
    table: &str,
    column: &str,
    values_csv: &str,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let values: Vec<String> = values_csv
        .split(',')
        .map(|s| s.trim().trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if values.is_empty() {
        return Ok(AssertionState {
            results: vec![AssertionResult {
                rule: "expect_not_in_set".into(),
                table: table.into(),
                column: column.into(),
                passed: false,
                row_count: 0,
                failed_count: 0,
                error: "empty value set".into(),
            }],
            cursor: 0,
        });
    }
    let quoted: Vec<String> = values
        .iter()
        .map(|v| format!("'{}'", v.replace('\'', "''")))
        .collect();
    let cond = format!("{}::VARCHAR IN ({})", column, quoted.join(", "));
    let (total, matched, err) = count_rows(table, Some(&cond));
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_not_in_set".into(),
            table: table.into(),
            column: column.into(),
            passed: err.is_empty() && matched == 0,
            row_count: total,
            failed_count: matched,
            error: if err.is_empty() && matched > 0 {
                format!("{} rows in forbidden set [{}]", matched, values_csv)
            } else {
                err
            },
        }],
        cursor: 0,
    })
}

fn not_match_regex_bind(
    bind: &BindInfo,
    table: &str,
    column: &str,
    pattern: &str,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let esc = pattern.replace('\'', "''");
    let cond = format!("regexp_matches({}::VARCHAR, '{}')", column, esc);
    let (total, matched, err) = count_rows(table, Some(&cond));
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_not_match_regex".into(),
            table: table.into(),
            column: column.into(),
            passed: err.is_empty() && matched == 0,
            row_count: total,
            failed_count: matched,
            error: if err.is_empty() && matched > 0 {
                format!("{} rows matched forbidden pattern", matched)
            } else {
                err
            },
        }],
        cursor: 0,
    })
}

fn match_date_format_bind(
    bind: &BindInfo,
    table: &str,
    column: &str,
    format: &str,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    // try_strptime returns NULL for non-matching values → count non-null results
    let esc_fmt = format.replace('\'', "''");
    let cond = format!(
        "{} IS NOT NULL AND try_strptime({}::VARCHAR, '{}') IS NULL",
        column, column, esc_fmt
    );
    let (total, matched, err) = count_rows(table, Some(&cond));
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_match_date_format".into(),
            table: table.into(),
            column: column.into(),
            passed: err.is_empty() && matched == 0,
            row_count: total,
            failed_count: matched,
            error: if err.is_empty() && matched > 0 {
                format!("{} rows failed date format '{}'", matched, format)
            } else {
                err
            },
        }],
        cursor: 0,
    })
}

fn sorted_bind(
    bind: &BindInfo,
    table: &str,
    column: &str,
    direction: &str,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let is_asc = direction.eq_ignore_ascii_case("asc");
    if !is_asc && !direction.eq_ignore_ascii_case("desc") {
        return Ok(AssertionState {
            results: vec![AssertionResult {
                rule: "expect_sorted".into(),
                table: table.into(),
                column: column.into(),
                passed: false,
                row_count: 0,
                failed_count: 0,
                error: format!("direction must be 'asc' or 'desc', got '{}'", direction),
            }],
            cursor: 0,
        });
    }
    // Count adjacent inversions: subquery with LAG ordered by physical rowid
    let cmp = if is_asc { "<" } else { ">" };
    let sql = format!(
        "SELECT (SELECT COUNT(*) FROM {t}), \
         COALESCE((SELECT COUNT(*) FROM (SELECT {c} AS v, LAG({c}) OVER (ORDER BY rowid) AS prev FROM {t}) sub WHERE v {cmp} prev), 0)",
        c = column,
        t = table
    );
    let (total, bad, err) = match run_query_rows(&sql) {
        Ok(rows) if !rows.is_empty() && rows[0].len() >= 2 => {
            let total = rows[0][0].parse::<i64>().unwrap_or(-1);
            let bad = rows[0][1].parse::<i64>().unwrap_or(-1);
            (total, bad, String::new())
        }
        Ok(_) => (0, 0, "no rows returned".into()),
        Err(e) => (0, 0, e.to_string()),
    };
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_sorted".into(),
            table: table.into(),
            column: column.into(),
            passed: err.is_empty() && bad == 0,
            row_count: total,
            failed_count: bad,
            error: if err.is_empty() && bad > 0 {
                format!("{} adjacent inversions ({} order)", bad, direction)
            } else {
                err
            },
        }],
        cursor: 0,
    })
}

fn median_between_bind(
    bind: &BindInfo,
    table: &str,
    column: &str,
    lo: f64,
    hi: f64,
) -> Result<AssertionState, ExtensionError> {
    add_assertion_columns(bind);
    let sql = format!(
        "SELECT COUNT(*), median({}) FROM {}",
        column, table
    );
    let (total, val, err) = match run_query_rows(&sql) {
        Ok(rows) if !rows.is_empty() && rows[0].len() >= 2 => {
            let total = rows[0][0].parse::<i64>().unwrap_or(-1);
            let v = rows[0][1].parse::<f64>().unwrap_or(f64::NAN);
            (total, v, String::new())
        }
        Ok(_) => (0, f64::NAN, "no rows returned".into()),
        Err(e) => (0, f64::NAN, e.to_string()),
    };
    let passed = err.is_empty() && !val.is_nan() && val >= lo && val <= hi;
    Ok(AssertionState {
        results: vec![AssertionResult {
            rule: "expect_median_between".into(),
            table: table.into(),
            column: column.into(),
            passed,
            row_count: total,
            failed_count: if err.is_empty() && !passed { 1 } else { 0 },
            error: if err.is_empty() && !passed {
                format!("median {} not within [{}, {}]", val, lo, hi)
            } else {
                err
            },
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

    // expect_accepted_values(table, col, 'a,b,c')
    let tf = TableFunctionBuilder::new("expect_accepted_values")
        .param(TypeId::Varchar).param(TypeId::Varchar).param(TypeId::Varchar)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let column = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
            let values = unsafe { bind.get_parameter_value(2) }.as_str().unwrap_or_default().to_string();
            expect_accepted_values_bind(bind, &table, &column, &values)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // expect_match_regex(table, col, pattern)
    let tf = TableFunctionBuilder::new("expect_match_regex")
        .param(TypeId::Varchar).param(TypeId::Varchar).param(TypeId::Varchar)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let column = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
            let pattern = unsafe { bind.get_parameter_value(2) }.as_str().unwrap_or_default().to_string();
            expect_match_regex_bind(bind, &table, &column, &pattern)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // expect_relationship(table, col, to_table, to_col)
    let tf = TableFunctionBuilder::new("expect_relationship")
        .param(TypeId::Varchar).param(TypeId::Varchar).param(TypeId::Varchar).param(TypeId::Varchar)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let column = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
            let to_table = unsafe { bind.get_parameter_value(2) }.as_str().unwrap_or_default().to_string();
            let to_col = unsafe { bind.get_parameter_value(3) }.as_str().unwrap_or_default().to_string();
            expect_relationship_bind(bind, &table, &column, &to_table, &to_col)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // expect_custom_sql(table, where_clause)
    let tf = TableFunctionBuilder::new("expect_custom_sql")
        .param(TypeId::Varchar).param(TypeId::Varchar)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let where_sql = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
            expect_custom_sql_bind(bind, &table, &where_sql)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // ── Statistical metric assertions (table, col, lo, hi) ──
    // Macro: registers expect_{name}_between → metric_between_bind with agg expr.
    macro_rules! register_metric {
        ($fn_name:literal, $rule_name:literal, $agg:expr) => {{
            let tf = TableFunctionBuilder::new($fn_name)
                .param(TypeId::Varchar).param(TypeId::Varchar)
                .param(TypeId::Double).param(TypeId::Double)
                .with_state::<AssertionState, _>(move |bind| {
                    let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
                    let column = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
                    let lo = unsafe { bind.get_parameter_value(2) }.as_f64_or(f64::MIN);
                    let hi = unsafe { bind.get_parameter_value(3) }.as_f64_or(f64::MAX);
                    let expr = format!($agg, column);
                    metric_between_bind(bind, $rule_name, &table, &column, &expr, lo, hi)
                })
                .scan(write_assertion).build()?;
            unsafe { let _ = con.register_table(tf); }
        }};
    }
    register_metric!("expect_min_between", "expect_min_between", "MIN({})");
    register_metric!("expect_max_between", "expect_max_between", "MAX({})");
    register_metric!("expect_mean_between", "expect_mean_between", "AVG({})");
    register_metric!("expect_stddev_between", "expect_stddev_between", "STDDEV({})");
    register_metric!("expect_sum_between", "expect_sum_between", "SUM({})");
    register_metric!("expect_distinct_count_between", "expect_distinct_count_between", "COUNT(DISTINCT {})");

    // expect_column_type(table, col, expected_type)
    let tf = TableFunctionBuilder::new("expect_column_type")
        .param(TypeId::Varchar).param(TypeId::Varchar).param(TypeId::Varchar)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let column = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
            let expected = unsafe { bind.get_parameter_value(2) }.as_str().unwrap_or_default().to_string();
            expect_column_type_bind(bind, &table, &column, &expected)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // expect_table_column_count_between(table, lo, hi)
    let tf = TableFunctionBuilder::new("expect_table_column_count_between")
        .param(TypeId::Varchar).param(TypeId::BigInt).param(TypeId::BigInt)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let lo = unsafe { bind.get_parameter_value(1) }.as_i64_or(0);
            let hi = unsafe { bind.get_parameter_value(2) }.as_i64_or(i64::MAX);
            expect_table_column_count_between_bind(bind, &table, lo, hi)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // ── Proportion / quantile / composite-uniqueness assertions ──
    macro_rules! register_ratio {
        ($fn_name:literal, $rule_name:literal, $is_null:expr) => {{
            let tf = TableFunctionBuilder::new($fn_name)
                .param(TypeId::Varchar).param(TypeId::Varchar)
                .param(TypeId::Double).param(TypeId::Double)
                .with_state::<AssertionState, _>(move |bind| {
                    let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
                    let column = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
                    let lo = unsafe { bind.get_parameter_value(2) }.as_f64_or(f64::MIN);
                    let hi = unsafe { bind.get_parameter_value(3) }.as_f64_or(f64::MAX);
                    ratio_between_bind(bind, $rule_name, &table, &column, $is_null, lo, hi)
                })
                .scan(write_assertion).build()?;
            unsafe { let _ = con.register_table(tf); }
        }};
    }
    register_ratio!("expect_null_proportion_between", "expect_null_proportion_between", true);
    register_ratio!("expect_unique_proportion_between", "expect_unique_proportion_between", false);

    // expect_quantile_between(table, col, q, lo, hi)
    let tf = TableFunctionBuilder::new("expect_quantile_between")
        .param(TypeId::Varchar).param(TypeId::Varchar)
        .param(TypeId::Double).param(TypeId::Double).param(TypeId::Double)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let column = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
            let q = unsafe { bind.get_parameter_value(2) }.as_f64_or(0.5);
            let lo = unsafe { bind.get_parameter_value(3) }.as_f64_or(f64::MIN);
            let hi = unsafe { bind.get_parameter_value(4) }.as_f64_or(f64::MAX);
            quantile_between_bind(bind, &table, &column, q, lo, hi)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // expect_columns_unique_together(table, col1, col2) — 2 columns
    let tf = TableFunctionBuilder::new("expect_columns_unique_together")
        .param(TypeId::Varchar).param(TypeId::Varchar).param(TypeId::Varchar)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let c1 = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
            let c2 = unsafe { bind.get_parameter_value(2) }.as_str().unwrap_or_default().to_string();
            columns_unique_together_bind(bind, &table, &[c1, c2])
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // ── GX-parity assertions ──
    // expect_column_length_between(table, col, lo, hi)
    let tf = TableFunctionBuilder::new("expect_column_length_between")
        .param(TypeId::Varchar).param(TypeId::Varchar)
        .param(TypeId::BigInt).param(TypeId::BigInt)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let column = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
            let lo = unsafe { bind.get_parameter_value(2) }.as_i64_or(0);
            let hi = unsafe { bind.get_parameter_value(3) }.as_i64_or(i64::MAX);
            column_length_between_bind(bind, &table, &column, lo, hi)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // expect_null_count_between(table, col, lo, hi)
    let tf = TableFunctionBuilder::new("expect_null_count_between")
        .param(TypeId::Varchar).param(TypeId::Varchar)
        .param(TypeId::BigInt).param(TypeId::BigInt)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let column = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
            let lo = unsafe { bind.get_parameter_value(2) }.as_i64_or(0);
            let hi = unsafe { bind.get_parameter_value(3) }.as_i64_or(i64::MAX);
            null_count_between_bind(bind, &table, &column, lo, hi)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // expect_row_count_to_equal(table, n)
    let tf = TableFunctionBuilder::new("expect_row_count_to_equal")
        .param(TypeId::Varchar).param(TypeId::BigInt)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let expected = unsafe { bind.get_parameter_value(1) }.as_i64_or(-1);
            row_count_to_equal_bind(bind, &table, expected)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // ── GX parity batch 2 ──
    // expect_not_in_set(table, col, 'a,b,c')
    let tf = TableFunctionBuilder::new("expect_not_in_set")
        .param(TypeId::Varchar).param(TypeId::Varchar).param(TypeId::Varchar)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let column = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
            let values = unsafe { bind.get_parameter_value(2) }.as_str().unwrap_or_default().to_string();
            not_in_set_bind(bind, &table, &column, &values)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // expect_not_match_regex(table, col, pattern)
    let tf = TableFunctionBuilder::new("expect_not_match_regex")
        .param(TypeId::Varchar).param(TypeId::Varchar).param(TypeId::Varchar)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let column = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
            let pattern = unsafe { bind.get_parameter_value(2) }.as_str().unwrap_or_default().to_string();
            not_match_regex_bind(bind, &table, &column, &pattern)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // expect_match_date_format(table, col, format)
    let tf = TableFunctionBuilder::new("expect_match_date_format")
        .param(TypeId::Varchar).param(TypeId::Varchar).param(TypeId::Varchar)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let column = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
            let format = unsafe { bind.get_parameter_value(2) }.as_str().unwrap_or_default().to_string();
            match_date_format_bind(bind, &table, &column, &format)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // expect_sorted(table, col, 'asc'|'desc')
    let tf = TableFunctionBuilder::new("expect_sorted")
        .param(TypeId::Varchar).param(TypeId::Varchar).param(TypeId::Varchar)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let column = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
            let direction = unsafe { bind.get_parameter_value(2) }.as_str().unwrap_or_default().to_string();
            sorted_bind(bind, &table, &column, &direction)
        })
        .scan(write_assertion).build()?;
    unsafe { let _ = con.register_table(tf); }

    // expect_median_between(table, col, lo, hi)
    let tf = TableFunctionBuilder::new("expect_median_between")
        .param(TypeId::Varchar).param(TypeId::Varchar)
        .param(TypeId::Double).param(TypeId::Double)
        .with_state::<AssertionState, _>(move |bind| {
            let table = unsafe { bind.get_parameter_value(0) }.as_str().unwrap_or_default().to_string();
            let column = unsafe { bind.get_parameter_value(1) }.as_str().unwrap_or_default().to_string();
            let lo = unsafe { bind.get_parameter_value(2) }.as_f64_or(f64::MIN);
            let hi = unsafe { bind.get_parameter_value(3) }.as_f64_or(f64::MAX);
            median_between_bind(bind, &table, &column, lo, hi)
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
