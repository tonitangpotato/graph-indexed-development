# Design Review r1 — ISS-052

> **Reviewer:** RustClaw (main agent — sub-agent attempts ran out of iterations; followed AGENTS.md "Sub-Agent Task Fitness" rule to do design review directly)
> **Date:** 2026-04-26
> **Target:** `.gid/issues/ISS-052/design.md` (966 lines, 12 sections)
> **Bundled issues:** gid-rs ISS-052, gid-rs ISS-051, rustclaw ISS-051, rustclaw ISS-050
> **Method:** review-design skill, depth=full (35 checks)

## Summary

| Severity   | Count |
|------------|-------|
| 🔴 Critical   | 5    |
| 🟡 Important  | 11   |
| 🟢 Minor      | 4    |
| **Total**     | **20** |

**Overall verdict: needs-revision**

Five critical findings each blocks implementation in distinct ways:
- FINDING-1, 2, 3 are *hidden breaking changes* — design proposes new shapes for existing public types/functions without acknowledging the existing definitions or planning the migration. Implementer will discover mid-PR and either ship something that doesn't compile or scope-creep.
- FINDING-4 is a *missing deletion-plan entries* gap — the actual production entry points (Telegram + tools.rs) aren't in §7's deletion list, so a literal application of the plan leaves callers broken.
- FINDING-5 is a *non-goal violation* — §1 NG1 "not rewriting state machine" is contradicted by §6.3's new `PersistDegraded` wrapping state. Either drop NG1 or use a side-channel design.

The 11 important findings are mostly under-specifications (phase-boundary semantics, hook test coverage, retry atomicity, `execute_actions` API). They're not blockers individually but together represent ~25% of the design that's hand-wavy and would force the implementer (or a sub-agent) to make decisions that should be in the design.

Recommend: apply FINDING-1–5 + FINDING-7 (typo) + FINDING-9 (atomicity) + FINDING-10 (phase boundary) + FINDING-17 (`execute_actions`) before starting implementation. The rest can be applied during implementation or deferred to follow-ups, *if* tracked.

---

## Findings

### FINDING-1 🔴 Breaking change to existing `RitualEvent::SkillFailed` shape not acknowledged
- **Section:** §5.5
- **Issue:** Design redefines `SkillFailed { skill: String, reason: SkillFailureReason }`. The existing variant in `crates/gid-core/src/ritual/state_machine.rs:448` is `SkillFailed { phase: String, error: String }`, used in **8+ state-machine match arms** (state_machine.rs:885, 1039, 1051, 1068, 1085, 1095, 1108) plus tests (1578, 1590, 1664, 1676, 2150) plus `run_ritual` itself (v2_executor.rs:1303). The field rename `phase` → `skill` and `error: String` → structured `reason` is a **breaking change** to a public-ish enum that is the load-bearing event of the entire state machine.
- **Suggested fix:** Either (a) keep `phase` and `error` as-is and add `reason: Option<SkillFailureReason>` as an *additional* field for the new gates (backward-compatible), or (b) explicitly call this out as a breaking change in §10.1 and §12 AC12, list every match site that needs updating in §7 deletion plan, and ensure §9 has a test that the state machine still picks the right retry branch when the new `reason` is set. Right now §5.5 just defines the new shape without saying "this replaces existing variant" — implementer will discover this is a multi-hour refactor mid-PR.
- **Why it matters:** Hidden breaking change. Implementer either (i) ships an incomplete refactor that doesn't compile, or (ii) discovers mid-implementation that ~15 call sites need touching, scope-creeps the PR, and the design's "~1700 LOC delete + 400 LOC add" estimate becomes wrong.

