//! Graph refactoring operations.
//!
//! Preview and apply structural changes: rename, merge, split, extract.

use serde::{Deserialize, Serialize};
use crate::graph::{Graph, Node, Edge};

/// A preview of changes that a refactoring operation would make.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefactorPreview {
    /// Operation type
    pub operation: String,
    /// Changes to be made
    pub changes: Vec<Change>,
    /// Node IDs affected
    pub affected_nodes: Vec<String>,
    /// Edge count affected
    pub affected_edges: usize,
}

impl std::fmt::Display for RefactorPreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "📋 {} Preview", self.operation)?;
        writeln!(f, "═══════════════════════════════════════════════════")?;
        
        for change in &self.changes {
            writeln!(f, "{}", change)?;
        }
        
        writeln!(f)?;
        writeln!(f, "Affected: {} nodes, {} edges", 
            self.affected_nodes.len(), 
            self.affected_edges
        )?;
        
        Ok(())
    }
}

/// A single change in a refactoring operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    /// Type of change
    pub change_type: ChangeType,
    /// Description of the change
    pub description: String,
    /// Before value (if applicable)
    pub before: Option<String>,
    /// After value (if applicable)
    pub after: Option<String>,
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let icon = match self.change_type {
            ChangeType::RenameNode | ChangeType::UpdateTitle => "✏️ ",
            ChangeType::DeleteNode => "🗑️ ",
            ChangeType::CreateNode => "➕",
            ChangeType::UpdateEdge => "🔗",
            ChangeType::DeleteEdge => "✂️ ",
            ChangeType::CreateEdge => "🔗",
            ChangeType::MergeNode => "🔀",
            ChangeType::SplitNode => "✂️ ",
        };
        
        write!(f, "{} {}", icon, self.description)?;
        
        if let (Some(before), Some(after)) = (&self.before, &self.after) {
            write!(f, "\n      {} → {}", before, after)?;
        }
        
        Ok(())
    }
}

/// Type of change in a refactoring operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    RenameNode,
    UpdateTitle,
    DeleteNode,
    CreateNode,
    UpdateEdge,
    DeleteEdge,
    CreateEdge,
    MergeNode,
    SplitNode,
}

/// Definition for how to split a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitDefinition {
    /// New node ID
    pub id: String,
    /// New node title
    pub title: String,
    /// Optional description
    pub description: Option<String>,
    /// Tags to inherit or add
    pub tags: Vec<String>,
}

// ═══════════════════════════════════════════════════════════════════════════════
// Rename Operations
// ═══════════════════════════════════════════════════════════════════════════════

/// Preview what would change if a node is renamed.
pub fn preview_rename(graph: &Graph, old_id: &str, new_id: &str) -> Option<RefactorPreview> {
    // Check node exists
    graph.get_node(old_id)?;
    
    let mut changes = Vec::new();
    let mut affected_edges = 0;
    
    // Node rename
    changes.push(Change {
        change_type: ChangeType::RenameNode,
        description: "Rename node ID".to_string(),
        before: Some(old_id.to_string()),
        after: Some(new_id.to_string()),
    });
    
    // Find affected edges
    for edge in &graph.edges {
        if edge.from == old_id {
            changes.push(Change {
                change_type: ChangeType::UpdateEdge,
                description: "Update edge source".to_string(),
                before: Some(format!("{} → {}", edge.from, edge.to)),
                after: Some(format!("{} → {}", new_id, edge.to)),
            });
            affected_edges += 1;
        }
        if edge.to == old_id {
            changes.push(Change {
                change_type: ChangeType::UpdateEdge,
                description: "Update edge target".to_string(),
                before: Some(format!("{} → {}", edge.from, edge.to)),
                after: Some(format!("{} → {}", edge.from, new_id)),
            });
            affected_edges += 1;
        }
    }
    
    Some(RefactorPreview {
        operation: "Rename".to_string(),
        changes,
        affected_nodes: vec![old_id.to_string()],
        affected_edges,
    })
}

/// Apply a node rename operation.
pub fn apply_rename(graph: &mut Graph, old_id: &str, new_id: &str) -> bool {
    // Check source exists and target doesn't
    if graph.get_node(old_id).is_none() || graph.get_node(new_id).is_some() {
        return false;
    }
    
    // Rename node
    if let Some(node) = graph.get_node_mut(old_id) {
        node.id = new_id.to_string();
    }
    
    // Update all edges
    for edge in &mut graph.edges {
        if edge.from == old_id {
            edge.from = new_id.to_string();
        }
        if edge.to == old_id {
            edge.to = new_id.to_string();
        }
    }
    
    true
}

