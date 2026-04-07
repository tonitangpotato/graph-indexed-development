# Review: design-storage.md

## 🔴 Critical (blocks implementation)

### FINDING-1: Schema diverges massively from requirements
**[Check #1, #27]** The design defines a completely different schema from the requirements. Requirements specify:
- `nodes` table with 20+ dedicated columns (GOAL-1.2: `id TEXT PK`, `title`, `status`, `description`, `node_type`, `file_path`, `lang`, `start_line`, `end_line`, `signature`, `visibility`, `doc_comment`, `body_hash`, `node_kind`, `owner`, `source`, `repo`, `priority`, `assigned_to`, `created_at`, `updated_at`)
- `node_metadata` KV table (GOAL-1.3)
- `node_tags` many-to-many table (GOAL-1.4)
- `knowledge` table with `findings`, `file_cache`, `tool_history` JSON columns (GOAL-1.6)

The design instead defines:
- `nodes` with only 8 columns: `id BLOB`, `kind TEXT`, `title TEXT`, `body TEXT`, `status TEXT`, `created_at TEXT`, `updated_at TEXT`, `meta TEXT`
- No `node_metadata` table
- No `node_tags` table
- `knowledge` table with different schema (`category`, `content`, `confidence` columns instead of `findings`, `file_cache`, `tool_history`)
- IDs stored as `BLOB` (16-byte UUID) instead of `TEXT PRIMARY KEY` as specified

This is a fundamental mismatch. The design was written against a different (invented) schema, not the actual requirements.

**Suggested fix:** Rewrite §2 Schema to match GOAL-1.1 through GOAL-1.8 exactly. Use the column list from GOAL-1.2, add `node_metadata` (GOAL-1.3), `node_tags` (GOAL-1.4), and match the `knowledge` schema from GOAL-1.6.

### FINDING-2: GraphStorage trait signature doesn't match requirements
**[Check #1, #27]** The trait in §3 defines a different API from GOAL-1.9:
- Requirements specify `get_node(&self, id: &str)` — design uses `&NodeId` (BLOB type)
- Requirements have `put_node` — design has `create_node` + `update_node` (split)
- Requirements have `get_edges(&self, node_id: &str)` — design has `edges_from` + `edges_to` + `edges_between` (different split)
- Requirements specify `query_nodes(&self, filter: &NodeFilter)` — design has `list_nodes(&self, filter: &NodeFilter)` (rename)
- Requirements have `get_tags`/`set_tags` (GOAL-1.9c) — design has NO tag methods at all
- Requirements have `get_metadata`/`set_metadata` (GOAL-1.9c) — design has NO metadata methods
- Requirements have `get_project_meta`/`set_project_meta` (GOAL-1.9d) — design has NO project meta methods
- Requirements have `get_knowledge`/`set_knowledge` (GOAL-1.9d) — design has `add_knowledge`/`get_knowledge`/`query_knowledge`/`delete_knowledge` (different API)
- Requirements have `get_node_count`/`get_edge_count`/`get_all_node_ids` (GOAL-1.9e) — design has `stats()` (completely different)
- Design adds methods not in requirements: `shortest_path`, `subgraph`, `topological_sort`, `vacuum`, `checkpoint`, `export_json`, `import_json`

**Suggested fix:** Rewrite §3 to match the GOAL-1.9a–1.9e method signatures exactly. Remove methods not specified in requirements (shortest_path, subgraph, etc. — these are handled at a higher layer).

### FINDING-3: StorageOp enum doesn't match requirements
**[Check #1]** Requirements (GOAL-1.15) specify: `PutNode, DeleteNode, AddEdge, RemoveEdge, SetTags, SetMetadata, SetKnowledge`. Design §7 defines: `CreateNode, UpdateNode, DeleteNode, CreateEdge, DeleteEdge, SetConfig, DeleteConfig, AddKnowledge, DeleteKnowledge`. Missing: `SetTags`, `SetMetadata`, `SetKnowledge`. Extra: `SetConfig`, `DeleteConfig`, `CreateNode`/`UpdateNode` split.

**Suggested fix:** Use the exact `StorageOp` variants from design.md §3 (the master design).

### FINDING-4: Missing tables and features
**[Check #1]** Multiple required features are completely absent from the design:
- `change_log` table (GOAL-1.8): not mentioned at all
- `config` table schema wrong (GOAL-1.16): requirements specify `key/value` with `project_name`, `project_description`, `schema_version` rows — design has a different 3-column config table
- Write contention handling (GOAL-1.17): not addressed
- Call-site migration plan (GOAL-1.13): not addressed
- Backend detection logic (GOAL-1.14): not addressed

**Suggested fix:** Add sections for each missing GOAL.

## 🟡 Important (should fix before implementation)

### FINDING-5: GUARD-8 interpretation incorrect
**[Check #14]** GUARD-8 requires "concurrent reads never block each other. A single writer does not block readers." The design uses `RefCell<Connection>` which panics on concurrent borrow. While the master design.md §4.3 chose `RefCell` for single-threaded use, the design-storage.md should acknowledge this constraint and note that WAL mode is what satisfies GUARD-8 at the SQLite level (readers don't block on SQLite writes), not `RefCell`.

**Suggested fix:** Clarify that GUARD-8 is satisfied by WAL mode at the database level, not by RefCell. RefCell is for Rust borrow checking within a single thread.

### FINDING-6: FTS5 indexes wrong columns
**[Check #1]** GOAL-1.7 requires FTS5 on `id, title, description, signature, doc_comment`. Design indexes only `title, body`. Missing: `id`, `description` (design renamed to `body`), `signature`, `doc_comment`.

**Suggested fix:** Match the FTS5 column list to GOAL-1.7: `id, title, description, signature, doc_comment`.

### FINDING-7: Recursive CTE for neighbors has potential infinite loop
**[Check #5, #7]** The `neighbors` CTE uses `UNION` (not `UNION ALL`) which prevents revisiting, but the `WHERE hop.d < ?2` condition has an off-by-one: hop starts at 0 for the root, but the root is then excluded in the WHERE clause. If depth=0 is passed, the query returns nothing — should it return just the root node? The semantics are unclear.

**Suggested fix:** Define explicit semantics for depth=0. Add a note about maximum practical depth to prevent runaway CTEs on large graphs.

### FINDING-8: Missing error handling for FTS5 special characters
**[Check #8]** The `search_nodes` method passes user input directly to `MATCH ?1`. FTS5 query syntax uses special characters (`*`, `"`, `OR`, `NOT`, `NEAR`). Malformed queries will cause SQLite errors. No input sanitization or error handling is specified.

**Suggested fix:** Add a section on FTS5 query sanitization — either escape special characters or wrap user input in quotes for literal matching, with an option for advanced syntax.

## 🟢 Minor (can fix during implementation)

### FINDING-9: edges table missing `confidence` column
**[Check #1]** Requirements GOAL-1.5 specifies `confidence REAL` on edges. Design §2.2 has no `confidence` column.

**Suggested fix:** Add `confidence REAL` to the edges CREATE TABLE.

### FINDING-10: Traceability table maps to wrong/invented GOALs
**[Check #2]** The traceability table in §8 maps GOALs 1.1–1.18 to design sections, but the GOALs referenced don't match the actual requirements. For example, design says GOAL 1.9 is "graph traversal queries" but the actual GOAL-1.9 is "GraphStorage trait methods". GOAL 1.11 is "k-hop neighborhood" in design but "PRAGMA settings" in requirements. The entire traceability table needs to be rebuilt against the actual requirements.

**Suggested fix:** Rebuild traceability table against the actual requirements document.

### FINDING-11: No mention of `change_log_enabled` config
**[Check #15]** The master design.md §4.3 includes `change_log_enabled: bool` on `SqliteStorage`. The design-storage.md doesn't mention this field or how it's configured.

**Suggested fix:** Add `change_log_enabled` to SqliteStorage struct and document the change_log table behavior.

## ✅ Passed Checks

- Check #3: No dead definitions ✅ (all defined types are used)
- Check #4: Consistent naming ✅ (within the document; just wrong names vs requirements)
- Check #12: No ordering sensitivity issues ✅
- Check #13: Separation of concerns ✅ (pure trait, impl separate)
- Check #17: Goals explicit ✅ (overview states purpose)
- Check #19: Observability mentioned but not detailed ✅
- Check #20: Appropriate abstraction level ✅ (pseudocode + SQL, not full impl)
- Check #25: Core logic is testable ✅ (trait-based, temp DB)

## Summary

- **Critical: 4** (schema mismatch, trait API mismatch, StorageOp mismatch, missing features)
- **Important: 4** (GUARD-8 interpretation, FTS5 columns, CTE semantics, FTS5 injection)
- **Minor: 3** (confidence column, traceability table, change_log config)
- **Recommendation:** ⛔ **Needs major revision** — the design was written against an invented schema, not the actual requirements. The schema, trait, and type definitions all need to be rewritten to match GOAL-1.1 through GOAL-1.18.
- **Estimated implementation confidence:** LOW — an implementer following this design would build the wrong system.
- **Root cause:** The specialist that wrote this design did not use the actual requirements. It appears to have invented a simpler schema instead of implementing the 20+ column `nodes` table, separate `node_metadata`/`node_tags` tables, and the specific `GraphStorage` trait API from the requirements.

---
## ✅ All Findings Applied (2026-04-06 19:55)

All 11 findings applied to design-storage.md:
- FINDING-1 ✅ Schema rewritten: 21-column nodes + node_metadata + node_tags + knowledge (JSON-blob) + change_log + config tables
- FINDING-2 ✅ GraphStorage trait rewritten to match GOAL-1.9a-e exactly (get_node/put_node/get_edges/add_edge/remove_edge + query/search + tags/metadata + project_meta/knowledge + counts/ids + batch)
- FINDING-3 ✅ StorageOp enum uses canonical variants from design.md §3 (PutNode, DeleteNode, AddEdge, RemoveEdge, SetTags, SetMetadata, SetKnowledge)
- FINDING-4 ✅ Added §8 Write Contention, §9 Backend Detection, §10 Call-Site Migration Plan, §2.7 change_log table
- FINDING-5 ✅ Clarified GUARD-8: WAL mode satisfies at SQLite level, RefCell is Rust borrow checking
- FINDING-6 ✅ FTS5 indexes id, title, description, signature, doc_comment (matching GOAL-1.7)
- FINDING-7 ✅ CTE depth=0 semantics defined + max practical depth 10
- FINDING-8 ✅ FTS5 query sanitization: user input wrapped in double-quotes for literal matching
- FINDING-9 ✅ edges.confidence REAL column added
- FINDING-10 ✅ Traceability table rebuilt against actual requirements (GOAL-1.1 through 1.18)
- FINDING-11 ✅ change_log_enabled field added to SqliteStorage struct

