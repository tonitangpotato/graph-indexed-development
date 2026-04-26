---
id: "ISS-035"
title: "Low-Confidence Tree-Sitter Edges Pollute Impact/Caller Queries by Default"
status: closed
priority: P2
created: 2026-04-26
closed: 2026-04-25
related: ["ISS-002", "ISS-016", "ISS-012"]
---
# ISS-035: Low-Confidence Tree-Sitter Edges Pollute Impact/Caller Queries by Default

**Status:** closed
**Severity:** important (affects correctness of impact analysis for all agents using code graph)
**Reported:** 2026-04-24
**Closed:** 2026-04-25
**Reporter:** potato + RustClaw
**Related:** ISS-002 (tree-sitter false positives), ISS-016 (LSP pass1 dangling edges), ISS-012 (confidence-weighted edges)
**Resolution:** Option D (hybrid) implemented. `DEFAULT_MIN_CONFIDENCE = 0.8` constant added in `gid-core::query`. `--min-confidence` flag added to `gid query impact`, `gid query deps`, `gid code-impact`. Hidden edge counts surfaced in both human and JSON output. Public APIs (`impact_with_filters`, `deps_with_filters`, `analyze_impact_with_filters`) added; legacy entry points preserved. Backward compatible (legacy `analyze_impact_filtered` calls `analyze_impact_with_filters(..., None)` = no filtering, same as before). Regression tests in `query.rs::tests::test_iss035_*`.

## Summary

When LSP symbol resolution is incomplete (the common case — LSP only covers symbols it fully resolves), gid falls back to tree-sitter name-matching to create `calls` edges. These fallback edges get `confidence=0.6` and are **indistinguishable from high-confidence edges in default query output**.

For common Rust method names (`.contains()`, `.clone()`, `.to_string()`, `.push()`, `.is_empty()`, `.len()`, `.iter()`, etc.), this produces **massive false positive pollution**: every call to `some_vec.contains(&x)` gets attributed as a caller of `YourType.contains()` if `YourType` happens to define a `contains` method.

**Impact:** Any agent using `gid_query_impact` or caller queries to reason about blast radius will get wildly wrong answers unless it knows to filter `confidence >= 0.8` — which is undocumented in the query tool descriptions and not the default.

## Concrete Example

While analyzing rename blast radius for `SessionWorkingMemory` in engram v0.3 design work, a caller query returned:

- `parse_soul` — obviously unrelated
- `tokenize_cjk_boundaries` — obviously unrelated
- `deserialize_flexible_string` — obviously unrelated
- ...plus many more

Root cause: all of these functions call `.contains()` on some `Vec` or `HashSet`. Tree-sitter name-matching attributed every `.contains()` call to `SessionWorkingMemory::contains`, producing ~30+ false positive edges all with `confidence=0.6`.

After filtering `confidence >= 0.9`, the result was precise: only 3 files, 22 references, exactly matching manual grep — and showed the rename was actually low cost (r1 review had estimated "~20 call sites" — real was 3 files).

Without the confidence filter, an agent would conclude the rename is massively expensive and abandon the refactor based on garbage data.

## Why This Is Systemic

Rust's trait/method system makes this failure mode **structural, not occasional**:

- `Vec<T>::contains`, `HashSet::contains`, `HashMap::contains_key`, `str::contains`, `Option::contains` — all exist
- `.clone()` is on literally everything
- `.to_string()`, `.into()`, `.as_ref()`, `.as_str()` — ubiquitous
- Any user type defining these common method names will absorb all unrelated call sites into its caller set

This isn't a bug in tree-sitter or in gid's parser — it's a fundamental limitation of name-only matching without type context. LSP solves it, but LSP coverage is incomplete (ISS-016), so tree-sitter fallback is necessary. The problem is the **default presentation** of mixed-confidence results.

## Options

### Option A: Filter `confidence >= 0.8` by default in query output

- `gid_query_impact`, caller queries, `gid_working_memory` all default to high-confidence edges
- Add explicit flag `--include-low-confidence` or `--min-confidence 0.0` for users who want everything
- Pros: correct-by-default, matches what agents actually want
- Cons: hides potentially-useful fuzzy matches; users who don't know the flag exists may miss real edges where LSP failed

### Option B: Blocklist common method names for tree-sitter name-match

- Maintain a Rust-specific (and per-language) blocklist: `contains`, `clone`, `to_string`, `push`, `is_empty`, `len`, `iter`, `into`, `as_ref`, `as_str`, `new`, `default`, etc.
- When tree-sitter encounters these names, don't create fallback edges at all — require LSP
- Pros: eliminates the noise at the source; default query behavior stays unchanged
- Cons: blocklist maintenance burden; may miss legitimate edges for methods that genuinely have unique names per type; per-language lists needed

### Option C: Visible warning in query output

- Query tool output explicitly tags low-confidence edges: `[low-conf]` prefix, or a warning header: "N of M results are tree-sitter-only (confidence=0.6), may contain false positives. Use --min-confidence 0.8 to filter."
- Pros: cheapest to implement; preserves all information; educates users
- Cons: agents may still ignore the warning; doesn't fix default behavior

### Option D (recommended): Hybrid A + C

- Default output filters to `confidence >= 0.8`
- Summary line: `"Showing N high-confidence edges. M low-confidence edges hidden (use --min-confidence 0.0 to include)."`
- Always-visible count ensures users know when fallback data exists
- Explicit flag to opt into low-confidence results
- Combines correctness-by-default with transparency

## Acceptance Criteria

- `gid_query_impact` / caller queries default to `confidence >= 0.8` (or configurable threshold)
- Hidden low-confidence edge count shown in summary
- `--min-confidence N` flag documented in tool descriptions
- Existing tests updated; new test verifies default filters out a known `confidence=0.6` edge
- Changelog notes this as a behavior change (not just a feature) — existing scripts relying on all-edges need to pass `--min-confidence 0.0`

## Non-Goals

- Fixing LSP coverage itself — that's ISS-016
- Improving tree-sitter accuracy — that's fundamentally limited without type info
- Per-language blocklists (Option B) — can be a follow-up if Option A+C proves insufficient

## Priority

**Important, not critical.** Code graph is still useful with manual filtering, and we now have a procedural note (in engram's `06-session-wm-rename-blast-radius.md`) that documents the filter requirement. But every agent that doesn't know this will produce wrong impact analyses silently — which is worse than a loud error. Should fix before gid is used more broadly for automated refactor planning.

## References

- Discovered during: engram v0.3 design working memory session, `docs/v0.3-working-memory/06-session-wm-rename-blast-radius.md`
- ISS-002: original tree-sitter false positive discussion
- ISS-016: LSP pass1 dangling edges (why fallback is needed)
- ISS-012: confidence-weighted edges (the infrastructure this would leverage)
