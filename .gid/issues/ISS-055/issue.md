---
id: "ISS-055"
title: "ritual::resume_ritual entry point for paused-state user-event injection"
status: closed
priority: P1
created: 2026-04-27
closed: 2026-04-27
severity: medium
related: ["ISS-052"]
---
# ISS-055 — `ritual::resume_ritual` entry point

**Status:** closed (2026-04-27 — implemented in same session, tests pass).

## Problem

[rustclaw ISS-052](../../../../rustclaw/.gid/issues/ISS-052/) T13b requires migrating 6 user-event Telegram call sites (`/retry`, `/skip`, `/clarify`, `/reply`, `/cancel`, `/resume-from-phase`) onto gid-core's canonical ritual dispatcher. ISS-052 AC3 says **there must be exactly one ritual event dispatcher in the codebase** — embedders shall NOT re-implement the loop.

`gid_core::ritual::run_ritual` is the only public entry point. It takes initial state and unconditionally fires `RitualEvent::Start`. There is no public way to drive a previously-persisted, paused ritual forward by injecting a user event. Without this, T13b cannot honor AC3 — embedders would have to either (a) re-implement the dispatcher (violates AC3) or (b) defer the user-event sites (leaves AC3 unenforced for that subset of the API surface).

## Decision

Add `gid_core::ritual::resume_ritual(state, user_event, config, hooks) -> RitualOutcome`.

- **Additive change** to gid-core. No public surface removed or renamed.
- Minor version bump (0.3.x → 0.4.0) on next publish — additive but introduces new public symbols (`resume_ritual`, `UserEvent`).
- Internal refactor: extracts a private `drive_event_loop` helper; `run_ritual` and `resume_ritual` differ only in pre-loop setup (workspace resolve + stamp_metadata for start; nothing for resume) and initial event (`Start` vs caller-provided).

### Public API

```rust
pub enum UserEvent {
    Cancel,
    Retry,
    SkipPhase,
    Clarification { response: String },
    Approval { approved: String },
}

pub async fn resume_ritual(
    state: RitualState,
    user_event: UserEvent,
    config: V2ExecutorConfig,
    hooks: Arc<dyn RitualHooks>,
) -> RitualOutcome;
```

`UserEvent` is a deliberate subset of the internal `state_machine::RitualEvent` — embedders depend only on this enum, not on the FSM type, so the state machine can evolve freely.

### Invariants preserved

- **No workspace re-resolution.** The paused state already has `target_root` populated from its first run; re-resolving could pick a different path if the registry changed mid-ritual.
- **No `stamp_metadata` re-stamp** (FINDING-12 / design §4 once-only invariant). Verified by a dedicated test (`resume_does_not_restamp_metadata`).
- **Same 50-iteration cap, same terminal/paused break behavior, same Notify/SaveState dispatch path** as `run_ritual` — they share `drive_event_loop`.

## Tests

`crates/gid-core/tests/ritual_resume.rs`:

1. `resume_with_user_cancel_reaches_cancelled` — `UserEvent::Cancel` from `WaitingClarification` → `Cancelled` terminal. Exercises the early-exit path (terminal phase reached on first transition; final actions still drained).
2. `resume_with_user_retry_drives_event_loop` — `UserEvent::Retry` from `WaitingClarification` → re-triage → escalation (no LLM client). Exercises the full event loop, proves `drive_event_loop` is shared with `run_ritual`.
3. `resume_does_not_restamp_metadata` — asserts `NoopHooks::stamp_metadata_count() == 0` after resume.

## Verification

```
cargo test --features full -p gid-core
```

→ 1199 unit + 3 new integration tests pass; 0 regressions; 0 warnings.

## Files changed

- `crates/gid-core/src/ritual/v2_executor.rs` — extracted `drive_event_loop`; added `UserEvent` + `resume_ritual`.
- `crates/gid-core/src/ritual/hooks.rs` — added `stamp_metadata_calls` counter to `NoopHooks` (test-only assertion enabler).
- `crates/gid-core/src/ritual/mod.rs` — re-exported `resume_ritual` + `UserEvent`.
- `crates/gid-core/tests/ritual_resume.rs` — new integration test file.

## Cross-repo consequences

rustclaw T13b can now:

1. Delete its legacy public dispatcher API (`crate::ritual::dispatcher::*`).
2. Migrate the 6 user-event Telegram call sites to `gid_core::ritual::resume_ritual(loaded_state, UserEvent::*, ...)`.
3. Migrate the remaining 11 telegram sites to `gid_core::ritual::run_ritual` (start) or `resume_ritual` (resume) as appropriate.
