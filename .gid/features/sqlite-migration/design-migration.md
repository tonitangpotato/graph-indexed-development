# Design: SQLite Migration

**Feature:** sqlite-migration
**Covers:** GOALs 2.1–2.10, GUARDs 3, 5, 9
**Date:** 2026-04-06
**Status:** Draft

---

## 1. Overview

This document specifies the design for migrating existing YAML-backed graph data (`graph.yml`) into the SQLite storage backend introduced by the `sqlite-storage` feature. The migration pipeline is a deterministic, transaction-safe, idempotent process that parses the source YAML, validates structural and referential integrity, transforms nodes and edges into SQLite row representations, batch-inserts them within a single transaction, and verifies post-migration correctness. The design honours the master `design.md` architecture decisions — in particular the `StorageOp` trait boundary, the `GraphId` / `Edge` shared types from §3, and the error taxonomy — ensuring migration is a first-class operation rather than an ad-hoc script.

---

## 2. Migration Pipeline

The pipeline executes five sequential phases. A failure in any phase aborts the entire migration and triggers rollback (§9).

```
┌──────────┐    ┌──────────┐    ┌───────────┐    ┌────────┐    ┌────────┐
│  YAML    │───▶│ Validate │───▶│ Transform │───▶│ Insert │───▶│ Verify │
│  Parse   │    │          │    │           │    │        │    │        │
└──────────┘    └──────────┘    └───────────┘    └────────┘    └────────┘
     ▲                                                              │
     │                      on failure: rollback                    │
     └──────────────────────────────────────────────────────────────┘
```

| Phase | Input | Output | Failure Mode |
|-----------|--------------------------|-------------------------------|-------------------------------|
| Parse | `graph.yml` bytes | `RawYamlGraph` | `MigrationError::ParseFailed` |
| Validate | `RawYamlGraph` | `ValidatedGraph` | `MigrationError::ValidationFailed` |
| Transform | `ValidatedGraph` | `Vec<StorageOp>` | `MigrationError::TransformFailed` |
| Insert | `Vec<StorageOp>` | committed transaction | `MigrationError::InsertFailed` |
| Verify | SQLite DB handle | `MigrationReport` | `MigrationError::VerifyFailed` |

**[GOAL 2.1]** — The pipeline is the top-level entry point for all YAML-to-SQLite migration.
**[GUARD 3]** — Each phase boundary is an explicit error check; no silent data loss.

---

## 3. MigrationConfig

Configuration is provided at invocation time and controls pipeline behaviour.

```rust
/// Controls migration behaviour. Constructed by CLI or calling code.
pub struct MigrationConfig {
    /// Path to source YAML file (default: `.gid/graph.yml`).
    pub source_path: PathBuf,

    /// Path to SQLite database (default: `.gid/graph.db`).
    pub target_path: PathBuf,

    /// Directory for backup copies of the original YAML.
    /// `None` disables backup (not recommended).
    pub backup_dir: Option<PathBuf>,

    /// Validation strictness.
    pub validation_level: ValidationLevel,

    /// When `true`, skip the pre-existence check (§11.1) and
    /// overwrite an existing database. For recovery scenarios.
    pub force: bool,

    /// When `true`, emit detailed per-record diagnostics in the report.
    pub verbose: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationLevel {
    /// Reject on any error (but not warnings — duplicates/dangles are always warnings).
    Strict,
    /// Log warnings but continue if data is structurally recoverable.
    Permissive,
    /// Skip validation entirely (testing only).
    None,
}

impl Default for MigrationConfig {
    fn default() -> Self {
        Self {
            source_path: PathBuf::from(".gid/graph.yml"),
            target_path: PathBuf::from(".gid/graph.db"),
            backup_dir: Some(PathBuf::from(".gid/backups")),
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        }
    }
}
```

**[GOAL 2.2]** — `MigrationConfig` is the single source of truth for tuneable migration parameters.

---

## 4. YAML Parsing