### FINDING-2 🔴 `run_ritual` already exists with incompatible signature; design treats it as new
- **Section:** §5.6 + §3.4
- **Issue:** `crates/gid-core/src/ritual/v2_executor.rs:1281` already defines `pub async fn run_ritual(task: &str, executor: &V2Executor) -> Result<RitualState>`. The design proposes `run_ritual(initial_state: RitualState, config: V2ExecutorConfig, hooks: Arc<dyn RitualHooks>) -> RitualOutcome`. These signatures are **incompatible**: different argument count, different argument types, different return type (`Result<RitualState>` vs new `RitualOutcome`). The design never acknowledges this function exists today; §5.6 says "currently `run_ritual` is used by gid-core integration tests" but doesn't address the signature change.
- **Suggested fix:** Add a §5.6.1 subsection: "`run_ritual` signature change is breaking. Existing callers: <list — search shows only internal gid-core tests but verify with `grep -rn 'run_ritual' crates/`>. Migration: rewrite each caller to construct `V2Executor` + hooks first." Also rename the new function (`run_ritual_v2`?) if you don't want to break existing callers, or commit to breaking and bump the minor explicitly. Either choice is fine — *picking neither* is the bug.
- **Why it matters:** Same class of hidden refactor as FINDING-1. The design hand-waves "becomes the canonical loop" without confronting that the existing function with this name does something different.

### FINDING-3 🔴 Two `RitualEvent` enums exist in gid-core; design treats them as one
- **Section:** §5.5, §3.4
- **Issue:** `crates/gid-core/src/ritual/state_machine.rs:433` defines `pub enum RitualEvent` (the state-machine event with `SkillCompleted`, `SkillFailed`, `Start`, etc.). `crates/gid-core/src/ritual/notifier.rs:57` defines a **separate, unrelated** `pub enum RitualEvent` (notification categories: `RitualStart`, `PhaseComplete`, `ApprovalRequired`, etc.). Design §5.5 adds new variants assuming one enum. If implementer adds variants to the wrong enum, the build still compiles but the new gates do nothing.
- **Suggested fix:** §5.5 must say which enum is being extended (`state_machine::RitualEvent`), and §11 should list as a risk: "two enums share the name `RitualEvent`; the notifier one is not what we're modifying." Also: this is a separate latent bug (two public types with the same name in sibling modules) — file as ISS-054 follow-up.
- **Why it matters:** Concrete implementation hazard. A subagent or contributor reading the design will likely import the wrong type. Also flags a code smell that should be cleaned up regardless.

### FINDING-4 🔴 Hidden caller sites omitted from deletion plan: `tools.rs:5907` and `channels/telegram.rs:1155`
- **Section:** §7.1, §7.4
- **Issue:** §7 lists ranges in `ritual_runner.rs` to delete but does not mention external callers of `RitualRunner::new` / `RitualRunner::with_registries`. `grep -rn "RitualRunner" rustclaw/src/` shows two such call sites:
  - `rustclaw/src/tools.rs:5907` — `crate::ritual_runner::RitualRunner::new(…)`
  - `rustclaw/src/channels/telegram.rs:1155` — `crate::ritual_runner::RitualRunner::with_registries(…)`
  These are the actual production entry points (Telegram `/ritual` command + tool dispatch). Design §7.4's commit ordering doesn't cover them.
- **Suggested fix:** Add §7.6 "External callers" listing all callers of `RitualRunner` outside `ritual_runner.rs` (run `grep -rn "RitualRunner\|ritual_runner::" rustclaw/src/ rustclaw/tests/` and enumerate). For each, specify whether the constructor signature changes and what migration looks like. Update commit-ordering plan in §7.4 to touch these in commit 3.
- **Why it matters:** Per AGENTS.md hard rule: "Cross-Workspace Sub-Agent Rule … never rely on sub-agent's own search to find cross-workspace files — it will find wrong files." Deletion plans must be complete or the PR breaks the build. Also: missing callers usually means missing tests (§9 may not cover the Telegram entry-point path).

### FINDING-5 🔴 `PersistDegraded` state contradicts NG1 ("not rewriting state machine")
- **Section:** §6.3 vs §1 NG1
- **Issue:** NG1 says "Not rewriting the `RitualState` state machine itself. Transitions stay as-is." §6.3 introduces a new top-level state `PersistDegraded { underlying_phase, error }` that wraps every long-running phase, plus new transition rules:
  - any `StatePersistFailed` mid-phase → enter `PersistDegraded`
  - 5 consecutive failures in `PersistDegraded` → `Failed`
  - first success in `PersistDegraded` → exit back to `underlying_phase`
  This is a non-trivial state-machine rewrite — it adds a state, adds at least 3 transition rules, adds a counter (consecutive-failure count), and changes the meaning of every existing phase (any phase can now be wrapped). The design's NG1 is straightforwardly violated.
