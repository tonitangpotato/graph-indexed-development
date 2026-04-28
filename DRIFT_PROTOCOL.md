# Drift Protocol — Spec (ISS-050)

> **Status:** Spec only. CLI implementation deferred to post-0.4.0 publish.
> **Owner:** ISS-050. Related: ISS-035, ISS-038, ISS-039.
> **Audience:** Agents (LLM-driven implementers) and humans reviewing graph hygiene.

This document specifies the **drift-and-sync** protocol that GID will enforce between a
project's filesystem and its `code:*` / `code:planned:*` graph nodes. It is the source
of truth for the future `gid drift` command family and the ritual `implement` phase
post-condition that consumes it.

The CLI is **not** yet implemented — this spec exists so that (a) the design can be
reviewed and stabilized before code lands, (b) ritual integration on the rustclaw side
can be wired to a known interface, and (c) the engram v0.3 manual reconciliation
(2026-04-26) can be replayed as the first validation case once the tool exists.

---

## 1. Problem statement

When an agent implements a feature whose `code:planned:*` nodes were created during a
design phase, three real-world events occur during coding that the current tooling does
not handle:

1. **Splits** — one planned file (`stage_resolve.rs`) becomes 5 modules
   (`signals.rs` + `fusion.rs` + `decision.rs` + `trace.rs` + `adapters.rs`) because
   §3.4.x of the design grew during implementation.
2. **Replacements** — a planned file is superseded by a differently-named one
   (`stage_resolve.rs` → `candidate_retrieval.rs`).
3. **Additions** — implementation discovers genuine new modules the design did not
   anticipate (`status.rs`, `edge_decision.rs`).

Today the agent's options are:

- **A. Strict planned mode** — refuse to write any file without a planned node. Forces
  design churn for every micro-decision; in practice gets bypassed.
- **B. Free-form** — write whatever feels right, ignore the graph. The graph rots,
  `gid_query_impact` silently undercounts, future LLM context misses real modules.
  **This is the current default.** The engram v0.3 audit on 2026-04-26 caught
  ~3,600 LOC of accumulated drift across one feature only because the human asked.
- **C. Drift-and-sync** — write code, but every drift event (new file, cancelled plan,
  split, merge) triggers an in-flight graph update before the next drift accumulates.

C is the right behavior. The protocol below makes C cheap and B expensive.

### 1.1 Drift categories

The drift detector classifies every file under a directory into exactly one of four
categories. Categories are mutually exclusive:

| Category | Definition | On disk? | Has node? |
|---|---|---|---|
| **Orphan** | File exists, no `code:*` or `code:planned:*` node points at it | ✅ | ❌ |
| **Missing** | `code:planned:*` node exists, file does not exist on disk | ❌ | ✅ |
| **Stale** | File and node both exist, but node description references a design section (`§N.M`) whose content has changed since the node was created (compare git blame on `design.md`) | ✅ | ✅ (out of date) |
| **Aligned** | File and node both exist, node not stale | ✅ | ✅ (current) |

**Drift = Orphans + Missing + Stale.** Aligned is the goal state.

`code:*` (already-realized) and `code:planned:*` (designed but unwritten) share a single
namespace for the purpose of drift accounting — both must be considered when matching
files against nodes. Implementations are encouraged to also recognize a `cancelled`
status on planned nodes; cancelled planned nodes are excluded from "missing" reporting.

---

## 2. `gid drift` — detection command

### 2.1 Synopsis

```
gid drift --dir <path> [--threshold <N>] [--format text|json] [--include-stale]
```

| Flag | Default | Meaning |
|---|---|---|
| `--dir <path>` | required | Directory to scan. Recursive. Honors `.gidignore`. |
| `--threshold <N>` | `0` | Exit nonzero only if `orphans + missing + stale > N`. |
| `--format text\|json` | `text` | Output format. JSON for tooling, text for humans. |
| `--include-stale` | off | Include stale-description detection (requires `git blame` on design.md). Off by default to keep the fast path fast. |

