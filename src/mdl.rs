// MDL (Modeling Definition Language) types and parser.
// Defines the semantic contract: models, columns, relationships, views.
// This is a clean-room implementation — input format inspired by
// the open-source semantic layer concept, not any specific tool.

use serde::{Deserialize, Serialize};

/// Full semantic context — the loaded state of a semantic project.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SemanticContext {
    pub catalog: String,
    pub schema: String,
    pub models: Vec<Model>,
    pub relationships: Vec<Relationship>,
    pub views: Vec<View>,
}

/// A logical model backed by a physical table or SQL definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub name: String,
    pub table_reference: Option<TableReference>,
    pub ref_sql: Option<String>,
    pub columns: Vec<Column>,
    pub primary_key: Option<String>,
    pub description: Option<String>,
}

/// Maps a model to a physical table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableReference {
    pub catalog: Option<String>,
    pub schema: Option<String>,
    pub table: String,
}

/// A column exposed by a model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub name: String,
    #[serde(rename = "type")]
    pub col_type: String,
    #[serde(default)]
    pub is_calculated: bool,
    pub expression: Option<String>,
    #[serde(default)]
    pub not_null: bool,
    #[serde(default)]
    pub is_primary_key: bool,
    pub description: Option<String>,
    /// Hidden columns are invisible to consumers.
    #[serde(default)]
    pub is_hidden: bool,
}

/// A relationship between two models.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Relationship {
    pub name: String,
    pub models: Vec<String>,
    pub join_type: String,
    pub condition: String,
}

/// A named view (pre-defined SQL).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct View {
    pub name: String,
    pub statement: String,
}

/// Load MDL from a JSON string or file path.
/// Accepts both the compiled manifest format and inline JSON.
pub fn load_mdl_json(input: &str) -> Result<SemanticContext, String> {
    // Try as file path first
    let content = if input.ends_with(".json") || input.ends_with(".mdl.json") {
        std::fs::read_to_string(input).map_err(|e| format!("Cannot read {}: {}", input, e))?
    } else {
        input.to_string()
    };

    // Safety: limit JSON size to 100MB
    const MAX_JSON_SIZE: usize = 100 * 1024 * 1024;
    if content.len() > MAX_JSON_SIZE {
        return Err(format!("MDL JSON too large: {} bytes (max {})", content.len(), MAX_JSON_SIZE));
    }
    serde_json::from_str::<SemanticContext>(&content)
        .map_err(|e| format!("Invalid MDL JSON: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_mdl() {
        let json = r#"{
            "catalog": "test",
            "schema": "main",
            "models": [{
                "name": "customers",
                "tableReference": {
                    "catalog": "test",
                    "schema": "main",
                    "table": "customers"
                },
                "columns": [
                    {"name": "id", "type": "INTEGER", "isPrimaryKey": true},
                    {"name": "name", "type": "VARCHAR"}
                ],
                "primaryKey": "id"
            }],
            "relationships": [],
            "views": []
        }"#;

        let ctx = load_mdl_json(json).unwrap();
        assert_eq!(ctx.models.len(), 1);
        assert_eq!(ctx.models[0].name, "customers");
        assert_eq!(ctx.models[0].columns.len(), 2);
    }

    #[test]
    fn parse_calculated_column() {
        let json = r#"{
            "catalog": "test",
            "schema": "main",
            "models": [{
                "name": "orders",
                "tableReference": {"table": "orders"},
                "columns": [
                    {"name": "id", "type": "INTEGER"},
                    {"name": "total_with_tax", "type": "DECIMAL",
                     "isCalculated": true,
                     "expression": "total * 1.1"}
                ]
            }],
            "relationships": [],
            "views": []
        }"#;

        let ctx = load_mdl_json(json).unwrap();
        let col = &ctx.models[0].columns[1];
        assert!(col.is_calculated);
        assert_eq!(col.expression.as_deref(), Some("total * 1.1"));
    }
}
