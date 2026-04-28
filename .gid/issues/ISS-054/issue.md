---
id: "ISS-054"
title: "ISS-052 design — deferred refinements (9 follow-up findings)"
status: closed
priority: P2
created: 2026-04-27
closed: 2026-04-28
severity: low
related: ["ISS-052"]
---

## Resolution (2026-04-28)

All 7 actionable deferred findings applied to `.gid/issues/ISS-052/design.md` in
a single doc-polish pass:

- **FINDING-11** — §10.6 added: cross-repo issue closeout table mapping
  ISS-052 / ISS-038 / ISS-039 / ISS-051 closure status to this PR. Captures
  why ISS-051 closes "main fix only" with T08 wiring deferred.
- **FINDING-13** — §4 `should_cancel` doc extended: documents the planned
  `should_cancel_during(&action)` follow-up shape and why it's not added in
  this PR (additive, no urgency until the up-to-N-min latency surfaces).
- **FINDING-15** — §6.2 retry notify: replaced free-standing `notify` with a
  `verbose_retries: bool` config flag (default `false`). Spec-grade comment
  explains the spam reduction and points at the single-summary path that
  takes over when the flag is `false`.
- **FINDING-16** — §12 AC3 split into AC3a (regex `match\s+(\&?\w+\.)?action\b`
  excluding comment lines) + AC3b (`wc -l` ≤ 800). Both must hold; rationale
  for the AND included inline.
- **FINDING-18** — §8.4 wording: "subloop is now in V2Executor" → "ported
  into V2Executor (rewritten + adapted + extended with the new turn-limit
  gate; not a relocation of an existing identical function)". Also points at
  §7.4 commit ordering 3b/3c reflecting the port-then-light-gate sequence.
- **FINDING-19** — §10.5 rollback window: `cargo publish` is the gate;
  manual acceptance + AC5 zero-file regression test must pass on the
  path-dep build before publish. After publish, rollback = `yank` + `.1`.
- **FINDING-20** — §6.3.3.a worked Rust pseudo-code for the
  `StatePersistFailed` arm (Periodic increment / reset, Boundary abort).
  Removes ambiguity between the table's "increment" and "terminate at 5"
  rows; calls out that boundary failures never touch `persist_degraded`.

Two additional minor items from the review summary that were already
addressed elsewhere:

- **Trait coherence between `RitualHooks` defaults and tests** — covered by
  FINDING-12 (already applied in r1 → in-place edits before this issue was
  filed; no further work).
- **`RitualEvent` enum naming clash** — FINDING-3 disambiguates inline. A
  rename of either enum is a separate, larger refactor and **explicitly
  not in scope** for ISS-054 (not pursued).

Net design.md change: ~165 lines added across 7 sections (§4, §6.2, §6.3,
§8.4, §10.5, §10.6, §12 AC3). No code change. No behavioral change. Pure
clarity / precision uplift on the in-PR-archive design document.

---


# ISS-054 — ISS-052 design refinements (deferred from r1 review)

**Status:** open
**Type:** design polish — non-blocking refinements deferred from ISS-052 design review
**Severity:** low (none block implementation; all are quality-of-life / wording / minor scope)
**Discovered:** 2026-04-27 — design review of `.gid/issues/ISS-052/design.md` (r1) produced 20 findings. The 11 critical + important findings were applied in-place to design.md (lines 966 → 1212). The remaining 9 are tracked here so the implementation PR for ISS-052 isn't held up while still preserving the review's full output.

## Why a separate issue

ISS-052's design is now at the "good enough to start coding" bar. The deferred findings fall into three buckets:

1. **Wording / framing fixes** — design says "X" when it should say "X+Y". No behavior change. (FINDING-18, partial 11.)
2. **Acceptance-criteria tightening** — current checks could be gamed; tighter regex / extra LOC count. (FINDING-16.)
3. **Polish during release / implementation** — rollback notes, notify-spam reduction, worked pseudo-code. (FINDING-15, 19, 20.)
4. **Cross-repo coordination** — issue closeout plan across the 4 related issues. (FINDING-11.)

None of these is "the design is wrong"; they're "the design could be more precise / less foot-gunny / clearer to the next person." Ideal time to apply: during ISS-052 implementation when each section gets touched anyway, OR as a single docs PR after merge.

## Deferred findings

### FINDING-11 🟡 Cross-repo issue closeout plan missing
- **Where in design:** §10
- **What's missing:** ISS-052 closes when this design lands. ISS-038 / ISS-039 / ISS-051 each have a partial relationship — some close with ISS-052, some are subsumed, some remain open. No table says which.
- **Suggested fix:** Add §10.6 with a table:
  - ISS-052 → closed by this PR
  - ISS-038 → closed (gate now lives in V2Executor, applies to all rituals)
  - ISS-039 → closed (subloop now in V2Executor)
  - ISS-051 → closed (state machine now has `persist_degraded` side-channel)