// ═══════════════════════════════════════════════════════════════════════════════
// Merge Operations
// ═══════════════════════════════════════════════════════════════════════════════

/// Preview what would change if two nodes are merged.
pub fn preview_merge(
    graph: &Graph, 
    node_a: &str, 
    node_b: &str, 
    new_id: &str
) -> Option<RefactorPreview> {
    let a = graph.get_node(node_a)?;
    let b = graph.get_node(node_b)?;
    
    let mut changes = Vec::new();
    let mut affected_edges = 0;
    
    // Merge description
    let merged_title = format!("{} + {}", a.title, b.title);
    changes.push(Change {
        change_type: ChangeType::MergeNode,
        description: format!("Create merged node '{}'", new_id),
        before: Some(format!("'{}', '{}'", node_a, node_b)),
        after: Some(merged_title.clone()),
    });
    
    // Delete original nodes
    changes.push(Change {
        change_type: ChangeType::DeleteNode,
        description: format!("Remove node '{}'", node_a),
        before: Some(node_a.to_string()),
        after: None,
    });
    changes.push(Change {
        change_type: ChangeType::DeleteNode,
        description: format!("Remove node '{}'", node_b),
        before: Some(node_b.to_string()),
        after: None,
    });
    
    // Count affected edges (edges to/from either node)
    for edge in &graph.edges {
        if edge.from == node_a || edge.from == node_b 
            || edge.to == node_a || edge.to == node_b 
        {
            affected_edges += 1;
        }
    }
    
    changes.push(Change {
        change_type: ChangeType::UpdateEdge,
        description: format!("Redirect {} edges to new merged node", affected_edges),
        before: None,
        after: None,
    });
    
    Some(RefactorPreview {
        operation: "Merge".to_string(),
        changes,
        affected_nodes: vec![node_a.to_string(), node_b.to_string()],
        affected_edges,
    })
}

/// Apply a merge operation.
pub fn apply_merge(
    graph: &mut Graph, 
    node_a: &str, 
    node_b: &str, 
    new_id: &str
) -> bool {
    let a = match graph.get_node(node_a) {
        Some(n) => n.clone(),
        None => return false,
    };
    let b = match graph.get_node(node_b) {
        Some(n) => n.clone(),
        None => return false,
    };
    
    // Create merged node
    let mut merged = Node::new(new_id, &format!("{} + {}", a.title, b.title));
    
    // Merge descriptions
    merged.description = match (a.description, b.description) {
        (Some(da), Some(db)) => Some(format!("{}\n\n{}", da, db)),
        (Some(d), None) | (None, Some(d)) => Some(d),
        (None, None) => None,
    };
    
    // Merge tags (dedupe)
    let mut tags: Vec<String> = a.tags.into_iter().chain(b.tags).collect();
    tags.sort();
    tags.dedup();
    merged.tags = tags;
    
    // Use more "done" status
    merged.status = if a.status == crate::graph::NodeStatus::Done 
        || b.status == crate::graph::NodeStatus::Done 
    {
        crate::graph::NodeStatus::Done
    } else if a.status == crate::graph::NodeStatus::InProgress 
        || b.status == crate::graph::NodeStatus::InProgress 
    {
        crate::graph::NodeStatus::InProgress
    } else {
        a.status
    };
    
    // Add merged node
    graph.add_node(merged);
    
    // Update edges to point to new node
    for edge in &mut graph.edges {
        if edge.from == node_a || edge.from == node_b {
            edge.from = new_id.to_string();
        }
        if edge.to == node_a || edge.to == node_b {
            edge.to = new_id.to_string();
        }
    }
    
    // Remove duplicate edges (same from, to, relation)
    let mut seen = std::collections::HashSet::new();
    graph.edges.retain(|e| {
        seen.insert((e.from.clone(), e.to.clone(), e.relation.clone()))
    });
    
    // Remove self-referential edges
    graph.edges.retain(|e| e.from != e.to);
    
    // Remove original nodes
    graph.remove_node(node_a);
    graph.remove_node(node_b);
    
    true
}

