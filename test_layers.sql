-- Integration tests for duckdb_semantic L1-L3 + BM25
-- Each test returns PASS or FAIL.

LOAD '/Users/m2max/tmp/duckdb_semantic/build/release/semantic.duckdb_extension';

-- ================================================================
-- L1: Vector Search (cosine similarity)
-- ================================================================
SELECT '=== L1: VECTOR SEARCH ===' as section;

SELECT semantic_index_model('customers', '0.1,0.8,0.3,-0.2');
SELECT semantic_index_model('orders', '0.2,0.1,0.9,0.0');
SELECT semantic_index_model('products', '0.05,0.85,0.25,-0.1');
SELECT semantic_index_model('inventory', '0.9,0.05,0.05,0.1');

-- Search with a query vector close to customers/products
SELECT 'L1_test1_top_3' as test, model_name, score
FROM semantic_vector_search('0.1,0.8,0.3,-0.2', 3);

-- Verify ordering: customers (perfect match) should be #1, products #2
SELECT CASE
  WHEN (SELECT model_name FROM semantic_vector_search('0.1,0.8,0.3,-0.2', 1)) = 'customers'
  THEN 'PASS' ELSE 'FAIL' END AS L1_exact_match;

SELECT CASE
  WHEN (SELECT COUNT(*) FROM semantic_vector_search('0.1,0.8,0.3,-0.2', 2)) = 2
  THEN 'PASS' ELSE 'FAIL' END AS L1_topk_count;

-- ================================================================
-- BM25: Full-Text Search (Okapi)
-- ================================================================
SELECT '=== L1b: BM25 SEARCH ===' as section;

-- Set English stemmer
SELECT semantic_bm25_stemmer('english');

-- Index documents
SELECT semantic_bm25_index_doc('d1', 'DuckDB is a fast analytical database for querying data');
SELECT semantic_bm25_index_doc('d2', 'PostgreSQL is a relational database system');
SELECT semantic_bm25_index_doc('d3', 'DuckDB runs analytical queries very fast on large datasets');
SELECT semantic_bm25_index_doc('d4', 'MongoDB is a document database for unstructured data');

-- Search for "duckdb analytical fast"
SELECT 'BM25_test1' as test, doc_id, bm25_score
FROM semantic_bm25_search('duckdb analytical fast', 5);

-- d1 and d3 should appear (match duckdb/analytical/fast), d2 and d4 should not
SELECT CASE
  WHEN (SELECT COUNT(*) FROM semantic_bm25_search('duckdb analytical fast', 5)) >= 2
  THEN 'PASS' ELSE 'FAIL' END AS BM25_multi_match;

-- Stemmer test: "running" should match "runs"
SELECT semantic_bm25_index_doc('s1', 'The database runs analytical queries');
SELECT semantic_bm25_index_doc('s2', 'Running is good exercise');

SELECT CASE
  WHEN (SELECT COUNT(*) FROM semantic_bm25_search('running', 5)) >= 2
  THEN 'PASS' ELSE 'FAIL' END AS BM25_stemmer;

-- Remove test
SELECT semantic_bm25_remove_doc('s1');
SELECT semantic_bm25_remove_doc('s2');
-- Verify s1 and s2 are gone (d3 still matches via stemmer: "runs"→"run")
SELECT CASE
  WHEN (SELECT COUNT(*) FROM semantic_bm25_search('running', 5)
        WHERE doc_id IN ('s1', 's2')) = 0
  THEN 'PASS' ELSE 'FAIL' END AS BM25_remove;

-- Reset BM25 to clean state
SELECT semantic_bm25_reset();

-- ================================================================
-- L2: Graph — Relationship Discovery + Shortest Path
-- ================================================================
SELECT '=== L2: GRAPH ===' as section;

