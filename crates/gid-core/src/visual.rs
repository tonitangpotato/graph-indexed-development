//! Visual rendering for GID graphs.
//!
//! Supports ASCII, Graphviz DOT, and Mermaid diagram formats.

use crate::graph::{Graph, Node, NodeStatus};
use crate::query::QueryEngine;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Output format for graph visualization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VisualFormat {
    Ascii,
    Dot,
    Mermaid,
}

impl std::str::FromStr for VisualFormat {
    type Err = anyhow::Error;
    
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "ascii" => Ok(Self::Ascii),
            "dot" | "graphviz" => Ok(Self::Dot),
            "mermaid" => Ok(Self::Mermaid),
            _ => Err(anyhow::anyhow!("Unknown format: {}. Valid: ascii, dot, mermaid", s)),
        }
    }
}

/// Status symbols for different output formats.
pub fn status_symbol(status: &NodeStatus) -> &'static str {
    match status {
        NodeStatus::Done => "✅",
        NodeStatus::InProgress => "🔄",
        NodeStatus::Todo => "○",
        NodeStatus::Blocked => "⛔",
        NodeStatus::Cancelled => "⊘",
        NodeStatus::Failed => "❌",
        NodeStatus::NeedsResolution => "⚠️",
    }
}

/// ASCII status symbol (simpler).
pub fn status_symbol_ascii(status: &NodeStatus) -> &'static str {
    match status {
        NodeStatus::Done => "[x]",
        NodeStatus::InProgress => "[~]",
        NodeStatus::Todo => "[ ]",
        NodeStatus::Blocked => "[!]",
        NodeStatus::Cancelled => "[-]",
        NodeStatus::Failed => "[F]",
        NodeStatus::NeedsResolution => "[?]",
    }
}

/// Render graph to the specified format.
pub fn render(graph: &Graph, format: VisualFormat) -> String {
    match format {
        VisualFormat::Ascii => render_ascii(graph),
        VisualFormat::Dot => render_dot(graph),
        VisualFormat::Mermaid => render_mermaid(graph),
    }
}

/// Render graph as ASCII tree/diagram.
pub fn render_ascii(graph: &Graph) -> String {
    if graph.nodes.is_empty() {
        return "Empty graph.".to_string();
    }
    
    let _engine = QueryEngine::new(graph);
    let mut output = Vec::new();
    
    // Get project name
    if let Some(ref project) = graph.project {
        output.push(format!("📊 {}", project.name));
        if let Some(ref desc) = project.description {
            output.push(format!("   {}", desc));
        }
        output.push(String::new());
    }
    
    // Find root nodes (nodes with no incoming depends_on edges)
    let has_incoming: HashSet<&str> = graph.edges.iter()
        .filter(|e| e.relation == "depends_on")
        .map(|e| e.to.as_str())
        .collect();
    
    let roots: Vec<&Node> = graph.nodes.iter()
        .filter(|n| !has_incoming.contains(n.id.as_str()))
        .collect();
    
    // Build adjacency list for depends_on edges (from -> [to])
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &graph.edges {
        if edge.relation == "depends_on" {
            children.entry(edge.from.as_str()).or_default().push(&edge.to);
        }
    }
    
    // Track visited nodes to avoid infinite loops
    let mut visited = HashSet::new();
    
    // Render each root and its subtree
    for (i, root) in roots.iter().enumerate() {
        let is_last = i == roots.len() - 1;
        render_node_ascii(graph, root.id.as_str(), &children, &mut visited, &mut output, "", is_last);
    }
    
    // Show orphan nodes if any
    let all_rendered: HashSet<&str> = visited.iter().map(|s| s.as_str()).collect();
    let orphans: Vec<&Node> = graph.nodes.iter()
        .filter(|n| !all_rendered.contains(n.id.as_str()))
        .collect();
    
    if !orphans.is_empty() {
        output.push(String::new());
        output.push("📦 Disconnected nodes:".to_string());
        for node in orphans {
            output.push(format!("   {} {} — {}", 
                status_symbol(&node.status),
                node.id,
                node.title
            ));
        }
    }
    
    // Summary
    let summary = graph.summary();
    output.push(String::new());
    output.push("─────────────────────────────────".to_string());
    output.push(format!("{}", summary));
    
    output.join("\n")
}

