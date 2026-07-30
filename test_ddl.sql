-- Test CREATE SEMANTIC VIEW DDL + semantic_view_expand
LOAD '/Users/m2max/tmp/duckdb_semantic/build/release/semantic.duckdb_extension';

-- Create a semantic view using DDL
SELECT semantic_create_view(
  'CREATE SEMANTIC VIEW sales AS TABLES (d AS demo PRIMARY KEY (region)) DIMENSIONS (d.region AS d.region) METRICS (d.revenue AS SUM(d.amount))'
);

-- Expand with dimensions + metrics
SELECT '--- expand (region, revenue) ---' as info;
SELECT semantic_view_expand('sales', 'region', 'revenue') as sql;

-- Test 1: verify expanded SQL contains GROUP BY
SELECT CASE
  WHEN semantic_view_expand('sales', 'region', 'revenue') LIKE '%GROUP BY%'
  THEN 'PASS' ELSE 'FAIL' END AS DDL_1_group_by;

-- Test 2: verify SUM appears
SELECT CASE
  WHEN semantic_view_expand('sales', 'region', 'revenue') LIKE '%SUM(d.amount)%'
  THEN 'PASS' ELSE 'FAIL' END AS DDL_2_sum;

-- Test 3: verify FROM clause
SELECT CASE
  WHEN semantic_view_expand('sales', 'region', 'revenue') LIKE '%demo AS d%'
  THEN 'PASS' ELSE 'FAIL' END AS DDL_3_from;

-- Test 4: unknown view error
SELECT CASE
  WHEN semantic_view_expand('nonexistent', 'x', 'y') LIKE 'Error%'
  THEN 'PASS' ELSE 'FAIL' END AS DDL_4_error;

-- Test 5: parse error
SELECT CASE
  WHEN semantic_create_view('CREATE SEMANTIC VIEW bad AS DIMENSIONS (a.b)') LIKE 'Error%'
  THEN 'PASS' ELSE 'FAIL' END AS DDL_5_bad_ddl;
