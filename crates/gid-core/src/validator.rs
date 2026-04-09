//! Graph validation: detect cycles, orphan nodes, missing references, etc.

use std::collections::{HashMap, HashSet, VecDeque};
use crate::graph::Graph;

/// Validation result with all issues found.
#[derive(Debug, Default)]
pub struct ValidationResult {
    pub orphan_nodes: Vec<String>,
    pub missing_refs: Vec<MissingRef>,
    pub cycles: Vec<Vec<String>>,
    pub duplicate_nodes: Vec<String>,
    pub duplicate_edges: Vec<DuplicateEdge>,
    pub self_edges: Vec<SelfEdge>,
}

#[derive(Debug)]
pub struct MissingRef {
    pub edge_from: String,
    pub edge_to: String,
    pub missing_node: String,
}

#[derive(Debug)]
pub struct DuplicateEdge {
    pub from: String,
    pub to: String,
    pub relation: String,
}

#[derive(Debug)]
pub struct SelfEdge {
    pub node: String,
    pub relation: String,
}

impl ValidationResult {
    pub fn is_valid(&self) -> bool {
        self.orphan_nodes.is_empty()
            && self.missing_refs.is_empty()
            && self.cycles.is_empty()
            && self.duplicate_nodes.is_empty()
            && self.duplicate_edges.is_empty()
            && self.self_edges.is_empty()
    }

    pub fn issue_count(&self) -> usize {
        self.orphan_nodes.len()
            + self.missing_refs.len()
            + self.cycles.len()
            + self.duplicate_nodes.len()
            + self.duplicate_edges.len()
            + self.self_edges.len()
    }
}

impl std::fmt::Display for ValidationResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_valid() {
            return write!(f, "✓ Graph is valid");
        }

        let mut lines = Vec::new();

        if !self.orphan_nodes.is_empty() {
            lines.push(format!(
                "Orphan nodes (no edges): {}",
                self.orphan_nodes.join(", ")
            ));
        }

        for mr in &self.missing_refs {
            lines.push(format!(
                "Missing node '{}' referenced by edge {} → {}",
                mr.missing_node, mr.edge_from, mr.edge_to
            ));
        }

        for cycle in &self.cycles {
            lines.push(format!("Cycle detected: {}", cycle.join(" → ")));
        }

        if !self.duplicate_nodes.is_empty() {
            lines.push(format!(
                "Duplicate node IDs: {}",
                self.duplicate_nodes.join(", ")
            ));
        }

        for de in &self.duplicate_edges {
            lines.push(format!(
                "Duplicate edge: {} → {} ({})",
                de.from, de.to, de.relation
            ));
        }

        for se in &self.self_edges {
            lines.push(format!(
                "Self-referential edge: {} → {} ({})",
                se.node, se.node, se.relation
            ));
        }

        write!(f, "✗ {} issues found:\n  {}", self.issue_count(), lines.join("\n  "))
    }
}

/// Validator for graph integrity.
pub struct Validator<'a> {
    graph: &'a Graph,
}

impl<'a> Validator<'a> {
    pub fn new(graph: &'a Graph) -> Self {
        Self { graph }
    }

    /// Run all validations and return combined result.
    pub fn validate(&self) -> ValidationResult {
        let mut result = ValidationResult::default();

        result.duplicate_nodes = self.find_duplicate_nodes();
        result.missing_refs = self.find_missing_refs();
        result.orphan_nodes = self.find_orphan_nodes();
        result.cycles = self.find_cycles();
        result.duplicate_edges = self.find_duplicate_edges();
        result.self_edges = self.find_self_edges();

        result
    }

    /// Find nodes that have no edges (neither incoming nor outgoing).
    pub fn find_orphan_nodes(&self) -> Vec<String> {
        let connected: HashSet<&str> = self.graph.edges.iter()
            .flat_map(|e| [e.from.as_str(), e.to.as_str()])
            .collect();

        self.graph.nodes.iter()
            .filter(|n| !connected.contains(n.id.as_str()))
            .map(|n| n.id.clone())
            .collect()
    }

    /// Find edges that reference non-existent nodes.
    pub fn find_missing_refs(&self) -> Vec<MissingRef> {
        let node_ids: HashSet<&str> = self.graph.nodes.iter()
            .map(|n| n.id.as_str())
            .collect();

        let mut missing = Vec::new();

        for edge in &self.graph.edges {
            if !node_ids.contains(edge.from.as_str()) {
                missing.push(MissingRef {
                    edge_from: edge.from.clone(),
                    edge_to: edge.to.clone(),
                    missing_node: edge.from.clone(),
                });
            }
            if !node_ids.contains(edge.to.as_str()) {
                missing.push(MissingRef {
                    edge_from: edge.from.clone(),
                    edge_to: edge.to.clone(),
                    missing_node: edge.to.clone(),
                });
            }
        }

        missing
    }

