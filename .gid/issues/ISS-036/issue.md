---
id: "ISS-036"
title: "design-to-graph Pipeline Incomplete — Missing Skill, Schema Gaps"
status: closed
priority: P2
created: 2026-04-26
related: ["ISS-031"]
---
# ISS-036: design-to-graph Pipeline Incomplete — Missing Skill, Schema Gaps

**Status:** closed (duplicate of ISS-039)
**Severity:** important (blocks ritual v2 Graphing phase from running end-to-end; blocks design-doc-to-graph automation for any non-trivial project)
**Reported:** 2026-04-24
**Reporter:** potato + RustClaw
**Related:** ritual v2 state machine (`Graphing` phase), `gid design --parse --merge`, ISS-031 (ritual graph phase pollutes existing tasks)

## Summary

The ritual v2 state machine declares a `Graphing` phase that should call a `draft-graph` / `update-graph` skill to convert `design.md` into graph nodes (features, components, planned code, tasks, edges). The skill **does not exist**, and the existing `generate_scoped_graph_prompt` only emits feature + task two-tier structure with no support for component middle-layer or planned-code-node placeholders.

**Symptom encountered (2026-04-24):** While building task graph for engram v0.3 (5 design docs, 43 components, ~198 subtasks, modifies/implements references to ~76 source files), there was no clean GID path from design.md → graph. Workaround: hand-write LLM prompt, pipe yaml through `gid design --parse --merge` — works because `parse_llm_response` is schema-agnostic, but the surrounding skill / prompt / agent orchestration is all DIY per-project.

## Current State

What exists today:

| Component | Status | Location |
|---|---|---|
| Ritual v2 `Graphing` phase | ✅ declared | `crates/gid-core/src/ritual/state_machine.rs:33` |
| Composer references `update-graph` | ✅ | `crates/gid-core/src/ritual/composer.rs:155` |
| v2 executor handles `generate-graph \| design-to-graph` | ✅ | `crates/gid-core/src/ritual/v2_executor.rs:686` |
| `gid design --parse --merge` (yaml stdin → graph merge) | ✅ schema-agnostic | `crates/gid-cli/src/main.rs:2593` |
| `parse_llm_response` (yaml → Graph) | ✅ schema-agnostic, accepts any node_type | `crates/gid-core/src/design.rs:281` |
| `generate_scoped_graph_prompt` | ⚠️ exists but limited | `crates/gid-core/src/design.rs:193` |
| `build_graph_from_proposals` (knows component) | ✅ exists, unused by prompt | `crates/gid-core/src/design.rs:291` |
| `draft-graph` / `update-graph` skill | ❌ **does not exist** | (no `SKILL.md` anywhere) |

What's broken / missing:

1. **No skill file.** Ritual v2's `Graphing` phase has nothing to dispatch to. `state_machine.rs` references skill names that have no implementation. Any agent (including RustClaw) running ritual v2 will fail or no-op at this phase.

2. **`generate_scoped_graph_prompt` only emits feature + task two-tier.** No instruction for LLM to produce:
   - **component** middle-layer nodes (between feature and tasks — needed when a feature decomposes into 5–15 components, each with multiple subtasks)
   - **planned code nodes** (`node_kind=planned`) — design docs declare files/types that **don't exist yet**; tasks need to reference them via `implements` edges; current schema has no concept for "node that will exist post-implementation"
   - **modifies vs implements distinction** — task touching existing `class:src/foo.rs:Bar` (already extracted) vs task creating new `class:src/graph/entity.rs:Entity` (planned) require different edge semantics. Current prompt doesn't differentiate.

3. **No protocol for "task references existing extracted code node."** LLM doesn't know what code nodes already exist in the graph, so it can't emit edges like `task → modifies → class:src/memory.rs:Memory` reliably. Need to inject a list of existing code node IDs into the prompt context.

4. **No reconciliation pass post-implementation.** When implementation finishes and `gid extract` is re-run, planned code nodes need to be replaced/merged with real extracted nodes. No tooling for this.

## Concrete Use Case (engram v0.3, 2026-04-24)

Project structure:
- 1 master design.md
- 5 feature design docs (`v03-graph-layer`, `v03-resolution`, `v03-retrieval`, `v03-migration`, `v03-benchmarks`)
- ~43 components across the 5 features
- ~198 subtasks
- Touches ~76 existing source files (extracted as code nodes in `.gid-v03-context/.gid/graph.db`)
- Designs new types: `Entity`, `Relation`, `EntityResolver`, `GraphRetriever`, ~20 new files

What we wanted to do: `gid ritual` would drive design → graph automatically. Each of the 5 features produces a yaml with `feature → component → subtask` nodes, plus `subtask → modifies → existing_code_node` edges and `subtask → implements → planned_code_node` edges. All merged into the v0.3 working graph.

