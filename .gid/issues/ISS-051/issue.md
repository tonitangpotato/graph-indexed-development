---
id: "ISS-051"
title: "Ritual wedges silently when save_state fails (no error recovery in state machine)"
status: open
priority: P1
created: 2026-04-26
severity: high
related: ["ISS-052"]
---
# ISS-051 — Ritual wedges silently when save_state fails

**Status:** open
**Type:** bug — error handling gap in ritual state machine
**Severity:** high (any IO failure during a ritual leaves it stuck in a non-terminal phase forever; user has no signal except "ritual hasn't progressed")
**Discovered:** 2026-04-26 — ritual `r-950ebf` for engram ISS-028/ISS-029 wedged at Verifying phase. Root cause: cargo test in verify phase failed → ritual tried to advance to Failed → `save_state` IO call returned `Err("No space left on device")` → error was logged but **state machine did not transition to terminal Failed**, ritual stayed at Verifying with no further progress and no notification.

## Problem

`save_state` calls inside the ritual runner are treated as fire-and-forget side effects. When they fail (disk full, permission, FS readonly, etc.), the failure is logged via `tracing::warn!` but:

1. The state machine continues as if state was persisted.
2. No `SkillFailed` / `RitualFailed` event is emitted.
3. The ritual sits in whatever transient phase it was in (here: Verifying).
4. Next run-loop tick re-reads stale on-disk state → keeps re-attempting the same step that already "completed" in memory but never persisted.
5. User-visible symptom: silence. No Telegram notify, no `/ritual status` change, no log loud enough to surface.

This is a classic "swallow the error, hope for the best" anti-pattern in a state machine where persistence IS the source of truth.

### Where the failure occurs (concrete)

In `rustclaw/src/ritual_runner.rs`:

- `RitualAction::SaveState` dispatch (around line 606) — fire-and-forget closure that logs but doesn't propagate.
- Same pattern in `gid-core/src/ritual/v2_executor.rs` — `save_state` returns `()` not `Result<()>`, so even if the executor wanted to propagate, the type signature prevents it.
- Multiple `SaveState` actions appear after every event-producing action; any of them can silently fail.

## Why this is root-fix and not a "just check for disk space" patch

A pre-flight disk check would be a workaround for this specific cause. The bug is **the state machine cannot represent "I tried to persist and failed"**. Other persistence failures (FS readonly, permissions changed mid-run, encrypted volume locked, etc.) would all wedge the same way.

Root fix: persistence is a first-class action whose failure is a first-class state transition.

## Proposed fix

### 1. `save_state` returns `Result`

Change signature in gid-core's V2Executor and any matching path in rustclaw:

```rust
async fn save_state(&self, state: &RitualState) -> Result<()>;
```

### 2. New event: `StatePersistFailed { error: String, phase: PhaseId }`

State machine handles this exactly like `SkillFailed`: transitions to `Failed`, emits `RitualFailed` event, fires terminal notify.

### 3. Runner-level retry policy (small, bounded)

Before declaring `StatePersistFailed`, retry with exponential backoff: 100ms, 500ms, 2s. If still failing → emit failure event. Transient FS hiccups should not blow up the ritual; durable failures should not be silent.

### 4. Visible notification on persistence failure

Telegram notify must fire on `StatePersistFailed` with: ritual_id, phase, IO error string, suggestion ("check disk space / permissions on .gid/runtime/rituals/"). Same channel as other terminal failures.

## Acceptance criteria

- [ ] `save_state` signature returns `Result<()>` end-to-end (gid-core + rustclaw).
- [ ] `StatePersistFailed` event exists; state machine transitions to Failed.
- [ ] Bounded retry (3 attempts, exp backoff) before failure event.
- [ ] Telegram notify fires on StatePersistFailed with diagnostic info.
- [ ] Test (gid-core): mock state writer returning `io::Error::from(io::ErrorKind::Other)` → ritual ends in Failed, not stuck.
- [ ] Test (rustclaw integration): inject readonly state dir → ritual fails fast with notification.

## Out of scope

- Pre-flight disk space check (separate, optional, low priority).
- Resumability after persist failure (resume from last good checkpoint) — currently rituals don't resume; that's a bigger feature.

## Related

- ISS-052 — separate, parallel ritual bug: rustclaw has its own action dispatcher that bypasses gid-core V2Executor's quality gates. Both bugs are in the ritual runtime; both should be fixed together to avoid touching the same files twice.

## Notes

- Discovered 2026-04-26 alongside ISS-052 during engram ISS-028/ISS-029 implementation. Combined incident: disk filled (cargo target, 7.7GB freed via `cargo clean`), cargo test failed, ritual tried to record failure, save_state hit "No space left on device", ritual wedged. Three layers of failure compounded; this issue addresses only the third layer.

## Progress (2026-04-28)

### Done in gid-core (this commit)

- `V2Executor::save_state` now returns `std::io::Result<()>` instead of swallowing errors. Serde failures wrapped as `io::Error::other`; FS write errors propagated as-is.
- `RitualAction::SaveState` dispatcher logs propagated errors (preserves historical fire-and-forget contract for this path; full event-feedback wiring is T08).
- 2 new unit tests pin the Result contract: writable tmp → Ok + file exists; project_root pointing at a regular file → Err propagated (not panic, not swallow).
- T03 `persist_state` retry wrapper (3 attempts, 50ms/250ms backoff, emits `StatePersisted{attempt}` / `StatePersistFailed{attempt:3}`) was already in place from ISS-052 work — covers retry/event ACs.

### Deferred to post-0.4.0-publish

- **rustclaw-side AC** (`Test (rustclaw integration): inject readonly state dir → ritual fails fast with notification`) deferred to post-0.4.0 publish. Reason: rustclaw still depends on gid-core 0.3.x; the integration test would need to pin against this branch. Post-publish, rustclaw can bump and add the test.
- **Telegram notify on StatePersistFailed**: state-machine arm already produces the right `Notify` action; rustclaw notify hook wiring is part of the same post-0.4.0 work.
- **T08 wiring**: replacing the dispatcher's `save_state` call with `persist_state` (so retry+event are driven by the action dispatcher, not just by direct test calls) — bigger change, separate task. Tracked at the dispatcher's TODO comment.
