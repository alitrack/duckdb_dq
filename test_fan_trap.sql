-- Integration test: Fan-trap detection
LOAD '/Users/m2max/tmp/duckdb_semantic/build/release/semantic.duckdb_extension';

-- Setup tables and MDL with non-key FK column
CREATE TABLE customers(id INTEGER PRIMARY KEY, name VARCHAR);
CREATE TABLE orders(id INTEGER PRIMARY KEY, customer_id INTEGER, total DECIMAL(10,2));

-- relationship uses orders.customer_id which is NOT a key column
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
        {"name": "name", "type": "VARCHAR"}
      ],
      "primaryKey": "id"
    },
    {
      "name": "orders",
      "tableReference": {"table": "orders"},
      "columns": [
        {"name": "id", "type": "INTEGER", "isPrimaryKey": true},
        {"name": "customer_id", "type": "INTEGER", "isPrimaryKey": false},
        {"name": "total", "type": "DECIMAL", "isPrimaryKey": false}
      ],
      "primaryKey": "id"
    }
  ],
  "relationships": [
    {
      "name": "customer_orders",
      "models": ["customers", "orders"],
      "joinType": "ONE_TO_MANY",
      "condition": "customers.id = orders.customer_id"
    }
  ],
  "views": []
}
');

-- Test 1: Fan-trap should be detected (orders.customer_id is NOT a key)
SELECT CASE
  WHEN semantic_dry_plan('SELECT customers.name, orders.total FROM customers, orders')
       LIKE 'Error: Fan trap%'
  THEN 'PASS' ELSE 'FAIL' END AS FT_detect_non_key;

-- Test 2: Same tables, but with key-based join should work
SELECT semantic_load('
{
  "catalog": "test", "schema": "main",
  "models": [
    {"name": "customers", "tableReference": {"table": "customers"},
     "columns": [{"name": "id", "type": "INTEGER", "isPrimaryKey": true}, {"name": "name", "type": "VARCHAR"}],
     "primaryKey": "id"},
    {"name": "orders", "tableReference": {"table": "orders"},
     "columns": [{"name": "id", "type": "INTEGER", "isPrimaryKey": true}, {"name": "customer_id", "type": "INTEGER"}, {"name": "total", "type": "DECIMAL"}],
     "primaryKey": "id"}
  ],
  "relationships": [{"name": "co", "models": ["customers","orders"], "joinType": "ONE_TO_MANY", "condition": "customers.id = orders.id"}],
  "views": []
}
');

SELECT CASE
  WHEN semantic_dry_plan('SELECT customers.name, orders.total FROM customers, orders')
       LIKE '%JOIN%'
  THEN 'PASS' ELSE 'FAIL' END AS FT_ok_for_key;

-- Cleanup
DROP TABLE IF EXISTS orders;
DROP TABLE IF EXISTS customers;
