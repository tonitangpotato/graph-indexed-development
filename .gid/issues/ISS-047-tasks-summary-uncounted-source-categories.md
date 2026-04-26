# ISS-047: `gid tasks` summary still misleading — `manual` and `infer` source nodes are uncounted (regression of ISS-034 fix)

**Status:** open
**Priority:** P2 (UX correctness; same trust impact as ISS-034 — agents and humans see "0 nodes" in a populated graph)
**Component:** `crates/gid-core/src/graph.rs` (`summary()`, `summary_text()`, Display impl)
**Filed:** 2026-04-26
**Discovered by:** RustClaw (after adding ISS-042..046 task nodes via `gid_add_task` to gid-rs's own graph)
**Related:** ISS-034 (closed 2026-04-25 — original "summary ignores node_type filter" fix)

---

## Symptom

After adding 5 manually-created issue nodes (ISS-042..046) to gid-rs's `.gid/graph.db`, `gid_tasks` displays:

```
📊 Graph: 0 nodes, 11966 edges
  todo=0 in_progress=0 done=0 blocked=0 cancelled=0
  ready=0

  [todo] infer:component:0 (Project Root Structure) ...
  [todo] iss-042-clippy-warnings-cleanup ...
  ... (33 more nodes)
```

Header claims **0 nodes**. Body lists **33 nodes** (28 infer + 5 manual). Same contradiction ISS-034 was supposed to fix.

## Root Cause

ISS-034's fix (2026-04-25) made `GraphSummary` carry both `total_nodes` (project) and `code_nodes` separately, with the Display impl rendering both when `code_nodes > 0`. The fix correctly handles the project/code dichotomy.

But the codebase has **at least 4 distinct `source` values** in practice, not 2:

```sql
sqlite> SELECT source, COUNT(*) FROM nodes GROUP BY source;
extract|2705   ← code layer (counted by code_nodes ✓)
infer|28      ← inferred component/feature nodes (uncounted ✗)
manual|5      ← issues / tasks added via gid_add_task with source="manual" (uncounted ✗)
              ← (NULL / "project" — counted by total_nodes if present)
```

`Graph::project_nodes()` filter is something like `source.is_none() || source == "project"`. `Graph::code_nodes()` is `source == "extract"`. Anything else (`infer`, `manual`, etc.) is silently dropped from both counts — but still listed in the body of `gid tasks`.

This is a **classification gap**, not a filter logic bug. ISS-034 fixed the symmetry between project and code populations but didn't generalize to "count every node, by source".

## Secondary Bug: `gid_add_task` Default Source Categorization

When `gid_add_task` is called with `source="manual"` (a documented option), the resulting node falls outside both the project-layer and code-layer buckets. This means **manually-added issues vanish from the summary**, which is the opposite of what a user wants — manual tasks are precisely the ones we care most about tracking.

Two possible fixes for this secondary issue:
- (a) `gid_add_task` should default `source` to `"project"` (or `None`) when the node_type is `task/feature/component/issue`, so they land in `project_nodes()`. `source="manual"` becomes redundant for tasks.
- (b) `Graph::project_nodes()` should treat `source IN ("project", "manual", NULL)` as project layer.

Either works. (b) is more permissive and backward-compatible. (a) is cleaner semantically but requires updating callers that explicitly pass `source="manual"`.

## Impact

- **Same as ISS-034:** misleading "0 nodes" header trains agents to think the graph is empty and re-run `gid extract` or assume the database is broken.
- **Worse than ISS-034:** the ISS-034 reproduction needed a special v03-context graph. This one happens on **any project that uses `gid_add_task` with `source="manual"`** — i.e., any project where issues were filed via rustclaw tools.
- **Specifically misleading right now:** gid-rs's own graph shows "0 nodes" but contains 5 active issues + 28 inferred components.

## Fix

### Phase 1 — Generalize summary counts (root fix)

Rename `GraphSummary` fields and update Display:

```rust
pub struct GraphSummary {
    pub project_nodes: usize,   // was total_nodes
    pub code_nodes: usize,      // unchanged (source = "extract")
    pub inferred_nodes: usize,  // NEW — source = "infer"
    pub other_nodes: usize,     // NEW — anything else (manual, design, etc.)
    pub total_edges: usize,
    // ... task status counts unchanged
}
```

Display logic:
```
📊 Graph: 5 project nodes, 2705 code nodes, 28 inferred nodes, 11966 edges
  Tasks: todo=5 in_progress=0 done=0 blocked=0 cancelled=0
  Ready: 5
```

Show only the buckets with `count > 0`. Bucket the long tail (anything not project/code/infer) into `other_nodes` with a count, optionally listing distinct source values in verbose mode.

### Phase 2 — Fix `gid_add_task` source default (secondary)

Pick (a) or (b) from above. Recommend (b) because it doesn't require coordinating with callers and matches user mental model ("manually filed = project task").

### Phase 3 — Update tests

ISS-034 added 5 tests. Add 2 more:
- `test_iss047_summary_reports_inferred_nodes_separately`
- `test_iss047_summary_includes_manual_source_nodes_in_project_count` (or equivalent depending on fix choice)

Update `test_iss034_summary_display_omits_code_nodes_when_zero` to also assert inferred=0 case stays clean.

## Verification

```bash
# After fix, this graph should show non-zero project nodes:
cd /Users/potato/clawd/projects/gid-rs
gid tasks
# Expected: "5 project nodes, 2705 code nodes, 28 inferred nodes, ..."
```

- `cargo test --workspace -- iss034 iss047` → all green
- `gid tasks` on a project-only graph → unchanged backward-compat output

## Notes

ISS-034 was closed correctly *for the population it considered* (project vs code). This issue is not a bug in that fix — it's an incomplete generalization. The codebase has organically grown to 4+ source values; the summary infrastructure needs to keep up.

The fact that this regression surfaced **immediately** after using `gid_add_task` (rustclaw's preferred way to file issues) suggests: every tool that creates nodes should be tested against `gid tasks` summary output as part of acceptance.
