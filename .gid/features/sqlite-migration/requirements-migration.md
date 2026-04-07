# Requirements: YAML→SQLite Migration

## Feature Overview

The migration feature provides a one-time, user-initiated conversion from the existing `.gid/graph.yml` YAML storage to the new `.gid/graph.db` SQLite database. It reads all data from YAML, writes it into SQLite, validates correctness, and backs up the original file — ensuring zero data loss and a safe rollback path.

*Parent: [requirements.md](requirements.md) — see GUARDs there for cross-cutting constraints.*

## Goals

### Auto-Detection & Prompting

- **GOAL-2.1** [P0]: When any `gid` command runs and a YAML graph file (`.gid/graph.yml` or `.gid/graph.yaml`) exists but `.gid/graph.db` does not, the system prints a message to stderr: "YAML graph detected. Run `gid migrate` to upgrade to SQLite for better performance." The command still executes using the YAML backend — it does not block or fail. The detection uses the same candidate list as `find_graph_file` (`graph.yml`, `graph.yaml`). *(ref: discussion, Migration — auto-detect and prompt)*

- **GOAL-2.2** [P0]: When any `gid` command runs and `.gid/graph.db` exists, the system uses SQLite regardless of whether `.gid/graph.yml` also exists. No migration prompt is shown. *(ref: discussion, Backward Compatibility — prefer graph.db)*

### Migration Execution

- **GOAL-2.3** [P0]: Running `gid migrate` when a YAML graph file (`.gid/graph.yml` or `.gid/graph.yaml`) exists and `.gid/graph.db` does not creates the SQLite database with the full schema, reads every node from the YAML file, and writes each node into the SQLite `nodes`, `node_tags`, and `node_metadata` tables. Core fields (id, title, status, description, assigned_to, priority, node_type) are written to their corresponding `nodes` columns. During migration, metadata keys that match a dedicated column name in the `nodes` table (`file_path`, `lang`, `start_line`, `end_line`, `signature`, `visibility`, `doc_comment`, `body_hash`, `node_kind`, `owner`, `source`, `repo`, `created_at`, `updated_at`) are promoted to those columns. Remaining metadata keys go to `node_metadata`. Tags go to `node_tags`. *(ref: discussion, Migration — reads YAML, creates SQLite, writes all data)*

- **GOAL-2.4** [P0]: Running `gid migrate` transfers every edge from YAML (from, to, relation) into the SQLite `edges` table. Edge relations are preserved exactly. Edges referencing the `weight`, `confidence`, or `metadata` fields in their YAML representation (if present) have those values migrated; otherwise `weight` defaults to 1.0, `confidence` defaults to NULL, and `metadata` defaults to NULL. The `confidence: Option<f64>` field from the existing `Edge` struct is written to the `confidence REAL` column. *(ref: discussion, Migration — writes all data)*

- **GOAL-2.5** [P0]: Running `gid migrate` transfers all knowledge graph data from each node's `knowledge` field (a `KnowledgeNode` containing `findings: HashMap<String, String>`, `file_cache: HashMap<String, String>`, `tool_history: Vec<ToolCallRecord>`) into the SQLite `knowledge` table. For each node with non-empty knowledge: `findings` and `file_cache` are stored as JSON objects, `tool_history` is stored as a JSON array. Each node has at most one row in the `knowledge` table, keyed by `node_id`. *(ref: discussion, Migration — writes all data)*

- **GOAL-2.6** [P0]: After writing all data, `gid migrate` validates that: (a) the count of nodes in SQLite matches the count in YAML, (b) the count of edges in SQLite matches the count in YAML, and (c) the project metadata (name, description) is preserved. If validation fails, the SQLite database file is deleted and an error is reported listing the specific mismatch. *(ref: discussion, Migration — validates counts match)*

### Backup & Safety

- **GOAL-2.7** [P0]: After successful migration and validation, `gid migrate` copies `.gid/graph.yml` to `.gid/graph.yml.bak`. The original `.gid/graph.yml` is NOT deleted — both files remain. A message confirms: "Migration complete. Backup saved to .gid/graph.yml.bak" *(ref: discussion, Migration — backs up YAML to .yml.bak, preserves original)*

### Error Cases

- **GOAL-2.8a** [P0]: Running `gid migrate` when `.gid/graph.db` already exists prints an error: "SQLite database already exists at .gid/graph.db. Migration is only needed once." and exits with a non-zero status code. *(ref: discussion, Migration — error handling for edge cases)*

- **GOAL-2.8b** [P0]: Running `gid migrate` when no YAML graph file (`.gid/graph.yml` or `.gid/graph.yaml`) exists prints an error: "No YAML graph found. Nothing to migrate." and exits with a non-zero status code. *(ref: discussion, Migration — error handling for edge cases)*

- **GOAL-2.9** [P1]: If the YAML file fails to parse, `gid migrate` exits with a non-zero status code and an error describing the parse failure location. If edges reference non-existent node IDs, they are still migrated (the edges table does not enforce foreign keys during migration). If duplicate node IDs exist in the YAML, the last occurrence takes precedence and a warning is emitted to stderr. *(ref: review FINDING-11, corrupt/partial YAML handling)*

### Observability

- **GOAL-2.10** [P1]: `gid migrate` logs progress to stderr: node count during transfer, edge count during transfer, knowledge row count, and total elapsed time. On success, a summary line is printed: "Migrated {N} nodes, {E} edges, {K} knowledge entries in {T}ms." *(ref: review FINDING-16, observability)*

**10 GOALs** (8 P0 / 2 P1 / 0 P2)

*Note: GOAL-2.8a and GOAL-2.8b count as a single requirement (the original GOAL-2.8, split per review FINDING-7). Total distinct requirement items: 11.*
