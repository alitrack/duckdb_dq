//! validate_expectations: batch assertion engine.
//!
//! Accepts a JSON object of rules, e.g.:
//! {
//!   "expect_table_row_count_between": {"min": 100, "max": 100000},
//!   "expect_column_values_not_null": {"column": "order_id"},
//!   "expect_column_values_unique": {"column": "order_id"},
//!   "expect_column_values_in_range": {"column": "amount", "min": 0, "max": 100000}
//! }
//!
//! Each rule dispatches to the same underlying SQL counting helpers as the
//! standalone expect_* functions and yields one AssertionResult row.

use crate::engine::run_query_rows;
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct AssertionResult {
    pub rule: String,
    pub table: String,
    pub column: String,
    pub passed: bool,
    pub row_count: i64,
    pub failed_count: i64,
    pub error: String,
}

pub(crate) fn count_rows(table: &str, cond: Option<&str>) -> (i64, i64, String) {
    let total_sql = format!("SELECT COUNT(*) FROM {}", table);
    let total = match run_query_rows(&total_sql) {
        Ok(rows) if !rows.is_empty() && !rows[0].is_empty() => rows[0][0].parse::<i64>().unwrap_or(-1),
        Ok(_) => return (0, 0, format!("no rows returned for {}", total_sql)),
        Err(e) => return (0, 0, e.to_string()),
    };
    match cond {
        None => (total, 0, String::new()),
        Some(c) => {
            let sql = format!("SELECT COUNT(*) FROM {} WHERE {}", table, c);
            match run_query_rows(&sql) {
                Ok(rows) => {
                    let matched = if rows.is_empty() || rows[0].is_empty() {
                        0
                    } else {
                        rows[0][0].parse::<i64>().unwrap_or(-1)
                    };
                    (total, matched, String::new())
                }
                Err(e) => (0, 0, e.to_string()),
            }
        }
    }
}

