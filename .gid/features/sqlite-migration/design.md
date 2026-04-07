# Design: GID YAML→SQLite Migration — Master

## 1. Architecture Overview

The migration replaces the current load-entire-YAML / save-entire-YAML pattern with a `GraphStorage` trait backed by SQLite. Today every `gid` command calls `load_graph()` (deserialize full YAML into `Graph` struct) → mutate → `save_graph()` (serialize back). This is O(n) on every operation and breaks at scale.

After migration:
- A **`GraphStorage` trait** (sync, `&self`, object-safe) defines CRUD + query + batch operations.
- **`SqliteStorage`** implements the trait against `.gid/graph.db` (WAL mode, FTS5, indexed).
- **`YamlStorage`** is a thin compatibility shim that keeps the old load/save behavior for unmigrated projects.
- CLI commands call trait methods instead of `load_graph`/`save_graph`.
- `gid migrate` is a one-time conversion command.
- History uses SQLite backup API instead of copying YAML files.
- A new `gid context` command leverages fast graph traversal for AI agents.

```
                    ┌──────────────────────┐
                    │    gid CLI / MCP /    │
                    │    LSP / crate API    │
                    └──────────┬───────────┘
                               │ calls
                    ┌──────────▼───────────┐
                    │   GraphStorage trait  │
                    │ get_node, put_node,   │
                    │ query_nodes, search,  │
                    │ execute_batch, ...    │
                    └──────┬────────┬───────┘
                           │        │
              ┌────────────▼──┐  ┌──▼────────────┐
              │ SqliteStorage │  │  YamlStorage   │
              │ .gid/graph.db │  │ .gid/graph.yml │
              │ (primary)     │  │ (compat shim)  │
              └───────────────┘  └────────────────┘
```

### Non-goals (GUARD-10, requirements.md Out of Scope)

- No remote/cloud backend — only SQLite in v1
- No multi-user concurrent writes — SQLite single-writer is sufficient
- No schema migration tooling — v1 schema is final for this release
- No dual-write (YAML + SQLite) — after migration, writes go to SQLite only
- No changes to `code-graph.json` — extractor pipeline is separate

## 2. Module Organization

All new code lives in `crates/gid-core/src/storage/`:

```
crates/gid-core/src/
├── storage/
│   ├── mod.rs          # GraphStorage trait, StorageError, StorageOp, NodeFilter, shared types
│   ├── sqlite.rs       # SqliteStorage implementation (~800 lines)
│   ├── yaml.rs         # YamlStorage compatibility shim (~100 lines)
│   ├── migrate.rs      # YAML→SQLite migration logic (~300 lines)
│   ├── context.rs      # gid context assembly algorithm (~400 lines)
│   └── schema.sql      # DDL as embedded string (included via include_str!)
├── history.rs          # Updated: HistoryManager uses SqliteStorage for snapshot
├── graph.rs            # Unchanged: Node, Edge, Graph types stay
├── query.rs            # Gradual: QueryEngine gets GraphStorage-backed alternative
└── parser.rs           # Unchanged but deprecated: load_graph/save_graph still exist
```

### Feature Flag

```toml
# Cargo.toml
[features]
default = ["graph"]
sqlite = ["graph", "dep:rusqlite"]    # NEW
harness = ["sqlite", ...]              # harness implies sqlite
```

`sqlite` feature gates `storage::sqlite`, `storage::migrate`, and `storage::context`. The `GraphStorage` trait itself is always available (no feature gate) so downstream code can be generic.

## 3. Shared Types

Defined in `storage/mod.rs`. *(ref: GOAL-1.9, GOAL-1.15, GOAL-1.17)*

