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

-- 22. proportion assertions
SELECT '22. expect_null_proportion_between(amount, 0.1, 0.5) — PASS (1/8=0.125)' AS test;
SELECT * FROM expect_null_proportion_between('sales', 'amount', 0.1, 0.5);  -- PASS
SELECT '23. expect_unique_proportion_between(id, 0.5, 1.0) — PASS (7/8=0.875)' AS test;
SELECT * FROM expect_unique_proportion_between('sales', 'id', 0.5, 1.0);    -- PASS

-- 24. quantile assertion
SELECT '24. expect_quantile_between(amount, 0.5, 0, 300) — PASS (median≈187.5)' AS test;
SELECT * FROM expect_quantile_between('sales', 'amount', 0.5, 0, 300);      -- PASS

-- 25. composite uniqueness: (id, order_date) unique — PASS; dup_pairs has real dup
SELECT '25. expect_columns_unique_together(id, order_date) — PASS' AS test;
SELECT * FROM expect_columns_unique_together('sales', 'id', 'order_date');  -- PASS
CREATE OR REPLACE TABLE dup_pairs AS
SELECT * FROM (VALUES (1, 'a'), (1, 'a'), (2, 'b')) t(k, v);
SELECT '26. expect_columns_unique_together(k, v) on dup_pairs — FAIL (1 dupe)' AS test;
SELECT * FROM expect_columns_unique_together('dup_pairs', 'k', 'v');  -- FAIL (1 dupe)

-- 27. proportion/quantile rules in validate_expectations batch
SELECT '27. validate_expectations proportion rules' AS test;
SELECT rule, column_name, passed, row_count, failed_count, error
FROM validate_expectations('sales', '{
  "expect_column_null_proportion_to_be_between": {"column": "amount", "min": 0.1, "max": 0.5},
  "expect_column_unique_proportion_to_be_between": {"column": "id", "min": 0.5, "max": 1.0},
  "expect_column_quantile_to_be_between": {"column": "amount", "quantile": 0.5, "min": 0, "max": 300},
  "expect_columns_unique_together": {"columns": ["id", "order_date"]}
}');

-- 28. GX-parity: string length
CREATE OR REPLACE TABLE names AS
SELECT * FROM (VALUES ('alice'), ('bob'), (NULL), ('charlie')) t(name);
SELECT '28. expect_column_length_between(name, 2, 7) — PASS (len 3..7)' AS test;
SELECT * FROM expect_column_length_between('names', 'name', 2, 7);   -- PASS
SELECT '29. expect_column_length_between(name, 4, 7) — FAIL (bob=3)' AS test;
SELECT * FROM expect_column_length_between('names', 'name', 4, 7);   -- FAIL

-- 30. GX-parity: null count
SELECT '30. expect_null_count_between(amount, 0, 2) — PASS (1 null)' AS test;
SELECT * FROM expect_null_count_between('sales', 'amount', 0, 2);    -- PASS
SELECT '31. expect_null_count_between(amount, 2, 5) — FAIL (1 null)' AS test;
SELECT * FROM expect_null_count_between('sales', 'amount', 2, 5);    -- FAIL

-- 32. GX-parity: exact row count
SELECT '32. expect_row_count_to_equal(sales, 8) — PASS' AS test;
SELECT * FROM expect_row_count_to_equal('sales', 8);                 -- PASS
SELECT '33. expect_row_count_to_equal(sales, 10) — FAIL' AS test;
SELECT * FROM expect_row_count_to_equal('sales', 10);                -- FAIL

-- 34. GX-parity rules in validate_expectations batch
SELECT '34. validate_expectations GX-parity rules' AS test;
SELECT rule, column_name, passed, row_count, failed_count, error
FROM validate_expectations('sales', '{
  "expect_column_null_count_to_be_between": {"column": "amount", "min": 0, "max": 2},
  "expect_table_row_count_to_equal": {"value": 8}
}');
SELECT rule, column_name, passed, row_count, failed_count, error
FROM validate_expectations('names', '{
  "expect_column_value_lengths_to_be_between": {"column": "name", "min_length": 2, "max_length": 7}
}');

