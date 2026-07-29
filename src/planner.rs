// SQL planner: expands modeled SQL by substituting model references,
// calculated fields, and relationships. Pure text transformation —
// no database connection needed for planning (dry-plan).

use sqlparser::ast::{
    ObjectName, SelectItem, SetExpr, Statement, TableFactor,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::mdl::SemanticContext;

/// Expand modeled SQL against a semantic context.
/// Phase 2: replaces model names with physical table_reference.
/// Phase 3: substitutes calculated columns with their expressions (TODO).
/// Phase 4: injects relationship joins (TODO).
pub fn expand_sql(sql: &str, ctx: &SemanticContext) -> Result<String, String> {
    let dialect = GenericDialect {};
    let mut ast = Parser::parse_sql(&dialect, sql)
        .map_err(|e| format!("SQL parse error: {}", e))?;

    if ast.is_empty() {
        return Err("Empty SQL".into());
    }

    // Walk the AST and expand model references
    expand_statement(&mut ast[0], ctx)?;

    let result = ast[0].to_string();
    Ok(result)
}

/// Recursively expand model references in a statement.
fn expand_statement(stmt: &mut Statement, ctx: &SemanticContext) -> Result<(), String> {
    match stmt {
        Statement::Query(query) => {
            expand_setexpr(&mut query.body, ctx)?;
        }
        _ => {} // unsupported statement types pass through unchanged
    }
    Ok(())
}

fn expand_setexpr(expr: &mut SetExpr, ctx: &SemanticContext) -> Result<(), String> {
    match expr {
        SetExpr::Select(select) => {
            // Phase 2: expand table references in FROM clause and all JOINs
            let mut models_in_from: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for table_with_joins in &mut select.from {
                expand_table_factor(&mut table_with_joins.relation, ctx)?;
                collect_model_name(&table_with_joins.relation, ctx, &mut models_in_from);
                for join in &mut table_with_joins.joins {
                    expand_table_factor(&mut join.relation, ctx)?;
                    collect_model_name(&join.relation, ctx, &mut models_in_from);
                }
            }
            // Phase 4: auto-inject relationship joins for implicit cross-joins
            inject_relationship_joins(select, ctx, &models_in_from)?;
            // Phase 3: expand calculated fields in SELECT
            let model_map = build_model_map(ctx, &select.from);
            for item in &mut select.projection {
                expand_select_item(item, &model_map);
            }
            // Phase 5: expand SELECT * to exclude hidden columns
            expand_wildcard(select, &model_map);
        }
        _ => {}
    }
    Ok(())
}

/// Build a map from alias/table name → model reference for SELECT expansion.
fn build_model_map<'a>(
    ctx: &'a SemanticContext,
    from: &[sqlparser::ast::TableWithJoins],
) -> std::collections::HashMap<String, &'a crate::mdl::Model> {
    let mut map = std::collections::HashMap::new();
    for twj in from {
        if let TableFactor::Table { name, alias, .. } = &twj.relation {
            let model_name = name_to_table_name(name);
            if let Some(model) = ctx.models.iter().find(|m| m.name == model_name) {
                let key = alias
                    .as_ref()
                    .map(|a| a.name.value.clone())
                    .unwrap_or_else(|| model_name.clone());
                map.insert(key, model);
            }
        }
    }
    map
}

/// Collect the semantic model name (if any) from a table factor.
fn collect_model_name(
    factor: &TableFactor,
    ctx: &SemanticContext,
    out: &mut std::collections::HashSet<String>,
) {
    if let TableFactor::Table { name, .. } = factor {
        let model_name = name_to_table_name(name);
        if ctx.models.iter().any(|m| m.name == model_name) {
            out.insert(model_name);
        }
    }
}

