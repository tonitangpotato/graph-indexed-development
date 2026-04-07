# Requirements: SQLite Storage Layer + GraphStorage Trait

## Feature Overview

The SQLite storage layer replaces the current load-entire-YAML-into-memory approach with a relational database that supports O(1) node lookups, indexed queries, full-text search, and atomic partial updates. A `GraphStorage` trait abstracts the storage backend so that the rest of the codebase operates through a uniform interface, enabling future alternative backends (e.g., remote/SaaS storage).

*Parent: [requirements.md](requirements.md) — see GUARDs there for cross-cutting constraints.*

## Goals

### Schema & Table Structure

- **GOAL-1.1** [P0]: A `nodes` table stores all node types (task, file, function, class, module, feature, component, layer, knowledge) in a single table, differentiated by a `node_type` column. Inserting a node with any of these types succeeds, and querying by `node_type` returns only nodes of that type. *(ref: discussion, Schema — unified nodes table)*

- **GOAL-1.2** [P0]: The `nodes` table has dedicated columns for high-frequency fields: `id` (TEXT PRIMARY KEY), `title` (TEXT), `status` (TEXT), `description` (TEXT), `node_type` (TEXT NOT NULL), `file_path` (TEXT), `lang` (TEXT), `start_line` (INTEGER), `end_line` (INTEGER), `signature` (TEXT), `visibility` (TEXT), `doc_comment` (TEXT), `body_hash` (TEXT), `node_kind` (TEXT), `owner` (TEXT), `source` (TEXT), `repo` (TEXT), `priority` (INTEGER), `assigned_to` (TEXT), `created_at` (TEXT), `updated_at` (TEXT). All fields except `id` and `node_type` are nullable. Domain constraints: `status` accepts known values (todo, in_progress, done, blocked, cancelled, failed, needs_resolution) but is not CHECK-constrained to allow future extension; `node_type` accepts values listed in GOAL-1.1 (task, file, function, class, module, feature, component, layer, knowledge) with no CHECK constraint; `priority` is INTEGER (0–255) matching the current codebase `Option<u8>` type; `visibility` accepts free-form TEXT (known values: public, private, crate). **Forward-looking columns** (`lang`, `start_line`, `end_line`, `visibility`, `body_hash`, `node_kind`, `owner`) are for future code-graph integration and will be NULL for all YAML-migrated data (GOAL-2.3). Semantic distinction: `node_type` is the high-level graph role (task, file, knowledge, etc.); `node_kind` is the code-level construct (Function, Struct, Impl, Trait, etc.) from `CodeNode.kind`. `start_line`/`end_line` map from `CodeNode.line` (start) with `end_line` populated only when code-graph range data is available. *(ref: discussion, Schema — high-frequency fields as columns)*

- **GOAL-1.3** [P0]: A `node_metadata` table stores key-value pairs per node (`node_id` TEXT FK, `key` TEXT, `value` TEXT, PRIMARY KEY (node_id, key)). Any dynamic field not in the `nodes` column list is stored here. Querying metadata by node_id + key returns the correct value. *(ref: discussion, Schema — low-frequency/dynamic fields in KV table)*

- **GOAL-1.4** [P0]: A `node_tags` table stores many-to-many tag associations (`node_id` TEXT FK, `tag` TEXT, PRIMARY KEY (node_id, tag)). Adding multiple tags to a node and querying all nodes with a given tag both return correct results. *(ref: discussion, Schema — many-to-many tags)*

- **GOAL-1.5** [P0]: An `edges` table stores relationships with columns: `id` (INTEGER PRIMARY KEY AUTOINCREMENT), `from_node` (TEXT FK NOT NULL), `to_node` (TEXT FK NOT NULL), `relation` (TEXT NOT NULL), `weight` (REAL DEFAULT 1.0), `confidence` (REAL), `metadata` (TEXT — JSON). The `confidence` column maps directly to the existing `Edge.confidence: Option<f64>` field used across the codebase (code_graph/build.rs, extract.rs, unified.rs, semantify.rs). Inserting an edge with a weight, confidence, and JSON metadata succeeds, and querying edges by from_node or to_node returns correct results. During migration (GOAL-2.4), `Edge.confidence` values are stored in this dedicated column, not in the JSON metadata blob. *(ref: discussion, Schema — edges with weight and metadata; GUARD-3 — no data loss)*

