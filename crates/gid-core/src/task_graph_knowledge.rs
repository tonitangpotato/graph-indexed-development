//! Knowledge management extension for task graphs
//!
//! Provides per-node knowledge storage, file caching, and tool call tracking
//! for building up context during graph-driven exploration.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use chrono::Utc;
use anyhow::Result;

/// Record of a tool call made during exploration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub tool_name: String,
    pub timestamp: String,
    pub summary: String,
}

/// Task node with knowledge storage capabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnowledgeNode {
    /// Findings attached to this node (key-value pairs)
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub findings: HashMap<String, String>,
    /// Cached file contents
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub file_cache: HashMap<String, String>,
    /// History of tool calls made for this node
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_history: Vec<ToolCallRecord>,
}

impl KnowledgeNode {
    /// Returns true if this knowledge node has no data stored.
    pub fn is_empty(&self) -> bool {
        self.findings.is_empty() && self.file_cache.is_empty() && self.tool_history.is_empty()
    }
}

/// A graph that supports knowledge management on nodes
pub trait KnowledgeGraph {
    /// Get mutable access to a node's knowledge storage
    fn get_knowledge_mut(&mut self, node_id: &str) -> Option<&mut KnowledgeNode>;
    
    /// Get read access to a node's knowledge storage
    fn get_knowledge(&self, node_id: &str) -> Option<&KnowledgeNode>;
    
    /// Get edges pointing to a node (for upstream lookups)
    fn get_incoming_edges(&self, node_id: &str) -> Vec<String>;
}

/// Knowledge management functions
pub trait KnowledgeManagement: KnowledgeGraph {
    /// Store a finding in a node
    fn store_finding(&mut self, node_id: &str, key: &str, value: &str) -> Result<()> {
        let node = self.get_knowledge_mut(node_id)
            .ok_or_else(|| anyhow::anyhow!("Node not found: {}", node_id))?;
        node.findings.insert(key.to_string(), value.to_string());
        Ok(())
    }
    
    /// Get a finding from a node or any upstream node
    fn get_finding(&self, node_id: &str, key: &str) -> Option<String> {
        // First check current node
        if let Some(node) = self.get_knowledge(node_id) {
            if let Some(value) = node.findings.get(key) {
                return Some(value.clone());
            }
        }
        
        // Check upstream nodes (dependencies)
        self.get_upstream_findings(node_id, key)
    }
    
    /// Get finding from upstream nodes recursively
    fn get_upstream_findings(&self, node_id: &str, key: &str) -> Option<String> {
        for upstream_id in self.get_incoming_edges(node_id) {
            if let Some(node) = self.get_knowledge(&upstream_id) {
                if let Some(value) = node.findings.get(key) {
                    return Some(value.clone());
                }
                // Recursively check further upstream
                if let Some(value) = self.get_upstream_findings(&upstream_id, key) {
                    return Some(value);
                }
            }
        }
        None
    }
    
    /// Cache file content in a node
    fn cache_file(&mut self, node_id: &str, path: &str, content: &str) -> Result<()> {
        let node = self.get_knowledge_mut(node_id)
            .ok_or_else(|| anyhow::anyhow!("Node not found: {}", node_id))?;
        node.file_cache.insert(path.to_string(), content.to_string());
        Ok(())
    }
    
    /// Get cached file from this node or upstream
    fn get_cached_file(&self, node_id: &str, path: &str) -> Option<String> {
        // Check current node
        if let Some(node) = self.get_knowledge(node_id) {
            if let Some(content) = node.file_cache.get(path) {
                return Some(content.clone());
            }
        }
        
        // Check upstream nodes
        for upstream_id in self.get_incoming_edges(node_id) {
            if let Some(content) = self.get_cached_file(&upstream_id, path) {
                return Some(content);
            }
        }
        None
    }
    
    /// Record a tool call
    fn record_tool_call(&mut self, node_id: &str, tool_name: &str, summary: &str) -> Result<()> {
        let node = self.get_knowledge_mut(node_id)
            .ok_or_else(|| anyhow::anyhow!("Node not found: {}", node_id))?;
        node.tool_history.push(ToolCallRecord {
            tool_name: tool_name.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            summary: summary.to_string(),
        });
        Ok(())
    }
    
    /// Get all tool calls from this and upstream nodes
    fn get_tool_history(&self, node_id: &str) -> Vec<ToolCallRecord> {
        let mut history = Vec::new();
        
        // Get from upstream first (chronological order)
        for upstream_id in self.get_incoming_edges(node_id) {
            history.extend(self.get_tool_history(&upstream_id));
        }
        
        // Add current node
        if let Some(node) = self.get_knowledge(node_id) {
            history.extend(node.tool_history.clone());
        }
        
        history
    }
    