    /// Find cycles in the graph using Tarjan's SCC algorithm.
    /// By default checks depends_on edges. Any SCC with size > 1 is a cycle.
    pub fn find_cycles(&self) -> Vec<Vec<String>> {
        self.find_cycles_for_relations(&["depends_on"])
    }

    /// Find cycles considering specific edge relations.
    pub fn find_cycles_for_relations(&self, relations: &[&str]) -> Vec<Vec<String>> {
        let relation_set: HashSet<&str> = relations.iter().copied().collect();

        // Build adjacency list for specified edge relations
        let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
        for node in &self.graph.nodes {
            adj.entry(&node.id).or_default();
        }
        for edge in &self.graph.edges {
            if relation_set.contains(edge.relation.as_str()) {
                adj.entry(&edge.from).or_default().push(&edge.to);
            }
        }

        // Tarjan's SCC
        let mut index_counter: usize = 0;
        let mut stack: Vec<&str> = Vec::new();
        let mut on_stack: HashSet<&str> = HashSet::new();
        let mut indices: HashMap<&str, usize> = HashMap::new();
        let mut lowlinks: HashMap<&str, usize> = HashMap::new();
        let mut sccs: Vec<Vec<String>> = Vec::new();

        fn strongconnect<'a>(
            node: &'a str,
            adj: &HashMap<&'a str, Vec<&'a str>>,
            index_counter: &mut usize,
            stack: &mut Vec<&'a str>,
            on_stack: &mut HashSet<&'a str>,
            indices: &mut HashMap<&'a str, usize>,
            lowlinks: &mut HashMap<&'a str, usize>,
            sccs: &mut Vec<Vec<String>>,
        ) {
            indices.insert(node, *index_counter);
            lowlinks.insert(node, *index_counter);
            *index_counter += 1;
            stack.push(node);
            on_stack.insert(node);

            if let Some(neighbors) = adj.get(node) {
                for &neighbor in neighbors {
                    if !indices.contains_key(neighbor) {
                        strongconnect(neighbor, adj, index_counter, stack, on_stack, indices, lowlinks, sccs);
                        let neighbor_low = lowlinks[neighbor];
                        let node_low = lowlinks.get_mut(node).unwrap();
                        if neighbor_low < *node_low {
                            *node_low = neighbor_low;
                        }
                    } else if on_stack.contains(neighbor) {
                        let neighbor_idx = indices[neighbor];
                        let node_low = lowlinks.get_mut(node).unwrap();
                        if neighbor_idx < *node_low {
                            *node_low = neighbor_idx;
                        }
                    }
                }
            }

            // If node is a root of an SCC
            if lowlinks[node] == indices[node] {
                let mut scc = Vec::new();
                loop {
                    let w = stack.pop().unwrap();
                    on_stack.remove(w);
                    scc.push(w.to_string());
                    if w == node {
                        break;
                    }
                }
                // Only report SCCs with size > 1 (actual cycles)
                if scc.len() > 1 {
                    scc.reverse(); // Put in traversal order
                    // Add closing node to match the existing format: [a, b, c, a]
                    if let Some(first) = scc.first().cloned() {
                        scc.push(first);
                    }
                    sccs.push(scc);
                }
            }
        }

        for node in &self.graph.nodes {
            if !indices.contains_key(node.id.as_str()) {
                strongconnect(
                    &node.id, &adj, &mut index_counter, &mut stack,
                    &mut on_stack, &mut indices, &mut lowlinks, &mut sccs,
                );
            }
        }

