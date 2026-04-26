use std::collections::HashMap;
use serde_json::Value;

use crate::graph::{Edge, Node, ProjectMeta};
use crate::task_graph_knowledge::KnowledgeNode;
use super::error::StorageError;

/// Filter criteria for node queries.
#[derive(Debug, Clone, Default)]
pub struct NodeFilter {
    /// Filter by node type (e.g., "task", "file", "function").
    pub node_type: Option<String>,
    /// Filter by status (e.g., "todo", "in_progress", "done").
    pub status: Option<String>,
    /// Filter by file path (exact match or prefix).
    pub file_path: Option<String>,
    /// Filter by tag (node must have this tag).
    pub tag: Option<String>,
    /// Filter by owner.
    pub owner: Option<String>,
    /// Maximum number of results to return.
    pub limit: Option<usize>,
    /// Offset for pagination (skip this many results).
    pub offset: Option<usize>,
}

impl NodeFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_node_type(mut self, node_type: impl Into<String>) -> Self {
        self.node_type = Some(node_type.into());
        self
    }

    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }

    pub fn with_file_path(mut self, file_path: impl Into<String>) -> Self {
        self.file_path = Some(file_path.into());
        self
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = Some(tag.into());
        self
    }

    pub fn with_owner(mut self, owner: impl Into<String>) -> Self {
        self.owner = Some(owner.into());
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn with_offset(mut self, offset: usize) -> Self {
        self.offset = Some(offset);
        self
    }
}

/// GOAL-1.9: GraphStorage trait — abstract interface for all storage operations.
///
/// Sync, `&self`, object-safe (GUARD-10).
/// All methods return `Result<T, StorageError>`.
pub trait GraphStorage {
    // ── GOAL-1.9a: CRUD operations ─────────────────────────────

    /// Insert or replace a node. Writes all 21 dedicated columns.
    fn put_node(&self, node: &Node) -> Result<(), StorageError>;

    /// Retrieve a node by ID, or `None` if it does not exist.
    fn get_node(&self, id: &str) -> Result<Option<Node>, StorageError>;

    /// Delete a node and its associated edges, tags, metadata, and knowledge.
    fn delete_node(&self, id: &str) -> Result<(), StorageError>;

    /// Get all edges where the given node is `from_node` or `to_node`.
    fn get_edges(&self, node_id: &str) -> Result<Vec<Edge>, StorageError>;

    /// Insert a new edge.
    fn add_edge(&self, edge: &Edge) -> Result<(), StorageError>;

    /// Remove an edge matching (from, to, relation).
    fn remove_edge(&self, from: &str, to: &str, relation: &str) -> Result<(), StorageError>;

    // ── GOAL-1.9b: Query and search ───────────────────────────

    /// Query nodes matching the given filter criteria.
    fn query_nodes(&self, filter: &NodeFilter) -> Result<Vec<Node>, StorageError>;

    /// Full-text search over node content (FTS5).
    fn search(&self, query: &str) -> Result<Vec<Node>, StorageError>;

    // ── GOAL-1.9c: Tag and node-metadata accessors ─────────────

    /// Get all tags for a node.
    fn get_tags(&self, node_id: &str) -> Result<Vec<String>, StorageError>;

    /// Replace all tags for a node (delete + insert).
    fn set_tags(&self, node_id: &str, tags: &[String]) -> Result<(), StorageError>;

    /// Get all metadata key-value pairs for a node.
    fn get_metadata(&self, node_id: &str) -> Result<HashMap<String, Value>, StorageError>;

    /// Replace all metadata for a node (delete + insert).
    fn set_metadata(
        &self,
        node_id: &str,
        metadata: &HashMap<String, Value>,
    ) -> Result<(), StorageError>;

    // ── GOAL-1.9d: Project and knowledge accessors ─────────────

    /// Read project metadata from the config table.
    fn get_project_meta(&self) -> Result<Option<ProjectMeta>, StorageError>;

    /// Write project metadata to the config table.
    fn set_project_meta(&self, meta: &ProjectMeta) -> Result<(), StorageError>;

    /// Get knowledge data for a node.
    fn get_knowledge(&self, node_id: &str) -> Result<Option<KnowledgeNode>, StorageError>;

    /// Set knowledge data for a node.
    fn set_knowledge(&self, node_id: &str, knowledge: &KnowledgeNode)
        -> Result<(), StorageError>;

    // ── GOAL-1.9e: Enumeration and counts ──────────────────────

    /// Return the total number of nodes.
    fn get_node_count(&self) -> Result<usize, StorageError>;

    /// Return the total number of edges.
    fn get_edge_count(&self) -> Result<usize, StorageError>;

    /// Return all node IDs.
    fn get_all_node_ids(&self) -> Result<Vec<String>, StorageError>;

    // ── GOAL-1.15: Batch operations ───────────────────────────

    /// Execute a batch of operations atomically (all-or-nothing).
    fn execute_batch(&self, ops: &[BatchOp]) -> Result<(), StorageError>;
}

/// GOAL-1.15: atomic batch operations (command pattern for object-safety).
///
/// Variants match design.md §7.1 — PutNode, DeleteNode, AddEdge, RemoveEdge,
/// SetTags, SetMetadata, SetKnowledge.
///
/// # Execution Semantics
///
/// - **Atomicity:** All operations execute within a single transaction.
///   If any operation fails, the entire batch is rolled back.
/// - **Ordering:** Operations execute in slice order. Callers may depend
///   on sequential consistency within a batch.
/// - **FTS Synchronization:** Content-sync triggers fire within the
///   transaction, maintaining FTS consistency even on rollback.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // Boxing PutNode/SetKnowledge would be a breaking API change; batches are short-lived so the heap savings aren't worth it.
pub enum BatchOp {
    PutNode(Node),
    DeleteNode(String),
    AddEdge(Edge),
    RemoveEdge {
        from: String,
        to: String,
        relation: String,
    },
    SetTags(String, Vec<String>),
    SetMetadata(String, HashMap<String, Value>),
    SetKnowledge(String, KnowledgeNode),
}
