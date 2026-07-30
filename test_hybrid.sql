-- Integration test: semantic_hybrid_search — 3-way fusion
-- Signature (7 params):
--   semantic_hybrid_search(query_vec, k, dense_w, graph_w, bm25_query, bm25_w, hub)
--   → table: model_name, dense_score, bm25_score, graph_score, fused_score

LOAD '/Users/m2max/tmp/duckdb_semantic/build/release/semantic.duckdb_extension';

-- Setup
SELECT semantic_index_model('customers', '0.1,0.9,0.2,-0.1');
SELECT semantic_index_model('orders', '0.2,0.1,0.9,0.0');
SELECT semantic_index_model('products', '0.08,0.88,0.22,-0.05');
SELECT semantic_index_model('inventory', '0.95,0.05,0.0,0.1');

SELECT semantic_bm25_stemmer('english');
SELECT semantic_bm25_index_doc('customers', 'customer profile with names addresses and account history');
SELECT semantic_bm25_index_doc('orders', 'order tracking system with purchase status and shipping details');
SELECT semantic_bm25_index_doc('products', 'product catalog with customer preferences and order history tracking');
SELECT semantic_bm25_index_doc('inventory', 'warehouse stock levels and supply chain logistics');

SELECT semantic_graph_reset();
SELECT semantic_graph_add_edge('customers', 'orders', 'customers.id = orders.customer_id');
SELECT semantic_graph_add_edge('customers', 'products', 'customers.fav_product = products.id');

-- ================================================================
-- Test 1: Pure dense (dw=1.0, gw=0.0, bw=0.0, hub='')
-- ================================================================
SELECT CASE
  WHEN (SELECT model_name FROM semantic_hybrid_search(
    '0.1,0.9,0.2,-0.1', 1,
    1.0::FLOAT, 0.0::FLOAT, '', 0.0::FLOAT, ''
  )) = 'customers'
  THEN 'PASS' ELSE 'FAIL' END AS H_pure_dense_top1;

-- ================================================================
-- Test 2: Pure BM25 (dw=0, gw=0, bw=1.0)
-- ================================================================
SELECT CASE
  WHEN (SELECT COUNT(*) FROM semantic_hybrid_search(
    '0.1,0.9,0.2,-0.1', 4,
    0.0::FLOAT, 0.0::FLOAT, 'customer order history', 1.0::FLOAT, ''
  )) >= 3
  THEN 'PASS' ELSE 'FAIL' END AS H_pure_bm25_count;

-- ================================================================
-- Test 3: 3-way fusion with hub='customers'
-- ================================================================
SELECT '--- 3-way fusion results ---' as info;
SELECT model_name,
       ROUND(dense_score, 3) as dense,
       ROUND(bm25_score, 3) as bm25,
       ROUND(graph_score, 3) as graph,
       ROUND(fused_score, 3) as fused
FROM semantic_hybrid_search(
    '0.1,0.9,0.2,-0.1', 4,
    0.5::FLOAT, 0.2::FLOAT, 'customer order history', 0.3::FLOAT, 'customers'
);

-- inventory (no edge to hub) → graph=0
SELECT CASE
  WHEN (SELECT graph_score FROM semantic_hybrid_search(
    '0.1,0.9,0.2,-0.1', 4,
    0.5::FLOAT, 0.2::FLOAT, 'customer order history', 0.3::FLOAT, 'customers'
  ) WHERE model_name = 'inventory') = 0.0
  THEN 'PASS' ELSE 'FAIL' END AS H_fusion_inventory_zero;

-- orders (edge from hub) → graph>0
SELECT CASE
  WHEN (SELECT graph_score FROM semantic_hybrid_search(
    '0.1,0.9,0.2,-0.1', 4,
    0.5::FLOAT, 0.2::FLOAT, 'customer order history', 0.3::FLOAT, 'customers'
  ) WHERE model_name = 'orders') > 0.0
  THEN 'PASS' ELSE 'FAIL' END AS H_fusion_orders_graph;

-- ================================================================
-- Test 4: Graph-heavy (gw=0.8), verify graph component works
-- ================================================================
SELECT CASE
  WHEN (SELECT graph_score FROM semantic_hybrid_search(
    '0.1,0.9,0.2,-0.1', 4,
    0.1::FLOAT, 0.8::FLOAT, 'customer order history', 0.1::FLOAT, 'customers'
  ) WHERE model_name = 'inventory') = 0.0
  THEN 'PASS' ELSE 'FAIL' END AS H_graph_inventory_zero;

-- products (edge from hub) → graph>0 even with graph-heavy
SELECT CASE
  WHEN (SELECT graph_score FROM semantic_hybrid_search(
    '0.1,0.9,0.2,-0.1', 4,
    0.1::FLOAT, 0.8::FLOAT, 'customer order history', 0.1::FLOAT, 'customers'
  ) WHERE model_name = 'products') > 0.0
  THEN 'PASS' ELSE 'FAIL' END AS H_graph_products_connected;
