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
    updated_at    TEXT
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
