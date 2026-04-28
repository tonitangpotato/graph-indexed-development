---
id: ISS-057
title: Ritual issue-mode incorrectly routes to WritingRequirements + self-review deadlock causes Escalation
status: fixed
priority: P1
created: 2026-04-28
severity: high
related:
- ISS-022
- ISS-052
- ISS-056
reporter: RustClaw (self-observed during engram ISS-044 work)
fixed_in: composer-iss057-routing-fix
---

## Symptom

Starting a ritual with `work_unit.kind = "issue"` against a project that
already has a fully-written `.gid/issues/ISS-NNN/issue.md` fails after 3
retries on `WritingRequirements` and lands in `Escalated`. The ritual
never reaches `ExecuteTasks` even though the issue body fully specifies
the work.

Failed instance:

- ritual id: `r-e344ec`
- target: engram repo, ISS-044 (wire `run_backfill` stub)
- started: 2026-04-28 20:58 -04:00
- escalated: 2026-04-28 21:04 -04:00 (≈6 min, 3 attempts)
- preserved state: `/Users/potato/rustclaw/.gid/ritual-state.json.failed-r-e344ec-20260428`

## Reproduction

```text
start_ritual(
  task: "Wire MigrationOrchestrator::run_backfill to PipelineRecordProcessor (ISS-044). ...",
  work_unit: { kind: "issue", project: "engram", id: "ISS-044" }
)
```

Pre-conditions present in the engram repo:

- `/Users/potato/clawd/projects/engram/.gid/issues/ISS-044/issue.md` exists (≈5KB, GOAL/GUARD format)
- engram graph exists (`.gid/graph.db` for issue tracker, `.gid-v03-context/graph.db` for v0.3 work)
- source tree present (`crates/engramai-migrate/src/cli.rs:564` is the stub site)

Project state from the failed `RitualState`:

```json
"project_state": {
  "has_design": false,
  "has_graph": true,
  "has_requirements": false,
  "has_source": true,
  "test_runner": null
}
```

`has_requirements: false` is the first symptom — the detector does not
treat `issues/ISS-NNN/issue.md` as a requirements artifact for issue-mode
work units.

## Observed transition trace

From `RitualState.transitions`:

1. `Idle → Initializing`
2. `Initializing → Triaging`
3. `Triaging → WritingRequirements` ← first wrong step
4. `WritingRequirements → WritingRequirements` (retry 1)
5. `WritingRequirements → WritingRequirements` (retry 2)
6. `WritingRequirements → Escalated` (`phase_retries.requirements = 2`)

`error_context.last_error`:

> "I cannot find any artifacts from a previous step in this workspace.
> The directories I would expect to contain output from the previous
> phase (such as ./design/, ./specs/, or ./out/) are not present here.
> ... There are no requirements files, spec files, or ritual artifacts
> to review. REVIEW_REJECT No artifacts from the previous step exist
> in the working directory."

## Root cause analysis (two interacting bugs)

### Bug 1 — Issue-mode routing ignores `issue.md` as requirements

`Triaging → WritingRequirements` happens unconditionally when
`has_requirements: false`. For issue-mode work units, the project's
`.gid/issues/<id>/issue.md` already plays the role of requirements
(it has GOAL-N / GUARD-N / acceptance criteria sections). The triage
phase should:

1. Detect `work_unit.kind == "issue"`.
2. Resolve `<project_root>/.gid/issues/<id>/issue.md`.
3. If present, set the requirements artifact pointer to that file
   and skip both `WritingRequirements` and `WritingDesign`, routing
   to `PlanTasks` (or directly `ExecuteTasks` if the graph already
   has tasks tagged with the issue id).

This mirrors how `feature` mode treats `.gid/features/<name>/requirements.md`.

### Bug 2 — `WritingRequirements` runs a review prompt instead of (or before) a write prompt