    /// Get all findings from this and upstream nodes as formatted context
    fn get_knowledge_context(&self, node_id: &str) -> String {
        let mut context = String::new();
        
        // Collect all findings from upstream
        let mut all_findings = HashMap::new();
        self.collect_upstream_findings_all(node_id, &mut all_findings);
        
        if !all_findings.is_empty() {
            context.push_str("**Knowledge from previous tasks:**\n");
            for (key, value) in &all_findings {
                context.push_str(&format!("- {}: {}\n", key, value));
            }
            context.push('\n');
        }
        
        // Add tool history summary
        let tool_history = self.get_tool_history(node_id);
        if !tool_history.is_empty() {
            context.push_str("**Previously accessed files:**\n");
            let mut seen = std::collections::HashSet::new();
            for record in &tool_history {
                if record.tool_name == "view_file" && seen.insert(&record.summary) {
                    context.push_str(&format!("- {}\n", record.summary));
                }
            }
        }
        
        context
    }
    
    /// Helper to collect all findings recursively
    fn collect_upstream_findings_all(&self, node_id: &str, findings: &mut HashMap<String, String>) {
        // Get from upstream first
        for upstream_id in self.get_incoming_edges(node_id) {
            self.collect_upstream_findings_all(&upstream_id, findings);
        }
        
        // Add current node findings
        if let Some(node) = self.get_knowledge(node_id) {
            findings.extend(node.findings.clone());
        }
    }
}

/// Simple in-memory knowledge graph for testing
#[derive(Debug, Clone, Default)]
pub struct SimpleKnowledgeGraph {
    pub nodes: HashMap<String, KnowledgeNode>,
    pub edges: Vec<(String, String)>, // (from, to)
}

impl SimpleKnowledgeGraph {
    pub fn new() -> Self {
        Self::default()
    }
    
    pub fn add_node(&mut self, node_id: &str) {
        self.nodes.insert(node_id.to_string(), KnowledgeNode::default());
    }
    
    pub fn add_edge(&mut self, from: &str, to: &str) {
        self.edges.push((from.to_string(), to.to_string()));
    }
}

impl KnowledgeGraph for SimpleKnowledgeGraph {
    fn get_knowledge_mut(&mut self, node_id: &str) -> Option<&mut KnowledgeNode> {
        self.nodes.get_mut(node_id)
    }
    
    fn get_knowledge(&self, node_id: &str) -> Option<&KnowledgeNode> {
        self.nodes.get(node_id)
    }
    
    fn get_incoming_edges(&self, node_id: &str) -> Vec<String> {
        self.edges.iter()
            .filter(|(_, to)| to == node_id)
            .map(|(from, _)| from.clone())
            .collect()
    }
}

impl KnowledgeManagement for SimpleKnowledgeGraph {}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_store_and_get_finding() {
        let mut graph = SimpleKnowledgeGraph::new();
        graph.add_node("node1");
        
        graph.store_finding("node1", "key1", "value1").unwrap();
        assert_eq!(graph.get_finding("node1", "key1"), Some("value1".to_string()));
    }
    
    #[test]
    fn test_upstream_finding_lookup() {
        let mut graph = SimpleKnowledgeGraph::new();
        graph.add_node("parent");
        graph.add_node("child");
        graph.add_edge("parent", "child");
        
        graph.store_finding("parent", "shared_key", "parent_value").unwrap();
        
        // Child should find parent's finding
        assert_eq!(graph.get_finding("child", "shared_key"), Some("parent_value".to_string()));
    }
    
    #[test]
    fn test_file_cache() {
        let mut graph = SimpleKnowledgeGraph::new();
        graph.add_node("node1");
        
        graph.cache_file("node1", "path/to/file.py", "file content").unwrap();
        assert_eq!(graph.get_cached_file("node1", "path/to/file.py"), Some("file content".to_string()));
    }
    
    #[test]
    fn test_tool_history() {
        let mut graph = SimpleKnowledgeGraph::new();
        graph.add_node("node1");
        
        graph.record_tool_call("node1", "view_file", "foo.py").unwrap();
        graph.record_tool_call("node1", "edit_file", "bar.py").unwrap();
        
        let history = graph.get_tool_history("node1");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].tool_name, "view_file");
        assert_eq!(history[1].tool_name, "edit_file");
    }
}
