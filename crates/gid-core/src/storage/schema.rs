/// DDL for the gid-core SQLite storage schema.
///
/// Contains CREATE TABLE statements for all tables, the FTS5 virtual table,
/// content-sync triggers, and indexes.
///
/// Design reference: design-storage.md §2, §5, §6
pub const SCHEMA_SQL: &str = r#"
-- ═══════════════════════════════════════════════════════════
-- GOAL-1.1 / GOAL-1.2: nodes table (21 dedicated columns)
-- ═══════════════════════════════════════════════════════════
CREATE TABLE IF NOT EXISTS nodes (
    id            TEXT PRIMARY KEY NOT NULL,
    title         TEXT,
    status        TEXT,
    description   TEXT,
    node_type     TEXT NOT NULL,
    file_path     TEXT,
    lang          TEXT,
    start_line    INTEGER,
    end_line      INTEGER,
    signature     TEXT,
    visibility    TEXT,
    doc_comment   TEXT,
    body_hash     TEXT,
    node_kind     TEXT,
    owner         TEXT,
    source        TEXT,
    repo          TEXT,
    priority      INTEGER,
    assigned_to   TEXT,
    parent_id     TEXT,
    depth         INTEGER,
    complexity    REAL,
    is_public     INTEGER,                     -- 0/1 boolean
    body          TEXT,
    created_at    TEXT,
    updated_at    TEXT,
    -- ISS-058 (schema_version 2): doc_path is appended LAST so that
    -- ALTER TABLE-migrated DBs and freshly-created DBs share the same
    -- column ordering. Positional row reads in row_to_node assume this.
    doc_path      TEXT
) STRICT;

-- ═══════════════════════════════════════════════════════════
-- GOAL-1.5: edges table with relation, weight, confidence
-- ═══════════════════════════════════════════════════════════
CREATE TABLE IF NOT EXISTS edges (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    from_node   TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    to_node     TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    relation    TEXT NOT NULL,
    weight      REAL DEFAULT 1.0,
    confidence  REAL,
    metadata    TEXT
) STRICT;

-- ═══════════════════════════════════════════════════════════
-- GOAL-1.3: node_metadata KV table
-- ═══════════════════════════════════════════════════════════
CREATE TABLE IF NOT EXISTS node_metadata (
    node_id     TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    key         TEXT NOT NULL,
    value       TEXT NOT NULL,
    PRIMARY KEY (node_id, key)
) STRICT;

-- ═══════════════════════════════════════════════════════════
-- GOAL-1.4: node_tags many-to-many table
-- ═══════════════════════════════════════════════════════════
CREATE TABLE IF NOT EXISTS node_tags (
    node_id     TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    tag         TEXT NOT NULL,
    PRIMARY KEY (node_id, tag)
) STRICT;

-- ═══════════════════════════════════════════════════════════
-- GOAL-1.6: knowledge table (JSON-blob per node)
-- ═══════════════════════════════════════════════════════════
CREATE TABLE IF NOT EXISTS knowledge (
    node_id       TEXT PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    findings      TEXT,
    file_cache    TEXT,
    tool_history  TEXT
) STRICT;

-- ═══════════════════════════════════════════════════════════
-- GOAL-1.16: config table (project metadata + schema version)
-- ═══════════════════════════════════════════════════════════
CREATE TABLE IF NOT EXISTS config (
    key         TEXT PRIMARY KEY NOT NULL,
    value       TEXT NOT NULL
) STRICT;

-- ═══════════════════════════════════════════════════════════
-- GOAL-1.8: change_log audit trail
-- ═══════════════════════════════════════════════════════════
CREATE TABLE IF NOT EXISTS change_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    batch_id    TEXT,
    timestamp   TEXT NOT NULL,
    actor       TEXT,
    operation   TEXT NOT NULL,
    node_id     TEXT,
    field       TEXT,
    old_value   TEXT,
    new_value   TEXT,
    context     TEXT
) STRICT;

-- ═══════════════════════════════════════════════════════════
-- GOAL-1.7: FTS5 virtual table for full-text search
--
-- SECURITY NOTE: User input to MATCH queries MUST be sanitized.
-- The search() method wraps input in double-quotes for literal
-- matching. Advanced FTS5 syntax (AND, OR, NEAR, etc.) should
-- only be exposed through a separate API with explicit opt-in.
-- ═══════════════════════════════════════════════════════════
CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
    id,
    title,
    description,
    signature,
    doc_comment,
    content='nodes',
    content_rowid='rowid'
);

