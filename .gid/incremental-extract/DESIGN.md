# ISS-006: Incremental Updates for `gid extract`

## Overview

This feature implements incremental extraction for the `gid extract` command to dramatically improve performance when re-extracting code graphs. Instead of re-parsing all files every time, only changed files are processed, with proper cleanup of stale nodes and dangling edges.

## Requirements

1. **Performance Targets**:
   - No-change case: <100ms (vs ~2-5s full rebuild)
   - 1-5 changed files: <1s (vs ~2-5s full rebuild)
   - 50+ changed files: Comparable to full rebuild

2. **Correctness**:
   - Stale nodes and edges must be removed when files change
   - Dangling edges (pointing to removed nodes) must be cleaned up
   - Version mismatch or corruption → automatic fallback to full rebuild
   - Must handle file renames, moves, and deletions correctly

3. **User Experience**:
   - Transparent: works automatically without user action
   - `--full` flag available to force full rebuild
   - Clear logging of what's being processed

## Architecture

### Metadata Structures

```rust
/// Metadata tracking the state of the last extraction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractMetadata {
    /// Schema version for compatibility checking
    pub version: u32,
    /// Map of file_path → FileState
    pub files: HashMap<String, FileState>,
}

/// State of a single file from the last extraction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileState {
    /// xxHash64 of file contents
    pub content_hash: u64,
    /// Last modified time (seconds since epoch)
    pub mtime: u64,
    /// Node IDs created by parsing this file
    pub node_ids: Vec<String>,
    /// Count of edges where this file's nodes are source (for validation)
    pub edge_count: usize,
}
```

**Key design decisions**:
- xxHash64 for fast hashing (faster than SHA256, sufficient for change detection)
- Both `content_hash` and `mtime` stored; mtime for fast pre-filter, hash for definitive change detection
- `node_ids` enables precise removal of stale nodes
- `edge_count` for validation only (not used for edge removal)
- Current schema version: `1`

### Persistence

- Location: `.gid/extract-meta.json`
- Format: JSON (human-readable for debugging)
- Atomic write: Write to temp file, then rename (prevents corruption on crash)
- Validation on load: Check version, handle missing/corrupt file gracefully

### Incremental Pipeline

The extraction process follows a 5-phase pipeline:

```
Phase 1: Scan Files
├─ Read extract-meta.json (or create empty if missing/corrupt)
├─ Scan filesystem for all source files
├─ For each file:
│  ├─ Check mtime against metadata
│  ├─ If mtime changed → compute content_hash
│  └─ Classify as: unchanged, changed, or new
└─ Identify removed files (in metadata but not on disk)

Phase 2: Remove Stale Nodes
├─ For each removed or changed file:
│  ├─ Get node_ids from metadata
│  └─ Remove nodes from graph
└─ Update node_index, outgoing, incoming maps

Phase 3: Parse Changed Files
├─ For each changed or new file:
│  ├─ Parse file → new nodes and edges
│  ├─ Track node_ids for metadata
│  └─ Add nodes to graph
└─ Create new FileState entries

Phase 4: Resolve References & Add Edges
├─ For changed files only:
│  └─ Resolve references → edges
├─ Add edges to graph
└─ Update adjacency lists

Phase 5: Cleanup & Save
├─ Remove dangling edges (source or target node_id not in graph)
├─ Save updated graph to graph.json
└─ Save updated metadata to extract-meta.json
```

### Dangling Edge Cleanup

After removing stale nodes (Phase 2), we must clean up dangling edges:

```rust
// After removing stale nodes, scan ALL edges
graph.edges.retain(|edge| {
    let source_exists = graph.node_index.contains_key(&edge.from);
    let target_exists = graph.node_index.contains_key(&edge.to);
    source_exists && target_exists
});
```

**Why this is necessary**:
- When a file changes, we remove all its nodes
- Edges from OTHER files may point to these removed nodes
- These become "dangling edges" that must be removed
- We use `node_ids` (not `edge_count`) to identify nodes to remove

### Version Compatibility

