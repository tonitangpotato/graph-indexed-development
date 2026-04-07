# Design: Storage Layer (SQLite Migration)

**Covers:** GOALs 1.1–1.18 | GUARDs 1, 2, 6, 7, 8, 10
**Date:** 2026-04-06
**Status:** Draft

---

## 1. Overview

The storage layer replaces the existing JSON-file persistence with a single SQLite database (`graph.db`) accessed through a `GraphStorage` trait and its concrete `SqliteStorage` implementation. All graph data—nodes (with 21 dedicated columns), edges (with relation, weight, and confidence), node metadata (KV pairs), tags (many-to-many), knowledge entries (JSON-serialized), and configuration—live in a unified schema with FTS5 full-text search on node content. The implementation uses `rusqlite` with a `RefCell<Connection>` for interior mutability, batches writes through a `StorageOp` enum for atomicity, and uses `String` IDs matching the existing gid-core `Node.id` type. This design satisfies the performance, integrity, and query requirements specified in GOALs 1.1–1.18 while respecting all applicable GUARDs.

---

## 2. SQLite Schema

### 2.1 `nodes`

```sql
-- GOAL-1.1: persistent node storage (all node types in single table)
-- GOAL-1.2: dedicated columns for high-frequency fields
CREATE TABLE nodes (
    id            TEXT PRIMARY KEY NOT NULL,
    title         TEXT,
    status        TEXT,                        -- todo, in_progress, done, blocked, cancelled, etc.
    description   TEXT,
    node_type     TEXT NOT NULL,               -- task, file, function, class, module, feature, component, layer, knowledge
    file_path     TEXT,
    lang          TEXT,
    start_line    INTEGER,
    end_line      INTEGER,
    signature     TEXT,
    visibility    TEXT,                         -- public, private, crate (free-form TEXT)
    doc_comment   TEXT,
    body_hash     TEXT,
    node_kind     TEXT,                         -- code-level: Function, Struct, Impl, Trait, Enum, etc.
    owner         TEXT,
    source        TEXT,
    repo          TEXT,
    priority      INTEGER,                     -- 0–255, maps to Option<u8>
    assigned_to   TEXT,
    created_at    TEXT,                         -- ISO-8601
    updated_at    TEXT                          -- ISO-8601
) STRICT;
```

### 2.2 `edges`

```sql
-- GOAL-1.5: directed, typed edges with weight and confidence
CREATE TABLE edges (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    from_node   TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    to_node     TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    relation    TEXT NOT NULL,                -- depends_on, blocks, calls, imports, etc.
    weight      REAL DEFAULT 1.0,
    confidence  REAL,                         -- maps to Edge.confidence: Option<f64>
    metadata    TEXT                          -- JSON blob for additional edge data
) STRICT;
```

### 2.3 `node_metadata`

```sql
-- GOAL-1.3: key-value metadata per node (dynamic/low-frequency fields)
CREATE TABLE node_metadata (
    node_id     TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    key         TEXT NOT NULL,
    value       TEXT NOT NULL,
    PRIMARY KEY (node_id, key)
) STRICT;
```

### 2.4 `node_tags`

```sql
-- GOAL-1.4: many-to-many tag associations
CREATE TABLE node_tags (
    node_id     TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    tag         TEXT NOT NULL,
    PRIMARY KEY (node_id, tag)
) STRICT;
```

### 2.5 `knowledge`

```sql
-- GOAL-1.6: knowledge data (JSON-blob approach matching KnowledgeNode struct)
CREATE TABLE knowledge (
    node_id       TEXT PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    findings      TEXT,                        -- JSON object: HashMap<String, String>
    file_cache    TEXT,                        -- JSON object: HashMap<String, String>
    tool_history  TEXT                         -- JSON array of {tool_name, timestamp, summary}
) STRICT;
```

### 2.6 `config`

```sql
-- GOAL-1.16: project-level metadata and schema info
CREATE TABLE config (
    key         TEXT PRIMARY KEY NOT NULL,
    value       TEXT NOT NULL
) STRICT;
-- Required rows: 'project_name', 'project_description', 'schema_version' (initially "1")
```

### 2.7 `change_log`