-- ═══════════════════════════════════════════════════════════
-- GOAL-1.7: FTS5 content-sync triggers (§6.2)
-- ═══════════════════════════════════════════════════════════

-- After INSERT: add new content to FTS
CREATE TRIGGER IF NOT EXISTS nodes_fts_insert AFTER INSERT ON nodes BEGIN
    INSERT INTO nodes_fts(rowid, id, title, description, signature, doc_comment)
    VALUES (new.rowid, new.id, new.title, new.description, new.signature, new.doc_comment);
END;

-- After UPDATE: remove old content, add new content
CREATE TRIGGER IF NOT EXISTS nodes_fts_update AFTER UPDATE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, id, title, description, signature, doc_comment)
    VALUES ('delete', old.rowid, old.id, old.title, old.description, old.signature, old.doc_comment);
    INSERT INTO nodes_fts(rowid, id, title, description, signature, doc_comment)
    VALUES (new.rowid, new.id, new.title, new.description, new.signature, new.doc_comment);
END;

-- After DELETE: remove content from FTS
CREATE TRIGGER IF NOT EXISTS nodes_fts_delete AFTER DELETE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, id, title, description, signature, doc_comment)
    VALUES ('delete', old.rowid, old.id, old.title, old.description, old.signature, old.doc_comment);
END;

-- ═══════════════════════════════════════════════════════════
-- GOAL-1.12: Indexes on high-frequency query columns
-- ═══════════════════════════════════════════════════════════

CREATE INDEX IF NOT EXISTS idx_nodes_node_type ON nodes(node_type);
CREATE INDEX IF NOT EXISTS idx_nodes_status    ON nodes(status);
CREATE INDEX IF NOT EXISTS idx_nodes_file_path ON nodes(file_path);

CREATE INDEX IF NOT EXISTS idx_edges_from      ON edges(from_node);
CREATE INDEX IF NOT EXISTS idx_edges_to        ON edges(to_node);
CREATE INDEX IF NOT EXISTS idx_edges_relation  ON edges(relation);
CREATE INDEX IF NOT EXISTS idx_edges_from_to   ON edges(from_node, to_node);

CREATE INDEX IF NOT EXISTS idx_tags_tag        ON node_tags(tag);
CREATE INDEX IF NOT EXISTS idx_metadata_key    ON node_metadata(key);

CREATE INDEX IF NOT EXISTS idx_nodes_parent_id ON nodes(parent_id);
CREATE INDEX IF NOT EXISTS idx_nodes_owner     ON nodes(owner);
CREATE INDEX IF NOT EXISTS idx_nodes_file_lang ON nodes(file_path, lang);

-- ═══════════════════════════════════════════════════════════
-- Initial config: schema version
-- ═══════════════════════════════════════════════════════════
INSERT OR IGNORE INTO config (key, value) VALUES ('schema_version', '1');
"#;

// ═══════════════════════════════════════════════════════════════════════════
// Schema version tracking & migrations  (ISS-058)
// ═══════════════════════════════════════════════════════════════════════════
//
// Design reference: .gid/features/iss-058-doc-path/design.md §3.1
//
// `apply_migrations` is called by `SqliteStorage::open` after `SCHEMA_SQL` has
// run. It uses SQLite's `PRAGMA user_version` as a numeric schema-version
// counter (separate from the `config` table's `schema_version` row, which
// remains for backward-compat introspection).
//
// Versions:
//   - 0  : pre-migration database (legacy DBs predating this runner). The
//          migration runner treats `user_version=0` as "needs all migrations".
//          Newly created DBs that ran SCHEMA_SQL already have the doc_path
//          column, so v1→v2 ALTER becomes a no-op via `IF NOT EXISTS`-style
//          duplicate-column handling (see below).
//   - 1  : initial published schema (no doc_path column).
//   - 2  : ISS-058 — adds `nodes.doc_path TEXT` column.
//
// Idempotency:
//   - The runner re-reads `user_version` on every call.
//   - If it equals `CURRENT_SCHEMA_VERSION`, it returns immediately.
//   - The ALTER step uses a duplicate-column probe so that fresh DBs (where
//     SCHEMA_SQL already created the column) do not error.

/// Latest schema version this build of gid-core targets.
pub const CURRENT_SCHEMA_VERSION: i64 = 2;

