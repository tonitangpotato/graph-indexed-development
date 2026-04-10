# ISS-005: Directory Co-location Signal Absent from Clustering Network

## Problem

Files in the same directory have a strong prior probability of belonging to the same module, but this signal is completely absent from the network that Infomap clusters. When import/call edges are sparse, Infomap has no fallback signal — files with few explicit dependencies become orphans even when their directory placement clearly indicates module membership.

## Root Cause

The current edge types in `build_network()` are:

| Edge Type | Weight | Source |
|-----------|--------|--------|
| `calls` | 1.0 | Function/method calls |
| `imports` | 0.8 | Import statements |
| `inherits` / `implements` | 0.5 | Type hierarchy |
| `depends_on` | 0.4 | General dependency |
| `defined_in` / `contains` / `belongs_to` | 0.2 | File↔symbol containment |

**`defined_in` / `contains` / `belongs_to` represent file↔function containment, NOT directory co-location.** There are zero edges between two files that share the same parent directory. A file with no imports and no callers is a complete isolate in the network — even if it sits in `src/auth/` alongside 15 other auth files.

This matters because:
- Config files, type definitions, constants, and utility files often have minimal explicit imports
- New/incomplete code may not yet have all dependency edges extracted
- Some languages (CSS, HTML, config) lack import-like constructs entirely
- Directory structure is the single strongest human-curated signal for module membership

## Proposed Fix

### 1. Add synthetic directory co-location edges

In `build_network()`, after collecting all file-level nodes:

```rust
// Group files by parent directory
let mut dir_groups: HashMap<&str, Vec<NodeIndex>> = HashMap::new();
for (idx, node) in file_nodes {
    if let Some(parent) = Path::new(&node.file_path).parent() {
        dir_groups.entry(parent.to_str().unwrap())
            .or_default()
            .push(idx);
    }
}

// Add edges between files in the same directory
for (_dir, files) in &dir_groups {
    for i in 0..files.len() {
        for j in (i+1)..files.len() {
            network.add_edge(files[i], files[j], dir_colocation_weight);
        }
    }
}
```

### 2. Configurable weight

Default weight: **0.3** (higher than `defined_in` at 0.2, lower than `depends_on` at 0.4).

Rationale: Directory co-location is a meaningful but weaker signal than explicit code dependencies. It should influence clustering when explicit edges are absent, but not override strong import/call patterns.

Should be configurable via `ClusterConfig` (ties into ISS-002).

### 3. Depth-aware weighting (optional enhancement)

Files sharing the exact same parent directory get full weight (0.3). Files sharing a grandparent but different parent get reduced weight (0.15). This reflects that `src/auth/handlers/` and `src/auth/models/` are more related than `src/auth/` and `src/api/`.

### 4. Quadratic edge explosion guard

For large directories (100+ files), pairwise edges create O(n²) edges. Guard:
- If directory has > N files (e.g., 50), either:
  - Cap at N=50 and skip co-location for oversized directories (they're likely poorly organized anyway)
  - Or use a representative sampling approach
- Log a warning when this triggers

## Impact

- `crates/gid-core/src/infer/clustering.rs` — `build_network()` gains ~30 lines
- `ClusterConfig` gains `dir_colocation_weight: Option<f64>`
- No API changes to public interface

## Verification

On Claude Code's 1,902-file graph:
- Count files with zero edges before and after adding co-location edges
- Compare Infomap community assignments — co-located files should cluster together
- Verify no oversized directories cause O(n²) blowup

## Related Issues

- ISS-003: Orphan reassignment bugs (directory fallback in ISS-003 is a bandaid; ISS-005 is the proper fix upstream)
- ISS-004: Teleportation rate (co-location edges help Infomap detect communities even at higher τ)
- ISS-002: Configurable edge weights (co-location weight should be configurable too)

## Priority

**P1** — Important for clustering quality, especially for files with sparse explicit dependencies. But ISS-003 fixes are more impactful and should land first. ISS-005 improves the input signal quality so Infomap produces better raw output.

## Execution Order

Recommended: ISS-003 (P0) → ISS-004 (P1) → ISS-005 (P1) → ISS-002 (P1, enables tuning all weights)

## Status: Done