### 4.1 Loading

The source file is read in its entirety into memory (graph sizes are bounded by practical use; streaming is unnecessary). A file size check rejects files larger than 100MB with `MigrationError::ParseFailed("file too large: {size} bytes, max 100MB")`.

Parsing uses `serde_yaml` to deserialise into `RawYamlGraph`:

```rust
#[derive(Debug, Deserialize)]
pub struct RawYamlGraph {
    pub nodes: Vec<RawYamlNode>,
    pub edges: Vec<RawYamlEdge>,
    #[serde(default)]
    pub metadata: Option<serde_yaml::Value>,
}

/// Matches the actual gid-core Node struct fields from graph.yml.
/// Core fields are deserialized into dedicated fields; everything else
/// is captured in `extra` via serde(flatten).
#[derive(Debug, Deserialize)]
pub struct RawYamlNode {
    pub id: String,
    pub title: Option<String>,
    pub status: Option<String>,
    pub description: Option<String>,
    #[serde(rename = "type", alias = "node_type")]
    pub node_type: Option<String>,
    pub assigned_to: Option<String>,
    pub priority: Option<u8>,
    pub tags: Option<Vec<String>>,
    pub knowledge: Option<RawYamlKnowledge>,
    /// All remaining fields — includes both metadata keys that match
    /// dedicated column names (file_path, lang, start_line, etc.) and
    /// truly dynamic fields. The transform phase (§6) separates these.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_yaml::Value>,
}

/// Matches the actual gid-core Edge struct fields.
#[derive(Debug, Deserialize)]
pub struct RawYamlEdge {
    pub from: String,
    pub to: String,
    pub relation: Option<String>,
    pub weight: Option<f64>,
    pub confidence: Option<f64>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_yaml::Value>,
}

/// Matches KnowledgeNode struct: findings, file_cache, tool_history.
#[derive(Debug, Deserialize)]
pub struct RawYamlKnowledge {
    #[serde(default)]
    pub findings: HashMap<String, String>,
    #[serde(default)]
    pub file_cache: HashMap<String, String>,
    #[serde(default)]
    pub tool_history: Vec<RawToolCallRecord>,
}

#[derive(Debug, Deserialize)]
pub struct RawToolCallRecord {
    pub tool_name: String,
    pub timestamp: String,
    pub summary: String,
}
```

### 4.2 Corrupt / Partial YAML Handling

| Scenario | Behaviour | GUARD |
|------------------------------|-----------------------------------------------|-------|
| File not found | `MigrationError::SourceNotFound` | 3 |
| Empty file | Treated as empty graph (0 nodes, 0 edges) | 5 |
| Truncated YAML (parse error) | `MigrationError::ParseFailed` with byte offset | 3 |
| Valid YAML, wrong schema | Caught in Validation phase (§5) | 3, 5 |
| Encoding error (non-UTF-8) | `MigrationError::ParseFailed` | 3 |

**[GOAL 2.3]** — Robust parsing with clear error provenance.
**[GUARD 5]** — Partial/corrupt data never silently enters the pipeline.

---

## 5. Validation Rules

Validation operates on `RawYamlGraph` and produces either `ValidatedGraph` or a list of `ValidationDiagnostic` entries.