What we had to do: hand-write the prompt, manually inject existing code node ids as context, spawn 5 sub-agents to produce yaml, manually pipe each through `gid design --parse --merge --scope=<feature>`. Every project-specific decision (three-tier vs two-tier, planned-node convention, edge semantics) reinvented from scratch.

## Why This Is Systemic

Every non-trivial project hitting ritual v2 will encounter this gap. The current pipeline assumes:
- Two-tier feature + task structure (insufficient for >20-task features)
- All code nodes already exist (insufficient when design declares new files/types)
- Edge target resolution is trivial (insufficient when target IDs aren't visible to the LLM)

The `Graphing` phase being a placeholder makes ritual v2 **non-functional for real projects** at this step — it works for tiny demos where two-tier is enough, fails for anything with structure.

## Proposed Fix

### Phase 1: Schema (in-tree, no skill yet)

1. **Add `node_kind=planned` to graph schema** — convention only, no enum change needed (graph already accepts arbitrary node_kind strings). Document it.
2. **Add planned→real reconciliation in `gid extract`**:
   - When extract creates a new code node, check for existing `node_kind=planned` node with same id → merge edges, mark planned node deleted (or replace in-place)
   - Alternative: emit a `gid reconcile-planned` command for explicit invocation
3. **Extend `generate_scoped_graph_prompt` to support 3-tier**:
   - Accept optional `feature_structure: { tiers: ["feature", "component", "task"] }` parameter
   - Inject existing code node ids list (relevant ones, not all — filter by feature scope from design doc)
   - Document `modifies` vs `implements` edge semantics in the prompt
   - Document planned-node convention

### Phase 2: Skill

4. **Write `draft-graph` skill** (`packages/skills/draft-graph/SKILL.md`):
   - Inputs: design.md path, feature scope id, existing graph context (code nodes within scope)
   - Process: read design → generate scoped prompt → LLM call → parse yaml → `gid design --parse --merge`
   - Output: graph mutations + summary
5. **Write `update-graph` skill** (delta-update version):
   - Inputs: design.md (modified), feature scope id, existing tasks under that feature
   - Process: identify additions/removals/changes vs current graph → propose delta yaml → human review → merge
   - Critical for ISS-031 (avoid pollution when re-running graph phase)

### Phase 3: Ritual v2 wiring

6. Wire `Graphing` phase in state machine to dispatch `draft-graph` (first time) or `update-graph` (delta) based on whether feature already has tasks.
7. Compose phases: `Graphing` runs once per feature scope (loop in v2_executor) — current code seems to assume single graph phase; verify.

## Acceptance Criteria

- A project with feature → component → subtask structure can be generated from design.md via ritual v2 with **no manual prompt engineering**
- Tasks can reference both extracted code nodes (via `modifies`) and planned code nodes (via `implements`) automatically
- Re-running `Graphing` after design.md edit produces a clean delta merge, not duplicate/orphan nodes (interaction with ISS-031)
- `gid extract` post-implementation reconciles planned nodes with real ones

## Out of Scope (this issue)

- Component-level dependency inference (could be a follow-up)
- Auto-generation of `satisfies → GOAL-X.Y` edges (currently in metadata; could become real edges later)
- Cross-feature reasoning (the prompt is per-feature scoped; a "global graph reviewer" pass is a separate concern)

## Notes

- `parse_llm_response` and `gid design --parse --merge` are already schema-agnostic — the merge plumbing works. Gap is purely in the **prompt + skill + reconciliation** layer.
- Workaround for engram v0.3 (this project): hand-write prompt + run 5 sub-agents + manual merge. Documented for reference; should not be required once this issue is resolved.
- The yaml format used by `gid design --parse` is a **stdin/stdout protocol** between LLM and tool, not on-disk storage. This is OK and consistent with the "never yaml as storage" rule.

---

## Resolution (2026-04-25)

Closed as duplicate of **ISS-039**. The "skill does not exist / prompt is inline" complaint is fixed by ISS-039 commits:

1. **Prompt extraction** (commit `1d7943e`): the four graph-phase prompts (`generate_graph.txt`, `update_graph.txt`, `implement.txt`, `update_design.txt`) are now real `include_str!`-loaded files, replacing the ~110-line inline string.
2. **SQLite-canonical storage** (commit `1d7943e`): the prompts no longer reference deprecated `graph.yml`; load_graph_auto / save_graph_auto handle backend selection.
3. **Mode dispatch + validation + wiring** (commits `ae834ff`, `d60da91`): the skill is now a real, testable phase with PlanNew / Reconcile / NoOp dispatch and ID-collision validation.

The component-middle-layer / planned-code-node concerns from this issue's "Current State" table are real but separate — they describe schema gaps in `parse_llm_response`, not the skill itself. If still relevant after ISS-039, file a fresh issue scoped to the schema specifically.
