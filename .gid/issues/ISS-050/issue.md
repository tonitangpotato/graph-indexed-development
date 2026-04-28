---
id: "ISS-050"
title: "Standardize protocol for handling implementation-vs-planned-graph drift"
status: open
priority: P2
created: 2026-04-26
severity: high
related: ["ISS-035", "ISS-038", "ISS-039"]
---
# ISS-050 — Standardize protocol for handling implementation-vs-planned-graph drift

**Status:** open
**Type:** process / tooling — root fix
**Severity:** high (without this, the planned-code graph rots silently and `gid_query_*` becomes unreliable for any project past Day 1 of implementation)
**Discovered:** 2026-04-26 — engram v0.3 implementation session. Audit revealed 9 orphan modules in `crates/engramai/src/resolution/` (~3,600 LOC: signals.rs, fusion.rs, decision.rs, trace.rs, adapters.rs, candidate_retrieval.rs, edge_decision.rs, stage_edge_extract.rs, status.rs) with no `code:planned:*` nodes, while 5 planned nodes (worker, preserve, failure, telemetry, stats) remain unwritten. design.md §3.4/§3.5/§3.2 was updated in-flight but graph node descriptions still reference the old design sections.
**Related:** ISS-035 (code-graph low-confidence edges), ISS-038 (implement-phase post-condition — adjacent: catches *zero* drift but not *uncatalogued* drift), ISS-039 (graph-phase prompts stale)

## Problem

When implementing a feature whose `code:planned:*` nodes were created during a design phase, three things happen during real coding that the current tooling does not handle:

1. **Splits** — a single planned file (`stage_resolve.rs`) turns out to deserve 5 modules (`signals.rs` + `fusion.rs` + `decision.rs` + `trace.rs` + `adapters.rs`) because §3.4.2 / §3.4.3 of the design grew during implementation.
2. **Replacements** — a planned file is superseded by a differently-named one (`stage_resolve.rs` → `candidate_retrieval.rs`).
3. **Additions** — implementation discovers genuine new modules the design didn't anticipate (`status.rs`, `edge_decision.rs`).

Today, the agent's actual behavior is one of:

- **A. Strict planned mode** — refuse to write any file without a planned node. Forces design churn for every micro-decision; in practice gets bypassed.
- **B. Free-form** — write whatever feels right, ignore graph. graph rots; `gid_query_impact` silently undercounts; future LLM context misses real modules. **This is what currently happens by default.** The engram v0.3 audit caught this only because the human asked.
- **C. Drift-and-sync** — write code, but every drift event (new file, cancelled plan, split, merge) triggers an in-flight graph update before the next drift accumulates.

C is the right behavior. The tooling and process should make C cheap and B expensive.

## Why this is a root-fix issue, not a guideline

A SOUL.md rule "remember to update the graph when you create a new file" is what we have now. It does not work — the engram audit shows ~3,600 LOC of drift accumulated across one feature without any rule violation being noticed. The fix is not "be more disciplined"; it is **tooling + standardized protocol** that makes drift visible at commit time and trivial to reconcile.

## Proposed solution

### 1. Drift detection tool: `gid_drift` (new command)

Compares filesystem under a directory against `code:planned:*` and `code:*` nodes for that path:

```
gid drift --dir crates/engramai/src/resolution
```

Reports four categories:

- **Orphans** — files on disk with no corresponding node (untracked drift).
- **Missing** — `code:planned:*` nodes whose file does not exist on disk (unwritten plan or cancelled-without-update).
- **Stale descriptions** — node descriptions referencing design sections (§N.M) whose content has changed since the node was created (compare git blame on design.md).
- **Aligned** — files with a node, node not stale.

Exit nonzero if orphans + stale > 0 (configurable threshold). Hookable into pre-commit / ritual `verify` phase.

### 2. Drift reconciliation actions (extend `gid_refactor` or add `gid_drift_resolve`)

Three primitives, each one tool call:

- `gid drift add <path>` — orphan → planned node (auto-fills file_path, prompts for description, attaches `defined_in` edge to the parent feature node).
- `gid drift cancel <node_id> [--superseded-by <id>...]` — mark a planned node cancelled, optionally pointing at the replacements; auto-adds `superseded_by` edges.
- `gid drift split <node_id> <new_id_1> <new_id_2> ...` — cancel original + create N new planned nodes, copy `satisfies`/`depends_on` edges to all new nodes, ask user to prune.

### 3. Ritual integration

`implement` phase post-condition (currently checks "did any file change") gains a second check: **"are all changed/created files registered in graph?"** If not → `SkillFailed` with the orphan list. Forces drift sync before the phase succeeds. Same mechanism as ISS-038 but for catalog completeness, not just file-existence.

### 4. SOUL.md / AGENTS.md update

Add a short section documenting the drift-and-sync protocol with the three primitives above. Tooling enforces it; doc explains it. Without doc, agents won't know the tool exists.

## Acceptance criteria

- [ ] `gid drift --dir <path>` exists, reports orphans/missing/stale/aligned, exits nonzero on drift.
- [ ] `gid drift add` / `cancel` / `split` work as one-call reconciliations and update edges correctly.
- [ ] Ritual `implement` post-condition rejects unregistered new files with diagnostic.
- [ ] Tests: drift detection on a fixture project with all four categories present.
- [ ] AGENTS.md (or equivalent skill doc) documents the protocol.
- [ ] Engram v0.3 reconciliation (this week, manual) is the validation case — does the tool, once built, catch what the human caught manually today.

## Out of scope

- Auto-suggesting *what* a new orphan file is for (LLM naming) — that's gid_infer territory; this issue is about detection + manual reconciliation primitives.
- Cross-feature drift (multi-feature graphs). Single-feature drift first; v0.2.

## Notes

- The protocol matches how good engineering teams treat tests: tests must pass at commit time, not "remember to add tests later". Drift sync should feel the same — a graph commit is part of the implementation commit.
- Rationale source: engram v0.3 session 2026-04-26. Audit transcript and reconciliation steps will be linked from this issue once the manual cleanup is complete (separate engram-side issue).

## Progress

**2026-04-28 — Spec doc landed.** `DRIFT_PROTOCOL.md` (repo root — `docs/` is
gitignored in gid-rs, same precedent as `RITUAL_RUN_IMPLEMENTATION.md`). Doc covers:
problem statement, four drift categories, `gid drift --dir <path>` synopsis (flags,
exit codes 0/1/2/3, text + JSON output formats), `gid drift add` / `cancel` / `split`
semantics with edge effects, ritual `implement` post-condition contract + failure
diagnostic format + escape hatch, AGENTS.md guidance, engram v0.3 reconciliation as
the validation fixture (9 orphans / 5 missing / stale on §3.4), out-of-scope items
for v0.1, references. AGENTS.md (rustclaw side) has a short pointer section.

CLI implementation **deferred to post-0.4.0-publish** — coupling protocol churn with
a release would force premature stabilization. ACs (`gid drift` exists, reconciliation
primitives work, ritual post-condition rejects unregistered files, fixture tests, doc)
are unmet by design until that gate clears. Spec doc is the contract the
implementation must meet.
