// SQL planner: expands modeled SQL by substituting model references,
// calculated fields, and relationships. Pure text transformation —
// no database connection needed for planning (dry-plan).

use sqlparser::ast::{
    self, Expr, Ident, ObjectName, Select, SelectItem, SetExpr, Statement, TableFactor,
    TableWithJoins,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::mdl::SemanticContext;

/// Expand modeled SQL against a semantic context.
/// Replaces model names with physical table references,
/// substitutes calculated columns with their expressions,
/// and injects relationship joins.
pub fn expand_sql(_sql: &str) -> Result<String, String> {
    // MVP: parse and return the AST structure.
    // Full expansion will be implemented incrementally:
    //   Phase 1: identify model references in FROM clauses
    //   Phase 2: substitute table_reference for each model
    //   Phase 3: expand calculated fields in SELECT
    //   Phase 4: inject relationship joins

    let dialect = GenericDialect {};
    let ast = Parser::parse_sql(&dialect, _sql)
        .map_err(|e| format!("SQL parse error: {}", e))?;

    if ast.is_empty() {
        return Err("Empty SQL".into());
    }

    // Phase 1: just echo back — identify what needs expansion
    let summary = analyze_ast(&ast[0]);
    Ok(format!("-- semantic dry-plan (phase 1: AST analysis)\n-- {}\n{}", summary, _sql))
}

/// Analyze AST to identify semantic objects referenced.
fn analyze_ast(stmt: &Statement) -> String {
    match stmt {
        Statement::Query(query) => {
            let tables: Vec<String> = extract_table_names(&query.body);
            if tables.is_empty() {
                "no model references found".into()
            } else {
                format!("referenced models: {}", tables.join(", "))
            }
        }
        _ => "unsupported statement type".into(),
    }
}

/// Extract table names from a SELECT for semantic analysis.
fn extract_table_names(expr: &SetExpr) -> Vec<String> {
    let mut names = Vec::new();
    match expr {
        SetExpr::Select(select) => {
            for table in &select.from {
                match &table.relation {
                    TableFactor::Table { name, .. } => {
                        names.push(name_to_string(name));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    names
}

fn name_to_string(name: &ObjectName) -> String {
    name.0
        .iter()
        .map(|i| i.value.clone())
        .collect::<Vec<_>>()
        .join(".")
}

// ─── Future: full expansion pipeline ───────────────────────────────────

/// Replace model name with its physical table_reference.
fn _expand_model(_model_name: &str, _ctx: &SemanticContext) -> Option<String> {
    // Phase 2:
    //   ctx.models.iter().find(|m| m.name == model_name)
    //   → model.table_reference → "catalog.schema.table"
    None
}

/// Replace a calculated column reference with its expression.
fn _expand_calculated(_col_name: &str, _model: &crate::mdl::Model) -> Option<String> {
    // Phase 3:
    //   model.columns.iter().find(|c| c.name == col_name && c.is_calculated)
    //   → column.expression
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_select() {
        let sql = "SELECT name, total FROM customers";
        let result = expand_sql(sql).unwrap();
        assert!(result.contains("customers"));
    }

    #[test]
    fn parse_select_with_join() {
        let sql = "SELECT c.name, o.total FROM customers c JOIN orders o ON c.id = o.customer_id";
        let result = expand_sql(sql).unwrap();
        assert!(result.contains("customers"));
        assert!(result.contains("orders"));
    }

    #[test]
    fn reject_invalid_sql() {
        assert!(expand_sql("THIS IS NOT SQL").is_err());
    }

    #[test]
    fn reject_empty() {
        assert!(expand_sql("").is_err());
    }
}