// ═══════════════════════════════════════════════════════════════════════════════
// Split Operations
// ═══════════════════════════════════════════════════════════════════════════════

/// Preview what would change if a node is split.
pub fn preview_split(
    graph: &Graph,
    node_id: &str,
    splits: &[SplitDefinition],
) -> Option<RefactorPreview> {
    let _node = graph.get_node(node_id)?;
    
    let mut changes = Vec::new();
    
    // Delete original node
    changes.push(Change {
        change_type: ChangeType::SplitNode,
        description: format!("Split node '{}' into {} parts", node_id, splits.len()),
        before: Some(format!("'{}'", node_id)),
        after: Some(splits.iter().map(|s| s.id.as_str()).collect::<Vec<_>>().join(", ")),
    });
    
    // Create new nodes
    for split in splits {
        changes.push(Change {
            change_type: ChangeType::CreateNode,
            description: format!("Create node '{}'", split.id),
            before: None,
            after: Some(split.title.clone()),
        });
    }
    
    // Count affected edges
    let affected_edges = graph.edges.iter()
        .filter(|e| e.from == node_id || e.to == node_id)
        .count();
    
    if affected_edges > 0 {
        changes.push(Change {
            change_type: ChangeType::UpdateEdge,
            description: format!("Note: {} edges need manual reassignment", affected_edges),
            before: None,
            after: None,
        });
    }
    
    Some(RefactorPreview {
        operation: "Split".to_string(),
        changes,
        affected_nodes: std::iter::once(node_id.to_string())
            .chain(splits.iter().map(|s| s.id.clone()))
            .collect(),
        affected_edges,
    })
}

/// Apply a split operation.
/// Returns the IDs of created nodes.
pub fn apply_split(
    graph: &mut Graph,
    node_id: &str,
    splits: &[SplitDefinition],
) -> Vec<String> {
    let original = match graph.get_node(node_id) {
        Some(n) => n.clone(),
        None => return Vec::new(),
    };
    
    let mut created = Vec::new();
    
    // Create new nodes
    for (i, split) in splits.iter().enumerate() {
        let mut new_node = Node::new(&split.id, &split.title);
        new_node.description = split.description.clone()
            .or_else(|| original.description.clone());
        new_node.status = original.status.clone();
        new_node.node_type = original.node_type.clone();
        
        // Merge tags
        let mut tags = original.tags.clone();
        tags.extend(split.tags.clone());
        tags.sort();
        tags.dedup();
        new_node.tags = tags;
        
        graph.add_node(new_node);
        created.push(split.id.clone());
        
        // First split inherits incoming edges, all inherit outgoing edges
        // This is a heuristic; user may need to adjust
        if i == 0 {
            // Redirect incoming edges to first split
            for edge in &mut graph.edges {
                if edge.to == node_id {
                    edge.to = split.id.clone();
                }
            }
        }
    }
    
    // Redirect outgoing edges to first split (simple heuristic)
    if let Some(first) = splits.first() {
        for edge in &mut graph.edges {
            if edge.from == node_id {
                edge.from = first.id.clone();
            }
        }
    }
    
    // Remove original node
    graph.remove_node(node_id);
    
    created
}

// ═══════════════════════════════════════════════════════════════════════════════
// Extract Operations
// ═══════════════════════════════════════════════════════════════════════════════

/// Preview extracting nodes into a new parent/group.
pub fn preview_extract(
    graph: &Graph,
    node_ids: &[String],
    new_parent_id: &str,
    new_parent_title: &str,
) -> Option<RefactorPreview> {
    // Verify all nodes exist
    for id in node_ids {
        graph.get_node(id)?;
    }
    
    let mut changes = Vec::new();
    
    // Create parent node
    changes.push(Change {
        change_type: ChangeType::CreateNode,
        description: format!("Create parent node '{}'", new_parent_id),
        before: None,
        after: Some(new_parent_title.to_string()),
    });
    
    // Add contains edges
    for id in node_ids {
        changes.push(Change {
            change_type: ChangeType::CreateEdge,
            description: format!("Add contains edge to '{}'", id),
            before: None,
            after: Some(format!("{} → {}", new_parent_id, id)),
        });
    }
    
    Some(RefactorPreview {
        operation: "Extract".to_string(),
        changes,
        affected_nodes: std::iter::once(new_parent_id.to_string())
            .chain(node_ids.iter().cloned())
            .collect(),
        affected_edges: node_ids.len(),
    })
}

