# Review: requirements.md + requirements-storage.md (SQLite Migration)

**Reviewed**: 2026-04-06  
**Documents**: `.gid/features/sqlite-migration/requirements.md`, `.gid/features/sqlite-migration/requirements-storage.md`  
**Total**: 15 GOALs (storage) + 10 GUARDs (master) reviewed exhaustively  
**Related docs scanned for cross-references**: requirements-migration.md, requirements-history.md, requirements-context.md  

---

## 🔴 Critical (blocks implementation)

### FINDING-1 — [Check #2, #7] GOAL-1.5: Edge `confidence` field missing from schema

The current `Edge` struct in `graph.rs:191-193` has a `confidence: Option<f64>` field that is actively used across the codebase (code_graph/build.rs, code_graph/extract.rs, unified.rs, semantify.rs — 50+ references). GOAL-1.5 defines the `edges` table with `weight` and `metadata` (JSON) columns but **no `confidence` column**.

During migration (GOAL-2.4), edge `confidence` values would be silently dropped unless stored in the JSON `metadata` blob — but nothing specifies this. This violates **GUARD-3** (no data loss during migration).

**Suggested fix**: Either:
(a) Add `confidence (REAL)` as a dedicated column to the `edges` table in GOAL-1.5, or
(b) Explicitly state in GOAL-1.5 and GOAL-2.4 that `confidence` is stored in the `metadata` JSON field with key `"confidence"`, and that `GraphStorage::get_edges` must deserialize it back.

---

### FINDING-2 — [Check #6, #8] No `ProjectMeta` storage in SQLite schema

The current `Graph` struct has a `project: Option<ProjectMeta>` field with `name` and `description`. GOAL-2.6 explicitly requires validating that "project metadata (name, description) is preserved" during migration. But **no table or storage mechanism** for `ProjectMeta` is defined in requirements-storage.md.

The storage-layer requirements define: `nodes`, `node_metadata`, `node_tags`, `edges`, `knowledge`, `nodes_fts`, `change_log`. None of these hold project-level metadata.

Without this, GOAL-2.6 is unimplementable and GUARD-3 is violated.

**Suggested fix**: Add a GOAL (e.g., GOAL-1.16) defining a `project_metadata` table (or a generic `config` table with key-value rows) to store project name, description, and schema version. Example:
```sql
CREATE TABLE config (
    key TEXT PRIMARY KEY,
    value TEXT
);
-- rows: project_name, project_description, schema_version
```

---

### FINDING-3 — [Check #1, #5] GOAL-1.6: Knowledge table schema is critically underspecified

GOAL-1.6 says: *"A `knowledge` table stores knowledge entries with columns for node-level knowledge data (findings, file_cache, tool_history). Knowledge data associated with a node is retrievable by node ID."*

This is insufficient for implementation. The current `KnowledgeNode` struct (task_graph_knowledge.rs:21-32) contains three distinct structures:
- `findings`: `HashMap<String, String>` — key-value pairs
- `file_cache`: `HashMap<String, String>` — file path → content
- `tool_history`: `Vec<ToolCallRecord>` — list of `{tool_name, timestamp, summary}`

Two engineers would implement this differently (one table? three tables? JSON blob?). No column names, types, or primary keys are specified — contrast with the precise schemas in GOAL-1.1 through GOAL-1.5.

**Suggested fix**: Specify concrete columns. Likely needs multiple tables or a JSON column approach:
```
Option A (normalized):
  knowledge_findings (node_id TEXT FK, key TEXT, value TEXT, PK(node_id, key))
  knowledge_file_cache (node_id TEXT FK, file_path TEXT, content TEXT, PK(node_id, file_path))
  knowledge_tool_history (id INTEGER PK, node_id TEXT FK, tool_name TEXT, timestamp TEXT, summary TEXT)

Option B (JSON blob):
  knowledge (node_id TEXT PK FK, findings TEXT JSON, file_cache TEXT JSON, tool_history TEXT JSON)
```

---

### FINDING-4 — [Check #1, #16] GOAL-1.9: GraphStorage trait method signatures unspecified

GOAL-1.9 lists method names (`get_node`, `put_node`, `delete_node`, etc.) but does not specify:
- **Return types** — Does `get_node` return `Option<Node>`? `Result<Option<Node>>`? A storage-specific `StorageNode`?
- **Parameter types** — Does `query_nodes` take a filter struct? A closure? A SQL-like DSL?
- **Error types** — Is there a `StorageError` enum? Or is it `anyhow::Result`?
- **Async vs sync** — GUARD-10 says "object-safe" which constrains async (no `async fn` in object-safe traits in stable Rust without workarounds). Is it sync? Is it `async`? This fundamentally changes the implementation.

Two engineers would produce incompatible trait definitions from this requirement.

