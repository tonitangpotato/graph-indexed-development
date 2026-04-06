use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use crate::task_graph_knowledge::{KnowledgeNode, KnowledgeGraph, KnowledgeManagement};

/// A complete GID graph with nodes and edges.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Graph {
    #[serde(default)]
    pub project: Option<ProjectMeta>,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

/// Project metadata.
/// Accepts either a string (`project: myproject`) or a struct (`project: {name: myproject}`).
#[derive(Debug, Clone, Serialize)]
pub struct ProjectMeta {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

impl<'de> serde::Deserialize<'de> for ProjectMeta {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;

        struct ProjectMetaVisitor;

        impl<'de> de::Visitor<'de> for ProjectMetaVisitor {
            type Value = ProjectMeta;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string or a map with 'name' field")
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<ProjectMeta, E>
            where
                E: de::Error,
            {
                Ok(ProjectMeta { name: v.to_string(), description: None })
            }

            fn visit_map<M>(self, map: M) -> std::result::Result<ProjectMeta, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                #[derive(serde::Deserialize)]
                struct ProjectMetaInner {
                    name: String,
                    #[serde(default)]
                    description: Option<String>,
                }
                let inner = ProjectMetaInner::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(ProjectMeta { name: inner.name, description: inner.description })
            }
        }

        deserializer.deserialize_any(ProjectMetaVisitor)
    }
}

/// A node in the graph (task, code file, component, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub status: NodeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    /// Node type: task, file, component, feature, layer, etc.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub node_type: Option<String>,
    /// Knowledge storage: findings, file cache, and tool history.
    #[serde(default, skip_serializing_if = "KnowledgeNode::is_empty")]
    pub knowledge: KnowledgeNode,
    /// Additional metadata.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Node {
    pub fn new(id: &str, title: &str) -> Self {
        Self {
            id: id.to_string(),
            title: title.to_string(),
            status: NodeStatus::Todo,
            description: None,
            assigned_to: None,
            tags: Vec::new(),
            priority: None,
            node_type: None,
            knowledge: KnowledgeNode::default(),
            metadata: HashMap::new(),
        }
    }

    pub fn with_description(mut self, desc: &str) -> Self {
        self.description = Some(desc.to_string());
        self
    }

    pub fn with_status(mut self, status: NodeStatus) -> Self {
        self.status = status;
        self
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = Some(priority);
        self
    }
}

/// Status of a node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeStatus {
    Todo,
    #[serde(alias = "in_progress", alias = "in-progress")]
    InProgress,
    Done,
    Blocked,
    Cancelled,
    /// Task execution failed (verify failed, sub-agent error, etc.)
    Failed,
    /// Task needs human/re-planner intervention (merge conflict, structural issue)
    #[serde(alias = "needs_resolution", alias = "needs-resolution")]
    NeedsResolution,
}

impl Default for NodeStatus {
    fn default() -> Self {
        Self::Todo
    }
}

impl std::fmt::Display for NodeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeStatus::Todo => write!(f, "todo"),
            NodeStatus::InProgress => write!(f, "in_progress"),
            NodeStatus::Done => write!(f, "done"),
            NodeStatus::Blocked => write!(f, "blocked"),
            NodeStatus::Cancelled => write!(f, "cancelled"),
            NodeStatus::Failed => write!(f, "failed"),
            NodeStatus::NeedsResolution => write!(f, "needs_resolution"),
        }
    }
}

impl std::str::FromStr for NodeStatus {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "todo" => Ok(NodeStatus::Todo),
            "in_progress" | "in-progress" => Ok(NodeStatus::InProgress),
            "done" => Ok(NodeStatus::Done),
            "blocked" => Ok(NodeStatus::Blocked),
            "cancelled" => Ok(NodeStatus::Cancelled),
            "failed" => Ok(NodeStatus::Failed),
            "needs_resolution" | "needs-resolution" => Ok(NodeStatus::NeedsResolution),
            _ => Err(anyhow::anyhow!("Unknown status: {}", s)),
        }
    }
}

/// An edge (relationship) between two nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    #[serde(default = "default_relation")]
    pub relation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

