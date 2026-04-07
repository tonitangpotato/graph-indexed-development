# SQLite Storage Design for gid-rs

## §1 Overview

This document specifies the SQLite-based persistent storage layer for gid-rs, replacing the current in-memory-only graph representation.

## §2 Goals

- Persist code graph (nodes, edges, metadata) to SQLite
- Support full-text search over node content
- Enable incremental updates (add/update/delete nodes without full rebuild)
- Provide transaction safety for multi-step graph mutations
- Support future features: history tracking, change logs, collaborative editing

## §3 GraphStorage Trait

All storage backends implement this trait:

```rust
#[async_trait]
pub trait GraphStorage: Send + Sync {
    // Initialization
    async fn initialize(&mut self) -> Result<(), StorageError>;
    async fn close(&mut self) -> Result<(), StorageError>;
    
    // Node operations
    async fn add_node(&mut self, node: &Node) -> Result<(), StorageError>;
    async fn get_node(&self, id: &str) -> Result<Option<Node>, StorageError>;
    async fn update_node(&mut self, node: &Node) -> Result<(), StorageError>;
    async fn delete_node(&mut self, id: &str) -> Result<(), StorageError>;
    async fn list_nodes(&self) -> Result<Vec<Node>, StorageError>;
    async fn search_nodes(&self, query: &str) -> Result<Vec<Node>, StorageError>;
    
    // Edge operations
    async fn add_edge(&mut self, edge: &Edge) -> Result<(), StorageError>;
    async fn get_edges_from(&self, node_id: &str) -> Result<Vec<Edge>, StorageError>;
    async fn get_edges_to(&self, node_id: &str) -> Result<Vec<Edge>, StorageError>;
    async fn delete_edge(&mut self, from: &str, to: &str, edge_type: &str) -> Result<(), StorageError>;
    async fn list_edges(&self) -> Result<Vec<Edge>, StorageError>;
    
    // Batch operations
    async fn add_nodes_batch(&mut self, nodes: &[Node]) -> Result<(), StorageError>;
    async fn add_edges_batch(&mut self, edges: &[Edge]) -> Result<(), StorageError>;
    
    // Transaction support
    async fn begin_transaction(&mut self) -> Result<(), StorageError>;
    async fn commit_transaction(&mut self) -> Result<(), StorageError>;
    async fn rollback_transaction(&mut self) -> Result<(), StorageError>;
    
    // Metadata operations
    async fn set_metadata(&mut self, node_id: &str, key: &str, value: serde_json::Value) -> Result<(), StorageError>;
    async fn get_metadata(&self, node_id: &str, key: &str) -> Result<Option<serde_json::Value>, StorageError>;
    async fn delete_metadata(&mut self, node_id: &str, key: &str) -> Result<(), StorageError>;
    
    // Tag operations
    async fn add_tag(&mut self, node_id: &str, tag: &str) -> Result<(), StorageError>;
    async fn remove_tag(&mut self, node_id: &str, tag: &str) -> Result<(), StorageError>;
    async fn get_tags(&self, node_id: &str) -> Result<Vec<String>, StorageError>;
    async fn find_by_tag(&self, tag: &str) -> Result<Vec<Node>, StorageError>;
    
    // Configuration
    async fn set_config(&mut self, key: &str, value: &str) -> Result<(), StorageError>;
    async fn get_config(&self, key: &str) -> Result<Option<String>, StorageError>;
    
    // Change tracking
    async fn log_change(&mut self, node_id: &str, change_type: &str, description: &str) -> Result<(), StorageError>;
    async fn get_change_history(&self, node_id: &str, limit: usize) -> Result<Vec<ChangeEntry>, StorageError>;
}
```

## §4 StorageError Types

```rust
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("Database error: {0}")]
    Database(String),
    
    #[error("Node not found: {0}")]
    NodeNotFound(String),
    
    #[error("Edge not found: from={from} to={to} type={edge_type}")]
    EdgeNotFound { from: String, to: String, edge_type: String },
    
    #[error("Serialization error: {0}")]
    Serialization(String),
    
    #[error("Transaction error: {0}")]
    Transaction(String),
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Invalid query: {0}")]
    InvalidQuery(String),
    
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub enum StorageOp {
    Initialize,
    Close,
    AddNode,
    GetNode,
    UpdateNode,
    DeleteNode,
    ListNodes,
    SearchNodes,
    AddEdge,
    GetEdgesFrom,
    GetEdgesTo,
    DeleteEdge,
    ListEdges,
    BeginTransaction,
    CommitTransaction,
    RollbackTransaction,
    SetMetadata,
    GetMetadata,
    DeleteMetadata,
    AddTag,
    RemoveTag,
    GetTags,
    FindByTag,
    SetConfig,
    GetConfig,
    LogChange,
    GetChangeHistory,
}
```

## §5 SQLite Schema

### Core Tables

```sql
-- Nodes table (primary graph nodes)
CREATE TABLE nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    file_path TEXT,
    lang TEXT,
    signature TEXT,
    node_type TEXT,
    description TEXT,
    priority INTEGER,
    assigned_to TEXT,
    line_start INTEGER,
    line_end INTEGER,
    body TEXT,
    parent_id TEXT,
    depth INTEGER DEFAULT 0,
    complexity INTEGER,
    is_public BOOLEAN DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (parent_id) REFERENCES nodes(id) ON DELETE SET NULL
);

-- Edges table (relationships between nodes)
CREATE TABLE edges (
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL,
    edge_type TEXT NOT NULL,
    metadata TEXT, -- JSON
    created_at INTEGER NOT NULL,
    PRIMARY KEY (from_id, to_id, edge_type),
    FOREIGN KEY (from_id) REFERENCES nodes(id) ON DELETE CASCADE,
    FOREIGN KEY (to_id) REFERENCES nodes(id) ON DELETE CASCADE
);

-- Node metadata (flexible key-value store)
CREATE TABLE node_metadata (
    node_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL, -- JSON
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (node_id, key),
    FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE
);

-- Node tags (categorization)
CREATE TABLE node_tags (
    node_id TEXT NOT NULL,
    tag TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (node_id, tag),
    FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE
);

-- Knowledge base (user-added context)
CREATE TABLE knowledge (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    tags TEXT, -- JSON array
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

-- Configuration (system settings)
CREATE TABLE config (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

-- Change log (audit trail)
CREATE TABLE change_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id TEXT,
    change_type TEXT NOT NULL, -- 'create', 'update', 'delete'
    description TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE SET NULL
);
```