/// Apply an extract operation.
pub fn apply_extract(
    graph: &mut Graph,
    node_ids: &[String],
    new_parent_id: &str,
    new_parent_title: &str,
) -> bool {
    // Verify all nodes exist
    for id in node_ids {
        if graph.get_node(id).is_none() {
            return false;
        }
    }
    
    // Create parent node
    let mut parent = Node::new(new_parent_id, new_parent_title);
    parent.node_type = Some("module".to_string());
    graph.add_node(parent);
    
    // Add contains edges
    for id in node_ids {
        graph.add_edge(Edge::new(new_parent_id, id, "contains"));
    }
    
    true
}

// ═══════════════════════════════════════════════════════════════════════════════
// Utility Operations
// ═══════════════════════════════════════════════════════════════════════════════

/// Update a node's title.
pub fn update_title(graph: &mut Graph, node_id: &str, new_title: &str) -> bool {
    if let Some(node) = graph.get_node_mut(node_id) {
        node.title = new_title.to_string();
        true
    } else {
        false
    }
}

/// Move a node to a different layer.
pub fn move_to_layer(graph: &mut Graph, node_id: &str, layer: &str) -> bool {
    if let Some(node) = graph.get_node_mut(node_id) {
        node.metadata.insert("layer".to_string(), serde_json::json!(layer));
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_preview_rename() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("old", "Old Node"));
        graph.add_node(Node::new("other", "Other"));
        graph.add_edge(Edge::depends_on("other", "old"));
        
        let preview = preview_rename(&graph, "old", "new").unwrap();
        assert_eq!(preview.operation, "Rename");
        assert_eq!(preview.affected_edges, 1);
    }
    
    #[test]
    fn test_apply_rename() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("old", "Old Node"));
        graph.add_node(Node::new("other", "Other"));
        graph.add_edge(Edge::depends_on("other", "old"));
        
        assert!(apply_rename(&mut graph, "old", "new"));
        assert!(graph.get_node("old").is_none());
        assert!(graph.get_node("new").is_some());
        assert_eq!(graph.edges[0].to, "new");
    }
    
    #[test]
    fn test_apply_merge() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("a", "Node A").with_tags(vec!["tag1".to_string()]));
        graph.add_node(Node::new("b", "Node B").with_tags(vec!["tag2".to_string()]));
        graph.add_node(Node::new("c", "Node C"));
        graph.add_edge(Edge::depends_on("c", "a"));
        
        assert!(apply_merge(&mut graph, "a", "b", "merged"));
        
        assert!(graph.get_node("a").is_none());
        assert!(graph.get_node("b").is_none());
        
        let merged = graph.get_node("merged").unwrap();
        assert_eq!(merged.tags.len(), 2);
        
        // Edge should point to merged node
        assert_eq!(graph.edges[0].to, "merged");
    }
    
    #[test]
    fn test_apply_split() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("original", "Original Node")
            .with_description("Description")
            .with_tags(vec!["tag1".to_string()]));
        graph.add_node(Node::new("dep", "Dependency"));
        graph.add_edge(Edge::depends_on("original", "dep"));
        
        let splits = vec![
            SplitDefinition {
                id: "part1".to_string(),
                title: "Part 1".to_string(),
                description: None,
                tags: vec![],
            },
            SplitDefinition {
                id: "part2".to_string(),
                title: "Part 2".to_string(),
                description: Some("Custom desc".to_string()),
                tags: vec!["new_tag".to_string()],
            },
        ];
        
        let created = apply_split(&mut graph, "original", &splits);
        assert_eq!(created.len(), 2);
        assert!(graph.get_node("original").is_none());
        assert!(graph.get_node("part1").is_some());
        assert!(graph.get_node("part2").is_some());
    }
    
    #[test]
    fn test_apply_extract() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("a", "A"));
        graph.add_node(Node::new("b", "B"));
        graph.add_node(Node::new("c", "C"));
        
        assert!(apply_extract(
            &mut graph,
            &["a".to_string(), "b".to_string()],
            "module_ab",
            "Module AB"
        ));
        
        assert!(graph.get_node("module_ab").is_some());
        
        // Check contains edges
        let contains_edges: Vec<_> = graph.edges.iter()
            .filter(|e| e.relation == "contains" && e.from == "module_ab")
            .collect();
        assert_eq!(contains_edges.len(), 2);
    }
}
