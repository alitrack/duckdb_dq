# duckdb_dq

Data quality assertion framework for DuckDB — SQL-native `expect_*` rules, profiling, and persisted quality reports. Zero Python, zero external services: everything runs inside DuckDB on a persistent secondary connection, so DuckDB's vectorized engine does all the counting.

## Functions

| Function | Type | Description |
|----------|------|-------------|
| `expect_not_null(table, column)` | table | Fails if any NULL in column |
| `expect_unique(table, column)` | table | Fails if duplicate values in column |
| `expect_in_range(table, column, lo, hi)` | table | Fails on NULL / below lo / above hi |
| `expect_row_count_between(table, lo, hi)` | table | Fails if row count outside [lo, hi] |
| `expect_accepted_values(table, column, 'a,b,c')` | table | Fails on NULL / values outside allowed set |
| `expect_match_regex(table, column, pattern)` | table | Fails on NULL / values not matching regex |
| `expect_relationship(table, column, to_table, to_column)` | table | Fails on orphan values (broken FK) |
| `expect_custom_sql(table, where_clause)` | table | Fails on rows returned by custom WHERE (supports `{table}`) |
| `expect_min_between(table, column, lo, hi)` | table | Fails if MIN(column) outside [lo, hi] |
| `expect_max_between(table, column, lo, hi)` | table | Fails if MAX(column) outside [lo, hi] |
| `expect_mean_between(table, column, lo, hi)` | table | Fails if AVG(column) outside [lo, hi] |
| `expect_stddev_between(table, column, lo, hi)` | table | Fails if STDDEV(column) outside [lo, hi] |
| `expect_sum_between(table, column, lo, hi)` | table | Fails if SUM(column) outside [lo, hi] |
| `expect_distinct_count_between(table, column, lo, hi)` | table | Fails if COUNT(DISTINCT column) outside [lo, hi] |
| `expect_column_type(table, column, type)` | table | Fails if column's logical type ≠ expected |
| `expect_table_column_count_between(table, lo, hi)` | table | Fails if table column count outside [lo, hi] |
| `expect_null_proportion_between(table, column, lo, hi)` | table | Fails if NULL ratio outside [lo, hi] |
| `expect_unique_proportion_between(table, column, lo, hi)` | table | Fails if distinct ratio outside [lo, hi] |
| `expect_quantile_between(table, column, q, lo, hi)` | table | Fails if quantile_cont(col, q) outside [lo, hi] |
| `expect_columns_unique_together(table, col1, col2)` | table | Fails if (col1, col2) tuple has duplicates |
| `expect_column_length_between(table, column, lo, hi)` | table | Fails if any non-null value length outside [lo, hi] |
| `expect_null_count_between(table, column, lo, hi)` | table | Fails if NULL row count outside [lo, hi] |
| `expect_row_count_to_equal(table, n)` | table | Fails if table row count ≠ n |
| `expect_not_in_set(table, column, 'a,b,c')` | table | Fails on values in forbidden set |
| `expect_not_match_regex(table, column, pattern)` | table | Fails on values matching forbidden regex |
| `expect_match_date_format(table, column, strptime_fmt)` | table | Fails on values not parsing per strptime format |
| `expect_sorted(table, column, 'asc'\|'desc')` | table | Fails on adjacent inversions (physical order) |
| `expect_median_between(table, column, lo, hi)` | table | Fails if median outside [lo, hi] |
| `profile_table(table)` | table | Per-column profiling (count, null %, distinct, min, max) |
| `validate_expectations(table, json_rules)` | table | Batch assertions from a JSON rule set |
| `dq_run(name, table, json_rules)` | scalar | Run rule set, persist one report row, return summary |
| `dq_reports()` | table | Persisted run history (name, table, summary, passed, run_at) |
| `dq_dashboard(table, json_rules)` | table | dash-compatible dashboard JSON + checks/passed/failed/pass_rate |
| `dq_federated(sources_json, table, json_rules)` | table | Cross-DB quality baseline via duckdb_universal (per-source results) |
| `dq_suggest(table)` | table | Profile-driven candidate rules (rule, column, severity, reason, params) — recall-first: surfaces checks worth running instead of waiting for you to invent them; suggestions feed straight into `validate_expectations` |

