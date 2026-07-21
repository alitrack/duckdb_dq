//! Graph-powered relationship discovery.
//!
//! Builds a directed graph from FK edges (model_a → model_b via join_condition),
//! then walks it to discover relationships and find shortest JOIN paths.
//!
//! Functions:
//!   semantic_graph_reset()                              → clear the graph
//!   semantic_graph_add_edge(from, to, condition)        → add an FK edge
//!   semantic_discover_relationships(model_name)          → table: all reachable models
//!   semantic_shortest_path(from_model, to_model)         → table: JOIN path nodes

use once_cell::sync::Lazy;
use petgraph::graph::DiGraph;
use petgraph::algo;
use std::collections::HashMap;
use std::sync::Mutex;

/// An edge in the semantic relationship graph (used for serialization).
#[allow(dead_code)]
pub struct SemEdge {
    pub from: String,
    pub to: String,
    pub condition: String,
}

/// Graph state: DiGraph + node/edge metadata.
pub struct SemGraph {
    graph: DiGraph<String, String>,           // node=model_name, edge=join_condition
    name_to_idx: HashMap<String, petgraph::graph::NodeIndex>,
    idx_to_name: HashMap<petgraph::graph::NodeIndex, String>,
}

impl SemGraph {
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            name_to_idx: HashMap::new(),
            idx_to_name: HashMap::new(),
        }
    }

    pub fn reset(&mut self) {
        self.graph = DiGraph::new();
        self.name_to_idx.clear();
        self.idx_to_name.clear();
    }

    fn ensure_node(&mut self, name: &str) -> petgraph::graph::NodeIndex {
        if let Some(idx) = self.name_to_idx.get(name) {
            return *idx;
        }
        let idx = self.graph.add_node(name.to_string());
        self.name_to_idx.insert(name.to_string(), idx);
        self.idx_to_name.insert(idx, name.to_string());
        idx
    }

    pub fn add_edge(&mut self, from: &str, to: &str, condition: &str) {
        let u = self.ensure_node(from);
        let v = self.ensure_node(to);
        self.graph.add_edge(u, v, condition.to_string());
    }

    /// Find all models reachable from `from_model` by BFS, with distances.
    pub fn discover(&self, from_model: &str) -> Vec<(String, i32, String)> {
        let start = match self.name_to_idx.get(from_model) {
            Some(idx) => *idx,
            None => return vec![],
        };

        let mut result = Vec::new();
        let mut bfs = petgraph::visit::Bfs::new(&self.graph, start);
        // Track depth manually: distance from start
        let mut depth: HashMap<petgraph::graph::NodeIndex, i32> = HashMap::new();
        depth.insert(start, 0);

        while let Some(node) = bfs.next(&self.graph) {
            if node == start {
                continue;
            }
            // Find the parent that led to this node (lowest depth neighbor)
            let mut best_parent = start;
            let mut best_depth = i32::MAX;
            for parent in self.graph.neighbors_directed(node, petgraph::Direction::Incoming) {
                if let Some(&d) = depth.get(&parent) {
                    if d < best_depth {
                        best_depth = d;
                        best_parent = parent;
                    }
                }
            }
            let dist = best_depth + 1;
            depth.insert(node, dist);

            let name = self.idx_to_name.get(&node).cloned().unwrap_or_default();
            let condition = self
                .graph
                .find_edge(best_parent, node)
                .map(|ei| self.graph[ei].clone())
                .unwrap_or_default();

            result.push((name, dist, condition));
        }

        result
    }

    /// Find shortest path from `from` to `to`, returning (model_name, join_condition) steps.
    pub fn shortest_path(&self, from: &str, to: &str) -> Vec<(String, String)> {
        let start = match self.name_to_idx.get(from) {
            Some(idx) => *idx,
            None => return vec![],
        };
        let end = match self.name_to_idx.get(to) {
            Some(idx) => *idx,
            None => return vec![],
        };

        let path = algo::astar(
            &self.graph,
            start,
            |n| n == end,
            |_| 1,
            |_| 0,
        );

        match path {
            Some((_, nodes)) => {
                let mut steps = Vec::new();
                for w in nodes.windows(2) {
                    let from_name = self.idx_to_name.get(&w[0]).cloned().unwrap_or_default();
                    let to_name = self.idx_to_name.get(&w[1]).cloned().unwrap_or_default();
                    let cond = self
                        .graph
                        .find_edge(w[0], w[1])
                        .map(|ei| self.graph[ei].clone())
                        .unwrap_or_default();
                    steps.push((format!("{} → {}", from_name, to_name), cond));
                }
                steps
            }
            None => vec![],
        }
    }
}

static SEM_GRAPH: Lazy<Mutex<SemGraph>> = Lazy::new(|| Mutex::new(SemGraph::new()));

pub fn get_graph() -> &'static Mutex<SemGraph> {
    &SEM_GRAPH
}