fn render_node_ascii(
    graph: &Graph,
    node_id: &str,
    children: &HashMap<&str, Vec<&str>>,
    visited: &mut HashSet<String>,
    output: &mut Vec<String>,
    prefix: &str,
    is_last: bool,
) {
    if visited.contains(node_id) {
        // Show back-reference
        output.push(format!("{}{}↺ {}", prefix, if is_last { "└── " } else { "├── " }, node_id));
        return;
    }
    visited.insert(node_id.to_string());
    
    let node = match graph.get_node(node_id) {
        Some(n) => n,
        None => return,
    };
    
    let connector = if is_last { "└── " } else { "├── " };
    let status = status_symbol(&node.status);
    
    // Format node line
    let tags = if node.tags.is_empty() {
        String::new()
    } else {
        format!(" [{}]", node.tags.join(", "))
    };
    
    output.push(format!("{}{}{} {} — {}{}", prefix, connector, status, node.id, node.title, tags));
    
    // Render children
    if let Some(deps) = children.get(node_id) {
        let child_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
        for (i, &child_id) in deps.iter().enumerate() {
            let child_is_last = i == deps.len() - 1;
            render_node_ascii(graph, child_id, children, visited, output, &child_prefix, child_is_last);
        }
    }
}

/// Render graph as Graphviz DOT format.
pub fn render_dot(graph: &Graph) -> String {
    let mut output = Vec::new();
    
    // Header
    let name = graph.project.as_ref()
        .map(|p| p.name.as_str())
        .unwrap_or("graph");
    output.push(format!("digraph \"{}\" {{", escape_dot(name)));
    output.push("    rankdir=TB;".to_string());
    output.push("    node [shape=box, style=rounded];".to_string());
    output.push(String::new());
    
    // Node styles by status
    output.push("    // Status colors".to_string());
    output.push("    node [fillstyle=solid];".to_string());
    
    // Nodes
    for node in &graph.nodes {
        let color = match node.status {
            NodeStatus::Done => "palegreen",
            NodeStatus::InProgress => "lightyellow",
            NodeStatus::Todo => "white",
            NodeStatus::Blocked => "lightcoral",
            NodeStatus::Cancelled => "lightgray",
            NodeStatus::Failed => "salmon",
            NodeStatus::NeedsResolution => "lightyellow",
        };
        
        let label = format!("{} {}\\n{}", 
            status_symbol_ascii(&node.status),
            escape_dot(&node.id),
            escape_dot(&node.title)
        );
        
        output.push(format!(
            "    \"{}\" [label=\"{}\", fillcolor=\"{}\", style=filled];",
            escape_dot(&node.id),
            label,
            color
        ));
    }
    
    output.push(String::new());
    
    // Edges
    output.push("    // Edges".to_string());
    for edge in &graph.edges {
        let style = match edge.relation.as_str() {
            "depends_on" => "solid",
            "implements" => "dashed",
            "contains" => "dotted",
            _ => "solid",
        };
        
        output.push(format!(
            "    \"{}\" -> \"{}\" [label=\"{}\", style={}];",
            escape_dot(&edge.from),
            escape_dot(&edge.to),
            escape_dot(&edge.relation),
            style
        ));
    }
    
    output.push("}".to_string());
    output.join("\n")
}

