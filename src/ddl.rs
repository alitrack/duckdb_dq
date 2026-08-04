//! DDL layer: CREATE SEMANTIC VIEW … AS TABLES (…) DIMENSIONS (…) METRICS (…)
//!
//! Maps SQL DDL definitions into the SemanticContext for `semantic_view_expand`.
//!
//! Functions:
//!   semantic_create_view(ddl_text)        → parse + store a semantic view
//!   semantic_view_expand(view, dims, met) → generate GROUP BY SQL

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;

/// A table declared in a semantic view.
#[derive(Debug, Clone)]
pub struct ViewTable {
    pub alias: String,
    pub table_name: String,
    /// Reserved — currently unused in this crate.
    #[allow(dead_code)]
    pub primary_key: Vec<String>,
}

/// A dimension column (used in GROUP BY / SELECT).
#[derive(Debug, Clone)]
pub struct ViewDim {
    pub qualifier: String,
    pub col_name: String,
    /// AS alias, defaults to col_name
    pub alias: String,
}

/// A metric (aggregation expression).
#[derive(Debug, Clone)]
pub struct ViewMetric {
    pub qualifier: String,
    pub name: String,
    /// e.g. "SUM(d.amount)"
    pub expression: String,
}

/// A complete semantic view definition.
#[derive(Debug, Clone)]
pub struct SemanticView {
    pub name: String,
    pub tables: Vec<ViewTable>,
    pub dimensions: Vec<ViewDim>,
    pub metrics: Vec<ViewMetric>,
}

/// Global store: view_name → SemanticView
static VIEWS: Lazy<Mutex<HashMap<String, SemanticView>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub fn get_views() -> &'static Mutex<HashMap<String, SemanticView>> {
    &VIEWS
}

// ─── DDL parser ──────────────────────────────────────────────────────────

/// Parse "CREATE SEMANTIC VIEW <name> AS TABLES (...) DIMENSIONS (...) METRICS (...)"
pub fn parse_ddl(input: &str) -> Result<SemanticView, String> {
    let s = input.trim();

    // Strip "CREATE SEMANTIC VIEW "
    let rest = s
        .strip_prefix("CREATE SEMANTIC VIEW ")
        .or_else(|| s.strip_prefix("create semantic view "))
        .ok_or("Expected: CREATE SEMANTIC VIEW <name> AS ...")?;

    // Split name from body at " AS "
    let (name_part, body) = split_at_keyword(rest, " AS ")
        .or_else(|| split_at_keyword(rest, " as "))
        .ok_or("Expected 'AS' after view name")?;

    let name = name_part.trim().to_string();
    if name.is_empty() {
        return Err("View name is empty".into());
    }

    let mut tables: Vec<ViewTable> = Vec::new();
    let mut dimensions: Vec<ViewDim> = Vec::new();
    let mut metrics: Vec<ViewMetric> = Vec::new();

    // Split body into TABLES / DIMENSIONS / METRICS sections
    let body = body.trim();
    let mut remaining = body;

    while !remaining.is_empty() {
        let (keyword, after) = if remaining.to_lowercase().starts_with("tables") {
            ("tables", remaining[6..].trim_start())
        } else if remaining.to_lowercase().starts_with("dimensions") {
            ("dimensions", remaining[10..].trim_start())
        } else if remaining.to_lowercase().starts_with("metrics") {
            ("metrics", remaining[7..].trim_start())
        } else {
            return Err(format!("Expected TABLES, DIMENSIONS, or METRICS, got: {}", &remaining[..20.min(remaining.len())]));
        };

        // Find the parenthesized block
        let (block, rest) = extract_parens(after)?;

        match keyword {
            "tables" => {
                tables = parse_tables(&block)?;
            }
            "dimensions" => {
                dimensions = parse_dimensions(&block)?;
            }
            "metrics" => {
                metrics = parse_metrics(&block)?;
            }
            _ => unreachable!(),
        }

        remaining = rest.trim();
    }

    if tables.is_empty() {
        return Err("At least one table is required in TABLES clause".into());
    }

    Ok(SemanticView {
        name,
        tables,
        dimensions,
        metrics,
    })
}

/// Split `s` at the first occurrence of `keyword` (case-insensitive for AS part).
fn split_at_keyword<'a>(s: &'a str, keyword: &'a str) -> Option<(&'a str, &'a str)> {
    let lower = s.to_lowercase();
    let pos = lower.find(&keyword.to_lowercase())?;
    Some((&s[..pos], &s[pos + keyword.len()..]))
}