/// Auto-inject JOINs for implicit cross-joins.
fn inject_relationship_joins(
    select: &mut sqlparser::ast::Select,
    ctx: &SemanticContext,
    models_in_from: &std::collections::HashSet<String>,
) -> Result<(), String> {
    if ctx.relationships.is_empty() || models_in_from.len() < 2 {
        return Ok(());
    }
    for rel in &ctx.relationships {
        if rel.models.len() != 2 { continue; }
        let a = &rel.models[0];
        let b = &rel.models[1];
        if models_in_from.contains(a) && models_in_from.contains(b) {
            let already_joined = select.from.iter().any(|twj| {
                twj.joins.iter().any(|j| {
                    if let TableFactor::Table { name, .. } = &j.relation {
                        let n = name_to_table_name(name);
                        n == *b || n == *a
                    } else { false }
                })
            });
            if !already_joined {
                if let Some(twj) = select.from.first_mut() {
                    let join_expr = parse_join_condition(&rel.condition)?;
                    twj.joins.push(sqlparser::ast::Join {
                        relation: sqlparser::ast::TableFactor::Table {
                            name: sqlparser::ast::ObjectName(vec![
                                sqlparser::ast::Ident::new(b),
                            ]),
                            alias: None, args: None, with_hints: vec![],
                            version: None, partitions: vec![],
                            with_ordinality: false, json_path: None,
                        },
                        join_operator: sqlparser::ast::JoinOperator::Inner(
                            sqlparser::ast::JoinConstraint::On(join_expr),
                        ),
                        global: false,
                    });
                }
            }
        }
    }
    Ok(())
}

fn parse_join_condition(condition: &str) -> Result<sqlparser::ast::Expr, String> {
    let dialect = sqlparser::dialect::GenericDialect {};
    let mut parser = sqlparser::parser::Parser::new(&dialect)
        .try_with_sql(condition)
        .map_err(|e| format!("Parse error: {}", e))?;
    parser.parse_expr()
        .map_err(|e| format!("Condition parse error: {}", e))
}

/// Expand SELECT * to exclude hidden columns from models.
fn expand_wildcard(
    select: &mut sqlparser::ast::Select,
    model_map: &std::collections::HashMap<String, &crate::mdl::Model>,
) {
    use sqlparser::ast::SelectItem;
    let mut new_projection: Vec<SelectItem> = Vec::new();
    let mut expanded = false;

    for item in &select.projection {
        match item {
            SelectItem::Wildcard(_) => {
                // Check if any model has hidden columns
                let has_hidden = model_map.values().any(|m| {
                    m.columns.iter().any(|c| c.is_hidden)
                });
                if !has_hidden {
                    new_projection.push(item.clone());
                    continue;
                }
                for (alias, model) in model_map.iter() {
                    let mut cols: Vec<SelectItem> = Vec::new();
                    for col in &model.columns {
                        if !col.is_hidden {
                            cols.push(SelectItem::UnnamedExpr(
                                sqlparser::ast::Expr::CompoundIdentifier(vec![
                                    sqlparser::ast::Ident::new(alias),
                                    sqlparser::ast::Ident::new(&col.name),
                                ]),
                            ));
                        }
                    }
                    if !cols.is_empty() {
                        new_projection.extend(cols);
                        expanded = true;
                    }
                }
            }
            _ => {
                new_projection.push(item.clone());
            }
        }
    }

    if expanded {
        select.projection = new_projection;
    }
}

/// Expand a calculated field reference in a SELECT item.
fn expand_select_item(
    item: &mut SelectItem,
    model_map: &std::collections::HashMap<String, &crate::mdl::Model>,
) {
    match item {
        SelectItem::UnnamedExpr(expr) => {
            expand_expr(expr, model_map);
        }
        SelectItem::ExprWithAlias { expr, .. } => {
            expand_expr(expr, model_map);
        }
        _ => {}
    }
}

