//! Unify — convert CodeGraph to graph::Node/Edge for unified graph storage.

use crate::graph::{Graph, Node, Edge, NodeStatus};
use crate::code_graph::{CodeGraph, CodeEdge, CodeNode};
use std::collections::HashMap;

/// Convert CodeGraph nodes and edges to graph-layer Nodes and Edges.
/// All resulting nodes have `source: "extract"`, `node_type: "code"`, `status: Done`.
pub fn codegraph_to_graph_nodes(cg: &CodeGraph, _project_root: &std::path::Path) -> (Vec<Node>, Vec<Edge>) {
    let mut nodes = Vec::with_capacity(cg.nodes.len());
    let mut edges = Vec::with_capacity(cg.edges.len());

    for cn in &cg.nodes {
        let mut node = Node::new(&cn.id, &cn.name);
        node.source = Some("extract".to_string());
        node.node_type = Some("code".to_string());
        node.node_kind = Some(format!("{:?}", cn.kind)); // "File", "Class", "Function", etc.
        node.status = NodeStatus::Done;
        node.file_path = Some(cn.file_path.clone());
        if let Some(line) = cn.line {
            node.start_line = Some(line);
        }
        if let Some(ref sig) = cn.signature {
            node.signature = Some(sig.clone());
        }
        if let Some(ref doc) = cn.docstring {
            node.doc_comment = Some(doc.clone());
        }
        // Store additional fields in metadata
        if cn.is_test {
            node.metadata.insert("is_test".to_string(), serde_json::json!(true));
        }
        if cn.line_count > 0 {
            node.metadata.insert("line_count".to_string(), serde_json::json!(cn.line_count));
        }
        if !cn.decorators.is_empty() {
            node.metadata.insert("decorators".to_string(), serde_json::json!(cn.decorators));
        }
        nodes.push(node);
    }

    for ce in &cg.edges {
        let relation = ce.relation.to_string(); // "imports", "calls", "inherits", etc.
        let mut edge = Edge::new(&ce.from, &ce.to, &relation);
        let mut meta = serde_json::Map::new();
        meta.insert("source".to_string(), serde_json::json!("extract"));
        if ce.weight != 0.5 {
            meta.insert("weight".to_string(), serde_json::json!(ce.weight));
        }
        if ce.call_count > 1 {
            meta.insert("call_count".to_string(), serde_json::json!(ce.call_count));
        }
        if ce.in_error_path {
            meta.insert("in_error_path".to_string(), serde_json::json!(true));
        }
        if ce.confidence != 1.0 {
            meta.insert("confidence".to_string(), serde_json::json!(ce.confidence));
        }
        edge.metadata = Some(serde_json::Value::Object(meta));
        edges.push(edge);
    }

    (nodes, edges)
}

/// Merge code-layer nodes/edges into an existing graph.
/// Removes old code nodes (`source == "extract"`) and bridge edges (`source == "auto-bridge"`),
/// then appends new code nodes/edges.
pub fn merge_code_layer(graph: &mut Graph, code_nodes: Vec<Node>, code_edges: Vec<Edge>) {
    // Remove old code nodes
    graph.nodes.retain(|n| n.source.as_deref() != Some("extract"));
    // Remove old code edges and bridge edges
    graph.edges.retain(|e| {
        let src = e.source();
        src != Some("extract") && src != Some("auto-bridge")
    });
    // Append new
    graph.nodes.extend(code_nodes);
    graph.edges.extend(code_edges);
}

