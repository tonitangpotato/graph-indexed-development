# ISS-003: Orphan Reassignment Logic Has Four Bugs → 69% Single-File Components

## Problem

Running `gid infer` on a 1,902-file codebase (Claude Code) produced 339 components with 236 single-file clusters (69%). The **direct cause** is four bugs in the orphan reassignment logic in `clustering.rs:315-365` — small communities filtered by `min_community_size` are supposed to be merged into neighboring clusters, but the reassignment code is fundamentally broken.

## Root Cause Analysis

### Bug A: Only checks `out_neighbors`, ignores `in_neighbors`

```rust
// Line 328-333: ONLY outgoing edges checked
for &(neighbor_idx, w) in net.out_neighbors(*node_idx) {
    if let Some(&ci) = net_idx_to_cluster.get(&neighbor_idx) {
        if w > best_weight {
            best_weight = w;
            best_cluster = Some(ci);
        }
    }
}
```

The comment on lines 336-345 literally says "we should check in_neighbors too... let's just use outgoing for simplicity." But `in_neighbors()` IS public on Network.

**Why this matters**: If file A imports file B, the edge is A→B. When B is the orphan, only `out_neighbors` sees B's own dependencies — not who depends on B. For leaf files that mostly *receive* imports (utility modules, type definitions), `out_neighbors` may be empty while `in_neighbors` is rich.

**Impact**: ~50% of potential merge targets missed.

### Bug B: Each unmerged orphan becomes its own singleton cluster

```rust
// Line 355-363: Each misc orphan = its own 1-file cluster
if !misc_ids.is_empty() {
    for misc_id in misc_ids {
        clusters.push(RawCluster {
            id: clusters.len(),
            member_ids: vec![misc_id],
            ...
        });
    }
}
```

This is the line that directly creates the 236 single-file components. The fallback should group remaining orphans by directory path, not create individual singleton clusters.

### Bug C: No iterative propagation

Orphans are processed in a single pass. If orphan A could merge into cluster X, but the only path is through orphan B (which hasn't been assigned yet), A falls through to misc. Needs multi-pass iteration until convergence (no new merges in a round).

### Bug D: Weight comparison ignores aggregate cluster affinity

The code picks the single strongest-connected *neighbor*, not the cluster with the strongest *total* edge weight. If an orphan has 3 weak edges (w=0.3 each) to cluster X and 1 strong edge (w=0.8) to cluster Y, it picks Y. But X has stronger aggregate affinity (0.9 > 0.8).

## Proposed Fix

1. **Check both `out_neighbors` AND `in_neighbors`** for merge target selection
2. **Aggregate by cluster**: sum total weight to each candidate cluster, pick highest aggregate
3. **Multi-pass propagation**: loop orphan reassignment until no more merges occur in a round
4. **Directory-based fallback**: orphans with no edges after all passes → group by shared parent directory
5. **No singleton misc clusters**: remaining true isolates go into nearest-directory cluster or a single "unclassified" component

## Impact

- `crates/gid-core/src/infer/clustering.rs` — `reassign_orphans()` rewrite (~50 lines)
- No API changes, no new dependencies
- **Estimated reduction**: 69% single-file → <10% single-file (Phase 1 alone)

## Verification

Re-run on Claude Code's 1,902-file graph:
- Single-file components: target < 5% (from 69%)
- Component count: target 30-80 (from 339)
- Median component size: 3-10 files

## Related Issues

- ISS-002: Configurable edge weights (complementary, not blocking)
- ISS-004: Teleportation rate too high (amplifies fragmentation)
- ISS-005: Directory co-location signal missing (no fallback for sparse edges)

## Priority

**P0** — This is the single highest-impact fix. The bugs are clear, the fix is straightforward, and it should eliminate ~80% of fragmentation on its own.

## Status: Done