```sql
-- GOAL-1.8: audit trail (can be enabled/disabled via configuration)
CREATE TABLE change_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    batch_id    TEXT,                          -- groups related changes in one logical op
    timestamp   TEXT NOT NULL,                 -- ISO-8601
    actor       TEXT,
    operation   TEXT NOT NULL,
    node_id     TEXT,
    field       TEXT,
    old_value   TEXT,
    new_value   TEXT,
    context     TEXT
) STRICT;
```

### 2.8 `nodes_fts` (FTS5 virtual table)

```sql
-- GOAL-1.7: full-text search over node content
-- Indexes id, title, description, signature, doc_comment
-- See §6 for content-sync triggers
CREATE VIRTUAL TABLE nodes_fts USING fts5(
    id,
    title,
    description,
    signature,
    doc_comment,
    content='nodes',
    content_rowid='rowid'
);
```

---

## 3. GraphStorage Trait

All methods return `Result<T, StorageError>`. The trait is object-safe (GUARD-10) and takes `&self` (not `&mut self`) to allow concurrent read access (GUARD-8). Designed for a single concrete implementation today (SQLite) with room for future backends.

Method signatures match GOAL-1.9a through GOAL-1.9e exactly. Methods not specified in the requirements (shortest_path, subgraph, topological_sort, export/import) are intentionally excluded — those are handled at the higher query layer.

```rust
/// GOAL-1.9: GraphStorage trait — abstract interface for all storage operations.
/// Sync, &self, object-safe.
pub trait GraphStorage {
    // ── GOAL-1.9a: CRUD operations ─────────────────────────────
    fn get_node(&self, id: &str) -> Result<Option<Node>, StorageError>;
    fn put_node(&self, node: &Node) -> Result<(), StorageError>;
    fn delete_node(&self, id: &str) -> Result<(), StorageError>;
    fn get_edges(&self, node_id: &str) -> Result<Vec<Edge>, StorageError>;
    fn add_edge(&self, edge: &Edge) -> Result<(), StorageError>;
    fn remove_edge(&self, from: &str, to: &str, relation: &str) -> Result<(), StorageError>;

    // ── GOAL-1.9b: Query and search ───────────────────────────
    fn query_nodes(&self, filter: &NodeFilter) -> Result<Vec<Node>, StorageError>;
    fn search(&self, query: &str) -> Result<Vec<Node>, StorageError>;

    // ── GOAL-1.9c: Tag and node-metadata accessors ─────────────
    fn get_tags(&self, node_id: &str) -> Result<Vec<String>, StorageError>;
    fn set_tags(&self, node_id: &str, tags: &[String]) -> Result<(), StorageError>;
    fn get_metadata(&self, node_id: &str) -> Result<HashMap<String, Value>, StorageError>;
    fn set_metadata(&self, node_id: &str, metadata: &HashMap<String, Value>) -> Result<(), StorageError>;

    // ── GOAL-1.9d: Project and knowledge accessors ─────────────
    fn get_project_meta(&self) -> Result<Option<ProjectMeta>, StorageError>;
    fn set_project_meta(&self, meta: &ProjectMeta) -> Result<(), StorageError>;
    fn get_knowledge(&self, node_id: &str) -> Result<Option<KnowledgeNode>, StorageError>;
    fn set_knowledge(&self, node_id: &str, knowledge: &KnowledgeNode) -> Result<(), StorageError>;

    // ── GOAL-1.9e: Enumeration and counts ──────────────────────
    fn get_node_count(&self) -> Result<usize, StorageError>;
    fn get_edge_count(&self) -> Result<usize, StorageError>;
    fn get_all_node_ids(&self) -> Result<Vec<String>, StorageError>;

    // ── GOAL-1.15: Batch operations ───────────────────────────
    fn execute_batch(&self, ops: &[StorageOp]) -> Result<(), StorageError>;
}
```

**Note on `ProjectMeta`:** Read/written via the `config` table (§2.6). `get_project_meta` reads `project_name` and `project_description` keys; `set_project_meta` writes them.