/// Extract content between outer parentheses.
fn extract_parens(s: &str) -> Result<(String, &str), String> {
    let s = s.trim();
    if !s.starts_with('(') {
        return Err(format!("Expected '(' but got: {}", &s[..20.min(s.len())]));
    }
    let mut depth = 0;
    let mut end = 0;
    for (i, ch) in s.char_indices() {
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 {
                end = i;
                break;
            }
        }
    }
    if depth != 0 {
        return Err("Unmatched parentheses".into());
    }
    let inner = &s[1..end];
    let rest = &s[end + 1..];
    Ok((inner.to_string(), rest))
}

/// Parse TABLES clause: (d AS demo PRIMARY KEY (region), ...)
fn parse_tables(input: &str) -> Result<Vec<ViewTable>, String> {
    let mut tables = Vec::new();
    for part in split_top_level(input, ',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let table = parse_single_table(part)?;
        tables.push(table);
    }
    Ok(tables)
}

fn parse_single_table(input: &str) -> Result<ViewTable, String> {
    // Handle PRIMARY KEY
    let (rest, pk) = if let Some(pos) = input.to_lowercase().find("primary key") {
        let before = &input[..pos].trim_end();
        let after_pk = &input[pos + 11..].trim();
        let (pk_block, _) = extract_parens(after_pk)?;
        let keys: Vec<String> = pk_block.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        (before.to_string(), keys)
    } else {
        (input.to_string(), vec![])
    };

    // Parse "alias AS table_name" or just "table_name"
    let rest = rest.trim();
    if let Some(result) = parse_alias_as(rest) {
        Ok(ViewTable {
            alias: result.0,
            table_name: result.1,
            primary_key: pk,
        })
    } else {
        Ok(ViewTable {
            alias: rest.to_string(),
            table_name: rest.to_string(),
            primary_key: pk,
        })
    }
}

/// Parse "alias.column AS alias.column" or "alias.column"
fn parse_dimensions(input: &str) -> Result<Vec<ViewDim>, String> {
    let mut dims = Vec::new();
    for part in split_top_level(input, ',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(result) = parse_alias_as(part) {
            // alias.col AS alias.col
            let (qualifier, col) = split_dot(&result.0)?;
            dims.push(ViewDim {
                qualifier,
                col_name: col,
                alias: result.1,
            });
        } else {
            let (qualifier, col) = split_dot(part)?;
            dims.push(ViewDim {
                qualifier: qualifier.clone(),
                col_name: col.clone(),
                alias: col,
            });
        }
    }
    Ok(dims)
}

/// Parse "left AS right" where AS is surrounded by spaces.
/// Used for both table aliases ("d AS demo") and dimension columns ("d.col AS d.col").
fn parse_alias_as(input: &str) -> Option<(String, String)> {
    let lower = input.to_lowercase();
    if let Some(pos) = lower.find(" as ") {
        let left = input[..pos].trim();
        let right = input[pos + 4..].trim();
        if !left.is_empty() && !right.is_empty() {
            return Some((left.to_string(), right.to_string()));
        }
    }
    None
}

/// Split "alias.column" into (alias, column)
fn split_dot(input: &str) -> Result<(String, String), String> {
    if let Some(pos) = input.find('.') {
        Ok((input[..pos].to_string(), input[pos + 1..].to_string()))
    } else {
        Err(format!("Expected 'alias.column' but got: {}", input))
    }
}

/// Parse METRICS clause: (d.revenue AS SUM(d.amount), ...)
fn parse_metrics(input: &str) -> Result<Vec<ViewMetric>, String> {
    let mut metrics = Vec::new();
    for part in split_top_level(input, ',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        // "alias.name AS expression"
        let lower = part.to_lowercase();
        if let Some(pos) = lower.find(" as ") {
            let left = part[..pos].trim();
            let right = part[pos + 4..].trim();
            let (qualifier, name) = split_dot(left)?;
            metrics.push(ViewMetric {
                qualifier,
                name,
                expression: right.to_string(),
            });
        } else {
            return Err(format!("Metric must have AS: {}", part));
        }
    }
    Ok(metrics)
}