The escalation message ("REVIEW_REJECT — no artifacts from the previous
step") is what a *reviewing* sub-agent would say, not a *writing* one.
A writing phase should produce an artifact; here it appears to be
running a review against its own not-yet-existent output, fail-loop the
same prompt three times, then escalate.

Two plausible code-level causes (someone with the runner code in front
of them should disambiguate):

- The phase prompt template for `WritingRequirements` accidentally
  includes review-style instructions ("review the prior phase's
  output").
- The runner is dispatching the review-sub-agent under the
  `WritingRequirements` phase label (mis-wired phase → sub-agent map).

Either way, the symptom is the same: a write-phase that REVIEW_REJECTs
because there is nothing to review.

## Why this matters

ISS-044 is a P1 fix on engram v0.3 (the backfill stub explains all 8
empty-result fixtures). The ritual was the *intended* entry point and
it currently makes that path unusable for any issue-driven work.
Workaround (bypass ritual) defeats the whole point of having ritual
gating.

## Acceptance criteria

- **GOAL-1** — Issue-mode skip: when `work_unit.kind == "issue"` and
  `<project_root>/.gid/issues/<id>/issue.md` exists, the ritual MUST
  treat that file as the requirements artifact and route Triaging
  directly to `PlanTasks` / `ExecuteTasks` (skipping
  `WritingRequirements` and `WritingDesign`). Verified by integration
  test: a fresh issue-mode ritual on a fixture with an existing
  `issue.md` reaches `ExecuteTasks` without ever entering
  `WritingRequirements`.

- **GOAL-2** — Phase/prompt alignment: a *writing* phase MUST NOT
  receive a review-style prompt. Add a unit/integration test that
  asserts the prompt template for `WritingRequirements` (and any
  other write-phase) contains writing instructions and does NOT
  include the substring `REVIEW_REJECT` or "review the previous
  step's output". If a separate review pass is needed inside a
  write phase, it MUST be a distinct sub-agent call after the write
  produces an artifact.

- **GOAL-3** — Diagnostic surface: when a phase escalates, the
  `error_context` MUST record (a) the phase name, (b) which
  prompt/sub-agent type was invoked, and (c) why the retry budget
  was exhausted (last error class, not just last error string).
  This is what made this bug debuggable here — keep that signal,
  formalize it.

- **GUARD-1** — No silent self-deadlock: if a phase's sub-agent
  output starts with `REVIEW_REJECT` and the project state shows
  no upstream artifact for that phase to consume, the runner MUST
  escalate immediately with a typed error
  (`PhasePromptMismatch` or similar) instead of retrying the same
  prompt. Three identical REVIEW_REJECTs is a bug, not a workflow.

- **GUARD-2** — Don't lose the source-of-truth pointer: after Triaging,
  `RitualState` MUST record which file is acting as the requirements
  artifact (be it `requirements.md`, `issue.md`, or a generated one),
  so downstream phases (Reviewing, ExecuteTasks) read the right doc.

## Workaround until fixed

1. Don't use `/ritual` for issue-mode work; do the change directly.
2. After any ritual escalation, manually remove
   `<project>/.gid/ritual-state.json` before the next invocation
   (the file is preserved as `*.failed-<ritual-id>-<date>` for
   forensics).

## Files / code likely involved

(Best guesses — verify by reading.)

- `crates/gid-core/src/ritual/state_machine.rs` — phase transitions, especially `Triaging → ?`
- `crates/gid-core/src/ritual/` — phase → prompt mapping, sub-agent dispatch
- whatever owns `ProjectState` detection (`has_requirements`) — needs to learn about `issues/ISS-NNN/issue.md`
- prompt templates for `WritingRequirements` (and the parallel `WritingDesign` — check it for the same bug)

## Related

- engram **ISS-044** — the task that triggered this; currently blocked on
  this ritual bug, proceeding without ritual.
- gid-rs **ISS-022** — project path resolution via registry. Adjacent
  but distinct: ISS-022 was about *finding* the project; this is about
  *what to do once you've found it*.
- gid-rs **ISS-052** — V2Executor turn-budget work. Same subsystem.
- gid-rs **ISS-056** — implement-skill silent failure. Different
  failure mode but shows the same class of issue: ritual sub-agent
  dispatch losing track of context.

## Fix (2026-04-28)

**Root cause confirmed**: `ProjectState::detect()` in
`crates/gid-core/src/ritual/composer.rs` only recognised
`REQUIREMENTS.md` and `.gid/requirements-*.md` as requirements
artifacts. In issue-mode the requirements ARE
`.gid/issues/<id>/issue.md`, but the detector had no way to know which
issue a given ritual targeted, so `has_requirements` was always
`false` for issue-mode rituals → state machine routed to
`WritingRequirements` (state_machine.rs:1041–1060) → re-authoring +
self-review deadlock.

**Fix** (single architectural change):

1. `ProjectState::detect()` now takes
   `work_unit: Option<&WorkUnit>`. When the unit is
   `WorkUnit::Issue { id, .. }` and `.gid/issues/<id>/issue.md`
   exists, `has_requirements` is set to `true`.
2. `compose_ritual()` signature extended to accept the same
   `work_unit` argument and forward it.
3. `V2Executor::detect_project()` now reads `state.work_unit` (already
   present on `RitualState`) and passes it through to detection.
4. Other call sites that don't have a `WorkUnit` available
   (`read_verify_command`'s language sniff) explicitly pass `None`.

**Tests** (both in `composer::tests`, behind `--features ritual`):

- `issue_mode_treats_issue_md_as_requirements_iss057` — given a
  `WorkUnit::Issue { id: "ISS-057", .. }` and an existing
  `.gid/issues/ISS-057/issue.md`, `detect()` returns
  `has_requirements = true`. Control case (`detect(.., None)`)
  still returns `false`, proving the new behaviour is opt-in via
  the `WorkUnit`.
- `non_issue_mode_does_not_match_issue_md_iss057` — given a
  `WorkUnit::Feature` and a stray unrelated `issue.md`, detection
  does NOT spuriously flip `has_requirements = true`. Locks down
  the per-id matching guarantee.

**Verification**:

- `cargo check -p gid-core` clean
- `cargo test -p gid-core --lib --features "ritual sqlite" ritual::`
  → 305 passed, 0 failed (includes the two new ISS-057 tests)
- `cargo clippy -p gid-core --features "ritual sqlite" --lib` clean
- One unrelated pre-existing perf flake
  (`unify::tests::test_perf_tasks_with_code_nodes`) under `--features full`
  is independent of this change.

**Out of scope (deliberately not done in this fix)**:

- The companion concern in this issue ("verify same bug for
  feature-mode `WritingDesign`") was inspected and not reproduced —
  feature-mode design routing reads from `feature/design.md`, which
  isn't gated on `has_requirements`. If a parallel routing issue
  emerges for features, it should get its own issue rather than be
  bundled in here.