## Quick start

```sql
LOAD 'dq.duckdb_extension';

-- Single assertion → result table
SELECT * FROM expect_not_null('sales', 'amount');
-- rule | table_name | column_name | passed | row_count | failed_count | error

-- Enumerated values, regex, foreign keys, custom SQL
SELECT * FROM expect_accepted_values('customers', 'status', 'active,inactive,suspended');
SELECT * FROM expect_match_regex('customers', 'email', '^[^@]+@[^@]+\\.com$');
SELECT * FROM expect_relationship('orders', 'customer_id', 'customers', 'id');
SELECT * FROM expect_custom_sql('orders', 'amount < 0');

-- Profiling
SELECT * FROM profile_table('sales');

-- Batch assertions (GX-style JSON rule set)
SELECT * FROM validate_expectations('sales', '{
  "expect_table_row_count_between":   {"min": 100, "max": 10000000},
  "expect_column_values_not_null":    {"column": "order_id"},
  "expect_column_values_unique":      {"column": "order_id"},
  "expect_column_values_in_range":    {"column": "amount", "min": 0, "max": 100000},
  "expect_column_values_match_regex": {"column": "email", "pattern": "^[^@]+@[^@]+$"},
  "expect_column_relationship":       {"column": "customer_id", "to_table": "customers", "to_column": "id"},
  "expect_custom_sql":                {"sql": "{table}.amount < 0"}
}');

-- Run + persist a named quality check
SELECT dq_run('daily_sales', 'sales', '{
  "expect_table_row_count_between": {"min": 100, "max": 10000000}
}');
-- → "dq_run 'daily_sales': 1/1 passed, 0 failed, 0 errors"

-- Report history (time-series quality tracking)
SELECT * FROM dq_reports();

-- Profile-driven suggestions: what should I be checking? (recall-first)
SELECT * FROM dq_suggest('sales');
-- rule | column_name | severity | reason | params
-- → high-severity NULL / duplication / near-constant signals, each with
--   ready-to-run params you can feed into validate_expectations
```

## Design

- **Assertions compile to SQL** — `expect_in_range('t','c',0,100)` becomes
  `SELECT COUNT(*) FROM t WHERE c IS NULL OR c < 0 OR c > 100`. DuckDB counts,
  Rust does zero per-row work.
- **Persistent secondary connection** — DuckDB forbids querying the main
  connection from function callbacks; `engine.rs` keeps ONE connection created
  in the init callback (early connect, required on macOS ARM64) and reuses it
  for every assertion query.
- **Result is a first-class table** — assertions can be `WHERE NOT passed`'d,
  joined, aggregated, or fed into dashboards/CI.
- **Rule JSON = data contract seed** — a `validate_expectations` rule set is a
  machine-verifiable contract in the direction Gartner predicts for 2030.

## Build

```sh
make configure   # once: venv + platform stamp
make release     # → build/release/dq.duckdb_extension
```

Test locally:

```sh
(echo "LOAD './build/release/dq.duckdb_extension';"; cat test/dq_test.sql) | duckdb -unsigned
```

## Roadmap

- [x] Core assertions (not_null / unique / in_range / row_count_between)
- [x] Statistical assertions (min / max / mean / stddev / sum / distinct_count)
- [x] Schema assertions (column type / table column count)
- [x] Proportion + quantile assertions (null % / unique % / quantile_cont)
- [x] Composite uniqueness (columns_unique_together)
- [x] GX-parity (string length / null count / exact row count)
- [x] GX batch 2 (forbidden set / negated regex / date format / sorted / median)
- [x] profile_table (SUMMARIZE-based)
- [x] validate_expectations JSON batch engine
- [x] dq_run + dq_reports persistence
- [x] dq_dashboard → duckdb_dashboard (dash_create-compatible JSON, verified end-to-end)
- [x] dq_federated → duckdb_universal (cross-DB quality baselines, SQLite fixtures verified)
- [x] dq_suggest → profile-driven candidate rules (recall-first: NULL / duplication / near-constant signals with severity + runnable params)
- [ ] 30+ assertions (GX full parity: set membership of expected, match_like)

## License

MIT