- **GOAL-1.6** [P1]: Knowledge data is stored using a JSON-blob approach in a `knowledge` table with columns: `node_id` (TEXT PRIMARY KEY, FK to nodes.id), `findings` (TEXT — JSON object mapping string keys to string values), `file_cache` (TEXT — JSON object mapping file paths to content strings), `tool_history` (TEXT — JSON array of objects with fields `tool_name` TEXT, `timestamp` TEXT, `summary` TEXT). This maps directly to the current `KnowledgeNode` struct: `findings: HashMap<String, String>`, `file_cache: HashMap<String, String>`, `tool_history: Vec<ToolCallRecord>`. Knowledge data associated with a node is retrievable by node ID. Inserting and retrieving each of the three knowledge fields round-trips correctly through JSON serialization. *(ref: discussion, Schema — knowledge nodes; KnowledgeNode struct in task_graph_knowledge.rs)*

- **GOAL-1.7** [P1]: A `nodes_fts` virtual table (FTS5) indexes the `id`, `title`, `description`, `signature`, and `doc_comment` fields from the `nodes` table using content-sync configuration (`content=nodes, content_rowid=rowid`). Triggers on INSERT, UPDATE, and DELETE on the `nodes` table keep the FTS index synchronized. FTS updates occur within the same transaction as the node modification. Full-text search queries against this table return matching nodes ranked by relevance. *(ref: discussion, Schema — FTS5 full-text search)*

- **GOAL-1.8** [P2]: A `change_log` table records an audit trail with columns: `id` (INTEGER PRIMARY KEY AUTOINCREMENT), `batch_id` (TEXT), `timestamp` (TEXT NOT NULL), `actor` (TEXT), `operation` (TEXT NOT NULL), `node_id` (TEXT), `field` (TEXT), `old_value` (TEXT), `new_value` (TEXT), `context` (TEXT). The `batch_id` column groups related changes within a single logical operation (e.g., a `put_node` updating title + status + description generates multiple rows sharing the same `batch_id`). Change logging can be enabled or disabled via configuration. *(ref: discussion, Schema — change_log, enterprise default on)*

### GraphStorage Trait

- **GOAL-1.9** [P0]: A `GraphStorage` trait defines the abstract interface for all storage operations. The trait is **synchronous** with all methods returning `Result<T, StorageError>`, and all methods take `&self` (not `&mut self`) to allow concurrent read access per GUARD-8. The trait is object-safe per GUARD-10. Methods are organized as follows:

  - **GOAL-1.9a** [P0] — CRUD operations:
    - `get_node(&self, id: &str) -> Result<Option<Node>>`
    - `put_node(&self, node: &Node) -> Result<()>`
    - `delete_node(&self, id: &str) -> Result<()>`
    - `get_edges(&self, node_id: &str) -> Result<Vec<Edge>>`
    - `add_edge(&self, edge: &Edge) -> Result<()>`
    - `remove_edge(&self, from: &str, to: &str, relation: &str) -> Result<()>`

  - **GOAL-1.9b** [P1] — Query and search:
    - `query_nodes(&self, filter: &NodeFilter) -> Result<Vec<Node>>` — `NodeFilter` is a struct with optional fields (`node_type`, `status`, `file_path`, `tag`, `limit`, `offset`)
    - `search(&self, query: &str) -> Result<Vec<Node>>` — full-text search via FTS5

  - **GOAL-1.9c** [P0] — Tag and node-metadata accessors:
    - `get_tags(&self, node_id: &str) -> Result<Vec<String>>`
    - `set_tags(&self, node_id: &str, tags: &[String]) -> Result<()>`
    - `get_metadata(&self, node_id: &str) -> Result<HashMap<String, Value>>` — operates on the `node_metadata` table (not edge metadata or project config)
    - `set_metadata(&self, node_id: &str, metadata: &HashMap<String, Value>) -> Result<()>`

  - **GOAL-1.9d** [P0] — Project and knowledge accessors:
    - `get_project_meta(&self) -> Result<Option<ProjectMeta>>`
    - `set_project_meta(&self, meta: &ProjectMeta) -> Result<()>`
    - `get_knowledge(&self, node_id: &str) -> Result<Option<KnowledgeNode>>`
    - `set_knowledge(&self, node_id: &str, knowledge: &KnowledgeNode) -> Result<()>`

  - **GOAL-1.9e** [P1] — Enumeration and counts:
    - `get_node_count(&self) -> Result<usize>`
    - `get_edge_count(&self) -> Result<usize>`
    - `get_all_node_ids(&self) -> Result<Vec<String>>` — for iteration without full deserialization

  All existing `gid` commands can be expressed through these methods without loading the entire graph. *(ref: discussion, GraphStorage Trait — core methods)*