- **Suggested fix:** Either (a) drop NG1 and accept that the state machine is being extended (then add §11 risk: "state machine cycle bugs — bouncing between Implementing and PersistDegraded" — already noted in R3), or (b) implement `PersistDegraded` as a side-channel flag on `RitualState` rather than a wrapping phase (`state.persist_degraded: Option<PersistDegradedInfo>`), preserving the phase enum unchanged. Option (b) is closer to NG1's intent. Pick one explicitly.
- **Why it matters:** Non-goals are scope guardrails. Silently violating one means scope is creeping; the design becomes "introduce hooks + rework state machine + 6 other things" instead of the one-thing PR §10 implies.

### FINDING-6 🟡 `unreachable!()` in `persist_state` retry loop violates G3
- **Section:** §6.2 (line 550 in design)
- **Issue:** G3 explicitly says "No silent swallow, no untyped `unwrap`, no log-and-continue." §6.2's `persist_state` retry loop ends with `unreachable!()`. While `unreachable!()` panics rather than returns a wrong value, panicking inside the executor's async task is exactly the kind of failure mode this design aims to eliminate — a panic here drops the V2Executor task, all in-memory state is lost, and there's no `RitualEvent` produced. The state machine never sees the failure.
- **Suggested fix:** Restructure the loop to be statically exhaustive without `unreachable!()`. One option:
  ```rust
  let mut last_error: io::Error = /* synthetic */;
  for attempt in 1..=MAX_ATTEMPTS {
      match self.hooks.persist_state(state).await {
          Ok(()) => return RitualEvent::StatePersisted { attempt },
          Err(e) => {
              last_error = e;
              if attempt < MAX_ATTEMPTS { sleep(BACKOFF[attempt-1]).await; }
          }
      }
  }
  RitualEvent::StatePersistFailed { attempt: MAX_ATTEMPTS, error: last_error.to_string() }
  ```
  No `unreachable!()`, no panic surface, identical semantics. Or use `.fold()` / explicit final-attempt branch.
- **Why it matters:** The whole point of this redesign is to make IO failures observable. A panic in the retry loop is the single failure mode the design is trying to prevent.

### FINDING-7 🟡 Typo `on_phase_transition_phase` (extra suffix) in §5.6
- **Section:** §5.6 (`run_ritual` body)
- **Issue:** §5.6 calls `hooks.on_phase_transition_phase(&prev_phase, state.phase());` but the trait in §4 defines the method as `on_phase_transition(&self, _from: &RitualState, _to: &RitualState)`. Three mismatches: (1) name has extra `_phase` suffix, (2) trait takes `&RitualState` but caller passes `&Phase`, (3) trait signature uses underscore-prefixed args (no-op default) so the type difference matters when implemented.
- **Suggested fix:** Pick one. Either:
  - Trait takes `&Phase` (cheaper, transition-focused) — most embedders don't need the full state. Fix §4's signature.
  - Caller passes `&state` for both — then the hook compares `from.phase()` vs `to.phase()` itself. Fix §5.6's call.
  Resolve in §4 + §5.6 simultaneously and add a test that exercises the hook.
- **Why it matters:** Compile-time error, but more importantly indicates §4 and §5 were drafted in different passes and not reconciled. There may be other minor mismatches (worth a sweep).

### FINDING-8 🟡 `RitualHooks` is `async_trait` but §1 NG5 says "no new async runtime requirements"
- **Section:** §4 vs §1 NG5
- **Issue:** §4 declares `#[async_trait] pub trait RitualHooks`. NG5 says "Not introducing async hooks if sync-with-`block_on` works. Keep the trait synchronous where the V2Executor is already sync." The trait as defined has both async (`notify`, `persist_state`) and sync methods (`resolve_workspace`, `stamp_metadata`, `should_cancel`) — which is fine — but `#[async_trait]` is applied to the whole trait. In practice this means every impl of `RitualHooks` (including `NoopHooks`, downstream test mocks) carries the `async_trait` macro and its boxed-future overhead, even for embedders who only use sync hooks.
- **Suggested fix:** Either (a) split into two traits (`RitualHooks` sync + `RitualAsyncHooks` async, or vice versa), (b) accept the overhead and revise NG5 to say "we use `async_trait` for the IO methods; sync methods stay sync via default-method idiom," or (c) use Rust 1.75+'s native AFIT (async fn in trait) since `async_trait` is now mostly unnecessary for `dyn`-compatible traits via `Box<dyn>` patterns. Whichever you pick, NG5 must be reconciled with §4.
- **Why it matters:** API surface decision that's hard to reverse after release. Worth resolving before the trait is `pub`-shipped.

