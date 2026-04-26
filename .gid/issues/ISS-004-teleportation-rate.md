# ISS-004: Infomap Teleportation Rate Too High for Code Graphs

**Status:** done

## Problem

The default teleportation rate `tau=0.15` in Infomap causes over-fragmentation on code dependency graphs. The random walker escapes communities too easily, reducing Infomap's ability to detect module structure.

## Root Cause

Teleportation (τ) controls the probability that a random walker jumps to a random node instead of following an edge. At τ=0.15, 15% of all steps are random jumps — appropriate for web graphs (PageRank, adversarial link structures) but far too high for code graphs which are:

- **More structured** — directory trees, module boundaries, layered architecture
- **Sparser per-node** — most files import 3-10 others, not hundreds
- **Denser within clusters** — files in the same module have high interconnection
- **Non-adversarial** — no link farms or spam structures to escape from

**Effect of high teleportation**: The walker escapes tight communities before Infomap can detect them → fewer communities detected → small genuine clusters get filtered by `min_community_size` → cascade into orphan reassignment (ISS-003) → singletons.

Literature on community detection in structured graphs suggests τ=0.01-0.05. Rosvall & Bergstrom's original Infomap papers use τ=0.15 as a default for web-scale graphs, not code dependency graphs.

## Current Code

```rust
// infer/clustering.rs — auto_config()
tau: 0.15,  // hardcoded default
```

No user override, no adaptation based on graph properties.

## Proposed Fix

### 1. Lower default for code graphs

Change default `tau` from 0.15 → 0.05.

### 2. Adaptive τ based on graph density

In `auto_config()`, compute average degree and set τ accordingly:

```
avg_degree < 3  → τ = 0.01  (very sparse, keep walker in community)
avg_degree 3-20 → τ = 0.05  (normal code graph)
avg_degree > 20 → τ = 0.10  (dense graph, need some teleportation)
```

### 3. Make τ configurable

Add `teleportation_rate: Option<f64>` to `ClusterConfig`. When set, overrides auto-detection.

### 4. Increase `num_trials`

Raise from 5 → 10 to improve optimization convergence with lower τ (tighter communities = more local optima).

## Impact

- `crates/gid-core/src/infer/clustering.rs` — `auto_config()` and `ClusterConfig`
- Small change, high leverage on clustering quality
- Should reduce raw fragmentation from Infomap before orphan reassignment even runs

## Verification

Compare Infomap output (before orphan reassignment) at different τ values on Claude Code graph:
- Count raw communities at τ=0.15, τ=0.05, τ=0.01
- Measure: % single-node communities, largest community size, community count
- Target: fewer raw singletons, more mid-size communities

## Related Issues

- ISS-003: Orphan reassignment bugs (downstream — fixes symptoms ISS-004 creates)
- ISS-049: Configurable edge weights (complementary tuning)
- ISS-005: Directory co-location edges (alternative signal for sparse subgraphs)

## Priority

**P1** — Important but lower priority than ISS-003. Fixing orphan reassignment handles the immediate symptoms. Tuning τ improves the raw Infomap output so there are fewer orphans to reassign in the first place.

## Status: Done
