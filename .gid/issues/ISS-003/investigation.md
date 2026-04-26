# ISS-003 Investigation: Root Cause Analysis

**Status:** companion-doc (parent: ISS-003, status: done — see ISS-003-clustering-fragmentation.md)
**Type:** investigation report (not a separate issue)

## Executive Summary

The 69% single-file fragmentation has **three interacting root causes**, not one. The current orphan reassignment logic is fundamentally flawed — it only checks outgoing edges, creates singleton "misc" clusters, and processes orphans sequentially without propagation. But even fixing the reassignment won't help if Infomap itself over-fragments because of teleportation and edge weight calibration issues.


## Root Cause 1: Orphan reassignment is broken (clustering.rs:315-365)

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

The comment on lines 336-345 literally says "we should check in_neighbors too... let's just use outgoing for simplicity." But `in_neighbors()` IS public on Network. **If file A imports file B, the edge is A→B. When B is the orphan, only out_neighbors sees B's dependencies, not who depends on B.** For leaf files that mostly *receive* imports (like utility modules), out_neighbors may be empty while in_neighbors is rich.

**Impact**: ~50% of potential merge targets missed. Many orphans fall through to the "misc" path unnecessarily.

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

This directly creates the 236 single-file components. The fallback should group by directory path, not create singletons.

### Bug C: No iterative propagation

Orphans are processed in a single pass. If orphan A could merge into cluster X, but the only path is through orphan B (which hasn't been assigned yet), A falls through to misc. Need multi-pass until convergence.

### Bug D: Weight comparison ignores aggregate cluster affinity

The code picks the single strongest-connected *neighbor*, not the cluster with the strongest *total* edge weight. If an orphan has 3 weak edges (w=0.3 each) to cluster X and 1 strong edge (w=0.8) to cluster Y, it picks Y. But X is actually the better home (0.9 > 0.8 aggregate).


## Root Cause 2: Teleportation rate too high (τ=0.15)

Default `tau=0.15` means 15% of random walk jumps teleport randomly. This is fine for web graphs (PageRank) but too high for code dependency graphs which are:
- More structured (directory trees, module boundaries)
- Sparser per-node but denser in clusters
- Not adversarially constructed (no link farms)

**Effect of high teleportation**: The random walker escapes communities too easily → Infomap sees less module structure → fewer/larger communities at one end, but also can't merge small disconnected subgraphs → singletons at the other end. The resolution limit is exacerbated.

**Literature suggests τ=0.01-0.05 for structured graphs.** We should default to τ=0.05 for code graphs and make it configurable.


## Root Cause 3: Directory structure signal completely unused

Code in the same directory has a strong prior probability of belonging to the same module. Currently:

- `defined_in` / `contains` / `belongs_to` edges get weight 0.2 (structural)
- But these represent file↔function containment, NOT directory co-location
- **There are ZERO synthetic edges between files in the same directory**

When Infomap can't determine community from import/call edges alone (sparse subgraphs), it has no fallback signal. Directory co-location is the single strongest heuristic for code organization and it's entirely absent from the network.


## Proposed Fix Plan

### Phase 1: Fix orphan reassignment (CRITICAL)

1. **Check both `out_neighbors` AND `in_neighbors`** for merge target selection
2. **Aggregate by cluster**: sum total weight to each candidate cluster, not single-best-neighbor
3. **Multi-pass propagation**: iterate orphan reassignment until no more merges occur
4. **Directory-based fallback**: orphans with no edges → group by shared directory prefix
5. **No singleton misc clusters**: after all passes, remaining true isolates go into a single "unclassified" component or nearest-directory cluster

### Phase 2: Add directory co-location edges

In `build_network()`, after collecting file nodes:
- For each pair of files sharing the same parent directory, add a synthetic edge with configurable weight (default 0.3)
- This gives Infomap a baseline signal for files that lack explicit import/call edges
- Weight should be lower than `imports` (0.8) but meaningful

### Phase 3: Tune Infomap parameters

- Default `tau` from 0.15 → 0.05 for code graphs
- `auto_config()` should set tau based on graph density:
  - Sparse graphs (avg degree < 3): tau=0.01 (keep walker in community)
  - Normal graphs: tau=0.05
  - Dense graphs (avg degree > 20): tau=0.10
- Increase `num_trials` from 5→10 for better optimization


## Verification Plan

After fixes, re-run on Claude Code's 1,902-file graph and measure:
- **Single-file components**: target < 5% (from 69%)
- **Component size distribution**: median should be 3-10 files
- **Max component**: < 100 files (from 206)
- **Gini coefficient** of component sizes: < 0.6 (from ~0.9)


## Priority

Phase 1 alone should reduce single-file components by ~80%. The bugs in orphan reassignment are the immediate cause — the Infomap resolution limit makes it worse, but most files DO have edges, they just don't get merged because the reassignment logic is broken.
