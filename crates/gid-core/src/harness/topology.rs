//! Topology analysis: cycle detection, layer computation, critical path.
//!
//! All functions are pure — no I/O, no side effects.

use std::collections::{HashMap, HashSet, VecDeque};
use anyhow::{Result, bail};
use crate::graph::{Graph, NodeStatus};

/// Detect all cycles in the dependency graph.
/// Returns a vector of cycles, where each cycle is a vector of node IDs.
/// Returns empty vec if the graph is acyclic.
pub fn detect_cycles(graph: &Graph) -> Vec<Vec<String>> {
    let mut cycles = Vec::new();
    let mut visited = HashSet::new();
    let mut rec_stack = HashSet::new();
    let mut path = Vec::new();

    // Build adjacency list (depends_on edges: from depends on to)
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for node in &graph.nodes {
        adj.entry(&node.id).or_default();
    }
    for edge in &graph.edges {
        if edge.relation == "depends_on" {
            adj.entry(&edge.from).or_default().push(&edge.to);
        }
    }

    for node in &graph.nodes {
        if !visited.contains(node.id.as_str()) {
            dfs_cycles(
                &node.id,
                &adj,
                &mut visited,
                &mut rec_stack,
                &mut path,
                &mut cycles,
            );
        }
    }

    cycles
}

fn dfs_cycles<'a>(
    node: &'a str,
    adj: &HashMap<&'a str, Vec<&'a str>>,
    visited: &mut HashSet<&'a str>,
    rec_stack: &mut HashSet<&'a str>,
    path: &mut Vec<&'a str>,
    cycles: &mut Vec<Vec<String>>,
) {
    visited.insert(node);
    rec_stack.insert(node);
    path.push(node);

    if let Some(neighbors) = adj.get(node) {
        for &next in neighbors {
            if !visited.contains(next) {
                dfs_cycles(next, adj, visited, rec_stack, path, cycles);
            } else if rec_stack.contains(next) {
                // Found a cycle — extract it from path
                let start = path.iter().position(|&n| n == next).unwrap();
                let cycle: Vec<String> = path[start..].iter().map(|s| s.to_string()).collect();
                cycles.push(cycle);
            }
        }
    }

    path.pop();
    rec_stack.remove(node);
}