```rust
/// Storage errors — covers all failure modes without exposing SQLite internals (GUARD-10).
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("node not found: {0}")]
    NodeNotFound(String),

    #[error("database is locked — another process is writing. Try again. (SQLITE_BUSY after timeout)")]
    DatabaseLocked,

    #[error("foreign key violation: {0}")]
    ForeignKeyViolation(String),

    #[error("schema version mismatch: expected {expected}, found {found}")]
    SchemaMismatch { expected: String, found: String },

    #[error("storage I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("storage error: {0}")]
    Other(String),
}

/// Batch operation for execute_batch (GOAL-1.15, GUARD-10 object-safety).
#[derive(Debug, Clone)]
pub enum StorageOp {
    PutNode(Node),
    DeleteNode(String),          // node_id
    AddEdge(Edge),
    RemoveEdge {
        from: String,
        to: String,
        relation: String,
    },
    SetTags(String, Vec<String>),                       // node_id, tags
    SetMetadata(String, HashMap<String, Value>),        // node_id, metadata
    SetKnowledge(String, KnowledgeNode),                // node_id, knowledge
}

/// Filter for query_nodes (GOAL-1.9b).
#[derive(Debug, Default, Clone)]
pub struct NodeFilter {
    pub node_type: Option<String>,
    pub status: Option<String>,
    pub file_path: Option<String>,   // exact match or LIKE prefix
    pub tag: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}
```

## 4. Cross-cutting Decisions

### 4.1 Error Handling Strategy

All `GraphStorage` methods return `Result<T, StorageError>`. The `StorageError` enum does NOT contain `rusqlite::Error` (GUARD-10). `SqliteStorage` maps rusqlite errors:
- `rusqlite::Error::SqliteFailure` with `SQLITE_BUSY` → `StorageError::DatabaseLocked` (GOAL-1.17)
- FK violation → `StorageError::ForeignKeyViolation`
- Everything else → `StorageError::Other` with message

### 4.2 Logging (GOAL-1.18, 2.10, 3.9, 4.13)

Uses the `tracing` crate (already a dependency).
- Failed operations: `tracing::warn!`
- Successful writes: `tracing::debug!`
- Migration progress: `tracing::info!` (node/edge counts, elapsed time)
- Context traversal: `tracing::debug!` (nodes visited, budget usage)

CLI commands that want human-visible output print to stderr directly; tracing is for programmatic observability.

### 4.3 Connection Management

`SqliteStorage` holds a `rusqlite::Connection` (not a pool). The `&self` requirement (GUARD-8) is met via `RefCell<Connection>` — single-threaded interior mutability. This is safe because:
- `gid` CLI is single-threaded
- `GraphStorage` is not `Send + Sync` (not required by any consumer)
- WAL mode ensures readers don't block (GUARD-8)

If future consumers need thread-safety, we add a `Mutex<Connection>` variant or connection pool — out of scope for v1.

```rust
pub struct SqliteStorage {
    conn: RefCell<rusqlite::Connection>,
    change_log_enabled: bool,
}
```

### 4.4 Migration Path (Incremental Transition)

The existing `parser.rs` (`load_graph` / `save_graph`) stays — it's used by tests and the `YamlStorage` shim. The transition is:

1. **Phase 1**: Implement `GraphStorage` trait + `SqliteStorage` + `YamlStorage` + `gid migrate`
2. **Phase 2**: Update `gid-cli/main.rs` to use `GraphStorage` instead of `load_graph`/`save_graph`
3. **Phase 3**: Update `harness/scheduler.rs` and `ritual/executor.rs`
4. **Phase 4**: Update `history.rs` to use SQLite backup API
5. **Phase 5**: Implement `gid context`

Phase 1 is self-contained and testable. Phases 2–3 can happen in parallel. Phase 5 depends on Phase 1 being deployed.

### 4.5 Backend Detection (GOAL-1.14)

```rust
/// Detect storage backend. Returns (backend, path).
pub fn detect_backend(project_dir: &Path) -> Result<(Box<dyn GraphStorage>, PathBuf)> {
    let gid_dir = project_dir.join(".gid");
    let db_path = gid_dir.join("graph.db");
    let yml_candidates = [
        gid_dir.join("graph.yml"),
        gid_dir.join("graph.yaml"),
        project_dir.join("graph.yml"),
        project_dir.join("graph.yaml"),
    ];

    if db_path.exists() {
        // Case 1: SQLite exists → use it (GOAL-2.2)
        let storage = SqliteStorage::open(&db_path)?;
        return Ok((Box::new(storage), db_path));
    }

    if let Some(yml_path) = yml_candidates.iter().find(|p| p.exists()) {
        // Case 2: Only YAML → use YamlStorage + warn (GOAL-2.1)
        eprintln!("YAML graph detected. Run `gid migrate` to upgrade to SQLite for better performance.");
        let storage = YamlStorage::open(yml_path)?;
        return Ok((Box::new(storage), yml_path.clone()));
    }

    if gid_dir.exists() {
        // Case 3: .gid/ exists but no graph → error
        Err(StorageError::Other("No graph found. Run `gid init` to create one.".into()).into())
    } else {
        // Case 4: no .gid/ → error
        Err(StorageError::Other("Not a GID project. Run `gid init` to initialize.".into()).into())
    }
}
```