/// Merge project-layer nodes into an existing graph (preserving code layer).
/// Used by design --parse and ritual generate-graph.
pub fn merge_project_layer(existing: &mut Graph, new_project: Graph) {
    // Retain code-layer nodes and bridge edges
    let code_nodes: Vec<Node> = existing.nodes.drain(..).filter(|n| n.source.as_deref() == Some("extract")).collect();
    let code_and_bridge_edges: Vec<Edge> = existing.edges.drain(..).filter(|e| {
        let src = e.source();
        src == Some("extract") || src == Some("auto-bridge")
    }).collect();

    // Replace project layer with new project nodes/edges
    // Set source on new project nodes
    let mut project_nodes: Vec<Node> = new_project.nodes.into_iter().map(|mut n| {
        if n.source.is_none() {
            n.source = Some("project".to_string());
        }
        n
    }).collect();

    // Restore code nodes + new project nodes
    existing.nodes = code_nodes;
    existing.nodes.append(&mut project_nodes);

    // Restore code/bridge edges + new project edges
    existing.edges = code_and_bridge_edges;
    existing.edges.extend(new_project.edges);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_graph::{CodeNode, CodeEdge, CodeGraph, NodeKind, EdgeRelation};
    use std::path::Path;

    fn sample_codegraph() -> CodeGraph {
        let mut cg = CodeGraph::default();
        let file = CodeNode::new_file("src/main.rs");
        let func = CodeNode::new_function("src/main.rs", "main", 5, false);
        let class = CodeNode::new_class("src/auth.rs", "AuthService", 10);
        cg.nodes = vec![file, func, class];
        cg.edges = vec![
            CodeEdge::new("func:src/main.rs:main", "file:src/main.rs", EdgeRelation::DefinedIn),
            CodeEdge::new("func:src/main.rs:main", "class:src/auth.rs:AuthService", EdgeRelation::Calls),
        ];
        cg
    }

    #[test]
    fn test_codegraph_to_graph_nodes_basic() {
        let cg = sample_codegraph();
        let (nodes, edges) = codegraph_to_graph_nodes(&cg, Path::new("/tmp/project"));
        assert_eq!(nodes.len(), 3);
        assert_eq!(edges.len(), 2);

        // All nodes have source=extract, type=code, status=Done
        for n in &nodes {
            assert_eq!(n.source.as_deref(), Some("extract"));
            assert_eq!(n.node_type.as_deref(), Some("code"));
            assert_eq!(n.status, NodeStatus::Done);
        }
    }

    #[test]
    fn test_codegraph_node_kind_mapping() {
        let cg = sample_codegraph();
        let (nodes, _) = codegraph_to_graph_nodes(&cg, Path::new("/tmp"));

        let file_node = nodes.iter().find(|n| n.id == "file:src/main.rs").unwrap();
        assert_eq!(file_node.node_kind.as_deref(), Some("File"));

        let func_node = nodes.iter().find(|n| n.id == "func:src/main.rs:main").unwrap();
        assert_eq!(func_node.node_kind.as_deref(), Some("Function"));
        assert_eq!(func_node.start_line, Some(5));
    }

    #[test]
    fn test_codegraph_edge_conversion() {
        let cg = sample_codegraph();
        let (_, edges) = codegraph_to_graph_nodes(&cg, Path::new("/tmp"));

        let defined_in = edges.iter().find(|e| e.relation == "defined_in").unwrap();
        assert_eq!(defined_in.source(), Some("extract"));

        let calls = edges.iter().find(|e| e.relation == "calls").unwrap();
        assert_eq!(calls.source(), Some("extract"));
    }

    #[test]
    fn test_merge_code_layer() {
        let mut graph = Graph::new();
        // Add a project node
        let mut task = Node::new("task-1", "My Task");
        task.source = Some("project".to_string());
        graph.add_node(task);
        graph.add_edge(Edge::new("task-1", "task-2", "depends_on"));

        // Old code node that should be replaced
        let mut old_code = Node::new("file:old.rs", "old file");
        old_code.source = Some("extract".to_string());
        graph.add_node(old_code);

        let cg = sample_codegraph();
        let (code_nodes, code_edges) = codegraph_to_graph_nodes(&cg, Path::new("/tmp"));
        merge_code_layer(&mut graph, code_nodes, code_edges);

        // Project node preserved
        assert!(graph.nodes.iter().any(|n| n.id == "task-1"));
        // Old code node gone
        assert!(!graph.nodes.iter().any(|n| n.id == "file:old.rs"));
        // New code nodes present
        assert!(graph.nodes.iter().any(|n| n.id == "file:src/main.rs"));
        // Project edge preserved
        assert!(graph.edges.iter().any(|e| e.relation == "depends_on"));
    }

    #[test]
    fn test_merge_project_layer() {
        let mut existing = Graph::new();
        // Code node
        let mut code = Node::new("file:src/main.rs", "main.rs");
        code.source = Some("extract".to_string());
        existing.add_node(code);

        let mut code_edge = Edge::new("file:src/main.rs", "func:main", "defined_in");
        code_edge.metadata = Some(serde_json::json!({"source": "extract"}));
        existing.add_edge(code_edge);

        // Old project node
        let mut old_task = Node::new("old-task", "Old");
        old_task.source = Some("project".to_string());
        existing.add_node(old_task);

        // New project graph from LLM
        let mut new_project = Graph::new();
        new_project.add_node(Node::new("task-1", "New Task"));
        new_project.add_edge(Edge::new("task-1", "task-2", "depends_on"));

        merge_project_layer(&mut existing, new_project);

        // Code node preserved
        assert!(existing.nodes.iter().any(|n| n.id == "file:src/main.rs"));
        // Old project node gone
        assert!(!existing.nodes.iter().any(|n| n.id == "old-task"));
        // New project node present with source=project
        let task = existing.nodes.iter().find(|n| n.id == "task-1").unwrap();
        assert_eq!(task.source.as_deref(), Some("project"));
    }

    #[test]
    fn test_all_node_kinds() {
        let mut cg = CodeGraph::default();
        cg.nodes = vec![
            CodeNode::new_file("src/lib.rs"),
            CodeNode::new_class("src/lib.rs", "Foo", 1),
            CodeNode::new_function("src/lib.rs", "bar", 10, false),
            CodeNode::new_module("src/mod"),
            CodeNode::new_constant("src/lib.rs", "MAX", 1),
            CodeNode::new_interface("src/lib.rs", "IService", 20),
            CodeNode::new_enum("src/e.rs", "Color", 1),
            CodeNode::new_type_alias("src/t.rs", "Id", 1),
            CodeNode::new_trait("src/tr.rs", "Storage", 1),
        ];
        let (nodes, _) = codegraph_to_graph_nodes(&cg, Path::new("/tmp"));
        assert_eq!(nodes.len(), 9);
        // Verify all have extract source
        assert!(nodes.iter().all(|n| n.source.as_deref() == Some("extract")));
    }

    #[test]
    fn test_edge_metadata_fields() {
        let mut cg = CodeGraph::default();
        cg.nodes = vec![
            CodeNode::new_function("src/a.rs", "foo", 1, false),
            CodeNode::new_function("src/b.rs", "bar", 1, false),
        ];
        let mut edge = CodeEdge::new("func:src/a.rs:foo", "func:src/b.rs:bar", EdgeRelation::Calls);
        edge.call_count = 5;
        edge.in_error_path = true;
        edge.confidence = 0.8;
        edge.weight = 0.9;
        cg.edges = vec![edge];

        let (_, edges) = codegraph_to_graph_nodes(&cg, Path::new("/tmp"));
        assert_eq!(edges.len(), 1);
        let e = &edges[0];
        let meta = e.metadata.as_ref().unwrap();
        assert_eq!(meta.get("source").unwrap(), "extract");
        assert_eq!(meta.get("call_count").unwrap(), 5);
        assert_eq!(meta.get("in_error_path").unwrap(), true);
        assert!(meta.get("confidence").is_some());
        assert!(meta.get("weight").is_some());
    }

    #[test]
    fn test_node_metadata_fields() {
        let mut cg = CodeGraph::default();
        let mut func = CodeNode::new_function("src/test.rs", "test_foo", 10, false);
        func.is_test = true;
        func.line_count = 25;
        func.decorators = vec!["#[test]".to_string()];
        func.signature = Some("fn test_foo()".to_string());
        func.docstring = Some("A test function".to_string());
        cg.nodes = vec![func];

        let (nodes, _) = codegraph_to_graph_nodes(&cg, Path::new("/tmp"));
        let n = &nodes[0];
        assert_eq!(n.signature.as_deref(), Some("fn test_foo()"));
        assert_eq!(n.doc_comment.as_deref(), Some("A test function"));
        assert_eq!(n.metadata.get("is_test"), Some(&serde_json::json!(true)));
        assert_eq!(n.metadata.get("line_count"), Some(&serde_json::json!(25)));
        assert_eq!(n.metadata.get("decorators"), Some(&serde_json::json!(["#[test]"])));
    }

    #[test]
    fn test_merge_code_layer_removes_bridge_edges() {
        let mut graph = Graph::new();
        // Add a bridge edge
        let mut bridge_edge = Edge::new("task-1", "file:src/main.rs", "touches");
        bridge_edge.metadata = Some(serde_json::json!({"source": "auto-bridge"}));
        graph.add_edge(bridge_edge);

        // Add a project edge
        graph.add_edge(Edge::new("task-1", "task-2", "depends_on"));

        let (code_nodes, code_edges) = codegraph_to_graph_nodes(&CodeGraph::default(), Path::new("/tmp"));
        merge_code_layer(&mut graph, code_nodes, code_edges);

        // Bridge edge removed
        assert!(!graph.edges.iter().any(|e| e.source() == Some("auto-bridge")));
        // Project edge preserved
        assert!(graph.edges.iter().any(|e| e.relation == "depends_on"));
    }
}
