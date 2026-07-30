-- Integration tests for duckdb_semantic core loop
-- Tests: load, single table, calculated column, multi-table JOIN, passthrough

LOAD '/Users/m2max/tmp/duckdb_semantic/build/release/semantic.duckdb_extension';

-- Setup test data
CREATE TABLE customers(id INTEGER, name VARCHAR, region VARCHAR);
INSERT INTO customers VALUES (1, 'Alice', 'US'), (2, 'Bob', 'US'), (3, 'Charlie', 'EU');

CREATE TABLE orders(id INTEGER, customer_id INTEGER, total DECIMAL(10,2));
INSERT INTO orders VALUES (1, 1, 100.00), (2, 1, 50.00), (3, 2, 200.00), (4, 3, 150.00);

-- Load MDL
SELECT semantic_load('
{
  "catalog": "test",
  "schema": "main",
  "models": [
    {
      "name": "customers",
      "tableReference": {"table": "customers"},
      "columns": [
        {"name": "id", "type": "INTEGER", "isPrimaryKey": true},
        {"name": "name", "type": "VARCHAR"},
        {"name": "region", "type": "VARCHAR"},
        {"name": "lifetime_value", "type": "DECIMAL", "isCalculated": true,
         "expression": "(SELECT COALESCE(SUM(total), 0) FROM orders WHERE orders.customer_id = customers.id)"}
      ],
      "primaryKey": "id"
    },
    {
      "name": "orders",
      "tableReference": {"table": "orders"},
      "columns": [
        {"name": "id", "type": "INTEGER", "isPrimaryKey": true},
        {"name": "customer_id", "type": "INTEGER"},
        {"name": "total", "type": "DECIMAL"}
      ],
      "primaryKey": "id"
    }
  ],
  "relationships": [
    {
      "name": "customer_orders",
      "models": ["customers", "orders"],
      "joinType": "LEFT",
      "condition": "customers.id = orders.id"
    }
  ],
  "views": []
}
');

-- TEST 1: Single table expansion
SELECT CASE
  WHEN semantic_dry_plan('SELECT name FROM customers') = 'SELECT name FROM customers'
  THEN 'PASS' ELSE 'FAIL' END AS test_single_table;

-- TEST 2: Calculated column expansion
SELECT CASE
  WHEN semantic_dry_plan('SELECT name, lifetime_value FROM customers')
       LIKE '%SELECT COALESCE(SUM(total)%'
  THEN 'PASS' ELSE 'FAIL' END AS test_calc_col;

-- TEST 3: Multi-table implicit join (BUG FIX: no duplicate table)
SELECT CASE
  WHEN semantic_dry_plan('SELECT customers.name, orders.total FROM customers, orders')
       = 'SELECT customers.name, orders.total FROM customers JOIN orders ON customers.id = orders.id'
  THEN 'PASS' ELSE 'FAIL' END AS test_multi_table_join;

-- TEST 4: Passthrough physical table
SELECT CASE
  WHEN semantic_dry_plan('SELECT * FROM physical_table LIMIT 1') = 'SELECT * FROM physical_table LIMIT 1'
  THEN 'PASS' ELSE 'FAIL' END AS test_passthrough;

-- TEST 5: semantic_models listing
SELECT CASE WHEN COUNT(*) = 2 THEN 'PASS' ELSE 'FAIL' END AS test_models_count
FROM semantic_models();

-- TEST 6: semantic_query table function
SELECT CASE
  WHEN (SELECT expanded_sql FROM semantic_query('SELECT name FROM customers')) = 'SELECT name FROM customers'
  THEN 'PASS' ELSE 'FAIL' END AS test_semantic_query;

-- Cleanup
DROP TABLE IF EXISTS orders;
DROP TABLE IF EXISTS customers;