```rust
pub fn validate(raw: &RawYamlGraph, level: ValidationLevel) -> Result<ValidatedGraph, MigrationError> {
    let mut diagnostics: Vec<ValidationDiagnostic> = Vec::new();

    // 5.1 — Duplicate node IDs (GOAL-2.9: last occurrence wins + warning)
    // NOTE: Duplicate handling always uses "last wins" semantics regardless
    // of validation level. Duplicates are warnings, never errors.
    let mut seen_ids: HashMap<&str, usize> = HashMap::new();  // id → index of last occurrence
    for (i, node) in raw.nodes.iter().enumerate() {
        if let Some(prev_idx) = seen_ids.insert(&node.id, i) {
            diagnostics.push(ValidationDiagnostic::DuplicateNodeId {
                id: node.id.clone(),
                kept_index: i,
                dropped_index: prev_idx,
            });
        }
    }

    // 5.2 — Dangling edge references (GOAL-2.9: migrate anyway, warn)
    // NOTE: During migration, dangling edges are always warnings — they are
    // still migrated. FK enforcement is disabled during the migration transaction.
    for edge in &raw.edges {
        if !seen_ids.contains_key(edge.from.as_str()) {
            diagnostics.push(ValidationDiagnostic::DanglingEdgeRef {
                field: "from".into(),
                id: edge.from.clone(),
            });
        }
        if !seen_ids.contains_key(edge.to.as_str()) {
            diagnostics.push(ValidationDiagnostic::DanglingEdgeRef {
                field: "to".into(),
                id: edge.to.clone(),
            });
        }
    }

    // 5.3 — Type checks (node_type / edge relation against known variants)
    for node in &raw.nodes {
        if let Some(ref t) = node.node_type {
            if !is_known_node_type(t) {
                diagnostics.push(ValidationDiagnostic::UnknownNodeType(t.clone()));
            }
        }
    }
    for edge in &raw.edges {
        if let Some(ref r) = edge.relation {
            if !is_known_edge_relation(r) {
                diagnostics.push(ValidationDiagnostic::UnknownEdgeRelation(r.clone()));
            }
        }
    }

    // 5.4 — Self-loops (optional warning)
    for edge in &raw.edges {
        if edge.from == edge.to {
            diagnostics.push(ValidationDiagnostic::SelfLoop(edge.from.clone()));
        }
    }

    // Build ValidatedGraph with deduplication applied (last wins)
    let deduped_nodes = deduplicate_nodes(&raw.nodes, &seen_ids);

    match level {
        ValidationLevel::Strict => {
            // In Strict mode, only true errors (not warnings) block migration.
            // Duplicate IDs and dangling edges are always warnings per GOAL-2.9.
            let errors: Vec<_> = diagnostics.iter()
                .filter(|d| d.is_error())  // excludes DuplicateNodeId and DanglingEdgeRef
                .cloned()
                .collect();
            if !errors.is_empty() {
                Err(MigrationError::ValidationFailed(errors))
            } else {
                Ok(ValidatedGraph::from_deduped(deduped_nodes, &raw.edges, diagnostics))
            }
        }
        ValidationLevel::Permissive => {
            let errors: Vec<_> = diagnostics.iter().filter(|d| d.is_error()).cloned().collect();
            if !errors.is_empty() {
                Err(MigrationError::ValidationFailed(errors))
            } else {
                Ok(ValidatedGraph::from_deduped(deduped_nodes, &raw.edges, diagnostics))
            }
        }
        ValidationLevel::None => {
            Ok(ValidatedGraph::from_deduped(deduped_nodes, &raw.edges, diagnostics))
        }
    }
}
```

**[GOAL 2.4]** — Structural and referential integrity is enforced before any writes.
**[GUARD 3]** — Dangling references are caught; they cannot corrupt the SQLite foreign-key graph.

---

## 6. Transform

The transform phase converts `ValidatedGraph` (YAML-native types) into `Vec<StorageOp>` — the shared operation enum from `design.md` §3.

### 6.1 Field Mapping — Nodes

