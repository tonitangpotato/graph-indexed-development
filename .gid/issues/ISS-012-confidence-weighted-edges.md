# ISS-012: Edge Confidence Ignored in Clustering — LSP and Heuristic Edges Weighted Identically

**Status:** open (fix in stash@{1} — needs review/commit, see 2026-04-25 sweep notes)
**Priority:** P0

## Problem

`build_network()` and `add_co_citation_edges()` completely ignore the `confidence` field on graph edges. This means:

- LSP-confirmed edges (`confidence = 1.0`, ground truth) get the same Infomap weight as tree-sitter heuristic guesses (`confidence = 0.3–0.5`)
- Co-citation treats every import-like edge as equally valid, inflating synthetic coupling from noisy heuristic edges
- Co-citation adds redundant edges between file pairs that LSP already confirmed are coupled, double-counting the signal

**Net effect:** Infomap clustering treats precise LSP data no differently from fuzzy guesses, degrading cluster quality. Projects with mixed-confidence edges (common after LSP-augmented extract) get worse clustering than they should.

## Root Cause

Three locations in `clustering.rs` operate on edges without reading `edge.confidence`:

1. **`build_network()`** — computes `w = relation_weight(relation)` and accumulates directly. Never multiplies by confidence.
2. **`add_co_citation_edges()` — citer qualification** — any import-like edge counts as a citer, regardless of confidence. Low-confidence heuristic edges inflate shared-citer counts.
3. **`add_co_citation_edges()` — no direct-edge suppression** — co-citation happily adds synthetic edges between file pairs that already have high-confidence direct edges (LSP `calls`/`imports`), redundantly echoing existing signal.

## Fix (3 changes in `clustering.rs`)

### Change 1: `build_network()` — weight × confidence

```rust
let confidence = edge.confidence.unwrap_or(1.0).clamp(0.0, 1.0);
let effective_weight = w * confidence;
// ...
*edge_weights.entry((f, t)).or_insert(0.0) += effective_weight;
```

- `None` defaults to `1.0` → backward compatible with legacy/manual edges
- LSP edges (1.0) get full relation weight; heuristic edges (0.3) get 30%

### Change 2: `add_co_citation_edges()` — high-confidence citers only

```rust
const CO_CITATION_CONFIDENCE_THRESHOLD: f64 = 0.7;

// In the reverse-import index builder:
let confidence = edge.confidence.unwrap_or(1.0).clamp(0.0, 1.0);
if confidence < CO_CITATION_CONFIDENCE_THRESHOLD {
    continue; // Skip low-confidence heuristic edges
}
```

- Only edges with confidence ≥ 0.7 qualify as citers
- `imported_by` changed from `HashMap<usize, HashSet<usize>>` to `HashMap<usize, HashMap<usize, f64>>` to track per-citer confidence
- Co-citation weight scaled by geometric mean confidence of shared citing edges: `(conf_a × conf_b).sqrt()` per citer → averaged across all shared citers

### Change 3: `add_co_citation_edges()` — suppress LSP-redundant pairs

```rust
// Build set of file pairs with direct high-confidence (≥0.9) coupling edges
let mut direct_high_confidence_pairs: HashSet<(usize, usize)> = HashSet::new();
// ... populate from edges with confidence ≥ 0.9 and relation in {calls, imports, uses, type_reference}

// In the shared-citer loop:
if direct_high_confidence_pairs.contains(&pair_key) {
    continue; // Don't add co-citation where LSP already confirmed coupling
}
```

- Prevents double-counting: if LSP already says A calls B (confidence 1.0), co-citation doesn't redundantly pile on
- Threshold 0.9 (not 0.7) ensures only very high confidence direct edges suppress co-citation

## Tests Added (6 new)

| Test | Validates |
|------|-----------|
| `test_build_network_confidence_scales_weight` | 1.0 conf → full weight, 0.3 conf → 30% weight |
| `test_build_network_no_confidence_defaults_to_one` | `None` confidence → treated as 1.0 (backward compat) |
| `test_co_citation_ignores_low_confidence_citers` | Citers with conf < 0.7 don't generate co-citation edges |
| `test_co_citation_uses_high_confidence_citers` | Citers with conf ≥ 0.95 DO generate co-citation edges |
| `test_co_citation_skips_pairs_with_direct_high_confidence_edge` | Direct LSP edge suppresses redundant co-citation |
| `test_co_citation_weight_scales_by_confidence` | Mixed-confidence co-citation weighted less than all-1.0 co-citation |

## Verification

- 65 clustering tests pass (59 existing + 6 new)
- 602 total project tests pass
- No regressions — existing tests have no explicit confidence on edges, so they default to 1.0 and behave identically to before

## Design Decisions

- **Threshold 0.7 for citers:** Generous enough to include "pretty confident" tree-sitter results but excludes wild guesses. Could be made configurable via `InferConfig` in the future (see ISS-002 for configurable edge weights).
- **Threshold 0.9 for suppression:** Higher bar because suppressing co-citation removes signal — only do it when we're very sure the direct edge is ground truth.
- **Geometric mean for confidence scaling:** `sqrt(conf_a × conf_b)` penalizes asymmetric confidence (one strong + one weak citer) more than arithmetic mean. If A→target is 1.0 but B→target is 0.7, the shared citation strength is 0.837, not 0.85.
- **`unwrap_or(1.0)`:** Legacy edges without confidence are assumed reliable. This preserves existing behavior for all pre-ISS-012 graphs.

## Files Changed

- `crates/gid-core/src/infer/clustering.rs` — +370 lines (implementation + 6 tests)