        sccs
    }

    /// Find duplicate node IDs.
    pub fn find_duplicate_nodes(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut duplicates = Vec::new();

        for node in &self.graph.nodes {
            if !seen.insert(&node.id) {
                duplicates.push(node.id.clone());
            }
        }

        duplicates
    }

    /// Find duplicate edges (same from, to, relation).
    pub fn find_duplicate_edges(&self) -> Vec<DuplicateEdge> {
        let mut seen = HashSet::new();
        let mut duplicates = Vec::new();

        for edge in &self.graph.edges {
            let key = (&edge.from, &edge.to, &edge.relation);
            if !seen.insert(key) {
                duplicates.push(DuplicateEdge {
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                    relation: edge.relation.clone(),
                });
            }
        }

        duplicates
    }

    /// Find self-referential edges (from == to).
    pub fn find_self_edges(&self) -> Vec<SelfEdge> {
        self.graph.edges.iter()
            .filter(|e| e.from == e.to)
            .map(|e| SelfEdge {
                node: e.from.clone(),
                relation: e.relation.clone(),
            })
            .collect()
    }

    /// Check if adding an edge would create a cycle.
    pub fn would_create_cycle(&self, from: &str, to: &str) -> bool {
        // Adding from -> to creates a cycle if there's already a path from to -> from
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(to);
        visited.insert(to);

        while let Some(current) = queue.pop_front() {
            if current == from {
                return true;
            }
            for edge in &self.graph.edges {
                if edge.from == current && edge.relation == "depends_on" {
                    if visited.insert(&edge.to) {
                        queue.push_back(&edge.to);
                    }
                }
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Edge, Node};

    #[test]
    fn test_orphan_detection() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("a", "A"));
        graph.add_node(Node::new("b", "B"));
        graph.add_node(Node::new("c", "C"));
        graph.add_edge(Edge::depends_on("a", "b"));
        
        let validator = Validator::new(&graph);
        let orphans = validator.find_orphan_nodes();
        assert_eq!(orphans, vec!["c"]);
    }

    #[test]
    fn test_missing_refs() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("a", "A"));
        graph.edges.push(Edge::depends_on("a", "missing"));

        let validator = Validator::new(&graph);
        let missing = validator.find_missing_refs();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].missing_node, "missing");
    }

    #[test]
    fn test_self_edge_detection() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("a", "A"));
        graph.add_node(Node::new("b", "B"));
        graph.edges.push(Edge::depends_on("a", "a")); // self-edge
        graph.add_edge(Edge::depends_on("a", "b"));    // normal edge

        let validator = Validator::new(&graph);
        let self_edges = validator.find_self_edges();
        assert_eq!(self_edges.len(), 1);
        assert_eq!(self_edges[0].node, "a");
        assert_eq!(self_edges[0].relation, "depends_on");
    }

    #[test]
    fn test_self_edge_makes_graph_invalid() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("a", "A"));
        graph.add_node(Node::new("b", "B"));
        graph.add_edge(Edge::depends_on("a", "b"));

        let validator = Validator::new(&graph);
        assert!(validator.find_self_edges().is_empty());

        // Add self-edge
        graph.edges.push(Edge::depends_on("b", "b"));
        let validator = Validator::new(&graph);
        let result = validator.validate();
        assert!(!result.self_edges.is_empty());
        // self_edges contribute to issue_count and is_valid
        assert!(result.issue_count() > 0);
    }

    #[test]
    fn test_no_self_edges_in_clean_graph() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("a", "A"));
        graph.add_node(Node::new("b", "B"));
        graph.add_node(Node::new("c", "C"));
        graph.add_edge(Edge::depends_on("a", "b"));
        graph.add_edge(Edge::depends_on("b", "c"));

        let validator = Validator::new(&graph);
        assert!(validator.find_self_edges().is_empty());
    }

    #[test]
    fn test_cycle_detection() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("a", "A"));
        graph.add_node(Node::new("b", "B"));
        graph.add_node(Node::new("c", "C"));
        graph.add_edge(Edge::depends_on("a", "b"));
        graph.add_edge(Edge::depends_on("b", "c"));
        graph.add_edge(Edge::depends_on("c", "a")); // cycle!

        let validator = Validator::new(&graph);
        let cycles = validator.find_cycles();
        assert!(!cycles.is_empty());
    }

    #[test]
    fn test_multiple_independent_cycles() {
        let mut graph = Graph::new();
        // Cycle 1: a -> b -> a
        graph.add_node(Node::new("a", "A"));
        graph.add_node(Node::new("b", "B"));
        graph.add_edge(Edge::depends_on("a", "b"));
        graph.add_edge(Edge::depends_on("b", "a"));
        // Cycle 2: c -> d -> c
        graph.add_node(Node::new("c", "C"));
        graph.add_node(Node::new("d", "D"));
        graph.add_edge(Edge::depends_on("c", "d"));
        graph.add_edge(Edge::depends_on("d", "c"));
        // Unconnected node
        graph.add_node(Node::new("e", "E"));

        let validator = Validator::new(&graph);
        let cycles = validator.find_cycles();
        assert_eq!(cycles.len(), 2, "Should find both independent cycles");
    }

    #[test]
    fn test_no_false_positives_on_dag() {
        let mut graph = Graph::new();
        // Diamond DAG: a -> b, a -> c, b -> d, c -> d
        graph.add_node(Node::new("a", "A"));
        graph.add_node(Node::new("b", "B"));
        graph.add_node(Node::new("c", "C"));
        graph.add_node(Node::new("d", "D"));
        graph.add_edge(Edge::depends_on("a", "b"));
        graph.add_edge(Edge::depends_on("a", "c"));
        graph.add_edge(Edge::depends_on("b", "d"));
        graph.add_edge(Edge::depends_on("c", "d"));

        let validator = Validator::new(&graph);
        let cycles = validator.find_cycles();
        assert!(cycles.is_empty(), "Diamond DAG should have no cycles");
    }

    #[test]
    fn test_cycle_detection_multi_relation() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("a", "A"));
        graph.add_node(Node::new("b", "B"));
        // No depends_on cycle, but blocks creates one
        graph.add_edge(Edge::new("a", "b", "blocks"));
        graph.add_edge(Edge::new("b", "a", "blocks"));

        let validator = Validator::new(&graph);
        // Default: only depends_on — no cycles
        let cycles = validator.find_cycles();
        assert!(cycles.is_empty());
        // With blocks relation — finds cycle
        let cycles = validator.find_cycles_for_relations(&["blocks"]);
        assert_eq!(cycles.len(), 1);
    }
}