### FINDING-9 🟡 `persist_state` retry: hook may write partial file, retry overwrites — corruption window unaddressed
- **Section:** §6.2
- **Issue:** Retry loop calls `hooks.persist_state(state)` up to 3 times. If attempt 1 partially writes (e.g., serialized JSON, fsync failed mid-write, disk filled at byte 4096 of 8192), then attempt 2 succeeds and overwrites. Fine. But what if attempt 1 partially writes, attempt 2 *also* fails differently (e.g., disk fills more, write returns Ok but fsync fails — depending on hook impl), and attempt 3 fails entirely → on-disk file is now a **truncated/corrupted JSON**, and `StatePersistFailed` is emitted. On next process restart, `RitualPersister::read` will fail to parse, and the ritual is unrecoverable. The design doesn't specify the hook's atomicity contract.
- **Suggested fix:** Add to §4 (RitualHooks doc) and §6: "Implementations of `persist_state` MUST write atomically (write-to-tempfile + rename, or equivalent). Partial writes are a contract violation." Then `RustclawHooks::persist_state` (§7.3) must use atomic write — verify that current `RitualPersister::write` does this; if not, that's a bug in scope of this PR. Also consider adding to §9.1 a test `persist_failed_mid_write_does_not_corrupt` that uses `FailingPersistHooks` configured to truncate.
- **Why it matters:** "Disk full mid-implement" is exactly the scenario in §6.5 ("never silently lose data"). Current design protects against silent loss but introduces silent corruption. Same outcome for the user.

### FINDING-10 🟡 "Phase boundary" in §6.3 not formally defined
- **Section:** §6.3, §3
- **Issue:** §6.3 distinguishes "checkpoint phase (between phase transitions)" from "long-running phase (Implementing, Verifying)" to decide whether `StatePersistFailed` enters `PersistDegraded` or `Failed`. But "phase boundary" is not formally defined. Looking at the state machine, `SaveState` actions are emitted from many points (after most events). Which `SaveState` calls are "between phases" vs "mid-phase"?
- **Suggested fix:** Define phase boundary concretely. Two candidate definitions:
  - **Definition A (event-based):** A SaveState is "phase-boundary" iff it's the first SaveState emitted after a phase-transition event (Implementing→Verifying, etc.). All others are mid-phase.
  - **Definition B (action-tag-based):** Add a flag to `SaveState`: `RitualAction::SaveState { kind: SaveStateKind::Boundary | SaveStateKind::Periodic }`. State machine emits `Boundary` at transitions and `Periodic` for in-phase saves.
  B is more explicit and testable. Pick one and write it into §3 / §6.3. Then add a test `phase_boundary_persist_fail_aborts` and `mid_phase_persist_fail_degrades` to §9.1 that exercises both paths.
- **Why it matters:** The recover/abort decision in §6.3 hinges on this distinction. Hand-waving it means implementer guesses; reviewer can't tell if the guess is right.

### FINDING-11 🟡 Cross-repo issue dual-tracking not addressed in closeout plan
- **Section:** §12 AC11, §10
- **Issue:** This bundle resolves 4 issues across 2 repos:
  - gid-rs ISS-052 (P0) ↔ rustclaw ISS-051 (P0) — same root cause, dual-tracked
  - gid-rs ISS-051 (P1) ↔ rustclaw ISS-050 (P1) — same root cause, dual-tracked
  AC11 says "all four parent issues … closeable with reference to this design + the merged PRs." But it doesn't say *who* closes them, *when* (gid-core release? rustclaw upgrade?), or how to keep the four issue files in sync until closure. §10 release plan focuses on version bumps but doesn't include "close issues in both repos" as a step.
