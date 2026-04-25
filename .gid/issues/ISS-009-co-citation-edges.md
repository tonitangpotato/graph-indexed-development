# ISS-009: Clustering input graph lacks co-citation edges — root cause of monolithic utility clusters

**Status:** open
**Severity**: Architecture (root cause)  
**Discovered**: 2026-04-10, after 6+ rounds of debugging clustering quality  
**Supersedes**: ISS-006 (split_mega fallback), ISS-008 (max_cluster_size formula) — these were symptom-level fixes

## Problem

Utility/helper directories with hundreds of files (e.g., `utils/` with 294 files, `hooks/` with 80 files, `components/` with 110 files) produce monolithic clusters that Infomap cannot split. After multiple rounds of fixes:

- ISS-006: Added directory-based fallback when Infomap sub-clustering returns 1 module
- ISS-007: Fixed colocation hardcoded threshold
- ISS-008: Changed max_cluster_size from linear to log-based formula

These all attack the problem **after** Infomap runs. The residual 3 mega-clusters (utils-294, components-110, hooks-80) remain unsplittable because:

1. Infomap returns 1 module (monolithic) → `split_mega_clusters` gives up
2. All files are in the same directory → `split_oversized_by_directory` gives up
3. No further fallback exists

## Root Cause

**The input graph fed to Infomap is too sparse for utility-style code.**

Currently, `build_network()` creates edges from these sources:

| Source | Edge Type | Weight |
|--------|-----------|--------|
| `calls` | Direct function call | 1.0 |
| `imports` | Import statement | 0.8 |
| `type_reference` / `inherits` / `implements` / `uses` | Type coupling | 0.5 |
| `depends_on` | General dependency | 0.4 |
| `defined_in` / `contains` / `belongs_to` | Structural | 0.2 |
| Directory co-location | Synthetic (ISS-005) | 0.3 (decayed for large dirs) |

**All of these are direct edges between files.** The critical missing signal is **indirect co-usage**:

Utility files are **consumed** by other modules but rarely **import each other**. `useAuth.ts` and `useSession.ts` both live in `hooks/` and are both imported by the same set of feature files, but they have zero direct edges between them. Infomap sees them as isolated nodes (or weakly connected via decayed colocation), so it cannot form meaningful communities.

This is analogous to academic citation analysis: two papers that never cite each other but are frequently **co-cited** by the same papers are topically related. The same principle applies to code.

## Analysis: Why previous fixes were symptom-level

```
Root cause: sparse input graph
    ↓
Symptom: Infomap produces mega-clusters
    ↓
Patch 1 (ISS-006): split_mega_clusters with Infomap re-run → fails (still sparse subgraph)
    ↓
Patch 2 (ISS-006): directory fallback → fails (all in same directory)
    ↓
Patch 3 (ISS-008): tighter max_cluster_size → more clusters flagged, but still can't split them
```

The correct fix is enriching the input graph so Infomap has enough signal on the **first** pass.

## Solution: Co-citation edges

### What is co-citation?

If file A and file B are both imported by file C, then A and B have a **co-citation** relationship through C. The more shared importers they have, the stronger the signal that A and B belong to the same functional domain.

```
Feature module X imports: useAuth, useSession, AuthContext
Feature module Y imports: useAuth, useSession, LoginForm
Feature module Z imports: usePermissions, useSession, AuthGuard

→ useAuth ↔ useSession: co-cited by {X, Y} → weight = 2
→ useAuth ↔ AuthContext: co-cited by {X} → weight = 1
→ useAuth ↔ LoginForm: co-cited by {Y} → weight = 1
→ useSession ↔ usePermissions: co-cited by {Z} → weight = 1
```

### Algorithm

In `build_network()`, after constructing direct edges:

1. **Build reverse-import index**: for each file node F, collect the set of files that import F (its "citers")
2. **For each pair of files (A, B) in the same mega-directory** (optimization: only compute co-citation for files that lack strong direct edges):
   - Compute `shared_citers = citers(A) ∩ citers(B)`
   - If `|shared_citers| >= threshold` (e.g., 2): add edge A↔B with weight = `WEIGHT_CO_CITATION * |shared_citers|` (capped)