```rust
const METADATA_VERSION: u32 = 1;

fn load_metadata() -> Option<ExtractMetadata> {
    let meta = read_from_disk()?;
    if meta.version != METADATA_VERSION {
        warn!("Metadata version mismatch, falling back to full rebuild");
        return None;
    }
    Some(meta)
}
```

**Fallback scenarios**:
1. Missing `extract-meta.json` → full rebuild
2. Corrupted JSON → full rebuild
3. Version mismatch → full rebuild
4. Any deserialization error → full rebuild

### Change Detection

**Two-level check**:
1. **mtime** (fast): Pre-filter to identify potential changes
2. **content_hash** (definitive): Only for files with changed mtime

```rust
fn classify_file(path: &Path, meta: &FileState) -> FileStatus {
    let mtime = get_mtime(path);
    if mtime == meta.mtime {
        return FileStatus::Unchanged;
    }
    
    let content_hash = compute_xxhash64(path);
    if content_hash == meta.content_hash {
        // mtime changed but content didn't (e.g., git checkout)
        return FileStatus::Unchanged;
    }
    
    FileStatus::Changed
}
```

### Edge Removal Algorithm

When a file changes, we must remove edges involving its nodes:

```rust
// Get all node_ids from the file being removed/changed
let stale_node_ids: HashSet<_> = file_state.node_ids.iter().collect();

// Remove edges where EITHER endpoint is stale
graph.edges.retain(|edge| {
    !stale_node_ids.contains(&edge.from) && 
    !stale_node_ids.contains(&edge.to)
});
```

**Important**: We check BOTH `from` and `to` because:
- `from` edges: Created by this file (e.g., calls, imports)
- `to` edges: Created by other files pointing to this file's entities

## Implementation Plan

### Files to Modify

1. **crates/gid-core/src/code_graph/types.rs**
   - Add `ExtractMetadata` and `FileState` structs
   - Add serialization support

2. **crates/gid-core/src/code_graph/extract.rs**
   - Add metadata persistence functions
   - Implement 5-phase incremental pipeline
   - Add change detection logic
   - Add dangling edge cleanup
   - Add `--full` flag support

3. **crates/gid-core/src/code_graph/mod.rs**
   - Export new metadata types
   - Update public API if needed

### Testing Strategy

1. **Unit tests**:
   - `test_metadata_serialization()`
   - `test_content_hash_computation()`
   - `test_change_detection()`
   - `test_dangling_edge_cleanup()`

2. **Integration tests**:
   - `test_no_change_case()` - Verify <100ms
   - `test_single_file_change()` - Verify correct update
   - `test_multiple_file_changes()` - Verify correctness
   - `test_file_deletion()` - Verify stale node removal
   - `test_version_mismatch()` - Verify fallback
   - `test_corrupt_metadata()` - Verify fallback

3. **Performance benchmarks**:
   - Measure no-change case
   - Measure 1, 5, 10, 50 file changes
   - Compare against full rebuild

## Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Dangling edges after node removal | High - Corrupted graph | Comprehensive edge cleanup in Phase 5 |
| Hash collisions | Low - Incorrect change detection | xxHash64 has excellent collision resistance |
| Metadata corruption | Medium - Failed extraction | Automatic fallback to full rebuild |
| Performance regression on large repos | Medium - No improvement | Profile and optimize hot paths |
| Breaking changes to graph format | High - Incompatible versions | Version field in metadata |

## Success Metrics

1. **Performance**: Achieve <100ms for no-change, <1s for 1-5 files
2. **Correctness**: 100% test pass rate, no dangling edges in CI
3. **Adoption**: No user-reported issues with incremental extraction
4. **Fallback rate**: <1% of extractions fall back to full rebuild

## Future Enhancements

1. **Parallel file parsing**: Use rayon for multi-threaded parsing
2. **Incremental reference resolution**: Only re-resolve affected references
3. **Smart edge invalidation**: Track edge dependencies more precisely
4. **Cache ASTs**: Store parsed ASTs alongside metadata for even faster re-extraction
5. **Delta encoding**: Store only changed portions of large files

## References

- Original issue: ISS-006
- Related: Performance optimization epic
- Inspiration: Rust's incremental compilation model
