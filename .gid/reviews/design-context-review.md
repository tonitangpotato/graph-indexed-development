# Review: design-context.md

**Reviewed:** 2026-04-06
**Document:** GID SQLite Migration — Context Assembly
**GOALs covered:** 4.1–4.13

---

## 🔴 Critical (blocks implementation)

### FINDING-1 [Check #1] ContextQuery doesn't match requirements input model
The design's `ContextQuery` has a single `task_id: NodeId` as the focal point. But requirements GOAL-4.1 and GOAL-4.6 specify `--targets <node_id>[,<node_id>...]` — **multiple** target nodes. The entire pipeline is built around single-root BFS, which fundamentally doesn't support multi-target context assembly.

**Impact:** With single-root BFS, if you want context for modifying both `auth.rs` and `login.rs`, you'd need to run the pipeline twice and manually merge results (with budget double-counting). The requirements expect a single call with multiple targets.

**Suggested fix:** Change `task_id: NodeId` to `targets: Vec<NodeId>`. Modify `gather_candidates` to start BFS from all targets simultaneously (multi-source BFS). Adjust scoring so `hop_distance` is the minimum distance from any target.

### FINDING-2 [Check #1] Design types don't align with requirements output
The design's `ContextItem` has `{node_id, kind, body: Vec<u8>, token_estimate, score, truncated}`. But requirements GOAL-4.1 specifies the output should contain: (a) full target details (title, file_path, signature, doc_comment, description), (b) **source code read from disk** at file_path between start_line/end_line, (c) direct deps, (d) transitive deps, (e) callers, (f) related tests. The design treats everything as homogeneous `ContextItem`s with a byte blob body — there's no distinction between targets and deps, no source code reading from disk, no caller/test categorization.

**Suggested fix:** The output structure needs categories:
```rust
struct ContextResult {
    targets: Vec<TargetContext>,    // full details + source code
    dependencies: Vec<DepContext>,  // ranked by relevance
    callers: Vec<CallerContext>,    // callers of targets
    tests: Vec<TestContext>,        // related test nodes
    truncation_info: TruncationInfo,
}
```
Add a source code reading step (read file at `file_path`, extract `start_line..end_line`).

### FINDING-3 [Check #6] Truncation priority order wrong
The design's `budget_fit` (§6) uses a simple greedy knapsack — ranked items consumed in score order until budget exhausted. But requirements GOAL-4.3 specifies a specific priority order: "transitive dependencies truncated first (furthest hops first), then callers, then direct dependencies. Target node details are **never truncated**."

The design doesn't distinguish these categories at all. A high-scoring transitive dep could survive while a direct dep gets dropped. Targets could be truncated (no protection). This violates the requirements.

**Suggested fix:** Implement category-based budget allocation:
1. Reserve budget for all targets (never truncated)
2. Fill direct deps
3. Fill callers
4. Fill transitive deps (furthest hops first to drop)
Within each category, use relevance scoring for ordering.

---

## 🟡 Important (should fix before implementation)

### FINDING-4 [Check #6] Relevance scoring doesn't match requirements ranking
Requirements GOAL-4.4 defines a specific 5-tier ranking based on edge relation type:
1. Direct call (calls, imports)
2. Type reference (type_reference, inherits, implements, uses)
3. Same-file (contains, defined_in with shared file_path)
4. Structural (depends_on, part_of, blocks, tests_for)
5. Transitive (any relation at hop > 1)

The design's scoring (§5) uses `W_PROXIMITY * proximity + W_FRESHNESS * freshness + W_KIND * kind_score` — which is based on node kind (task/code_file/doc), NOT edge relation type. The `kind_score` checks `candidate.kind` not the edge that connected this node. A `calls` edge and a `depends_on` edge would get the same score if they're the same hop distance.

**Suggested fix:** Replace `W_KIND * kind_score` with edge-relation-based ranking that matches the 5-tier model. Each candidate should carry the edge relation that connected it to the context.