fn default_relation() -> String {
    "depends_on".to_string()
}

impl Edge {
    pub fn new(from: &str, to: &str, relation: &str) -> Self {
        Self {
            from: from.to_string(),
            to: to.to_string(),
            relation: relation.to_string(),
            weight: None,
            confidence: None,
        }
    }

    pub fn depends_on(from: &str, to: &str) -> Self {
        Self::new(from, to, "depends_on")
    }
}

// ─── Graph operations ────────────────────────────────────────

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    // ── Node operations ──

    pub fn get_node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn get_node_mut(&mut self, id: &str) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    pub fn add_node(&mut self, node: Node) {
        if self.get_node(&node.id).is_none() {
            self.nodes.push(node);
        }
    }

    pub fn remove_node(&mut self, id: &str) -> Option<Node> {
        let pos = self.nodes.iter().position(|n| n.id == id)?;
        let node = self.nodes.remove(pos);
        // Remove associated edges
        self.edges.retain(|e| e.from != id && e.to != id);
        Some(node)
    }

    pub fn update_status(&mut self, id: &str, status: NodeStatus) -> bool {
        if let Some(node) = self.get_node_mut(id) {
            node.status = status;
            true
        } else {
            false
        }
    }

    // ── Edge operations ──

    pub fn add_edge(&mut self, edge: Edge) {
        // Avoid duplicates
        let exists = self.edges.iter().any(|e| {
            e.from == edge.from && e.to == edge.to && e.relation == edge.relation
        });
        if !exists {
            self.edges.push(edge);
        }
    }

    pub fn remove_edge(&mut self, from: &str, to: &str, relation: Option<&str>) {
        self.edges.retain(|e| {
            !(e.from == from && e.to == to && relation.map_or(true, |r| e.relation == r))
        });
    }

    pub fn edges_from(&self, id: &str) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.from == id).collect()
    }

    pub fn edges_to(&self, id: &str) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.to == id).collect()
    }

    // ── Query helpers ──

    /// Get tasks that are ready (todo + all depends_on are done).
    pub fn ready_tasks(&self) -> Vec<&Node> {
        self.nodes
            .iter()
            .filter(|n| n.status == NodeStatus::Todo)
            .filter(|n| {
                let deps: Vec<&Edge> = self.edges_from(&n.id)
                    .into_iter()
                    .filter(|e| e.relation == "depends_on")
                    .collect();
                deps.iter().all(|e| {
                    self.get_node(&e.to)
                        .map_or(true, |dep| dep.status == NodeStatus::Done)
                })
            })
            .collect()
    }

    /// Get tasks by status.
    pub fn tasks_by_status(&self, status: &NodeStatus) -> Vec<&Node> {
        self.nodes.iter().filter(|n| &n.status == status).collect()
    }

    /// Summary statistics.
    pub fn summary(&self) -> GraphSummary {
        let mut s = GraphSummary {
            total_nodes: self.nodes.len(),
            total_edges: self.edges.len(),
            ..Default::default()
        };
        for n in &self.nodes {
            match n.status {
                NodeStatus::Todo => s.todo += 1,
                NodeStatus::InProgress => s.in_progress += 1,
                NodeStatus::Done => s.done += 1,
                NodeStatus::Blocked => s.blocked += 1,
                NodeStatus::Cancelled => s.cancelled += 1,
                NodeStatus::Failed => s.failed += 1,
                NodeStatus::NeedsResolution => s.needs_resolution += 1,
            }
        }
        s.ready = self.ready_tasks().len();
        s
    }

    /// Get a human-readable text summary of the graph state.
    pub fn summary_text(&self) -> String {
        let s = self.summary();
        let mut lines = vec![
            format!("Graph: {} nodes, {} edges", s.total_nodes, s.total_edges),
        ];

        if s.total_nodes > 0 {
            lines.push(format!(
                "Status: {} todo, {} in-progress, {} done, {} blocked, {} cancelled",
                s.todo, s.in_progress, s.done, s.blocked, s.cancelled
            ));
            lines.push(format!("Ready tasks: {}", s.ready));
        }

        // Show project name if available
        if let Some(ref project) = self.project {
            lines.insert(0, format!("Project: {}", project.name));
        }

        lines.join("\n")
    }

    /// Calculate graph health score (0.0 to 1.0).
    /// 
    /// Health is based on:
    /// - Progress: ratio of done tasks to total
    /// - Flow: ratio of ready tasks to remaining (non-blocked) tasks
    /// - Connectivity: graphs with edges are healthier than isolated nodes
    /// 
    /// Returns 1.0 for a fully complete graph, 0.0 for an empty or stuck graph.
    pub fn health(&self) -> f64 {
        if self.nodes.is_empty() {
            return 0.0;
        }

        let s = self.summary();
        let total = s.total_nodes as f64;

        // Progress score: what fraction is done?
        let progress = s.done as f64 / total;

        // Flow score: are there ready tasks to work on? (avoid stuck graphs)
        let remaining = s.todo + s.in_progress;
        let flow = if remaining == 0 {
            1.0 // All done, perfect flow
        } else if s.ready == 0 && s.todo > 0 {
            0.0 // Stuck: todos exist but none are ready (all blocked by dependencies)
        } else {
            (s.ready as f64) / (remaining as f64)
        };

        // Connectivity score: graphs with structure are healthier
        let connectivity = if self.nodes.len() > 1 {
            let max_edges = self.nodes.len() * (self.nodes.len() - 1);
            let actual = self.edges.len().min(max_edges);
            (actual as f64 / max_edges as f64).min(1.0)
        } else {
            1.0 // Single node is "connected"
        };

        // Blocked penalty: heavily blocked graphs are unhealthy
        let blocked_ratio = s.blocked as f64 / total;
        let blocked_penalty = 1.0 - blocked_ratio;

        // Weighted combination
        let health = 0.4 * progress + 0.3 * flow + 0.1 * connectivity + 0.2 * blocked_penalty;
        health.clamp(0.0, 1.0)
    }

    /// Mark a task as done. Returns true if found and updated.
    pub fn mark_task_done(&mut self, node_id: &str) -> bool {
        self.update_status(node_id, NodeStatus::Done)
    }

    /// Get executable tasks (alias for ready_tasks, returns owned Task structs).
    pub fn get_executable_tasks(&self) -> Vec<Task> {
        self.ready_tasks()
            .into_iter()
            .map(|node| Task {
                id: node.id.clone(),
                title: node.title.clone(),
                description: node.description.clone(),
                priority: node.priority,
            })
            .collect()
    }
}

