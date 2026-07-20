# duckdb_semantic

Semantic layer extension for DuckDB — expand modeled SQL against MDL definitions.

## What it does

Wrap your raw database tables in a semantic model, then write SQL against
business-meaningful names. The extension expands model references, calculated
fields, and relationships into executable physical SQL.

```sql
LOAD semantic;

-- Load your semantic model
SELECT semantic_load('target/mdl.json');

-- See what models are available
SELECT * FROM semantic_models();

-- Preview expanded SQL without executing
SELECT semantic_dry_plan('SELECT name, lifetime_value FROM customers');

-- Execute through the semantic layer
SELECT * FROM semantic_query('SELECT name, lifetime_value FROM customers');
```

## MDL Format

```json
{
  "catalog": "my_project",
  "schema": "main",
  "models": [
    {
      "name": "customers",
      "table_reference": {
        "catalog": "my_db",
        "schema": "public",
        "table": "customers"
      },
      "columns": [
        {"name": "id", "type": "INTEGER", "is_primary_key": true},
        {"name": "name", "type": "VARCHAR"},
        {
          "name": "lifetime_value",
          "type": "DECIMAL",
          "is_calculated": true,
          "expression": "(SELECT COALESCE(SUM(total), 0) FROM orders WHERE orders.customer_id = customers.id)"
        }
      ],
      "primary_key": "id"
    }
  ],
  "relationships": [],
  "views": []
}
```

## Build

```bash
cargo build --release
```

## License

MIT