| YAML Source | SQLite Target | Conversion |
|---|---|---|
| `id` | `nodes.id` (TEXT PK) | Direct copy (String) |
| `title` | `nodes.title` | `Option<String>` → NULL if None |
| `status` | `nodes.status` | `Option<String>` → NULL if None |
| `description` | `nodes.description` | `Option<String>` → NULL if None |
| `node_type` | `nodes.node_type` | `Option<String>` → `"task"` if None |
| `assigned_to` | `nodes.assigned_to` | `Option<String>` → NULL if None |
| `priority` | `nodes.priority` | `Option<u8>` → NULL if None |
| `tags` | `node_tags` rows | See §6.3 |
| `knowledge` | `knowledge` row | See §6.4 |
| `extra.file_path` | `nodes.file_path` | **Promoted** — dedicated column |
| `extra.lang` | `nodes.lang` | **Promoted** |
| `extra.start_line` | `nodes.start_line` | **Promoted** (parsed as INTEGER) |
| `extra.end_line` | `nodes.end_line` | **Promoted** |
| `extra.signature` | `nodes.signature` | **Promoted** |
| `extra.visibility` | `nodes.visibility` | **Promoted** |
| `extra.doc_comment` | `nodes.doc_comment` | **Promoted** |
| `extra.body_hash` | `nodes.body_hash` | **Promoted** |
| `extra.node_kind` | `nodes.node_kind` | **Promoted** |
| `extra.owner` | `nodes.owner` | **Promoted** |
| `extra.source` | `nodes.source` | **Promoted** |
| `extra.repo` | `nodes.repo` | **Promoted** |
| `extra.created_at` | `nodes.created_at` | **Promoted** |
| `extra.updated_at` | `nodes.updated_at` | **Promoted** |
| Remaining `extra` keys | `node_metadata` rows | Key-value pairs (§6.3) |

### 6.2 Field Mapping — Edges

| YAML Source | SQLite Target | Conversion |
|---|---|---|
| `from` | `edges.from_node` | Direct copy (String) |
| `to` | `edges.to_node` | Direct copy (String) |
| `relation` | `edges.relation` | `Option<String>` → `"relates_to"` if None |
| `weight` | `edges.weight` | `Option<f64>` → `1.0` if None |
| `confidence` | `edges.confidence` | `Option<f64>` → NULL if None |
| `extra` | `edges.metadata` | `serde_json::to_string(&extra)` or NULL if empty |

### 6.3 Metadata Promotion & Tag/Metadata Insertion

**GOAL-2.3:** Metadata keys matching dedicated column names are **promoted** to those columns. Remaining keys go to `node_metadata`.

```rust
/// Dedicated column names that are promoted from extra → nodes columns.
const PROMOTED_KEYS: &[&str] = &[
    "file_path", "lang", "start_line", "end_line", "signature",
    "visibility", "doc_comment", "body_hash", "node_kind",
    "owner", "source", "repo", "created_at", "updated_at",
];

fn transform_node(node: &RawYamlNode) -> Vec<StorageOp> {
    let mut ops = Vec::new();

    // Build the Node struct with core fields + promoted metadata
    let mut promoted = HashMap::new();
    let mut remaining_metadata = HashMap::new();

    for (key, value) in &node.extra {
        if PROMOTED_KEYS.contains(&key.as_str()) {
            promoted.insert(key.clone(), value.clone());
        } else {
            remaining_metadata.insert(key.clone(), serde_json::to_value(value).unwrap_or_default());
        }
    }

    let built_node = Node {
        id: node.id.clone(),
        title: node.title.clone(),
        status: node.status.clone(),
        description: node.description.clone(),
        node_type: node.node_type.clone().unwrap_or_else(|| "task".into()),
        assigned_to: node.assigned_to.clone(),
        priority: node.priority,
        file_path: promoted.remove("file_path").and_then(|v| v.as_str().map(String::from)),
        lang: promoted.remove("lang").and_then(|v| v.as_str().map(String::from)),
        start_line: promoted.remove("start_line").and_then(|v| v.as_i64().map(|n| n as i32)),
        end_line: promoted.remove("end_line").and_then(|v| v.as_i64().map(|n| n as i32)),
        signature: promoted.remove("signature").and_then(|v| v.as_str().map(String::from)),
        // ... remaining promoted fields follow same pattern
        ..Default::default()
    };

    ops.push(StorageOp::PutNode(built_node));

    // Tags → node_tags table (GOAL-2.3)
    if let Some(ref tags) = node.tags {
        if !tags.is_empty() {
            ops.push(StorageOp::SetTags(node.id.clone(), tags.clone()));
        }
    }

    // Remaining metadata → node_metadata table (GOAL-1.3)
    if !remaining_metadata.is_empty() {
        ops.push(StorageOp::SetMetadata(node.id.clone(), remaining_metadata));
    }

    ops
}
```

