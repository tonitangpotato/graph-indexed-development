# ISS-034: `gid tasks` Summary Ignores `--node-type` Filter

**Status:** closed
**Severity:** minor (cosmetic — misleading output, no logic impact)
**Reported:** 2026-04-24
**Closed:** 2026-04-25
**Reporter:** potato (via rustclaw session — engram v0.3 context-assembly work)

## Resolution (Option B from original analysis)

`GraphSummary` now carries a `code_nodes: usize` field alongside `total_nodes` (project-layer count, unchanged for backward compat). The `Display` impl renders `"N project nodes, M code nodes, K edges …"` whenever `code_nodes > 0`, and falls back to the legacy `"N nodes, K edges …"` format when there are no code nodes (preserves existing behavior for project-only graphs).

`summary_text()` mirrors the Display behavior. `summary()` populates both counts in one pass.

CLI JSON output for `gid tasks --json` now includes a `summary.code_nodes` field.

**Why Option B (always show both) over Option A (filter summary by layer):**
Project-layer and code-layer are two distinct populations by design (`Graph::project_nodes()` / `Graph::code_nodes()`). Hiding one based on a filter flag pretends they're interchangeable. Surfacing both is honest and the cost is one extra header line.

## Tests Added (5)

- `test_iss034_summary_reports_code_nodes_separately` — both counts populated, status counts still come from project nodes only
- `test_iss034_summary_display_includes_code_nodes_when_present` — header mentions both populations
- `test_iss034_summary_display_omits_code_nodes_when_zero` — backward compat for project-only graphs
- `test_iss034_summary_text_includes_code_nodes` — `summary_text()` mirrors Display
- `test_iss034_code_only_graph_does_not_lie_about_zero_nodes` — reproduces original report scenario (v03-style db with only code nodes)

695 gid-core lib tests pass (was 690 before this fix, +5 new). 2 gid-dev-cli tests pass.

---

## Original Report

`gid tasks --node-type all` (and `--node-type code`) displays a summary header that claims `0 nodes` while simultaneously listing dozens of nodes in the body. The header and the body disagree because the summary counts are computed from `project_nodes()` only, regardless of the `node_type` filter the user requested.

## Observed Output

```
$ gid tasks --node-type all --project /Users/potato/clawd/projects/engram/.gid-v03-context

📊 Graph: 0 nodes, 8020 edges
  todo=0 in_progress=0 done=0 blocked=0 cancelled=0
  ready=0

  [done] class:examples/iss019_smoke_pilot.rs:Args (Args)
  [done] class:examples/iss019_smoke_pilot.rs:D...
  ... (dozens more code nodes follow)
```

The graph actually contains ~3056 code nodes (source=`extract`) plus 8020 edges. The summary's "0 nodes" is technically correct for `project_nodes()` (nodes with `source == "project"` or legacy None), but:

1. User asked for `--node-type all` → they expect the summary to reflect all nodes
2. The body then lists code nodes (which is consistent with `all`) → creating a contradiction between header and body
3. Edge count (`8020 edges`) is not filtered the same way → further inconsistency

## Root Cause

In `crates/gid-core/src/graph.rs`, the summary builder hardcodes project-node counts instead of responding to the requested filter:

- `Graph::summary_text()` (≈ line 833) builds `"Graph: {} nodes, {} edges"` using `GraphStats { total_nodes, total_edges }` — but `total_nodes` appears to be computed from `project_nodes()` only (0 in this case), while `total_edges` counts all edges (8020).
- The list rendering in `tasks` command does not apply the same project-only filter, so it prints code nodes when `--node-type all` is passed.

The asymmetry between "summary counts project nodes only" and "body lists whatever the filter says" is the real bug.

## Reproduction

```bash
# On any project with extracted code nodes in graph.db:
cd /Users/potato/clawd/projects/engram/.gid-v03-context
gid tasks --node-type all
# Header shows "0 nodes" but body lists thousands of code nodes
```

Alternatively via rustclaw's `gid_tasks` tool with `node_type: "all"`.

## Impact

- **Correctness of logic:** none — no code path depends on these summary numbers for decisions.
- **User trust / UX:** moderate — the misleading header causes agents and humans to second-guess whether the graph actually has data. In this session it caused me to re-verify via `sqlite3` that the graph.db was populated (it was).
- **Agent reasoning:** mild — LLMs reading the CLI output may infer "graph is empty" and take wrong next steps (e.g., re-running `gid extract`).

## Fix Options

### Option A: Summary respects `--node-type` filter
- When `--node-type all` → count all nodes (project + code + other)
- When `--node-type code` → count code nodes only
- When default (project view) → current behavior (count project_nodes)

Pros: summary matches body, no surprise.
Cons: "ready=N / todo=N" counters only make sense for task-type nodes — would need to either hide or zero them out when node_type includes non-tasks.

### Option B: Always show both counts in header
Replace the current single line with:
```
📊 Graph: 15 project nodes, 3056 code nodes, 8020 edges
  Tasks: todo=3 in_progress=1 done=11 blocked=0 cancelled=0
  Ready: 2
```

Pros: always transparent, never misleading regardless of filter. Matches the conceptual reality that project-layer and code-layer are two distinct populations.
Cons: slightly more verbose by default.

**Recommendation: Option B.** It's the root fix — project nodes and code nodes are two separate populations by design (see `Graph::project_nodes()` / `Graph::code_nodes()` in `graph.rs`). The summary should reflect that reality instead of hiding one or the other based on a flag.

## Related Code

- `crates/gid-core/src/graph.rs:740-750` — `project_nodes()` / `code_nodes()` split by `source` field
- `crates/gid-core/src/graph.rs:833` — summary line construction
- CLI command handler for `tasks` subcommand — needs to confirm where list filtering diverges from summary counting

## Acceptance

- `gid tasks` (default) — output identical to current (backward compatible)
- `gid tasks --node-type all` — header shows combined counts; body lists all nodes; no contradiction
- `gid tasks --node-type code` — header clearly labels code-node counts; body lists code nodes
- Unit test covers all three invocations against a graph with both project and code nodes

## Priority

**P2.** Cosmetic / UX. Fix alongside any other `tasks` command work. Not blocking anything.