/// A simplified task representation for execution.
#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<u8>,
}

#[derive(Debug, Default)]
pub struct GraphSummary {
    pub total_nodes: usize,
    pub total_edges: usize,
    pub todo: usize,
    pub in_progress: usize,
    pub done: usize,
    pub blocked: usize,
    pub cancelled: usize,
    pub failed: usize,
    pub needs_resolution: usize,
    pub ready: usize,
}

// Implement knowledge management for Graph so users can call
// graph.store_finding(), graph.cache_file(), etc. directly.
impl KnowledgeGraph for Graph {
    fn get_knowledge_mut(&mut self, node_id: &str) -> Option<&mut KnowledgeNode> {
        self.nodes.iter_mut()
            .find(|n| n.id == node_id)
            .map(|n| &mut n.knowledge)
    }

    fn get_knowledge(&self, node_id: &str) -> Option<&KnowledgeNode> {
        self.nodes.iter()
            .find(|n| n.id == node_id)
            .map(|n| &n.knowledge)
    }

    fn get_incoming_edges(&self, node_id: &str) -> Vec<String> {
        self.edges.iter()
            .filter(|e| e.to == node_id)
            .map(|e| e.from.clone())
            .collect()
    }
}

impl KnowledgeManagement for Graph {}

impl std::fmt::Display for GraphSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} nodes, {} edges | todo={} progress={} done={} blocked={} failed={} cancelled={} | ready={}",
            self.total_nodes, self.total_edges,
            self.todo, self.in_progress, self.done, self.blocked, self.failed, self.cancelled,
            self.ready,
        )
    }
}
