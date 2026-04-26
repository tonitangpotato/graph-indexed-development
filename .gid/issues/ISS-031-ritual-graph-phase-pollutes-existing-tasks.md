# ISS-031: Ritual Graph Phase Pollutes Existing Tasks When skip_design=true

**Status:** closed (duplicate of ISS-039)
**Severity:** critical (silent data corruption — LLM invents tasks, overwrites graph, then verifies itself green)
**Reported:** 2026-04-23
**Reporter:** potato (via rustclaw session — Phase 5b of engram-ai-rust ISS-021)

## Summary

When a ritual is launched for implementation work that already has an existing, planned task in the project graph (e.g., `start_ritual` with `skip_design=true` targeting a specific sub-task ID), the state machine still routes through the **Graphing** phase. In Graphing, the LLM is prompted with "update the graph" but is not given:

1. An explicit list of existing task IDs for the parent issue
2. A clear semantic distinction between "update existing node status" vs "plan new sub-tasks"

The LLM interprets the prompt as "plan implementation tasks for this work unit", fabricates a fresh subdivision (5 fake sub-tasks in the observed case), assigns them the next available IDs — which **collide with IDs that the human had already planned in the issue design document but not yet materialized** — and writes them to `graph.yml`. The Graphing phase "succeeds" because YAML parses; subsequent phases inherit the polluted graph as ground truth.

## Observed Incident (2026-04-23, engram-ai-rust ISS-021 Phase 5b)

Work unit dispatched: `ISS-021` Phase 5b (counterfactual recall measurement — one CLI tool, one report).

- **Before ritual**: graph.yml had `ISS-021-1` through `ISS-021-8` (all `done`, Phase 2+3 completed). `issue.md` mentioned `ISS-021-12` and `ISS-021-13` as *planned future IDs* (not yet in graph) for Phase 5b's CLI and counterfactual baseline.
- **During Graphing phase**: LLM wrote 5 new nodes `ISS-021-9` through `ISS-021-13`, all `status: todo`. Descriptions were LLM-invented sub-steps of Phase 5b ("Create tempdir-based DB copy utility", "Implement dry-run migration transformations", "Re-embed transformed records", "Run recall baseline against hypothetical DB", "Compute delta and gate decision") with fabricated `estimated_effort: 30min/60min/45min/20min` metadata — none of which matched the human's actual plan.
- **During Implementing phase**: Sub-agent read the 5 todo nodes, concluded (incorrectly) that "the Graph phase has produced the plan; my job is to mark them as I execute." It wrote ~15k tokens of commentary, produced zero source code, marked nothing, returned control.
- **During Verifying phase**: `cargo test` passed (because nothing changed), gate went green.
- **Net result**: 15k tokens wasted. graph.yml polluted with 5 fake todo nodes. User manually rolled back via `git checkout HEAD -- .gid/graph.yml`.

The failure was silent — no phase reported an error. Only manual inspection of the graph revealed the pollution.

## Root Causes (three chained bugs)

### RC1 — State machine routes implementation work through Graphing unconditionally

In the ritual state machine, `skip_design=true` only bypasses Design. Graphing still runs. For a work unit that points at an *existing* task (or an existing issue with existing planned IDs), there is nothing new to graph — the phase should either be skipped or should run in **reconcile mode** (reads existing nodes, optionally updates statuses, never invents new IDs).

**Fix direction**: add a `work_unit.has_existing_plan` signal (true when `task_id` resolves to an existing graph node, OR when `issue_id` resolves to an issue with existing child nodes in the graph). When true, Graphing is either skipped or enters a restricted reconcile mode.

### RC2 — Graphing skill prompt is semantically ambiguous

Current `update-graph` skill (or equivalent) instructs the LLM to "update the graph to reflect the planned work." With no existing nodes shown, the LLM reasonably interprets this as "plan the work as graph nodes." With existing nodes shown (but the prompt still saying "plan"), the LLM interprets it as "plan more fine-grained sub-tasks underneath."

**Fix direction**: split into two distinct prompts, selected by state machine based on `has_existing_plan`:
- `plan-new-tasks` (only when no existing plan): "Given this work unit, produce the task breakdown as graph nodes."
- `reconcile-existing-tasks` (when existing plan present): "Here are the existing task nodes for this work unit: `[list]`. Update statuses only. Do NOT create new nodes. Do NOT invent sub-tasks. If the work unit is already represented, output `no_changes`."

### RC3 — LLM lacks existing-ID context → invents colliding IDs

Even if the state machine routed correctly, the LLM was given the issue description without the list of task IDs already present in the graph (or the planned IDs mentioned in the issue design doc). It picked "next available" by counting what was in-context, which was wrong.

**Fix direction**: the Graphing phase prompt must always include the exhaustive list of graph node IDs that share the work unit's issue prefix. In reconcile mode, this is the editable set. In plan-new mode, this is the "do not use these IDs" set.

## Why Cargo Test Gate Did Not Catch It

Verifying phase runs `cargo test` on the workspace. Phase 5b work is non-code (counterfactual measurement report) — no test could fail because no production code changed. The Verifying phase trusted `cargo test green` as proxy for "work done", but the actual deliverable was a written report + graph status update, neither of which was produced.

**Fix direction (secondary)**: Verifying phase should consult the Implementing phase's declared deliverables (files modified, nodes transitioned to done) and fail when the declared set is empty for non-trivial work units.

## Reproduction

