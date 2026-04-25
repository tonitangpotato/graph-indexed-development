# ISS-039: Graph Phase Prompts Are Stale, Hardcoded, and Pollution-Prone

**Status:** open
**Severity:** critical (every project running ritual v2 hits this; silent graph corruption + wasted token spend; blocks ISS-032 and any other ritual-driven work)
**Reported:** 2026-04-25
**Reporter:** potato + RustClaw (during pre-flight investigation for engram ISS-032 ritual run)
**Supersedes:** ISS-031, ISS-036 (will close as duplicates of this when ISS-039 lands)
**Related:** ISS-038 (implement-phase-no-output — sibling root-cause fix in same subsystem)

## Summary

Ritual v2's `Graphing` phase (both `generate-graph` and `update-graph` skill paths) is **architecturally stale**: prompts are hardcoded as Rust string literals in `v2_executor.rs`, they reference `.gid/graph.yml` (deprecated since v0.3 — canonical is `.gid/graph.db` SQLite), there is no plan-new vs add-to-existing mode dispatch, and there is no ID-collision validation. The result is that the Graphing phase, when it runs against a real project, is one of three things:

1. **Empty no-op** — LLM writes `.gid/graph.yml` (a gitignored, derived-state file no tool reads anymore). Burn tokens, change nothing. Misleading "phase succeeded" signal.
2. **Active pollution** — LLM invents sub-tasks for an issue that already has planned IDs in `issue.md`, ID-collides with the human's plan, "succeeds" because YAML parses (ISS-031 incident, 2026-04-23, engram ISS-021 Phase 5b).
3. **Hard fail** — on projects with no `graph.yml` at all (modern v0.3+), the read-then-write logic in `v2_executor::graph_phase` short-circuits with `info!("No graph.yml found, skipping graph update")` (line 625). Phase silently skipped; downstream phases run against a graph that was never planned.

All three failure modes are silent. None reports an error to the ritual state machine.

## Why This Is The Root Cause

ISS-031 and ISS-036 (both currently `open`) describe symptoms of the same underlying defect:

- **ISS-031** (graph phase pollutes existing tasks) → root cause: **no plan-new vs reconcile mode dispatch + no existing-ID context injection in prompt**.
- **ISS-036** (design-to-graph skill incomplete) → root cause: **skill prompts are hardcoded fallbacks in v2_executor.rs, not extractable, not editable, and target a deprecated graph format**.

Fixing either one in isolation patches a symptom and leaves the other live. Fixing the prompt subsystem properly resolves both, plus catches the latent ISS-038-adjacent failure mode (graph phase silently no-ops on modern projects).

This is not a "the prompt has a typo" fix. It is "the prompt subsystem was built before the v0.3 SQLite-canonical migration and has not been updated since."

## Diagnostic Evidence (verified 2026-04-25)

> **Pre-flight architectural note (added 2026-04-25 during self-review).** The graph-phase LLM does **not** invoke graph operations through native agent tools (e.g. `gid_add_task`). The ritual `graph_ops` ToolScope (`crates/gid-core/src/ritual/scope.rs:158`) exposes only `Read / Write / Bash`, with `bash_policy = AllowList(["gid "])`. The LLM operates the graph by emitting `gid <subcommand>` shell calls. This is the correct intended design — keep it. The fixes below assume this constraint. CLI surface verified to support every operation the new prompts require: `gid tasks` (list), `gid query …` (inspect), `gid add-node --id … --node-type task` (create with caller-specified ID, used in plan-new mode), `gid add-task` (create with auto-generated ID, alternative for plan-new), `gid task-update --id … --status …` (status reconcile), `gid add-edge` (relations), `gid complete` (mark done). No CLI extension is required.

### E1 — All graph-phase prompts are hardcoded `match` arms in v2_executor.rs

`crates/gid-core/src/ritual/v2_executor.rs:725` — `load_skill_prompt` has a three-layer fallback:

```rust
// Priority: .gid/skills/{name}.md → ~/rustclaw/skills/{name}/SKILL.md → built-in fallback
```

