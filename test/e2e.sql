-- E2E test for duckdb_semantic extension
-- Usage: duckdb -unsigned < test/e2e.sql

.timer on
.nullvalue NULL

.print ========================================
.print 1. Load extension & MDL
.print ========================================

LOAD 'build/debug/semantic.duckdb_extension';

SELECT semantic_load('{
  "catalog": "my_db",
  "schema": "public",
  "models": [
    {
      "name": "customers",
      "tableReference": {"table": "customers"},
      "columns": [
        {"name": "id", "type": "INTEGER"},
        {"name": "name", "type": "VARCHAR"},
        {"name": "email", "type": "VARCHAR"}
      ]
    },
    {
      "name": "orders",
      "tableReference": {"table": "orders"},
      "columns": [
        {"name": "id", "type": "INTEGER"},
        {"name": "customer_id", "type": "INTEGER"},
        {"name": "total", "type": "FLOAT"},
        {"name": "status", "type": "VARCHAR"}
      ]
    }
  ],
  "relationships": [
    {
      "name": "customer_orders",
      "models": ["customers", "orders"],
      "joinType": "many_to_one",
      "condition": "customers.id = orders.customer_id"
    }
  ],
  "views": []
}');

.print ========================================
.print 2. semantic_models()
.print ========================================

SELECT * FROM semantic_models();

.print ========================================
.print 3. semantic_dry_plan - simple
.print ========================================

SELECT semantic_dry_plan('SELECT name, email FROM customers');

.print ========================================
.print 4. semantic_query - with join
.print ========================================

SELECT * FROM semantic_query('SELECT customers.name, orders.total FROM customers JOIN orders ON customers.id = orders.customer_id');

.print ========================================
.print 5. L1: Vector index + search
.print ========================================

SELECT semantic_index_model('customers', '0.1,0.2,0.3,0.4,0.5');
SELECT semantic_index_model('orders', '0.5,0.4,0.3,0.2,0.1');
SELECT semantic_index_model('products', '0.2,0.3,0.1,0.5,0.4');

SELECT * FROM semantic_vector_search('0.1,0.2,0.3,0.4,0.5', 3);

.print ========================================
.print 6. L2: Graph edges + discovery
.print ========================================

SELECT semantic_graph_reset();
SELECT semantic_graph_add_edge('customers', 'orders', 'customers.id = orders.customer_id');
SELECT semantic_graph_add_edge('orders', 'order_items', 'orders.id = order_items.order_id');
SELECT semantic_graph_add_edge('order_items', 'products', 'order_items.product_id = products.id');

SELECT * FROM semantic_discover_relationships('customers');
SELECT * FROM semantic_shortest_path('customers', 'products');

.print ========================================
.print 7. L3: Ontology
.print ========================================

SELECT semantic_class_define('Person', '');
SELECT semantic_class_define('Customer', 'Person');
SELECT semantic_class_map('Customer', 'customers', '');
SELECT semantic_property_define('hasEmail', 'Customer', 'Email', 'email');

SELECT * FROM semantic_class_inheritance('Customer');
SELECT * FROM semantic_class_query('Customer');
SELECT semantic_ontology_export('ofn');

.print ========================================
.print 8. L4: Process context
.print ========================================

SELECT semantic_pattern_add('ecommerce_checkout', 'customers,orders,payments', 'ecommerce', 'Standard checkout flow');
SELECT semantic_pattern_add('order_fulfillment', 'orders,order_items,products', 'ecommerce', 'Order to shipment');

SELECT * FROM semantic_process_context('orders');
SELECT * FROM semantic_pattern_search('checkout', 3);
SELECT * FROM semantic_discover_patterns();

.print ========================================
.print 9. BM25: Index, search, remove
.print ========================================

SELECT semantic_bm25_reset();
SELECT semantic_bm25_index_doc('d1', 'DuckDB is an in-process analytical database');
SELECT semantic_bm25_index_doc('d2', 'PostgreSQL is a client-server relational database');
SELECT semantic_bm25_index_doc('d3', 'DuckDB supports full text search with BM25 scoring');
SELECT semantic_bm25_index_doc('d4', 'SQLite is an embedded database perfect for mobile apps');

SELECT * FROM semantic_bm25_search('duckdb analytical', 3);
SELECT * FROM semantic_bm25_search('database', 5);

SELECT semantic_bm25_remove_doc('d4');
SELECT * FROM semantic_bm25_search('database mobile', 5);

.print ========================================
.print 10. Hybrid fusion (dense + BM25 + graph)
.print ========================================

SELECT * FROM semantic_hybrid_search('0.1,0.2,0.3,0.4,0.5', 5, 0.50, 0.20, 'duckdb', 0.30);
SELECT * FROM semantic_hybrid_search('0.5,0.4,0.3,0.2,0.1', 5, 0.50, 0.20, '', 0.30);

.print ========================================
.print 11. Persistence: save + restore
.print ========================================

SELECT semantic_save('/tmp/semantic_snapshot.json');
SELECT semantic_bm25_reset();
SELECT semantic_graph_reset();
SELECT semantic_restore('/tmp/semantic_snapshot.json');

-- Verify BM25 survived
SELECT * FROM semantic_bm25_search('database', 3);
-- Verify graph survived
SELECT * FROM semantic_discover_relationships('customers');

.print ========================================
.print 12. Bulk BM25 indexing via SELECT
.print ========================================

SELECT semantic_bm25_reset();
SELECT semantic_bm25_index_doc('bulk1', 'fast in-process OLAP database');
SELECT semantic_bm25_index_doc('bulk2', 'lightweight embedded analytical engine');
SELECT semantic_bm25_index_doc('bulk3', 'PostgreSQL wire protocol compatible');
SELECT semantic_bm25_index_doc('bulk4', 'columnar storage vectorized execution');
SELECT semantic_bm25_index_doc('bulk5', 'parquet CSV JSON import support');

SELECT * FROM semantic_bm25_search('analytical OLAP', 3);
SELECT * FROM semantic_bm25_search('storage execution', 3);

.print ========================================
.print ALL TESTS PASSED
.print ========================================
