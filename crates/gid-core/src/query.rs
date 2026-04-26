use std::collections::{HashMap, HashSet, VecDeque};
use crate::graph::{Edge, Graph, Node};

/// Default confidence threshold for query results.
///
/// Edges with `confidence` below this threshold are hidden by default.
/// Tree-sitter name-match fallback edges have `confidence=0.6` and produce
/// massive false-positive pollution for common method names (`.contains()`,
/// `.clone()`, `.to_string()` etc.) — they must be filtered out by default
/// so impact/caller queries return correct-by-default results.
///
/// Edges with `confidence == None` are treated as fully trusted (1.0):
/// hand-authored design edges, `depends_on` task edges, and high-confidence
/// LSP-confirmed edges all fall into this category.
///
/// See ISS-035 for the full rationale.
pub const DEFAULT_MIN_CONFIDENCE: f64 = 0.8;

/// Result of a graph traversal query: visible nodes plus a count of edges
/// hidden because they fell below the confidence threshold.
///
/// The `hidden_low_confidence` count exists so callers can surface a summary
/// like *"N hidden low-confidence edges (use `--min-confidence 0.0` to include)"*
/// — preserving transparency without polluting default output.
#[derive(Debug, Clone)]
pub struct QueryResult<'a> {
    pub nodes: Vec<&'a Node>,
    /// Number of edges that were eligible (correct relation, correct direction)
    /// but skipped because `confidence < min_confidence`.
    pub hidden_low_confidence: usize,
}

/// Returns true if `edge` passes the confidence threshold.
/// `None` confidence is treated as fully trusted (>= any threshold).
fn edge_passes_confidence(edge: &Edge, min_confidence: Option<f64>) -> bool {
    match (min_confidence, edge.confidence) {
        (None, _) => true,                      // no filter requested
        (Some(_), None) => true,                 // None confidence = fully trusted
        (Some(thresh), Some(c)) => c >= thresh,
    }
}

/// Query engine for graph traversal and analysis.
pub struct QueryEngine<'a> {
    graph: &'a Graph,
}

impl<'a> QueryEngine<'a> {
    pub fn new(graph: &'a Graph) -> Self {
        Self { graph }
    }