/// Group tasks into parallelizable layers via topological sort.
///
/// Layer N depends only on layers 0..N-1.
/// Tasks enter the earliest possible layer based on actual dependencies.
/// Only includes tasks that are not yet `done` (or `cancelled`).
/// Returns error if cycles are detected.
///
/// Uses Kahn's algorithm: initialize in-degree map, BFS from zero-degree nodes,
/// group by depth level.
pub fn compute_layers(graph: &Graph) -> Result<Vec<Vec<String>>> {
    // Filter to pending task nodes only
    let task_ids: HashSet<&str> = graph.nodes.iter()
        .filter(|n| {
            n.node_type.as_deref() == Some("task")
                && !matches!(n.status, NodeStatus::Done | NodeStatus::Cancelled)
        })
        .map(|n| n.id.as_str())
        .collect();

    if task_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Build in-degree map for task nodes only (depends_on edges)
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    for &id in &task_ids {
        in_degree.entry(id).or_insert(0);
    }
    for edge in &graph.edges {
        if edge.relation == "depends_on"
            && task_ids.contains(edge.from.as_str())
        {
            // Check if dependency is pending (not done)
            let dep_done = graph.get_node(&edge.to)
                .map(|n| matches!(n.status, NodeStatus::Done))
                .unwrap_or(true); // missing dep treated as done (already satisfied)
            
            if !dep_done && task_ids.contains(edge.to.as_str()) {
                *in_degree.entry(edge.from.as_str()).or_insert(0) += 1;
            }
        }
    }

    // BFS by layers
    let mut layers = Vec::new();
    let mut queue: VecDeque<&str> = in_degree.iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&id, _)| id)
        .collect();

    // Sort for deterministic output
    let mut initial: Vec<&str> = queue.drain(..).collect();
    initial.sort();
    queue.extend(initial);

    let mut processed = HashSet::new();

    while !queue.is_empty() {
        let layer_size = queue.len();
        let mut layer = Vec::new();

        for _ in 0..layer_size {
            let node = queue.pop_front().unwrap();
            layer.push(node.to_string());
            processed.insert(node);
        }

        // Sort layer for deterministic output
        layer.sort();

        // Find newly unblocked nodes
        let mut next_ready: Vec<&str> = Vec::new();
        for edge in &graph.edges {
            if edge.relation == "depends_on"
                && task_ids.contains(edge.from.as_str())
                && !processed.contains(edge.from.as_str())
            {
                if let Some(_deg) = in_degree.get_mut(edge.from.as_str()) {
                    if processed.contains(edge.to.as_str()) || 
                       graph.get_node(&edge.to).map(|n| n.status == NodeStatus::Done).unwrap_or(true) {
                        // This dependency was just satisfied
                    }
                }
            }
        }
        
        // Recompute: check which unprocessed tasks now have all deps satisfied
        for &id in &task_ids {
            if processed.contains(id) {
                continue;
            }
            let all_deps_done = graph.edges.iter()
                .filter(|e| e.from == id && e.relation == "depends_on")
                .all(|e| {
                    processed.contains(e.to.as_str())
                        || graph.get_node(&e.to)
                            .map(|n| n.status == NodeStatus::Done)
                            .unwrap_or(true)
                });
            if all_deps_done && !queue.iter().any(|&q| q == id) && !next_ready.contains(&id) {
                next_ready.push(id);
            }
        }
        next_ready.sort();
        queue.extend(next_ready);

        layers.push(layer);
    }

    // Check for unprocessed nodes (indicates cycle)
    let unprocessed: Vec<&str> = task_ids.iter()
        .filter(|&&id| !processed.contains(id))
        .copied()
        .collect();
    if !unprocessed.is_empty() {
        bail!(
            "Cycle detected involving {} task(s): {}",
            unprocessed.len(),
            unprocessed.join(", ")
        );
    }

    Ok(layers)
}

/// Find the critical path (longest dependency chain) through pending tasks.
/// Returns the chain of task IDs from first to last.
pub fn critical_path(graph: &Graph) -> Vec<String> {
    let task_ids: HashSet<&str> = graph.nodes.iter()
        .filter(|n| {
            n.node_type.as_deref() == Some("task")
                && !matches!(n.status, NodeStatus::Done | NodeStatus::Cancelled)
        })
        .map(|n| n.id.as_str())
        .collect();

    if task_ids.is_empty() {
        return Vec::new();
    }

    // Build adjacency: task → tasks it blocks (reverse of depends_on)
    let mut blocked_by: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &graph.edges {
        if edge.relation == "depends_on"
            && task_ids.contains(edge.from.as_str())
            && task_ids.contains(edge.to.as_str())
        {
            blocked_by.entry(&edge.to).or_default().push(&edge.from);
        }
    }

    // Find longest path using DFS + memoization
    let mut memo: HashMap<&str, Vec<String>> = HashMap::new();

    fn longest_from<'a>(
        node: &'a str,
        blocked_by: &HashMap<&'a str, Vec<&'a str>>,
        memo: &mut HashMap<&'a str, Vec<String>>,
    ) -> Vec<String> {
        if let Some(cached) = memo.get(node) {
            return cached.clone();
        }

        let mut best = Vec::new();
        if let Some(nexts) = blocked_by.get(node) {
            for &next in nexts {
                let path = longest_from(next, blocked_by, memo);
                if path.len() > best.len() {
                    best = path;
                }
            }
        }

        let mut result = vec![node.to_string()];
        result.extend(best);
        memo.insert(node, result.clone());
        result
    }

    let mut best_path = Vec::new();
    for &id in &task_ids {
        let path = longest_from(id, &blocked_by, &mut memo);
        if path.len() > best_path.len() {
            best_path = path;
        }
    }

    best_path
}

