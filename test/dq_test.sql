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

-- 11. statistical assertions
SELECT '11. expect_min_between(amount, 0, 1000) — FAIL (min = -10)' AS test;
SELECT * FROM expect_min_between('sales', 'amount', 0, 1000);   -- FAIL (min = -10)
SELECT '12. expect_max_between(amount, 0, 1000) — PASS (max=500)' AS test;
SELECT * FROM expect_max_between('sales', 'amount', 0, 1000);   -- PASS
SELECT '13. expect_mean_between(amount, 0, 1000) — PASS (mean≈163.6)' AS test;
SELECT * FROM expect_mean_between('sales', 'amount', 0, 1000);  -- PASS
SELECT '14. expect_sum_between(amount, 1000, 2000) — PASS (sum=1145.5)' AS test;
SELECT * FROM expect_sum_between('sales', 'amount', 1000, 2000); -- PASS
SELECT '15. expect_distinct_count_between(id, 1, 7) — PASS (7 distinct)' AS test;
SELECT * FROM expect_distinct_count_between('sales', 'id', 1, 7); -- PASS
SELECT '16. expect_stddev_between(amount, 0, 300) — PASS' AS test;
SELECT * FROM expect_stddev_between('sales', 'amount', 0, 300);  -- PASS

-- 17. schema assertions
SELECT '17. expect_column_type(amount, DECIMAL(4,1)) — PASS' AS test;
SELECT * FROM expect_column_type('sales', 'amount', 'DECIMAL(4,1)');
SELECT '18. expect_column_type(amount, VARCHAR) — FAIL' AS test;
SELECT * FROM expect_column_type('sales', 'amount', 'VARCHAR');
SELECT '19. expect_table_column_count_between(3, 5) — PASS (3 cols)' AS test;
SELECT * FROM expect_table_column_count_between('sales', 3, 5);  -- PASS
SELECT '20. expect_table_column_count_between(1, 2) — FAIL (3 cols)' AS test;
SELECT * FROM expect_table_column_count_between('sales', 1, 2);  -- FAIL

-- 21. statistical rules in validate_expectations batch
SELECT '21. validate_expectations statistical rules' AS test;
SELECT rule, column_name, passed, row_count, failed_count, error
FROM validate_expectations('sales', '{
  "expect_table_column_count_between": {"min": 3, "max": 5},
  "expect_column_max_to_be_between": {"column": "amount", "min": 0, "max": 1000},
  "expect_column_mean_to_be_between": {"column": "amount", "min": 0, "max": 1000},
  "expect_column_sum_to_be_between": {"column": "amount", "min": 1000, "max": 2000},
  "expect_column_to_be_of_type": {"column": "amount", "type": "DECIMAL(4,1)"}
}');
