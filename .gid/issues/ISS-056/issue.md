---
id: "ISS-056"
title: "implement skill agent silently fails on medium/large refactors (turn limit + tool gating + workspace scope)"
status: closed
priority: P1
created: 2026-04-27
closed: 2026-04-28
severity: medium
related: ["ISS-052", "ISS-055"]
---

## Resolution (2026-04-28)

Layers 1 and 2 implemented and tested; Layer 3 explicitly out of scope per
issue body and not pursued.

- **Layer 1 — triage→implement budget** (commit `f8cc32c` — `feat: ISS-056a triage-aware turn budget`):
  `V2Executor::implement_iterations_for_triage_size` maps small=15 / medium=30 /
  large=50; medium is also the default for `None`/unknown. Other phases keep the
  100 fallback. 2 unit tests cover small/medium/large/None/unknown.

- **Layer 2 — STATUS self-report + ImplementIncomplete → Paused** (commit `bd67e68` —
  `feat: ISS-056b implement-skill STATUS self-report`):
  - `RitualPhase::Paused` added; distinct from `Escalated` (escalation) and
    `Cancelled` (user abort) — work is unfinished but not failed.
  - `RitualEvent::ImplementIncomplete { reason }` + `(Implementing, ImplementIncomplete) → Paused`
    arm. Verify shell is **not** run on this path (the silent-success leak
    closes here).
  - `prompts/implement.txt` appends the `STATUS: complete` / `STATUS: incomplete: <reason>`
    contract; recognised reasons cover the four documented failure modes
    (turn limit / tool gating / unclear spec / blocked dependency).
  - `parse_implement_status()` parses the trailing STATUS line out of the last
    100 non-empty output lines (case-insensitive, markdown-stripped). Missing
    STATUS treated as incomplete (fail-closed).
  - Tests: `test_implement_incomplete_pauses_without_verify` in `state_machine.rs`
    pins the no-verify invariant; `test_parse_implement_status_*` cover parser
    happy paths and the missing-status case.
  - `is_paused()` updated to include `Paused` so existing terminal/paused
    accounting in `drive_event_loop` and `health()` continues to hold.

ACs 1–4 met. AC5 (`docs/ritual/v2-pipeline.md` describing the contract) deferred
to ISS-052 T17 release docs / general v2 pipeline doc refresh — the contract
itself lives in `prompts/implement.txt` and the state machine, both of which
are first-class and discoverable from the issue.

Discovery context: rustclaw ISS-052 T13b silent-success incident
(2026-04-27). T13b itself was retried in main session post-fix and landed via
direct commits — see rustclaw `tasks/2026-04-27-night-autopilot.md`.

---

# ISS-056 — implement skill agent silently fails on medium/large refactors

## Problem

Ritual `r-e196af` (rustclaw ISS-052 T13b: migrate 17 dispatcher call sites + delete embedder dispatcher) ran the full V2 pipeline (Designing → Implementing → Verifying → Done) and reported success after ~12 minutes, but **the actual code change was 0%**. No call sites migrated, no dispatcher deleted, only the `Cargo.toml` path edit that had been done manually beforehand.

The Implementing phase silently masked the failure. Three independent contributing causes:

### Cause A: 20-turn agent budget vs medium-refactor scope

`RitualLlmAdapter::run_skill("implement")` runs a sub-agent with **20-turn tool loop**. T13b required:
- Reading 6+ Telegram handler files (~6 turns of `read_file`)
- Reading the existing dispatcher in `crates/openclaw-ritual/src/dispatcher.rs` (~1 turn)
- Reading gid-core's `resume_ritual` signature (~1 turn)
- Editing 6 call sites with the new API (~6 turns of `edit_file`)
- Deleting/shrinking the local dispatcher (~2 turns)
- Running `cargo check` to verify (~1 turn)
- Iterating on compile errors (~3+ turns)

Realistic budget: **~20 turns just for the happy path**, zero margin for error. Triage classified T13b as `large` (3 files × 50+ lines each, cross-crate). Triage size feeds review-skill `max_iterations` but **does not feed implement-skill turn budget**. The implement skill is hardcoded to 20 turns regardless of triage classification.

Result: agent ran out of turns mid-refactor, returned `SkillCompleted { artifacts: [] }` (or with stale partial artifacts), and Verifying phase had no signal that work was incomplete.