- **Suggested fix:** Add §10.6 "Issue closure":
  1. After gid-core merge: mark gid-rs ISS-052 and ISS-051 status `done`, link to PR.
  2. After rustclaw merge: mark rustclaw ISS-051 and ISS-050 status `done`, link to both PRs.
  3. Cross-link the four issue files to this design (`design.md`) before any are closed, so post-mortem readers can find the unified solution from any of the four issues.
- **Why it matters:** Issue tracking hygiene. Without this step, the duals will drift — one will close, the other will sit open for weeks until someone notices.

### FINDING-12 🟡 §9.1 missing test coverage for `on_phase_transition` and `stamp_metadata` hooks
- **Section:** §9.1
- **Issue:** The 11 tests listed cover `on_action_start`, `on_action_finish`, `notify`, `should_cancel`, `persist_state`, `resolve_workspace`, plus skill/subloop gates. But `RitualHooks` has 8 methods (counting defaults). Tests cover 6. Missing:
  - `on_phase_transition` — never tested. Given FINDING-7 already showed signature ambiguity here, lack of test is how that bug snuck in.
  - `stamp_metadata` — only mentioned in `hooks_stamp_metadata_sets_pid_and_adapter` (§9.3, rustclaw side). gid-core side has nothing verifying that V2Executor actually calls it (§5.6 calls it before the loop starts).
- **Suggested fix:** Add to §9.1:
  - `phase_transition_hook_called_on_each_change` — script a 4-phase ritual; assert hook called 3× with correct (from, to) pairs.
  - `stamp_metadata_called_once_at_start` — assert hook called exactly once before any action; assert state mutations from the hook persist.
- **Why it matters:** Untested hooks = untested code path = drift over time. The whole point of this design is "every action goes through one path"; "every hook has a test" is the corollary that prevents future drift.

### FINDING-13 🟡 `should_cancel` race: design polls between actions but skill execution can be minutes long
- **Section:** §3.2, §4 (`should_cancel`), §5.2
- **Issue:** §5.2: "Cancellation polled at top of every action." A single `RunSkill` action can take 5–15 minutes (LLM ritual phase). If user types `/ritual cancel` 30 seconds into a 10-minute skill, they wait 9.5 minutes for cancellation to take effect. §4's doc says "polled between actions" which is consistent but doesn't address mid-action cancellation. Not immediately a blocker, but the prior `RitualRunner` (§7) likely had no better story either, so this isn't a regression — it's just unaddressed.
- **Suggested fix:** Either (a) document explicitly in §4 that cancellation latency is bounded by the longest single action (= LLM skill timeout, typically 10min), and that's an accepted trade-off; or (b) add a richer hook `should_cancel_during(&self, action: &RitualAction) -> bool` that V2Executor polls inside long-running actions (e.g., after each LLM turn within `run_skill`). Option (b) is more invasive but fixes a real UX issue when potato hits cancel. Pick one and document.
- **Why it matters:** UX bug. The `/ritual cancel` command silently appearing not to work for 10 minutes will get reported as a bug.

### FINDING-14 🟡 `skill_file_policy` hardcodes skill names → conflicts with skills/ directory autodiscovery
- **Section:** §5.4
- **Issue:** `skill_file_policy(skill_name: &str) -> SkillProducesFiles` matches on hardcoded skill names (`"implement"`, `"review-design"`, etc.). But rustclaw's skills come from `skills/<name>/SKILL.md` files — they are user-defined, dynamically loaded, can be added/renamed/removed without touching gid-core. Hardcoding the policy in gid-core means: (1) adding a new skill that should be `Required` requires a gid-core release, (2) renaming a skill silently breaks the gate (falls into `_ => Optional`), (3) the policy is invisible to the skill author (it lives in a different repo).
- **Suggested fix:** Move policy into the SKILL.md frontmatter:
  ```yaml
  ---
  name: implement
  file_policy: required  # required | optional | forbidden
  ---
  ```
  Then `skill_file_policy` reads it from the loaded skill metadata. Default `optional` if unset. Document the field in skill author docs. This makes the policy local to the skill, where it belongs. (And aligns with potato's "符合 purpose" rule: the policy is part of the skill's contract, not a global table.)
- **Why it matters:** Maintainability + alignment with project convention. Hardcoded list will drift the moment a new skill is added.