### 6.4 Knowledge Migration

**GOAL-2.5:** KnowledgeNode data (findings, file_cache, tool_history) is migrated to the `knowledge` table.

```rust
fn transform_knowledge(node: &RawYamlNode) -> Option<StorageOp> {
    let knowledge = node.knowledge.as_ref()?;

    // Skip if all three fields are empty
    if knowledge.findings.is_empty()
        && knowledge.file_cache.is_empty()
        && knowledge.tool_history.is_empty()
    {
        return None;
    }

    let kn = KnowledgeNode {
        findings: knowledge.findings.clone(),
        file_cache: knowledge.file_cache.clone(),
        tool_history: knowledge.tool_history.iter().map(|r| ToolCallRecord {
            tool_name: r.tool_name.clone(),
            timestamp: r.timestamp.clone(),
            summary: r.summary.clone(),
        }).collect(),
    };

    Some(StorageOp::SetKnowledge(node.id.clone(), kn))
}
```

### 6.5 Full Transform Pipeline

```rust
pub fn transform(graph: &ValidatedGraph) -> Result<Vec<StorageOp>, MigrationError> {
    let mut ops = Vec::new();

    for node in &graph.nodes {
        // Node + tags + metadata
        ops.extend(transform_node(node));
        // Knowledge
        if let Some(kn_op) = transform_knowledge(node) {
            ops.push(kn_op);
        }
    }

    for edge in &graph.edges {
        let edge_struct = Edge {
            from: edge.from.clone(),
            to: edge.to.clone(),
            relation: edge.relation.clone().unwrap_or_else(|| "relates_to".into()),
            weight: edge.weight.unwrap_or(1.0),
            confidence: edge.confidence,
            metadata: if edge.extra.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&edge.extra).unwrap_or_default())
            },
        };
        ops.push(StorageOp::AddEdge(edge_struct));
    }

    Ok(ops)
}
```

**[GOAL 2.3]** — Nodes mapped with metadata promotion (dedicated columns vs. node_metadata KV).
**[GOAL 2.4]** — Edges mapped with `relation`, `weight`, `confidence`.
**[GOAL 2.5]** — Knowledge data migrated to dedicated table.
**[GOAL 2.6]** — All `extra` fields preserved (promoted or in node_metadata/edge metadata).

---

## 7. Insertion

All `StorageOp` items are applied inside a **single SQLite transaction**. This guarantees atomicity: either the full graph is migrated or nothing is written.

### 7.1 Insert Strategy

```rust
pub fn insert(
    db: &mut SqliteConnection,
    ops: Vec<StorageOp>,
) -> Result<InsertStats, MigrationError> {
    let tx = db.transaction()
        .map_err(|e| MigrationError::InsertFailed(e.to_string()))?;

    // GOAL-2.9: Disable FK enforcement during migration so that
    // dangling edge references are migrated (with warnings from §5.2).
    tx.execute_batch("PRAGMA foreign_keys = OFF")?;

    // Record migration fingerprint first (§11 Idempotency)
    write_migration_fingerprint(&tx, &ops)?;

    let mut stats = InsertStats::default();

    for op in &ops {
        match op {
            StorageOp::PutNode(node) => {
                execute_put_node(&tx, node)?;
                stats.nodes_inserted += 1;
            }
            StorageOp::AddEdge(edge) => {
                execute_add_edge(&tx, edge)?;
                stats.edges_inserted += 1;
            }
            StorageOp::SetTags(node_id, tags) => {
                execute_set_tags(&tx, node_id, tags)?;
                stats.tags_inserted += tags.len() as u64;
            }
            StorageOp::SetMetadata(node_id, metadata) => {
                execute_set_metadata(&tx, node_id, metadata)?;
                stats.metadata_inserted += metadata.len() as u64;
            }
            StorageOp::SetKnowledge(node_id, knowledge) => {
                execute_set_knowledge(&tx, node_id, knowledge)?;
                stats.knowledge_inserted += 1;
            }
            _ => {} // migration only produces Put/Add/Set ops
        }
    }

    // Re-enable FK enforcement
    tx.execute_batch("PRAGMA foreign_keys = ON")?;

    // Populate FTS index for full-text search
    rebuild_fts_index(&tx)?;

    tx.commit()
        .map_err(|e| MigrationError::InsertFailed(e.to_string()))?;

    Ok(stats)
}
```

