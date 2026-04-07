# ISS-006: Incremental Updates for `gid extract`

## Problem

`gid extract --lsp` rebuilds the entire code graph from scratch every time:
- Tree-sitter parses **every** source file (~80 files for RustClaw)
- LSP refines **every** low-confidence call edge (hundreds of queries)
- Cold-start rust-analyzer takes ~8 minutes for large projects (587 crates)

Changing one file triggers the same ~10 minute pipeline as a full rebuild.

## Design

### Approach: File-level delta with persistent metadata

Store file metadata (mtime + content hash) alongside the graph. On extract, compare current state vs stored metadata to compute a delta (added/modified/deleted files), then only re-parse and re-refine the delta.

### New Data Structures

```rust
/// Stored alongside the code graph for change detection.
#[derive(Serialize, Deserialize)]
pub struct ExtractMetadata {
    /// Schema version — bump on struct changes. Mismatch → full rebuild.
    pub version: u32,
    /// When this metadata was last updated.
    pub updated_at: String,
    /// Per-file tracking: path → FileState
    pub files: HashMap<String, FileState>,
}

#[derive(Serialize, Deserialize)]
pub struct FileState {
    /// File modification time (Unix seconds).
    pub mtime: u64,
    /// xxHash64 of file content (fast, already a dep).
    pub content_hash: u64,
    /// Node IDs that were extracted from this file.
    pub node_ids: Vec<String>,
    /// Number of edges originating from this file (reporting only).
    /// Edge removal uses `node_ids` to find edges where source ∈ node_ids.
    pub edge_count: usize,
}

/// Result of comparing current filesystem vs stored metadata.
pub struct FileDelta {
    pub added: Vec<String>,    // New files not in metadata
    pub modified: Vec<String>, // Files with changed mtime/hash
    pub deleted: Vec<String>,  // Files in metadata but not on disk
    pub unchanged: Vec<String>,
}
```

**Storage location:** `.gid/extract-meta.json` (next to `graph.yml`)

### Modified Extraction Flow

```
gid extract [--force]
    │
    ├─ Load existing graph + metadata from .gid/
    │
    ├─ Scan directory → compute FileDelta
    │   ├─ Quick path: compare mtime only (< 1ms per file)
    │   └─ If mtime changed: compute content hash to confirm
    │
    ├─ If delta is empty → "Graph is up to date" (exit)
    │
    ├─ Phase 1: Remove stale data
    │   ├─ For deleted files: remove their nodes + edges from graph
    │   ├─ For modified files: remove their nodes + edges (will re-add)
    │   └─ Scan ALL edges; remove any where source or target
    │       node_id no longer exists (dangling edge cleanup)
    │
    ├─ Phase 2: Parse changed files only
    │   ├─ Tree-sitter parse added + modified files
    │   ├─ Extract nodes, edges, imports (same per-file functions)
    │   └─ Merge into existing graph
    │
    ├─ Phase 3: Re-resolve cross-file references
    │   ├─ Rebuild name maps (class_map, func_map, module_map)
    │   │   from ALL nodes (existing unchanged + new)
    │   ├─ Resolve placeholder refs only for new/modified edges
    │   └─ Deduplicate + recompute weights
    │
    ├─ Phase 4: LSP refinement (if --lsp)
    │   ├─ Open only changed files in LSP server
    │   ├─ Query definition only for edges from changed files
    │   ├─ Keep existing LSP-refined edges for unchanged files
    │   └─ (Daemon client: instant if already warm)
    │
    ├─ Phase 5: Save
    │   ├─ Serialize graph
    │   └─ Update extract-meta.json
    │
    └─ Report: "Updated 3 files (2 modified, 1 added), 42 unchanged"
```

### Key Design Decisions

**1. Mtime-first, hash-second change detection**

Check mtime first (free from directory walk metadata). Only compute content hash when mtime differs — this catches cases where mtime changed but content didn't (e.g., `touch` or editor save-without-change). xxHash64 is already a dependency and processes ~10 GB/s.

**2. Remove-then-reinsert for modified files**

Rather than trying to diff ASTs, simply remove all nodes/edges from a modified file and re-extract. This is safe and simple — the expensive part (LSP queries) is already scoped to changed files.

**3. Full reference resolution pass (but scoped edge resolution)**

Name maps (`class_map`, `func_map`) must include ALL nodes (unchanged + new) since a new file might call an existing function. But placeholder resolution only runs on edges from changed files. This is O(changed_edges) not O(all_edges).

**4. `--force` flag for full rebuild**

Always available as escape hatch. Also triggered automatically if `extract-meta.json` is missing or corrupted.

**5. LSP daemon synergy**

The daemon keeps rust-analyzer warm between extracts. Combined with incremental:
- First extract: ~8 min (cold RA + full parse)
- Subsequent with changes: ~2-5 seconds (warm RA + delta parse)
- Subsequent no changes: instant ("up to date")

### File Changes

| File | Change |
|------|--------|
| `code_graph/types.rs` | Add `ExtractMetadata`, `FileState`, `FileDelta` structs |
| `code_graph/extract.rs` | New `extract_incremental()` method, refactor `extract_from_dir()` to share per-file parsing logic |
| `code_graph/build.rs` | Add `refine_with_lsp_incremental(changed_files)` variant |
| `code_graph/mod.rs` | Export new types |
| `gid-cli/src/main.rs` | Add `--force` flag, call incremental by default |

### API

```rust
impl CodeGraph {
    /// Incremental extraction: only re-parse changed files.
    /// Falls back to full extraction if no prior metadata exists.
    pub fn extract_incremental(
        dir: &Path,
        meta_path: &Path,      // .gid/extract-meta.json
        force: bool,           // --force: ignore cache, full rebuild
    ) -> Result<(Self, ExtractReport)>;
}

pub struct ExtractReport {
    pub added: usize,
    pub modified: usize,
    pub deleted: usize,
    pub unchanged: usize,
    pub full_rebuild: bool,    // true if --force or no prior metadata
    pub duration_ms: u64,
}
```

### Edge Cases

| Case | Behavior |
|------|----------|
| No prior metadata | Full rebuild, create metadata |
| Corrupted metadata | Full rebuild, recreate |
| Version mismatch in metadata | Full rebuild (same as corrupted) |
| File modified during extract | May miss the change; next extract will catch it |
| File renamed (same content) | Shows as delete + add; nodes get new IDs |
| New file imports existing module | Reference resolution catches it (full name map rebuild) |
| Deleted file was imported by others | After Phase 1, scan ALL edges; remove any edge where source or target `node_id` no longer exists in the graph (catches edges from unchanged files pointing to deleted nodes) |
| Only non-source files changed | Delta empty → "up to date" |
| `--force` | Ignores metadata, full rebuild, rewrites metadata |

### Performance Targets

| Scenario | Current | Target |
|----------|---------|--------|
| Full rebuild (80 files) | ~3s parse + ~8min LSP | Same (unavoidable cold start) |
| 1 file changed, no LSP | ~3s | < 500ms |
| 1 file changed, warm LSP daemon | ~8min | < 3s |
| No changes | ~3s | < 100ms |

### Not In Scope

- AST-level diffing (overkill; remove+reinsert is simple and fast enough)
- Watching for file changes (inotify/fsevents) — users run `gid extract` explicitly
- Incremental graph serialization — full serialize is fast enough (< 50ms)
- Cross-repo incremental (path deps like gid-core → rustclaw)