- **GOAL-1.10** [P0]: A `SqliteStorage` struct implements `GraphStorage` and operates on a `.gid/graph.db` file. Opening a `SqliteStorage` against a path that has no existing database creates the database with all tables and indexes. *(ref: discussion, GraphStorage Trait — SqliteStorage)*

- **GOAL-1.11** [P0]: `SqliteStorage` configures the database connection with WAL journal mode, `synchronous=NORMAL`, `foreign_keys=ON`, and `busy_timeout=5000` on every connection open. These settings are verifiable by querying PRAGMAs after opening. Additional PRAGMAs (`cache_size`, `mmap_size`, `temp_store`) may be tuned by implementation for performance but are not required. *(ref: discussion, Transaction Semantics — WAL mode + PRAGMAs)*

### Indexes & Query Performance

- **GOAL-1.12** [P1]: The schema includes indexes on: `nodes.node_type`, `nodes.file_path`, `nodes.status`, `edges.from_node`, `edges.to_node`, `edges.relation`, `node_tags.tag`, `node_metadata.key`. Queries filtering by these columns use the index (verifiable via EXPLAIN QUERY PLAN). *(ref: discussion, Schema — performance at scale)*

### Integration with Existing Call Sites

- **GOAL-1.13** [P0]: All existing call sites (~9 `load_graph` + ~12 `save_graph` across gid-core: parser, history, harness/scheduler, ritual/executor) are updated to use `GraphStorage` methods instead of loading/saving entire YAML. Each call site performs only the reads/writes it needs (e.g., `get_node` + `put_node` instead of load-modify-save). *(ref: discussion, Transaction Semantics — no more read-entire/write-entire)*

- **GOAL-1.14** [P0]: The `find_graph_file` function (or its replacement) detects which storage backend is available by checking all possible states:
  1. `.gid/graph.db` exists (with or without `.gid/graph.yml`) → use SQLite (per GOAL-2.2, SQLite takes precedence regardless of YAML presence)
  2. Only `.gid/graph.yml` exists → use YAML with a migration prompt (per GUARD-5)
  3. Neither `.gid/graph.db` nor `.gid/graph.yml` exists, but `.gid/` directory exists → error with guidance to run `gid init`
  4. `.gid/` directory does not exist → error with guidance to run `gid init`
  5. `gid init` creates a new `.gid/graph.db` via `SqliteStorage::open()` (per GOAL-1.10)
  *(ref: discussion, Backward Compatibility — read path preference; GOAL-2.2)*

- **GOAL-1.15** [P1]: Batch operations (such as `gid extract` writing many nodes/edges, or `gid design --parse` creating multiple nodes) execute within an explicit transaction so that either all nodes/edges are committed or none are. To maintain object-safety (GUARD-10), the `GraphStorage` trait exposes batch operations via the command pattern: `execute_batch(&self, ops: &[StorageOp]) -> Result<()>`, where `StorageOp` is an enum of storage operations (PutNode, DeleteNode, AddEdge, RemoveEdge, etc.). The implementation wraps all operations in a single database transaction. *(ref: discussion, Transaction Semantics — batch operations: explicit transactions; GUARD-10 object-safety)*

### Project & Configuration Storage

- **GOAL-1.16** [P0]: A `config` table stores project-level metadata and schema information with columns: `key` (TEXT PRIMARY KEY), `value` (TEXT). Required rows: `project_name`, `project_description` (mapping from `ProjectMeta.name` and `ProjectMeta.description`), and `schema_version` (initially "1"). The `get_project_meta`/`set_project_meta` methods in GOAL-1.9d read from and write to this table. *(ref: GUARD-3 — no data loss; GOAL-2.6 — project metadata preserved)*

### Write Contention

- **GOAL-1.17** [P0]: When a write operation encounters `SQLITE_BUSY` after the `busy_timeout` (GOAL-1.11, 5000ms) expires, the operation fails with a descriptive `StorageError` including the message "database is locked" and suggesting retry. No application-level automatic retry is performed. This applies to all `GraphStorage` write methods and `execute_batch`. *(ref: GUARD-8 — concurrency; GOAL-1.11 — busy_timeout)*

### Observability

- **GOAL-1.18** [P1]: All storage operations that fail log the error at WARN level including: the operation name, the affected node/edge ID (if applicable), and the underlying SQLite error message. Successful write operations log at DEBUG level. This provides operational observability independent of the audit-trail change_log (GOAL-1.8). *(ref: operational observability for storage layer)*

**21 GOALs** (13 P0 / 6 P1 / 1 P2 + sub-GOALs 1.9a–1.9e)