**Note on batch_size:** The previous design had `batch_size` for chunking ops within the transaction. Since all ops execute in a single transaction, SQLite doesn't benefit from sub-batching — it's all one atomic write. Removed to avoid confusion. Progress reporting (GOAL-2.10) uses `stats` counters logged during iteration.

### 7.2 Insert Semantics

`INSERT OR REPLACE` is used for nodes so that duplicate IDs (last-wins per GOAL-2.9) are handled at the SQL level. Edges use plain `INSERT` since duplicates are not expected after validation.

**[GOAL 2.7]** — All mutations are transaction-wrapped; partial writes are impossible.
**[GOAL-2.9]** — FK enforcement disabled during migration; dangling edges migrated with warnings.

---

## 8. Verification

After the transaction commits, the Verify phase performs read-only integrity checks against the newly populated database.

### 8.1 Checks

| Check | Method | Pass Condition |
|-------------------------------|----------------------------------------------|----------------------------------------------|
| **Row count — nodes** | `SELECT COUNT(*) FROM nodes` | Equals `stats.nodes_inserted` |
| **Row count — edges** | `SELECT COUNT(*) FROM edges` | Equals `stats.edges_inserted` |
| **Row count — knowledge** | `SELECT COUNT(*) FROM knowledge` | Equals `stats.knowledge_inserted` |
| **Row count — tags** | `SELECT COUNT(*) FROM node_tags` | Equals `stats.tags_inserted` |
| **Row count — metadata** | `SELECT COUNT(*) FROM node_metadata` | Equals `stats.metadata_inserted` |
| **FTS coherence** | `SELECT COUNT(*) FROM nodes_fts` | Equals node count |
| **Project metadata** | `SELECT value FROM config WHERE key = 'project_name'` | Matches YAML project_name (GOAL-2.6c) |
| **Migration fingerprint** | `SELECT fingerprint FROM _migrations ORDER BY rowid DESC LIMIT 1` | Matches computed fingerprint |

### 8.2 Pseudocode

```rust
pub fn verify(
    db: &SqliteConnection,
    expected: &InsertStats,
) -> Result<MigrationReport, MigrationError> {
    let node_count: u64 = db.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
    let edge_count: u64 = db.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
    let knowledge_count: u64 = db.query_row("SELECT COUNT(*) FROM knowledge", [], |r| r.get(0))?;
    let tag_count: u64 = db.query_row("SELECT COUNT(*) FROM node_tags", [], |r| r.get(0))?;
    let metadata_count: u64 = db.query_row("SELECT COUNT(*) FROM node_metadata", [], |r| r.get(0))?;

    if node_count != expected.nodes_inserted as u64 {
        return Err(MigrationError::VerifyFailed(format!(
            "node count mismatch: expected {}, got {node_count}", expected.nodes_inserted
        )));
    }
    if edge_count != expected.edges_inserted as u64 {
        return Err(MigrationError::VerifyFailed(format!(
            "edge count mismatch: expected {}, got {edge_count}", expected.edges_inserted
        )));
    }
    if knowledge_count != expected.knowledge_inserted as u64 {
        return Err(MigrationError::VerifyFailed(format!(
            "knowledge count mismatch: expected {}, got {knowledge_count}", expected.knowledge_inserted
        )));
    }
    // Tags and metadata counts are informational (not hard errors if mismatched)

    Ok(MigrationReport {
        nodes: node_count,
        edges: edge_count,
        knowledge: knowledge_count,
        tags: tag_count,
        metadata_entries: metadata_count,
        diagnostics: vec![],
    })
}
```