Of the built-in fallbacks (lines 753–845):
- `draft-design` → loaded from `prompts/draft_design.txt` (file, ✅ correct pattern)
- `update-design`, `generate-graph`, `design-to-graph`, `update-graph`, `implement` → all hardcoded string literals

The "correct" extraction pattern exists (`include_str!("prompts/draft_design.txt")`) and is used for exactly one skill. Every other skill is inline.

### E2 — Hardcoded prompts reference deprecated `.gid/graph.yml`

Searched in `crates/gid-core/src/ritual/v2_executor.rs`:

- Line 763: `generate-graph` prompt → "write it to .gid/graph.yml"
- Line 807: `generate-graph` prompt → "Use the Read tool to read DESIGN.md, then Write tool to create .gid/graph.yml"
- Line 811–822: `update-graph` prompt → "Read the existing .gid/graph.yml" + "Write to update .gid/graph.yml"
- Line 828–842: `implement` prompt → "Read .gid/graph.yml to find all task nodes" + "Update graph.yml status incrementally"

All four prompts instruct the LLM to round-trip a YAML file. The canonical store has been SQLite (`graph.db`) since v0.3. `graph.yml`, when it exists, is either a leftover from migration or is gitignored and never read by current `gid` commands.

### E3 — v2_executor itself round-trips graph.yml in TWO functions

Not just the prompts — the executor's own bookkeeping operates on YAML in two places, both of which are invoked by the ritual loop:

**`graph_phase` (graph-update orchestration), lines 623–671:**
- Line 623–625: `let graph_path = ".../graph.yml"; if !graph_path.exists() { skip }`
- Line 633–640: parse YAML manually
- Line 671: write YAML back

**`update_graph` (called from `implement` phase after each task completes), lines 620–680:**
- Same yml read/parse/write pattern
- Uses fuzzy title matching (`desc.contains(title) || title.contains(desc)`) to find the node, then `mark_task_done` + serialize back to yml

**`build_graph_context` (assembles task context for `implement` phase prompt), line 973:**
- `let graph_path = gid_root.join("graph.yml")` → reads graph.yml
- Parses, filters for non-Done task nodes, returns string for prompt injection
- On modern (post-v0.3) projects with no graph.yml, this returns `None` and the implement phase runs with **zero graph context** — invisible failure mode

So even if the LLM did the right thing through `gid` CLI, the executor's bookkeeping would not see it (writes to graph.db, reads from graph.yml). The ritual is operating on a parallel, stale, deprecated representation. All three call sites must be ported to graph.db (SQLite) — this is a non-trivial expansion of Fix-2's scope but is the only correct boundary for the root fix.

### E4 — No mode dispatch for plan-new vs reconcile

`composer.rs:155` selects `update-graph` when graph exists, `generate-graph` when not. But "graph exists" is the wrong question. The right question is **"does this work unit already have a planned task breakdown?"**:

- If `work_unit.task_id` resolves to an existing graph node → reconcile mode (status updates only)
- If `work_unit.issue_id` has child task nodes already in the graph → reconcile mode
- If `issue.md` contains a numbered task list with planned IDs → those IDs are the editable set; LLM must not invent new ones
- Else → plan-new mode

Current code only branches on `graph.yml` file presence. This is the structural defect that allowed the ISS-031 incident.

### E5 — No ID-collision validation post-LLM