-- 35. set membership (negated)
SELECT '35. expect_not_in_set(status, "closed,pending") — PASS (no such values)' AS test;
CREATE OR REPLACE TABLE orders2 AS
SELECT * FROM (VALUES ('open'), ('open'), ('cancelled'), ('shipped')) t(status);
SELECT * FROM expect_not_in_set('orders2', 'status', 'closed,pending');  -- PASS
SELECT '36. expect_not_in_set(status, "open,cancelled") — FAIL (3 rows)' AS test;
SELECT * FROM expect_not_in_set('orders2', 'status', 'open,cancelled');  -- FAIL (3)

-- 37. negated regex
SELECT '37. expect_not_match_regex(name, "^b") — FAIL (bob matches)' AS test;
SELECT * FROM expect_not_match_regex('names', 'name', '^b');            -- FAIL (1)
SELECT '38. expect_not_match_regex(name, "^z") — PASS' AS test;
SELECT * FROM expect_not_match_regex('names', 'name', '^z');            -- PASS
-- 39. date format
CREATE OR REPLACE TABLE events AS
SELECT * FROM (VALUES ('2026-01-15'), ('2026-02-01'), ('not-a-date')) t(event_date);
SELECT '39. expect_match_date_format(event_date, "%Y-%m-%d") — FAIL (1 bad)' AS test;
SELECT * FROM expect_match_date_format('events', 'event_date', '%Y-%m-%d');  -- FAIL (1)
SELECT '40. expect_match_date_format(event_date, "%Y/%m/%d") — FAIL (2 bad)' AS test;
SELECT * FROM expect_match_date_format('events', 'event_date', '%Y/%m/%d');  -- FAIL (2)

-- 41. sorted
CREATE OR REPLACE TABLE seq AS SELECT * FROM (VALUES (1), (2), (2), (3)) t(n);
CREATE OR REPLACE TABLE unsorted AS SELECT * FROM (VALUES (3), (1), (2)) t(n);
SELECT '41. expect_sorted(seq, n, asc) — PASS' AS test;
SELECT * FROM expect_sorted('seq', 'n', 'asc');                         -- PASS
SELECT '42. expect_sorted(unsorted, n, asc) — FAIL (1 inversion)' AS test;
SELECT * FROM expect_sorted('unsorted', 'n', 'asc');                    -- FAIL (1)

-- 43. median
SELECT '43. expect_median_between(amount, 50, 150) — PASS (median=100)' AS test;
SELECT * FROM expect_median_between('sales', 'amount', 50, 150);        -- PASS
SELECT '44. expect_median_between(amount, 200, 300) — FAIL (median=100)' AS test;
SELECT * FROM expect_median_between('sales', 'amount', 200, 300);       -- FAIL

-- 45. GX batch 2 rules in validate_expectations
SELECT '45. validate_expectations GX batch 2 rules' AS test;
SELECT rule, column_name, passed, row_count, failed_count, error
FROM validate_expectations('events', '{
  "expect_column_values_to_match_strftime_format": {"column": "event_date", "strftime_format": "%Y-%m-%d"}
}');
SELECT rule, column_name, passed, row_count, failed_count, error
FROM validate_expectations('orders2', '{
  "expect_column_values_to_not_be_in_set": {"column": "status", "value_set": ["closed", "pending"]},
  "expect_column_values_to_be_sorted": {"column": "status", "descending": false}
}');  -- not_in_set PASS; sorted FAIL (open,open,cancelled,shipped not ascending)
SELECT rule, column_name, passed, row_count, failed_count, error
FROM validate_expectations('sales', '{
  "expect_column_median_to_be_between": {"column": "amount", "min": 100, "max": 250},
  "expect_column_values_to_not_match_regex": {"column": "id", "pattern": "^9"}
}');

-- 46. dq_dashboard: emits dash-compatible JSON + summary metrics
SELECT '46. dq_dashboard(table, rules) — JSON valid, metrics correct' AS test;
SELECT checks, passed, failed, round(pass_rate::DOUBLE, 2) AS pass_rate,
       json_valid(dashboard_json) AS json_ok,
       (dashboard_json LIKE '%"panels"%') AS has_panels
FROM dq_dashboard('sales', '{
  "expect_table_row_count_between": {"min": 5, "max": 10},
  "expect_column_values_not_null": {"column": "id"},
  "expect_column_values_unique": {"column": "id"},
  "expect_column_values_in_range": {"column": "amount", "min": 0, "max": 100000}
}');  -- 4 checks; unique FAILS (7/8) + in_range FAILS (-10 < 0) → failed=2, pass_rate=0.5