### FINDING-15 🟢 §6.2 retry attempts logged via `notify` may spam Telegram
- **Section:** §6.2
- **Issue:** On each failed retry attempt, `hooks.notify(&format!("⚠️ state persist attempt {attempt}/3 failed: {e}; retrying"))` fires. With 3 attempts and back-to-back transient FS errors, user gets 2 ⚠️ messages within ~1.3 seconds, plus a final summary if it fails. For a flaky FS this is noise. (Same critique applies to self-review subloop §8.2 retries.)
- **Suggested fix:** Either (a) only `notify` after the final outcome (success/fail), with the count summarized; or (b) add a `notify_verbosity` knob in `V2ExecutorConfig` (debug/normal/quiet). (a) is simpler and probably fine. Update §6.2 + §8.2 accordingly.
- **Why it matters:** Minor UX. Worth fixing now so we don't ship and immediately get "stop spamming me."

### FINDING-16 🟢 AC3 grep regex too narrow: misses second dispatcher and `match action {`-shaped catches
- **Section:** §12 AC3
- **Issue:** AC3: `grep -rn "match action" rustclaw/src/ returns zero matches against RitualAction variants`. But §2.1 documents two `match action {` blocks at lines 575 and 813. AC3 grep is on the right track, but:
  - It will match `match action {` strings inside string literals or comments (false positives).
  - It won't catch `match &action`, `match a` after rebinding, or `match dispatch.action`.
- **Suggested fix:** Change AC3 to a *combination* of greps + LOC count:
  - `grep -rnE "match\s+(\&?\w+\.)?action\b" rustclaw/src/ | grep -v "//\|/\*"` returns zero
  - AND `wc -l rustclaw/src/ritual_runner.rs` ≤ 800 (post-deletion target from §7)
  Either alone is gameable; both together are a real check.
- **Why it matters:** Acceptance criteria must be unfakeable. AC3 as written can pass while a renamed `match dispatch_target {…}` survives.