    /// Impact analysis: what nodes are affected if `node_id` changes?
    /// Follows reverse dependency edges (who depends on this node?).
    /// Traverses all edge relations by default.
    pub fn impact(&self, node_id: &str) -> Vec<&'a Node> {
        self.impact_filtered(node_id, None)
    }

    /// Impact analysis with optional relation filter.
    /// If `relations` is None, traverses all edge types.
    pub fn impact_filtered(&self, node_id: &str, relations: Option<&[&str]>) -> Vec<&'a Node> {
        self.impact_with_filters(node_id, relations, None).nodes
    }

    /// Impact analysis with full filtering: relations + confidence threshold.
    ///
    /// Edges with `confidence < min_confidence` are skipped during traversal
    /// (counted as `hidden_low_confidence` in the result). Edges with
    /// `confidence == None` are treated as fully trusted.
    ///
    /// Pass `min_confidence = None` to disable confidence filtering entirely.
    /// Pass `min_confidence = Some(DEFAULT_MIN_CONFIDENCE)` for safe defaults.
    pub fn impact_with_filters(
        &self,
        node_id: &str,
        relations: Option<&[&str]>,
        min_confidence: Option<f64>,
    ) -> QueryResult<'a> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        let mut hidden_low_confidence = 0usize;
        queue.push_back(node_id.to_string());
        visited.insert(node_id.to_string());

        while let Some(current) = queue.pop_front() {
            for edge in &self.graph.edges {
                if edge.to != current {
                    continue;
                }
                if let Some(rels) = relations {
                    if !rels.contains(&edge.relation.as_str()) {
                        continue;
                    }
                }
                // Confidence gate: hidden edges still get counted so the
                // caller can surface "N hidden low-confidence edges".
                if !edge_passes_confidence(edge, min_confidence) {
                    hidden_low_confidence += 1;
                    continue;
                }
                if visited.insert(edge.from.clone()) {
                    queue.push_back(edge.from.clone());
                }
            }
        }

        visited.remove(node_id);
        let nodes = self.graph.nodes.iter()
            .filter(|n| visited.contains(&n.id))
            .collect();
        QueryResult { nodes, hidden_low_confidence }
    }

    /// Dependencies: what does `node_id` depend on? (transitive)
    /// Traverses all edge relations by default.
    pub fn deps(&self, node_id: &str, transitive: bool) -> Vec<&'a Node> {
        self.deps_filtered(node_id, transitive, None)
    }

    /// Dependencies with optional relation filter.
    /// If `relations` is None, traverses all edge types.
    pub fn deps_filtered(&self, node_id: &str, transitive: bool, relations: Option<&[&str]>) -> Vec<&'a Node> {
        self.deps_with_filters(node_id, transitive, relations, None).nodes
    }

    /// Dependencies with full filtering: relations + confidence threshold.
    ///
    /// See [`Self::impact_with_filters`] for the confidence semantics.
    pub fn deps_with_filters(
        &self,
        node_id: &str,
        transitive: bool,
        relations: Option<&[&str]>,
        min_confidence: Option<f64>,
    ) -> QueryResult<'a> {
        let mut hidden_low_confidence = 0usize;

        if !transitive {
            // Direct deps only — single-hop traversal.
            let mut dep_ids: HashSet<String> = HashSet::new();
            for e in &self.graph.edges {
                if e.from != node_id {
                    continue;
                }
                if let Some(rels) = relations {
                    if !rels.contains(&e.relation.as_str()) {
                        continue;
                    }
                }
                if !edge_passes_confidence(e, min_confidence) {
                    hidden_low_confidence += 1;
                    continue;
                }
                dep_ids.insert(e.to.clone());
            }
            let nodes = self.graph.nodes.iter()
                .filter(|n| dep_ids.contains(&n.id))
                .collect();
            return QueryResult { nodes, hidden_low_confidence };
        }

        // Transitive — BFS over outgoing edges.
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(node_id.to_string());
        visited.insert(node_id.to_string());

        while let Some(current) = queue.pop_front() {
            for edge in &self.graph.edges {
                if edge.from != current {
                    continue;
                }
                if let Some(rels) = relations {
                    if !rels.contains(&edge.relation.as_str()) {
                        continue;
                    }
                }
                if !edge_passes_confidence(edge, min_confidence) {
                    hidden_low_confidence += 1;
                    continue;
                }
                if visited.insert(edge.to.clone()) {
                    queue.push_back(edge.to.clone());
                }
            }
        }

        visited.remove(node_id);
        let nodes = self.graph.nodes.iter()
            .filter(|n| visited.contains(&n.id))
            .collect();
        QueryResult { nodes, hidden_low_confidence }
    }

    /// Find shortest path between two nodes (any edge direction).
    pub fn path(&self, from: &str, to: &str) -> Option<Vec<String>> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        let mut parent: HashMap<String, String> = HashMap::new();

        queue.push_back(from.to_string());
        visited.insert(from.to_string());

        while let Some(current) = queue.pop_front() {
            if current == to {
                // Reconstruct path
                let mut path = vec![to.to_string()];
                let mut cur = to.to_string();
                while let Some(p) = parent.get(&cur) {
                    path.push(p.clone());
                    cur = p.clone();
                }
                path.reverse();
                return Some(path);
            }

            // Follow edges in both directions
            for edge in &self.graph.edges {
                let neighbor = if edge.from == current {
                    &edge.to
                } else if edge.to == current {
                    &edge.from
                } else {
                    continue;
                };
                if visited.insert(neighbor.clone()) {
                    parent.insert(neighbor.clone(), current.clone());
                    queue.push_back(neighbor.clone());
                }
            }
        }

        None
    }

    /// Common cause: find shared dependencies of two nodes.
    pub fn common_cause(&self, node_a: &str, node_b: &str) -> Vec<&'a Node> {
        let deps_a: HashSet<String> = self.deps(node_a, true)
            .iter().map(|n| n.id.clone()).collect();
        let deps_b: HashSet<String> = self.deps(node_b, true)
            .iter().map(|n| n.id.clone()).collect();
        let common: HashSet<&String> = deps_a.intersection(&deps_b).collect();

        self.graph.nodes.iter()
            .filter(|n| common.contains(&n.id))
            .collect()
    }

    /// Topological sort (returns error if cycle detected).
    pub fn topological_sort(&self) -> anyhow::Result<Vec<String>> {
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        for node in &self.graph.nodes {
            in_degree.entry(&node.id).or_insert(0);
        }
        for edge in &self.graph.edges {
            if edge.relation == "depends_on" {
                *in_degree.entry(&edge.from).or_insert(0) += 1;
            }
        }

        let mut queue: VecDeque<&str> = in_degree.iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&id, _)| id)
            .collect();

        let mut sorted = Vec::new();
        while let Some(node) = queue.pop_front() {
            sorted.push(node.to_string());
            for edge in &self.graph.edges {
                if edge.to == node && edge.relation == "depends_on" {
                    if let Some(deg) = in_degree.get_mut(edge.from.as_str()) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(&edge.from);
                        }
                    }
                }
            }
        }

        if sorted.len() != self.graph.nodes.len() {
            anyhow::bail!("Cycle detected in graph");
        }

        Ok(sorted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Graph, Node, Edge};

    fn make_edge(from: &str, to: &str, relation: &str) -> Edge {
        Edge {
            from: from.into(),
            to: to.into(),
            relation: relation.into(),
            weight: None,
            confidence: None,
            metadata: None,
        }
    }

    fn make_test_graph() -> Graph {
        // A → depends_on → B → depends_on → C
        // A → implements → D
        // E → belongs_to → D
        let nodes = vec![
            Node::new("A", "A"),
            Node::new("B", "B"),
            Node::new("C", "C"),
            Node::new("D", "D"),
            Node::new("E", "E"),
        ];
        let edges = vec![
            make_edge("A", "B", "depends_on"),
            make_edge("B", "C", "depends_on"),
            make_edge("A", "D", "implements"),
            make_edge("E", "D", "belongs_to"),
        ];
        Graph { nodes, edges, ..Default::default() }
    }

    #[test]
    fn test_impact_multi_relation() {
        let graph = make_test_graph();
        let qe = QueryEngine::new(&graph);

        // Default impact traverses all relations
        // Changing C: B depends_on C → A depends_on B → impacted: A, B
        let impacted = qe.impact("C");
        let ids: Vec<&str> = impacted.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"B"));
        assert!(ids.contains(&"A"));
    }

    #[test]
    fn test_impact_filtered_depends_on_only() {
        let graph = make_test_graph();
        let qe = QueryEngine::new(&graph);

        // Filter to depends_on: changing D, A implements D but with depends_on filter, not traversed
        let impacted = qe.impact_filtered("D", Some(&["depends_on"]));
        let ids: Vec<&str> = impacted.iter().map(|n| n.id.as_str()).collect();
        // No node has depends_on edge to D
        assert!(ids.is_empty());
    }

    #[test]
    fn test_impact_filtered_all_relations() {
        let graph = make_test_graph();
        let qe = QueryEngine::new(&graph);

        // Changing D: A implements D, E belongs_to D → both impacted
        let impacted = qe.impact_filtered("D", None);
        let ids: Vec<&str> = impacted.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"A"), "A implements D");
        assert!(ids.contains(&"E"), "E belongs_to D");
    }

    #[test]
    fn test_deps_multi_relation() {
        let graph = make_test_graph();
        let qe = QueryEngine::new(&graph);

        // A's deps (all relations): B (depends_on), D (implements), and transitively C (via B)
        let deps = qe.deps("A", true);
        let ids: Vec<&str> = deps.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"B"));
        assert!(ids.contains(&"C"));
        assert!(ids.contains(&"D"));
    }

    #[test]
    fn test_deps_filtered_depends_on_only() {
        let graph = make_test_graph();
        let qe = QueryEngine::new(&graph);

        // A's deps with depends_on filter: B, C — but NOT D (implements edge)
        let deps = qe.deps_filtered("A", true, Some(&["depends_on"]));
        let ids: Vec<&str> = deps.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"B"));
        assert!(ids.contains(&"C"));
        assert!(!ids.contains(&"D"), "D should be excluded — implements, not depends_on");
    }

    #[test]
    fn test_deps_non_transitive_filtered() {
        let graph = make_test_graph();
        let qe = QueryEngine::new(&graph);

        // A direct deps only, all relations: B (depends_on) and D (implements)
        let deps = qe.deps_filtered("A", false, None);
        let ids: Vec<&str> = deps.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"B"));
        assert!(ids.contains(&"D"));
        assert!(!ids.contains(&"C"), "C is transitive, should be excluded");
    }

    #[test]
    fn test_backward_compat_impact_uses_all_relations() {
        let graph = make_test_graph();
        let qe = QueryEngine::new(&graph);

        // impact() without filter should traverse all relations (backward compat)
        let impacted = qe.impact("D");
        let ids: Vec<&str> = impacted.iter().map(|n| n.id.as_str()).collect();
        // A implements D, E belongs_to D
        assert!(ids.contains(&"A"));
        assert!(ids.contains(&"E"));
    }

    // ─── ISS-035 regression tests ───────────────────────────────────────
    //
    // Tree-sitter name-match fallback creates `calls` edges with confidence=0.6.
    // Common Rust method names like `.contains()`, `.clone()`, `.to_string()`
    // produce massive false-positive pollution when those edges are visible
    // by default. These tests verify that:
    //   1. Default min_confidence threshold filters confidence=0.6 edges
    //   2. min_confidence=0.0 (or None) preserves the legacy "show everything" behavior
    //   3. Edges with confidence=None are always included (hand-authored / depends_on)
    //   4. The hidden_low_confidence count is reported accurately

    fn make_edge_with_confidence(from: &str, to: &str, relation: &str, confidence: Option<f64>) -> Edge {
        Edge {
            from: from.into(),
            to: to.into(),
            relation: relation.into(),
            weight: None,
            confidence,
            metadata: None,
        }
    }

    /// Mimics the real-world ISS-035 scenario: a `SessionWorkingMemory.contains`
    /// method gets ~3 real callers (high-confidence LSP edges) plus many
    /// false-positive callers (tree-sitter 0.6 fallback for `Vec::contains`).
    fn make_iss035_graph() -> Graph {
        let nodes = vec![
            Node::new("session_wm.contains", "SessionWorkingMemory::contains"),
            Node::new("real_caller_1", "real_caller_1"),
            Node::new("real_caller_2", "real_caller_2"),
            Node::new("real_caller_3", "real_caller_3"),
            Node::new("parse_soul", "parse_soul"),
            Node::new("tokenize_cjk", "tokenize_cjk_boundaries"),
            Node::new("deserialize_str", "deserialize_flexible_string"),
        ];
        let edges = vec![
            // High-confidence LSP-confirmed callers (confidence=1.0)
            make_edge_with_confidence("real_caller_1", "session_wm.contains", "calls", Some(1.0)),
            make_edge_with_confidence("real_caller_2", "session_wm.contains", "calls", Some(1.0)),
            make_edge_with_confidence("real_caller_3", "session_wm.contains", "calls", Some(0.9)),
            // Tree-sitter false positives — these all call `.contains()` on Vec/HashSet
            // but get attributed to SessionWorkingMemory::contains because of name-only match
            make_edge_with_confidence("parse_soul",      "session_wm.contains", "calls", Some(0.6)),
            make_edge_with_confidence("tokenize_cjk",    "session_wm.contains", "calls", Some(0.6)),
            make_edge_with_confidence("deserialize_str", "session_wm.contains", "calls", Some(0.6)),
        ];
        Graph { nodes, edges, ..Default::default() }
    }

    #[test]
    fn test_iss035_default_threshold_filters_treesitter_fallback() {
        let graph = make_iss035_graph();
        let qe = QueryEngine::new(&graph);

        let result = qe.impact_with_filters(
            "session_wm.contains",
            None,
            Some(DEFAULT_MIN_CONFIDENCE),
        );

        let ids: Vec<&str> = result.nodes.iter().map(|n| n.id.as_str()).collect();
        // Three high-confidence callers visible
        assert!(ids.contains(&"real_caller_1"));
        assert!(ids.contains(&"real_caller_2"));
        assert!(ids.contains(&"real_caller_3"));
        // Three tree-sitter false positives hidden
        assert!(!ids.contains(&"parse_soul"),     "parse_soul is conf=0.6 and must be hidden by default");
        assert!(!ids.contains(&"tokenize_cjk"),   "tokenize_cjk is conf=0.6 and must be hidden by default");
        assert!(!ids.contains(&"deserialize_str"),"deserialize_str is conf=0.6 and must be hidden by default");
        // Hidden count surfaces the noise so the agent knows fallback data exists
        assert_eq!(result.hidden_low_confidence, 3);
    }

    #[test]
    fn test_iss035_zero_threshold_includes_all() {
        let graph = make_iss035_graph();
        let qe = QueryEngine::new(&graph);

        let result = qe.impact_with_filters("session_wm.contains", None, Some(0.0));
        let ids: Vec<&str> = result.nodes.iter().map(|n| n.id.as_str()).collect();
        // All 6 callers visible when threshold = 0.0
        assert_eq!(result.nodes.len(), 6);
        assert!(ids.contains(&"parse_soul"));
        assert_eq!(result.hidden_low_confidence, 0);
    }

    #[test]
    fn test_iss035_no_threshold_includes_all_legacy_behavior() {
        let graph = make_iss035_graph();
        let qe = QueryEngine::new(&graph);

        // min_confidence = None preserves legacy "show everything" behavior
        let result = qe.impact_with_filters("session_wm.contains", None, None);
        assert_eq!(result.nodes.len(), 6);
        assert_eq!(result.hidden_low_confidence, 0);
    }

    #[test]
    fn test_iss035_none_confidence_treated_as_trusted() {
        // Hand-authored design edges, depends_on task edges, and legacy graph
        // edges all have confidence=None. They MUST always be visible, even
        // with a strict threshold — otherwise we'd silently break every
        // non-code-graph query.
        let nodes = vec![
            Node::new("task-a", "task-a"),
            Node::new("task-b", "task-b"),
        ];
        let edges = vec![
            // Confidence=None: hand-authored task dependency
            make_edge_with_confidence("task-a", "task-b", "depends_on", None),
        ];
        let graph = Graph { nodes, edges, ..Default::default() };
        let qe = QueryEngine::new(&graph);

        let result = qe.impact_with_filters("task-b", None, Some(0.99));
        let ids: Vec<&str> = result.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"task-a"), "None-confidence edge must always be visible");
        assert_eq!(result.hidden_low_confidence, 0);
    }

    #[test]
    fn test_iss035_deps_with_filters_respects_threshold() {
        // Mirror impact behavior on the outgoing-edge direction (deps).
        let nodes = vec![
            Node::new("caller", "caller"),
            Node::new("real_callee", "real_callee"),
            Node::new("noise_callee", "noise_callee"),
        ];
        let edges = vec![
            make_edge_with_confidence("caller", "real_callee",  "calls", Some(1.0)),
            make_edge_with_confidence("caller", "noise_callee", "calls", Some(0.6)),
        ];
        let graph = Graph { nodes, edges, ..Default::default() };
        let qe = QueryEngine::new(&graph);

        let result = qe.deps_with_filters("caller", false, None, Some(DEFAULT_MIN_CONFIDENCE));
        let ids: Vec<&str> = result.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"real_callee"));
        assert!(!ids.contains(&"noise_callee"));
        assert_eq!(result.hidden_low_confidence, 1);
    }

    #[test]
    fn test_iss035_deps_transitive_threshold_blocks_traversal() {
        // A → B (high-conf) → C (low-conf): with default threshold,
        // C must NOT be reachable transitively from A.
        // This proves the filter blocks traversal, not just final filtering.
        let nodes = vec![
            Node::new("A", "A"),
            Node::new("B", "B"),
            Node::new("C", "C"),
        ];
        let edges = vec![
            make_edge_with_confidence("A", "B", "calls", Some(1.0)),
            make_edge_with_confidence("B", "C", "calls", Some(0.6)),
        ];
        let graph = Graph { nodes, edges, ..Default::default() };
        let qe = QueryEngine::new(&graph);

        let result = qe.deps_with_filters("A", true, None, Some(DEFAULT_MIN_CONFIDENCE));
        let ids: Vec<&str> = result.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"B"), "B is reachable via high-conf edge");
        assert!(!ids.contains(&"C"), "C must be unreachable — its inbound edge is low-conf");
        assert_eq!(result.hidden_low_confidence, 1);
    }
}
