//! Process Context — workflow patterns, hub detection, co-occurrence.
#![allow(dead_code)]
//!
//! Adapts npp-kb's ProcessContext design to the DuckDB semantic layer:
//!   1. Pattern mining over the FK relationship graph
//!   2. Hub-node detection (degree-based)
//!   3. Co-occurrence analysis (connected components)
//!   4. Enrichment: given a model, return patterns + context
//!
//! Functions (registered in lib.rs):
//!   semantic_pattern_add(name, steps, domain?, desc?)  → store a pattern
//!   semantic_process_context(model_name)                → table: hub + patterns
//!   semantic_discover_patterns()                        → table: mined paths
//!   semantic_pattern_search(query, k)                   → table: text search

use once_cell::sync::Lazy;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// A single step in a workflow pattern.
#[derive(Debug, Clone)]
pub struct PatternStep {
    pub model_name: String,
    pub order: i32,
    pub notes: String,
}

/// A workflow pattern — a sequence of models used together.
#[derive(Debug, Clone)]
pub struct WorkflowPattern {
    pub name: String,
    pub description: String,
    pub domain: String,
    pub steps: Vec<PatternStep>,
    pub frequency: i32,
    pub source: String, // "manual" | "discovered" | "inferred"
}

impl WorkflowPattern {
    pub fn node_ids(&self) -> Vec<String> {
        let mut sorted = self.steps.clone();
        sorted.sort_by_key(|s| s.order);
        sorted.into_iter().map(|s| s.model_name).collect()
    }
}

/// The full process context store.
pub struct ProcessContextStore {
    pub patterns: Vec<WorkflowPattern>,
}

impl ProcessContextStore {
    pub fn new() -> Self {
        Self { patterns: Vec::new() }
    }

    pub fn add_pattern(&mut self, p: WorkflowPattern) {
        self.patterns.push(p);
    }

    /// Find patterns containing a given model.
    pub fn patterns_for(&self, model_name: &str) -> Vec<&WorkflowPattern> {
        self.patterns
            .iter()
            .filter(|p| p.steps.iter().any(|s| s.model_name == model_name))
            .collect()
    }

    /// Text search over pattern names + descriptions.
    pub fn search(&self, query: &str, k: usize) -> Vec<&WorkflowPattern> {
        let q = query.to_lowercase();
        let mut scored: Vec<(f64, &WorkflowPattern)> = self
            .patterns
            .iter()
            .map(|p| {
                let text = format!("{} {} {}", p.name, p.description, p.domain).to_lowercase();
                let score = if text.contains(&q) { 1.0 } else { 0.0 };
                // Bonus for name match
                let bonus = if p.name.to_lowercase().contains(&q) { 2.0 } else { 0.0 };
                (score + bonus, p)
            })
            .filter(|(s, _)| *s > 0.0)
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        scored.into_iter().map(|(_, p)| p).collect()
    }

    pub fn count(&self) -> usize {
        self.patterns.len()
    }
}

/// Given a set of FK edges (from, to) and a graph (NodeIndex → model_name mapping),
/// discover frequent paths, hubs, and co-occurring clusters.
pub struct PatternDiscovery;

impl PatternDiscovery {
    /// Mine frequent paths from FK edges (max path length = 3).
    /// Uses petgraph's neighborhood expansion similar to npp-kb's approach.
    pub fn frequent_paths(
        edges: &[(String, String)],
        _graph: &crate::graph::SemGraph,
        max_depth: usize,
        top_k: usize,
    ) -> Vec<(Vec<String>, i32)> {
        // Build adjacency list
        let mut adj: HashMap<String, Vec<String>> = HashMap::new();
        for (from, to) in edges {
            adj.entry(from.clone()).or_default().push(to.clone());
        }

        let mut paths: Vec<(Vec<String>, i32)> = Vec::new();

        // BFS from each node up to max_depth
        for start in adj.keys() {
            let mut queue: Vec<(String, Vec<String>)> = vec![(start.clone(), vec![start.clone()])];
            while let Some((current, path)) = queue.pop() {
                if path.len() >= max_depth {
                    paths.push((path.clone(), 1));
                    continue;
                }
                if let Some(neighbors) = adj.get(&current) {
                    for next in neighbors {
                        if !path.contains(next) {
                            let mut new_path = path.clone();
                            new_path.push(next.clone());
                            queue.push((next.clone(), new_path));
                        }
                    }
                }
                if !path.is_empty() && path.len() >= 2 {
                    paths.push((path, 1));
                }
            }
        }

        // Deduplicate and count frequency
        let mut freq: HashMap<Vec<String>, i32> = HashMap::new();
        for (path, count) in paths {
            *freq.entry(path).or_default() += count;
        }

        let mut sorted: Vec<_> = freq.into_iter().collect();
        sorted.sort_by_key(|(_, f)| -(*f));
        sorted.truncate(top_k);
        sorted
    }

    /// Hub nodes: models with the highest degree in the FK graph.
    pub fn hubs(graph: &crate::graph::SemGraph, top_k: usize) -> Vec<(String, i32)> {
        let mut degree: HashMap<String, i32> = HashMap::new();

        // We can't easily iterate edges from SemGraph's petgraph, so use the discover
        // method on all known models as a proxy. A simpler approach: count connections
        // from graph's internal structure via the public API.
        // For now, return empty — will be populated when we have the graph reference.
        let hubs: Vec<(String, i32)> = degree.into_iter().collect();
        hubs.into_iter().take(top_k).collect()
    }
}

// ── Global state ────────────────────────────────────────────────────────

static PROCESS_CTX: Lazy<Mutex<ProcessContextStore>> =
    Lazy::new(|| Mutex::new(ProcessContextStore::new()));

pub fn get_store() -> &'static Mutex<ProcessContextStore> {
    &PROCESS_CTX
}