## 5. Feature Dependencies

```
┌─────────────┐
│  1. Storage  │ ← GraphStorage trait, SqliteStorage, schema
│  (P0 core)   │
└──────┬───────┘
       │ depends_on
  ┌────┴────┐──────────────┐
  │         │              │
  ▼         ▼              ▼
┌──────┐  ┌──────┐  ┌──────────┐
│2.Migr│  │3.Hist│  │4.Context │
│ation │  │ory   │  │(Phase 2) │
└──────┘  └──────┘  └──────────┘
```

- **Storage** (design-storage.md) blocks everything — trait + schema must exist first.
- **Migration** (design-migration.md) depends on Storage — needs schema to write into.
- **History** (design-history.md) depends on Storage — needs `SqliteStorage` to backup.
- **Context** (design-context.md) depends on Storage — needs fast graph traversal via SQL.
- Migration, History, and Context are independent of each other.

## 6. External Dependencies

```toml
# New dependency
rusqlite = { version = "0.33", features = ["bundled", "backup"], optional = true }
```

`bundled` — embeds SQLite, no system dependency. `backup` — enables `Connection::backup()` for GOAL-3.1.

No other new dependencies. `serde_json` and `chrono` are already in use.

## 7. Testing Strategy

| Layer | What | How |
|-------|------|-----|
| Unit | SqliteStorage CRUD | `tempfile::TempDir`, create storage, exercise each method |
| Unit | Migration logic | Create known YAML, migrate, verify DB contents |
| Unit | Context assembly | Build test graph in SQLite, verify traversal + budget |
| Integration | CLI commands | `assert_cmd` crate, run `gid migrate` / `gid context` on temp dirs |
| Property | Round-trip | Generate random `Graph`, write to YAML, migrate, read back via `GraphStorage`, compare |
| Existing | Regression | All 140+ existing tests must still pass (GUARD-4) |

## 8. Sub-document Index

| Document | Covers | GOALs | GUARDs |
|----------|--------|-------|--------|
| [design-storage.md](design-storage.md) | Schema, trait, SqliteStorage, indexes, FTS5 | 1.1–1.18 | 1,2,6,7,8,10 |
| [design-migration.md](design-migration.md) | YAML→SQLite conversion, validation, backup | 2.1–2.10 | 3,5,9 |
| [design-history.md](design-history.md) | Snapshot save/list/restore/diff | 3.1–3.9 | — |
| [design-context.md](design-context.md) | Context assembly, relevance, token budget | 4.1–4.13 | — |

## 9. Trade-offs

| Decision | Alternative | Why this choice |
|----------|-------------|-----------------|
| `RefCell<Connection>` not `Mutex` | `Mutex<Connection>` for thread-safety | No consumer needs `Send + Sync` today. `Mutex` adds overhead. Easy to upgrade later. |
| Single `nodes` table, not per-type tables | Separate `task_nodes`, `file_nodes`, etc. | Simpler schema. `node_type` column + indexes are sufficient. Code-graph columns are nullable. |
| FTS5 content-sync triggers | Manual FTS update in application code | Content-sync is atomic with the row change. No risk of stale FTS data. |
| `StorageOp` enum for batches | `begin_transaction` / `commit` on trait | Trait-level transactions aren't object-safe. `StorageOp` is data, not control flow. |
| `bytes / 4` token estimation | tiktoken / cl100k tokenizer | No external dependency. Close enough for budget decisions. Tokenizer adds 5MB+ to binary. |
| `index.json` for snapshot metadata | Metadata table inside each snapshot DB | Reading metadata shouldn't require opening every .db file. Fast listing. |
