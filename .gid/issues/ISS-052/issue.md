---
id: "ISS-052"
title: "Rustclaw RitualRunner has parallel action dispatcher that bypasses gid-core V2Executor quality gates"
status: open
priority: P0
created: 2026-04-26
severity: critical
related: ["ISS-038", "ISS-039", "ISS-051", "ISS-054"]
---
# ISS-052 — Rustclaw's parallel ritual dispatcher bypasses V2Executor quality gates

**Status:** open
**Type:** architectural debt — duplicate dispatcher across crates
**Severity:** critical (every ritual quality feature added to gid-core silently does NOT apply when ritual runs through rustclaw — which is the production path)
**Discovered:** 2026-04-26 — investigating ISS-051 revealed `rustclaw/src/ritual_runner.rs` carries its own implementations of every `RitualAction` variant in parallel with `gid_core::ritual::v2_executor::V2Executor`. Concretely: ISS-038 added a `file_snapshot` post-condition to `V2Executor::run_skill` that detects "skill claimed success but wrote zero files" → emit `SkillFailed`. **rustclaw's `run_skill` has zero `file_snapshot` calls**, so this gate is silently disabled in production.

## Problem

Two crates implement the same dispatcher. They have drifted:

```
gid_core::V2Executor                    rustclaw::RitualRunner
├─ execute(action, state)               ├─ execute_event_producing_single
├─ execute_actions(actions, state)      ├─ execute_event_producing
├─ run_skill (file_snapshot ✓)          ├─ run_skill (file_snapshot ✗)   ← ISS-038 GATE BYPASSED
├─ run_shell                            ├─ run_shell
├─ run_triage                           ├─ run_triage
├─ run_planning                         ├─ run_planning
├─ run_harness                          ├─ run_harness
├─ detect_project                       ├─ detect_project
├─ save_state                           ├─ save_state                    ← ISS-051 fixed here too?
├─ update_graph                         ├─ update_graph
└─ ...                                  └─ ...
```

`grep -rn "file_snapshot" /Users/potato/rustclaw/src/ → zero hits`. Confirmed.

Consequence: rustclaw's `implement` phase will mark a skill "passed" even when the LLM produced zero tool calls and wrote zero files. The exact failure mode ISS-038 was designed to prevent.

## Why two dispatchers exist (history)

Rustclaw needs things gid-core's V2Executor doesn't natively have:

1. **Sub-agent (typed AgentRunner) path** for `RunSkill { name: implement / review / ... }` — V2Executor only knows the `LlmClient` direct path.
2. **CancellationToken** for interactive `/ritual cancel`.
3. **ApplyReview** action — uses a REVIEWER sub-agent, not generic LLM.
4. **Telegram notify** as a real channel, not just a `NotifyFn`.
5. **Session/conversation context** — the rustclaw chat session this ritual was launched from.

Original solution: copy V2Executor's dispatcher into rustclaw and inline rustclaw-specific bits. This worked at v1; every subsequent gid-core ritual feature has had to be **manually re-applied to rustclaw** or it silently does nothing. ISS-038 (file_snapshot) was never re-applied. ISS-039 (graph preflight) — needs audit; probably also missing.

## Why this is root-fix and not "copy file_snapshot into rustclaw"