fn expand_expr(
    expr: &mut sqlparser::ast::Expr,
    model_map: &std::collections::HashMap<String, &crate::mdl::Model>,
) {
    // Helper: try parsing a calculated field expression and substitute it
    let try_substitute = |expr: &mut sqlparser::ast::Expr, exp_str: &str| -> bool {
        let dialect = sqlparser::dialect::GenericDialect {};
        if let Ok(mut parser) = sqlparser::parser::Parser::new(&dialect).try_with_sql(exp_str) {
            if let Ok(parsed) = parser.parse_expr() {
                *expr = parsed;
                return true;
            }
        }
        false
    };

    match expr {
        sqlparser::ast::Expr::CompoundIdentifier(idents) => {
            if idents.len() == 2 {
                let qualifier = &idents[0].value;
                let col_name = &idents[1].value;
                if let Some(model) = model_map.get(qualifier) {
                    if let Some(col) = model.columns.iter().find(|c| &c.name == col_name) {
                        if col.is_calculated {
                            if let Some(ref exp_str) = col.expression {
                                try_substitute(expr, exp_str);
                            }
                        }
                    }
                }
            }
        }
        sqlparser::ast::Expr::Identifier(ident) => {
            let col_name = &ident.value;
            for (_qual, model) in model_map.iter() {
                if let Some(col) = model.columns.iter().find(|c| &c.name == col_name) {
                    if col.is_calculated {
                        if let Some(ref exp_str) = col.expression {
                            try_substitute(expr, exp_str);
                        }
                        break;
                    }
                }
            }
        }
        _ => {}
    }
}

/// Replace model name with physical table_reference or ref_sql subquery.
fn expand_table_factor(
    factor: &mut TableFactor,
    ctx: &SemanticContext,
) -> Result<(), String> {
    // View check first (before any borrows)
    if let TableFactor::Table { name, alias, .. } = factor {
        let view_name = name_to_table_name(name);
        if let Some(view) = ctx.views.iter().find(|v| v.name == view_name) {
            let alias_clause = alias
                .as_ref()
                .map(|a| format!(" AS {}", a.name))
                .unwrap_or_default();
            let derived = format!("({}){}", view.statement, alias_clause);
            *factor = TableFactor::Table {
                name: sqlparser::ast::ObjectName(vec![
                    sqlparser::ast::Ident::new(derived),
                ]),
                alias: None, args: None, with_hints: vec![],
                version: None, partitions: vec![],
                with_ordinality: false, json_path: None,
            };
            return Ok(());
        }
    }

    match factor {
        TableFactor::Table { name, alias, .. } => {
            let table_name = name_to_table_name(name);
            if let Some(model) = ctx.models.iter().find(|m| m.name == table_name) {
                // ref_sql models → wrap as derived table (subquery)
                if let Some(ref ref_sql) = model.ref_sql {
                    let alias_clause = alias
                        .as_ref()
                        .map(|a| format!(" AS {}", a.name))
                        .unwrap_or_default();
                    let derived = format!("({}){}", ref_sql, alias_clause);
                    *factor = TableFactor::Table {
                        name: ObjectName(vec![sqlparser::ast::Ident::new(derived)]),
                        alias: None,
                        args: None,
                        with_hints: vec![],
                        version: None,
                        partitions: vec![],
                        with_ordinality: false,
                        json_path: None,
                    };
                }
                // table_reference models → replace with physical name
                else if let Some(ref tr) = model.table_reference {
                    let mut parts: Vec<sqlparser::ast::Ident> = Vec::new();
                    if let Some(ref catalog) = tr.catalog {
                        if !catalog.is_empty() {
                            parts.push(sqlparser::ast::Ident::new(catalog));
                        }
                    }
                    if let Some(ref schema) = tr.schema {
                        if !schema.is_empty() {
                            parts.push(sqlparser::ast::Ident::new(schema));
                        }
                    }
                    parts.push(sqlparser::ast::Ident::new(&tr.table));
                    *name = ObjectName(parts);
                }
            }
        }
        TableFactor::Derived { subquery, .. } => {
            expand_setexpr(&mut subquery.body, ctx)?;
        }
        _ => {}
    }
    Ok(())
}