### 2.2 Exit codes

| Exit | Meaning |
|---|---|
| `0` | No drift above threshold. Safe to proceed. |
| `1` | Drift detected above threshold (orphans + missing + stale > N). Caller should reconcile before commit. |
| `2` | Usage error (bad flags, missing path). |
| `3` | I/O / graph load error. Distinguishable from drift so CI can decide retry vs fail. |

### 2.3 Text output format

Human-readable, scannable, deterministic ordering (alphabetical within each section):

```
gid drift report — crates/engramai/src/resolution
═══════════════════════════════════════════════════

Orphans (9 files, no graph node):
  signals.rs                       (added 2026-04-25, 412 LOC)
  fusion.rs                        (added 2026-04-25, 287 LOC)
  decision.rs                      (added 2026-04-26,  98 LOC)
  ...

Missing (5 planned nodes, no file):
  code:planned:resolution::worker      (planned 2026-04-20, design §3.4.1)
  code:planned:resolution::preserve    (planned 2026-04-20, design §3.4.4)
  ...

Stale (2 nodes referencing changed design sections):
  code:resolution::stage_resolve   (refs §3.4, last updated in design 2026-04-26)
  ...

Aligned: 14 files / 14 nodes

Summary: 9 orphans + 5 missing + 2 stale = 16 drift items
Threshold: 0
Result: DRIFT (exit 1)

Suggested next steps:
  gid drift add crates/engramai/src/resolution/signals.rs --feature engram-v0.3
  gid drift cancel code:planned:resolution::worker --reason "absorbed into telemetry"
  gid drift split  code:planned:resolution::stage_resolve   <new ids...>
```

### 2.4 JSON output format

Stable schema, additive evolution only. Fields with no value are omitted (not `null`).

```json
{
  "schema_version": 1,
  "scanned_dir": "crates/engramai/src/resolution",
  "scanned_at": "2026-04-28T06:50:12Z",
  "orphans": [
    {
      "path": "crates/engramai/src/resolution/signals.rs",
      "size_loc": 412,
      "git_added_at": "2026-04-25T14:12:00Z"
    }
  ],
  "missing": [
    {
      "node_id": "code:planned:resolution::worker",
      "expected_path": "crates/engramai/src/resolution/worker.rs",
      "planned_at": "2026-04-20T09:00:00Z",
      "design_ref": "§3.4.1"
    }
  ],
  "stale": [
    {
      "node_id": "code:resolution::stage_resolve",
      "path": "crates/engramai/src/resolution/stage_resolve.rs",
      "design_ref": "§3.4",
      "design_changed_at": "2026-04-26T11:30:00Z",
      "node_updated_at": "2026-04-20T09:00:00Z"
    }
  ],
  "aligned_count": 14,
  "summary": {
    "orphans": 9,
    "missing": 5,
    "stale": 2,
    "aligned": 14,
    "drift_total": 16
  },
  "threshold": 0,
  "result": "drift"
}
```

`result` ∈ { `clean`, `drift`, `error` } — matches exit codes 0 / 1 / 3.

---

## 3. Reconciliation primitives

Three subcommands, each one tool call from the agent's perspective. Each is a
single graph mutation that updates edges atomically.

### 3.1 `gid drift add`

Promote an orphan file to a tracked `code:*` node.

```
gid drift add <path> [--feature <feature-id>] [--description <text>] [--id <node-id>]
```

| Flag | Required? | Default | Notes |
|---|---|---|---|
| `<path>` | yes | — | Must be an orphan. Refuses if the file is already tracked. |
| `--feature <id>` | yes | — | Parent feature node. Used to attach `defined_in` edge. |
| `--description <text>` | no | inferred | If omitted, derives a stub from the file's module-level doc-comment. Editable later. |
| `--id <node-id>` | no | derived | Override for the auto-derived ID (`code:<feature-slug>:<module-path>`). |