/// Split by `,` but respect nested parentheses.
fn split_top_level(input: &str, delim: char) -> Vec<String> {
    let mut result = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, ch) in input.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c == delim && depth == 0 => {
                result.push(input[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    result.push(input[start..].to_string());
    result
}

// ─── semantic_view_expand ────────────────────────────────────────────────

/// Expand a semantic view query:
///   semantic_view_expand('view_name', 'dim1,dim2', 'metric1,metric2')
///   → "SELECT dim1, dim2, metric1, metric2 FROM ... GROUP BY dim1, dim2"
pub fn expand_view(view_name: &str, dims: &[String], mets: &[String]) -> Result<String, String> {
    let views = get_views().lock().map_err(|e| e.to_string())?;
    let view = views
        .get(view_name)
        .ok_or_else(|| format!("Semantic view '{}' not found", view_name))?;

    if view.tables.is_empty() {
        return Err("View has no tables".into());
    }

    // Build SELECT list
    let mut select_parts: Vec<String> = Vec::new();
    // Track which aliases are used for FROM/JOIN
    let mut from_tables: Vec<String> = Vec::new();
    let mut seen_aliases = std::collections::HashSet::new();

    // Resolve dimensions
    let table = &view.tables[0];
    let default_alias = &table.alias;

    for dim_name in dims {
        // Find the dimension definition
        if let Some(dim) = view.dimensions.iter().find(|d| d.alias == *dim_name || d.col_name == *dim_name) {
            let col_ref = format!("{}.{}", dim.qualifier, dim.col_name);
            if dim.alias != dim.col_name && dim.alias != dim_name.as_str() {
                select_parts.push(format!("{} AS {}", col_ref, dim.alias));
            } else {
                select_parts.push(col_ref);
            }
            seen_aliases.insert(dim.qualifier.clone());
        } else {
            // Unqualified dimension — use default alias
            select_parts.push(format!("{}.{}", default_alias, dim_name));
            seen_aliases.insert(default_alias.clone());
        }
    }

    // Resolve metrics
    for met_name in mets {
        if let Some(met) = view.metrics.iter().find(|m| m.name == *met_name) {
            select_parts.push(format!("{} AS {}", met.expression, met.name));
            seen_aliases.insert(met.qualifier.clone());
        } else {
            return Err(format!("Metric '{}' not found in view '{}'", met_name, view_name));
        }
    }

    // FROM clause — collect unique aliases
    for t in &view.tables {
        if seen_aliases.contains(&t.alias) || seen_aliases.is_empty() {
            from_tables.push(format!("{} AS {}", t.table_name, t.alias));
        }
    }

    // GROUP BY from dimension columns
    let group_parts: Vec<String> = dims.iter().map(|d| {
        if let Some(dim) = view.dimensions.iter().find(|dim| dim.alias == *d || dim.col_name == *d) {
            format!("{}.{}", dim.qualifier, dim.col_name)
        } else {
            format!("{}.{}", default_alias, d)
        }
    }).collect();

    let mut sql = format!(
        "SELECT {}\nFROM {}",
        select_parts.join(", "),
        from_tables.join(", ")
    );

    if !group_parts.is_empty() {
        sql.push_str(&format!("\nGROUP BY {}", group_parts.join(", ")));
    }

    Ok(sql)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_view() {
        let ddl = "CREATE SEMANTIC VIEW sales AS TABLES (d AS demo PRIMARY KEY (region)) DIMENSIONS (d.region AS d.region) METRICS (d.revenue AS SUM(d.amount))";
        let view = parse_ddl(ddl).unwrap();
        assert_eq!(view.name, "sales");
        assert_eq!(view.tables.len(), 1);
        assert_eq!(view.tables[0].alias, "d");
        assert_eq!(view.tables[0].table_name, "demo");
        assert_eq!(view.tables[0].primary_key, vec!["region"]);
        assert_eq!(view.dimensions.len(), 1);
        assert_eq!(view.dimensions[0].qualifier, "d");
        assert_eq!(view.dimensions[0].col_name, "region");
        assert_eq!(view.metrics.len(), 1);
        assert_eq!(view.metrics[0].name, "revenue");
        assert_eq!(view.metrics[0].expression, "SUM(d.amount)");
    }

    #[test]
    fn expand_simple_view() {
        let ddl = "CREATE SEMANTIC VIEW sales AS TABLES (d AS demo PRIMARY KEY (region)) DIMENSIONS (d.region AS d.region) METRICS (d.revenue AS SUM(d.amount))";
        let view = parse_ddl(ddl).unwrap();
        {
            let mut views = get_views().lock().unwrap();
            views.insert(view.name.clone(), view);
        }
        let sql = expand_view("sales", &["region".into()], &["revenue".into()]).unwrap();
        assert!(sql.contains("SUM(d.amount)"));
        assert!(sql.contains("GROUP BY d.region"));
        assert!(sql.contains("demo AS d"));
    }

    #[test]
    fn parse_case_insensitive() {
        let ddl = "create semantic view myview as tables (x as tbl) dimensions (x.col) metrics (x.sum as COUNT(*))";
        let view = parse_ddl(ddl).unwrap();
        assert_eq!(view.name, "myview");
        assert_eq!(view.tables[0].alias, "x");
        assert_eq!(view.dimensions[0].col_name, "col");
        assert_eq!(view.metrics[0].name, "sum");
    }

    #[test]
    fn parse_missing_name() {
        assert!(parse_ddl("CREATE SEMANTIC VIEW AS TABLES (x AS t)").is_err());
    }

    #[test]
    fn parse_no_tables() {
        assert!(parse_ddl("CREATE SEMANTIC VIEW x AS DIMENSIONS (a.b)").is_err());
    }
}
