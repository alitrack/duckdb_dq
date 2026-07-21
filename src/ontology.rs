//! Ontology layer: class hierarchy, property definitions, and class-to-model mapping.
//!
//! Supports:
//!   - Class taxonomy (is-a DAG) with subclass reasoning
//!   - Property definitions with domain/range
//!   - Class-to-model mapping with optional filters
//!   - EL-style inheritance (subclass inherits parent mappings + properties)
//!   - OWL Functional Syntax export
#![allow(dead_code)]
//!
//! Functions (registered in lib.rs):
//!   semantic_class_define(name, parent)       → add a class
//!   semantic_class_map(class, model, opts?)   → map class to model
//!   semantic_property_define(name, dom, rng, map) → define property
//!   semantic_class_query(class)               → table: expanded SQL
//!   semantic_class_inheritance(class)          → table: is-a chain
//!   semantic_ontology_export(format)           → scalar: OFN export

use once_cell::sync::Lazy;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// A class in the ontology taxonomy.
#[derive(Debug, Clone)]
pub struct OntoClass {
    pub name: String,
    pub description: String,
    /// is-a parents
    pub parents: Vec<String>,
}

/// A property definition with domain and range.
#[derive(Debug, Clone)]
pub struct OntoProperty {
    pub name: String,
    /// Domain class name
    pub domain: String,
    /// Range: either a class name or a data type (e.g. "xsd:string")
    pub range: String,
    /// SQL expression mapping the property
    pub mapping: Option<String>,
}

/// Maps a class to a physical model with optional filter.
#[derive(Debug, Clone)]
pub struct ClassMapping {
    pub class_name: String,
    pub model_name: String,
    /// Optional SQL WHERE clause filter
    pub filter: Option<String>,
}

/// The full ontology state.
pub struct Ontology {
    pub classes: HashMap<String, OntoClass>,
    pub properties: Vec<OntoProperty>,
    pub mappings: Vec<ClassMapping>,
    /// Source schema/catalog
    pub catalog: String,
    pub schema: String,
}

impl Ontology {
    pub fn new() -> Self {
        Self {
            classes: HashMap::new(),
            properties: Vec::new(),
            mappings: Vec::new(),
            catalog: String::new(),
            schema: String::new(),
        }
    }

    pub fn reset(&mut self) {
        self.classes.clear();
        self.properties.clear();
        self.mappings.clear();
    }

    /// Add a class to the taxonomy.
    pub fn define_class(&mut self, name: &str, parent: Option<&str>, description: &str) {
        let parents = parent.map(|p| vec![p.to_string()]).unwrap_or_default();
        self.classes.insert(
            name.to_string(),
            OntoClass {
                name: name.to_string(),
                description: description.to_string(),
                parents,
            },
        );
    }

    /// Map a class to a physical model with optional filter.
    pub fn map_class(&mut self, class_name: &str, model_name: &str, filter: Option<&str>) {
        self.mappings.push(ClassMapping {
            class_name: class_name.to_string(),
            model_name: model_name.to_string(),
            filter: filter.map(|s| s.to_string()),
        });
    }

    /// Define a property.
    pub fn define_property(
        &mut self,
        name: &str,
        domain: &str,
        range: &str,
        mapping: Option<&str>,
    ) {
        self.properties.push(OntoProperty {
            name: name.to_string(),
            domain: domain.to_string(),
            range: range.to_string(),
            mapping: mapping.map(|s| s.to_string()),
        });
    }

    /// Get all ancestors of a class via is-a DAG (BFS).
    pub fn ancestors(&self, class_name: &str) -> Vec<String> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let mut queue: Vec<String> = vec![class_name.to_string()];
        visited.insert(class_name.to_string());

        while let Some(current) = queue.pop() {
            result.push(current.clone());
            if let Some(cls) = self.classes.get(&current) {
                for parent in &cls.parents {
                    if !visited.contains(parent) {
                        visited.insert(parent.clone());
                        queue.push(parent.clone());
                    }
                }
            }
        }
        // Remove self
        result.remove(0);
        result
    }

    /// Get all descendants of a class (subclasses that have this class as ancestor).
    pub fn descendants(&self, class_name: &str) -> Vec<String> {
        self.classes
            .keys()
            .filter(|c| {
                *c != class_name && self.ancestors(c).contains(&class_name.to_string())
            })
            .cloned()
            .collect()
    }

    /// Collect inherited mappings: find the best mapping for a class by walking
    /// up the is-a chain.
    pub fn inherited_mapping(&self, class_name: &str) -> Option<ClassMapping> {
        // Direct mapping first
        if let Some(m) = self.mappings.iter().find(|m| m.class_name == class_name) {
            return Some(m.clone());
        }
        // Walk ancestors
        for ancestor in self.ancestors(class_name) {
            if let Some(m) = self.mappings.iter().find(|m| m.class_name == ancestor) {
                return Some(m.clone());
            }
        }
        None
    }

    /// Collect all properties applicable to a class (own + inherited from ancestors).
    pub fn inherited_properties(&self, class_name: &str) -> Vec<OntoProperty> {
        let all_ancestors: HashSet<String> = {
            let mut s = HashSet::new();
            s.insert(class_name.to_string());
            s.extend(self.ancestors(class_name));
            s
        };
        self.properties
            .iter()
            .filter(|p| all_ancestors.contains(&p.domain))
            .cloned()
            .collect()
    }

    /// Build expanded SQL for a class query.
    /// Resolves the class → model mapping, applies inherited filter, and includes
    /// properties as SELECT columns.
    pub fn class_query_sql(&self, class_name: &str) -> Result<String, String> {
        let mapping = self
            .inherited_mapping(class_name)
            .ok_or_else(|| format!("No mapping for class '{}'", class_name))?;

        let mut sql = format!("SELECT * FROM \"{}\"", mapping.model_name);
        if let Some(ref filter) = mapping.filter {
            sql.push_str(&format!(" WHERE {}", filter));
        }

        Ok(sql)
    }

    /// Export ontology as OWL Functional Syntax.
    pub fn export_ofn(&self) -> String {
        let mut buf = String::new();
        buf.push_str("Prefix(:=<http://semantic.org/ontology#>)\n");
        buf.push_str("Prefix(owl:=<http://www.w3.org/2002/07/owl#>)\n");
        buf.push_str("Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n");
        buf.push_str("Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\n\n");
        buf.push_str("Ontology(<http://semantic.org/ontology>\n\n");

        // Class declarations
        for (name, _cls) in &self.classes {
            buf.push_str(&format!(
                "  Declaration(Class(:{}))\n",
                name
            ));
        }

        // SubClassOf axioms
        for (_name, cls) in &self.classes {
            for parent in &cls.parents {
                buf.push_str(&format!(
                    "  SubClassOf(:{} :{})\n",
                    cls.name, parent
                ));
            }
        }

        // Object properties (class → class)
        for prop in &self.properties {
            if self.classes.contains_key(&prop.range) {
                buf.push_str(&format!("  Declaration(ObjectProperty(:{}))\n", prop.name));
                buf.push_str(&format!("  ObjectPropertyDomain(:{} :{})\n", prop.name, prop.domain));
                buf.push_str(&format!("  ObjectPropertyRange(:{} :{})\n", prop.name, prop.range));
            }
        }

        buf.push_str(")\n");
        buf
    }
}

// ── Global state ────────────────────────────────────────────────────────

static ONTOLOGY: Lazy<Mutex<Ontology>> = Lazy::new(|| Mutex::new(Ontology::new()));

pub fn get_ontology() -> &'static Mutex<Ontology> {
    &ONTOLOGY
}