/// Run a full JSON rule set against a table; returns one result per rule.
pub fn run_rules(table: &str, rules_json: &str) -> Vec<AssertionResult> {
    let parsed: Value = match serde_json::from_str(rules_json) {
        Ok(v) => v,
        Err(e) => {
            return vec![AssertionResult {
                rule: "json_parse_error".into(),
                table: table.into(),
                column: String::new(),
                passed: false,
                row_count: 0,
                failed_count: 0,
                error: format!("invalid rules JSON: {}", e),
            }];
        }
    };

    let obj = match parsed.as_object() {
        Some(o) => o,
        None => {
            return vec![AssertionResult {
                rule: "json_shape_error".into(),
                table: table.into(),
                column: String::new(),
                passed: false,
                row_count: 0,
                failed_count: 0,
                error: "rules must be a JSON object".into(),
            }];
        }
    };

    let mut results = Vec::new();
    for (rule, params) in obj {
        let r = match rule.as_str() {
            "expect_table_row_count_between" => {
                let min = params.get("min").and_then(|v| v.as_i64()).unwrap_or(0);
                let max = params.get("max").and_then(|v| v.as_i64()).unwrap_or(i64::MAX);
                let (total, _, err) = count_rows(table, None);
                AssertionResult {
                    rule: rule.clone(),
                    table: table.into(),
                    column: String::new(),
                    passed: err.is_empty() && total >= min && total <= max,
                    row_count: total,
                    failed_count: if err.is_empty() && (total < min || total > max) { 1 } else { 0 },
                    error: err,
                }
            }
            "expect_column_values_not_null" => {
                let col = params.get("column").and_then(|v| v.as_str()).unwrap_or_default();
                let (total, matched, err) = count_rows(table, Some(&format!("{} IS NULL", col)));
                AssertionResult {
                    rule: rule.clone(),
                    table: table.into(),
                    column: col.into(),
                    passed: err.is_empty() && matched == 0,
                    row_count: total,
                    failed_count: matched,
                    error: err,
                }
            }
            "expect_column_values_unique" => {
                let col = params.get("column").and_then(|v| v.as_str()).unwrap_or_default();
                let (total, err) = match run_query_rows(&format!(
                    "SELECT COUNT(*), COUNT(DISTINCT {}) FROM {}", col, table
                )) {
                    Ok(rows) if !rows.is_empty() && rows[0].len() >= 2 => (
                        rows[0][0].parse::<i64>().unwrap_or(-1),
                        String::new(),
                    ),
                    Ok(_) => (0, "no rows returned".into()),
                    Err(e) => (0, e.to_string()),
                };
                let dupes = if err.is_empty() {
                    match run_query_rows(&format!("SELECT COUNT(DISTINCT {}) FROM {}", col, table)) {
                        Ok(rows) if !rows.is_empty() && !rows[0].is_empty() => {
                            total - rows[0][0].parse::<i64>().unwrap_or(-1)
                        }
                        _ => -1,
                    }
                } else { 0 };
                AssertionResult {
                    rule: rule.clone(),
                    table: table.into(),
                    column: col.into(),
                    passed: err.is_empty() && dupes == 0,
                    row_count: total,
                    failed_count: dupes,
                    error: err,
                }
            }
            "expect_column_values_in_range" => {
                let col = params.get("column").and_then(|v| v.as_str()).unwrap_or_default();
                let lo = params.get("min").and_then(|v| v.as_f64()).unwrap_or(f64::MIN);
                let hi = params.get("max").and_then(|v| v.as_f64()).unwrap_or(f64::MAX);
                let cond = format!("{} IS NULL OR {} < {} OR {} > {}", col, col, lo, col, hi);
                let (total, matched, err) = count_rows(table, Some(&cond));
                AssertionResult {
                    rule: rule.clone(),
                    table: table.into(),
                    column: col.into(),
                    passed: err.is_empty() && matched == 0,
                    row_count: total,
                    failed_count: matched,
                    error: err,
                }
            }
            "expect_column_values_match_regex" => {
                let col = params.get("column").and_then(|v| v.as_str()).unwrap_or_default();
                let pat = params.get("pattern").and_then(|v| v.as_str()).unwrap_or_default();
                let (total, matched, err) = count_rows(
                    table,
                    Some(&format!("{} IS NULL OR NOT regexp_matches(CAST({} AS VARCHAR), '{}')", col, col, pat)),
                );
                AssertionResult {
                    rule: rule.clone(),
                    table: table.into(),
                    column: col.into(),
                    passed: err.is_empty() && matched == 0,
                    row_count: total,
                    failed_count: matched,
                    error: err,
                }
            }
            "expect_column_values_to_be_between" => {
                // alias of in_range for GX naming compat
                let col = params.get("column").and_then(|v| v.as_str()).unwrap_or_default();
                let lo = params.get("min").and_then(|v| v.as_f64()).unwrap_or(f64::MIN);
                let hi = params.get("max").and_then(|v| v.as_f64()).unwrap_or(f64::MAX);
                let cond = format!("{} IS NULL OR {} < {} OR {} > {}", col, col, lo, col, hi);
                let (total, matched, err) = count_rows(table, Some(&cond));
                AssertionResult {
                    rule: rule.clone(),
                    table: table.into(),
                    column: col.into(),
                    passed: err.is_empty() && matched == 0,
                    row_count: total,
                    failed_count: matched,
                    error: err,
                }
            }
            "expect_column_values_to_be_in_set" | "expect_column_values_to_be_in" => {
                // accepted_values: values: ["a","b","c"]
                let col = params.get("column").and_then(|v| v.as_str()).unwrap_or_default();
                let values: Vec<String> = params
                    .get("values")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                if values.is_empty() {
                    AssertionResult {
                        rule: rule.clone(),
                        table: table.into(),
                        column: col.into(),
                        passed: false,
                        row_count: 0,
                        failed_count: 0,
                        error: "values list is empty".into(),
                    }
                } else {
                    let quoted: Vec<String> = values.iter().map(|v| format!("'{}'", v.replace('\'', "''"))).collect();
                    let cond = format!("{} IS NULL OR {} NOT IN ({})", col, col, quoted.join(", "));
                    let (total, matched, err) = count_rows(table, Some(&cond));
                    AssertionResult {
                        rule: rule.clone(),
                        table: table.into(),
                        column: col.into(),
                        passed: err.is_empty() && matched == 0,
                        row_count: total,
                        failed_count: matched,
                        error: err,
                    }
                }
            }
            "expect_column_values_to_match_regex" => {
                let col = params.get("column").and_then(|v| v.as_str()).unwrap_or_default();
                let pat = params.get("pattern").and_then(|v| v.as_str()).unwrap_or_default().replace('\'', "''");
                let cond = format!(
                    "{} IS NULL OR NOT regexp_matches(CAST({} AS VARCHAR), '{}')",
                    col, col, pat
                );
                let (total, matched, err) = count_rows(table, Some(&cond));
                AssertionResult {
                    rule: rule.clone(),
                    table: table.into(),
                    column: col.into(),
                    passed: err.is_empty() && matched == 0,
                    row_count: total,
                    failed_count: matched,
                    error: err,
                }
            }
            "expect_column_values_to_exist_in_table" | "expect_column_relationship" => {
                // relationship: to_table, to_column (orphan check)
                let col = params.get("column").and_then(|v| v.as_str()).unwrap_or_default();
                let to_table = params.get("to_table").and_then(|v| v.as_str()).unwrap_or_default();
                let to_col = params.get("to_column").and_then(|v| v.as_str()).unwrap_or_default();
                let cond = format!(
                    "{} IS NOT NULL AND {} NOT IN (SELECT {} FROM {})",
                    col, col, to_col, to_table
                );
                let (total, matched, err) = count_rows(table, Some(&cond));
                AssertionResult {
                    rule: rule.clone(),
                    table: table.into(),
                    column: col.into(),
                    passed: err.is_empty() && matched == 0,
                    row_count: total,
                    failed_count: matched,
                    error: err,
                }
            }
            "expect_custom_sql" => {
                let where_sql = params.get("sql").and_then(|v| v.as_str()).unwrap_or_default().replace("{table}", table);
                let (total, matched, err) = count_rows(table, Some(&where_sql));
                AssertionResult {
                    rule: rule.clone(),
                    table: table.into(),
                    column: String::new(),
                    passed: err.is_empty() && matched == 0,
                    row_count: total,
                    failed_count: matched,
                    error: err,
                }
            }
            _ => AssertionResult {
                rule: rule.clone(),
                table: table.into(),
                column: String::new(),
                passed: false,
                row_count: 0,
                failed_count: 0,
                error: format!("unsupported rule: {}", rule),
            },
        };
        results.push(r);
    }
    results
}