**Edge effects:**
- Creates `code:*` node with `file_path` set.
- Adds `defined_in` edge from new node → parent feature node.
- If the file imports symbols already tracked, optionally adds `imports` edges
  (best-effort, low-confidence; flagged for ISS-035 follow-up).

### 3.2 `gid drift cancel`

Mark a planned node cancelled. Use when the design changed and the file will not be
written, or when the planned module was absorbed into a sibling.

```
gid drift cancel <node-id> [--superseded-by <id>...] [--reason <text>]
```

| Flag | Required? | Notes |
|---|---|---|
| `<node-id>` | yes | Must be a `code:planned:*` node. Refuses if file already exists. |
| `--superseded-by <id>...` | no | Zero or more replacement node IDs. Adds `superseded_by` edge from cancelled node → each replacement. |
| `--reason <text>` | no | Stored on the node as `cancellation_reason` metadata. Strongly recommended for audit trail. |

**Edge effects:**
- Sets node status to `cancelled` (status field, not deletion — deletion would orphan
  incoming `satisfies` / `depends_on` edges).
- Adds `superseded_by` edges to each replacement.
- Cancelled nodes are excluded from "missing" reporting in subsequent `gid drift` runs.

### 3.3 `gid drift split`

Cancel an original planned node and create N new planned nodes, copying inbound
relationships (`satisfies`, `depends_on`) to all new nodes. The agent then prunes which
relationships actually belong on which new node.

```
gid drift split <node-id> <new-id-1> <new-id-2> ... [--paths <path1> <path2> ...]
```

| Arg | Required? | Notes |
|---|---|---|
| `<node-id>` | yes | The planned node being split. |
| `<new-id-N>` | yes (≥2) | New planned node IDs. |
| `--paths <path...>` | no | Optional file paths for each new node, in order. If omitted, paths are derived from IDs. |

**Edge effects:**
- Cancels original (per §3.2 semantics, `--superseded-by` set to all new IDs).
- Creates new `code:planned:*` nodes.
- Copies all incoming `satisfies` / `depends_on` edges from original to each new node.
- Emits a warning telling the user to prune the over-broad copies via `gid edge rm`.

> **Why copy and prune, not ask?** Asking the user up front for which edges go where
> requires a decision they often cannot make until they see all N nodes. Copy + prune
> matches how humans think: "all of these touch GOAL-3, but only signals.rs touches
> GOAL-7."

---

## 4. Ritual integration

The ritual `implement` phase already enforces a post-condition (ISS-038, closed
2026-04-25): "did any file change?" via filesystem snapshot. ISS-050 extends this with
a **second** post-condition: **"are all changed/created files registered in the graph?"**

### 4.1 Post-condition contract

After the `implement` phase's filesystem snapshot diff, before the phase transitions to
`Reviewing`:

1. Compute the set of created/modified files from the snapshot diff.
2. Run the equivalent of `gid drift --dir <feature-root> --format json` (in-process —
   the runtime is already linked against `gid-core`, so it should call the library
   directly, not shell out).
3. If any file in the diff appears in the `orphans` list → `SkillFailed` with the
   orphan list embedded in the diagnostic.
4. If `missing` count is nonzero → emit a **warning** (not failure). Missing planned
   nodes are not always wrong — the agent may legitimately defer a planned module to a
   later phase.
5. `stale` is informational only at the post-condition stage. It may gate `verify`
   later (out of scope for v0.1 of this protocol).

### 4.2 Failure diagnostic format

The `SkillFailed` message must be agent-actionable. Format:

```
implement post-condition failed: 3 orphan files

Orphan files (created in this phase but not in graph):
  crates/engramai/src/resolution/signals.rs
  crates/engramai/src/resolution/fusion.rs
  crates/engramai/src/resolution/decision.rs

To resolve, register each file with:
  gid drift add <path> --feature <feature-id>

If a file should not be in the graph, add it to .gidignore.
```