3. **Weight**: `WEIGHT_CO_CITATION = 0.4` — stronger than colocation (0.3) but weaker than direct imports (0.8). This reflects that co-citation is a strong but indirect signal.

### Scope control

To avoid O(n²) explosion on the full file set:
- **Only compute co-citation for files that are "import targets"** — files with ≥1 incoming import edge. Files that import nothing and are imported by nobody won't benefit.
- **Min co-citation threshold = 2** — a single shared importer is noise; 2+ is signal.
- **Weight cap** — `min(shared_count * 0.4, 2.0)` — prevents a utility imported by 50 files from creating super-heavy edges.

### Why this works for the specific problem

The 294 utils files are **exactly** the pattern co-citation targets:
- Each util is imported by multiple feature files
- Utils serving the same feature domain are imported by the **same** feature files
- Co-citation edges will connect `useAuth` ↔ `useSession` ↔ `AuthContext` because they're all imported by auth-related features
- Infomap will see dense sub-communities within the previously-monolithic utils cluster

### Implementation location

```rust
// In clustering.rs, new function:
pub fn add_co_citation_edges(
    net: &mut Network, 
    graph: &Graph, 
    idx_to_id: &[String],
    weight: f64,        // WEIGHT_CO_CITATION = 0.4
    min_shared: usize,  // minimum shared citers (default: 2)
    max_weight: f64,    // cap per edge (default: 2.0)
)

// Called in cluster() between build_network() and add_dir_colocation_edges():
pub fn cluster(graph: &Graph, config: &ClusterConfig) -> Result<ClusterResult> {
    let (mut net, idx_to_id) = build_network(graph);
    
    // NEW: Add co-citation edges (indirect usage signal)
    add_co_citation_edges(&mut net, graph, &idx_to_id, config.co_citation_weight, 2, 2.0);
    
    // Existing: Add directory co-location edges
    add_dir_colocation_edges(&mut net, &idx_to_id, config.dir_colocation_weight);
    
    // ... rest unchanged
}
```

### Config additions

```rust
pub struct ClusterConfig {
    // ... existing fields ...
    
    /// Weight for synthetic co-citation edges (default: 0.4).
    /// Two files imported by the same set of consumers get connected.
    /// Set to 0.0 to disable co-citation.
    pub co_citation_weight: f64,
    
    /// Minimum number of shared citers to create a co-citation edge (default: 2).
    pub co_citation_min_shared: usize,
}
```

## Expected Impact

- **Utils-294**: files like `useAuth`, `useSession`, `usePermissions` that serve auth features will form a co-citation community. `formatDate`, `formatNumber` that serve display features will form another.
- **Components-110**: UI components used together on the same pages will cluster.
- **Hooks-80**: hooks used by the same feature modules will group.

The `split_mega_clusters` and `split_oversized_by_directory` fallbacks remain as safety nets, but should trigger far less often.

## Future extensions (not in this ISS)

Co-citation is the highest-value edge type to add. Future enrichments could include:
- **Symbol name similarity** — files exporting `formatX` functions cluster together
- **Type co-reference** — files that use the same types are related
- **Bibliographic coupling** — if A and B import the same set of dependencies (inverse of co-citation)

Each would be a separate `add_X_edges()` function following the same pattern.

## Test plan

1. **Unit test**: construct a graph with 10 "util" files in one directory, no mutual imports, but 3 "feature" files that each import overlapping subsets. Verify co-citation edges are created with correct weights.
2. **Unit test**: verify min_shared threshold — 1 shared citer should NOT create an edge when min_shared=2.
3. **Unit test**: verify weight cap works.
4. **Integration test**: run full `cluster()` on a graph mimicking the utils-294 pattern. Verify the monolithic cluster is now split.
5. **Regression**: existing two-community tests still pass (co-citation shouldn't break well-connected graphs).
6. **Real-world validation**: re-run on Claude Code codebase, compare cluster sizes before/after.