### Cause B: Verifying phase shell command is too shallow

`V2Executor::verify_implementation` runs `cd rustclaw && cargo test --lib 2>&1 | head -100`. With Cargo.toml pointing at local `path = "../gid-rs/crates/gid-core"` but source unchanged, **`cargo build` succeeds** because the type signatures of public API are still satisfied by the *old* call sites (no new code calls `resume_ritual`). `cargo test --lib` then runs the existing test suite, which passes (it tests old behavior, not the migration).

The verify phase has no notion of "did the implement skill actually accomplish what its skill prompt asked?" It only checks "does the code still compile and pass existing tests?" For an additive feature this is fine; for a **migration** (where success means *removing* old code paths), passing tests on the unchanged code is a false-positive.

### Cause C: Cross-workspace tool gating is opaque to the agent

T13b touched files in BOTH `rustclaw/` (handler call sites) and indirectly verified via `gid-rs/` (the new `resume_ritual` it was meant to call). The implement-skill agent ran with rustclaw as its workspace. It could read gid-rs source via absolute paths, but tool gating for `edit_file` may have rejected writes outside the ritual's declared writable paths.

The agent received tool errors but had no clear way to escalate ("I cannot complete this task because writes to X are blocked"). It just kept trying and burned turns.

## Reproduction

1. Start a ritual on a task triaged as `large` that requires editing >5 files across >1 crate.
2. Run V2 pipeline to completion.
3. Observe: ritual reports Done, but `git diff` shows only trivial changes.
4. Re-read implement-skill artifacts in ritual state — usually empty or contains a note like "started reading files, ran out of turns".

## Impact

- **Trust**: ritual outcomes cannot be trusted for refactors ≥medium. User must always re-verify with `git diff`.
- **Hidden cost**: each false-positive ritual burns ~10–15 minutes wall-clock + ~50–80k tokens before the user discovers nothing was done.
- **Process drift**: encourages users to bypass ritual for "real" work and only use it for trivial tasks, which defeats its purpose.

## Proposed Fix (two layers)

### Layer 1: Triage → implement-skill budget (small change)

Plumb triage size into implement-skill turn budget:

| triage | turn budget |
|---|---|
| small | 15 |
| medium | 30 |
| large | 50 |

This mirrors the existing review-skill triage→iteration mapping. Code change: `V2Executor::implement_phase` reads `state.triage.size` and passes `max_iterations` to `RitualLlmAdapter::run_skill`.

### Layer 2: Implement-skill must report incomplete work explicitly (medium change)

Update `skills/implement/SKILL.md` to require the agent to end its run with one of:
- `STATUS: complete` + summary of what was done
- `STATUS: incomplete` + list of what was *not* done + reason (turn limit / tool gating / unclear spec / blocked dependency)

`V2Executor::verify_implementation` reads this status and:
- `complete` → run cargo verification as today
- `incomplete` → emit `ImplementIncomplete` event, transition to `paused` state with the reason, do NOT run verify, do NOT mark Done

This blocks the silent-success failure mode at the boundary where it currently leaks.

### Layer 3 (optional, larger): Migration-aware verify

For ritual phases marked as "migration" or "refactor" in the design doc, verify should additionally check that intended deletions/replacements actually happened (e.g., grep-based assertions: "old API X no longer called" / "function Y deleted"). Out of scope for this issue — file as follow-up if Layer 1+2 prove insufficient.

## Acceptance Criteria

- [ ] Triage size feeds implement-skill turn budget (small=15, medium=30, large=50).
- [ ] Implement skill prompt requires `STATUS: complete | incomplete` self-report.
- [ ] V2Executor pauses ritual on `STATUS: incomplete` instead of proceeding to verify.
- [ ] Regression test: a deliberately-truncated implement run (e.g., max_iterations=2 on a real task) results in `paused` state, not `done`.
- [ ] Documentation: `docs/ritual/v2-pipeline.md` (or equivalent) describes the incomplete-status contract.

## Notes

- This issue is independent of ISS-055 (resume_ritual). ISS-055 fixed the *embedder* side (one canonical dispatcher); this issue fixes the *gid-core internal* implement-skill loop.
- Discovered during rustclaw ISS-052 T13b execution attempt 2026-04-27.
- T13b itself is being completed in main session (not via ritual) — see rustclaw memory log for the same date.