**Suggested fix**: Specify at minimum:
- Sync or async (given GUARD-10 object-safety, likely sync with `Result<T, StorageError>`)
- Return types for each method (e.g., `get_node(&self, id: &str) -> Result<Option<Node>>`)
- The `query_nodes` filter type (struct with optional fields? builder pattern?)
- Whether methods take `&self` or `&mut self` (important for concurrency per GUARD-8)

---

### FINDING-5 — [Check #15] GUARD-10 vs GOAL-1.9/GOAL-1.15: Object-safety tension with `transaction`/`batch`

GUARD-10 requires `GraphStorage` to be object-safe. GOAL-1.15 requires a `transaction` or `batch` method.

In Rust, object-safe traits cannot have methods that return `impl Trait` or take `Self` by value. A `transaction` method that takes a closure (`fn transaction<F: FnOnce(&mut Tx) -> Result<()>>(&self, f: F)`) is **not object-safe** because it's generic. A method returning a `Transaction` guard type may work but constrains the API.

This tension is not acknowledged or resolved in the requirements. An implementer would hit this immediately.

**Suggested fix**: Specify the transaction API pattern explicitly. Options:
(a) `begin_transaction(&self) -> Result<Box<dyn Transaction>>` (object-safe, returns trait object)
(b) `execute_batch(&self, ops: Vec<StorageOp>) -> Result<()>` (object-safe, command pattern)
(c) Acknowledge that `transaction` is a `SqliteStorage`-specific method not on the trait

---

## 🟡 Important (should fix before implementation)

### FINDING-6 — [Check #1, #9] GOAL-1.2: Multiple columns have no domain specification for valid values

GOAL-1.2 lists 21 columns. Several TEXT columns lack value constraints:
- `status` — The current codebase has 7 specific statuses (todo, in_progress, done, blocked, cancelled, failed, needs_resolution). Should this be a CHECK constraint? What happens if an invalid status is inserted?
- `node_type` — GOAL-1.1 lists 9 valid types. Is there a CHECK constraint? What about future types?
- `visibility` — Not present in current codebase at all. What are valid values? (public, private, protected, crate, pub(crate)?)
- `priority` — In current code this is `Option<u8>` (0-255 integer). The schema says TEXT. Type mismatch.

**Suggested fix**: For each constrained field, either (a) define a CHECK constraint with allowed values, or (b) explicitly state "free-form TEXT, no validation" and document known values. Fix `priority` type to match current code (INTEGER, not TEXT) or document the intentional change.

---

### FINDING-7 — [Check #2, #9] GOAL-1.2: Schema includes 7 columns with no current-codebase counterpart

These columns appear in GOAL-1.2 but do **not** exist in the current `Node` struct or `CodeNode` struct:
- `lang` — not on Node; CodeNode doesn't have it (language is inferred from file extension)
- `start_line` — not on Node; CodeNode has `line: Option<usize>` (single value, not start/end)
- `end_line` — not on Node or CodeNode
- `visibility` — not on Node or CodeNode
- `body_hash` — not on Node or CodeNode
- `node_kind` — not on Node or CodeNode (CodeNode has `kind: NodeKind` but Node has `node_type`)
- `owner` — not on Node or CodeNode

These appear to be forward-looking columns for code graph integration. This is fine, but:
1. Migration (GOAL-2.3) needs to know these are always NULL for migrated data
2. The distinction between `node_type` and `node_kind` is confusing — what's the semantic difference?
3. `start_line` vs CodeNode's `line` — is `line` mapped to `start_line`? Where does `end_line` come from?

**Suggested fix**: Add a note to GOAL-1.2 documenting which columns are for future code-graph merging and will be NULL after YAML migration. Clarify `node_type` vs `node_kind` semantics (e.g., node_type = "function" from task graph, node_kind = "Function" from CodeNode.kind?). Clarify `start_line`/`end_line` mapping from CodeNode.`line`.

---

### FINDING-8 — [Check #3] GOAL-1.13: Call-site count is inaccurate

GOAL-1.13 says "~30 `load_graph` + ~15 `save_graph`". Actual counts from the codebase:
- `load_graph`: 9 references across gid-core (parser definition + re-export + history + ritual/executor)
- `save_graph`: 12 references across gid-core (parser definition + re-export + history + scheduler + ritual/executor)
- **Total: ~21 call sites**, not ~45

The count is inaccurate and affects implementation effort estimation.

**Suggested fix**: Update to reflect actual counts: "~9 `load_graph` + ~12 `save_graph` across gid-core (parser, history, harness/scheduler, ritual/executor)". Consider generating this list programmatically as a migration checklist.

---

### FINDING-9 — [Check #5, #7] GOAL-1.9: Missing `GraphStorage` methods for `ProjectMeta` and `KnowledgeNode`