A patch-level fix (port `file_snapshot` calls into rustclaw's `run_skill`) restores ISS-038 but leaves the structural debt. Next gid-core ritual feature will have the same fate. Patch-fix says "remember to copy". Root-fix says "rustclaw cannot have its own dispatcher".

## Proposed fix — RustClaw delegates dispatch to V2Executor

### Architecture

`rustclaw::RitualRunner` does **not** dispatch actions. It owns a `gid_core::V2Executor` and an extension trait implementation; for every `actions: Vec<RitualAction>` from the state machine, it calls `executor.execute_actions(&actions, &state, hooks)`. V2Executor handles ALL action variants. Rustclaw's only responsibility is providing the hooks for things V2Executor cannot do natively.

### Hook trait (in gid-core)

```rust
#[async_trait]
pub trait RitualHooks: Send + Sync {
    /// Called by V2Executor::run_skill BEFORE the default LlmClient path.
    /// Returns Some(event) if hooks handled the skill (e.g. via sub-agent).
    /// Returns None to let V2Executor's default path run.
    /// CRITICAL: V2Executor will run file_snapshot post-condition regardless
    /// of which path produced the event.
    async fn try_run_skill(
        &self,
        name: &str,
        context: &str,
        state: &RitualState,
        cancel: &CancellationToken,
    ) -> Option<RitualEvent>;

    /// Optional override for ApplyReview — rustclaw uses REVIEWER sub-agent.
    /// Default impl returns None → V2Executor falls back to standard handler.
    async fn try_apply_review(&self, approved: &str, state: &RitualState) -> Option<RitualEvent> {
        None
    }

    /// Cancel signal accessor. Default = never-cancelling token.
    fn cancel_token(&self) -> &CancellationToken;

    /// Notify channel. Default impl provided via NotifyFn-style adapter.
    async fn notify(&self, message: &str);
}
```

V2Executor gains an `Option<Arc<dyn RitualHooks>>` field. `run_skill` becomes:

```rust
async fn run_skill(&self, name, context, state) -> Result<RitualEvent> {
    let event = if let Some(hooks) = &self.hooks {
        if let Some(e) = hooks.try_run_skill(name, context, state, hooks.cancel_token()).await {
            e
        } else {
            self.default_run_skill(name, context, state).await?
        }
    } else {
        self.default_run_skill(name, context, state).await?
    };

    // ISS-038 gate — runs for BOTH paths
    self.apply_file_snapshot_check(name, &event, state)?
}
```

### Rustclaw side — what survives

Rustclaw keeps:
- `RustclawRitualHooks: RitualHooks` — wraps AgentRunner / cancel / notify / session context. ~300 lines.
- `RitualRunner` — much thinner; just owns V2Executor + state machine driver + Telegram glue.

Rustclaw deletes:
- `execute_event_producing_single` / `execute_event_producing` / `execute_fire_and_forget_with_state`
- All `run_skill` / `run_shell` / `run_triage` / `run_planning` / `run_harness` / `detect_project` / `save_state` / `update_graph` / `cleanup` re-implementations
- Estimated ~1500 LOC removed, ~300 LOC added → net ~1200 LOC reduction.

## Combined with ISS-051

ISS-051 (save_state failure handling) touches the same `save_state` code path. Doing them together is cheaper:

1. Make `save_state` return `Result` in V2Executor (ISS-051 §1).
2. Delete rustclaw's parallel `save_state` (ISS-052 — covered by full deletion above).
3. State machine handles `StatePersistFailed` event (ISS-051 §2).

Order: do ISS-052 architectural change first; ISS-051 falls out naturally because there is only one save_state path left to fix.

## Acceptance criteria

- [x] `RitualHooks` trait defined in gid-core.
- [x] V2Executor accepts `Option<Arc<dyn RitualHooks>>`.
- [x] V2Executor `run_skill` runs `file_snapshot` post-condition for hook-produced events too (test: hook returns Success but writes zero files → V2Executor overrides to SkillFailed). — gid-core test `skill_required_zero_files_fails`.
- [x] Rustclaw `RitualRunner` no longer has any `RitualAction` match-arm dispatching. — `RitualRunner` struct entirely removed; only utility helpers (state I/O, registries, file preloading, phase parsing) remain in `src/ritual_runner.rs`.
- [x] Rustclaw `RustclawRitualHooks` implements `RitualHooks`, wraps AgentRunner / cancel / notify. — `RustclawHooks` in `src/ritual_hooks.rs`.
- [x] All existing rustclaw ritual integration tests pass unchanged. — 356/356 passing.
- [x] New test in rustclaw: hook-produced "success" with zero files → ritual ends in SkillFailed (proves ISS-038 gate now applies to rustclaw path). — `ritualhooks_surface_has_no_skill_dispatch_method` (structural invariant: `RitualHooks` exposes no skill-dispatch method, so rustclaw cannot bypass the gate; gate behavior itself is covered by gid-core's `skill_required_zero_files_fails`).
- [x] grep `file_snapshot` in rustclaw/src/ → still zero hits (because dispatcher is now entirely in gid-core, not because gate is missing).
- [x] Net LOC change in rustclaw negative (~1000+ deletion). — `src/ritual_runner.rs` 2960 → 1163 (−1797 LOC; struct + dispatcher gone, only utility helpers remain).

### Implementation tracking (rustclaw side)

The rustclaw migration runs across 4 graph tasks driven from `design.md` §7.6:

- **T12** (done 2026-04-27, rustclaw `d940e8b`) — migrate 2 production entry points (`tools.rs::start_ritual`, `channels/telegram.rs::handle_ritual_command`) to `run_ritual + RustclawHooks`. Old `RitualRunner` API still compiles. 348 tests green.
- **T13a** (done 2026-04-27, rustclaw `d940e8b` — bundled with T12 due to git ops) — stubbed `run_skill` / `run_shell` / `run_triage` / `run_planning` / `run_harness` / `save_state` bodies. **Deviation from §7.1.1**: stubs use `Err(anyhow!())` + `tracing::error!` tripwire instead of `unreachable!()` — daemon-safe during T13a→T13b window since 21 telegram call sites still reach legacy paths via `/ritual retry|skip|clarify|reply|cancel|resume-from-phase`. Approved by potato 2026-04-27 chat. Public API surface (`advance` / `send_event` / `resume_from_phase` / `make_ritual_runner`) preserved. Bisect point. `src/ritual_runner.rs` shrank 2960 → 2443 lines (517 lines deleted from stubs); 348 tests green.
- **T13b** (done 2026-04-27, rustclaw next-commit) — verified: all 21 telegram call sites already migrated to `spawn_ritual` / `spawn_resume` (which wrap `run_ritual` / `resume_ritual` + `RustclawHooks`). Zero callers of legacy `RitualRunner::advance` / `send_event` / `resume_from_phase` / `make_ritual_runner` remain anywhere in `src/`. The `RitualRunner` struct itself is gone — `src/ritual_runner.rs` retains only utility helpers (state I/O, registries, `preload_files_with_budget`, `parse_phase_name`, `has_target_project_dir`). Final LOC: 1163 (−1797 vs T12 baseline). Added `ritualhooks_surface_has_no_skill_dispatch_method` test pinning the structural invariant that `RitualHooks` has no skill-dispatch method (cannot bypass ISS-038 gate). 356 tests green, zero warnings.
- **T14 / T16 / T17** — cleanup, integration tests, release.

The T13 → T13a/T13b split was added during T12 implementation when the 17 secondary telegram call sites became visible. See `design.md` §7.1.1.

## Out of scope

- Refactoring how V2Executor itself is structured (config, builder pattern). Keep current shape; only add hooks field.
- Changing the `RitualAction` enum or state machine. Pure dispatcher refactor.
- Telegram-specific NotifyFn → RitualHooks::notify migration if it complicates the diff. Can do that as a follow-up cleanup.

## Risks

- **Trait signature wrong on first try** → painful to fix mid-refactor. Mitigation: write `design.md` for this issue with concrete trait signature + 3 example call sites BEFORE coding.
- **Hidden coupling**: rustclaw's dispatcher might assume things about action ordering V2Executor doesn't preserve. Mitigation: side-by-side run a few ritual scenarios in tests, compare event traces.
- **Cross-crate version skew**: rustclaw pins to a specific gid-core version. Need to bump gid-core minor version (new public trait), publish, update rustclaw. Extra step but routine.

## Notes

- Discovered 2026-04-26 during root-cause analysis of stuck ritual `r-950ebf`. Same incident as ISS-051. The disk-full symptom unmasked ISS-051; the ISS-051 investigation unmasked ISS-052.
- This is **the** structural reason every ritual gate added to gid-core feels "did it actually work?". It usually didn't, in production via rustclaw.
- After this fix, rustclaw becomes a thin runtime shell over gid-core's ritual engine — which is what it was supposed to be from the start.