### Indexes

```sql
-- Node lookup optimization
CREATE INDEX idx_nodes_name ON nodes(name);
CREATE INDEX idx_nodes_file_path ON nodes(file_path);
CREATE INDEX idx_nodes_node_type ON nodes(node_type);
CREATE INDEX idx_nodes_parent_id ON nodes(parent_id);
CREATE INDEX idx_nodes_priority ON nodes(priority);

-- Edge lookup optimization
CREATE INDEX idx_edges_from ON edges(from_id);
CREATE INDEX idx_edges_to ON edges(to_id);
CREATE INDEX idx_edges_type ON edges(edge_type);

-- Tag lookup optimization
CREATE INDEX idx_node_tags_tag ON node_tags(tag);

-- Change log optimization
CREATE INDEX idx_change_log_node ON change_log(node_id);
CREATE INDEX idx_change_log_time ON change_log(timestamp);
```

### Full-Text Search (FTS5)

```sql
-- FTS5 virtual table for node content search
CREATE VIRTUAL TABLE nodes_fts USING fts5(
    id UNINDEXED,
    name,
    description,
    body,
    content=nodes,
    content_rowid=rowid
);

-- Triggers to keep FTS in sync
CREATE TRIGGER nodes_fts_insert AFTER INSERT ON nodes BEGIN
    INSERT INTO nodes_fts(rowid, id, name, description, body)
    VALUES (new.rowid, new.id, new.name, new.description, new.body);
END;

CREATE TRIGGER nodes_fts_delete AFTER DELETE ON nodes BEGIN
    DELETE FROM nodes_fts WHERE rowid = old.rowid;
END;

CREATE TRIGGER nodes_fts_update AFTER UPDATE ON nodes BEGIN
    UPDATE nodes_fts SET
        name = new.name,
        description = new.description,
        body = new.body
    WHERE rowid = new.rowid;
END;
```

## §6 Data Types

### ChangeEntry

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChangeEntry {
    pub id: i64,
    pub node_id: Option<String>,
    pub change_type: String,
    pub description: String,
    pub timestamp: i64,
}
```

## §7 Migration Strategy

1. **Phase 1**: Implement trait and SQLite backend (Layer 1-3 tasks)
2. **Phase 2**: Add tests and integration
3. **Phase 3**: Migrate existing code to use trait
4. **Phase 4**: Add advanced features (history, collaboration)

## §8 Performance Considerations

- Use prepared statements for all queries
- Batch inserts in transactions (1000+ nodes/edges at once)
- Use connection pooling (sqlx Pool)
- Optimize FTS5 for large codebases (>100k nodes)
- Consider WAL mode for concurrent reads

## §9 Backwards Compatibility

- Keep existing in-memory Graph struct as default
- Add `--storage sqlite` flag to CLI
- Implement GraphStorage trait for in-memory backend too
- Allow seamless switching between backends

## §10 Future Extensions

- PostgreSQL backend (same trait)
- S3/cloud storage backend
- Graph version control (git-like diffs)
- Multi-user collaboration (CRDT-based)
- Real-time graph subscriptions (websocket)

## §11 Security

- Sanitize all SQL inputs (use parameterized queries only)
- Validate node IDs (alphanumeric + dash/underscore only)
- Limit FTS query complexity (prevent DOS)
- Encrypt database at rest (SQLCipher extension)

## §12 Extended Node and Edge Structures

### Node Extensions

The Node struct is extended with 14 new fields to support rich metadata:

```rust
pub struct Node {
    // Existing fields
    pub id: String,
    pub name: String,
    
    // New fields (all optional for backwards compatibility)
    pub file_path: Option<String>,       // Source file path
    pub lang: Option<String>,             // Programming language
    pub signature: Option<String>,        // Function/class signature
    pub node_type: Option<String>,        // "function", "class", "task", etc.
    pub description: Option<String>,      // Human-readable description
    pub priority: Option<i32>,            // Task priority (1-100)
    pub assigned_to: Option<String>,      // Task assignee
    pub line_start: Option<u32>,          // Start line in source file
    pub line_end: Option<u32>,            // End line in source file
    pub body: Option<String>,             // Full source code or content
    pub parent_id: Option<String>,        // Parent node (for hierarchies)
    pub depth: Option<u32>,               // Depth in hierarchy
    pub complexity: Option<i32>,          // Cyclomatic complexity or estimate
    pub is_public: Option<bool>,          // Public vs private visibility
}
```

### Edge Extensions

The Edge struct is extended with metadata support:

```rust
pub struct Edge {
    // Existing fields
    pub from: String,
    pub to: String,
    pub edge_type: String,
    
    // New field
    pub metadata: Option<serde_json::Value>,  // Arbitrary JSON metadata
}
```

Edge metadata examples:
- Call edges: `{"call_type": "direct", "args_count": 3}`
- Import edges: `{"import_type": "default", "alias": "React"}`
- Dependency edges: `{"version": "^1.2.3", "dev": false}`
- Task edges: `{"blocking": true, "effort_hours": 8}`