GOAL-1.9 lists: `get_node`, `put_node`, `delete_node`, `get_edges`, `add_edge`, `remove_edge`, `query_nodes`, `search`, `get_tags`, `set_tags`, `get_metadata`, `set_metadata`.

Missing methods for data that exists in the current codebase:
- **ProjectMeta**: No `get_project`/`set_project` method (see FINDING-2)
- **Knowledge**: No `get_knowledge`/`set_knowledge` methods — but GOAL-1.6 defines a knowledge table, and the current codebase has `KnowledgeGraph` and `KnowledgeManagement` traits with 6+ methods
- **Bulk operations**: No `get_all_nodes`/`get_all_edges` — needed for `gid show --full`, `gid export`, history diff

**Suggested fix**: Add methods to the trait list: `get_project_meta`, `set_project_meta`, `get_knowledge`, `set_knowledge`, `get_node_count`, `get_edge_count`, and potentially `get_all_node_ids` for iteration without full deserialization.

---

### FINDING-10 — [Check #11] GUARD-8 vs GOAL-1.9 `put_node`/`add_edge`: Writer concurrency semantics unclear

GUARD-8 says: "Concurrent read operations never block each other. A single writer does not block readers."

SQLite WAL mode achieves this, but the requirements don't specify:
- What happens when two processes try to write simultaneously? (SQLite returns SQLITE_BUSY after busy_timeout)
- Should `GraphStorage` retry on SQLITE_BUSY? How many times?
- GOAL-1.11 sets `busy_timeout=5000` (5 seconds) — what happens after 5 seconds? Error? Retry?
- Is there a locking protocol at the application level (e.g., file locks)?

This matters because `gid` commands are invoked by AI agents that may run concurrently.

**Suggested fix**: Add a GOAL or GUARD specifying write-contention behavior: "When a write operation encounters SQLITE_BUSY after the busy_timeout expires, the operation fails with a descriptive error message including 'database is locked' and suggesting retry. No application-level retry is performed."

---

### FINDING-11 — [Check #10] GOAL-1.14: State transitions incomplete for storage detection

GOAL-1.14 defines three states:
1. `.gid/graph.db` exists → use SQLite
2. Only `.gid/graph.yml` exists → use YAML + migration prompt
3. Neither exists → error

Missing states:
4. **Both `.gid/graph.db` AND `.gid/graph.yml` exist** — GOAL-2.2 says "uses SQLite regardless of whether .gid/graph.yml also exists", but GOAL-1.14 doesn't mention this case
5. **`.gid/` directory doesn't exist** — different from "neither file exists in .gid/"
6. **`gid init` scenario** — how does a fresh project create its first SQLite database? Does `gid init` call `SqliteStorage::open()` which creates the DB (GOAL-1.10)?

**Suggested fix**: Expand GOAL-1.14 to explicitly list all 5-6 states, or reference GOAL-2.2 for the "both exist" case. Add a note about the `gid init` path.

---

### FINDING-12 — [Check #12] Terminology inconsistency: `metadata` overloaded

The term "metadata" is used for three distinct concepts:
1. **Node metadata** — the `node_metadata` KV table (GOAL-1.3) / `HashMap<String, serde_json::Value>` on Node
2. **Edge metadata** — the `metadata` JSON column on edges (GOAL-1.5)
3. **Project metadata** — ProjectMeta (name, description) referenced in GOAL-2.6

Additionally, `get_metadata`/`set_metadata` in GOAL-1.9 presumably refers to #1, but could be confused with #2 or #3.

**Suggested fix**: Use distinct names: "node attributes" or "node properties" for #1, "edge metadata" for #2, "project config" for #3. At minimum, clarify in GOAL-1.9 that `get_metadata`/`set_metadata` operate on the `node_metadata` table.

---

### FINDING-13 — [Check #4] GOAL-1.9: Compound requirement — 12 methods in one GOAL

GOAL-1.9 defines the entire `GraphStorage` trait (12 methods) as a single requirement. This is not atomic — individual methods could have different acceptance criteria, different edge cases, and different priorities. If `search` (full-text) is P1 but `get_node` is P0, they shouldn't be in the same GOAL.

**Suggested fix**: Either split into sub-GOALs (GOAL-1.9a: CRUD methods, GOAL-1.9b: query/search, GOAL-1.9c: tag/metadata accessors) or at minimum list which methods are P0 vs P1. Given the 15-GOAL limit for this doc, sub-lettering is preferable.

---

### FINDING-14 — [Check #8] No observability requirements for SQLite storage layer

The master requirements have no observability GUARDs or GOALs. For a storage layer:
- No logging requirements (what operations are logged? At what level?)
- No metrics (query latency, cache hit rate, transaction count)
- GOAL-1.8 (change_log) is P2 and is for audit trail, not operational observability
- No error reporting format/structure