/// Apply any pending migrations on `conn`. Idempotent: safe to call on
/// already-migrated DBs and on freshly-created DBs.
///
/// Errors propagated as `rusqlite::Error` (the storage layer wraps them).
#[cfg(feature = "sqlite")]
pub fn apply_migrations(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

    if current >= CURRENT_SCHEMA_VERSION {
        return Ok(());
    }

    // ── v0/v1 → v2 : add nodes.doc_path ────────────────────────────────────
    if current < 2 {
        if !column_exists(conn, "nodes", "doc_path")? {
            conn.execute_batch("ALTER TABLE nodes ADD COLUMN doc_path TEXT;")?;
        }
        // (No data backfill here — backfill is a separate `gid backfill-doc-path`
        // CLI subcommand, deferred to ISS-058 B1 follow-up.)
    }

    // Future: if current < 3 { ... }

    // Stamp final version (single write, regardless of how many steps ran).
    conn.execute_batch(&format!(
        "PRAGMA user_version = {};",
        CURRENT_SCHEMA_VERSION
    ))?;
    Ok(())
}

/// Returns true if `column` exists on `table`. Uses sqlite_master / table_info.
#[cfg(feature = "sqlite")]
fn column_exists(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({});", table))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?; // table_info col 1 = name
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(all(test, feature = "sqlite"))]
mod migration_tests {
    use super::*;
    use rusqlite::Connection;

    fn fresh_v1_db() -> Connection {
        // Simulates an old v1 DB: just nodes table without doc_path,
        // user_version unset (=0).
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE nodes (
                 id TEXT PRIMARY KEY NOT NULL,
                 doc_comment TEXT,
                 body_hash TEXT
             ) STRICT;",
        )
        .unwrap();
        conn
    }

    #[test]
    fn migration_adds_doc_path_column_to_v1_db() {
        let conn = fresh_v1_db();
        assert!(!column_exists(&conn, "nodes", "doc_path").unwrap());
        apply_migrations(&conn).unwrap();
        assert!(column_exists(&conn, "nodes", "doc_path").unwrap());
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn migration_is_idempotent() {
        let conn = fresh_v1_db();
        apply_migrations(&conn).unwrap();
        // Calling twice must not error and must not duplicate the column.
        apply_migrations(&conn).unwrap();
        apply_migrations(&conn).unwrap();
        // table_info must still show exactly one doc_path column.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('nodes') WHERE name = 'doc_path'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn migration_skips_when_column_already_exists() {
        // Simulates a fresh DB created by SCHEMA_SQL (column already present),
        // but with user_version=0.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE nodes (
                 id TEXT PRIMARY KEY NOT NULL,
                 doc_path TEXT
             ) STRICT;",
        )
        .unwrap();
        apply_migrations(&conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn no_op_when_already_at_current() {
        let conn = fresh_v1_db();
        apply_migrations(&conn).unwrap();
        // Now user_version=CURRENT. A second call should early-return.
        let before: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        apply_migrations(&conn).unwrap();
        let after: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(before, after);
    }
}

#[cfg(test)]
mod yaml_doc_path_tests {
    //! Verifies `doc_path` roundtrips cleanly through serde_yaml, and that
    //! `skip_serializing_if = "Option::is_none"` elides the field when None
    //! (keeps yaml diffs minimal for code/extracted nodes).
    use crate::graph::Node;

    #[test]
    fn doc_path_roundtrips_through_yaml() {
        let mut n = Node::new("iss-058", "doc_path field");
        n.doc_path = Some(".gid/issues/ISS-058/issue.md".to_string());
        let yaml = serde_yaml::to_string(&n).unwrap();
        assert!(
            yaml.contains("doc_path: .gid/issues/ISS-058/issue.md"),
            "yaml must contain doc_path; got:\n{}",
            yaml
        );
        let back: Node = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back.doc_path, n.doc_path);
    }

    #[test]
    fn doc_path_none_is_elided_from_yaml() {
        let n = Node::new("code-foo", "extracted code");
        assert_eq!(n.doc_path, None);
        let yaml = serde_yaml::to_string(&n).unwrap();
        assert!(
            !yaml.contains("doc_path"),
            "doc_path: None must be elided (skip_serializing_if); got:\n{}",
            yaml
        );
        let back: Node = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back.doc_path, None);
    }
}