**Note on `KnowledgeNode`:** Maps to the `knowledge` table (§2.5). The struct has `findings: HashMap<String, String>`, `file_cache: HashMap<String, String>`, `tool_history: Vec<ToolCallRecord>` — each serialized as JSON.

---

## 4. SqliteStorage Implementation

### 4.1 Struct Definition

```rust
use rusqlite::Connection;
use std::cell::RefCell;
use std::path::PathBuf;

/// GUARD-1: single writer via RefCell interior mutability
/// GUARD-2: no unsafe code — RefCell is safe Rust
pub struct SqliteStorage {
    conn: RefCell<Connection>,
    path: PathBuf,
    /// GOAL-1.8: whether change_log writes are enabled
    change_log_enabled: bool,
}
```

### 4.2 Constructor

```rust
impl SqliteStorage {
    /// Opens (or creates) the database, runs migrations, enables WAL mode,
    /// sets PRAGMAs, and creates FTS triggers.
    ///
    /// GOAL-1.10: SqliteStorage implements GraphStorage
    /// GOAL-1.11: WAL mode + PRAGMAs configured on open
    /// GUARD-6: WAL mode for read concurrency
    /// GUARD-7: foreign_keys ON for referential integrity
    /// GUARD-8: WAL mode satisfies concurrent-read guarantee at the
    ///          SQLite level (readers never block on writes). RefCell
    ///          enforces Rust borrow checking within a single thread.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let path = path.into();
        let conn = Connection::open(&path)?;

        // GOAL-1.11: required PRAGMAs
        conn.execute_batch("
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            PRAGMA synchronous = NORMAL;
            PRAGMA busy_timeout = 5000;
            PRAGMA cache_size = -2000;   -- 2 MB
        ")?;

        let storage = Self {
            conn: RefCell::new(conn),
            path,
            change_log_enabled: true,  // default on per GOAL-1.8
        };
        storage.run_schema()?;
        storage.create_fts_triggers()?;
        Ok(storage)
    }
}
```

### 4.3 Key Method Pseudocode

#### `put_node`

```rust
fn put_node(&self, node: &Node) -> Result<(), StorageError> {
    let conn = self.conn.borrow();
    // GOAL-1.2: INSERT OR REPLACE into all dedicated columns (see §12 for Node struct extension)
    conn.execute(
        "INSERT OR REPLACE INTO nodes (
            id, title, status, description, node_type,
            file_path, lang, start_line, end_line, signature,
            visibility, doc_comment, body_hash, node_kind,
            owner, source, repo, priority, assigned_to,
            created_at, updated_at
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5,
            ?6, ?7, ?8, ?9, ?10,
            ?11, ?12, ?13, ?14,
            ?15, ?16, ?17, ?18, ?19,
            ?20, ?21
         )",
        params![
            node.id,
            node.title,
            node.status.as_deref(),
            node.description.as_deref(),
            node.node_type,
            node.file_path.as_deref(),
            node.lang.as_deref(),
            node.start_line,
            node.end_line,
            node.signature.as_deref(),
            node.visibility.as_deref(),
            node.doc_comment.as_deref(),
            node.body_hash.as_deref(),
            node.node_kind.as_deref(),
            node.owner.as_deref(),
            node.source.as_deref(),
            node.repo.as_deref(),
            node.priority,
            node.assigned_to.as_deref(),
            node.created_at.as_deref(),
            node.updated_at.as_deref(),
        ],
    )?;
    // FTS content-sync trigger fires automatically (§6)
    // Change log written if self.change_log_enabled (§8.1)
    Ok(())
}
```

#### `search` (FTS5)

```rust
fn search(&self, query: &str) -> Result<Vec<Node>, StorageError> {
    let conn = self.conn.borrow();
    // GOAL-1.7: full-text search via FTS5
    // FINDING-8: Sanitize user input to prevent FTS5 syntax errors.
    // Wrap raw user input in double-quotes for literal matching;
    // allow advanced syntax only via a separate API.
    let safe_query = format!("\"{}\"", query.replace('"', "\"\""));
    let mut stmt = conn.prepare(
        "SELECT n.* FROM nodes_fts f
         JOIN nodes n ON n.rowid = f.rowid
         WHERE nodes_fts MATCH ?1
         ORDER BY rank"
    )?;
    let rows = stmt.query_map(params![safe_query], |row| {
        Self::row_to_node(row)
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}
```

