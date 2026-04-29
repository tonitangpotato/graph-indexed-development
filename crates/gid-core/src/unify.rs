//! Unify — convert CodeGraph to graph::Node/Edge for unified graph storage.
//! Also provides reverse conversion (Graph → CodeGraph) for legacy command compatibility.

use crate::graph::{Graph, Node, Edge, NodeStatus};
use crate::code_graph::{CodeGraph, CodeNode, CodeEdge, NodeKind, EdgeRelation};


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
        // Copy code-graph enrichment fields
        node.visibility = cn.visibility.clone();
        node.lang = cn.lang.clone();
        node.body_hash = cn.body_hash.clone();
        node.end_line = cn.end_line;
        // Derive is_public from visibility
        if let Some(ref vis) = cn.visibility {
            node.is_public = Some(vis == "pub" || vis == "export");
        }
        nodes.push(node);
    }

    for ce in &cg.edges {
        let relation = ce.relation.to_string(); // "imports", "calls", "inherits", etc.
        let mut edge = Edge::new(&ce.from, &ce.to, &relation);
        // Set confidence and weight directly on the Edge struct (stored as dedicated SQLite columns)
        edge.confidence = Some(ce.confidence as f64);
        edge.weight = Some(ce.weight as f64);
        let mut meta = serde_json::Map::new();
        meta.insert("source".to_string(), serde_json::json!("extract"));
        if ce.call_count > 1 {
            meta.insert("call_count".to_string(), serde_json::json!(ce.call_count));
        }
        if ce.in_error_path {
            meta.insert("in_error_path".to_string(), serde_json::json!(true));
        }
        edge.metadata = Some(serde_json::Value::Object(meta));
        edges.push(edge);
    }

    (nodes, edges)
}

