//! Unified Graph — merge CodeGraph with TaskGraph
//!
//! Combines code structure nodes (files, classes, functions) with
//! task nodes (todo, in_progress, done) into a single graph for
//! holistic project analysis.

use crate::graph::{Graph, Node, Edge, NodeStatus};
use crate::code_graph::{CodeGraph, NodeKind, EdgeRelation};
use std::collections::HashMap;

/// Build a unified graph combining code structure and task nodes.
/// 
/// Code nodes become task nodes with status "active" and type based on their kind.
/// Code edges become task edges with appropriate relation strings.
pub fn build_unified_graph(code_graph: &CodeGraph, task_graph: &Graph) -> Graph {
    let mut nodes = Vec::new();
    let mut seen_node_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_edges: std::collections::HashSet<(String, String, String)> = std::collections::HashSet::new();
    let mut edges = Vec::new();
    
    // Track existing task node IDs to avoid duplicates
    for task_node in &task_graph.nodes {
        seen_node_ids.insert(task_node.id.clone());
    }
    
    // Convert code nodes to task nodes (with dedup)
    for code_node in &code_graph.nodes {
        let id = code_node_to_task_id(&code_node.id);
        
        // Skip duplicates (same code node extracted twice, e.g. trait def + impl)
        if !seen_node_ids.insert(id.clone()) {
            continue;
        }
        
        let node_type = match code_node.kind {
            NodeKind::File => "file",
            NodeKind::Class => "class",
            NodeKind::Function => "function",
            NodeKind::Module => "module",
        };
        
        let mut metadata = HashMap::new();
        metadata.insert("original_id".to_string(), serde_json::json!(code_node.id));
        metadata.insert("file_path".to_string(), serde_json::json!(code_node.file_path));
        if let Some(line) = code_node.line {
            metadata.insert("line".to_string(), serde_json::json!(line));
        }
        if let Some(ref sig) = code_node.signature {
            metadata.insert("signature".to_string(), serde_json::json!(sig));
        }
        
        nodes.push(Node {
            id,
            title: code_node.name.clone(),
            status: NodeStatus::Done,
            description: code_node.docstring.clone(),
            assigned_to: None,
            tags: if code_node.is_test { vec!["test".to_string()] } else { vec![] },
            priority: None,
            node_type: Some(node_type.to_string()),
            knowledge: Default::default(),
            metadata,
        });
    }
    
    // Add all task nodes
    for task_node in &task_graph.nodes {
        nodes.push(task_node.clone());
    }
    
    // Convert code edges (with dedup, skip edges referencing non-existent nodes)
    for code_edge in &code_graph.edges {
        let from = code_node_to_task_id(&code_edge.from);
        let to = code_node_to_task_id(&code_edge.to);
        
        // Skip edges that reference nodes not in the graph
        if !seen_node_ids.contains(&from) || !seen_node_ids.contains(&to) {
            continue;
        }
        
        let relation = match code_edge.relation {
            EdgeRelation::Imports => "imports",
            EdgeRelation::Inherits => "inherits",
            EdgeRelation::DefinedIn => "defined_in",
            EdgeRelation::Calls => "calls",
            EdgeRelation::TestsFor => "tests",
            EdgeRelation::Overrides => "overrides",
            EdgeRelation::Implements => "implements",
        };
        
        let edge_key = (from.clone(), to.clone(), relation.to_string());
        if !seen_edges.insert(edge_key) {
            continue; // Skip duplicate edge
        }
        
        edges.push(Edge {
            from,
            to,
            relation: relation.to_string(),
            weight: Some(code_edge.weight as f64),
            confidence: if code_edge.confidence > 0.0 {
                Some(code_edge.confidence as f64)
            } else {
                None
            },
        });
    }
    
    // Add task edges (with dedup)
    for task_edge in &task_graph.edges {
        let edge_key = (task_edge.from.clone(), task_edge.to.clone(), task_edge.relation.clone());
        if !seen_edges.insert(edge_key) {
            continue;
        }
        edges.push(task_edge.clone());
    }
    
    Graph {
        project: task_graph.project.clone(),
        nodes,
        edges,
    }
}

/// Merge code nodes relevant to a set of keywords into an existing task graph.
/// 
/// This is useful for adding code context to a task graph without including
/// the entire codebase.
pub fn merge_relevant_code(
    code_graph: &CodeGraph,
    task_graph: &mut Graph,
    keywords: &[&str],
    max_nodes: usize,
) {
    let relevant = code_graph.find_relevant_nodes(keywords);
    
    let task_ids: std::collections::HashSet<String> = task_graph.nodes.iter()
        .map(|n| n.id.clone())
        .collect();
    
    let mut added = 0;
    for code_node in relevant {
        if added >= max_nodes {
            break;
        }
        
        let id = code_node_to_task_id(&code_node.id);
        if task_ids.contains(&id) {
            continue;
        }
        
        let node_type = match code_node.kind {
            NodeKind::File => "file",
            NodeKind::Class => "class",
            NodeKind::Function => "function",
            NodeKind::Module => "module",
        };
        
        let mut metadata = HashMap::new();
        metadata.insert("file_path".to_string(), serde_json::json!(code_node.file_path));
        if let Some(line) = code_node.line {
            metadata.insert("line".to_string(), serde_json::json!(line));
        }
        
        task_graph.add_node(Node {
            id: id.clone(),
            title: code_node.name.clone(),
            status: NodeStatus::Done,
            description: code_node.docstring.clone(),
            assigned_to: None,
            tags: vec!["code".to_string()],
            priority: None,
            node_type: Some(node_type.to_string()),
            knowledge: Default::default(),
            metadata,
        });
        
        added += 1;
    }
}