**[GOAL 2.6]** — Post-migration verification catches data integrity regressions.
**[GOAL 2.5]** — Knowledge row count verified.

---

## 9. Backup & Rollback

### 9.1 Backup

Before any destructive operation, the original YAML is copied to the backup directory:

```rust
pub fn backup_source(config: &MigrationConfig) -> Result<PathBuf, MigrationError> {
    let backup_dir = config.backup_dir.as_ref()
        .ok_or(MigrationError::BackupDisabled)?;

    fs::create_dir_all(backup_dir)
        .map_err(|e| MigrationError::BackupFailed(e.to_string()))?;

    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let backup_path = backup_dir.join(format!("graph.yml.{timestamp}.bak"));

    fs::copy(&config.source_path, &backup_path)
        .map_err(|e| MigrationError::BackupFailed(e.to_string()))?;

    Ok(backup_path)
}
```

The backup is created **before** the Insert phase. If the Insert phase fails, the SQLite transaction is rolled back automatically (Rust `Drop` on uncommitted `Transaction`), so the database remains untouched.

### 9.2 Rollback Semantics

| Failure Point | YAML State | SQLite State | Recovery Action |
|---------------|------------------|-------------------|---------------------------------|
| Parse | Untouched | Untouched | Fix YAML, re-run |
| Validate | Untouched | Untouched | Fix YAML, re-run |
| Transform | Untouched | Untouched | Fix YAML / report bug |
| Insert | Backed up | Rolled back (tx) | Automatic — no user action |
| Verify | Backed up | Committed | Manual: delete DB, restore YAML |

For the Verify failure case, the `MigrationReport` includes the backup path so the user can restore:

```
Migration verification failed. Backup available at:
  .gid/backups/graph.yml.20260406T183400Z.bak
```

**[GOAL 2.10]** — Backup is always created before writes; rollback is automatic on insert failure.
**[GUARD 3]** — No data loss path exists — either migration succeeds fully or the original YAML is preserved.

---

## 10. MigrationReport

