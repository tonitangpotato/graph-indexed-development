//! GOAL-1.9: SQLite backend implementing the `GraphStorage` trait.
//!
//! Uses WAL mode, foreign keys, and FTS5 for full-text search.
//! All operations go through a `RefCell<Connection>` for interior mutability.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use rusqlite::{params, Connection};
use serde_json::Value;

use crate::graph::{Edge, Node, NodeStatus, ProjectMeta};
use crate::task_graph_knowledge::KnowledgeNode;
use super::error::{StorageError, StorageOp};
use super::schema::SCHEMA_SQL;
use super::trait_def::{BatchOp, GraphStorage, NodeFilter};

// ── Error mapping ──────────────────────────────────────────

impl From<rusqlite::Error> for StorageError {
    fn from(err: rusqlite::Error) -> Self {
        match &err {
            rusqlite::Error::SqliteFailure(e, _)
                if e.code == rusqlite::ErrorCode::DatabaseBusy =>
            {
                StorageError::DatabaseLocked {
                    op: StorageOp::Write,
                    detail: "database is locked — another process is writing".into(),
                    source: Some(Box::new(err)),
                }
            }
            // ISS-015: Distinguish constraint types via extended_code
            rusqlite::Error::SqliteFailure(e, _)
                if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY =>
            {
                StorageError::ForeignKeyViolation {
                    op: StorageOp::Write,
                    detail: err.to_string(),
                    source: Some(Box::new(err)),
                }
            }
            rusqlite::Error::SqliteFailure(e, _)
                if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
            {
                StorageError::UniqueViolation {
                    op: StorageOp::Write,
                    detail: err.to_string(),
                    source: Some(Box::new(err)),
                }
            }
            rusqlite::Error::SqliteFailure(e, _)
                if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_CHECK =>
            {
                StorageError::CheckViolation {
                    op: StorageOp::Write,
                    detail: err.to_string(),
                    source: Some(Box::new(err)),
                }
            }
            rusqlite::Error::SqliteFailure(e, _)
                if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_NOTNULL =>
            {
                StorageError::NotNullViolation {
                    op: StorageOp::Write,
                    detail: err.to_string(),
                    source: Some(Box::new(err)),
                }
            }
            _ => StorageError::Sqlite {
                op: StorageOp::Read,
                detail: err.to_string(),
                source: Some(Box::new(err)),
            },
        }
    }
}

// ── Edge traversal direction ───────────────────────────────

/// Direction for BFS neighbor traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Follow edges where the current node is `from_node` (outgoing).
    Outgoing,
    /// Follow edges where the current node is `to_node` (incoming).
    Incoming,
    /// Follow edges in both directions.
    Both,
}


// ── RAII Foreign Key Guard (ISS-015) ──────────────────────

/// RAII guard that disables foreign keys on creation and re-enables them on drop.
/// Guarantees FK re-enablement on all exit paths (success, error, panic).
///
/// Holds a reference to the `RefCell<Connection>` rather than a borrow of the
/// `Connection` itself, so that the caller can still obtain a mutable borrow
/// for `conn.transaction()` while the guard is alive.
struct FkGuard<'a> {
    cell: &'a RefCell<Connection>,
}

impl<'a> FkGuard<'a> {
    /// Disable foreign keys. Returns Err if the PRAGMA fails.
    fn new(cell: &'a RefCell<Connection>) -> Result<Self, rusqlite::Error> {
        cell.borrow().execute_batch("PRAGMA foreign_keys = OFF")?;
        Ok(Self { cell })
    }
}

impl<'a> Drop for FkGuard<'a> {
    fn drop(&mut self) {
        // Re-enable FK; log error on failure but don't panic in Drop
        if let Err(e) = self.cell.borrow().execute_batch("PRAGMA foreign_keys = ON") {
            tracing::error!("Failed to re-enable foreign_keys in FkGuard::drop: {}", e);
        }
    }
}

// ── SqliteStorage struct ───────────────────────────────────

pub struct SqliteStorage {
    conn: RefCell<Connection>,
    path: PathBuf,
}

impl SqliteStorage {
    /// Open (or create) a SQLite database at the given path.
    ///
    /// Runs PRAGMAs for performance and correctness, then applies the schema DDL.
    ///
    /// ## Foreign Key Enforcement (ISS-033)
    ///
    /// `PRAGMA foreign_keys=ON` is **required** — gid relies on FK enforcement
    /// for `ON DELETE CASCADE` (edges, tags, metadata, knowledge auxiliary tables)
    /// and to reject inserts of dangling edges. SQLite's FK PRAGMA is per-connection
    /// state with `OFF` as the default, so it must be explicitly enabled on every
    /// connection. After the PRAGMA is set, this method **verifies** the setting
    /// took effect; a failure returns `StorageError::Configuration` rather than
    /// silently running with FK disabled.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let path = path.into();
        let conn = Connection::open(&path).map_err(|e| StorageError::Sqlite {
            op: StorageOp::Open,
            detail: format!("failed to open database at {}: {}", path.display(), e),
            source: Some(Box::new(e)),
        })?;