- **Why deferred:** Easy fix; can be added when the implementation PR description is written.

### FINDING-13 🟡 (partially applied) `should_cancel` mid-action latency
- **Status:** Documented in §4 as accepted trade-off (up-to-10-min latency). Not added: optional richer `should_cancel_during(&action)` hook for in-skill polling.
- **Why deferred:** Documenting the trade-off is enough for now. If real users hit it, add the richer hook in a follow-up.

### FINDING-15 🟢 Retry-attempt `notify` spam
- **Where in design:** §6.2, §8.2
- **Issue:** 3 retry attempts → 2 ⚠️ Telegram messages within ~1.3s for transient FS errors.
- **Suggested fix:** Only `notify` after final outcome, with attempt count in the summary. (Implementation note: easy — wrap the existing per-attempt notify behind a `verbose_retries: bool` flag in `V2ExecutorConfig`, default false.)
- **Why deferred:** UX polish, not correctness. Apply during implementation or as a fast-follow.

### FINDING-16 🟢 AC3 grep regex too narrow
- **Where in design:** §12 AC3
- **Issue:** Current AC3: `grep -rn "match action" rustclaw/src/`. False positives (string literals / comments), false negatives (`match &action`, `match dispatch.action`).
- **Suggested fix:** AND-combine two checks:
  - `grep -rnE "match\s+(\&?\w+\.)?action\b" rustclaw/src/ | grep -v "//\|/\*"` returns zero
  - `wc -l rustclaw/src/ritual_runner.rs` ≤ 800 (post-deletion target)
- **Why deferred:** Tighten when AC3 is actually run during verification.

### FINDING-18 🟡 §8.4 wording: "subloop is now in V2Executor" → it's a port, not a relocation
- **Where in design:** §8.4
- **Issue:** Implies relocating an existing function, but the subloop currently lives in rustclaw and must be ported (rewrite + adapt + add new gate). Scope is "port + fix," not "fix."
- **Suggested fix:** Reword §8.4. Add explicit "port self-review subloop into V2Executor" task to §7.4 commit ordering (part of commit 3).
- **Why deferred:** Wording correctness; doesn't change technical scope but does affect estimation.

### FINDING-19 🟢 Rollback window closes at `cargo publish`
- **Where in design:** §10.5
- **Issue:** Q5 rejected feature flag. Without one, rollback is "yank from crates.io" once published — possible but disruptive.
- **Suggested fix:** Add to §10.5: "Manual acceptance §9.5 + zero-file regression test (AC5) MUST pass on path-dep build before `cargo publish`. After publish, rollback requires a yank + version bump."
- **Why deferred:** One-line addition during release prep.

### FINDING-20 🟢 Worked pseudo-code for consecutive-failure counter missing
- **Where in design:** §6.3
- **Issue:** §6.3.3 table now describes counter behavior (increment on `StatePersistFailed`, reset on `StatePersisted`, terminate at 5). But no worked Rust-shaped pseudo-code example.
- **Suggested fix:** Add a 10–15 line `match event` snippet to §6.3 showing the counter increment / reset / terminate paths inline.
- **Why deferred:** The behavior is unambiguous from the table; the example is nice-to-have. Add when implementing §6.3.

### Additional minor findings retained from the review summary
The review listed three additional minor items that were never assigned a FINDING-N because they overlapped with the above. They are preserved here for completeness:

- Trait coherence between `RitualHooks` defaults and tests — covered by FINDING-12 (already applied).
- `RitualEvent` enum naming clash — FINDING-3 disambiguates inline; renaming one of the enums is a separate, larger refactor and not urgent.
- `V2ExecutorConfig` default values not specified — implementation detail, falls out naturally from `Default` impl.

## Acceptance

This issue closes when:

- [ ] FINDING-11 §10.6 closeout table added to ISS-052 design (or to PR description if design.md is frozen by then)
- [ ] FINDING-15 retry-spam reduced (single summary notify on final outcome)
- [ ] FINDING-16 AC3 tightened with combined grep + LOC check
- [ ] FINDING-18 §8.4 wording corrected; commit-3 task list updated
- [ ] FINDING-19 §10.5 pre-publish gate explicitly stated
- [ ] FINDING-20 §6.3 pseudo-code snippet added

All six are independent and can be done as one small docs PR or folded into the ISS-052 implementation PR commit-by-commit.

## Out of scope

- Renaming `state_machine::RitualEvent` ↔ `notifier::RitualEvent` (separate refactor; FINDING-3 disambiguates inline, that's enough).
- Adding `should_cancel_during(&action)` hook (FINDING-13 b-option). Defer until real-world cancel UX complaints arrive.

## Reference

- Source review: `.gid/issues/ISS-052/reviews/design-r1.md`
- Applied findings (11): see "Applied" section in same review file
- Current design: `.gid/issues/ISS-052/design.md` (1212 lines after applied changes)