The agent that receives this diagnostic in skill output should be able to call
`gid drift add` directly without further human intervention, then re-run `verify`.

### 4.3 Skipping (escape hatch)

A `--skip-drift-check` flag on the ritual `implement` phase invocation suppresses the
post-condition for one phase. **This must log loudly** (WARN-level event with the
ritual ID and reason) and the ritual state should record the skip. Use case: emergency
hotfix where graph hygiene blocks shipping.

---

## 5. AGENTS.md / SOUL.md guidance (proposed)

Tooling enforces. Doc explains. The agent-facing doc (whichever workspace consumes the
ritual — currently `/Users/potato/rustclaw/AGENTS.md`) should add a brief section:

> ### Drift Protocol (ISS-050)
>
> When implementing a feature with a planned-code graph (`code:planned:*` nodes):
> - **Adding a new file** the design did not anticipate → `gid drift add <path> --feature <id>`.
> - **Replacing a planned file** with a differently-named one → `gid drift split <old-id> <new-id-1>` (single-target split is the rename idiom).
> - **Splitting a planned file** into multiple modules → `gid drift split <old-id> <new-1> <new-2> ...`.
> - **Cancelling a planned file** that will no longer be written → `gid drift cancel <id> [--superseded-by <ids>]`.
>
> The ritual `implement` phase will fail if any file you write is not registered.
> Reconcile before retrying. Do **not** add `--skip-drift-check` to bypass — it logs a
> WARN event the human will see.

---

## 6. Validation case: engram v0.3 reconciliation

The 2026-04-26 engram v0.3 implementation session is the validation fixture for this
tool. Once `gid drift` lands, it must reproduce — without human prompting — what the
human caught manually:

| Expected detection | Files / nodes |
|---|---|
| 9 orphan files | `signals.rs`, `fusion.rs`, `decision.rs`, `trace.rs`, `adapters.rs`, `candidate_retrieval.rs`, `edge_decision.rs`, `stage_edge_extract.rs`, `status.rs` (in `crates/engramai/src/resolution/`) |
| 5 missing planned | `worker`, `preserve`, `failure`, `telemetry`, `stats` |
| Stale (with `--include-stale`) | Any node whose description references a design section updated after the node's `created_at` (in particular §3.4 which grew during implementation) |
| Reconciliation flow | A single human session should be able to run `gid drift add` × 9 + `gid drift cancel` × 5 (or `split` where appropriate) and reach a clean `gid drift` exit-0 |

Once that is true, ISS-050 acceptance criteria are satisfied (see issue.md). Until then,
the protocol described here is the **specification** the implementation must meet.

---

## 7. Out of scope (for v0.1 of this protocol)

- **LLM-suggested naming for orphans** — covered by `gid_infer` / community detection.
  Drift detection is mechanical; semantic naming is a separate problem.
- **Cross-feature drift** — multi-feature graphs require a notion of feature ownership
  per file. v0.2.
- **Auto-`drift add` on Write tool calls** — tempting, but hides the decision the agent
  needs to make (which feature owns this file?). Better to fail loud at the
  post-condition than silently catalog.
- **Stale propagation** (changing design §3.4 marks all descendants stale) — needs a
  proper design-blame index. v0.2.
- **CLI implementation** — this document is the spec; no command exists yet. Wiring is
  deferred to post-0.4.0 publish to avoid coupling protocol churn with a release.

---

## 8. References

- **ISS-050** (this issue): `.gid/issues/ISS-050/issue.md`
- **ISS-038** (closed): implement-phase fs-snapshot post-condition — sibling mechanism
  this protocol extends.
- **ISS-035**: code-graph low-confidence edges — `gid drift add` may emit such edges;
  flagged for separate tracking.
- **ISS-039**: graph-phase prompts stale — adjacent problem on the design side, not
  drift on the code side.
- **engram v0.3 reconciliation transcript** (engram-side, separate issue) — links here
  once published.