### FINDING-5 [Check #6] No caller/test discovery logic
Requirements GOAL-4.1 includes "callers of target nodes" and "related test nodes" in the output. The design's BFS in §4 only follows **outgoing** edges (`get_edges_from`). Callers would be nodes with **incoming** edges to the targets. Tests are typically connected via `tests_for` edges pointing TO the target.

**Suggested fix:** Add reverse-edge traversal: `get_edges_to(&target_id)` to discover callers and tests. Filter by edge relation (`calls`/`imports` for callers, `tests_for` for tests).

### FINDING-6 [Check #1] Missing `--include` filter support
Requirements GOAL-4.8 specifies `--include <pattern>` for file path glob and node type filtering (e.g., `--include "*.rs"`, `--include "type:function"`). The design's `ContextFilters` has `node_kinds` and `exclude_ids` but no glob-based file path filter or the `type:` prefix syntax.

**Suggested fix:** Add `include_patterns: Vec<String>` to `ContextFilters`. In `passes_filters`, parse `type:X` patterns for node_type matching and treat others as file path globs.

### FINDING-7 [Check #1] Missing `--format` output support
Requirements GOAL-4.9 specifies `--format <json|yaml|markdown>` output formats. The design has no output formatting — `ContextResult` is returned as a Rust struct with no serialization or formatting logic specified.

**Suggested fix:** Add an output formatting section. At minimum, derive `Serialize` on all output types for JSON/YAML. Add a markdown formatter function.

### FINDING-8 [Check #6] `body: Vec<u8>` instead of structured source code
The design stores node content as raw bytes (`body: Vec<u8>`) throughout the pipeline. But requirements GOAL-4.1(b) says source code should be "read from disk at `file_path` between `start_line` and `end_line`". The design never reads from disk — it reads `node.body` from the graph storage, which may not contain source code at all (nodes in gid-core store metadata, not full source code).

**Suggested fix:** Add a source code reading step in the pipeline. After gathering candidates, for nodes with `file_path`/`start_line`/`end_line`, read the actual source file and extract the relevant lines. Fall back to node description/signature if file is missing.

### FINDING-9 [Check #14] Relevance score visibility (GOAL-4.5)
Requirements GOAL-4.5 says "The relevance score for each included node is visible in the output." The design's `ContextItem` does include `score: f64`, so this is partially addressed. But since the scoring formula doesn't match requirements (FINDING-4), the visible score would be meaningless to the consumer.

**Suggested fix:** After fixing the scoring formula (FINDING-4), ensure the score in output reflects the requirements' ranking model.

### FINDING-10 [Check #19] Observability (GOAL-4.13) not specified
Requirements GOAL-4.13 says `gid context` should log: nodes visited, nodes included, nodes excluded by filter, token budget used vs. available, and elapsed time. The design mentions none of this — no tracing calls, no stats collection.

**Suggested fix:** Add `tracing::info!` calls at key pipeline stages. Return traversal stats as part of `ContextResult` or a separate `ContextStats` struct.

---

## 🟢 Minor (can fix during implementation)

### FINDING-11 [Check #8] UTF-8 safety in truncation
§7 `truncate_body` operates on `&[u8]` and splits at `b'\n'` boundaries. If the body is valid UTF-8 (which source code should be), splitting at newline is UTF-8 safe. But the function doesn't validate UTF-8 before operating. If the body contains multi-byte UTF-8 sequences without newlines within `usable_bytes`, the `rposition` fallback could split mid-character.

**Suggested fix:** After computing `cut_point`, verify it's at a char boundary: `std::str::from_utf8(&body[..cut_point]).is_ok()`. If not, scan backward to find a valid boundary.

### FINDING-12 [Check #15] Hardcoded scoring weights
§5 hardcodes `W_PROXIMITY = 0.50`, `W_FRESHNESS = 0.30`, `W_KIND = 0.20`. Requirements don't mandate configurability, but these are the kind of values that need tuning.

**Suggested fix:** Accept as constants for v1 but document them as tunable. Consider accepting them in `ContextQuery` as optional overrides.