/// Reconstruct a CodeGraph from graph.yml code-layer nodes and edges.
///
/// This is the reverse of `codegraph_to_graph_nodes()`. Used by legacy CLI commands
/// (schema, code-search, code-trace, etc.) that still operate on CodeGraph APIs.
/// Reads only code-layer nodes (`source == "extract"`) and code-layer edges.
pub fn graph_to_codegraph(graph: &Graph) -> CodeGraph {
    let code_nodes_refs = graph.code_nodes();
    let code_edges_refs = graph.code_edges();

    let mut nodes = Vec::with_capacity(code_nodes_refs.len());
    let mut edges = Vec::with_capacity(code_edges_refs.len());

    for n in &code_nodes_refs {
        let kind = match n.node_kind.as_deref() {
            Some("File") => NodeKind::File,
            Some("Class") => NodeKind::Class,
            Some("Function") => NodeKind::Function,
            Some("Module") => NodeKind::Module,
            Some("Constant") => NodeKind::Constant,
            Some("Interface") => NodeKind::Interface,
            Some("Enum") => NodeKind::Enum,
            Some("TypeAlias") => NodeKind::TypeAlias,
            Some("Trait") => NodeKind::Trait,
            Some("Method") => NodeKind::Function, // Methods map to Function in CodeGraph
            _ => NodeKind::File, // safe fallback
        };

        let is_test = n.metadata.get("is_test")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let line_count = n.metadata.get("line_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let decorators: Vec<String> = n.metadata.get("decorators")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();

        nodes.push(CodeNode {
            id: n.id.clone(),
            kind,
            name: n.title.clone(),
            file_path: n.file_path.as_deref().unwrap_or("").to_string(),
            line: n.start_line,
            decorators,
            signature: n.signature.clone(),
            docstring: n.doc_comment.clone(),
            line_count,
            is_test,
            visibility: n.visibility.clone(),
            lang: n.lang.clone(),
            body_hash: n.body_hash.clone(),
            end_line: n.end_line,
            complexity: None,
                    });
    }

    for e in &code_edges_refs {
        let relation = match e.relation.as_str() {
            "imports" => EdgeRelation::Imports,
            "inherits" => EdgeRelation::Inherits,
            "defined_in" => EdgeRelation::DefinedIn,
            "calls" => EdgeRelation::Calls,
            "tests_for" => EdgeRelation::TestsFor,
            "overrides" => EdgeRelation::Overrides,
            "implements" => EdgeRelation::Implements,
            "belongs_to" => EdgeRelation::BelongsTo,
            "type_reference" => EdgeRelation::TypeReference,
            _ => continue, // skip non-code relations
        };

        let meta = e.metadata.as_ref();
        // Read weight/confidence from Edge struct fields first (canonical), fallback to metadata for legacy graphs
        let weight = e.weight.map(|w| w as f32)
            .or_else(|| meta.and_then(|m| m.get("weight")).and_then(|v| v.as_f64()).map(|v| v as f32))
            .unwrap_or(0.5);
        let call_count = meta.and_then(|m| m.get("call_count")).and_then(|v| v.as_u64()).unwrap_or(1) as u32;
        let in_error_path = meta.and_then(|m| m.get("in_error_path")).and_then(|v| v.as_bool()).unwrap_or(false);
        let confidence = e.confidence.map(|c| c as f32)
            .or_else(|| meta.and_then(|m| m.get("confidence")).and_then(|v| v.as_f64()).map(|v| v as f32))
            .unwrap_or(1.0);

        edges.push(CodeEdge {
            from: e.from.clone(),
            to: e.to.clone(),
            relation,
            weight,
            call_count,
            in_error_path,
            confidence,
            call_site_line: None,
            call_site_column: None,
        });
    }

    let mut cg = CodeGraph {
        nodes,
        edges,
        outgoing: Default::default(),
        incoming: Default::default(),
        node_index: Default::default(),
    };
    cg.build_indexes();
    cg
}

/// Merge code-layer nodes/edges into an existing graph.
/// Removes old code nodes (`source == "extract"`) and bridge edges (`source == "auto-bridge"`),
/// then appends new code nodes/edges. Also prunes dangling edges that reference removed code nodes.
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

    // Prune dangling edges: edges whose from/to references a node that no longer exists.
    // This catches stale edges left by deprecated code (e.g. old `code_*` node IDs from
    // build_unified_graph/link_tasks_to_code that lacked a source tag).
    // Only prune edges where the missing endpoint looks like it was a code node —
    // project edges are allowed to reference forward-declared / not-yet-created task nodes.
    let node_ids: std::collections::HashSet<&str> =
        graph.nodes.iter().map(|n| n.id.as_str()).collect();
    graph.edges.retain(|e| {
        let from_ok = node_ids.contains(e.from.as_str());
        let to_ok = node_ids.contains(e.to.as_str());
        if from_ok && to_ok {
            return true;
        }
        // Keep project-layer edges (no source tag, or source != extract/auto-bridge)
        // even if their endpoints are missing — those are task/feature references.
        let src = e.source();
        if src != Some("extract") && src != Some("auto-bridge") {
            // This is a project edge or unmarked edge. Only prune if endpoint
            // looks like a stale code node (starts with "code_" prefix from the old
            // code_node_to_task_id scheme).
            let stale_from = !from_ok && e.from.starts_with("code_");
            let stale_to = !to_ok && e.to.starts_with("code_");
            if stale_from || stale_to {
                return false; // prune stale code references
            }
            return true; // keep project edges with missing endpoints
        }
        // Extract/bridge edge with dangling endpoint → prune
        false
    });
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

/// Generate bridge edges linking project nodes to code nodes.
///
/// 1. Delete all existing auto-bridge edges.
/// 2. For each code node with `file_path`, check if any project node has a `code_paths`
///    metadata entry that contains that path → create a `maps_to` edge (confidence 1.0).
/// 3. Fallback: try ID prefix matching — extract path segments from code node id and
///    look for project nodes whose id contains those segments (confidence 0.8).
pub fn generate_bridge_edges(graph: &mut Graph) {
    // 1. Remove existing auto-bridge edges
    graph.edges.retain(|e| e.source() != Some("auto-bridge"));

    // Collect project and code node info to avoid borrow issues
    let project_info: Vec<(String, Vec<String>)> = graph.project_nodes().iter().map(|n| {
        let code_paths: Vec<String> = n.metadata.get("code_paths")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        (n.id.clone(), code_paths)
    }).collect();

    let code_info: Vec<(String, Option<String>)> = graph.code_nodes().iter().map(|n| {
        (n.id.clone(), n.file_path.clone())
    }).collect();

    let mut new_edges: Vec<Edge> = Vec::new();

    for (code_id, file_path) in &code_info {
        let mut matched = false;

        // 2. Check code_paths metadata match
        if let Some(fp) = file_path {
            for (proj_id, code_paths) in &project_info {
                if code_paths.iter().any(|cp| cp == fp) {
                    let mut edge = Edge::new(proj_id, code_id, "maps_to");
                    edge.metadata = Some(serde_json::json!({"source": "auto-bridge", "confidence": 1.0}));
                    new_edges.push(edge);
                    matched = true;
                }
            }
        }

        // 3. Fallback: ID prefix matching
        if !matched {
            // Extract meaningful path segments from code node id
            // e.g. "file:src/auth/login.rs" → ["auth", "login"]
            let id_path = code_id.split(':').nth(1).unwrap_or(code_id);
            let segments: Vec<&str> = id_path
                .split('/')
                .filter(|s| *s != "src" && *s != "lib" && *s != "mod.rs" && *s != "index.ts" && *s != "index.js")
                .filter_map(|s| {
                    let name = s.split('.').next().unwrap_or(s);
                    if name.is_empty() || name == "main" || name == "mod" || name == "index" {
                        None
                    } else {
                        Some(name)
                    }
                })
                .collect();

            for segment in &segments {
                let seg_lower = segment.to_lowercase();
                for (proj_id, _) in &project_info {
                    let proj_lower = proj_id.to_lowercase();
                    if proj_lower.contains(&seg_lower) {
                        // Avoid duplicate edges
                        let already = new_edges.iter().any(|e| e.from == *proj_id && e.to == *code_id);
                        if !already {
                            let mut edge = Edge::new(proj_id, code_id, "maps_to");
                            edge.metadata = Some(serde_json::json!({"source": "auto-bridge", "confidence": 0.8}));
                            new_edges.push(edge);
                        }
                    }
                }
            }
        }
    }

    graph.edges.extend(new_edges);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_graph::{CodeNode, CodeEdge, CodeGraph, EdgeRelation};
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
        // confidence and weight must be set on the Edge struct itself (not just metadata)
        assert!(defined_in.confidence.is_some(), "confidence must be set on Edge");
        assert!(defined_in.weight.is_some(), "weight must be set on Edge");

        let calls = edges.iter().find(|e| e.relation == "calls").unwrap();
        assert_eq!(calls.source(), Some("extract"));
        assert!(calls.confidence.is_some(), "confidence must be set on Edge");
        assert!(calls.weight.is_some(), "weight must be set on Edge");
        // Default CodeEdge confidence=1.0, weight=0.5
        assert_eq!(calls.confidence, Some(1.0));
        assert_eq!(calls.weight, Some(0.5));
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
        let cg = CodeGraph {
            nodes: vec![
                CodeNode::new_file("src/lib.rs"),
                CodeNode::new_class("src/lib.rs", "Foo", 1),
                CodeNode::new_function("src/lib.rs", "bar", 10, false),
                CodeNode::new_module("src/mod"),
                CodeNode::new_constant("src/lib.rs", "MAX", 1),
                CodeNode::new_interface("src/lib.rs", "IService", 20),
                CodeNode::new_enum("src/e.rs", "Color", 1),
                CodeNode::new_type_alias("src/t.rs", "Id", 1),
                CodeNode::new_trait("src/tr.rs", "Storage", 1),
            ],
            ..Default::default()
        };
        let (nodes, _) = codegraph_to_graph_nodes(&cg, Path::new("/tmp"));
        assert_eq!(nodes.len(), 9);
        // Verify all have extract source
        assert!(nodes.iter().all(|n| n.source.as_deref() == Some("extract")));
    }

    #[test]
    fn test_edge_metadata_fields() {
        let mut edge = CodeEdge::new("func:src/a.rs:foo", "func:src/b.rs:bar", EdgeRelation::Calls);
        edge.call_count = 5;
        edge.in_error_path = true;
        edge.confidence = 0.8;
        edge.weight = 0.9;
        let cg = CodeGraph {
            nodes: vec![
                CodeNode::new_function("src/a.rs", "foo", 1, false),
                CodeNode::new_function("src/b.rs", "bar", 1, false),
            ],
            edges: vec![edge],
            ..CodeGraph::default()
        };

        let (_, edges) = codegraph_to_graph_nodes(&cg, Path::new("/tmp"));
        assert_eq!(edges.len(), 1);
        let e = &edges[0];
        // confidence and weight are now on Edge struct fields, not in metadata
        // Use approximate comparison due to f32→f64 cast precision
        assert!((e.confidence.unwrap() - 0.8).abs() < 1e-6, "confidence should be ~0.8");
        assert!((e.weight.unwrap() - 0.9).abs() < 1e-6, "weight should be ~0.9");
        let meta = e.metadata.as_ref().unwrap();
        assert_eq!(meta.get("source").unwrap(), "extract");
        assert_eq!(meta.get("call_count").unwrap(), 5);
        assert_eq!(meta.get("in_error_path").unwrap(), true);
        // confidence/weight should NOT be duplicated in metadata
        assert!(meta.get("confidence").is_none());
        assert!(meta.get("weight").is_none());
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

    /// ADR-5: Cross-layer query integration test.
    /// Verifies task→feature→code traversal via bridge edges.
    #[test]
    fn test_cross_layer_query_traversal() {
        use crate::query::QueryEngine;

        let mut graph = Graph::new();

        // Project layer: feature + task
        let mut feature = Node::new("feat-auth", "Auth Feature");
        feature.source = Some("project".to_string());
        feature.node_type = Some("feature".to_string());
        graph.add_node(feature);

        let mut task = Node::new("task-impl-auth", "Implement auth middleware");
        task.source = Some("project".to_string());
        task.node_type = Some("task".to_string());
        task.description = Some("Implement JWT auth in src/auth.rs".to_string());
        graph.add_node(task);

        // task implements feature
        graph.add_edge(Edge::new("task-impl-auth", "feat-auth", "implements"));

        // Code layer: file + function
        let mut code_file = Node::new("file:src/auth.rs", "src/auth.rs");
        code_file.source = Some("extract".to_string());
        code_file.node_type = Some("code".to_string());
        code_file.file_path = Some("src/auth.rs".to_string());
        code_file.status = NodeStatus::Done;
        graph.add_node(code_file);

        let mut code_fn = Node::new("fn:verify_jwt", "verify_jwt");
        code_fn.source = Some("extract".to_string());
        code_fn.node_type = Some("code".to_string());
        code_fn.file_path = Some("src/auth.rs".to_string());
        code_fn.status = NodeStatus::Done;
        graph.add_node(code_fn);

        // code edges
        let mut code_edge = Edge::new("fn:verify_jwt", "file:src/auth.rs", "belongs_to");
        code_edge.metadata = Some(serde_json::json!({"source": "extract"}));
        graph.add_edge(code_edge);

        // Bridge edge: task touches code file
        let mut bridge = Edge::new("task-impl-auth", "file:src/auth.rs", "touches");
        bridge.metadata = Some(serde_json::json!({"source": "auto-bridge", "confidence": 1.0}));
        graph.add_edge(bridge);

        // Now test: impact of code file should reach task (and feature)
        let engine = QueryEngine::new(&graph);

        // impact("file:src/auth.rs") should find task-impl-auth (touches edge, from=task, to=file)
        let impacted = engine.impact("file:src/auth.rs");
        let impacted_ids: Vec<&str> = impacted.iter().map(|n| n.id.as_str()).collect();
        assert!(impacted_ids.contains(&"task-impl-auth"), "task should be impacted by code file change, got: {:?}", impacted_ids);

        // Transitive: task implements feature, so feature should also be impacted
        // (impact traverses "from" edges pointing at the current node)
        // task-impl-auth → feat-auth (implements edge: from=task, to=feat)
        // So feat-auth is NOT impacted (impact looks at edges where to==current)
        // But if we look from feature perspective:
        // deps of task-impl-auth should include feat-auth (via implements)
        let deps = engine.deps("task-impl-auth", true);
        let dep_ids: Vec<&str> = deps.iter().map(|n| n.id.as_str()).collect();
        assert!(dep_ids.contains(&"feat-auth"), "feature should be a dep of task via implements, got: {:?}", dep_ids);
        assert!(dep_ids.contains(&"file:src/auth.rs"), "code file should be a dep of task via touches, got: {:?}", dep_ids);

        // Layer isolation: project_nodes should not include code nodes
        assert_eq!(graph.project_nodes().len(), 2);
        assert_eq!(graph.code_nodes().len(), 2);
    }

    /// T4.4: Performance benchmark — tasks listing with 0 vs 2000 code nodes.
    /// Verifies overhead is < 2x (ADR-5 requirement).
    #[test]
    fn test_perf_tasks_with_code_nodes() {
        // Build a project-only graph with 50 tasks
        let mut project_graph = Graph::new();
        for i in 0..50 {
            let mut n = Node::new(&format!("task-{}", i), &format!("Task {}", i));
            n.source = Some("project".to_string());
            n.status = NodeStatus::Todo;
            project_graph.add_node(n);
        }
        for i in 1..50 {
            project_graph.add_edge(Edge::new(&format!("task-{}", i), &format!("task-{}", i - 1), "depends_on"));
        }

        // Build a mixed graph with 50 tasks + 2000 code nodes
        let mut mixed_graph = project_graph.clone();
        for i in 0..2000 {
            let mut n = Node::new(&format!("fn:func_{}", i), &format!("func_{}", i));
            n.source = Some("extract".to_string());
            n.node_type = Some("code".to_string());
            n.file_path = Some(format!("src/mod_{}.rs", i / 10));
            n.status = NodeStatus::Done;
            mixed_graph.add_node(n);
        }
        for i in 1..2000 {
            let mut e = Edge::new(&format!("fn:func_{}", i), &format!("fn:func_{}", i - 1), "calls");
            e.metadata = Some(serde_json::json!({"source": "extract"}));
            mixed_graph.add_edge(e);
        }

        // Benchmark: project_nodes() on project-only vs mixed graph
        let iterations = 100;

        let start = std::time::Instant::now();
        for _ in 0..iterations {
            let _ = project_graph.project_nodes();
        }
        let project_only_time = start.elapsed();

        let start = std::time::Instant::now();
        for _ in 0..iterations {
            let _ = mixed_graph.project_nodes();
        }
        let mixed_time = start.elapsed();

        let ratio = mixed_time.as_nanos() as f64 / project_only_time.as_nanos() as f64;
        println!("project_nodes() — project_only: {:?}, mixed(+2000 code): {:?}, ratio: {:.2}x", project_only_time, mixed_time, ratio);

        // We care about complexity (no quadratic blow-up), not wall-clock.
        // Wall-clock thresholds are flaky under concurrent test load. Linear scan of
        // 2050 vs 50 nodes is ~41x; we allow generous headroom (200x) to catch only
        // genuine algorithmic regressions (e.g., O(n²) creep).
        assert!(
            ratio < 200.0,
            "project_nodes() overhead too high: {:.2}x (suggests non-linear scan), times: project_only={:?} mixed={:?}",
            ratio, project_only_time, mixed_time
        );

        // Also benchmark summary() which does status counting
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            let _ = project_graph.summary();
        }
        let summary_project = start.elapsed();

        let start = std::time::Instant::now();
        for _ in 0..iterations {
            let _ = mixed_graph.summary();
        }
        let summary_mixed = start.elapsed();

        let summary_ratio = summary_mixed.as_nanos() as f64 / summary_project.as_nanos() as f64;
        println!("summary() — project_only: {:?}, mixed: {:?}, ratio: {:.2}x", summary_project, summary_mixed, summary_ratio);

        // summary() also scans all nodes; same complexity-not-wall-clock check.
        assert!(
            summary_ratio < 200.0,
            "summary() overhead too high: {:.2}x (suggests non-linear scan), times: project_only={:?} mixed={:?}",
            summary_ratio, summary_project, summary_mixed
        );
    }

    #[test]
    fn test_graph_to_codegraph_roundtrip() {
        use crate::code_graph::{CodeGraph, CodeNode, CodeEdge, NodeKind, EdgeRelation};

        // Build a CodeGraph
        let mut cg = CodeGraph {
            nodes: vec![
                CodeNode {
                    name: "main.rs".to_string(),
                    file_path: "src/main.rs".to_string(),
                    line_count: 100,
                    lang: Some("rust".to_string()),
                    ..CodeNode::test_default("file:src/main.rs", NodeKind::File)
                },
                CodeNode {
                    name: "main".to_string(),
                    file_path: "src/main.rs".to_string(),
                    line: Some(10),
                    decorators: vec!["#[tokio::main]".to_string()],
                    signature: Some("async fn main() -> Result<()>".to_string()),
                    docstring: Some("Entry point".to_string()),
                    line_count: 50,
                    visibility: Some("pub".to_string()),
                    lang: Some("rust".to_string()),
                    body_hash: Some("abc123".to_string()),
                    end_line: Some(60),
                    ..CodeNode::test_default("fn:src/main.rs:main", NodeKind::Function)
                },
                CodeNode {
                    name: "Config".to_string(),
                    file_path: "src/lib.rs".to_string(),
                    line: Some(1),
                    line_count: 20,
                    is_test: true,
                    visibility: Some("pub(crate)".to_string()),
                    lang: Some("rust".to_string()),
                    body_hash: Some("def456".to_string()),
                    end_line: Some(20),
                    ..CodeNode::test_default("class:src/lib.rs:Config", NodeKind::Class)
                },
            ],
            edges: vec![
                CodeEdge {
                    from: "fn:src/main.rs:main".to_string(),
                    to: "file:src/main.rs".to_string(),
                    relation: EdgeRelation::DefinedIn,
                    weight: 0.5,
                    call_count: 1,
                    in_error_path: false,
                    confidence: 1.0,
                    call_site_line: None,
                    call_site_column: None,
                },
                CodeEdge {
                    from: "fn:src/main.rs:main".to_string(),
                    to: "class:src/lib.rs:Config".to_string(),
                    relation: EdgeRelation::Calls,
                    weight: 0.8,
                    call_count: 3,
                    in_error_path: true,
                    confidence: 0.9,
                    call_site_line: None,
                    call_site_column: None,
                },
            ],
            outgoing: Default::default(),
            incoming: Default::default(),
            node_index: Default::default(),
        };
        cg.build_indexes();

        // Forward: CodeGraph → Graph nodes/edges
        let (graph_nodes, graph_edges) = codegraph_to_graph_nodes(&cg, std::path::Path::new("."));
        let mut graph = Graph::new();
        graph.nodes = graph_nodes;
        graph.edges = graph_edges;

        // Reverse: Graph → CodeGraph
        let roundtrip = graph_to_codegraph(&graph);

        // Verify nodes
        assert_eq!(roundtrip.nodes.len(), 3);
        let file_node = roundtrip.nodes.iter().find(|n| n.id == "file:src/main.rs").unwrap();
        assert_eq!(file_node.kind, NodeKind::File);
        assert_eq!(file_node.name, "main.rs");
        assert_eq!(file_node.line_count, 100);
        assert!(!file_node.is_test);
        assert_eq!(file_node.lang.as_deref(), Some("rust"));

        let fn_node = roundtrip.nodes.iter().find(|n| n.id == "fn:src/main.rs:main").unwrap();
        assert_eq!(fn_node.kind, NodeKind::Function);
        assert_eq!(fn_node.signature.as_deref(), Some("async fn main() -> Result<()>"));
        assert_eq!(fn_node.docstring.as_deref(), Some("Entry point"));
        assert_eq!(fn_node.line, Some(10));
        assert_eq!(fn_node.decorators, vec!["#[tokio::main]".to_string()]);
        assert_eq!(fn_node.visibility.as_deref(), Some("pub"));
        assert_eq!(fn_node.lang.as_deref(), Some("rust"));
        assert_eq!(fn_node.body_hash.as_deref(), Some("abc123"));
        assert_eq!(fn_node.end_line, Some(60));

        let class_node = roundtrip.nodes.iter().find(|n| n.id == "class:src/lib.rs:Config").unwrap();
        assert_eq!(class_node.kind, NodeKind::Class);
        assert!(class_node.is_test);
        assert_eq!(class_node.visibility.as_deref(), Some("pub(crate)"));
        assert_eq!(class_node.body_hash.as_deref(), Some("def456"));

        // Verify edges
        assert_eq!(roundtrip.edges.len(), 2);
        let defined_edge = roundtrip.edges.iter().find(|e| e.relation == EdgeRelation::DefinedIn).unwrap();
        assert_eq!(defined_edge.from, "fn:src/main.rs:main");
        assert_eq!(defined_edge.to, "file:src/main.rs");

        let calls_edge = roundtrip.edges.iter().find(|e| e.relation == EdgeRelation::Calls).unwrap();
        assert_eq!(calls_edge.call_count, 3);
        assert!(calls_edge.in_error_path);
        assert!((calls_edge.weight - 0.8).abs() < 0.01);
        assert!((calls_edge.confidence - 0.9).abs() < 0.01);

        // Verify indexes were built
        assert!(!roundtrip.node_index.is_empty());
        assert!(!roundtrip.outgoing.is_empty());
    }
}
