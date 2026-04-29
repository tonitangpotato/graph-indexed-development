---
id: ISS-062
title: Self-review subloop drops artifact paths from prompt — sub-agent cannot find files to review
status: closed
priority: P1
labels:
- ritual
- v2-executor
- sub-agent
- bug
- prompt
relates_to:
- ISS-057
- ISS-051
- ISS-038
- ISS-052
created: 2026-04-29
resolved: 2026-04-29
resolved_by: v2_executor self-review artifact plumbing fix
---

# ISS-062: Self-review subloop drops artifact paths from prompt

## Summary

When a phase completes (e.g. `draft-requirements`, `draft-design`, `implement`)
and the V2 executor runs its self-review subloop, the review sub-agent is
prompted with `"Read back ALL files you created or modified in the previous
step"` — but **the prompt does not include the actual paths**. The sub-agent's
ToolScope (Read/Write/Edit only — no `list_dir`, no `exec`) leaves it with no
way to discover what was produced. It either gives up with `REVIEW_REJECT`
("I cannot find any artifacts from a previous step") or fabricates paths and
fails to find them. Either way, the entire phase is rejected for a reason
unrelated to the work's actual quality.

## Reproduction (real, observed 2026-04-29)

1. rustclaw, `start_ritual` for ISS-060 (rustclaw issue), work_unit
   `{"id": "ISS-060", "kind": "issue", "project": "rustclaw"}`.
2. `requirements.md` and `design.md` already exist under
   `.gid/features/iss-060-context-pressure/` (created by prior runs, post
   ISS-057 fix the executor correctly skips `WritingRequirements`).
3. Executor advances to a review-eligible phase. The review sub-agent is
   spawned with the standard self-review prompt.
4. Sub-agent reply (verbatim):

   > I genuinely have no way to discover what was created in the previous
   > step without a directory-listing tool, and there's no stated task in
   > my context to anchor on. … I checked likely paths (DESIGN.md, PLAN.md,
   > STAGING.md, CURRENT_TASK.md, .ritual/state.json, README.md, SOUL.md)
   > — only pre-existing repo files exist, none of which appear to be
   > products of "the previous step." Without knowing what was created
   > and without a way to enumerate the working tree, I cannot perform a
   > meaningful self-review.
   > REVIEW_REJECT

5. Ritual escalates with no actual review performed.

## Root Cause

In `crates/gid-core/src/ritual/v2_executor.rs`:

- The phase runner already has the artifact list. After a skill succeeds, it
  computes an `FsDiff` of the mutation root (lines ~762–768) and converts it
  to `Vec<String>` via `artifact_strings(&diff)` (line ~824). This list is
  attached to the `RitualEvent::SkillCompleted { artifacts, .. }` event.

- But the review path **discards** it:

  ```
  let event = self.maybe_run_self_review(name, state, completed).await;
  ```

  `maybe_run_self_review` only forwards `name` + `state` to
  `run_self_review_subloop`, which in turn calls
  `build_self_review_prompt(skill_name, turn, max_turns)` — no artifacts
  parameter exists.

- `build_self_review_prompt` (lines ~1822–1860) emits a generic
  *"Read back ALL files you created or modified in the previous step"*
  with no paths. The sub-agent's ToolScope strips `list_dir` / shell, so
  it has no way to recover them.

This is the same family of bug as ISS-057 (executor not seeing prior
artifacts) — but one layer down. ISS-057 fixed the **executor's** view of
prior artifacts; ISS-062 is about the **review sub-agent's** view. Both
exist because phase boundaries lose state that was already computed.

## Fix Direction

Thread the artifact list end-to-end. Pure data plumbing, no new state, no
new IO:

1. `build_self_review_prompt(skill_name, turn, max_turns, artifacts: &[String])`
   — accept the list, render it into the prompt as an explicit bullet list
   under a `## Artifacts to review` heading, before the verdict instructions.
2. `run_self_review_subloop(name, state, artifacts, llm)` — accept and
   forward.
3. `maybe_run_self_review(name, state, completed)` — extract `artifacts`
   from the `RitualEvent::SkillCompleted` payload it already receives,
   pass them through.
4. Update the three existing tests of `build_self_review_prompt` (lines
   4270, 4281, 4284) to pass an artifact slice. Add one new test asserting
   the prompt contains every supplied path verbatim.
5. Add an integration test: scripted LLM run, fake skill that writes two
   files, assert the review prompt the LLM receives mentions both paths.

When the artifact list is empty (legitimate no-op success — rare, since
file_policy=required already rejects empty diffs upstream — but possible
for `optional` skills), render *"The previous step did not create or
modify any files; review the conversation context instead."* rather than
silently emitting an empty section.

## Severity

P1. Currently every review-eligible phase that hits self-review is at the
mercy of the sub-agent's ability to guess paths. ISS-060 (rustclaw issue)
is blocked behind this — the design is approved, the implementation is
shovel-ready, but the ritual cannot complete a review pass. Manual
fall-through is the only escape valve.

## Out of Scope

- ISS-051 (ritual state not persisted across restarts) — separate concern,
  this fix doesn't address it but doesn't conflict either.
- Adding `list_dir` / shell to the review sub-agent's ToolScope — wrong fix.
  Reviewers should be told what to review, not asked to discover it.
- Cross-phase artifact propagation beyond review (i.e. carrying the
  artifact list further through the state machine for later phases) —
  ISS-057 is the right place; this issue is scoped strictly to the
  review subloop.

## Verification

After fix, re-run the ISS-060 reproduction above. The review sub-agent
should receive a prompt containing
`.gid/features/iss-060-context-pressure/requirements.md` (or whichever
files the prior phase produced), open them with Read, and emit a
substantive `REVIEW_PASS` or `REVIEW_REJECT` based on actual content.

## Resolution (2026-04-29)

Fixed in `crates/gid-core/src/ritual/v2_executor.rs` — 92 insertions, 10
deletions. Implementation matches the design described above:

1. **`build_self_review_prompt`** now accepts `artifacts: &[String]` and
   emits a `## Artifacts to review` section listing every path. When the
   list is empty (defensive — should not happen for eligible phases), it
   emits an explicit warning so the sub-agent doesn't hallucinate paths.
2. **`run_self_review_subloop`** signature gained the same parameter and
   threads it through to the prompt builder on every turn.
3. **`maybe_run_self_review`** extracts `artifacts` from the
   `RitualEvent::SkillCompleted` it wraps and passes them down.
4. Two new unit tests added:
   - `build_self_review_prompt_lists_artifacts_when_supplied` — asserts
     each path appears in the rendered prompt.
   - `build_self_review_prompt_handles_empty_artifacts` — asserts the
     warning sentinel is emitted (and no fake paths leak in).
5. Existing test `build_self_review_prompt_contains_verdict_instructions`
   updated to call the new signature with an empty slice.

Test run: `cargo test -p gid-core --lib --features ritual,sqlite` →
**1276 passed, 0 failed**. All 7 self-review-related tests pass:

```
build_self_review_prompt_contains_verdict_instructions ... ok
build_self_review_prompt_handles_empty_artifacts ... ok
build_self_review_prompt_lists_artifacts_when_supplied ... ok
is_self_review_eligible_matches_rustclaw_preport_list ... ok
self_review_subloop_accepts_on_first_turn ... ok
self_review_subloop_rejects_on_review_reject ... ok
self_review_subloop_skipped_for_non_eligible_phase ... ok
```

Follow-up tracked in **ISS-063** (ritual phantom-state — verify outputs
on disk + state-file persistence) — that issue catches the broader class
of "phase reports success without producing artifacts", of which this
prompt bug was one symptom.