        // PRAGMAs — MUST be set per-connection; SQLite does not persist these.
        // foreign_keys=ON is critical: ISS-033 — without it, ON DELETE CASCADE
        // is a no-op and dangling-edge inserts succeed silently.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;
             PRAGMA cache_size=-2000;",
        )?;

        // ISS-033: Verify foreign_keys actually took effect. If something
        // (compile flags, locked DB, downstream override) prevented it, fail
        // loudly at open-time rather than corrupt data later.
        let fk_on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .map_err(|e| StorageError::Sqlite {
                op: StorageOp::Open,
                detail: format!("failed to read foreign_keys pragma: {}", e),
                source: Some(Box::new(e)),
            })?;
        if fk_on != 1 {
            return Err(StorageError::Sqlite {
                op: StorageOp::Open,
                detail: format!(
                    "PRAGMA foreign_keys did not enable (got {}, expected 1) at {} — \
                     gid requires FK enforcement for cascade deletes and dangling-edge \
                     rejection (ISS-033)",
                    fk_on,
                    path.display()
                ),
                source: None,
            });
        }

        // Apply schema
        conn.execute_batch(SCHEMA_SQL)?;

        tracing::debug!("opened SQLite storage at {} (foreign_keys=ON verified)", path.display());

        Ok(Self {
            conn: RefCell::new(conn),
            path,
        })
    }

    /// Return the path to the underlying database file.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    // ── Private helpers ────────────────────────────────────

    /// Load tags, metadata, and knowledge into an already-constructed Node.
    fn load_node_extras(&self, node: &mut Node) -> Result<(), StorageError> {
        let conn = self.conn.borrow();

        // Tags
        let mut tag_stmt = conn.prepare_cached(
            "SELECT tag FROM node_tags WHERE node_id = ?",
        )?;
        let tags: Vec<String> = tag_stmt
            .query_map(params![node.id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        node.tags = tags;

        // Metadata
        let mut meta_stmt = conn.prepare_cached(
            "SELECT key, value FROM node_metadata WHERE node_id = ?",
        )?;
        let meta_rows: Vec<(String, String)> = meta_stmt
            .query_map(params![node.id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut metadata = HashMap::new();
        for (k, v) in meta_rows {
            let val: Value = serde_json::from_str(&v).unwrap_or(Value::String(v));
            metadata.insert(k, val);
        }
        node.metadata = metadata;

        // Knowledge
        let mut know_stmt = conn.prepare_cached(
            "SELECT findings, file_cache, tool_history FROM knowledge WHERE node_id = ?",
        )?;
        let knowledge = know_stmt.query_row(params![node.id], |row| {
            let findings_json: Option<String> = row.get(0)?;
            let file_cache_json: Option<String> = row.get(1)?;
            let tool_history_json: Option<String> = row.get(2)?;
            Ok((findings_json, file_cache_json, tool_history_json))
        });
        match knowledge {
            Ok((findings_json, file_cache_json, tool_history_json)) => {
                node.knowledge = KnowledgeNode {
                    findings: findings_json
                        .as_deref()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_default(),
                    file_cache: file_cache_json
                        .as_deref()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_default(),
                    tool_history: tool_history_json
                        .as_deref()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_default(),
                };
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                node.knowledge = KnowledgeNode::default();
            }
            Err(e) => return Err(e.into()),
        }

        Ok(())
    }

    /// Execute a batch of operations with FK enforcement disabled.
    ///
    /// Used by the migration pipeline to insert nodes and edges atomically,
    /// even when edges reference nodes that don't exist (dangling edges are
    /// migrated as warnings per GOAL-2.9).
    ///
    /// ISS-015: PRAGMA foreign_keys must be set OUTSIDE a transaction. We use
    /// FkGuard RAII to guarantee FK re-enablement on all exit paths.
    pub fn execute_migration_batch(&self, ops: &[BatchOp]) -> Result<(), StorageError> {
        // ISS-015: Disable FK enforcement BEFORE starting transaction.
        // FkGuard borrows the RefCell transiently, so borrow_mut() below is fine.
        let _fk_guard = FkGuard::new(&self.conn).map_err(|e| StorageError::Sqlite {
            op: StorageOp::Migrate,
            detail: format!("failed to disable foreign_keys: {}", e),
            source: Some(Box::new(e)),
        })?;
        
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;

        for op in ops {
            match op {
                BatchOp::PutNode(node) => put_node_on(&tx, node)?,
                BatchOp::DeleteNode(id) => {
                    // ISS-037: DeleteNode must remove incident edges at the op level.
                    // FkGuard disables FK enforcement here, so the engine cannot cascade.
                    // A graph delete = remove vertex AND every incident edge.
                    tx.execute(
                        "DELETE FROM edges WHERE from_node = ? OR to_node = ?",
                        params![id, id],
                    )?;
                    tx.execute("DELETE FROM nodes WHERE id = ?", params![id])?;
                }
                BatchOp::AddEdge(edge) => add_edge_on(&tx, edge)?,
                BatchOp::RemoveEdge { from, to, relation } => {
                    remove_edge_on(&tx, from, to, relation)?;
                }
                BatchOp::SetTags(node_id, tags) => set_tags_on(&tx, node_id, tags)?,
                BatchOp::SetMetadata(node_id, metadata) => {
                    set_metadata_on(&tx, node_id, metadata)?;
                }
                BatchOp::SetKnowledge(node_id, knowledge) => {
                    set_knowledge_on(&tx, node_id, knowledge)?;
                }
            }
        }

        tx.commit()?;
        // FkGuard re-enables FK on drop
        tracing::debug!(ops_count = ops.len(), "execute_migration_batch committed (FK-off via RAII guard)");
        Ok(())
    }

    /// BFS k-hop neighbor query using a recursive CTE.
    ///
    /// Returns all nodes reachable within `depth` hops from node `id`,
    /// following edges in the specified `direction`. The root node itself
    /// is included in the result (at depth 0).
    ///
    /// - `depth = 0` returns only the root node.
    /// - Maximum depth is capped at 10 to prevent runaway CTEs on large graphs.
    ///
    /// This is an **inherent method** (not on `GraphStorage` trait) because
    /// recursive CTEs are a SQL-specific feature. Used internally by the
    /// context assembly pipeline.
    pub fn neighbors(
        &self,
        id: &str,
        depth: usize,
        direction: Direction,
    ) -> Result<Vec<Node>, StorageError> {
        let conn = self.conn.borrow();
        let effective_depth = depth.min(10);

        let sql = match direction {
            Direction::Outgoing => {
                "WITH RECURSIVE hop(nid, d) AS (
                     VALUES(?1, 0)
                   UNION
                     SELECT e.to_node, hop.d + 1
                     FROM edges e
                     JOIN hop ON e.from_node = hop.nid
                     WHERE hop.d < ?2
                 )
                 SELECT DISTINCT n.* FROM hop
                 JOIN nodes n ON n.id = hop.nid"
            }
            Direction::Incoming => {
                "WITH RECURSIVE hop(nid, d) AS (
                     VALUES(?1, 0)
                   UNION
                     SELECT e.from_node, hop.d + 1
                     FROM edges e
                     JOIN hop ON e.to_node = hop.nid
                     WHERE hop.d < ?2
                 )
                 SELECT DISTINCT n.* FROM hop
                 JOIN nodes n ON n.id = hop.nid"
            }
            Direction::Both => {
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
            }
        };

        let mut stmt = conn.prepare(sql)?;
        let nodes: Vec<Node> = stmt
            .query_map(params![id, effective_depth as i64], row_to_node)?
            .collect::<Result<Vec<_>, _>>()?;

        drop(stmt);
        drop(conn);

        let mut nodes = nodes;
        for node in &mut nodes {
            self.load_node_extras(node)?;
        }
        Ok(nodes)
    }
}

// ── row_to_node helper ─────────────────────────────────────

fn row_to_node(row: &rusqlite::Row) -> rusqlite::Result<Node> {
    let status_str: Option<String> = row.get(2)?;
    let status = status_str
        .as_deref()
        .and_then(|s| s.parse::<NodeStatus>().ok())
        .unwrap_or(NodeStatus::Todo);

    let priority_raw: Option<i64> = row.get(17)?;
    let priority = priority_raw.map(|p| p.clamp(0, 255) as u8);

    let start_line_raw: Option<i64> = row.get(7)?;
    let start_line = start_line_raw.map(|v| v.max(0) as usize);

    let end_line_raw: Option<i64> = row.get(8)?;
    let end_line = end_line_raw.map(|v| v.max(0) as usize);

    let depth_raw: Option<i64> = row.get(20)?;
    let depth = depth_raw.map(|v| v.max(0) as u32);

    let is_public_raw: Option<i64> = row.get(22)?;
    let is_public = is_public_raw.map(|v| v != 0);

    let node_type_raw: Option<String> = row.get(4)?;

    Ok(Node {
        id: row.get(0)?,
        title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        status,
        description: row.get(3)?,
        node_type: node_type_raw,
        file_path: row.get(5)?,
        lang: row.get(6)?,
        start_line,
        end_line,
        signature: row.get(9)?,
        visibility: row.get(10)?,
        doc_comment: row.get(11)?,
        body_hash: row.get(12)?,
        node_kind: row.get(13)?,
        owner: row.get(14)?,
        source: row.get(15)?,
        repo: row.get(16)?,
        priority,
        assigned_to: row.get(18)?,
        parent_id: row.get(19)?,
        depth,
        complexity: row.get(21)?,
        is_public,
        body: row.get(23)?,
        created_at: row.get(24)?,
        updated_at: row.get(25)?,
        // Loaded separately via load_node_extras
        tags: Vec::new(),
        metadata: HashMap::new(),
        knowledge: KnowledgeNode::default(),
    })
}

// ── Helper: execute put_node on a connection or transaction ─

fn put_node_on<C: std::ops::Deref<Target = Connection>>(
    conn: &C,
    node: &Node,
) -> Result<(), StorageError> {
    conn.execute(
        "INSERT OR REPLACE INTO nodes (
            id, title, status, description, node_type,
            file_path, lang, start_line, end_line, signature,
            visibility, doc_comment, body_hash, node_kind,
            owner, source, repo, priority, assigned_to,
            parent_id, depth, complexity, is_public,
            body, created_at, updated_at
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5,
            ?6, ?7, ?8, ?9, ?10,
            ?11, ?12, ?13, ?14,
            ?15, ?16, ?17, ?18, ?19,
            ?20, ?21, ?22, ?23,
            ?24, ?25, ?26
        )",
        params![
            node.id,
            node.title,
            node.status.to_string(),
            node.description,
            node.node_type.as_deref().unwrap_or("unknown"),
            node.file_path,
            node.lang,
            node.start_line.map(|v| v as i64),
            node.end_line.map(|v| v as i64),
            node.signature,
            node.visibility,
            node.doc_comment,
            node.body_hash,
            node.node_kind,
            node.owner,
            node.source,
            node.repo,
            node.priority.map(|p| p as i64),
            node.assigned_to,
            node.parent_id,
            node.depth.map(|v| v as i64),
            node.complexity,
            node.is_public.map(|b| if b { 1i64 } else { 0 }),
            node.body,
            node.created_at,
            node.updated_at,
        ],
    )?;

    // Sync tags
    conn.execute("DELETE FROM node_tags WHERE node_id = ?", params![node.id])?;
    for tag in &node.tags {
        conn.execute(
            "INSERT INTO node_tags (node_id, tag) VALUES (?, ?)",
            params![node.id, tag],
        )?;
    }

    // Sync metadata
    conn.execute(
        "DELETE FROM node_metadata WHERE node_id = ?",
        params![node.id],
    )?;
    for (key, value) in &node.metadata {
        let value_str = serde_json::to_string(value)
            .unwrap_or_else(|_| value.to_string());
        conn.execute(
            "INSERT INTO node_metadata (node_id, key, value) VALUES (?, ?, ?)",
            params![node.id, key, value_str],
        )?;
    }

    // Sync knowledge (only if non-empty)
    if !node.knowledge.is_empty() {
        let findings = serde_json::to_string(&node.knowledge.findings)?;
        let file_cache = serde_json::to_string(&node.knowledge.file_cache)?;
        let tool_history = serde_json::to_string(&node.knowledge.tool_history)?;
        conn.execute(
            "INSERT OR REPLACE INTO knowledge (node_id, findings, file_cache, tool_history) VALUES (?, ?, ?, ?)",
            params![node.id, findings, file_cache, tool_history],
        )?;
    } else {
        conn.execute(
            "DELETE FROM knowledge WHERE node_id = ?",
            params![node.id],
        )?;
    }

    Ok(())
}

fn set_tags_on<C: std::ops::Deref<Target = Connection>>(
    conn: &C,
    node_id: &str,
    tags: &[String],
) -> Result<(), StorageError> {
    conn.execute("DELETE FROM node_tags WHERE node_id = ?", params![node_id])?;
    for tag in tags {
        conn.execute(
            "INSERT INTO node_tags (node_id, tag) VALUES (?, ?)",
            params![node_id, tag],
        )?;
    }
    Ok(())
}

fn set_metadata_on<C: std::ops::Deref<Target = Connection>>(
    conn: &C,
    node_id: &str,
    metadata: &HashMap<String, Value>,
) -> Result<(), StorageError> {
    conn.execute(
        "DELETE FROM node_metadata WHERE node_id = ?",
        params![node_id],
    )?;
    for (key, value) in metadata {
        let value_str =
            serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
        conn.execute(
            "INSERT INTO node_metadata (node_id, key, value) VALUES (?, ?, ?)",
            params![node_id, key, value_str],
        )?;
    }
    Ok(())
}

fn set_knowledge_on<C: std::ops::Deref<Target = Connection>>(
    conn: &C,
    node_id: &str,
    knowledge: &KnowledgeNode,
) -> Result<(), StorageError> {
    let findings = serde_json::to_string(&knowledge.findings)?;
    let file_cache = serde_json::to_string(&knowledge.file_cache)?;
    let tool_history = serde_json::to_string(&knowledge.tool_history)?;
    conn.execute(
        "INSERT OR REPLACE INTO knowledge (node_id, findings, file_cache, tool_history) VALUES (?, ?, ?, ?)",
        params![node_id, findings, file_cache, tool_history],
    )?;
    Ok(())
}

fn add_edge_on<C: std::ops::Deref<Target = Connection>>(
    conn: &C,
    edge: &Edge,
) -> Result<(), StorageError> {
    let metadata_json = edge
        .metadata
        .as_ref()
        .map(|m| serde_json::to_string(m).unwrap_or_else(|_| "null".to_string()));
    conn.execute(
        "INSERT INTO edges (from_node, to_node, relation, weight, confidence, metadata) VALUES (?, ?, ?, ?, ?, ?)",
        params![
            edge.from,
            edge.to,
            edge.relation,
            edge.weight,
            edge.confidence,
            metadata_json,
        ],
    )?;
    Ok(())
}

fn remove_edge_on<C: std::ops::Deref<Target = Connection>>(
    conn: &C,
    from: &str,
    to: &str,
    relation: &str,
) -> Result<(), StorageError> {
    conn.execute(
        "DELETE FROM edges WHERE from_node = ? AND to_node = ? AND relation = ?",
        params![from, to, relation],
    )?;
    Ok(())
}

// ── GraphStorage implementation ────────────────────────────

impl GraphStorage for SqliteStorage {
    fn put_node(&self, node: &Node) -> Result<(), StorageError> {
        let conn = self.conn.borrow();
        put_node_on(&conn, node)?;
        tracing::debug!(node_id = %node.id, "put_node");
        Ok(())
    }

