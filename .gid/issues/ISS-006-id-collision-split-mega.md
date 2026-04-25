# ISS-006: ID Collision in split_mega_clusters

**Status:** open
**Severity**: Critical  
**Component**: `crates/gid-core/src/infer/clustering.rs`  
**Reported**: 2026-04-10  
**Note**: This number was reused — see issues-index.md ISS-006 (closed 2026-04-05) for the earlier issue with the same ID.

## Problem

`split_mega_clusters_recursive` assigns sub-cluster IDs using `base_id = result_clusters.len()` + `sub_idx`. During recursive splits, this can produce duplicate IDs — two different clusters end up with the same `id` field.

`map_to_components` then uses `cluster.id` to generate `"infer:component:{id}"` node IDs → **collision**: two components share the same graph node ID, one overwrites the other.

### Root Cause

`split_oversized_by_directory` (hierarchical mode) has a final renumbering step:
```rust
for (i, c) in result.iter_mut().enumerate() { c.id = i; }
```

But the flat-mode `split_mega_clusters` path **does NOT** do this final renumbering after recursive splitting.

### Location

- `clustering.rs` line ~478: `id: base_id + sub_idx` inside `split_mega_clusters_recursive`
- `clustering.rs` `map_to_components`: uses `cluster.id` for `format!("infer:component:{}", ...)`

## Fix

Add global renumbering after `split_mega_clusters` returns (or at the end of the recursive function), same pattern as `split_oversized_by_directory`:

```rust
for (i, c) in clusters.iter_mut().enumerate() {
    c.id = i;
}
```
