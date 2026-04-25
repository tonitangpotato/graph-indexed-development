# ISS-006: ID Collision in split_mega_clusters

**Status:** closed (2026-04-25 — global renumbering fix verified)
**Severity**: Critical  
**Component**: `crates/gid-core/src/infer/clustering.rs`  
**Reported**: 2026-04-10  
**Closed**: 2026-04-25 — fix verified in `split_mega_clusters` wrapper (clustering.rs:872–878): after recursive splitting completes, all clusters get globally renumbered `c.id = i` over the final flat list. Inner `split_mega_clusters_recursive` doesn't renumber but never returns to user-facing code without going through the wrapper. Comment at line 875 explicitly references ISS-006. Production code path (`cluster()` at line 2200) goes through the wrapper. ⚠️ Follow-up nice-to-have: add a dedicated regression test that constructs a deeply-nested split scenario and asserts all final cluster IDs are unique — currently relies on the renumbering being correct rather than testing it directly.

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