/// Get the unqualified table name from an ObjectName.
fn name_to_table_name(name: &ObjectName) -> String {
    name.0.last().map(|i| i.value.clone()).unwrap_or_default()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdl::{Model, TableReference, Column};

    fn make_ctx() -> SemanticContext {
        SemanticContext {
            catalog: "test".into(),
            schema: "main".into(),
            models: vec![
                Model {
                    name: "customers".into(),
                    table_reference: Some(TableReference {
                        catalog: Some("my_db".into()),
                        schema: Some("public".into()),
                        table: "customers".into(),
                    }),
                    ref_sql: None,
                    columns: vec![
                        Column { name: "id".into(), col_type: "INTEGER".into(), is_calculated: false, expression: None, not_null: true, is_primary_key: true, description: None, is_hidden: false },
                        Column { name: "name".into(), col_type: "VARCHAR".into(), is_calculated: false, expression: None, not_null: false, is_primary_key: false, description: None, is_hidden: false },
                    ],
                    primary_key: Some("id".into()),
                    description: None,
                },
                Model {
                    name: "orders".into(),
                    table_reference: Some(TableReference {
                        catalog: None,
                        schema: Some("public".into()),
                        table: "orders".into(),
                    }),
                    ref_sql: None,
                    columns: vec![
                        Column { name: "id".into(), col_type: "INTEGER".into(), is_calculated: false, expression: None, not_null: true, is_primary_key: true, description: None, is_hidden: false },
                        Column { name: "total".into(), col_type: "DECIMAL".into(), is_calculated: false, expression: None, not_null: false, is_primary_key: false, description: None, is_hidden: false },
                    ],
                    primary_key: Some("id".into()),
                    description: None,
                },
            ],
            relationships: vec![],
            views: vec![],
        }
    }

    #[test]
    fn expand_simple_select() {
        let ctx = make_ctx();
        let result = expand_sql("SELECT name FROM customers", &ctx).unwrap();
        assert_eq!(result, "SELECT name FROM my_db.public.customers");
    }

    #[test]
    fn expand_select_with_alias() {
        let ctx = make_ctx();
        let result = expand_sql("SELECT c.name FROM customers c", &ctx).unwrap();
        assert_eq!(result, "SELECT c.name FROM my_db.public.customers AS c");
    }

    #[test]
    fn expand_select_no_catalog() {
        let ctx = make_ctx();
        let result = expand_sql("SELECT id FROM orders", &ctx).unwrap();
        assert_eq!(result, "SELECT id FROM public.orders");
    }


    #[test]
    fn expand_view_reference() {
        let mut ctx = make_ctx();
        ctx.views.push(crate::mdl::View {
            name: "active_customers".into(),
            statement: "SELECT * FROM my_db.public.customers WHERE active = true".into(),
        });

        let result = expand_sql(
            "SELECT name FROM active_customers",
            &ctx,
        )
        .unwrap();
        assert!(result.contains("active = true"));
        assert!(result.contains("my_db.public.customers"));
    }

    #[test]
    fn expand_wildcard_excludes_hidden() {
        let mut ctx = make_ctx();
        ctx.models[0].columns.push(crate::mdl::Column {
            name: "secret_notes".into(),
            col_type: "VARCHAR".into(),
            is_calculated: false,
            expression: None,
            not_null: false,
            is_primary_key: false,
            description: None,
            is_hidden: true,
        });

        let result = expand_sql(
            "SELECT * FROM customers",
            &ctx,
        )
        .unwrap();
        assert!(result.contains("customers.id"));
        assert!(result.contains("customers.name"));
        assert!(!result.contains("secret_notes"));
        assert!(!result.contains("*"));
    }

    #[test]
    fn expand_wildcard_no_hidden_columns_keeps_star() {
        let ctx = make_ctx();
        // Single model with no hidden columns — * stays
        let result = expand_sql(
            "SELECT * FROM customers",
            &ctx,
        )
        .unwrap();
        assert!(result.contains("*"));
    }

    #[test]
    fn passthrough_physical_table() {
        let ctx = make_ctx();
        // "accounts" is not a model — pass through unchanged
        let result = expand_sql("SELECT * FROM accounts", &ctx).unwrap();
        assert!(result.contains("accounts"));
        assert!(!result.contains("my_db"));
    }

    #[test]
    fn expand_join() {
        let ctx = make_ctx();
        let result = expand_sql(
            "SELECT c.name, o.total FROM customers c JOIN orders o ON c.id = o.id",
            &ctx,
        )
        .unwrap();
        eprintln!("JOIN GOT: {}", result);
        assert!(result.contains("my_db.public.customers"), "{}", result);
        assert!(result.contains("public.orders"), "{}", result);
    }

    #[test]
    fn reject_empty() {
        let ctx = make_ctx();
        assert!(expand_sql("", &ctx).is_err());
    }

    #[test]
    fn expand_with_calculated_field() {
        let mut ctx = make_ctx();
        ctx.models[0].columns.push(Column {
            name: "total_spent".into(),
            col_type: "DECIMAL".into(),
            is_calculated: true,
            expression: Some("(SELECT COALESCE(SUM(o.total), 0) FROM public.orders o WHERE o.customer_id = customers.id)".into()),
            not_null: false,
            is_primary_key: false,
            description: None,
            is_hidden: false,
        });

        let result = expand_sql(
            "SELECT customers.total_spent FROM customers",
            &ctx,
        )
        .unwrap();
        // Table reference expansion works
        assert!(result.contains("my_db.public.customers"));
        // Phase 3: calculated fields are expanded, so total_spent is replaced
        // by its expression — total_spent won't appear in output
        assert!(!result.contains("total_spent"));
    }

    #[test]

    #[test]

    #[test]
    fn expand_with_relationship_join() {
        let mut ctx = make_ctx();
        ctx.relationships.push(crate::mdl::Relationship {
            name: "customer_orders".into(),
            models: vec!["customers".into(), "orders".into()],
            join_type: "ONE_TO_MANY".into(),
            condition: "customers.id = orders.customer_id".into(),
        });

        let result = expand_sql(
            "SELECT customers.name, orders.total FROM customers, orders",
            &ctx,
        )
        .unwrap();
        eprintln!("REL JOIN: {}", result);
        assert!(result.contains("JOIN"));
        assert!(result.contains("customers.id = orders.customer_id"));
        assert!(result.contains("my_db.public.customers"));
    }

    #[test]
    fn expand_no_join_when_explicit_join_exists() {
        let mut ctx = make_ctx();
        ctx.relationships.push(crate::mdl::Relationship {
            name: "customer_orders".into(),
            models: vec!["customers".into(), "orders".into()],
            join_type: "ONE_TO_MANY".into(),
            condition: "customers.id = orders.customer_id".into(),
        });

        let result = expand_sql(
            "SELECT c.name, o.total FROM customers c JOIN orders o ON c.id = o.customer_id",
            &ctx,
        )
        .unwrap();
        // Should only have one JOIN (the explicit one), not inject a duplicate
        let join_count = result.matches("JOIN").count();
        assert_eq!(join_count, 1, "Should not inject duplicate JOIN: {}", result);
    }

    #[test]
    fn expand_with_ref_sql_model() {
        let mut ctx = make_ctx();
        ctx.models[0].table_reference = None;
        ctx.models[0].ref_sql = Some(
            "SELECT id, UPPER(name) AS name FROM my_db.public.customers WHERE active = true"
                .into(),
        );

        let result = expand_sql("SELECT name FROM customers", &ctx).unwrap();
        assert!(result.contains("UPPER(name)"));
        assert!(result.contains("active = true"));
    }
}