fn escape_dot(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Render graph as Mermaid diagram syntax.
pub fn render_mermaid(graph: &Graph) -> String {
    let mut output = Vec::new();
    
    // Header
    output.push("graph TD".to_string());
    
    // Subgraphs by status
    let mut by_status: HashMap<&NodeStatus, Vec<&Node>> = HashMap::new();
    for node in &graph.nodes {
        by_status.entry(&node.status).or_default().push(node);
    }
    
    // Nodes with status styling
    for node in &graph.nodes {
        let shape = match node.status {
            NodeStatus::Done => format!("{}[✅ {}]", escape_mermaid_id(&node.id), escape_mermaid(&node.title)),
            NodeStatus::InProgress => format!("{}[🔄 {}]", escape_mermaid_id(&node.id), escape_mermaid(&node.title)),
            NodeStatus::Todo => format!("{}[○ {}]", escape_mermaid_id(&node.id), escape_mermaid(&node.title)),
            NodeStatus::Blocked => format!("{}[⛔ {}]", escape_mermaid_id(&node.id), escape_mermaid(&node.title)),
            NodeStatus::Cancelled => format!("{}[⊘ {}]", escape_mermaid_id(&node.id), escape_mermaid(&node.title)),
            NodeStatus::Failed => format!("{}[❌ {}]", escape_mermaid_id(&node.id), escape_mermaid(&node.title)),
            NodeStatus::NeedsResolution => format!("{}[⚠️ {}]", escape_mermaid_id(&node.id), escape_mermaid(&node.title)),
        };
        output.push(format!("    {}", shape));
    }
    
    output.push(String::new());
    
    // Edges
    for edge in &graph.edges {
        let arrow = match edge.relation.as_str() {
            "depends_on" => "-->",
            "implements" => "-.->",
            "contains" => "---",
            _ => "-->",
        };
        
        let label = if edge.relation != "depends_on" {
            format!("|{}|", escape_mermaid(&edge.relation))
        } else {
            String::new()
        };
        
        output.push(format!(
            "    {}{} {}{}",
            escape_mermaid_id(&edge.from),
            arrow,
            label,
            escape_mermaid_id(&edge.to)
        ));
    }
    
    // Add styling classes
    output.push(String::new());
    output.push("    %% Styling".to_string());
    
    for node in &graph.nodes {
        let class = match node.status {
            NodeStatus::Done => "done",
            NodeStatus::InProgress => "inprogress",
            NodeStatus::Todo => "todo",
            NodeStatus::Blocked => "blocked",
            NodeStatus::Cancelled => "cancelled",
            NodeStatus::Failed => "failed",
            NodeStatus::NeedsResolution => "needsresolution",
        };
        output.push(format!("    class {} {}", escape_mermaid_id(&node.id), class));
    }
    
    output.push(String::new());
    output.push("    classDef done fill:#90EE90,stroke:#228B22".to_string());
    output.push("    classDef inprogress fill:#FFFFE0,stroke:#DAA520".to_string());
    output.push("    classDef todo fill:#FFFFFF,stroke:#808080".to_string());
    output.push("    classDef blocked fill:#FFC0CB,stroke:#DC143C".to_string());
    output.push("    classDef cancelled fill:#D3D3D3,stroke:#696969".to_string());
    
    output.join("\n")
}

fn escape_mermaid(s: &str) -> String {
    s.replace('"', "'")
        .replace('[', "(")
        .replace(']', ")")
        .replace('{', "(")
        .replace('}', ")")
}

fn escape_mermaid_id(s: &str) -> String {
    // Mermaid IDs should be alphanumeric with underscores
    s.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Node, Edge};
    
    #[test]
    fn test_render_ascii_empty() {
        let graph = Graph::new();
        let result = render_ascii(&graph);
        assert_eq!(result, "Empty graph.");
    }
    
    #[test]
    fn test_render_ascii_simple() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("a", "Task A"));
        graph.add_node(Node::new("b", "Task B").with_status(NodeStatus::Done));
        graph.add_edge(Edge::depends_on("a", "b"));
        
        let result = render_ascii(&graph);
        assert!(result.contains("Task A"));
        assert!(result.contains("Task B"));
        assert!(result.contains("✅"));
    }
    
    #[test]
    fn test_render_dot() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("a", "Task A"));
        graph.add_node(Node::new("b", "Task B"));
        graph.add_edge(Edge::depends_on("a", "b"));
        
        let result = render_dot(&graph);
        assert!(result.starts_with("digraph"));
        assert!(result.contains("\"a\""));
        assert!(result.contains("\"b\""));
        assert!(result.contains("->"));
    }
    
    #[test]
    fn test_render_mermaid() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("a", "Task A"));
        graph.add_node(Node::new("b", "Task B"));
        graph.add_edge(Edge::depends_on("a", "b"));
        
        let result = render_mermaid(&graph);
        assert!(result.starts_with("graph TD"));
        assert!(result.contains("-->"));
    }
}
