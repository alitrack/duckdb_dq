-- duckdb_dq smoke tests
-- Run: duckdb -unsigned -c "INSTALL dq FROM './build/release/'; LOAD dq; .read test/dq_test.sql"

CREATE OR REPLACE TABLE sales AS
SELECT * FROM (VALUES
    (1, 100.0, '2026-01-01'),
    (2, 250.5, '2026-01-02'),
    (3, 30.0,  NULL),
    (4, 75.0,  '2026-01-04'),
    (3, 200.0, '2026-01-05'),  -- duplicate id
    (6, NULL,  '2026-01-06'),  -- null amount
    (7, 500.0, '2026-01-07'),
    (8, -10.0, '2026-01-08')   -- negative amount
) t(id, amount, order_date);

-- 1. expect_not_null — should FAIL (amount has 1 null)
SELECT '1. expect_not_null(amount)' AS test;
SELECT * FROM expect_not_null('sales', 'amount');

-- 2. expect_not_null(order_date) — should PASS
SELECT '2. expect_not_null(order_date)' AS test;
SELECT * FROM expect_not_null('sales', 'order_date');

-- 3. expect_unique(id) — should FAIL (id=3 duplicated)
SELECT '3. expect_unique(id)' AS test;
SELECT * FROM expect_unique('sales', 'id');

-- 4. expect_in_range(amount, 0, 1000) — should FAIL (-10, null)
SELECT '4. expect_in_range(amount, 0, 1000)' AS test;
SELECT * FROM expect_in_range('sales', 'amount', 0, 1000);

-- 5. expect_row_count_between(5, 20) — should PASS
SELECT '5. expect_row_count_between(5, 20)' AS test;
SELECT * FROM expect_row_count_between('sales', 5, 20);

-- 6. profile_table
SELECT '6. profile_table(sales)' AS test;
SELECT column_name, column_type, count, null_pct, distinct_count, min, max
FROM profile_table('sales');

-- 7. validate_expectations (batch)
SELECT '7. validate_expectations batch' AS test;
SELECT rule, column_name, passed, row_count, failed_count, error
FROM validate_expectations('sales', '{
  "expect_table_row_count_between": {"min": 5, "max": 20},
  "expect_column_values_not_null": {"column": "id"},
  "expect_column_values_not_null": {"column": "amount"},
  "expect_column_values_unique": {"column": "id"},
  "expect_column_values_in_range": {"column": "amount", "min": 0, "max": 1000}
}');

-- 8. dq_run + dq_reports
SELECT '8. dq_run' AS test;
SELECT dq_run('daily_sales', 'sales', '{
  "expect_table_row_count_between": {"min": 5, "max": 20},
  "expect_column_values_not_null": {"column": "id"},
  "expect_column_values_not_null": {"column": "amount"},
  "expect_column_values_unique": {"column": "id"},
  "expect_column_values_in_range": {"column": "amount", "min": 0, "max": 1000}
}');
SELECT '9. dq_reports' AS test;
SELECT * FROM dq_reports();

-- 10. error case: bad table
SELECT '10. expect on missing table' AS test;
SELECT * FROM expect_not_null('no_such_table', 'id');