`graph_phase` writes whatever YAML the LLM produced after parse-success check. There is no validation step that compares newly-introduced node IDs against the set of IDs the LLM was told "do not use" (planned-but-not-materialized IDs from `issue.md`, or existing IDs from the parent issue's task subtree).

The ISS-031 incident (5 fabricated nodes `ISS-021-9..13` colliding with human's planned `ISS-021-12/13`) had no defense layer between "LLM emits YAML" and "graph is mutated."

## Scope of Root Fix

Four changes in `crates/gid-core/src/ritual/`. Each is a discrete deliverable. Together they constitute the root fix.

### Fix 1 — Extract all graph-phase prompts to `prompts/*.txt`

Mirror the existing `draft_design.txt` pattern. New files:

- `crates/gid-core/src/ritual/prompts/generate_graph.txt` (plan-new mode)
- `crates/gid-core/src/ritual/prompts/update_graph.txt` (reconcile mode)
- `crates/gid-core/src/ritual/prompts/implement.txt` (currently inline at line ~828; extract for symmetry, content fixes are part of Fix 2)
- `crates/gid-core/src/ritual/prompts/update_design.txt` (currently a one-liner inline at line 757; extract for completeness)

`load_skill_prompt` `match` arm becomes:

```rust
match skill_name {
    "draft-design"               => Ok(include_str!("prompts/draft_design.txt").to_string()),
    "update-design"              => Ok(include_str!("prompts/update_design.txt").to_string()),
    "generate-graph" | "design-to-graph" => Ok(include_str!("prompts/generate_graph.txt").to_string()),
    "update-graph"               => Ok(include_str!("prompts/update_graph.txt").to_string()),
    "implement"                  => Ok(include_str!("prompts/implement.txt").to_string()),
    _ => Err(anyhow!("Unknown skill: {}", skill_name)),
}
```

**Why this matters even though it looks cosmetic:** Once prompts live in text files, iterating on them does not require recompiling, does not mix prose with Rust syntax (escaping hell), and is reviewable as content rather than as code. Every subsequent fix to graph phase becomes a low-friction edit rather than a recompile cycle.

**LOC estimate:** ~50 deletions in `v2_executor.rs`, ~4 new prompt files (~150 lines total content combined, though that is Fix 2's scope).

### Fix 2 — Rewrite all graph-phase prompts for SQLite-canonical, gid-CLI-driven graph manipulation

The fundamental shift: **stop asking the LLM to write YAML files. Tell it to invoke `gid` CLI subcommands via Bash.** The graph is a structured store with a typed CLI; round-tripping through human-readable YAML is both stale (wrong format) and lossy (no transactional guarantees, no validation).

The LLM's available toolset in `Graphing` phase is `Read / Write / Bash` with `bash_policy = AllowList(["gid "])`. The prompts must instruct it to use `gid` subcommands, **not** native agent tools (`gid_add_task` etc. are not present in this ToolScope).

**Companion executor-side changes (NOT just prompt rewrites):** Fix 2 also includes porting `v2_executor::graph_phase`, `update_graph`, and `build_graph_context` from yml round-trip to graph.db queries. These three functions (see E3) are how the ritual loop reads/writes the canonical state. They must use `gid_core::graph::Graph::load_sqlite(...)` (or equivalent) rather than `serde_yaml::from_str(read_to_string(graph.yml))`. Without this companion change, the prompt fix alone produces correct LLM behavior that the executor immediately ignores. Estimated +60 LOC executor changes on top of prompt rewrites.

For each new prompt file:

#### `prompts/generate_graph.txt` (plan-new mode)

Tell the LLM:
- The canonical graph is `.gid/graph.db` (SQLite). Do not read or write `.gid/graph.yml`.
- To add nodes, run `gid add-node --id <ID> --title "<title>" --node-type task --status todo` (use `add-node` when ID is dictated by the issue's planned-IDs list; use `add-task --title "..."` when ID can be auto-generated).
- To add edges, run `gid add-edge <from_id> <to_id> --relation depends_on`.
- Read the design document first (path provided via `{design_path}` template variable).
- Three-tier structure permitted: feature → component → task. Use `--node-type feature | component | task | code`. Component middle layer is optional; use only when a feature has >5 tasks.
- For tasks that will modify existing source code, emit `gid add-edge <task_id> <code_id> --relation modifies`. The list of existing code-node IDs available in this graph is injected as `{existing_code_nodes}` (computed pre-LLM by the executor — see Fix 3).
- For tasks that will create new files/symbols not yet extracted, the LLM should NOT pre-create code nodes; emit only `task` nodes with file paths in their description. Reconciliation with extracted code nodes is a follow-up (ISS-040).

#### `prompts/update_graph.txt` (reconcile mode)

Tell the LLM:
- These task nodes already exist for this work unit (injected as `{existing_task_nodes}`):
  ```
  ISS-XXX-1  | done       | <title>
  ISS-XXX-2  | in_progress| <title>
  ISS-XXX-3  | todo       | <title>
  ```
- These IDs are reserved by the issue's design doc and **must not be used for new nodes** (injected as `{reserved_planned_ids}`):
  ```
  ISS-XXX-12, ISS-XXX-13  (planned in issue.md §Phase 5b)
  ```
- Your task in this phase is **status reconciliation**, not planning. Permitted operations:
  - `gid task-update --id <ID> --status done|in_progress|blocked` to transition existing nodes.
  - `gid add-edge <from> <to> --relation depends_on` between existing nodes (if missing).
- Forbidden:
  - `gid add-node` and `gid add-task` — no new task nodes in this mode, period.
  - Any command that uses an ID from `{reserved_planned_ids}`.
- If after reading the work unit and the existing nodes you conclude no changes are needed, respond with the literal string `NO_GRAPH_CHANGES` and run no `gid` commands.

#### `prompts/implement.txt`

Same SQLite-canonical correction. The implement phase LLM uses `gid task-update --id <ID> --status done` (or `gid complete <ID>`) to mark its assigned task done. Other content (work_unit context, design section reference, file-to-modify list) carries over from the existing inline prompt.

#### `prompts/update_design.txt`

Minor — current one-liner says "Read DESIGN.md and update it." Carry through; no SQLite concerns here since this is a markdown phase.

**LOC estimate:** ~150 lines of prompt content total across 4 files + ~60 LOC for executor port (graph_phase, update_graph, build_graph_context — all three use the same `Graph::load_sqlite` boilerplate, so it's three small parallel rewrites, not three large ones).

### Fix 3 — Mode dispatch in v2_executor::graph_phase

Replace the current "graph.yml exists?" branch (lines 623–640 region) with structured mode determination:

```rust
enum GraphPhaseMode {
    PlanNew,                       // No existing task subtree for this work unit
    Reconcile {
        existing_nodes: Vec<TaskNode>,    // Injected into update_graph.txt
        reserved_ids: Vec<String>,        // Parsed from issue.md task list
    },
    NoOp,                          // work_unit.task_id resolves to a single task node — no graph work needed
}

fn determine_graph_mode(&self, work_unit: &WorkUnit) -> Result<GraphPhaseMode> {
    // 1. Query graph.db for nodes with id-prefix matching work_unit.issue_id
    // 2. If work_unit.task_id is set and resolves → NoOp (status update happens in implement phase)
    // 3. If issue has child task nodes → Reconcile
    // 4. Else → PlanNew
}
```

Skill selection in `composer.rs:155` follows from the mode (no longer based on graph.yml file presence). For Reconcile mode, the executor must:
- Pre-LLM: query graph.db for existing task subtree, format as table for prompt template.
- Pre-LLM: parse `issue.md` for explicit planned-ID list (look for patterns like "planned: ISS-XXX-12, ISS-XXX-13" or numbered task lists). If found, format as `reserved_planned_ids`. If not found, the reserved set is just "next-available IDs that conflict with existing IDs."
- Post-LLM: collect any node IDs the LLM proposed creating (parsed from `gid_add_task` tool calls in the agent transcript, or from a structured response format). Validate against forbidden set. See Fix 4.

**LOC estimate:** ~100 LOC. Mostly straightforward graph-db queries and prompt template formatting. Issue.md parsing is heuristic — a regex over numbered list lines.

### Fix 4 — ID collision validation hook (hard-fail ritual on detection)

This is the safety net that prevents ISS-031-class incidents from corrupting the graph silently.

**Mechanism: snapshot-and-diff, not transcript parsing.** Pre-LLM, capture the set of node IDs in graph.db for this work unit's subtree. Post-LLM (after the agent loop returns), capture again. The diff is the set of "node IDs the LLM created during this phase" — ground truth, no parsing of bash transcripts required.

```rust
fn validate_graph_phase_output(
    &self,
    mode: &GraphPhaseMode,
    work_unit: &WorkUnit,
    nodes_before: &HashSet<String>,
    nodes_after: &HashSet<String>,
) -> Result<()> {
    let new_node_ids: HashSet<&String> = nodes_after.difference(nodes_before).collect();

    if let GraphPhaseMode::Reconcile { reserved_ids, .. } = mode {
        let reserved_set: HashSet<&String> = reserved_ids.iter().collect();

        // Reconcile mode forbids new task nodes entirely.
        if !new_node_ids.is_empty() {
            bail!(
                "Graph phase produced {} new node(s) in Reconcile mode: {:?}. Forbidden — only status updates permitted. Ritual aborted.",
                new_node_ids.len(),
                new_node_ids
            );
        }

        // (Existing-ID overwrite is impossible via `gid` CLI — `add-node` rejects duplicate IDs at the CLI level.
        //  The reserved-ID check below is for plan-new mode where the issue.md declares planned IDs that
        //  must not be claimed before their time.)
        for new_id in &new_node_ids {
            if reserved_set.contains(new_id) {
                bail!(
                    "Graph phase ID collision: '{}' is reserved by issue design (planned-but-not-materialized). Ritual aborted to prevent pollution.",
                    new_id
                );
            }
        }
    }

    if let GraphPhaseMode::PlanNew { reserved_ids, .. } = mode {
        // Plan-new mode permits new nodes — but reserved IDs are still off-limits unless the LLM
        // is materializing exactly those planned IDs (allowed: the issue declared them as planned).
        // Distinction: if the LLM creates `ISS-X-12` and the issue's planned-IDs include `ISS-X-12`,
        // that's fine — the LLM is materializing the plan. Forbidden: creating `ISS-X-99` when planned-IDs = [12, 13].
        // This check is the inverse: any new node ID NOT in reserved_ids AND NOT matching the issue's expected scheme is suspect.
        // → For ISS-039 v1, we only enforce the simpler rule: in PlanNew mode, log new IDs but do not block.
        //   Stricter validation is ISS-041 follow-up.
    }

    Ok(())
}
```

On `bail`, the ritual state machine transitions to `Failed`. The graph mutation has already been written to graph.db by `gid` CLI calls — rollback requires deleting the diffed nodes:

```rust
// After bail, before propagating error:
for new_id in &new_node_ids {
    let _ = graph.delete_node(new_id);  // best-effort cleanup
}
```

Open question: does `gid_core::graph::Graph::delete_node` cascade-delete edges? (Per ISS-037 — "DeleteNode orphans edges" — it currently does not. Cleanup loop must also delete dangling edges. Tracked separately; ISS-039 implementation will use whatever ISS-037 lands.)

**LOC estimate:** ~80 LOC. Snapshot is a single `gid_core` query (`graph.list_node_ids_with_prefix(work_unit.issue_id)`); diff is a `HashSet` operation; rollback is a loop.

## Out of Scope (intentional — kept for follow-up issues)

These are real gaps but are **not blocking** for ISS-032 or other near-term ritual runs. Filing them separately keeps ISS-039 surgical:

- **planned-code-node reconciliation pass** (post-implement: replace `planned-code:` placeholders with real extracted code nodes after `gid extract` runs). Currently planned-code nodes just sit as orphans until manually cleaned. → file as ISS-040 follow-up.
- **issue.md "planned-IDs" auto-parser** beyond simple regex over numbered lists. ISS-039 uses heuristic; a structured frontmatter convention (`planned_task_ids: [ISS-XXX-12, ISS-XXX-13]`) would be more robust. → file as ISS-041 follow-up.
- **component middle-layer auto-decomposition** (ISS-036 mentioned this — when a feature has 15+ tasks, automatically inject component layer). ISS-039 makes the prompt support it but does not auto-trigger it. → ISS-036 will be partially closed by ISS-039; remaining auto-decomposition work re-filed as ISS-042 if still wanted.

## Acceptance Criteria

ISS-039 is `done` when all of the following pass:

1. **No more inline prompt strings in graph phase.** All graph-phase prompts (`generate-graph`, `update-graph`, `implement`, `update-design`, `design-to-graph`) load from `prompts/*.txt` via `include_str!`. The `match` arm in `load_skill_prompt` contains zero multiline string literals.
2. **Prompts and executor target graph.db, not graph.yml.** Files exist: `generate_graph.txt`, `update_graph.txt`, `implement.txt`, `update_design.txt`. Inside these files: zero occurrences of "graph.yml" (verified by `grep -l "graph.yml" crates/gid-core/src/ritual/prompts/*.txt` returning no matches). In `v2_executor.rs`: `graph_phase`, `update_graph`, and `build_graph_context` use `Graph::load_sqlite` (or equivalent SQLite-backed loader) — no `serde_yaml::from_str(read_to_string(graph.yml))` for canonical state. (Migration shims that detect a stray `graph.yml` and warn the user are permitted; they must not be used as the canonical read path.)
3. **Mode dispatch test.** Unit test: when `work_unit` references an issue with existing child task nodes in graph.db, `determine_graph_mode` returns `Reconcile { existing_nodes: ... }`. When no children, returns `PlanNew`.
4. **ID collision test.** Unit test: simulate Reconcile mode with `existing_nodes = [ISS-T-1]` and `reserved_ids = [ISS-T-2]`. Provide LLM "output" with `new_node_ids = [ISS-T-2]`. `validate_graph_phase_output` returns `Err` containing "reserved".
5. **End-to-end smoke test.** Run a ritual on a synthetic project where `issue.md` declares `ISS-T-12` as planned. Verify ritual either (a) does not create `ISS-T-12` as a fabricated node, OR (b) hard-fails with the expected collision error before mutating graph.db. The 2026-04-23 ISS-031 incident must not be reproducible.
6. **Cargo test green** on `crates/gid-core` after all changes.
7. **ISS-031 and ISS-036** updated: status set to `closed`, with note "Resolved by ISS-039." (ISS-036's component-decomposition gap re-filed as ISS-042 if not addressed by ISS-039's prompt rewrite.)

## Estimated Cost

- LOC: ~70 deletions, ~390 additions across 6 files (5 prompt txt + v2_executor.rs). Updated from initial estimate after self-review identified two additional yml round-trip sites in the executor (`update_graph`, `build_graph_context`) requiring port alongside `graph_phase`.
- Cycles: 1 ritual (design phase already done — this issue.md), 1–2 ritual implementation cycles. Risk-adjusted: 2 cycles likely, given the chicken-and-egg (this issue's own ritual will hit the bug being fixed).
- Wall time: 1 day, possibly 1.5 if the ritual self-bootstrap is rocky.

## Pre-flight Notes for the Implementing Ritual

When this issue itself is run through ritual v2:

- This is gid-rs's own repo. Ritual must run inside gid-rs project root, not from rustclaw or engram workspace.
- Graph phase will hit the **same defect this issue is fixing** — chicken-and-egg. Mitigation: this issue has no planned sub-task IDs in graph.db (verified pre-flight). Ritual will go through PlanNew mode against an empty work-unit subtree. Even with current broken prompt, plan-new mode is the safer of the two failure modes (no existing IDs to collide with).
- After Fix 1 + Fix 2 land mid-ritual, subsequent phases will use the new prompts. This is intentional — eat your own dog food.

## Reproduction of the Original ISS-031 Incident (for regression test design)

1. Create a graph.db with parent issue node `ISS-T` and child task nodes `ISS-T-1..8` (all `done`).
2. Place an `issue.md` at `.gid/issues/ISS-T.md` containing the line `Phase 2 planned IDs: ISS-T-12, ISS-T-13`.
3. Launch `start_ritual` with `work_unit = { kind: issue, project: <test>, id: ISS-T }`, `skip_design=true`.
4. **Pre-Fix behavior:** Graph phase invents `ISS-T-9..13`, ID-colliding with the planned 12/13. Writes graph.yml. Phase reports success. Implementing phase reads polluted graph, produces no code, returns. Verifying phase passes (no test changes). User rolls back graph manually.
5. **Post-Fix behavior:** `determine_graph_mode` returns `Reconcile`. update_graph.txt prompt injects existing `ISS-T-1..8` as the editable set and `ISS-T-12, ISS-T-13` as reserved. LLM is forbidden from creating new nodes; if it tries, collision validator hard-fails the ritual with a clear error message before graph.db is mutated.
