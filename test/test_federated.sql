-- dq_federated: cross-DB quality baseline via duckdb_universal.
--
-- Requires duckdb_universal loaded (sqlite driver ships in the extension).
-- Two SQLite files simulate remote sources; a view bridge over
-- universal_foreign_table lets every dq assertion run against remote data.
--
-- Run:
--   python3 -c "import sqlite3; c=sqlite3.connect('/tmp/fed_a.db'); c.execute('CREATE TABLE IF NOT EXISTS orders(id INTEGER, tag TEXT)'); c.executemany('INSERT INTO orders VALUES (?,?)', [(1,'x'),(2,'y'),(2,'z')]); c.commit(); c.close(); c=sqlite3.connect('/tmp/fed_b.db'); c.execute('CREATE TABLE IF NOT EXISTS orders(id INTEGER, tag TEXT)'); c.executemany('INSERT INTO orders VALUES (?,?)', [(1,'x'),(1,'x'),(3,None)]); c.commit(); c.close()"
--   duckdb -unsigned < test/test_federated.sql

LOAD '/mnt/d/wsl2/duckdb_universal/build/release/universal.duckdb_extension';
LOAD './build/release/dq.duckdb_extension';

-- Two named remote connections (SQLite files as stand-ins for PG/MySQL)
SELECT * FROM universal_connect('src_a', 'sqlite', 'sqlite:///tmp/fed_a.db');
SELECT * FROM universal_connect('src_b', 'sqlite', 'sqlite:///tmp/fed_b.db');

-- Cross-DB quality baseline: same rule set against both sources
SELECT source, rule, passed, row_count, failed_count, error
FROM dq_federated('["src_a", "src_b"]', 'orders', '{
  "expect_column_values_not_null": {"column": "id"},
  "expect_column_values_unique": {"column": "id"},
  "expect_column_values_not_null": {"column": "tag"},
  "expect_table_row_count_between": {"min": 1, "max": 10}
}')
ORDER BY source, rule;