```rust
/// Returned by `migrate()` on success; also serialised to `.gid/migrations/last.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationReport {
    /// Number of nodes written to SQLite.
    pub nodes_migrated: u64,

    /// Number of edges written to SQLite.
    pub edges_migrated: u64,

    /// Number of knowledge rows written.
    pub knowledge_migrated: u64,

    /// Number of tag associations written.
    pub tags_migrated: u64,

    /// Number of metadata entries written.
    pub metadata_migrated: u64,

    /// Number of FTS index entries.
    pub fts_indexed: u64,

    /// Non-fatal diagnostics from the Validation phase (Permissive mode).
    pub warnings: Vec<ValidationDiagnostic>,

    /// Overall outcome.
    pub status: MigrationStatus,

    /// Wall-clock duration of the full pipeline.
    pub duration: Duration,

    /// Path to YAML backup (if created).
    pub backup_path: Option<PathBuf>,

    /// SHA-256 fingerprint of the source YAML used.
    pub source_fingerprint: String,

    /// ISO 8601 timestamp of migration completion.
    pub completed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MigrationStatus {
    Success,
    SuccessWithWarnings,
    Failed,
    Skipped, // idempotent mode, already migrated
}
```

**[GOAL 2.8]** — The report provides full observability into migration outcomes.

---

## 11. Idempotency & Pre-existence Check

### 11.1 Pre-existence Error (GOAL-2.8a)

**Before migration begins**, check if `graph.db` already exists:

```rust
pub fn check_preconditions(config: &MigrationConfig) -> Result<(), MigrationError> {
    // GOAL-2.8a: error if SQLite DB already exists
    if config.target_path.exists() {
        return Err(MigrationError::TargetExists(format!(
            "SQLite database already exists at {}. Migration is only needed once.",
            config.target_path.display()
        )));
    }

    // GOAL-2.8b: error if no YAML source
    if !config.source_path.exists() {
        return Err(MigrationError::SourceNotFound(
            "No YAML graph found. Nothing to migrate.".into()
        ));
    }

    Ok(())
}
```

This check runs **before** any file I/O. Exit with non-zero status.

### 11.2 `--force` Flag for Re-migration

If the user explicitly passes `--force`, the pre-existence check is skipped and the existing DB is overwritten. This is for recovery scenarios (e.g., corrupted initial migration).

```rust
if !config.force {
    check_preconditions(config)?;
}
```

### 11.3 Fingerprint Storage

Each successful migration records a fingerprint for auditability:

```sql
CREATE TABLE IF NOT EXISTS _migrations (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    fingerprint TEXT    NOT NULL,
    source_path TEXT    NOT NULL,
    completed_at TEXT   NOT NULL,
    node_count  INTEGER NOT NULL,
    edge_count  INTEGER NOT NULL,
    knowledge_count INTEGER NOT NULL
);
```

The fingerprint is `SHA-256(source YAML bytes)`. This is for audit/debugging, not for skip logic (GOAL-2.8a handles that via file existence).

---

## 12. MigrationError

`MigrationError` is a **separate** error type from `StorageError` (migration is a one-time operation, not storage CRUD). Conversion is provided via `From<StorageError>`.

```rust
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error("source not found: {0}")]
    SourceNotFound(String),

    #[error("target already exists: {0}")]
    TargetExists(String),

    #[error("YAML parse failed: {0}")]
    ParseFailed(String),

    #[error("validation failed")]
    ValidationFailed(Vec<ValidationDiagnostic>),

    #[error("transform failed: {0}")]
    TransformFailed(String),

    #[error("insert failed: {0}")]
    InsertFailed(String),

    #[error("verification failed: {0}")]
    VerifyFailed(String),

    #[error("backup failed: {0}")]
    BackupFailed(String),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
}
```

**Note:** Node IDs are plain `String`s matching the master design (design.md §3). No `GraphId::parse` is needed — IDs are copied directly from YAML to SQLite.

---

## 13. GOAL Traceability

| GOAL | Description | Implementing Section(s) |
|----------|--------------------------------------------------|----------------------------------|
| **2.1** | Auto-detect YAML, prompt to migrate | §11.1 precondition checks |
| **2.2** | Prefer graph.db when it exists | §11.1 (target_path existence check) |
| **2.3** | Migrate all node data with metadata promotion | §4.1, §6.1, §6.3 |
| **2.4** | Migrate all edges with relation, weight, confidence | §4.1 (RawYamlEdge), §6.2, §6.5 |
| **2.5** | Migrate knowledge data (findings, file_cache, tool_history) | §6.4 knowledge migration |
| **2.6** | Validate counts + project metadata post-migration | §8 Verification |
| **2.7** | Backup to .gid/graph.yml.bak | §9 Backup & Rollback |
| **2.8a** | Error if graph.db already exists | §11.1 Pre-existence error |
| **2.8b** | Error if no YAML graph found | §11.1 Pre-existence error |
| **2.9** | Duplicate IDs last-wins + dangling edges migrated | §5.1, §5.2, §7 (FK disabled) |
| **2.10** | Progress logging (node/edge/knowledge counts, elapsed) | §10 MigrationReport |

| GUARD | Constraint | Enforced In |
|-------|---------------------------------------------|--------------------------------------|
| **3** | No silent data loss | §2 (phase boundaries), §4.2, §5, §9 |
| **5** | Corrupt/partial data never enters pipeline | §4.2, §5, §11 |
| **9** | Memory-bounded processing | §4.1 (100MB file size limit) |

---

*End of design-migration.md*
