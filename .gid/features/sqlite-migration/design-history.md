# Design: Snapshot History (GOALs 3.1–3.9)

## 1. Overview

This document specifies the design for `gid`'s snapshot history subsystem, which provides point-in-time captures of the full SQLite graph database, enabling users to save, list, restore, compare, and manage historical states of their dependency graph. The design builds on the master `design.md` architecture — reusing shared error types from §3 and the `index.json` sidecar trade-off from §9 — to deliver a lightweight, file-based snapshotting mechanism backed by SQLite's native backup facilities. All operations target the `.gid/history/` directory and are coordinated through an append-only `index.json` manifest to avoid requiring clients to open every `.db` file for metadata queries. Node and edge IDs are represented as `String` throughout this document (matching the master design's use of string identifiers).

---

## 2. Snapshot Storage Layout

All snapshot artifacts live under `.gid/history/`:

```
.gid/
├── graph.db            # live database
└── history/
    ├── index.json      # manifest of all snapshots
    ├── 2026-04-06T18-34-20Z.db
    ├── 2026-04-07T09-15-00Z.db
    └── ...
```

### Naming Convention

Each snapshot file follows the filesystem-safe ISO 8601 pattern specified by GOAL-3.2:

```
<filesystem-safe-ISO-8601>.db
```

| Segment | Example | Purpose |
|---|---|---|
| Timestamp | `2026-04-06T18-34-20Z` | Human-sortable, UTC, second precision, filesystem-safe (hyphens instead of colons) |
| Extension | `.db` | Identifies file as a self-contained SQLite database |

The timestamp is always UTC (suffix `Z`) regardless of local timezone, ensuring lexicographic sort equals chronological sort. Colons are replaced with hyphens for filesystem safety. The timestamp doubles as the snapshot identifier used in CLI commands (e.g., `gid history restore 2026-04-06T18-34-20Z`).

**Same-second collision handling:** If a snapshot file already exists for the current second (e.g., rapid auto-save + manual save), a numeric suffix is appended: `2026-04-06T18-34-20Z-1.db`, `2026-04-06T18-34-20Z-2.db`, etc. The `id` in `index.json` reflects the full stem including the suffix.

```rust
fn snapshot_filename(snapshots_dir: &Path) -> String {
    let now = Utc::now();
    let base = now.format("%Y-%m-%dT%H-%M-%SZ").to_string();
    let candidate = format!("{}.db", base);
    if !snapshots_dir.join(&candidate).exists() {
        return candidate;
    }
    // Handle same-second collision
    for i in 1.. {
        let candidate = format!("{}-{}.db", base, i);
        if !snapshots_dir.join(&candidate).exists() {
            return candidate;
        }
    }
    unreachable!()
}
```

---

## 3. index.json — Metadata Manifest

Per master `design.md` §9 trade-offs, we maintain an `index.json` sidecar to support fast listing without opening individual `.db` files. This is an append-friendly JSON array that is rewritten atomically on each snapshot save or delete.

### Schema

```json
{
  "version": 1,
  "snapshots": [
    {
      "id": "2026-04-06T18-34-20Z",
      "filename": "2026-04-06T18-34-20Z.db",
      "created_at": "2026-04-06T18:34:20Z",
      "message": "before refactoring auth module",
      "node_count": 42,
      "edge_count": 87,
      "size_bytes": 131072,
      "db_checksum": "sha256:ab12cd34..."
    }
  ]
}
```

| Field | Type | Required | Notes |
|---|---|---|---|
| `id` | `String` | ✅ | Stem of the filename (without `.db`) |
| `filename` | `String` | ✅ | Full filename relative to `history/` |
| `created_at` | `String` (RFC 3339) | ✅ | UTC creation timestamp |
| `message` | `String` | ❌ | User-supplied description; empty string if omitted |
| `node_count` | `u64` | ✅ | `SELECT COUNT(*) FROM nodes` at snapshot time |
| `edge_count` | `u64` | ✅ | `SELECT COUNT(*) FROM edges` at snapshot time |
| `size_bytes` | `u64` | ✅ | File size after backup completes |
| `db_checksum` | `String` | ✅ | `sha256:` prefixed hex digest of the `.db` file |

### Atomic Writes

`index.json` is updated via write-to-temp-then-rename to avoid corruption on crash:

```rust
fn update_index(snapshots_dir: &Path, entries: &[SnapshotEntry]) -> Result<()> {
    let index = IndexFile { version: 1, snapshots: entries.to_vec() };
    let json = serde_json::to_string_pretty(&index)?;
    let tmp = snapshots_dir.join("index.json.tmp");
    fs::write(&tmp, json.as_bytes())?;
    fs::rename(&tmp, snapshots_dir.join("index.json"))?;
    Ok(())
}
```

### Self-Healing

On startup or before any snapshot operation, if `index.json` is missing or fails to parse, the system rebuilds it by scanning all `.db` files in the directory, opening each to extract metadata. This ensures resilience against manual file manipulation. [GOAL 3.2]

**`load_index` behavior:**
- If `index.json` does not exist → returns an empty `Vec<SnapshotEntry>` (first use).
- If `index.json` exists but fails to parse (corrupt JSON, wrong version) → triggers a full rebuild by scanning `.db` files, then returns the rebuilt list.
- If `index.json` lists a `.db` file that no longer exists on disk → that entry is silently removed during rebuild.
- If a `.db` file exists on disk but is not in `index.json` → the file is opened, metadata is extracted, and an entry is added during rebuild.

### Trade-offs

Maintaining `index.json` as a sidecar introduces a sync failure mode: the index can become inconsistent with actual `.db` files if files are manually added, deleted, or renamed. The self-healing behavior above mitigates this but introduces a cost — rebuild requires opening every `.db` file, which is O(n) in the number of snapshots. For the expected maximum of 50 snapshots (GOAL-3.3), this is acceptable.

Alternative considered: storing metadata inside each snapshot `.db` in a `_snapshot_meta` table. This eliminates the sync issue but requires opening every `.db` for listing, making `snapshot_list` O(n) on every call rather than only on rebuild. Given that listing is the most frequent read operation, the sidecar approach is preferred.

Another failure mode: if `update_index` crashes after writing the `.db` file but before renaming `index.json.tmp` → the `.db` exists without an index entry. The self-healing rebuild handles this case on the next operation.

---

## 4. Snapshot Save

**Implements: GOAL 3.1 (save snapshot), GOAL 3.2 (snapshot metadata)**

### Strategy: SQLite Backup API

We use SQLite's Online Backup API (`sqlite3_backup_init` / `sqlite3_backup_step` / `sqlite3_backup_finish`) rather than `VACUUM INTO` because:

1. **Non-blocking** — the backup API works on a live database with concurrent readers.
2. **Incremental** — can be stepped to avoid holding locks for extended periods.
3. **Consistent** — produces a point-in-time consistent copy even with WAL mode enabled.

### `compute_sha256` Utility

SHA-256 checksums are used for integrity verification in both save and restore. The utility reads the file in 8 KB chunks to avoid loading large databases into memory:

```rust
use sha2::{Sha256, Digest};

/// Compute SHA-256 hex digest of a file.
/// Lives in `gid_core::util::crypto` (or inline in the history module).
fn compute_sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
```

### Pseudocode

```rust
/// Save a snapshot of the current graph database.
/// Returns the SnapshotEntry written to index.json.
///
/// [GOAL 3.1] [GOAL 3.2]
pub fn snapshot_save(
    db: &Connection,
    snapshots_dir: &Path,
    message: Option<&str>,
) -> Result<SnapshotEntry> {
    fs::create_dir_all(snapshots_dir)?;

    // 1. Generate destination path
    let filename = snapshot_filename(snapshots_dir);
    let dest_path = snapshots_dir.join(&filename);

    // 2. Perform backup via SQLite Backup API
    let mut dest_conn = Connection::open(&dest_path)?;
    let backup = rusqlite::backup::Backup::new(db, &mut dest_conn)?;
    // Step in pages of 256; sleep 50ms between steps to reduce lock contention
    backup.run_to_completion(256, std::time::Duration::from_millis(50), None)?;
    drop(dest_conn);

    // 3. Collect metadata
    let node_count: u64 = db.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
    let edge_count: u64 = db.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
    let size_bytes = fs::metadata(&dest_path)?.len();
    let db_checksum = compute_sha256(&dest_path)?;

    // 4. Build entry
    let id = filename.trim_end_matches(".db").to_string();
    let entry = SnapshotEntry {
        id: id.clone(),
        filename,
        created_at: Utc::now(),
        message: message.unwrap_or("").to_string(),
        node_count,
        edge_count,
        size_bytes,
        db_checksum: format!("sha256:{}", db_checksum),
    };

    // 5. Append to index.json (atomic rewrite)
    let mut entries = load_index(snapshots_dir)?;
    entries.push(entry.clone());
    update_index(snapshots_dir, &entries)?;

    Ok(entry)
}
```

### Integrity Check

After backup completes, we run `PRAGMA integrity_check` on the destination to verify correctness before committing the index entry. If the check fails, the `.db` file is deleted and an error is returned.

```rust
fn verify_snapshot(path: &Path) -> Result<()> {
    let conn = Connection::open(path)?;
    let result: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if result != "ok" {
        fs::remove_file(path)?;
        return Err(GidError::SnapshotCorrupted(result));
    }
    Ok(())
}
```

---

## 5. Snapshot List

**Implements: GOAL 3.3 (list snapshots)**

Listing reads `index.json` directly — no `.db` files are opened. Results are sorted by `created_at` descending (newest first).

### Pseudocode

```rust
/// List all snapshots, newest first.
///
/// [GOAL 3.3]
pub fn snapshot_list(snapshots_dir: &Path) -> Result<Vec<SnapshotEntry>> {
    let mut entries = load_index(snapshots_dir)?;
    entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(entries)
}
```

### Display Format

```
ID                              CREATED              NODES  EDGES  SIZE    MESSAGE
2026-04-07T09-15-00Z             2026-04-07 09:15:00  58     104    256 KB  added new auth edges
2026-04-06T18-34-20Z             2026-04-06 18:34:20  42     87     128 KB  before refactoring auth module
```

The CLI formatter converts `created_at` to the user's local timezone for display. The `SIZE` column uses human-readable units (KB, MB, GB).

---

## 6. Snapshot Restore

**Implements: GOAL 3.4 (restore snapshot)**

Restore replaces the live `graph.db` with a snapshot's `.db` file. This is a destructive operation — the current state is lost unless the user has saved it.

### Strategy

1. **Auto-save current state** — before restoring, auto-save the current graph as a snapshot (with message "auto-save before restore") per GOAL-3.5.
2. **Copy, don't move** — the snapshot file remains in `history/` after restore.
3. **Close connection before copy** — the live database connection must be dropped before file-level replacement to avoid file handle conflicts (especially on macOS where file descriptors persist through renames). The caller is responsible for closing the live connection before calling `snapshot_restore`, then reopening it after.
4. **File copy + atomic rename** — copy the snapshot to a `.restoring` temp file, then rename over the live database. This is preferred over the SQLite Backup API in reverse because it's simpler and the connection is already closed.

> **Design decision:** We use file copy rather than SQLite Backup API in reverse. The Backup API would allow restoring into an open connection, but requires careful handling of WAL mode settings and schema changes. Since we close the connection anyway (to avoid file handle conflicts), a file copy is simpler and equally correct.

### Pseudocode

```rust
/// Restore the live database from a snapshot.
///
/// The caller must close (drop) the live database connection before calling
/// this function, and reopen it after. This avoids file handle conflicts
/// during the file replacement.
///
/// [GOAL 3.4] [GOAL 3.5]
pub fn snapshot_restore(
    live_db_path: &Path,
    snapshots_dir: &Path,
    snapshot_id: &str,
) -> Result<()> {
    let entries = load_index(snapshots_dir)?;
    let entry = entries.iter()
        .find(|e| e.id == snapshot_id)
        .ok_or(GidError::SnapshotNotFound(snapshot_id.to_string()))?;

    let snap_path = snapshots_dir.join(&entry.filename);

    // 1. Verify snapshot integrity before restoring
    verify_snapshot(&snap_path)?;

    // 2. Verify checksum matches index
    let actual_checksum = format!("sha256:{}", compute_sha256(&snap_path)?);
    if actual_checksum != entry.db_checksum {
        return Err(GidError::ChecksumMismatch {
            expected: entry.db_checksum.clone(),
            actual: actual_checksum,
        });
    }

    // 3. Check disk space before restore
    let snap_size = fs::metadata(&snap_path)?.len();
    let required = (snap_size as f64 * 1.1) as u64;
    let available = fs2::available_space(live_db_path.parent().unwrap())?;
    if available < required {
        return Err(GidError::InsufficientDiskSpace { required, available });
    }

    // 4. Auto-save current state, protecting restore target from pruning
    {
        let live_conn = Connection::open(live_db_path)?;
        snapshot_save(&live_conn, snapshots_dir, Some("auto-save before restore"))?;
        // Prune with the restore target protected
        let default_policy = PrunePolicy { keep_last: Some(50), ..Default::default() };
        snapshot_prune(snapshots_dir, default_policy, &[snapshot_id])?;
    } // live_conn is dropped here — connection is closed

    // 5. Copy snapshot over live db (connection must be closed at this point)
    let tmp_path = live_db_path.with_extension("db.restoring");
    fs::copy(&snap_path, &tmp_path)?;
    fs::rename(&tmp_path, live_db_path)?;

    // 6. Remove WAL/SHM files left from old connection — hard error on failure
    //    Stale WAL/SHM could cause data corruption if replayed on next open.
    let wal_path = live_db_path.with_extension("db-wal");
    let shm_path = live_db_path.with_extension("db-shm");
    if wal_path.exists() {
        fs::remove_file(&wal_path).map_err(|e| GidError::RestoreCleanup {
            path: wal_path,
            source: e,
        })?;
    }
    if shm_path.exists() {
        fs::remove_file(&shm_path).map_err(|e| GidError::RestoreCleanup {
            path: shm_path,
            source: e,
        })?;
    }

    Ok(())
}
```

### Safety Checks

- The snapshot `.db` must pass `PRAGMA integrity_check`.
- The SHA-256 checksum must match the value recorded in `index.json`.
- If the live database is currently open by another `gid` process, the restore fails with `GidError::DatabaseLocked`. [GOAL 3.8]

---

## 7. Snapshot Diff

**Implements: GOAL 3.5 (diff two snapshots), GOAL 3.6 (SnapshotDiff struct)**

Diff loads two snapshot databases (or one snapshot and the live database) and compares their `nodes` and `edges` tables to produce a `SnapshotDiff`.

### Strategy

We attach both databases to a temporary in-memory connection and use SQL set-difference queries. This avoids loading all rows into Rust memory for large graphs.

### Pseudocode

```rust
/// Compare two snapshots and produce a diff.
/// Either ID may be "live" to reference the current graph.db.
///
/// [GOAL 3.5]
pub fn snapshot_diff(
    live_db_path: &Path,
    snapshots_dir: &Path,
    from_id: &str,
    to_id: &str,
) -> Result<SnapshotDiff> {
    let from_path = resolve_snapshot_path(live_db_path, snapshots_dir, from_id)?;
    let to_path = resolve_snapshot_path(live_db_path, snapshots_dir, to_id)?;

    let conn = Connection::open_in_memory()?;
    // ATTACH must use separate execute() calls with parameter binding.
    // execute_batch() does NOT support parameter binding.
    conn.execute("ATTACH DATABASE ?1 AS snap_from", [&from_path.to_string_lossy().as_ref()])?;
    conn.execute("ATTACH DATABASE ?1 AS snap_to", [&to_path.to_string_lossy().as_ref()])?;

    // Nodes added in `to` that don't exist in `from`
    let added_nodes: Vec<String> = conn.prepare("
        SELECT id FROM snap_to.nodes
        WHERE id NOT IN (SELECT id FROM snap_from.nodes)
    ")?.query_map([], |row| row.get(0))?
      .collect::<Result<_, _>>()?;

    // Nodes removed (in `from` but not in `to`)
    let removed_nodes: Vec<String> = conn.prepare("
        SELECT id FROM snap_from.nodes
        WHERE id NOT IN (SELECT id FROM snap_to.nodes)
    ")?.query_map([], |row| row.get(0))?
      .collect::<Result<_, _>>()?;

    // Nodes modified (same ID, different content)
    // Field-by-field comparison since the schema has no content_hash column.
    // Compares all mutable dedicated columns from design-storage.md §2.1:
    // node_type, title, description, status, file_path, lang, signature,
    // visibility, doc_comment, body_hash, node_kind, owner, source, repo,
    // priority, assigned_to.
    // Uses COALESCE to handle NULLs (NULL != NULL in SQL, but we want
    // NULL == NULL for "no change" semantics).
    let modified_nodes: Vec<NodeModification> = conn.prepare("
        SELECT f.id, f.title, t.title, f.status, t.status,
               f.description, t.description, f.node_type, t.node_type,
               f.file_path, t.file_path, f.signature, t.signature,
               f.priority, t.priority, f.assigned_to, t.assigned_to
        FROM snap_from.nodes f
        INNER JOIN snap_to.nodes t ON f.id = t.id
        WHERE COALESCE(f.node_type,'') != COALESCE(t.node_type,'')
           OR COALESCE(f.title,'') != COALESCE(t.title,'')
           OR COALESCE(f.description,'') != COALESCE(t.description,'')
           OR COALESCE(f.status,'') != COALESCE(t.status,'')
           OR COALESCE(f.file_path,'') != COALESCE(t.file_path,'')
           OR COALESCE(f.signature,'') != COALESCE(t.signature,'')
           OR COALESCE(f.priority,-1) != COALESCE(t.priority,-1)
           OR COALESCE(f.assigned_to,'') != COALESCE(t.assigned_to,'')
    ")?.query_map([], |row| {
        Ok(NodeModification {
            id: row.get(0)?,
            old_title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            new_title: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            old_status: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            new_status: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            changed_fields: {
                let mut fields = Vec::new();
                if row.get::<_, Option<String>>(1)? != row.get::<_, Option<String>>(2)? { fields.push("title".to_string()); }
                if row.get::<_, Option<String>>(5)? != row.get::<_, Option<String>>(6)? { fields.push("description".to_string()); }
                if row.get::<_, Option<String>>(7)? != row.get::<_, Option<String>>(8)? { fields.push("node_type".to_string()); }
                if row.get::<_, Option<String>>(3)? != row.get::<_, Option<String>>(4)? { fields.push("status".to_string()); }
                if row.get::<_, Option<String>>(9)? != row.get::<_, Option<String>>(10)? { fields.push("file_path".to_string()); }
                if row.get::<_, Option<String>>(11)? != row.get::<_, Option<String>>(12)? { fields.push("signature".to_string()); }
                if row.get::<_, Option<i64>>(13)? != row.get::<_, Option<i64>>(14)? { fields.push("priority".to_string()); }
                if row.get::<_, Option<String>>(15)? != row.get::<_, Option<String>>(16)? { fields.push("assigned_to".to_string()); }
                fields
            },
        })
    })?.collect::<Result<_, _>>()?;

    // Edges added / removed / modified — same pattern, comparing
    // from_node, to_node, relation, weight, confidence, and metadata columns.
    let added_edges = diff_edges_added(&conn)?;   // -> Vec<String>
    let removed_edges = diff_edges_removed(&conn)?; // -> Vec<String>
    let modified_edges = diff_edges_modified(&conn)?; // -> Vec<EdgeModification>

    Ok(SnapshotDiff {
        from_id: from_id.to_string(),
        to_id: to_id.to_string(),
        added_nodes,
        removed_nodes,
        modified_nodes,
        added_edges,
        removed_edges,
        modified_edges,
    })
}
```

### Field-by-Field Comparison

Since the storage schema (`design-storage.md` §2) does not include a `content_hash` column on `nodes` or `edges`, modification detection uses direct column comparison in SQL. The WHERE clause compares key mutable columns (`node_type`, `title`, `description`, `status`, `file_path`, `signature`, `priority`, `assigned_to` for nodes; `relation`, `weight`, `confidence`, `metadata` for edges) using `COALESCE` for NULL-safe comparison. This is slightly slower than hash comparison for very large graphs but avoids adding a computed column to the storage schema. For the expected graph sizes (≤10,000 nodes), the performance difference is negligible since SQLite performs the comparison server-side without transferring unchanged rows to Rust.

---

## 8. SnapshotDiff Struct

**Implements: GOAL 3.6 (SnapshotDiff struct)**

```rust
/// The result of comparing two graph snapshots.
///
/// [GOAL 3.6]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotDiff {
    /// Snapshot ID of the "before" state
    pub from_id: String,
    /// Snapshot ID of the "after" state
    pub to_id: String,

    /// Nodes present in `to` but absent from `from`
    pub added_nodes: Vec<String>,
    /// Nodes present in `from` but absent from `to`
    pub removed_nodes: Vec<String>,
    /// Nodes present in both but with different content
    pub modified_nodes: Vec<NodeModification>,

    /// Edges present in `to` but absent from `from`
    pub added_edges: Vec<String>,
    /// Edges present in `from` but absent from `to`
    pub removed_edges: Vec<String>,
    /// Edges present in both but with different content
    pub modified_edges: Vec<EdgeModification>,
}

/// Details about a modified node.
/// Uses field-by-field comparison since the schema has no content_hash column.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeModification {
    pub id: String,
    pub old_title: String,
    pub new_title: String,
    pub old_status: String,
    pub new_status: String,
    /// Names of columns that changed (e.g., ["title", "description", "node_type"])
    pub changed_fields: Vec<String>,
}

/// Details about a modified edge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EdgeModification {
    pub id: String,
    pub old_weight: Option<f64>,
    pub new_weight: Option<f64>,
    pub old_relation: String,
    pub new_relation: String,
    /// Names of columns that changed (e.g., ["weight", "relation", "metadata"])
    pub changed_fields: Vec<String>,
}

impl SnapshotDiff {
    /// Returns true if the two snapshots are identical.
    pub fn is_empty(&self) -> bool {
        self.added_nodes.is_empty()
            && self.removed_nodes.is_empty()
            && self.modified_nodes.is_empty()
            && self.added_edges.is_empty()
            && self.removed_edges.is_empty()
            && self.modified_edges.is_empty()
    }

    /// Total number of changes across all categories.
    pub fn total_changes(&self) -> usize {
        self.added_nodes.len()
            + self.removed_nodes.len()
            + self.modified_nodes.len()
            + self.added_edges.len()
            + self.removed_edges.len()
            + self.modified_edges.len()
    }

    /// Summary string for CLI display.
    pub fn summary(&self) -> String {
        format!(
            "Nodes: +{} -{} ~{}  Edges: +{} -{} ~{}",
            self.added_nodes.len(),
            self.removed_nodes.len(),
            self.modified_nodes.len(),
            self.added_edges.len(),
            self.removed_edges.len(),
            self.modified_edges.len(),
        )
    }
}
```

### Serialization

`SnapshotDiff` derives `Serialize` / `Deserialize` so it can be emitted as JSON via `--format json` for programmatic consumption. The default CLI output uses the `summary()` method followed by itemized lists when `--verbose` is set.

---

## 9. Storage Management

**Implements: GOAL 3.7 (pruning), GOAL 3.9 (disk space)**

### Pruning Policies

Pruning removes old snapshots to reclaim disk space. Three strategies are supported, combinable:

| Strategy | Flag | Description |
|---|---|---|
| **Keep N** | `--keep-last <N>` | Retain only the N most recent snapshots |
| **Max age** | `--older-than <duration>` | Delete snapshots older than a duration (e.g., `30d`, `6h`) |
| **Max size** | `--max-total <size>` | Delete oldest snapshots until total size is under limit |

### Pseudocode

```rust
/// Prune snapshots according to the given policy.
/// `protected_ids` prevents specific snapshots from being deleted
/// (e.g., a restore target that must survive the auto-save prune cycle).
///
/// [GOAL 3.7] [GOAL 3.5]
pub fn snapshot_prune(
    snapshots_dir: &Path,
    policy: PrunePolicy,
    protected_ids: &[&str],
) -> Result<Vec<SnapshotEntry>> {
    let mut entries = load_index(snapshots_dir)?;
    // Sort oldest first for pruning
    entries.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    let protected: HashSet<&str> = protected_ids.iter().copied().collect();
    let mut to_remove: HashSet<String> = HashSet::new();

    // Apply keep-last
    if let Some(keep) = policy.keep_last {
        if entries.len() > keep {
            let excess = entries.len() - keep;
            for entry in &entries[..excess] {
                if !protected.contains(entry.id.as_str()) {
                    to_remove.insert(entry.id.clone());
                }
            }
        }
    }

    // Apply max-age
    if let Some(max_age) = policy.older_than {
        let cutoff = Utc::now() - max_age;
        for entry in &entries {
            if entry.created_at < cutoff && !protected.contains(entry.id.as_str()) {
                to_remove.insert(entry.id.clone());
            }
        }
    }

    // Apply max-total-size (remove oldest until under budget)
    if let Some(max_bytes) = policy.max_total_bytes {
        let mut total: u64 = entries.iter().map(|e| e.size_bytes).sum();
        for entry in &entries {
            if total <= max_bytes { break; }
            if !protected.contains(entry.id.as_str()) {
                total -= entry.size_bytes;
                to_remove.insert(entry.id.clone());
            }
        }
    }

    // Collect removed entries for return value
    let removed_entries: Vec<SnapshotEntry> = entries.iter()
        .filter(|e| to_remove.contains(&e.id))
        .cloned()
        .collect();

    // Delete files and update index
    for entry in &removed_entries {
        let path = snapshots_dir.join(&entry.filename);
        fs::remove_file(&path).ok(); // Best-effort; file may already be gone
    }

    let remaining: Vec<SnapshotEntry> = entries.into_iter()
        .filter(|e| !to_remove.contains(&e.id))
        .collect();

    update_index(snapshots_dir, &remaining)?;

    Ok(removed_entries)
}

#[derive(Debug, Clone, Default)]
pub struct PrunePolicy {
    pub keep_last: Option<usize>,
    pub older_than: Option<chrono::Duration>,
    pub max_total_bytes: Option<u64>,
}
```

### Disk Space Checks [GOAL 3.9]

Before creating a new snapshot, we estimate the required space (current `graph.db` size × 1.1 safety margin) and compare it to available disk space via `fs2::available_space()` or equivalent:

```rust
fn check_disk_space(live_db_path: &Path, snapshots_dir: &Path) -> Result<()> {
    let db_size = fs::metadata(live_db_path)?.len();
    let required = (db_size as f64 * 1.1) as u64;
    let available = fs2::available_space(snapshots_dir)?;
    if available < required {
        return Err(GidError::InsufficientDiskSpace {
            required,
            available,
        });
    }
    Ok(())
}
```

The `snapshot_save` function calls `check_disk_space` before initiating the backup. The `snapshot_prune` function can be invoked automatically when space is low via a configurable `auto_prune` policy stored in `.gid/config.toml`.

---

## 10. Concurrency

**Implements: GOAL 3.8 (concurrent snapshot operations)**

### File Locking

Snapshot operations acquire an exclusive advisory lock on `.gid/history/.lock` to serialize concurrent access:

```rust
use fs2::FileExt;

/// Acquire the snapshot lock. Returns the lock file handle (lock is held until dropped).
///
/// [GOAL 3.8]
fn acquire_snapshot_lock(snapshots_dir: &Path) -> Result<File> {
    let lock_path = snapshots_dir.join(".lock");
    let lock_file = File::create(&lock_path)?;
    lock_file.try_lock_exclusive().map_err(|_| GidError::SnapshotLockContention)?;
    Ok(lock_file)
}
```

### Guarantees

| Operation | Lock Type | Rationale |
|---|---|---|
| `snapshot_save` | Exclusive | Mutates `index.json` and writes a new `.db` file |
| `snapshot_list` | None | Reads `index.json` only; tolerates stale data |
| `snapshot_restore` | Exclusive | Replaces live `graph.db`; must not race with save |
| `snapshot_diff` | None | Opens snapshot `.db` files read-only |
| `snapshot_prune` | Exclusive | Deletes files and rewrites `index.json` |

### WAL Mode Interaction

The live `graph.db` uses WAL (Write-Ahead Logging) mode per master `design.md`. The SQLite Backup API correctly handles WAL — it checkpoints internally before copying pages, ensuring the snapshot `.db` is a self-contained rollback-mode database. No special WAL handling is needed in snapshot code.

### Timeout

If the lock cannot be acquired within 5 seconds, the operation returns `GidError::SnapshotLockTimeout`. This prevents indefinite hangs in scripts:

```rust
fn acquire_snapshot_lock_with_timeout(
    snapshots_dir: &Path,
    timeout: Duration,
) -> Result<File> {
    let lock_path = snapshots_dir.join(".lock");
    let lock_file = File::create(&lock_path)?;
    let start = Instant::now();
    loop {
        match lock_file.try_lock_exclusive() {
            Ok(()) => return Ok(lock_file),
            Err(_) if start.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return Err(GidError::SnapshotLockTimeout),
        }
    }
}
```

---

## 11. GOAL Traceability

| GOAL | Title | Implementing Section(s) |
|---|---|---|
| 3.1 | Save snapshot | §4 Snapshot Save |
| 3.2 | Snapshot metadata / index | §3 index.json, §4 Snapshot Save |
| 3.3 | List snapshots | §5 Snapshot List |
| 3.4 | Restore snapshot | §6 Snapshot Restore |
| 3.5 | Diff two snapshots | §7 Snapshot Diff |
| 3.6 | SnapshotDiff struct | §8 SnapshotDiff struct |
| 3.7 | Pruning old snapshots | §9 Storage Management |
| 3.8 | Concurrent snapshot operations | §10 Concurrency |
| 3.9 | Disk space checks | §9 Storage Management |