#### `execute_batch`

```rust
fn execute_batch(&self, ops: &[StorageOp]) -> Result<(), StorageError> {
    let mut conn = self.conn.borrow_mut();
    // GOAL-1.15: atomic batch — all-or-nothing
    let tx = conn.transaction()?;
    for op in ops {
        match op {
            StorageOp::PutNode(node) => { /* INSERT OR REPLACE into nodes */ }
            StorageOp::DeleteNode(id) => { /* DELETE FROM nodes WHERE id = ? */ }
            StorageOp::AddEdge(edge) => { /* INSERT into edges */ }
            StorageOp::RemoveEdge { from, to, relation } => {
                /* DELETE FROM edges WHERE from_node = ? AND to_node = ? AND relation = ? */
            }
            StorageOp::SetTags(node_id, tags) => {
                /* DELETE FROM node_tags WHERE node_id = ?; INSERT for each tag */
            }
            StorageOp::SetMetadata(node_id, metadata) => {
                /* DELETE FROM node_metadata WHERE node_id = ?; INSERT for each key-value */
            }
            StorageOp::SetKnowledge(node_id, knowledge) => {
                /* INSERT OR REPLACE into knowledge with JSON-serialized fields */
            }
        }
    }
    tx.commit()?;
    Ok(())
}
```

#### `neighbors` (BFS via recursive CTE)

```rust
fn neighbors(&self, id: &str, depth: usize) -> Result<Vec<Node>, StorageError> {
    let conn = self.conn.borrow();
    // k-hop neighborhood via recursive CTE
    // depth=0 returns just the root node itself.
    // Maximum practical depth: 10 (prevents runaway CTEs on large graphs).
    let effective_depth = depth.min(10);
    let mut stmt = conn.prepare(
        "WITH RECURSIVE hop(nid, d) AS (
             VALUES(?1, 0)
           UNION
             SELECT CASE WHEN e.from_node = hop.nid THEN e.to_node
                         ELSE e.from_node END,
                    hop.d + 1
             FROM edges e
             JOIN hop ON (e.from_node = hop.nid OR e.to_node = hop.nid)
             WHERE hop.d < ?2
         )
         SELECT DISTINCT n.* FROM hop
         JOIN nodes n ON n.id = hop.nid"
    )?;
    let rows = stmt.query_map(params![id, effective_depth as i64], |row| {
        Self::row_to_node(row)
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}
```

**Note:** `neighbors` is NOT part of the `GraphStorage` trait — it is an inherent method on `SqliteStorage` used internally by the context pipeline (design-context.md). It is excluded from the trait because k-hop neighborhood queries are implementation-specific (recursive CTEs are a SQL feature) and not meaningful for all potential backends.

---

## 5. Indexes

```sql
-- GOAL-1.12: indexes on high-frequency query columns
CREATE INDEX idx_nodes_node_type ON nodes(node_type);
CREATE INDEX idx_nodes_status    ON nodes(status);
CREATE INDEX idx_nodes_file_path ON nodes(file_path);

-- GOAL-1.12: edge traversal in both directions
CREATE INDEX idx_edges_from      ON edges(from_node);
CREATE INDEX idx_edges_to        ON edges(to_node);
CREATE INDEX idx_edges_relation  ON edges(relation);

-- GOAL-1.12: tag and metadata queries
CREATE INDEX idx_tags_tag        ON node_tags(tag);
CREATE INDEX idx_metadata_key    ON node_metadata(key);

-- Composite index for common edge query pattern
CREATE INDEX idx_edges_from_to   ON edges(from_node, to_node);
```

**Rationale:**

| Index | GOAL | Query Pattern |
|---|---|---|
| `idx_nodes_node_type` | 1.12 | `WHERE node_type = ?` filtering |
| `idx_nodes_status` | 1.12 | `WHERE status = ?` filtering |
| `idx_nodes_file_path` | 1.12 | `WHERE file_path = ?` or `LIKE` prefix |
| `idx_edges_from` | 1.12 | `get_edges(node_id)`, neighbor traversal |
| `idx_edges_to` | 1.12 | Reverse traversal (callers) |
| `idx_edges_relation` | 1.12 | Edge-type filtering |
| `idx_tags_tag` | 1.12 | `get nodes with tag X` |
| `idx_metadata_key` | 1.12 | Metadata key lookup |
| `idx_edges_from_to` | 1.12 | BFS/shortest-path queries |