/// Link tasks to related code nodes based on file path mentions.
/// 
/// Scans task descriptions and titles for file paths, then creates
/// "relates_to" edges to the corresponding code nodes.
pub fn link_tasks_to_code(code_graph: &CodeGraph, task_graph: &mut Graph) {
    let code_files: std::collections::HashSet<String> = code_graph.nodes.iter()
        .filter(|n| n.kind == NodeKind::File)
        .map(|n| n.file_path.clone())
        .collect();
    
    // Collect edges to add (can't mutate while iterating)
    let mut edges_to_add = Vec::new();
    
    for task in &task_graph.nodes {
        let text = format!("{} {}", task.title, task.description.as_deref().unwrap_or(""));
        
        for file_path in &code_files {
            // Check if file name or path is mentioned
            let file_name = file_path.rsplit('/').next().unwrap_or(file_path);
            if text.contains(file_name) || text.contains(file_path) {
                let code_id = code_node_to_task_id(&format!("file:{}", file_path));
                
                // Check if edge already exists
                let exists = task_graph.edges.iter().any(|e| {
                    e.from == task.id && e.to == code_id
                });
                
                if !exists {
                    edges_to_add.push(Edge {
                        from: task.id.clone(),
                        to: code_id,
                        relation: "relates_to".to_string(),
                        weight: None,
                        confidence: None,
                    });
                }
            }
        }
    }
    
    // Add all edges after iteration
    for edge in edges_to_add {
        task_graph.add_edge(edge);
    }
}

/// Convert code node ID to task-compatible ID.
/// Strips prefixes and makes it safe for task graph.
fn code_node_to_task_id(code_id: &str) -> String {
    // Remove common prefixes and clean up
    code_id
        .replace("file:", "code:")
        .replace("class:", "code:")
        .replace("func:", "code:")
        .replace("method:", "code:")
        .replace("module_ref:", "code:")
        .replace('/', "_")
        .replace(':', "_")
}

/// Statistics about the unified graph.
#[derive(Debug, Default)]
pub struct UnifiedStats {
    pub total_nodes: usize,
    pub code_nodes: usize,
    pub task_nodes: usize,
    pub total_edges: usize,
    pub code_edges: usize,
    pub task_edges: usize,
    pub cross_edges: usize, // edges between code and task nodes
}

impl UnifiedStats {
    pub fn from_graph(graph: &Graph) -> Self {
        let code_node_ids: std::collections::HashSet<&str> = graph.nodes.iter()
            .filter(|n| n.id.starts_with("code:") || 
                        n.node_type.as_deref() == Some("file") ||
                        n.node_type.as_deref() == Some("class") ||
                        n.node_type.as_deref() == Some("function"))
            .map(|n| n.id.as_str())
            .collect();
        
        let code_nodes = code_node_ids.len();
        let task_nodes = graph.nodes.len() - code_nodes;
        
        let mut code_edges = 0;
        let mut task_edges = 0;
        let mut cross_edges = 0;
        
        for edge in &graph.edges {
            let from_is_code = code_node_ids.contains(edge.from.as_str());
            let to_is_code = code_node_ids.contains(edge.to.as_str());
            
            match (from_is_code, to_is_code) {
                (true, true) => code_edges += 1,
                (false, false) => task_edges += 1,
                _ => cross_edges += 1,
            }
        }
        
        Self {
            total_nodes: graph.nodes.len(),
            code_nodes,
            task_nodes,
            total_edges: graph.edges.len(),
            code_edges,
            task_edges,
            cross_edges,
        }
    }
}

impl std::fmt::Display for UnifiedStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Unified graph: {} nodes ({} code, {} tasks), {} edges ({} code, {} task, {} cross)",
            self.total_nodes, self.code_nodes, self.task_nodes,
            self.total_edges, self.code_edges, self.task_edges, self.cross_edges
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::ProjectMeta;
    use crate::code_graph::CodeNode;
    
    #[test]
    fn test_build_unified_graph() {
        let mut code_graph = CodeGraph::default();
        code_graph.nodes.push(CodeNode {
            id: "file:src/main.rs".into(),
            kind: NodeKind::File,
            name: "main.rs".into(),
            file_path: "src/main.rs".into(),
            line: None,
            decorators: vec![],
            signature: None,
            docstring: None,
            line_count: 0,
            is_test: false,
        });
        code_graph.nodes.push(CodeNode {
            id: "func:src/main.rs:main".into(),
            kind: NodeKind::Function,
            name: "main".into(),
            file_path: "src/main.rs".into(),
            line: Some(1),
            decorators: vec![],
            signature: Some("fn main()".into()),
            docstring: None,
            line_count: 10,
            is_test: false,
        });
        
        let task_graph = Graph {
            project: Some(ProjectMeta {
                name: "test".into(),
                description: Some("Test project".into()),
            }),
            nodes: vec![
                Node::new("task1", "Implement feature"),
            ],
            edges: vec![],
        };
        
        let unified = build_unified_graph(&code_graph, &task_graph);
        
        assert_eq!(unified.nodes.len(), 3); // 2 code + 1 task
        assert!(unified.nodes.iter().any(|n| n.title == "main"));
        assert!(unified.nodes.iter().any(|n| n.title == "Implement feature"));
    }
    
    #[test]
    fn test_code_node_to_task_id() {
        assert_eq!(code_node_to_task_id("file:src/main.rs"), "code_src_main.rs");
        assert_eq!(code_node_to_task_id("class:src/lib.rs:MyClass"), "code_src_lib.rs_MyClass");
        assert_eq!(code_node_to_task_id("func:test.py:my_func"), "code_test.py_my_func");
    }
}
