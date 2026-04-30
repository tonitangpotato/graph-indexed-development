---
id: ISS-066
title: Phantom-state root fix — file_policy/expected_artifacts default to permissive, phase skills can pass-through with zero output
status: open
kind: issue
priority: P1
labels:
  - ritual
  - v2-executor
  - root-fix
  - design
created: 2026-04-30
related:
  - ISS-025
  - ISS-038
  - ISS-051
  - ISS-063
filed_by: rustclaw (ISS-064 root-cause investigation)
---

# ISS-066: Phantom-state root fix — gate defaults are permissive

**Filed**: 2026-04-30
**Severity**: P1 (any phase skill missing both gates can claim success while writing nothing)
**Discovered while**: investigating rustclaw ISS-064 (Designing/Reviewing phases "complete" but write nothing). After eliminating ISS-051 (RitualRunner bypass — closed) and ISS-052 (artifact↔graph sync — irrelevant) as causes, the actual root cause turned out to be in V2Executor's gate defaults, not in any dispatcher.

## Symptom (in rustclaw)

A ritual phase (e.g. Designing) emits `SkillCompleted { artifacts: [] }` even when:

- The LLM made zero `Write`/`Edit` tool calls.
- `git diff` shows zero changes.
- The phase was contractually expected to produce a design document.

This is the same class of failure as ISS-025 / ISS-038 / ISS-051. ISS-051 fixed the dispatcher path. This issue fixes the **gate defaults** that still allow phantom-state when a skill author forgets to declare them.

## Root cause analysis

V2Executor has two independent gates that should detect "skill returned Ok but wrote nothing":

### Gate 1: `file_policy:` (file_snapshot post-condition)
- Declared in SKILL.md frontmatter. Values: `required` / `forbidden` / `optional`.
- **Default when absent: `Optional`** — `state_machine.rs:872-880`.
- When `Optional`, `v2_executor.rs:716` skips the pre-snapshot, so no diff is computed; 0 files written passes silently.

### Gate 2: `expected_artifacts:` (ISS-063 Phase B contract)
- Declared in SKILL.md frontmatter. List of artifact path templates.
- **Default when absent: pass-through** — `v2_executor.rs:898` "Skills without `expected_artifacts:` are pre-contract / legacy".
- Contract verification skipped entirely; 0 produced artifacts passes silently.

### Why both defaults are permissive

The defaults were chosen for backwards compatibility — pre-existing skills shouldn't break when a new gate lands. That's the right choice for an incremental rollout; it's the wrong choice as a long-term default. The result: any phase skill whose author forgot to declare both gates will pass-through silently, and the user has no way to detect this from the ritual status output.

### Audit (rustclaw skills, 2026-04-30)

5 of 5 phase-producing skills had at least one gate missing or wrong:

| Skill                | `file_policy:`         | `expected_artifacts:` |
| -------------------- | ---------------------- | --------------------- |
| draft-design         | missing → Optional     | missing               |
| draft-requirements   | missing → Optional     | missing               |
| review-design        | `forbidden` (wrong)    | missing               |
| review-requirements  | `forbidden` (wrong)    | missing               |
| review-tasks         | `forbidden` (wrong)    | missing               |

5/5. The opt-in defaults are not robust enough.

## Proposed fix (sketch — open to design discussion)

### Option A: phase-aware default (preferred)
The ritual definition (`gid-core/src/ritual/definition.rs`) knows which phases are contractually expected to produce artifacts (Designing, Reviewing, Implementing). Make the V2Executor look up the phase contract first, and only fall back to skill-declared `file_policy:` / `expected_artifacts:` when the phase contract is silent.

This means: for Designing/Reviewing/Implementing, `file_policy` defaults to `Required` regardless of whether the SKILL.md declares it. A skill author can still override with `file_policy: forbidden` (e.g. read-only research skill mid-ritual) but the override is explicit.

### Option B: warn-loud, fail-soft
At minimum, if a skill runs in a "produce artifacts" phase with both gates absent, emit a `WARN`-level log and surface the warning in the ritual completion message. Doesn't block phantom-state but makes it visible.

### Option C: deprecation cycle
Today: skills missing both gates → `WARN` + completion-message banner.
+1 release: missing both gates → `ERROR` + ritual fails.
This is breaking — every legacy skill must declare gates — so a deprecation window matters.

A combination of A (phase-aware default) + C (deprecation banner during the change) is probably right. Worth designing on its own.

## Why P1, not P0

Workaround exists (skill authors can declare gates explicitly — rustclaw applied this in ISS-064 Phase A, 2026-04-30). But every new ritual project starts from scratch with the same trap, and skill authors will forget. P1 = root fix needed but not on fire.

## Acceptance

- Phase-aware default lands (Option A or equivalent).
- A skill that doesn't declare `file_policy:` is treated as `Required` when invoked in Designing/Reviewing/Implementing.
- Existing skills that explicitly declare a different policy continue to honor the declaration.
- Test: ritual run with a skill that wrote zero files in Designing → SkillFailed (not SkillCompleted).
- Documentation updated (SKILL.md author guide reflects the new default).

## References

- rustclaw ISS-064 — symptom report + Phase A skill patches (workaround).
- ISS-051 (this repo) — sibling root-cause fix; that fixed dispatcher bypass, this fixes gate defaults.
- ISS-063 — added `expected_artifacts:` contract (Phase B).
- ISS-025 / ISS-038 — original phantom-state work; established the file_snapshot mechanism but left gate defaults permissive.