---

## 6. FTS5

### 6.1 Setup

The `nodes_fts` virtual table (§2.8) uses **content-sync** mode: it mirrors `id`, `title`, `description`, `signature`, and `doc_comment` from the `nodes` table but does not store a redundant copy of the content. The `content='nodes'` and `content_rowid='rowid'` directives tell FTS5 to read from `nodes` on demand. This matches GOAL-1.7 exactly.

### 6.2 Content-Sync Triggers

These triggers keep the FTS index in sync with the `nodes` table automatically (GOAL-1.7). FTS updates occur within the same transaction as the node modification.

```sql
-- After INSERT: add new content to FTS
CREATE TRIGGER nodes_fts_insert AFTER INSERT ON nodes BEGIN
    INSERT INTO nodes_fts(rowid, id, title, description, signature, doc_comment)
    VALUES (new.rowid, new.id, new.title, new.description, new.signature, new.doc_comment);
END;

-- After UPDATE: remove old content, add new content
CREATE TRIGGER nodes_fts_update AFTER UPDATE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, id, title, description, signature, doc_comment)
    VALUES ('delete', old.rowid, old.id, old.title, old.description, old.signature, old.doc_comment);
    INSERT INTO nodes_fts(rowid, id, title, description, signature, doc_comment)
    VALUES (new.rowid, new.id, new.title, new.description, new.signature, new.doc_comment);
END;

-- After DELETE: remove content from FTS
CREATE TRIGGER nodes_fts_delete AFTER DELETE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, id, title, description, signature, doc_comment)
    VALUES ('delete', old.rowid, old.id, old.title, old.description, old.signature, old.doc_comment);
END;
```

### 6.3 FTS5 Query Sanitization

The `search` method (§4.3) wraps user input in double-quotes for literal matching by default. This prevents FTS5 syntax injection — special characters (`*`, `"`, `OR`, `NOT`, `NEAR`) in user input are treated as literal text, not query operators. An advanced search API (future work) can expose raw FTS5 syntax for power users.

### 6.4 Search Syntax Reference

The `search_nodes` method (§4.3) uses FTS5's `MATCH` syntax and built-in `rank` function. Supported query syntax:

| Pattern | Example | Matches |
|---|---|---|
| Single term | `"migration"` | Nodes containing "migration" |
| Phrase | `"sqlite migration"` | Exact phrase match |
| AND (implicit) | `"sqlite storage"` | Both terms present |
| OR | `"sqlite OR postgres"` | Either term |
| NOT | `"sqlite NOT legacy"` | First term, excluding second |
| Prefix | `"migrat*"` | Prefix match |
| Column filter | `"title:storage"` | Match only in title column |

---

## 7. StorageOp Batch API

### 7.1 Enum Definition

The `StorageOp` enum matches the canonical definition from design.md §3 exactly.

```rust
/// GOAL-1.15: atomic batch operations (command pattern for object-safety).
/// Variants match design.md §3 — PutNode, DeleteNode, AddEdge, RemoveEdge,
/// SetTags, SetMetadata, SetKnowledge.
pub enum StorageOp {
    PutNode(Node),
    DeleteNode(String),                             // node_id
    AddEdge(Edge),
    RemoveEdge {
        from: String,
        to: String,
        relation: String,
    },
    SetTags(String, Vec<String>),                   // node_id, tags
    SetMetadata(String, HashMap<String, Value>),    // node_id, metadata
    SetKnowledge(String, KnowledgeNode),            // node_id, knowledge
}
```

### 7.2 `execute_batch` Method

See §4.3 for pseudocode. Key properties:

- **Atomicity (GOAL 1.8):** All operations run inside a single `transaction()`. If any operation fails, the entire batch is rolled back.
- **Ordering:** Operations execute in slice order — callers can depend on sequential consistency within a batch.
- **FTS sync:** Content-sync triggers fire within the transaction, so FTS stays consistent even on rollback.
- **Performance:** Batching N operations into one transaction avoids N separate `fsync` calls (GUARD 6: WAL mode amplifies this benefit).

---

## 8. Write Contention

**GOAL-1.17:** When a write operation encounters `SQLITE_BUSY` after the `busy_timeout` (5000ms per GOAL-1.11) expires, the operation fails with `StorageError::DatabaseLocked`. The error message includes "database is locked — another process is writing. Try again." No application-level automatic retry is performed.

This applies to all `GraphStorage` write methods (`put_node`, `add_edge`, `execute_batch`, `set_tags`, `set_metadata`, `set_knowledge`) and the maintenance methods.

**Mapping from rusqlite:**
```rust
// In SqliteStorage's error mapping:
rusqlite::Error::SqliteFailure { code: ErrorCode::DatabaseBusy, .. }
    => StorageError::DatabaseLocked
```

---

## 9. Backend Detection

**GOAL-1.14:** The `find_graph_file` function (or replacement) detects which storage backend is available:

| State | Behavior |
|-------|----------|
| `.gid/graph.db` exists (with or without `.gid/graph.yml`) | Use SQLite |
| Only `.gid/graph.yml` exists | Use YAML with migration prompt to stderr (GOAL-2.1) |
| Neither `.gid/graph.db` nor `.gid/graph.yml`, but `.gid/` exists | Error: "No graph found. Run `gid init`" |
| `.gid/` directory does not exist | Error: "No .gid/ directory. Run `gid init`" |
| `gid init` | Creates `.gid/graph.db` via `SqliteStorage::open()` (GOAL-1.10) |

---

## 10. Call-Site Migration Plan

**GOAL-1.13:** All existing call sites (~9 `load_graph` + ~12 `save_graph`) are updated to use `GraphStorage` methods.

**Before:**
```rust
let mut graph = load_graph(&path)?;  // deserialize entire YAML
graph.nodes.push(new_node);
save_graph(&path, &graph)?;          // serialize entire YAML
```

**After:**
```rust
let storage = find_storage(&gid_dir)?;  // returns Box<dyn GraphStorage>
storage.put_node(&new_node)?;           // single row INSERT
```

**Key call sites to migrate:**
- `parser.rs`: `load_graph` / `save_graph` — kept for YAML compat shim
- `history.rs`: snapshot/restore — uses SQLite backup API instead
- `harness/scheduler.rs`: task status updates — `put_node` instead of load-mutate-save
- `ritual/executor.rs`: phase transitions — `put_node` + `execute_batch`
- `query.rs`: `QueryEngine` — backed by `query_nodes`/`search` instead of in-memory filter

---

## 11. Observability

**GOAL-1.18:** All storage operations log errors at WARN level and successful writes at DEBUG level using the `tracing` crate.

```rust
// On failure:
tracing::warn!(operation = "put_node", node_id = %id, error = %e, "storage operation failed");

// On success:
tracing::debug!(operation = "put_node", node_id = %id, "node written");
```

---

## 12. Node Struct Extension

The current `Node` struct (graph.rs) has 10 fields. The SQLite schema has 21 dedicated columns. During implementation, the `Node` struct must be extended to include all dedicated columns:

### Fields to Add to `Node`

```rust
// Code-graph fields (populated by `gid extract`, NULL for task nodes)
pub file_path: Option<String>,
pub lang: Option<String>,
pub start_line: Option<usize>,
pub end_line: Option<usize>,
pub signature: Option<String>,
pub visibility: Option<String>,
pub doc_comment: Option<String>,
pub body_hash: Option<String>,
pub node_kind: Option<String>,

// Provenance fields
pub owner: Option<String>,
pub source: Option<String>,
pub repo: Option<String>,

// Timestamps
pub created_at: Option<String>,
pub updated_at: Option<String>,
```

### Migration Strategy