**Suggested fix**: Add at minimum a soft GUARD: "All storage operations that fail log the error at WARN level including the operation name, affected node/edge ID, and the SQLite error message." Consider a P1 GOAL for structured error types.

---

### FINDING-15 — [Check #18] GOAL-1.7: FTS5 synchronization mechanism not specified

GOAL-1.7 defines a `nodes_fts` virtual table indexing node fields. But it doesn't specify **how** FTS stays synchronized with the `nodes` table:
- SQLite FTS5 content tables can use `content=nodes` for automatic sync, but this has specific limitations
- Are triggers used? Manual insert/update/delete in the FTS table?
- What happens when a node is updated — is the FTS entry updated atomically?
- Performance impact of FTS maintenance on writes?

Two engineers would implement this differently (content sync vs. manual triggers vs. external rebuild).

**Suggested fix**: Specify: "The `nodes_fts` table is configured as a content-sync FTS5 table (`content=nodes, content_rowid=rowid`) with triggers to keep it updated on INSERT, UPDATE, and DELETE operations on the `nodes` table. FTS updates occur within the same transaction as the node modification."

---

## 🔵 Minor (nice to fix)

### FINDING-16 — [Check #13] GOAL-1.12: Missing index on `node_metadata.key`

GOAL-1.12 lists indexes on `nodes.node_type`, `nodes.file_path`, `nodes.status`, `edges.from_node`, `edges.to_node`, `edges.relation`, `node_tags.tag`. But `node_metadata.key` is not indexed, meaning queries like "find all nodes with metadata key X" require a full table scan.

**Suggested fix**: Add `node_metadata.key` to the index list in GOAL-1.12.

---

### FINDING-17 — [Check #14] GOAL-1.8: `change_log` lacks structured diff support

GOAL-1.8 has `old_value` and `new_value` TEXT columns, which works for single-field changes. But for operations that change multiple fields at once (e.g., `put_node` updating title + status + description), this creates multiple rows with the same timestamp and context. There's no grouping mechanism (e.g., a `batch_id` or `transaction_id` column).

**Suggested fix**: Add a `batch_id` (TEXT or INTEGER) column to `change_log` in GOAL-1.8 to group related changes within a single logical operation.

---

### FINDING-18 — [Check #17] GUARD-7: 3× size ratio has no empirical basis

GUARD-7 says SQLite file size should not exceed 3× YAML size. SQLite with FTS5, indexes, WAL file, and the change_log table can easily exceed this for small graphs (overhead is relatively larger). For a 100-node graph with a 50KB YAML, 150KB is tight with FTS indexes.

**Suggested fix**: Either increase to 5× or change to "for graphs with 1000+ nodes, does not exceed 3×" to account for fixed overhead on small graphs. Alternatively, state that WAL file size is excluded from the measurement.

---

### FINDING-19 — [Check #19] GOAL-1.11: PRAGMA list incomplete

GOAL-1.11 specifies: WAL journal mode, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5000`. Missing PRAGMAs that are commonly recommended for performance:
- `cache_size` — default is 2MB; for large graphs, increasing this improves read performance
- `mmap_size` — memory-mapped I/O can significantly improve read-heavy workloads
- `temp_store=MEMORY` — keeps temporary tables in memory

**Suggested fix**: Either add recommended PRAGMAs to GOAL-1.11 or add a note: "Additional PRAGMAs (cache_size, mmap_size, temp_store) may be tuned by implementation but are not required."

---

### FINDING-20 — [Check #20] Master requirements.md: GOAL count is stale

The master requirements.md footer says "**43 GOALs** (27 P0 / 13 P1 / 3 P2) + **10 GUARDs** (6 hard / 4 soft)". After applying findings from this review (adding GOAL-1.16 for config table, splitting GOAL-1.9, etc.), this count will be outdated. The count should either be removed or marked as auto-generated.

**Suggested fix**: Update the count after all changes are applied, or change to "See individual requirements documents for GOAL counts."

---

## Summary

| Severity | Count | Findings |
|----------|-------|----------|
| 🔴 Critical | 5 | FINDING-1 through FINDING-5 |
| 🟡 Important | 10 | FINDING-6 through FINDING-15 |
| 🔵 Minor | 5 | FINDING-16 through FINDING-20 |
| **Total** | **20** | |

### Top 3 Risks
1. **Data loss** — Edge confidence and ProjectMeta have no storage (FINDING-1, FINDING-2)
2. **Ambiguous trait** — GraphStorage trait is un-implementable from spec alone (FINDING-4, FINDING-5)
3. **Knowledge schema gap** — Knowledge table can't be built from GOAL-1.6 (FINDING-3)

---

## ✅ All Findings Applied (2026-04-06)

All 16 findings (FINDING-1 through FINDING-16) have been applied to `requirements-storage.md` and `requirements.md`.
