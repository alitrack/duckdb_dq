//! Persistence layer — save/restore the full state to JSON.
//!
//! Serializes: MDL context, ontology classes/mappings/properties,
//! graph edges, process context patterns.
//!
//! Functions:
//!   semantic_save(path)    → save all state to a JSON snapshot
//!   semantic_restore(path) → restore from a JSON snapshot

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Serializable snapshot of the entire semantic state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: String,
    pub catalog: String,
    pub schema: String,
    pub models: Vec<ModelSnapshot>,
    pub relationships: Vec<RelSnapshot>,
    pub graph_edges: Vec<EdgeSnapshot>,
    pub ontology: Option<OntologySnapshot>,
    pub patterns: Vec<PatternSnapshot>,
    pub bm25: Option<crate::bm25::Bm25Snapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSnapshot {
    pub name: String,
    pub table_reference: Option<TableRefSnapshot>,
    pub ref_sql: Option<String>,
    pub columns: Vec<serde_json::Value>,
    pub primary_key: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableRefSnapshot {
    pub catalog: Option<String>,
    pub schema: Option<String>,
    pub table: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelSnapshot {
    pub name: String,
    pub models: Vec<String>,
    pub join_type: String,
    pub condition: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeSnapshot {
    pub from: String,
    pub to: String,
    pub condition: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OntologySnapshot {
    pub classes: HashMap<String, ClassSnapshot>,
    pub mappings: Vec<MappingSnapshot>,
    pub properties: Vec<PropSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassSnapshot {
    pub name: String,
    pub parents: Vec<String>,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MappingSnapshot {
    pub class_name: String,
    pub model_name: String,
    pub filter: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropSnapshot {
    pub name: String,
    pub domain: String,
    pub range: String,
    pub mapping: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternSnapshot {
    pub name: String,
    pub description: String,
    pub domain: String,
    pub steps: Vec<String>,
    pub frequency: i32,
    pub source: String,
}

impl Snapshot {
    pub fn new() -> Self {
        Self {
            version: "0.1.0".into(),
            catalog: String::new(),
            schema: String::new(),
            models: vec![],
            relationships: vec![],
            graph_edges: vec![],
            ontology: None,
            patterns: vec![],
            bm25: None,
        }
    }
}

/// Capture the current state from all global stores into a snapshot.
pub fn capture() -> Result<Snapshot, String> {
    
    use crate::graph;
    use crate::ontology;
    use crate::process;

    let mut snap = Snapshot::new();

    // MDL
    if let Ok(guard) = crate::get_ctx().lock() {
        if let Some(ref ctx) = *guard {
            snap.catalog = ctx.catalog.clone();
            snap.schema = ctx.schema.clone();
            snap.models = ctx.models.iter().map(|m| ModelSnapshot {
                name: m.name.clone(),
                table_reference: m.table_reference.as_ref().map(|tr| TableRefSnapshot {
                    catalog: tr.catalog.clone(),
                    schema: tr.schema.clone(),
                    table: tr.table.clone(),
                }),
                ref_sql: m.ref_sql.clone(),
                columns: m.columns.iter().map(|c| {
                    serde_json::json!({
                        "name": c.name,
                        "type": c.col_type,
                        "isCalculated": c.is_calculated,
                        "expression": c.expression,
                        "notNull": c.not_null,
                        "isPrimaryKey": c.is_primary_key,
                        "isHidden": c.is_hidden,
                    })
                }).collect(),
                primary_key: m.primary_key.clone(),
                description: m.description.clone(),
            }).collect();
            snap.relationships = ctx.relationships.iter().map(|r| RelSnapshot {
                name: r.name.clone(), models: r.models.clone(),
                join_type: r.join_type.clone(), condition: r.condition.clone(),
            }).collect();
        }
    }

    // Graph edges
    if let Ok(g) = graph::get_graph().lock() {
        snap.graph_edges = g.edges().into_iter().map(|(from, to, cond)| {
            EdgeSnapshot { from, to, condition: cond }
        }).collect();
    }

    // Ontology
    if let Ok(o) = ontology::get_ontology().lock() {
        let classes: HashMap<String, ClassSnapshot> = o.classes.iter().map(|(n, c)| {
            (n.clone(), ClassSnapshot {
                name: c.name.clone(), parents: c.parents.clone(),
                description: c.description.clone(),
            })
        }).collect();
        let mappings: Vec<MappingSnapshot> = o.mappings.iter().map(|m| MappingSnapshot {
            class_name: m.class_name.clone(), model_name: m.model_name.clone(),
            filter: m.filter.clone(),
        }).collect();
        let properties: Vec<PropSnapshot> = o.properties.iter().map(|p| PropSnapshot {
            name: p.name.clone(), domain: p.domain.clone(),
            range: p.range.clone(), mapping: p.mapping.clone(),
        }).collect();
        if !classes.is_empty() || !mappings.is_empty() {
            snap.ontology = Some(OntologySnapshot { classes, mappings, properties });
        }
    }

    // Patterns
    if let Ok(store) = process::get_store().lock() {
        snap.patterns = store.patterns.iter().map(|p| PatternSnapshot {
            name: p.name.clone(), description: p.description.clone(),
            domain: p.domain.clone(),
            steps: p.node_ids(),
            frequency: p.frequency,
            source: p.source.clone(),
        }).collect();
    }

    // BM25
    if let Ok(bm) = crate::bm25::get_bm25().lock() {
        if bm.len() > 0 {
            snap.bm25 = Some(bm.export());
        }
    }

    Ok(snap)
}

/// Restore state from a snapshot into all global stores.
pub fn restore(snap: &Snapshot) -> Result<String, String> {
    use crate::graph;
    use crate::ontology;
    use crate::process;
    use crate::mdl::SemanticContext;
    let mut summary = Vec::new();

    // MDL
    let models: Vec<crate::mdl::Model> = snap.models.iter().map(|m| crate::mdl::Model {
        name: m.name.clone(),
        table_reference: m.table_reference.as_ref().map(|tr| crate::mdl::TableReference {
            catalog: tr.catalog.clone(), schema: tr.schema.clone(), table: tr.table.clone(),
        }),
        ref_sql: m.ref_sql.clone(),
        columns: m.columns.iter().filter_map(|v| {
            let obj = v.as_object()?;
            Some(crate::mdl::Column {
                name: obj.get("name")?.as_str()?.to_string(),
                col_type: obj.get("type").and_then(|v| v.as_str()).unwrap_or("VARCHAR").to_string(),
                is_calculated: obj.get("isCalculated").and_then(|v| v.as_bool()).unwrap_or(false),
                expression: obj.get("expression").and_then(|v| v.as_str()).map(|s| s.to_string()),
                not_null: obj.get("notNull").and_then(|v| v.as_bool()).unwrap_or(false),
                is_primary_key: obj.get("isPrimaryKey").and_then(|v| v.as_bool()).unwrap_or(false),
                description: obj.get("description").and_then(|v| v.as_str()).map(|s| s.to_string()),
                is_hidden: obj.get("isHidden").and_then(|v| v.as_bool()).unwrap_or(false),
            })
        }).collect(),
        primary_key: m.primary_key.clone(),
        description: m.description.clone(),
    }).collect();

    let ctx = SemanticContext {
        catalog: snap.catalog.clone(),
        schema: snap.schema.clone(),
        models,
        relationships: snap.relationships.iter().map(|r| crate::mdl::Relationship {
            name: r.name.clone(), models: r.models.clone(),
            join_type: r.join_type.clone(), condition: r.condition.clone(),
        }).collect(),
        views: vec![],
    };
    let model_count = ctx.models.len();

    if let Ok(mut guard) = crate::get_ctx().lock() {
        *guard = Some(ctx);
    }
    summary.push(format!("{} models", model_count));

    // Graph edges
    if let Ok(mut g) = graph::get_graph().lock() {
        for e in &snap.graph_edges {
            g.add_edge(&e.from, &e.to, &e.condition);
        }
    }
    if !snap.graph_edges.is_empty() {
        summary.push(format!("{} edges", snap.graph_edges.len()));
    }

    // Ontology
    if let Some(ref onto) = snap.ontology {
        if let Ok(mut o) = ontology::get_ontology().lock() {
            for (_, c) in &onto.classes {
                let parent = c.parents.first().map(|s| s.as_str());
                o.define_class(&c.name, parent, &c.description);
            }
            for m in &onto.mappings {
                o.map_class(&m.class_name, &m.model_name, m.filter.as_deref());
            }
            for p in &onto.properties {
                o.define_property(&p.name, &p.domain, &p.range, p.mapping.as_deref());
            }
        }
        summary.push(format!("{} classes", onto.classes.len()));
    }

    // Patterns
    if let Ok(mut store) = process::get_store().lock() {
        for p in &snap.patterns {
            let steps: Vec<process::PatternStep> = p.steps.iter().enumerate().map(|(i, n)| {
                process::PatternStep { model_name: n.clone(), order: i as i32, notes: String::new() }
            }).collect();
            store.add_pattern(process::WorkflowPattern {
                name: p.name.clone(), description: p.description.clone(),
                domain: p.domain.clone(), steps,
                frequency: p.frequency, source: p.source.clone(),
            });
        }
    }
    if !snap.patterns.is_empty() {
        summary.push(format!("{} patterns", snap.patterns.len()));
    }

    // BM25
    if let Some(ref bm_snap) = snap.bm25 {
        if let Ok(mut bm) = crate::bm25::get_bm25().lock() {
            bm.import(bm_snap);
        }
        summary.push(format!("{} bm25 docs", bm_snap.docs.len()));
    }

    Ok(format!("Restored: {}", summary.join(", ")))
}