### FINDING-13 [Check #10] `partial_cmp` unwrap in rank_candidates
§5 `rank_candidates` uses `b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)`. This is fine for non-NaN floats, but the scoring formula could theoretically produce NaN if inputs are wrong (e.g., negative age causing `exp(positive_large)` overflow). The `unwrap_or(Equal)` silently hides NaN scores.

**Suggested fix:** Add a NaN guard in `score_candidate`: `if score.is_nan() { score = 0.0; }`.

### FINDING-14 [Check #4] `NodeId` vs `String` inconsistency
Same as in the history review — `NodeId` is used but not defined. The master design uses `String` for node IDs.

**Suggested fix:** Use `String` or define the type alias.

---

## ✅ Passed Checks

- Check #2: References resolve ✅ (§N refs exist, GOAL refs valid)
- Check #3: No dead definitions ✅ (all types used in pipeline)
- Check #5: No state machine (linear pipeline) ✅
- Check #9: No integer overflow risk ✅ (usize arithmetic is bounded)
- Check #12: Pipeline order is fixed, no guard ordering ✅
- Check #13: Separation of concerns ✅ (gather/score/rank/budget are distinct)
- Check #16: API surface minimal ✅ (one public function + query/result types)
- Check #17: Goals explicit ✅ (GOAL traceability in §12)
- Check #20: Appropriate abstraction ✅
- Check #24: No migration needed ✅ (new feature)
- Check #25: Testability ✅ (pure functions, inject storage via trait)
- Check #26: No existing context code ✅
- Check #27: New API ✅
- Check #28: Behind `sqlite` feature ✅

---

## Summary

- **Critical: 3** (single-target vs multi-target, output structure mismatch, truncation priority order)
- **Important: 7** (scoring model wrong, no caller/test discovery, missing --include, missing --format, no disk source reading, score visibility, no observability)
- **Minor: 4** (UTF-8 edge case, hardcoded weights, NaN guard, NodeId type)
- **Recommendation:** ❌ **Needs major revision** — the design was written as a generic "graph context assembly" algorithm, but the requirements specify a much more specific tool: multi-target, categorized output (deps/callers/tests), edge-relation-based ranking, and source code reading from disk. The algorithm structure (gather→score→rank→budget) is sound, but the details need to match the actual `gid context` command requirements.
- **Estimated implementation confidence:** Low — the pipeline architecture is good but most details need rework.

---
## ✅ All Findings Applied (2026-04-06 19:56)

All 14 findings applied to design-context.md:
- FINDING-1 ✅ Multi-target support: targets: Vec<String> replaces task_id: NodeId, multi-source BFS in §4.2
- FINDING-2 ✅ Categorized output: ContextResult has targets/dependencies/callers/tests + TargetContext with source code
- FINDING-3 ✅ Category-based truncation: targets never truncated, direct deps → callers → tests → transitive deps (furthest first)
- FINDING-4 ✅ Edge-relation-based 5-tier scoring (calls/imports=1.0, type_ref=0.8, same-file=0.6, structural=0.4, transitive=0.2)
- FINDING-5 ✅ Reverse-edge traversal added (§4.3) for caller and test discovery via incoming edges
- FINDING-6 ✅ --include filter support: include_patterns on ContextFilters, file glob + type: prefix parsing
- FINDING-7 ✅ --format support: OutputFormat enum (Markdown/Json/Yaml)
- FINDING-8 ✅ Source code reading from disk: read_source_code() in §4.1 reads file_path between start_line/end_line
- FINDING-9 ✅ Score visible in ContextItem.score (meaningful after scoring fix)
- FINDING-10 ✅ Observability: ContextStats struct + tracing::info! in pipeline with visited/included/excluded/budget
- FINDING-11 ✅ UTF-8 safety: is_char_boundary() check + backward scan in truncate_text
- FINDING-12 ✅ Weights documented as v1 constants, tunable for future versions
- FINDING-13 ✅ NaN guard: if score.is_nan() { score = 0.0; }
- FINDING-14 ✅ NodeId → String throughout (matching master design)