- All new fields are `Option<T>` with `#[serde(default)]` — **backward compatible** with existing YAML graphs
- `CodeNode` fields map to `Node` fields during `gid extract`: `CodeNode.file_path` → `Node.file_path`, `CodeNode.line` → `Node.start_line`, `CodeNode.kind` → `Node.node_kind`, `CodeNode.docstring` → `Node.doc_comment`, `CodeNode.signature` → `Node.signature`
- `Node.status` remains a `NodeStatus` enum; stored as TEXT in SQLite via `.to_string()` / `.parse()`
- `Node.tags` is stored in the `node_tags` table (not a column) — `put_node` handles the `nodes` table only; tags are managed via `set_tags`
- `Node.knowledge` is stored in the `knowledge` table — managed via `set_knowledge`/`get_knowledge`
- `Node.metadata` is stored in the `node_metadata` table — managed via `set_metadata`/`get_metadata`

### Impact on Existing Code

The `put_node` method writes all 21 columns. Reading a node (`get_node`, `row_to_node`) maps all 21 columns back. Fields not present in YAML-migrated data will be NULL/None.

The `gid extract` pipeline currently creates `CodeNode` objects. After this change, it should populate `Node` fields directly (or the extract pipeline converts `CodeNode` → `Node` with the code-graph fields filled in).

### Edge Struct Extension

The `Edge` struct needs one new field:

```rust
/// Additional edge metadata, serialized as JSON in SQLite.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub metadata: Option<serde_json::Value>,
```

This maps to the `edges.metadata TEXT` column. During YAML migration (GOAL-2.4), the `RawYamlEdge.extra` HashMap is serialized to JSON and stored here. If `extra` is empty, `metadata` is NULL.

---

## 13. GOAL Traceability

| GOAL | Description | Design Section |
|------|-------------|----------------|
| 1.1 | Single `nodes` table for all types | §2.1 |
| 1.2 | Dedicated columns for high-frequency fields | §2.1 (21 columns) |
| 1.3 | `node_metadata` KV table | §2.3 |
| 1.4 | `node_tags` many-to-many table | §2.4 |
| 1.5 | `edges` table with relation, weight, confidence | §2.2 |
| 1.6 | `knowledge` table (findings, file_cache, tool_history) | §2.5 |
| 1.7 | FTS5 on id, title, description, signature, doc_comment | §2.8, §6 |
| 1.8 | `change_log` audit trail | §2.7 |
| 1.9a | CRUD: get_node, put_node, delete_node, get_edges, add_edge, remove_edge | §3, §4.3 |
| 1.9b | query_nodes, search | §3, §4.3 |
| 1.9c | get_tags, set_tags, get_metadata, set_metadata | §3 |
| 1.9d | get_project_meta, set_project_meta, get_knowledge, set_knowledge | §3 |
| 1.9e | get_node_count, get_edge_count, get_all_node_ids | §3 |
| 1.10 | SqliteStorage implements GraphStorage | §4 |
| 1.11 | WAL + PRAGMAs on open | §4.2 |
| 1.12 | Indexes on query columns | §5 |
| 1.13 | Call-site migration plan | §10 |
| 1.14 | Backend detection logic | §9 |
| 1.15 | Batch operations via StorageOp | §7 |
| 1.16 | Config table (project_name, project_description, schema_version) | §2.6 |
| 1.17 | Write contention handling | §8 |
| 1.18 | Observability (tracing) | §11 |

### GUARD Traceability

| GUARD | Constraint | How Satisfied |
|-------|-----------|---------------|
| 1 | Single-writer safety | `RefCell<Connection>` — borrow-checked at runtime (§4.1) |
| 2 | No unsafe code | Pure safe Rust; `RefCell` not `UnsafeCell` (§4.1) |
| 6 | WAL mode for reads | `PRAGMA journal_mode = WAL` in constructor (§4.2) |
| 7 | Referential integrity | `PRAGMA foreign_keys = ON`; `REFERENCES` + `ON DELETE CASCADE` (§2) |
| 8 | Concurrent reads | WAL mode at SQLite level ensures readers never block (§4.2 note). `RefCell` handles Rust borrow checking within a single thread |
| 10 | Extensibility / object-safe | `GraphStorage` trait decouples interface from impl; `StorageOp` command pattern avoids `&mut self` (§3, §7) |
