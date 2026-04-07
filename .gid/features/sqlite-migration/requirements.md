# Requirements: GID YAML→SQLite Migration

## Overview

GID currently stores its graph in `.gid/graph.yml` (YAML), which requires loading the entire graph into memory for every operation. This works for small projects but breaks down at enterprise scale (50k+ nodes): load times exceed seconds, every single-field update rewrites the entire file, and there's no way to query the graph without full deserialization. The migration to SQLite (`.gid/graph.db`) enables O(1) node lookups, partial reads, full-text search, atomic transactions, and the upcoming `gid context` feature — token-budget-aware code context assembly for AI agents.

## Priority Levels

- **P0**: Core — required for the system to function at all
- **P1**: Important — needed for production-quality operation
- **P2**: Enhancement — improves efficiency, UX, or observability

## Guard Severity

- **hard**: Violation = system is broken, execution must stop
- **soft**: Violation = degraded quality, should warn but can continue

## Feature Index

| Module | Feature | Document | GOAL Range |
|--------|---------|----------|------------|
| 1 | SQLite Storage Layer + GraphStorage Trait | [requirements-storage.md](requirements-storage.md) | GOAL-1.1 – GOAL-1.18 |
| 2 | YAML→SQLite Migration | [requirements-migration.md](requirements-migration.md) | GOAL-2.1 – GOAL-2.10 |
| 3 | History System on SQLite | [requirements-history.md](requirements-history.md) | GOAL-3.1 – GOAL-3.9 |
| 4 | `gid context` Command (Phase 2) | [requirements-context.md](requirements-context.md) | GOAL-4.1 – GOAL-4.13 |

## Guards

### Data Integrity

- **GUARD-1** [hard]: All write operations that modify more than one row are atomic — either all changes commit or none do. A crash or power loss mid-operation never leaves the database in a partially-written state. *(ref: discussion, Transaction Semantics)*
- **GUARD-2** [hard]: Foreign key constraints are enforced at all times — no edge references a non-existent node, no tag or metadata row references a non-existent node. *(ref: discussion, Transaction Semantics — foreign_keys=ON)*
- **GUARD-3** [hard]: No data is lost during migration — every node, edge (including confidence values), tag, metadata entry, knowledge node, and project metadata (name, description) present in the YAML source is present and correct in the SQLite target. *(ref: discussion, Migration — validates counts match)*

### Backward Compatibility

- **GUARD-4** [hard]: All existing `gid` CLI commands that currently work with YAML continue to work identically after the migration — same inputs produce same outputs (modulo formatting). *(ref: discussion, Backward Compatibility)*
- **GUARD-5** [soft]: When `.gid/graph.db` does not exist but `.gid/graph.yml` does, the system reads from YAML and prompts the user to run `gid migrate`. No silent data loss or crash. *(ref: discussion, Backward Compatibility — read path fallback)*

### Performance

- **GUARD-6** [soft]: Single-node read or write operations on a graph with 50,000+ nodes complete within 10ms (excluding disk I/O cold start). *(ref: discussion, enterprise monorepos with 50k+ nodes)*
- **GUARD-7** [soft]: For graphs with 1,000+ nodes, the SQLite database file size does not exceed 3× the equivalent YAML file size for the same graph data. For smaller graphs, fixed overhead from FTS5 indexes and schema structures may cause higher ratios. WAL file size is excluded from the measurement. *(ref: implicit — storage overhead must be reasonable)*

### Concurrency & Safety

- **GUARD-8** [hard]: Concurrent read operations never block each other. A single writer does not block readers. *(ref: discussion, Transaction Semantics — WAL mode)*
- **GUARD-9** [hard]: The original `.gid/graph.yml` file is never deleted or modified by any migration or storage operation — it is only backed up (copied to `.yml.bak`). *(ref: discussion, Migration — preserves original file)*

### API Surface Stability

- **GUARD-10** [soft]: The `GraphStorage` trait is object-safe and does not expose SQLite-specific types in its public interface, so that alternative backends can be implemented without depending on SQLite. *(ref: discussion, GraphStorage Trait — potentially RemoteStorage later)*

## Observability

All commands log progress and timing to stderr at default verbosity. Each sub-feature document defines specific observability GOALs:
- **GOAL-1.18** (storage): Failed operations log at WARN; successful writes at DEBUG.
- **GOAL-2.10** (migration): Logs node/edge/knowledge counts during transfer and total elapsed time.
- **GOAL-3.9** (history): Logs elapsed time and snapshot size for save; snapshot identity and time for restore; traversal stats for diff.
- **GOAL-4.13** (context): Logs traversal statistics — nodes visited, included, excluded, token budget usage, elapsed time.

See individual requirement documents for full specifications.

## Out of Scope

- **Remote/cloud storage backend** — `GraphStorage` trait enables it, but only `SqliteStorage` is implemented in this project
- **GUI or web interface** — CLI, MCP, and LSP are the interaction surfaces
- **Multi-user concurrent write access** — SQLite handles single-writer; multi-user collaboration is a future project
- **Schema migrations between SQLite versions** — first version of the schema is v1; upgrade tooling is future work
- **Code extraction changes** — the extractor continues writing `code-graph.json`; merging extracted data into SQLite is a separate concern handled by `gid extract` pipeline
- **YAML write path** — after migration, all writes go to SQLite only; no dual-write

## Glossary

| Term | Definition |
|------|------------|
| **node_type** | High-level graph role: `task`, `file`, `function`, `class`, `module`, `feature`, `component`, `layer`, `knowledge`. Stored in `nodes.node_type`. Used by `--include "type:..."` filters. |
| **node_kind** | Code-level sub-category from code extraction: `Function`, `Struct`, `Impl`, `Trait`, `Enum`, `Interface`, etc. (from `CodeNode.kind` / `NodeKind` enum). Stored in `nodes.node_kind`. Finer-grained than `node_type`. |
| **node metadata** | Dynamic key-value pairs stored in the `node_metadata` table (GOAL-1.3). Accessed via `get_metadata`/`set_metadata`. |
| **edge metadata** | JSON blob on the `edges.metadata` column (GOAL-1.5). Stores arbitrary edge properties. |
| **project config** | Project-level settings (name, description, schema_version) stored in the `config` table (GOAL-1.16). Accessed via `get_project_meta`/`set_project_meta`. |
| **snapshot** | A complete copy of `.gid/graph.db` stored in `.gid/history/`. The preferred term (not "checkpoint" or "history entry"). |

## Dependencies

- **rusqlite** (with `bundled` feature) — SQLite bindings for Rust, bundles SQLite so no system dependency required
- **serde_json** — already in use; needed for JSON metadata columns in SQLite
- **chrono** — already in use; needed for timestamps in history/change_log

**53 GOALs** (33 P0 / 17 P1 / 3 P2) + **10 GUARDs** (6 hard / 4 soft)

*Note: GOAL count includes sub-GOALs (e.g., 1.9a–1.9e) and findings-driven additions (2.8a/b, 2.9, 2.10, 3.9, 4.13). See individual requirements documents for detailed counts.*