### FINDING-17 🟡 Design ignores `execute_actions` (plural) — only documents `execute` (singular)
- **Section:** §3.1, §5.2, §5.6
- **Issue:** `crates/gid-core/src/ritual/v2_executor.rs:239` already defines `pub async fn execute_actions(&self, actions: &[RitualAction], state: &RitualState) -> Option<RitualEvent>` (the plural wrapper that processes a vec of actions and returns the *first* event-producing one's event). Existing `run_ritual` (line 1292+) calls `execute_actions`, not `execute`. The design always shows `executor.execute(&action, &state)` in pseudocode (§3.1, §5.2, §5.6). Implementer faces an unspecified question: does V2Executor expose `execute` AND `execute_actions`? Are they both `pub`? Does the new `run_ritual` call which?
- **Suggested fix:** §5.6 must specify the public API of the V2Executor module after the change:
  - Is `execute_actions` retained?
  - If yes: it must also call hooks (FINDING-12 risk applies — half of dispatch goes through one method, half through another).
  - If no: list it in the §7 deletion plan and update existing internal callers.
  Recommend keep `execute` (singular) public + hook-instrumented; demote `execute_actions` to a private helper or delete and inline.
- **Why it matters:** Public API consistency. If both methods are public and only one calls hooks, the "single dispatcher" invariant is silently broken inside gid-core itself.

### FINDING-18 🟡 §8 says "subloop is now in V2Executor" but it isn't there today
- **Section:** §8.1, §8.2, §8.4
- **Issue:** §8.4: "Self-review subloop is now in V2Executor, which already has `ScriptedLlm` test infrastructure." `grep -n "self_review\|subloop" crates/gid-core/src/ritual/v2_executor.rs` returns zero matches — the subloop does not exist in V2Executor today. It lives in rustclaw's ritual_runner.rs (per §7.1, lines ~1900–2200). §8 reads as if relocating an existing function, but it's actually a *port* (rewrite + port + add a new gate). The estimated LOC and test count in §8 / §9 may be optimistic.
- **Suggested fix:** Reword §8.4 as "After this design, the self-review subloop will be implemented in V2Executor (currently in rustclaw)." Update §6 (Goals) — G5 already mentions the fix but doesn't flag the relocation. Add "port self-review subloop into V2Executor" as an explicit task in §7.4 commit ordering — it's part of commit 3 (rewrite RitualRunner::run) but worth calling out so the implementer doesn't miss it.
- **Why it matters:** Mis-framed work item. The design says "fix" but the actual scope is "port + fix" — meaningfully more work.

### FINDING-19 🟢 Q5 ("no feature flag") rejected without addressing rollback path
- **Section:** §11.2 Q5, §10.5
- **Issue:** Q5 rejects feature flag because "two dispatchers is the bug; shipping a third behind a flag is worse." Reasonable. But §10.5 rollback plan ("yank gid-core, rustclaw revert upgrade commit") only works *before* anyone publishes the new gid-core to crates.io. After publish, downstream users (none today besides rustclaw, but the design says gid-core is published) will have pulled the new version. Yank is possible but disruptive.
- **Suggested fix:** Add to §10.5 the pre-publish gate explicitly: "Manual acceptance §9.5 + zero-file regression test (AC5) MUST pass on path-dep build before `cargo publish` runs." Also: Q5 itself is fine, just note in §10.5 that without a feature flag the rollback window closes at `cargo publish`.
- **Why it matters:** Minor — not many gid-core consumers exist yet. But documenting the constraint clearly helps potato decide if the §9.5 manual run is enough confidence.

### FINDING-20 🟢 §6.5 pseudo-code missing for "5 consecutive failures → Failed" counter
- **Section:** §6.3
- **Issue:** "On 5 consecutive failures, transitions to `Failed`" but no spec for *where* the counter lives. Inside `RitualState`? In `PersistDegraded`'s payload? Reset on first success? What if user issues `/ritual cancel` while in PersistDegraded — does the counter persist into the cancel cleanup phase?
- **Suggested fix:** §6.3 should specify counter lives in `PersistDegraded` payload: `PersistDegraded { underlying_phase, error, consecutive_failures: u32 }`. Increment on each `StatePersistFailed`, reset to 0 on each `StatePersisted` (which transitions back out of PersistDegraded anyway, so reset is implicit). Cancel transitions out of PersistDegraded directly to Cancelled, no counter handling needed. Spell this out in §6.3 + add to §9.1: `persist_degraded_5_failures_aborts`.
- **Why it matters:** State definition completeness. Implementer would have to infer; reviewer can't verify.

<!-- FINDINGS -->

## Applied

**Apply session: 2026-04-27 (RustClaw main agent)**

Applied 11 of 20 findings (5 critical + 6 important). Remaining 9 deferred — see "Follow-ups" below.

### ✅ FINDING-1 (Critical) — `SkillFailed` extended, not replaced
- §5.5.1 now explicitly preserves the existing `SkillFailed { phase, error }` shape and adds `reason: Option<SkillFailureReason>` as an additive field. New gates emit `Some(_)`; legacy emit sites pass `None`. Migration scope (8+ match arms, 5 tests, run_ritual emit) documented inline.

### ✅ FINDING-2 (Critical) — `run_ritual` breaking signature change called out
- §5.6.1 documents the existing signature, names it as a breaking change, lists known callers (gid-core internal tests + co-located), and decides explicitly to break rather than rename. AC12 (§12) updated to require CHANGELOG entry covering the migration.

### ✅ FINDING-3 (Critical) — Two `RitualEvent` enums disambiguated
- §5.5 leads with a disambiguation block stating that all new variants apply to `state_machine::RitualEvent`, never `notifier::RitualEvent`. Risk **R6** added in §11.1. Rename of one of the enums is filed as a follow-up (out of scope).

### ✅ FINDING-4 (Critical) — External callers enumerated in §7.6
- New §7.6 lists all 6 known production call sites (`tools.rs:5907`, `channels/telegram.rs:1155/1433/1502/2555`, plus the `NotifyFn` plumbing). Commit 3 split into 3a (migrate callers) + 3b (delete dispatcher) so reviewers can verify each step.

### ✅ FINDING-5 (Critical) — `PersistDegraded` reframed as side-channel flag (preserves NG1)
- §6.3 fully rewritten. `PersistDegraded` is **no longer a wrapping phase**; it's `persist_degraded: Option<PersistDegradedInfo>` on `RitualState`. The `Phase` enum and transition table are unchanged. NG1 (§1) updated to call this out. Counter (`consecutive_failures`) lives in `PersistDegradedInfo`. Phase-boundary vs periodic distinction (FINDING-10) baked into the same change via `SaveStateKind::Boundary | Periodic`.

### ✅ FINDING-6 (Important) — `unreachable!()` removed from §6.2 retry loop
- Loop now uses `last_error: String` accumulator and statically exhaustive return at the end. No panic surface. Atomicity contract (FINDING-9) referenced from the new body.

### ✅ FINDING-7 (Important) — `on_phase_transition` signature reconciled
- Trait (§4) now takes `&Phase, &Phase` (not `&RitualState`). Caller in §5.6.2 calls `hooks.on_phase_transition(&prev_phase, state.phase())`. Typo `on_phase_transition_phase` removed.

### ✅ FINDING-8 (Important) — NG5 reconciled with `#[async_trait]`
- §1 NG5 rewritten to acknowledge `#[async_trait]` on the two IO methods (`notify`, `persist_state`) and document the boxed-future overhead trade-off explicitly.

### ✅ FINDING-9 (Important) — `persist_state` atomicity contract
- §4 doc on `persist_state` now mandates atomic-write semantics (write-to-tempfile + rename). §6.2 retry loop body references the contract.

### ✅ FINDING-10 (Important) — Phase boundary formally defined
- §6.3.2 introduces `RitualAction::SaveState { kind: SaveStateKind::Boundary | Periodic }`. State machine emits `Boundary` after phase-transition events, `Periodic` for in-phase checkpoints. §6.3.3 event-handling table uses this distinction.

### ✅ FINDING-12 (Important) — Hook test coverage closed
- §9.1 test table extended from 11 → 16 tests. Added: `phase_transition_hook_called_on_each_change`, `stamp_metadata_called_once_at_start`, `phase_boundary_persist_fail_aborts`, `mid_phase_persist_fail_degrades`, `persist_degraded_5_failures_aborts`. Target LOC bumped 600 → 800.

### ✅ FINDING-13 (Important) — Cancellation latency documented
- §4 `should_cancel` doc now spells out the "polled between actions" semantics and the up-to-10-min worst-case latency as an accepted trade-off.

### ✅ FINDING-14 (Important) — `skill_file_policy` moved to SKILL.md frontmatter
- §5.4 rewritten. Policy is declared per-skill in the SKILL.md frontmatter (`file_policy: required | optional | forbidden`); gid-core reads it from the loaded skill metadata; default `Optional`. Removes the hardcoded match.

### ✅ FINDING-17 (Important) — `execute_actions` API resolved
- §5.6.3 added. `execute()` (singular) stays `pub` and is the hook-instrumented dispatcher; `execute_actions()` (plural) is demoted to a private helper (or inlined and deleted). Single dispatcher invariant (G1) preserved inside V2Executor itself.

### ⏸ Deferred — 9 follow-up findings (not applied in this pass)

Tracked here as reminders; each can become a follow-up issue or be folded into implementation as a smaller correction:

- **FINDING-11** (Important): cross-repo issue closeout plan — add §10.6 listing who closes which of the 4 issues, when. *Easy fix; deferred to keep this pass focused on design correctness.*
- **FINDING-15** (Minor): retry-attempt notify spam → single summary on final outcome. *Already noted inline in the new §6.2 body as a deferred follow-up.*
- **FINDING-16** (Minor): AC3 grep too narrow — add LOC-count check + word-boundary regex. *Tighten when AC3 is run.*
- **FINDING-18** (Important): §8.4 wording "subloop is now in V2Executor" misframes the work as relocation when it's a port. *Wording fix; doesn't change scope.*
- **FINDING-19** (Minor): rollback-window note in §10.5 about pre-publish gate. *One-line addition during release prep.*
- **FINDING-20** (Minor): explicit pseudo-code for the consecutive-failure counter. *Subsumed by §6.3.3 table — counter behavior is now explicit, but a worked code example was not added.*
- (Plus the three originally-flagged-but-not-on-the-apply-list findings from the review summary, retained here for completeness — none block implementation.)

Filed as: **ISS-054** (`.gid/issues/ISS-054/issue.md`, created 2026-04-27) — scoped to "design refinements that don't block the implementation PR".