1. Create an issue with a parent graph node + some completed child nodes (e.g., `ISS-XXX` with `ISS-XXX-1..8` all `done`).
2. In the issue markdown, mention future planned IDs (e.g., `ISS-XXX-12`) that are not yet in the graph.
3. Launch a ritual targeting that issue for further implementation work via `start_ritual` with `skip_design=true`.
4. Observe: Graphing phase will write new `todo` nodes, likely starting at `ISS-XXX-9`, and potentially colliding with the mentioned future IDs.

## Proposed Fix (design outline — do not implement before review)

### Change 1 — State machine: compute `has_existing_plan`

Before Graphing phase, resolve the work unit:
- `kind=task` → always `has_existing_plan=true` (task node must exist to be targeted)
- `kind=issue` → `has_existing_plan = graph.nodes.any(|n| n.id.starts_with(issue_id + "-"))`
- `kind=feature` → `has_existing_plan = graph.has_feature_nodes(feature_name)`

### Change 2 — State machine: Graphing routing

```
if has_existing_plan:
    → enter Graphing in "reconcile" mode
    → prompt: reconcile-existing-tasks
    → allowed writes: status transitions only (todo ↔ in_progress ↔ done)
    → hard reject: any new node creation with prefix matching the work unit
elif design_exists_but_graph_empty:
    → enter Graphing in "plan-new" mode
    → prompt: plan-new-tasks
    → required: include list of all graph IDs with same issue prefix as "reserved / do not use"
else:
    → skip Graphing entirely
```

### Change 3 — Schema guard in ritual runtime

Before committing graph writes from the Graphing phase, validate:
- In reconcile mode: diff must only touch `status` / `updated_at` fields of pre-existing nodes.
- In plan-new mode: new node IDs must not collide with any ID mentioned in the issue markdown's planned-IDs list (parsed via a simple `ISS-\d+-\d+` scan).

Hard-fail the ritual (not the agent turn) if either guard trips. Hard-fail = log, notify user, leave graph untouched.

### Change 4 — Verifying phase: deliverable audit

Verifying must receive a declared-deliverables manifest from Implementing (files changed, node statuses changed). If the manifest is empty AND `cargo test` is the only signal, mark the verification as `inconclusive` rather than `passed`, and require human confirmation.

## Impact if Not Fixed

- Every ritual on an existing issue with existing child nodes risks silent graph corruption.
- Token cost: ~15k per incident (observed).
- Trust cost: higher — the harness green-lights work that wasn't done, which erodes the "ritual is the guardrail" contract. If the human hadn't manually inspected the graph, the fabricated nodes would have become "the plan" and Phase 5b would have been implemented against LLM-invented sub-steps instead of the real issue design.

## Related

- **Parent tool**: `start_ritual` (rustclaw) / ritual state machine (gid-rs)
- **Observed in**: engram-ai-rust repo, ISS-021 Phase 5b
- **Not related to**: ISS-029 (ritual launcher work_unit) or ISS-030 (start-ritual tool) — those fix the *entry* contract. This issue is about the *Graphing phase* mid-pipeline behavior.
- **Tangentially related**: ISS-011 (walkup silent fallback) — same family of "silent fallback produces wrong answer" bugs.

## Acceptance Criteria

- [ ] State machine exposes `has_existing_plan` decision before Graphing phase.
- [ ] Two distinct Graphing prompts exist: `plan-new-tasks` and `reconcile-existing-tasks`.
- [ ] Reconcile mode hard-rejects new node creation via runtime guard (not just prompt instruction).
- [ ] Plan-new mode prompt includes exhaustive list of existing IDs with matching prefix.
- [ ] Verifying phase reports `inconclusive` (not `passed`) when Implementing produces empty deliverable manifest.
- [ ] Regression test: reproduce the incident scenario, assert ritual either skips Graphing or runs in reconcile mode with zero new nodes written.

## Evidence Artifacts

- Polluted graph.yml snapshot: `/tmp/graph.yml.polluted-2026-04-23.bak` (on potato's Mac mini, 4.2MB)
- Rollback commit context: rustclaw repo, HEAD = `28c3afb` prior to incident
- Full incident timeline: rustclaw `memory/2026-04-23.md` (ritual session record)

## Open Questions

- Should reconcile mode allow adding *new* sub-task nodes if the LLM legitimately detects missing granularity? (Current proposal: no. Escalate to human.)
- How does this interact with multi-issue features (one feature spanning multiple issues)? Probably the prefix-matching rule extends to feature scope — not in initial fix.

---

## Resolution (2026-04-25)

Closed as duplicate of **ISS-039**. The root cause described here (graph-phase prompts that lack mode dispatch and let the LLM invent colliding task IDs) is exactly what ISS-039 fixed. Specifically:

1. **Mode dispatch** (commit `ae834ff`): `determine_graph_mode` distinguishes PlanNew / Reconcile / NoOp from `WorkUnit` + graph state. The exact scenario in the incident report (engram-ai-rust ISS-021 Phase 5b) — work unit references an existing issue with materialized child tasks — now hits **Reconcile** mode, which forbids new node creation.
2. **ID-collision validation** (commit `ae834ff`): `validate_graph_phase_output` snapshots node IDs before and after the LLM runs and rejects any output that creates new nodes in Reconcile mode.
3. **Wiring** (commit `d60da91`): `V2Executor::run_skill` now invokes preflight + postvalidate around the graph phase. Verified by `graph_phase_reconcile_rejects_new_node_creation` test, which reproduces the original incident scenario and asserts the ritual emits `SkillFailed` with an ISS-039 diagnostic instead of letting the LLM pollute the graph.

The "skip_design=true routes through Graphing" issue is unchanged structurally — that routing is correct; what was wrong was Graphing's lack of guardrails.