-- Build a graph: customers → orders → order_items → products
--                        → invoices
SELECT semantic_graph_reset();
SELECT semantic_graph_add_edge('customers', 'orders', 'customers.id = orders.customer_id');
SELECT semantic_graph_add_edge('orders', 'order_items', 'orders.id = order_items.order_id');
SELECT semantic_graph_add_edge('order_items', 'products', 'order_items.product_id = products.id');
SELECT semantic_graph_add_edge('customers', 'invoices', 'customers.id = invoices.customer_id');

-- Test 1: Discover relationships from customers
SELECT 'L2_test1_discover' as test, target_model, distance, join_condition
FROM semantic_discover_relationships('customers')
ORDER BY distance;

-- Verify: orders(d=1), invoices(d=1), order_items(d=2), products(d=3)
SELECT CASE
  WHEN (SELECT COUNT(*) FROM semantic_discover_relationships('customers')) = 4
  THEN 'PASS' ELSE 'FAIL' END AS L2_discover_count;

-- Test 2: Shortest path from customers to products
SELECT 'L2_test2_path' as test, edge, join_condition
FROM semantic_shortest_path('customers', 'products');

-- Path should be: customers→orders, orders→order_items, order_items→products
SELECT CASE
  WHEN (SELECT COUNT(*) FROM semantic_shortest_path('customers', 'products')) = 3
  THEN 'PASS' ELSE 'FAIL' END AS L2_path_length;

-- Test 3: Non-existent path
SELECT CASE
  WHEN (SELECT COUNT(*) FROM semantic_shortest_path('products', 'unknown')) = 0
  THEN 'PASS' ELSE 'FAIL' END AS L2_no_path;

-- ================================================================
-- L3: Ontology — Class Hierarchy + Inheritance + OFN Export
-- ================================================================
SELECT '=== L3: ONTOLOGY ===' as section;

-- Define taxonomy: Thing → LivingThing → Person → Employee
SELECT semantic_class_define('Thing', '');
SELECT semantic_class_define('LivingThing', 'Thing');
SELECT semantic_class_define('Person', 'LivingThing');
SELECT semantic_class_define('Employee', 'Person');
SELECT semantic_class_define('Manager', 'Employee');

-- Map classes to physical models
SELECT semantic_class_map('Employee', 'staff_table', 'active = true');
SELECT semantic_class_map('Person', 'people_table', '');

-- Define properties
SELECT semantic_property_define('hasName', 'Person', 'xsd:string', 'people_table.name');
SELECT semantic_property_define('worksAt', 'Employee', 'Department', '');

-- Test 1: Manager has NO direct mapping, should inherit Person→people_table
SELECT CASE
  WHEN (SELECT expanded_sql FROM semantic_class_query('Manager'))
       LIKE '%people_table%'
  THEN 'PASS' ELSE 'FAIL' END AS L3_inherited_mapping;

-- Test 2: Inheritance chain for Manager
SELECT 'L3_test2_inheritance' as test, class, depth, kind, detail
FROM semantic_class_inheritance('Manager');

-- Should show: self, is-a(Employee,Person,LivingThing,Thing), Employee→Department
SELECT CASE
  WHEN (SELECT COUNT(*) FROM semantic_class_inheritance('Manager')) >= 5
  THEN 'PASS' ELSE 'FAIL' END AS L3_inheritance_count;

-- Test 3: OFN ontology export
SELECT 'L3_test3_ofn' as test, semantic_ontology_export('ofn') as ofn_output;

SELECT CASE
  WHEN (SELECT semantic_ontology_export('ofn')) LIKE '%SubClassOf%'
  THEN 'PASS' ELSE 'FAIL' END AS L3_ofn_export;

-- ================================================================
-- Persistence: Save / Restore
-- ================================================================
SELECT '=== PERSISTENCE ===' as section;

SELECT semantic_save('/tmp/semantic_snapshot.json');
SELECT semantic_restore('/tmp/semantic_snapshot.json');

-- Verify restore worked: graph should be intact
SELECT CASE
  WHEN (SELECT COUNT(*) FROM semantic_discover_relationships('customers')) = 4
  THEN 'PASS' ELSE 'FAIL' END AS P_snapshot_restore;