    fn get_node(&self, id: &str) -> Result<Option<Node>, StorageError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare_cached("SELECT * FROM nodes WHERE id = ?")?;
        let result = stmt.query_row(params![id], row_to_node);
        match result {
            Ok(mut node) => {
                drop(stmt);
                drop(conn);
                self.load_node_extras(&mut node)?;
                Ok(Some(node))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn delete_node(&self, id: &str) -> Result<(), StorageError> {
        let conn = self.conn.borrow();
        conn.execute("DELETE FROM nodes WHERE id = ?", params![id])?;
        tracing::debug!(node_id = %id, "delete_node");
        Ok(())
    }

    fn get_edges(&self, node_id: &str) -> Result<Vec<Edge>, StorageError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare_cached(
            "SELECT from_node, to_node, relation, weight, confidence, metadata FROM edges WHERE from_node = ? OR to_node = ?",
        )?;
        let edges = stmt
            .query_map(params![node_id, node_id], |row| {
                let metadata_str: Option<String> = row.get(5)?;
                let metadata: Option<Value> = metadata_str
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok());
                Ok(Edge {
                    from: row.get(0)?,
                    to: row.get(1)?,
                    relation: row.get(2)?,
                    weight: row.get(3)?,
                    confidence: row.get(4)?,
                    metadata,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(edges)
    }

    fn add_edge(&self, edge: &Edge) -> Result<(), StorageError> {
        let conn = self.conn.borrow();
        add_edge_on(&conn, edge)?;
        tracing::debug!(from = %edge.from, to = %edge.to, relation = %edge.relation, "add_edge");
        Ok(())
    }

    fn remove_edge(&self, from: &str, to: &str, relation: &str) -> Result<(), StorageError> {
        let conn = self.conn.borrow();
        remove_edge_on(&conn, from, to, relation)?;
        tracing::debug!(%from, %to, %relation, "remove_edge");
        Ok(())
    }

    fn query_nodes(&self, filter: &NodeFilter) -> Result<Vec<Node>, StorageError> {
        let conn = self.conn.borrow();

        let mut sql = String::from("SELECT DISTINCT n.* FROM nodes n");
        let mut conditions: Vec<String> = Vec::new();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        // Join with node_tags if tag filter is present
        if filter.tag.is_some() {
            sql.push_str(" JOIN node_tags t ON n.id = t.node_id");
        }

        sql.push_str(" WHERE 1=1");

        if let Some(ref nt) = filter.node_type {
            conditions.push("n.node_type = ?".to_string());
            param_values.push(Box::new(nt.clone()));
        }

        if let Some(ref status) = filter.status {
            conditions.push("n.status = ?".to_string());
            param_values.push(Box::new(status.clone()));
        }

        if let Some(ref fp) = filter.file_path {
            conditions.push("n.file_path LIKE ?".to_string());
            param_values.push(Box::new(format!("{}%", fp)));
        }

        if let Some(ref tag) = filter.tag {
            conditions.push("t.tag = ?".to_string());
            param_values.push(Box::new(tag.clone()));
        }

        if let Some(ref owner) = filter.owner {
            conditions.push("n.owner = ?".to_string());
            param_values.push(Box::new(owner.clone()));
        }

        for cond in &conditions {
            sql.push_str(" AND ");
            sql.push_str(cond);
        }

        if let Some(limit) = filter.limit {
            sql.push_str(&format!(" LIMIT {}", limit));
        }

        if let Some(offset) = filter.offset {
            // OFFSET requires LIMIT; default to large number if not specified
            if filter.limit.is_none() {
                sql.push_str(" LIMIT -1");
            }
            sql.push_str(&format!(" OFFSET {}", offset));
        }

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn.prepare(&sql)?;
        let node_ids: Vec<Node> = stmt
            .query_map(param_refs.as_slice(), row_to_node)?
            .collect::<Result<Vec<_>, _>>()?;

        drop(stmt);
        drop(conn);

        let mut nodes = node_ids;
        for node in &mut nodes {
            self.load_node_extras(node)?;
        }
        Ok(nodes)
    }

    fn search(&self, query: &str) -> Result<Vec<Node>, StorageError> {
        let conn = self.conn.borrow();
        // Sanitize query for FTS5: wrap in double quotes for literal matching
        let sanitized = format!("\"{}\"", query.replace('"', "\"\""));

        let mut stmt = conn.prepare_cached(
            "SELECT n.* FROM nodes n JOIN nodes_fts f ON n.rowid = f.rowid WHERE nodes_fts MATCH ? ORDER BY rank",
        )?;
        let nodes: Vec<Node> = stmt
            .query_map(params![sanitized], row_to_node)?
            .collect::<Result<Vec<_>, _>>()?;

        drop(stmt);
        drop(conn);

        let mut nodes = nodes;
        for node in &mut nodes {
            self.load_node_extras(node)?;
        }
        Ok(nodes)
    }

    fn get_tags(&self, node_id: &str) -> Result<Vec<String>, StorageError> {
        let conn = self.conn.borrow();
        let mut stmt =
            conn.prepare_cached("SELECT tag FROM node_tags WHERE node_id = ?")?;
        let tags = stmt
            .query_map(params![node_id], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(tags)
    }

    fn set_tags(&self, node_id: &str, tags: &[String]) -> Result<(), StorageError> {
        let conn = self.conn.borrow();
        set_tags_on(&conn, node_id, tags)?;
        tracing::debug!(node_id = %node_id, count = tags.len(), "set_tags");
        Ok(())
    }

    fn get_metadata(&self, node_id: &str) -> Result<HashMap<String, Value>, StorageError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare_cached(
            "SELECT key, value FROM node_metadata WHERE node_id = ?",
        )?;
        let rows: Vec<(String, String)> = stmt
            .query_map(params![node_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut metadata = HashMap::new();
        for (k, v) in rows {
            let val: Value = serde_json::from_str(&v).unwrap_or(Value::String(v));
            metadata.insert(k, val);
        }
        Ok(metadata)
    }

    fn set_metadata(
        &self,
        node_id: &str,
        metadata: &HashMap<String, Value>,
    ) -> Result<(), StorageError> {
        let conn = self.conn.borrow();
        set_metadata_on(&conn, node_id, metadata)?;
        tracing::debug!(node_id = %node_id, count = metadata.len(), "set_metadata");
        Ok(())
    }

    fn get_project_meta(&self) -> Result<Option<ProjectMeta>, StorageError> {
        let conn = self.conn.borrow();
        let name: Result<String, _> = conn.query_row(
            "SELECT value FROM config WHERE key = 'project_name'",
            [],
            |row| row.get(0),
        );
        match name {
            Ok(name) => {
                let description: Option<String> = conn
                    .query_row(
                        "SELECT value FROM config WHERE key = 'project_description'",
                        [],
                        |row| row.get(0),
                    )
                    .ok();
                Ok(Some(ProjectMeta { name, description }))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn set_project_meta(&self, meta: &ProjectMeta) -> Result<(), StorageError> {
        let conn = self.conn.borrow();
        conn.execute(
            "INSERT OR REPLACE INTO config (key, value) VALUES ('project_name', ?)",
            params![meta.name],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO config (key, value) VALUES ('project_description', ?)",
            params![meta.description.as_deref().unwrap_or("")],
        )?;
        tracing::debug!(project = %meta.name, "set_project_meta");
        Ok(())
    }

    fn get_knowledge(&self, node_id: &str) -> Result<Option<KnowledgeNode>, StorageError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare_cached(
            "SELECT findings, file_cache, tool_history FROM knowledge WHERE node_id = ?",
        )?;
        let result = stmt.query_row(params![node_id], |row| {
            let findings_json: Option<String> = row.get(0)?;
            let file_cache_json: Option<String> = row.get(1)?;
            let tool_history_json: Option<String> = row.get(2)?;
            Ok((findings_json, file_cache_json, tool_history_json))
        });
        match result {
            Ok((findings_json, file_cache_json, tool_history_json)) => {
                Ok(Some(KnowledgeNode {
                    findings: findings_json
                        .as_deref()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_default(),
                    file_cache: file_cache_json
                        .as_deref()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_default(),
                    tool_history: tool_history_json
                        .as_deref()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_default(),
                }))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn set_knowledge(
        &self,
        node_id: &str,
        knowledge: &KnowledgeNode,
    ) -> Result<(), StorageError> {
        let conn = self.conn.borrow();
        set_knowledge_on(&conn, node_id, knowledge)?;
        tracing::debug!(node_id = %node_id, "set_knowledge");
        Ok(())
    }

    fn get_node_count(&self) -> Result<usize, StorageError> {
        let conn = self.conn.borrow();
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    fn get_edge_count(&self) -> Result<usize, StorageError> {
        let conn = self.conn.borrow();
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    fn get_all_node_ids(&self) -> Result<Vec<String>, StorageError> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare_cached("SELECT id FROM nodes")?;
        let ids = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(ids)
    }

    fn execute_batch(&self, ops: &[BatchOp]) -> Result<(), StorageError> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;

        for op in ops {
            match op {
                BatchOp::PutNode(node) => {
                    put_node_on(&tx, node)?;
                }
                BatchOp::DeleteNode(id) => {
                    // ISS-037: DeleteNode is responsible for V *and* E.
                    // ISS-033 showed FK cascade is unreliable across schema/connection
                    // setups; explicit edge cleanup is the correct semantic contract.
                    tx.execute(
                        "DELETE FROM edges WHERE from_node = ? OR to_node = ?",
                        params![id, id],
                    )?;
                    tx.execute("DELETE FROM nodes WHERE id = ?", params![id])?;
                }
                BatchOp::AddEdge(edge) => {
                    add_edge_on(&tx, edge)?;
                }
                BatchOp::RemoveEdge {
                    from,
                    to,
                    relation,
                } => {
                    remove_edge_on(&tx, from, to, relation)?;
                }
                BatchOp::SetTags(node_id, tags) => {
                    set_tags_on(&tx, node_id, tags)?;
                }
                BatchOp::SetMetadata(node_id, metadata) => {
                    set_metadata_on(&tx, node_id, metadata)?;
                }
                BatchOp::SetKnowledge(node_id, knowledge) => {
                    set_knowledge_on(&tx, node_id, knowledge)?;
                }
            }
        }

        tx.commit()?;
        tracing::debug!(ops_count = ops.len(), "execute_batch committed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn temp_storage() -> SqliteStorage {
        let tmp = NamedTempFile::new().unwrap();
        SqliteStorage::open(tmp.path()).unwrap()
    }

    #[test]
    fn test_open_and_schema() {
        let storage = temp_storage();
        assert_eq!(storage.get_node_count().unwrap(), 0);
        assert_eq!(storage.get_edge_count().unwrap(), 0);
    }

    #[test]
    fn test_put_get_node() {
        let storage = temp_storage();
        let node = Node::new("n1", "Test Node")
            .with_description("A test node")
            .with_status(NodeStatus::InProgress)
            .with_tags(vec!["tag1".into(), "tag2".into()])
            .with_priority(5);

        storage.put_node(&node).unwrap();
        let loaded = storage.get_node("n1").unwrap().expect("node not found");
        assert_eq!(loaded.id, "n1");
        assert_eq!(loaded.title, "Test Node");
        assert_eq!(loaded.status, NodeStatus::InProgress);
        assert_eq!(loaded.description.as_deref(), Some("A test node"));
        assert_eq!(loaded.priority, Some(5));
        assert_eq!(loaded.tags, vec!["tag1", "tag2"]);
    }

    #[test]
    fn test_delete_node() {
        let storage = temp_storage();
        storage.put_node(&Node::new("n1", "Node")).unwrap();
        assert_eq!(storage.get_node_count().unwrap(), 1);
        storage.delete_node("n1").unwrap();
        assert_eq!(storage.get_node_count().unwrap(), 0);
        assert!(storage.get_node("n1").unwrap().is_none());
    }

    #[test]
    fn test_edges() {
        let storage = temp_storage();
        storage.put_node(&Node::new("a", "A")).unwrap();
        storage.put_node(&Node::new("b", "B")).unwrap();

        let edge = Edge::new("a", "b", "depends_on");
        storage.add_edge(&edge).unwrap();

        let edges = storage.get_edges("a").unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from, "a");
        assert_eq!(edges[0].to, "b");
        assert_eq!(edges[0].relation, "depends_on");

        storage.remove_edge("a", "b", "depends_on").unwrap();
        assert_eq!(storage.get_edges("a").unwrap().len(), 0);
    }

    #[test]
    fn test_query_nodes() {
        let storage = temp_storage();
        let mut n1 = Node::new("n1", "Task 1");
        n1.node_type = Some("task".into());
        n1.status = NodeStatus::Todo;
        storage.put_node(&n1).unwrap();

        let mut n2 = Node::new("n2", "Task 2");
        n2.node_type = Some("task".into());
        n2.status = NodeStatus::Done;
        storage.put_node(&n2).unwrap();

        let mut n3 = Node::new("n3", "File 1");
        n3.node_type = Some("file".into());
        storage.put_node(&n3).unwrap();

        // Filter by type
        let results = storage
            .query_nodes(&NodeFilter::new().with_node_type("task"))
            .unwrap();
        assert_eq!(results.len(), 2);

        // Filter by status
        let results = storage
            .query_nodes(&NodeFilter::new().with_status("done"))
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "n2");

        // Limit
        let results = storage
            .query_nodes(&NodeFilter::new().with_limit(1))
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search() {
        let storage = temp_storage();
        let mut n1 = Node::new("n1", "Implement authentication");
        n1.description = Some("Add OAuth2 login flow".into());
        storage.put_node(&n1).unwrap();

        let results = storage.search("authentication").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "n1");

        let results = storage.search("nonexistent").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_tags() {
        let storage = temp_storage();
        storage.put_node(&Node::new("n1", "Node")).unwrap();
        storage
            .set_tags("n1", &["rust".into(), "backend".into()])
            .unwrap();
        let tags = storage.get_tags("n1").unwrap();
        assert_eq!(tags.len(), 2);
        assert!(tags.contains(&"rust".to_string()));
        assert!(tags.contains(&"backend".to_string()));
    }

    #[test]
    fn test_metadata() {
        let storage = temp_storage();
        storage.put_node(&Node::new("n1", "Node")).unwrap();
        let mut meta = HashMap::new();
        meta.insert("key1".into(), Value::String("value1".into()));
        meta.insert("key2".into(), serde_json::json!(42));
        storage.set_metadata("n1", &meta).unwrap();

        let loaded = storage.get_metadata("n1").unwrap();
        assert_eq!(loaded.get("key1"), Some(&Value::String("value1".into())));
        assert_eq!(loaded.get("key2"), Some(&serde_json::json!(42)));
    }

    #[test]
    fn test_project_meta() {
        let storage = temp_storage();
        assert!(storage.get_project_meta().unwrap().is_none());

        let meta = ProjectMeta {
            name: "test-project".into(),
            description: Some("A test project".into()),
        };
        storage.set_project_meta(&meta).unwrap();

        let loaded = storage.get_project_meta().unwrap().unwrap();
        assert_eq!(loaded.name, "test-project");
        assert_eq!(loaded.description.as_deref(), Some("A test project"));
    }

    #[test]
    fn test_knowledge() {
        let storage = temp_storage();
        storage.put_node(&Node::new("n1", "Node")).unwrap();

        assert!(storage.get_knowledge("n1").unwrap().is_none());

        let mut knowledge = KnowledgeNode::default();
        knowledge.findings.insert("key".into(), "value".into());
        storage.set_knowledge("n1", &knowledge).unwrap();

        let loaded = storage.get_knowledge("n1").unwrap().unwrap();
        assert_eq!(loaded.findings.get("key").unwrap(), "value");
    }

    #[test]
    fn test_batch_ops() {
        let storage = temp_storage();
        let ops = vec![
            BatchOp::PutNode(Node::new("b1", "Batch 1")),
            BatchOp::PutNode(Node::new("b2", "Batch 2")),
            BatchOp::AddEdge(Edge::new("b1", "b2", "depends_on")),
            BatchOp::SetTags("b1".into(), vec!["batched".into()]),
        ];
        storage.execute_batch(&ops).unwrap();

        assert_eq!(storage.get_node_count().unwrap(), 2);
        assert_eq!(storage.get_edge_count().unwrap(), 1);
        assert_eq!(storage.get_tags("b1").unwrap(), vec!["batched"]);
    }

    #[test]
    fn test_get_all_node_ids() {
        let storage = temp_storage();
        storage.put_node(&Node::new("x", "X")).unwrap();
        storage.put_node(&Node::new("y", "Y")).unwrap();
        let mut ids = storage.get_all_node_ids().unwrap();
        ids.sort();
        assert_eq!(ids, vec!["x", "y"]);
    }

    // ── neighbors() BFS tests ──────────────────────────────

    /// Build a linear graph: A → B → C → D
    fn setup_linear_graph() -> SqliteStorage {
        let s = temp_storage();
        for id in &["a", "b", "c", "d"] {
            s.put_node(&Node::new(id, &id.to_uppercase())).unwrap();
        }
        s.add_edge(&Edge::new("a", "b", "depends_on")).unwrap();
        s.add_edge(&Edge::new("b", "c", "depends_on")).unwrap();
        s.add_edge(&Edge::new("c", "d", "depends_on")).unwrap();
        s
    }

    /// Build a diamond graph: A → B, A → C, B → D, C → D
    fn setup_diamond_graph() -> SqliteStorage {
        let s = temp_storage();
        for id in &["a", "b", "c", "d"] {
            s.put_node(&Node::new(id, &id.to_uppercase())).unwrap();
        }
        s.add_edge(&Edge::new("a", "b", "depends_on")).unwrap();
        s.add_edge(&Edge::new("a", "c", "depends_on")).unwrap();
        s.add_edge(&Edge::new("b", "d", "depends_on")).unwrap();
        s.add_edge(&Edge::new("c", "d", "depends_on")).unwrap();
        s
    }

    #[test]
    fn test_neighbors_depth_zero_returns_self() {
        let s = setup_linear_graph();
        let result = s.neighbors("a", 0, Direction::Both).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "a");
    }

    #[test]
    fn test_neighbors_outgoing_depth_1() {
        let s = setup_linear_graph();
        let result = s.neighbors("a", 1, Direction::Outgoing).unwrap();
        let mut ids: Vec<&str> = result.iter().map(|n| n.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn test_neighbors_outgoing_depth_2() {
        let s = setup_linear_graph();
        let result = s.neighbors("a", 2, Direction::Outgoing).unwrap();
        let mut ids: Vec<&str> = result.iter().map(|n| n.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_neighbors_outgoing_full_chain() {
        let s = setup_linear_graph();
        let result = s.neighbors("a", 10, Direction::Outgoing).unwrap();
        let mut ids: Vec<&str> = result.iter().map(|n| n.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn test_neighbors_incoming_depth_1() {
        let s = setup_linear_graph();
        // D has incoming edge from C
        let result = s.neighbors("d", 1, Direction::Incoming).unwrap();
        let mut ids: Vec<&str> = result.iter().map(|n| n.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["c", "d"]);
    }

    #[test]
    fn test_neighbors_incoming_full_chain() {
        let s = setup_linear_graph();
        // Walking backwards from D: D ← C ← B ← A
        let result = s.neighbors("d", 10, Direction::Incoming).unwrap();
        let mut ids: Vec<&str> = result.iter().map(|n| n.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn test_neighbors_outgoing_leaf_node() {
        let s = setup_linear_graph();
        // D is a leaf — no outgoing edges
        let result = s.neighbors("d", 5, Direction::Outgoing).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "d");
    }

    #[test]
    fn test_neighbors_incoming_root_node() {
        let s = setup_linear_graph();
        // A is a root — no incoming edges
        let result = s.neighbors("a", 5, Direction::Incoming).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "a");
    }

    #[test]
    fn test_neighbors_both_from_middle() {
        let s = setup_linear_graph();
        // B can reach A (incoming) and C (outgoing) at depth 1
        let result = s.neighbors("b", 1, Direction::Both).unwrap();
        let mut ids: Vec<&str> = result.iter().map(|n| n.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_neighbors_both_full_reach() {
        let s = setup_linear_graph();
        // From B, with enough depth, should reach all nodes in both directions
        let result = s.neighbors("b", 10, Direction::Both).unwrap();
        let mut ids: Vec<&str> = result.iter().map(|n| n.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn test_neighbors_diamond_outgoing() {
        let s = setup_diamond_graph();
        // A → B and A → C at depth 1, then B → D and C → D at depth 2
        let result = s.neighbors("a", 2, Direction::Outgoing).unwrap();
        let mut ids: Vec<&str> = result.iter().map(|n| n.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn test_neighbors_diamond_incoming_from_d() {
        let s = setup_diamond_graph();
        // D ← B and D ← C at depth 1; B ← A and C ← A at depth 2
        let result = s.neighbors("d", 2, Direction::Incoming).unwrap();
        let mut ids: Vec<&str> = result.iter().map(|n| n.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn test_neighbors_nonexistent_node() {
        let s = setup_linear_graph();
        let result = s.neighbors("zzz", 5, Direction::Both).unwrap();
        // Node doesn't exist in nodes table, so JOIN yields nothing
        assert!(result.is_empty());
    }

    #[test]
    fn test_neighbors_depth_capped_at_10() {
        let s = setup_linear_graph();
        // depth=100 should behave same as depth=10 (cap)
        let r1 = s.neighbors("a", 100, Direction::Outgoing).unwrap();
        let r2 = s.neighbors("a", 10, Direction::Outgoing).unwrap();
        let mut ids1: Vec<&str> = r1.iter().map(|n| n.id.as_str()).collect();
        let mut ids2: Vec<&str> = r2.iter().map(|n| n.id.as_str()).collect();
        ids1.sort();
        ids2.sort();
        assert_eq!(ids1, ids2);
    }

    #[test]
    fn test_neighbors_loads_extras() {
        let s = temp_storage();
        let node = Node::new("n1", "Node One")
            .with_tags(vec!["important".into()]);
        s.put_node(&node).unwrap();
        s.put_node(&Node::new("n2", "Node Two")).unwrap();
        s.add_edge(&Edge::new("n1", "n2", "depends_on")).unwrap();

        let result = s.neighbors("n1", 1, Direction::Outgoing).unwrap();
        let n1 = result.iter().find(|n| n.id == "n1").unwrap();
        assert_eq!(n1.tags, vec!["important"]);
    }

    #[test]
    fn test_neighbors_mixed_relations() {
        // Edges with different relation types should all be traversed
        let s = temp_storage();
        for id in &["a", "b", "c"] {
            s.put_node(&Node::new(id, &id.to_uppercase())).unwrap();
        }
        s.add_edge(&Edge::new("a", "b", "depends_on")).unwrap();
        s.add_edge(&Edge::new("a", "c", "calls")).unwrap();

        let result = s.neighbors("a", 1, Direction::Outgoing).unwrap();
        let mut ids: Vec<&str> = result.iter().map(|n| n.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_neighbors_isolated_node() {
        let s = temp_storage();
        s.put_node(&Node::new("lonely", "Lonely Node")).unwrap();
        let result = s.neighbors("lonely", 5, Direction::Both).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "lonely");
    }

    // ── CRUD edge cases ────────────────────────────────────

    #[test]
    fn test_put_node_all_fields() {
        let s = temp_storage();
        let mut node = Node::new("full", "Fully Populated Node");
        node.status = NodeStatus::InProgress;
        node.description = Some("A comprehensive test node".into());
        node.node_type = Some("function".into());
        node.file_path = Some("src/storage/sqlite.rs".into());
        node.lang = Some("rust".into());
        node.start_line = Some(42);
        node.end_line = Some(100);
        node.signature = Some("fn do_stuff(&self) -> Result<()>".into());
        node.visibility = Some("pub".into());
        node.doc_comment = Some("/// Does important stuff".into());
        node.body_hash = Some("abc123def456".into());
        node.node_kind = Some("method".into());
        node.owner = Some("potato".into());
        node.source = Some("code_extract".into());
        node.repo = Some("gid-rs".into());
        node.priority = Some(3);
        node.assigned_to = Some("rustclaw".into());
        node.parent_id = Some("parent-mod".into());
        node.depth = Some(2);
        node.complexity = Some(4.5);
        node.is_public = Some(true);
        node.body = Some("fn do_stuff(&self) -> Result<()> { Ok(()) }".into());
        node.created_at = Some("2026-04-07T12:00:00Z".into());
        node.updated_at = Some("2026-04-08T01:00:00Z".into());
        node.tags = vec!["important".into(), "tested".into()];
        node.metadata.insert("design_ref".into(), serde_json::json!("3.2"));
        node.knowledge = KnowledgeNode {
            findings: {
                let mut f = HashMap::new();
                f.insert("f1".into(), "found something".into());
                f
            },
            file_cache: HashMap::new(),
            tool_history: vec![],
        };

        s.put_node(&node).unwrap();
        let loaded = s.get_node("full").unwrap().expect("node should exist");

        assert_eq!(loaded.id, "full");
        assert_eq!(loaded.title, "Fully Populated Node");
        assert_eq!(loaded.status, NodeStatus::InProgress);
        assert_eq!(loaded.description.as_deref(), Some("A comprehensive test node"));
        assert_eq!(loaded.node_type.as_deref(), Some("function"));
        assert_eq!(loaded.file_path.as_deref(), Some("src/storage/sqlite.rs"));
        assert_eq!(loaded.lang.as_deref(), Some("rust"));
        assert_eq!(loaded.start_line, Some(42));
        assert_eq!(loaded.end_line, Some(100));
        assert_eq!(loaded.signature.as_deref(), Some("fn do_stuff(&self) -> Result<()>"));
        assert_eq!(loaded.visibility.as_deref(), Some("pub"));
        assert_eq!(loaded.doc_comment.as_deref(), Some("/// Does important stuff"));
        assert_eq!(loaded.body_hash.as_deref(), Some("abc123def456"));
        assert_eq!(loaded.node_kind.as_deref(), Some("method"));
        assert_eq!(loaded.owner.as_deref(), Some("potato"));
        assert_eq!(loaded.source.as_deref(), Some("code_extract"));
        assert_eq!(loaded.repo.as_deref(), Some("gid-rs"));
        assert_eq!(loaded.priority, Some(3));
        assert_eq!(loaded.assigned_to.as_deref(), Some("rustclaw"));
        assert_eq!(loaded.parent_id.as_deref(), Some("parent-mod"));
        assert_eq!(loaded.depth, Some(2));
        assert_eq!(loaded.complexity, Some(4.5));
        assert_eq!(loaded.is_public, Some(true));
        assert_eq!(loaded.body.as_deref(), Some("fn do_stuff(&self) -> Result<()> { Ok(()) }"));
        assert_eq!(loaded.created_at.as_deref(), Some("2026-04-07T12:00:00Z"));
        assert_eq!(loaded.updated_at.as_deref(), Some("2026-04-08T01:00:00Z"));
        assert_eq!(loaded.tags.len(), 2);
        assert!(loaded.tags.contains(&"important".to_string()));
        assert!(loaded.tags.contains(&"tested".to_string()));
        assert_eq!(loaded.metadata.get("design_ref"), Some(&serde_json::json!("3.2")));
        assert_eq!(loaded.knowledge.findings.get("f1").unwrap(), "found something");
    }

    #[test]
    fn test_put_node_upsert() {
        let s = temp_storage();
        let node = Node::new("u1", "Original Title")
            .with_status(NodeStatus::Todo)
            .with_description("original desc");
        s.put_node(&node).unwrap();

        // Update via upsert
        let updated = Node::new("u1", "Updated Title")
            .with_status(NodeStatus::Done)
            .with_description("updated desc");
        s.put_node(&updated).unwrap();

        let loaded = s.get_node("u1").unwrap().unwrap();
        assert_eq!(loaded.title, "Updated Title");
        assert_eq!(loaded.status, NodeStatus::Done);
        assert_eq!(loaded.description.as_deref(), Some("updated desc"));
        assert_eq!(s.get_node_count().unwrap(), 1); // still just one node
    }

    #[test]
    fn test_put_node_minimal() {
        let s = temp_storage();
        s.put_node(&Node::new("min", "Minimal")).unwrap();
        let loaded = s.get_node("min").unwrap().unwrap();
        assert_eq!(loaded.id, "min");
        assert_eq!(loaded.title, "Minimal");
        assert_eq!(loaded.status, NodeStatus::Todo); // default
        assert!(loaded.description.is_none());
        assert!(loaded.tags.is_empty());
        assert!(loaded.metadata.is_empty());
        assert!(loaded.knowledge.is_empty());
        assert!(loaded.file_path.is_none());
        assert!(loaded.priority.is_none());
    }

    #[test]
    fn test_get_node_nonexistent() {
        let s = temp_storage();
        assert!(s.get_node("xxx").unwrap().is_none());
    }

    #[test]
    fn test_delete_node_cascades_edges() {
        let s = temp_storage();
        s.put_node(&Node::new("a", "A")).unwrap();
        s.put_node(&Node::new("b", "B")).unwrap();
        s.add_edge(&Edge::new("a", "b", "depends_on")).unwrap();
        assert_eq!(s.get_edge_count().unwrap(), 1);

        s.delete_node("a").unwrap();
        // ON DELETE CASCADE should remove the edge
        assert_eq!(s.get_edge_count().unwrap(), 0);
        assert!(s.get_edges("b").unwrap().is_empty());
    }

    #[test]
    fn test_delete_node_cascades_tags_metadata_knowledge() {
        let s = temp_storage();
        let mut node = Node::new("del", "To Delete");
        node.tags = vec!["tag1".into()];
        node.metadata.insert("k".into(), serde_json::json!("v"));
        node.knowledge.findings.insert("f".into(), "v".into());
        s.put_node(&node).unwrap();

        // Verify extras exist
        assert_eq!(s.get_tags("del").unwrap().len(), 1);
        assert_eq!(s.get_metadata("del").unwrap().len(), 1);
        assert!(s.get_knowledge("del").unwrap().is_some());

        s.delete_node("del").unwrap();
        // CASCADE should clean up all auxiliary tables
        assert!(s.get_tags("del").unwrap().is_empty());
        assert!(s.get_metadata("del").unwrap().is_empty());
        assert!(s.get_knowledge("del").unwrap().is_none());
    }

    #[test]
    fn test_delete_node_nonexistent() {
        let s = temp_storage();
        // Should not error on deleting non-existent node
        s.delete_node("ghost").unwrap();
    }

    // ── Edge operations ────────────────────────────────────

    #[test]
    fn test_edge_with_weight_confidence() {
        let s = temp_storage();
        s.put_node(&Node::new("a", "A")).unwrap();
        s.put_node(&Node::new("b", "B")).unwrap();

        let mut edge = Edge::new("a", "b", "calls");
        edge.weight = Some(0.8);
        edge.confidence = Some(0.95);
        s.add_edge(&edge).unwrap();

        let edges = s.get_edges("a").unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].weight, Some(0.8));
        assert_eq!(edges[0].confidence, Some(0.95));
    }

    #[test]
    fn test_edge_with_metadata() {
        let s = temp_storage();
        s.put_node(&Node::new("a", "A")).unwrap();
        s.put_node(&Node::new("b", "B")).unwrap();

        let mut edge = Edge::new("a", "b", "relates_to");
        edge.metadata = Some(serde_json::json!({
            "reason": "shared interface",
            "confidence_source": "manual"
        }));
        s.add_edge(&edge).unwrap();

        let edges = s.get_edges("a").unwrap();
        assert_eq!(edges.len(), 1);
        let meta = edges[0].metadata.as_ref().unwrap();
        assert_eq!(meta["reason"], "shared interface");
    }

    #[test]
    fn test_edge_get_both_directions() {
        let s = temp_storage();
        s.put_node(&Node::new("a", "A")).unwrap();
        s.put_node(&Node::new("b", "B")).unwrap();
        s.add_edge(&Edge::new("a", "b", "depends_on")).unwrap();

        // get_edges queries both from_node and to_node
        let from_a = s.get_edges("a").unwrap();
        let from_b = s.get_edges("b").unwrap();
        assert_eq!(from_a.len(), 1);
        assert_eq!(from_b.len(), 1);
        assert_eq!(from_a[0].from, "a");
        assert_eq!(from_b[0].from, "a"); // same edge, both queries find it
    }

    #[test]
    fn test_edge_multiple_relations() {
        let s = temp_storage();
        s.put_node(&Node::new("a", "A")).unwrap();
        s.put_node(&Node::new("b", "B")).unwrap();
        s.add_edge(&Edge::new("a", "b", "depends_on")).unwrap();
        s.add_edge(&Edge::new("a", "b", "calls")).unwrap();

        let edges = s.get_edges("a").unwrap();
        assert_eq!(edges.len(), 2);
        let relations: Vec<&str> = edges.iter().map(|e| e.relation.as_str()).collect();
        assert!(relations.contains(&"depends_on"));
        assert!(relations.contains(&"calls"));
    }

    #[test]
    fn test_remove_edge_specific_relation() {
        let s = temp_storage();
        s.put_node(&Node::new("a", "A")).unwrap();
        s.put_node(&Node::new("b", "B")).unwrap();
        s.add_edge(&Edge::new("a", "b", "depends_on")).unwrap();
        s.add_edge(&Edge::new("a", "b", "calls")).unwrap();
        assert_eq!(s.get_edge_count().unwrap(), 2);

        s.remove_edge("a", "b", "calls").unwrap();
        let edges = s.get_edges("a").unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relation, "depends_on");
    }

    // ── query_nodes comprehensive ──────────────────────────

    #[test]
    fn test_query_by_file_path_prefix() {
        let s = temp_storage();
        let mut n1 = Node::new("n1", "Main");
        n1.file_path = Some("src/main.rs".into());
        let mut n2 = Node::new("n2", "Lib");
        n2.file_path = Some("src/lib.rs".into());
        let mut n3 = Node::new("n3", "Test");
        n3.file_path = Some("tests/test.rs".into());
        s.put_node(&n1).unwrap();
        s.put_node(&n2).unwrap();
        s.put_node(&n3).unwrap();

        let results = s.query_nodes(&NodeFilter::new().with_file_path("src/")).unwrap();
        assert_eq!(results.len(), 2);
        let ids: Vec<&str> = results.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"n1"));
        assert!(ids.contains(&"n2"));
    }

    #[test]
    fn test_query_by_tag() {
        let s = temp_storage();
        s.put_node(&Node::new("n1", "One").with_tags(vec!["rust".into(), "backend".into()])).unwrap();
        s.put_node(&Node::new("n2", "Two").with_tags(vec!["rust".into()])).unwrap();
        s.put_node(&Node::new("n3", "Three").with_tags(vec!["python".into()])).unwrap();

        let results = s.query_nodes(&NodeFilter::new().with_tag("rust")).unwrap();
        assert_eq!(results.len(), 2);
        let ids: Vec<&str> = results.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"n1"));
        assert!(ids.contains(&"n2"));
    }

    #[test]
    fn test_query_by_owner() {
        let s = temp_storage();
        let mut n1 = Node::new("n1", "Owned");
        n1.owner = Some("potato".into());
        let n2 = Node::new("n2", "Unowned");
        s.put_node(&n1).unwrap();
        s.put_node(&n2).unwrap();

        let results = s.query_nodes(&NodeFilter::new().with_owner("potato")).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "n1");
    }

    #[test]
    fn test_query_combined_filters() {
        let s = temp_storage();
        let mut n1 = Node::new("n1", "Task Todo");
        n1.node_type = Some("task".into());
        n1.status = NodeStatus::Todo;
        let mut n2 = Node::new("n2", "Task Done");
        n2.node_type = Some("task".into());
        n2.status = NodeStatus::Done;
        let mut n3 = Node::new("n3", "File Todo");
        n3.node_type = Some("file".into());
        n3.status = NodeStatus::Todo;
        s.put_node(&n1).unwrap();
        s.put_node(&n2).unwrap();
        s.put_node(&n3).unwrap();

        // Type=task AND status=todo → only n1
        let results = s.query_nodes(
            &NodeFilter::new().with_node_type("task").with_status("todo")
        ).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "n1");
    }

    #[test]
    fn test_query_offset_pagination() {
        let s = temp_storage();
        for i in 1..=5 {
            s.put_node(&Node::new(&format!("n{}", i), &format!("Node {}", i))).unwrap();
        }

        // Get page 2 (items 3-4) with limit=2, offset=2
        let results = s.query_nodes(
            &NodeFilter::new().with_limit(2).with_offset(2)
        ).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_query_offset_without_limit() {
        let s = temp_storage();
        for i in 1..=5 {
            s.put_node(&Node::new(&format!("n{}", i), &format!("Node {}", i))).unwrap();
        }

        // Offset=2 without explicit limit → should use LIMIT -1
        let results = s.query_nodes(
            &NodeFilter::new().with_offset(2)
        ).unwrap();
        assert_eq!(results.len(), 3); // 5 - 2 = 3
    }

    #[test]
    fn test_query_no_results() {
        let s = temp_storage();
        s.put_node(&Node::new("n1", "Node")).unwrap();
        let results = s.query_nodes(
            &NodeFilter::new().with_node_type("nonexistent")
        ).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_query_empty_filter() {
        let s = temp_storage();
        s.put_node(&Node::new("n1", "One")).unwrap();
        s.put_node(&Node::new("n2", "Two")).unwrap();
        s.put_node(&Node::new("n3", "Three")).unwrap();

        let results = s.query_nodes(&NodeFilter::new()).unwrap();
        assert_eq!(results.len(), 3);
    }

    // ── FTS search ─────────────────────────────────────────

    #[test]
    fn test_search_by_description() {
        let s = temp_storage();
        let mut n1 = Node::new("n1", "Generic Title");
        n1.description = Some("Add OAuth2 authentication flow".into());
        s.put_node(&n1).unwrap();

        let results = s.search("OAuth2").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "n1");
    }

    #[test]
    fn test_search_by_signature() {
        let s = temp_storage();
        let mut n1 = Node::new("n1", "A Function");
        n1.signature = Some("fn calculate_score(input: &[f64]) -> f64".into());
        s.put_node(&n1).unwrap();

        let results = s.search("calculate_score").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "n1");
    }

    #[test]
    fn test_search_by_doc_comment() {
        let s = temp_storage();
        let mut n1 = Node::new("n1", "Helper");
        n1.doc_comment = Some("/// Truncates a UTF-8 string safely at byte boundaries".into());
        s.put_node(&n1).unwrap();

        let results = s.search("truncates").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_special_characters() {
        let s = temp_storage();
        let mut n1 = Node::new("n1", "Test with (parentheses)");
        n1.description = Some("Uses \"quotes\" and special chars: AND OR NOT".into());
        s.put_node(&n1).unwrap();

        // FTS5 special chars should be sanitized (wrapped in quotes)
        let results = s.search("parentheses").unwrap();
        assert_eq!(results.len(), 1);

        // Double-quote handling
        let results = s.search("quotes").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_multiple_results() {
        let s = temp_storage();
        s.put_node(&Node::new("n1", "Implement authentication")).unwrap();
        s.put_node(&Node::new("n2", "Test authentication flow")).unwrap();
        s.put_node(&Node::new("n3", "Deploy database")).unwrap();

        let results = s.search("authentication").unwrap();
        assert_eq!(results.len(), 2);
        let ids: Vec<&str> = results.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"n1"));
        assert!(ids.contains(&"n2"));
    }

    // ── Metadata/Tags advanced ─────────────────────────────

    #[test]
    fn test_tags_empty_set() {
        let s = temp_storage();
        s.put_node(&Node::new("n1", "Node").with_tags(vec!["a".into(), "b".into()])).unwrap();
        assert_eq!(s.get_tags("n1").unwrap().len(), 2);

        s.set_tags("n1", &[]).unwrap();
        assert!(s.get_tags("n1").unwrap().is_empty());
    }

    #[test]
    fn test_tags_overwrite() {
        let s = temp_storage();
        s.put_node(&Node::new("n1", "Node")).unwrap();
        s.set_tags("n1", &["old1".into(), "old2".into()]).unwrap();
        s.set_tags("n1", &["new1".into()]).unwrap();

        let tags = s.get_tags("n1").unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0], "new1");
    }

    #[test]
    fn test_metadata_complex_values() {
        let s = temp_storage();
        s.put_node(&Node::new("n1", "Node")).unwrap();

        let mut meta = HashMap::new();
        meta.insert("string".into(), serde_json::json!("hello"));
        meta.insert("number".into(), serde_json::json!(42));
        meta.insert("float".into(), serde_json::json!(3.14));
        meta.insert("bool".into(), serde_json::json!(true));
        meta.insert("null".into(), serde_json::json!(null));
        meta.insert("array".into(), serde_json::json!([1, 2, 3]));
        meta.insert("object".into(), serde_json::json!({"nested": "value", "count": 7}));

        s.set_metadata("n1", &meta).unwrap();
        let loaded = s.get_metadata("n1").unwrap();

        assert_eq!(loaded.get("string"), Some(&serde_json::json!("hello")));
        assert_eq!(loaded.get("number"), Some(&serde_json::json!(42)));
        assert_eq!(loaded.get("float"), Some(&serde_json::json!(3.14)));
        assert_eq!(loaded.get("bool"), Some(&serde_json::json!(true)));
        assert_eq!(loaded.get("null"), Some(&serde_json::json!(null)));
        assert_eq!(loaded.get("array"), Some(&serde_json::json!([1, 2, 3])));
        assert_eq!(loaded.get("object"), Some(&serde_json::json!({"nested": "value", "count": 7})));
    }

    #[test]
    fn test_metadata_overwrite() {
        let s = temp_storage();
        s.put_node(&Node::new("n1", "Node")).unwrap();

        let mut meta1 = HashMap::new();
        meta1.insert("old_key".into(), serde_json::json!("old_value"));
        s.set_metadata("n1", &meta1).unwrap();

        let mut meta2 = HashMap::new();
        meta2.insert("new_key".into(), serde_json::json!("new_value"));
        s.set_metadata("n1", &meta2).unwrap();

        let loaded = s.get_metadata("n1").unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded.get("old_key").is_none());
        assert_eq!(loaded.get("new_key"), Some(&serde_json::json!("new_value")));
    }

    #[test]
    fn test_knowledge_full_roundtrip() {
        use crate::task_graph_knowledge::ToolCallRecord;

        let s = temp_storage();
        s.put_node(&Node::new("n1", "Node")).unwrap();

        let knowledge = KnowledgeNode {
            findings: {
                let mut f = HashMap::new();
                f.insert("FINDING-1".into(), "Critical: missing error handling".into());
                f.insert("FINDING-2".into(), "Minor: naming convention".into());
                f
            },
            file_cache: {
                let mut fc = HashMap::new();
                fc.insert("src/main.rs".into(), "fn main() {}".into());
                fc
            },
            tool_history: vec![
                ToolCallRecord {
                    tool_name: "read_file".into(),
                    timestamp: "2026-04-08T01:00:00Z".into(),
                    summary: "Read sqlite.rs".into(),
                },
                ToolCallRecord {
                    tool_name: "edit_file".into(),
                    timestamp: "2026-04-08T01:05:00Z".into(),
                    summary: "Added tests".into(),
                },
            ],
        };

        s.set_knowledge("n1", &knowledge).unwrap();
        let loaded = s.get_knowledge("n1").unwrap().unwrap();

        assert_eq!(loaded.findings.len(), 2);
        assert_eq!(loaded.findings.get("FINDING-1").unwrap(), "Critical: missing error handling");
        assert_eq!(loaded.file_cache.len(), 1);
        assert_eq!(loaded.file_cache.get("src/main.rs").unwrap(), "fn main() {}");
        assert_eq!(loaded.tool_history.len(), 2);
        assert_eq!(loaded.tool_history[0].tool_name, "read_file");
        assert_eq!(loaded.tool_history[1].summary, "Added tests");
    }

    // ── Batch operations ───────────────────────────────────

    #[test]
    fn test_batch_all_op_types() {
        let s = temp_storage();
        // Pre-populate a node for deletion and edge removal
        s.put_node(&Node::new("pre", "Pre-existing")).unwrap();
        s.put_node(&Node::new("pre2", "Pre-existing 2")).unwrap();
        s.add_edge(&Edge::new("pre", "pre2", "depends_on")).unwrap();

        let knowledge = KnowledgeNode {
            findings: {
                let mut f = HashMap::new();
                f.insert("k".into(), "v".into());
                f
            },
            file_cache: HashMap::new(),
            tool_history: vec![],
        };

        let mut metadata = HashMap::new();
        metadata.insert("batch_key".into(), serde_json::json!("batch_value"));

        let ops = vec![
            BatchOp::PutNode(Node::new("b1", "Batch Node")),
            BatchOp::AddEdge(Edge::new("b1", "pre2", "calls")),
            BatchOp::SetTags("b1".into(), vec!["batched".into()]),
            BatchOp::SetMetadata("b1".into(), metadata),
            BatchOp::SetKnowledge("b1".into(), knowledge),
            BatchOp::RemoveEdge {
                from: "pre".into(),
                to: "pre2".into(),
                relation: "depends_on".into(),
            },
            BatchOp::DeleteNode("pre".into()),
        ];
        s.execute_batch(&ops).unwrap();

        // Verify all effects
        assert!(s.get_node("pre").unwrap().is_none()); // deleted
        assert!(s.get_node("b1").unwrap().is_some()); // added
        assert_eq!(s.get_tags("b1").unwrap(), vec!["batched"]);
        assert_eq!(s.get_metadata("b1").unwrap().get("batch_key"), Some(&serde_json::json!("batch_value")));
        assert!(s.get_knowledge("b1").unwrap().is_some());
        // Only edge left is b1→pre2 (pre→pre2 was removed, then pre was deleted)
        let edges = s.get_edges("pre2").unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from, "b1");
    }

    #[test]
    fn test_batch_ordering() {
        let s = temp_storage();
        // PutNode first, then AddEdge referencing it — ordering matters
        let ops = vec![
            BatchOp::PutNode(Node::new("first", "First")),
            BatchOp::PutNode(Node::new("second", "Second")),
            BatchOp::AddEdge(Edge::new("first", "second", "depends_on")),
        ];
        s.execute_batch(&ops).unwrap();

        assert_eq!(s.get_edge_count().unwrap(), 1);
    }

    #[test]
    fn test_batch_empty() {
        let s = temp_storage();
        s.execute_batch(&[]).unwrap();
        assert_eq!(s.get_node_count().unwrap(), 0);
    }

    // ── Count operations ───────────────────────────────────

    #[test]
    fn test_node_count_after_operations() {
        let s = temp_storage();
        s.put_node(&Node::new("a", "A")).unwrap();
        s.put_node(&Node::new("b", "B")).unwrap();
        s.put_node(&Node::new("c", "C")).unwrap();
        assert_eq!(s.get_node_count().unwrap(), 3);

        s.delete_node("b").unwrap();
        assert_eq!(s.get_node_count().unwrap(), 2);
    }

    #[test]
    fn test_edge_count_after_operations() {
        let s = temp_storage();
        s.put_node(&Node::new("a", "A")).unwrap();
        s.put_node(&Node::new("b", "B")).unwrap();
        s.put_node(&Node::new("c", "C")).unwrap();
        s.add_edge(&Edge::new("a", "b", "depends_on")).unwrap();
        s.add_edge(&Edge::new("b", "c", "depends_on")).unwrap();
        s.add_edge(&Edge::new("a", "c", "calls")).unwrap();
        assert_eq!(s.get_edge_count().unwrap(), 3);

        s.remove_edge("a", "c", "calls").unwrap();
        assert_eq!(s.get_edge_count().unwrap(), 2);
    }

    // ── Migration batch ────────────────────────────────────

    #[test]
    fn test_migration_batch_valid_edges() {
        let s = temp_storage();
        // Migration batch with valid edges should work fine
        let ops = vec![
            BatchOp::PutNode(Node::new("a", "A")),
            BatchOp::PutNode(Node::new("b", "B")),
            BatchOp::AddEdge(Edge::new("a", "b", "depends_on")),
        ];
        s.execute_migration_batch(&ops).unwrap();

        assert_eq!(s.get_node_count().unwrap(), 2);
        assert_eq!(s.get_edge_count().unwrap(), 1);
    }

    #[test]
    fn test_migration_batch_allows_dangling_edges() {
        // ISS-015: After fix, execute_migration_batch correctly disables FK,
        // allowing dangling edges (needed for migration pipeline per GOAL-2.9).
        let s = temp_storage();
        let ops = vec![
            BatchOp::PutNode(Node::new("a", "A")),
            BatchOp::AddEdge(Edge::new("a", "nonexistent", "depends_on")),
        ];
        // Now succeeds: FK enforcement is disabled via FkGuard BEFORE transaction
        s.execute_migration_batch(&ops).unwrap();
        
        // Verify edge was inserted despite dangling reference
        assert_eq!(s.get_edge_count().unwrap(), 1);
        let edges = s.get_edges("a").unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to, "nonexistent");
    }

    #[test]
    fn test_migration_batch_fk_reenabled_after_success() {
        // ISS-015: FK enforcement must be re-enabled after successful batch
        let s = temp_storage();
        let ops = vec![
            BatchOp::PutNode(Node::new("a", "A")),
            BatchOp::AddEdge(Edge::new("a", "phantom", "depends_on")),
        ];
        s.execute_migration_batch(&ops).unwrap();
        
        // After batch completes, normal operations should enforce FK again
        let result = s.add_edge(&Edge::new("a", "ghost", "calls"));
        assert!(result.is_err(), "FK should be re-enabled after migration batch");
        
        // Verify error is ForeignKeyViolation (ISS-015: proper error classification)
        match result.unwrap_err() {
            StorageError::ForeignKeyViolation { .. } => {}
            other => panic!("expected ForeignKeyViolation, got {:?}", other),
        }
    }

    #[test]
    fn test_migration_batch_fk_reenabled_after_error() {
        // ISS-015: FK enforcement must be re-enabled even if batch fails
        let s = temp_storage();
        let ops = vec![
            BatchOp::PutNode(Node::new("a", "A")),
            // Cause an error with invalid JSON in metadata
            BatchOp::SetMetadata("nonexistent_node".to_string(), Default::default()),
        ];
        let _ = s.execute_migration_batch(&ops); // may fail
        
        // FK should be re-enabled regardless
        s.put_node(&Node::new("b", "B")).unwrap();
        let result = s.add_edge(&Edge::new("b", "ghost", "calls"));
        assert!(result.is_err(), "FK should be re-enabled even after failed batch");
    }

    #[test]
    fn test_migration_batch_fk_guard_on_panic() {
        // ISS-015: RAII guard must re-enable FK even on panic unwind.
        // We cannot inject a panic into migration_batch directly, so instead
        // we exercise FkGuard's Drop path the same way a panic would — by
        // letting the guard go out of scope inside a panicking closure that
        // catch_unwind absorbs — then verify FK is back ON afterwards.
        use std::panic::{catch_unwind, AssertUnwindSafe};

        let s = temp_storage();

        // Sanity: FK is ON before the test.
        let fk_before: i64 = s
            .conn
            .borrow()
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk_before, 1, "FK should start ON");

        // Run a closure that creates an FkGuard then panics. Drop must run on unwind.
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _guard = FkGuard::new(&s.conn).expect("FkGuard::new should succeed");
            // FK should now be OFF while the guard is alive.
            let fk_during: i64 = s
                .conn
                .borrow()
                .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
                .unwrap();
            assert_eq!(fk_during, 0, "FK should be OFF while FkGuard is alive");
            panic!("simulated panic during batch");
        }));

        // Panic was caught; assert that Drop re-enabled FK.
        assert!(result.is_err(), "closure should have panicked");
        let fk_after: i64 = s
            .conn
            .borrow()
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            fk_after, 1,
            "FK must be re-enabled by FkGuard::drop on panic unwind"
        );
    }

    #[test]
    fn test_normal_batch_rejects_dangling_edge() {
        let s = temp_storage();
        s.put_node(&Node::new("a", "A")).unwrap();

        // Normal batch with dangling edge should fail (FK on)
        let ops = vec![
            BatchOp::AddEdge(Edge::new("a", "ghost", "depends_on")),
        ];
        let result = s.execute_batch(&ops);
        assert!(result.is_err());
        // No edge should have been inserted (transaction rolled back)
        assert_eq!(s.get_edge_count().unwrap(), 0);
    }

    // ── ISS-037: DeleteNode must remove incident edges ──────────────────

    /// Helper: count orphan edges (edges referencing a missing node).
    fn count_orphan_edges(s: &SqliteStorage) -> i64 {
        let conn = s.conn.borrow();
        conn.query_row(
            "SELECT COUNT(*) FROM edges
             WHERE from_node NOT IN (SELECT id FROM nodes)
                OR to_node   NOT IN (SELECT id FROM nodes)",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn test_iss037_delete_node_in_execute_batch_removes_incident_edges() {
        // Setup: A→B, B→C. Delete B. Both edges must be gone, no orphans.
        let s = temp_storage();
        s.put_node(&Node::new("a", "A")).unwrap();
        s.put_node(&Node::new("b", "B")).unwrap();
        s.put_node(&Node::new("c", "C")).unwrap();
        s.add_edge(&Edge::new("a", "b", "depends_on")).unwrap();
        s.add_edge(&Edge::new("b", "c", "depends_on")).unwrap();
        assert_eq!(s.get_edge_count().unwrap(), 2);

        s.execute_batch(&[BatchOp::DeleteNode("b".into())]).unwrap();

        // B is gone, A and C remain
        assert!(s.get_node("b").unwrap().is_none());
        assert!(s.get_node("a").unwrap().is_some());
        assert!(s.get_node("c").unwrap().is_some());
        // Both edges referencing B must be gone
        assert_eq!(s.get_edge_count().unwrap(), 0,
            "all edges incident to deleted node B must be removed");
        // No orphans
        assert_eq!(count_orphan_edges(&s), 0,
            "no edge may reference a non-existent node");
    }

    #[test]
    fn test_iss037_delete_node_in_migration_batch_removes_incident_edges() {
        // FK-disabled path (FkGuard). Engine cannot cascade — op must clean up.
        let s = temp_storage();
        s.put_node(&Node::new("a", "A")).unwrap();
        s.put_node(&Node::new("b", "B")).unwrap();
        s.put_node(&Node::new("c", "C")).unwrap();
        s.add_edge(&Edge::new("a", "b", "depends_on")).unwrap();
        s.add_edge(&Edge::new("b", "c", "depends_on")).unwrap();
        assert_eq!(s.get_edge_count().unwrap(), 2);

        s.execute_migration_batch(&[BatchOp::DeleteNode("b".into())]).unwrap();

        assert!(s.get_node("b").unwrap().is_none());
        assert_eq!(s.get_edge_count().unwrap(), 0,
            "migration_batch DeleteNode must remove edges (FK disabled — no cascade)");
        assert_eq!(count_orphan_edges(&s), 0);
    }

    #[test]
    fn test_iss037_delete_node_self_loop_removed() {
        // Self-edge A→A must also be cleaned up.
        let s = temp_storage();
        s.put_node(&Node::new("a", "A")).unwrap();
        // Self-loops are allowed in some graph models; insert via migration batch
        // to bypass any FK constraint differences.
        s.execute_migration_batch(&[
            BatchOp::AddEdge(Edge::new("a", "a", "self_ref")),
        ]).unwrap();
        assert_eq!(s.get_edge_count().unwrap(), 1);

        s.execute_batch(&[BatchOp::DeleteNode("a".into())]).unwrap();

        assert!(s.get_node("a").unwrap().is_none());
        assert_eq!(s.get_edge_count().unwrap(), 0);
        assert_eq!(count_orphan_edges(&s), 0);
    }

    // ── Node extras roundtrip via put_node ──────────────────

    #[test]
    fn test_put_node_with_tags_roundtrip() {
        let s = temp_storage();
        let node = Node::new("t1", "Tagged")
            .with_tags(vec!["alpha".into(), "beta".into(), "gamma".into()]);
        s.put_node(&node).unwrap();

        let loaded = s.get_node("t1").unwrap().unwrap();
        assert_eq!(loaded.tags.len(), 3);
        assert!(loaded.tags.contains(&"alpha".to_string()));
        assert!(loaded.tags.contains(&"beta".to_string()));
        assert!(loaded.tags.contains(&"gamma".to_string()));
    }

    #[test]
    fn test_put_node_upsert_clears_old_tags() {
        let s = temp_storage();
        let node = Node::new("t1", "Tagged")
            .with_tags(vec!["old1".into(), "old2".into()]);
        s.put_node(&node).unwrap();

        // Upsert with different tags
        let node2 = Node::new("t1", "Tagged")
            .with_tags(vec!["new1".into()]);
        s.put_node(&node2).unwrap();

        let loaded = s.get_node("t1").unwrap().unwrap();
        assert_eq!(loaded.tags, vec!["new1"]);
    }

    #[test]
    fn test_put_node_upsert_clears_old_metadata() {
        let s = temp_storage();
        let mut node = Node::new("m1", "Meta");
        node.metadata.insert("old".into(), serde_json::json!("value"));
        s.put_node(&node).unwrap();

        // Upsert with different metadata
        let mut node2 = Node::new("m1", "Meta");
        node2.metadata.insert("new".into(), serde_json::json!(42));
        s.put_node(&node2).unwrap();

        let loaded = s.get_node("m1").unwrap().unwrap();
        assert!(loaded.metadata.get("old").is_none());
        assert_eq!(loaded.metadata.get("new"), Some(&serde_json::json!(42)));
    }

    #[test]
    fn test_put_node_upsert_clears_knowledge_when_empty() {
        let s = temp_storage();
        let mut node = Node::new("k1", "Knowledge");
        node.knowledge.findings.insert("f1".into(), "val".into());
        s.put_node(&node).unwrap();
        assert!(s.get_knowledge("k1").unwrap().is_some());

        // Upsert with empty knowledge → should clear
        let node2 = Node::new("k1", "Knowledge");
        s.put_node(&node2).unwrap();
        // Empty knowledge → deleted from knowledge table
        assert!(s.get_knowledge("k1").unwrap().is_none());
    }

    // ── Project meta edge cases ────────────────────────────

    #[test]
    fn test_project_meta_overwrite() {
        let s = temp_storage();
        s.set_project_meta(&ProjectMeta {
            name: "old-project".into(),
            description: Some("old desc".into()),
        }).unwrap();

        s.set_project_meta(&ProjectMeta {
            name: "new-project".into(),
            description: None,
        }).unwrap();

        let loaded = s.get_project_meta().unwrap().unwrap();
        assert_eq!(loaded.name, "new-project");
        // description is set to "" when None (INSERT OR REPLACE with "")
        assert!(loaded.description.is_some());
    }

    // ── FTS after update ───────────────────────────────────

    #[test]
    fn test_search_after_node_update() {
        let s = temp_storage();
        let n = Node::new("s1", "Old Title");
        s.put_node(&n).unwrap();

        assert_eq!(s.search("Old").unwrap().len(), 1);

        // Update title via upsert
        let n2 = Node::new("s1", "New Title");
        s.put_node(&n2).unwrap();

        // Old title should no longer match
        assert_eq!(s.search("Old").unwrap().len(), 0);
        // New title should match
        assert_eq!(s.search("New").unwrap().len(), 1);
    }

    #[test]
    fn test_search_after_node_delete() {
        let s = temp_storage();
        s.put_node(&Node::new("s1", "Searchable Node")).unwrap();
        assert_eq!(s.search("Searchable").unwrap().len(), 1);

        s.delete_node("s1").unwrap();
        assert_eq!(s.search("Searchable").unwrap().len(), 0);
    }

    // ── get_all_node_ids ordering ──────────────────────────

    #[test]
    fn test_get_all_node_ids_empty() {
        let s = temp_storage();
        assert!(s.get_all_node_ids().unwrap().is_empty());
    }

    #[test]
    fn test_get_all_node_ids_after_delete() {
        let s = temp_storage();
        s.put_node(&Node::new("a", "A")).unwrap();
        s.put_node(&Node::new("b", "B")).unwrap();
        s.put_node(&Node::new("c", "C")).unwrap();
        s.delete_node("b").unwrap();

        let mut ids = s.get_all_node_ids().unwrap();
        ids.sort();
        assert_eq!(ids, vec!["a", "c"]);
    }

    // ── ISS-033: Foreign-key cascade enforcement (regression suite) ──────────

    #[test]
    fn test_iss033_open_verifies_foreign_keys_on() {
        // Sanity: every freshly-opened storage must have FK=ON. If `open` ever
        // regresses (drops the PRAGMA, fails to verify), this test catches it.
        let s = temp_storage();
        let fk: i64 = s
            .conn
            .borrow()
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1, "SqliteStorage::open must enable foreign_keys");
    }

    #[test]
    fn test_iss033_bulk_node_delete_cascades_edges() {
        // Real-world regression: 2026-04-23 engram main graph pollution incident.
        // RustClaw ran extract without LSP, polluted the graph with code nodes +
        // tree-sitter edges. Rolling back via DELETE FROM nodes WHERE node_type=...
        // must cascade to edges, not leave orphans.
        let s = temp_storage();

        // Build a small graph: 5 "code" nodes + 1 "task" node, fully connected
        // among the code nodes (4 edges) plus 1 task→code edge.
        for id in &["c1", "c2", "c3", "c4", "c5"] {
            let mut n = Node::new(*id, *id);
            n.metadata.insert("node_type".into(), serde_json::json!("code"));
            s.put_node(&n).unwrap();
        }
        let mut task = Node::new("t1", "Task 1");
        task.metadata.insert("node_type".into(), serde_json::json!("task"));
        s.put_node(&task).unwrap();

        s.add_edge(&Edge::new("c1", "c2", "calls")).unwrap();
        s.add_edge(&Edge::new("c2", "c3", "calls")).unwrap();
        s.add_edge(&Edge::new("c3", "c4", "calls")).unwrap();
        s.add_edge(&Edge::new("c4", "c5", "calls")).unwrap();
        s.add_edge(&Edge::new("t1", "c1", "tracks")).unwrap();
        assert_eq!(s.get_edge_count().unwrap(), 5);

        // Bulk-delete all "code" nodes via raw SQL (simulates the pollution
        // rollback path: `DELETE FROM nodes WHERE node_type='code'` from a
        // metadata-driven cleanup script). The CASCADE must remove all edges
        // incident to the deleted nodes — including the cross-type t1→c1 edge.
        {
            let conn = s.conn.borrow();
            // node_type lives in node_metadata; delete by joining metadata.
            conn.execute(
                "DELETE FROM nodes WHERE id IN (
                    SELECT node_id FROM node_metadata
                    WHERE key = 'node_type' AND value = '\"code\"'
                 )",
                [],
            )
            .unwrap();
        }

        // All 5 code nodes gone, only t1 remains.
        let surviving_ids = s.get_all_node_ids().unwrap();
        assert_eq!(surviving_ids, vec!["t1".to_string()]);

        // CRITICAL: zero orphan edges. ISS-033 root fix.
        assert_eq!(
            s.get_edge_count().unwrap(),
            0,
            "FK cascade must remove all edges incident to deleted code nodes \
             (ISS-033 — was producing orphan edges in real engram main graph)"
        );

        // Belt-and-suspenders: verify no orphan edges exist via the same query
        // ISS-033's reproducer used.
        let orphans: i64 = s
            .conn
            .borrow()
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE from_node NOT IN (SELECT id FROM nodes)
                 OR to_node NOT IN (SELECT id FROM nodes)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0, "no orphan edges should exist after cascade");
    }

    #[test]
    fn test_iss033_dangling_edge_rejected_via_add_edge() {
        // FK enforcement: inserting an edge with a non-existent endpoint must
        // fail loudly, not silently corrupt the graph.
        let s = temp_storage();
        s.put_node(&Node::new("a", "A")).unwrap();
        // No "ghost" node — edge endpoint does not exist.
        let result = s.add_edge(&Edge::new("a", "ghost", "depends_on"));
        assert!(
            result.is_err(),
            "FK constraint should reject edge to non-existent node"
        );
        assert_eq!(s.get_edge_count().unwrap(), 0);
    }
}