/// Find orphan tasks — tasks with no incoming or outgoing dependency edges.
/// These execute in layer 0 but might indicate missing relationships.
pub fn orphan_tasks(graph: &Graph) -> Vec<String> {
    let task_ids: HashSet<&str> = graph.nodes.iter()
        .filter(|n| n.node_type.as_deref() == Some("task"))
        .map(|n| n.id.as_str())
        .collect();

    let connected: HashSet<&str> = graph.edges.iter()
        .filter(|e| e.relation == "depends_on")
        .flat_map(|e| [e.from.as_str(), e.to.as_str()])
        .collect();

    // Also consider implements edges as connections
    let implements_connected: HashSet<&str> = graph.edges.iter()
        .filter(|e| e.relation == "implements")
        .map(|e| e.from.as_str())
        .collect();

    task_ids.iter()
        .filter(|&&id| !connected.contains(id) && !implements_connected.contains(id))
        .map(|&s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Node, Edge};

    fn make_task(id: &str, title: &str) -> Node {
        let mut n = Node::new(id, title);
        n.node_type = Some("task".to_string());
        n
    }

    #[test]
    fn test_detect_cycles_acyclic() {
        let mut graph = Graph::new();
        graph.add_node(make_task("a", "A"));
        graph.add_node(make_task("b", "B"));
        graph.add_edge(Edge::depends_on("b", "a"));
        assert!(detect_cycles(&graph).is_empty());
    }

    #[test]
    fn test_detect_cycles_cyclic() {
        let mut graph = Graph::new();
        graph.add_node(make_task("a", "A"));
        graph.add_node(make_task("b", "B"));
        graph.add_node(make_task("c", "C"));
        graph.add_edge(Edge::depends_on("a", "b"));
        graph.add_edge(Edge::depends_on("b", "c"));
        graph.add_edge(Edge::depends_on("c", "a"));
        let cycles = detect_cycles(&graph);
        assert!(!cycles.is_empty());
    }

    #[test]
    fn test_compute_layers_simple() {
        let mut graph = Graph::new();
        graph.add_node(make_task("a", "A"));
        graph.add_node(make_task("b", "B"));
        graph.add_node(make_task("c", "C"));
        graph.add_edge(Edge::depends_on("b", "a"));
        graph.add_edge(Edge::depends_on("c", "b"));

        let layers = compute_layers(&graph).unwrap();
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0], vec!["a"]);
        assert_eq!(layers[1], vec!["b"]);
        assert_eq!(layers[2], vec!["c"]);
    }

    #[test]
    fn test_compute_layers_parallel() {
        let mut graph = Graph::new();
        graph.add_node(make_task("base", "Base"));
        graph.add_node(make_task("a", "A"));
        graph.add_node(make_task("b", "B"));
        graph.add_edge(Edge::depends_on("a", "base"));
        graph.add_edge(Edge::depends_on("b", "base"));

        let layers = compute_layers(&graph).unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0], vec!["base"]);
        assert!(layers[1].contains(&"a".to_string()));
        assert!(layers[1].contains(&"b".to_string()));
    }

    #[test]
    fn test_compute_layers_skips_done() {
        let mut graph = Graph::new();
        let mut done_task = make_task("a", "A");
        done_task.status = NodeStatus::Done;
        graph.add_node(done_task);
        graph.add_node(make_task("b", "B"));
        graph.add_edge(Edge::depends_on("b", "a"));

        let layers = compute_layers(&graph).unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0], vec!["b"]);
    }

    #[test]
    fn test_critical_path() {
        let mut graph = Graph::new();
        graph.add_node(make_task("a", "A"));
        graph.add_node(make_task("b", "B"));
        graph.add_node(make_task("c", "C"));
        graph.add_node(make_task("d", "D"));
        // a → b → c (length 3)
        // a → d (length 2)
        graph.add_edge(Edge::depends_on("b", "a"));
        graph.add_edge(Edge::depends_on("c", "b"));
        graph.add_edge(Edge::depends_on("d", "a"));

        let cp = critical_path(&graph);
        assert_eq!(cp, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_orphan_tasks() {
        let mut graph = Graph::new();
        graph.add_node(make_task("a", "Connected"));
        graph.add_node(make_task("b", "Connected"));
        graph.add_node(make_task("c", "Orphan"));
        graph.add_edge(Edge::depends_on("b", "a"));

        let orphans = orphan_tasks(&graph);
        assert_eq!(orphans, vec!["c"]);
    }
}
