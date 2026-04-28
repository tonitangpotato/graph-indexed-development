# Changelog

All notable changes to the `gid-core` and `gid-dev-cli` crates are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Pre-1.0 minor bumps
(`0.x.0`) signal breaking changes.

## [0.4.0] — 2026-04-2X (TBD)

The 0.4 release is dominated by **ISS-052** — eliminating the parallel ritual dispatcher
that lived in rustclaw and re-anchoring all ritual orchestration on `gid_core::V2Executor`
through a public `RitualHooks` trait. This is a breaking change to the `ritual` feature's
public surface; embedders need a small migration (see *Migration* below).

### Breaking changes — `ritual` feature (gid-core)

- **`run_ritual` signature replaced.**
  - **Old:** `run_ritual(ritual_id: &str, executor: &V2Executor) -> Result<RitualState>`
  - **New:** `run_ritual(state: RitualState, config: V2ExecutorConfig, hooks: Arc<dyn RitualHooks>) -> RitualOutcome`
  - The new function owns the state-machine loop end-to-end. Embedders no longer
    construct a `V2Executor` themselves; the loop builds one internally with the
    supplied hooks. (ISS-052 T08, design §5.6.)
- **`V2Executor::new` now requires `Arc<dyn RitualHooks>`.** `V2ExecutorConfig.notify`
  is still accepted for the legacy notification path but is now bridged into hooks
  internally. Set hooks to `Arc::new(NoopHooks::default())` if you don't need any
  embedder-specific behavior. (ISS-052 T02a/T02c.)
- **`RitualEvent::SkillFailed` extended with `reason: Option<SkillFailureReason>`.**
  Existing emit sites pass `reason: None`; new gates (subloop turn-limit, file-snapshot,
  workspace-unresolved) pass `Some(...)`. Pattern matches on `SkillFailed` will need
  to include the new field. (ISS-052 T02b, design §5.5.)
- **`RitualAction::SaveState` extended with `kind: SaveStateKind`** (`PhaseBoundary`
  vs `MidPhase`). Required so `V2Executor` can apply the right retry policy and
  fail-vs-degrade contract per ISS-051. (ISS-052 T04.)
- **`RitualState` extended with `persist_degraded: Option<PersistDegradedInfo>`**
  side-channel field. Serde-defaulted, so old state files load unchanged; live state
  in memory may now report transient persistence failures without halting the ritual.
  (ISS-052 T04, design §6.3.1.)
- **`pub use` graph in `ritual::mod`** added: `RitualHooks`, `WorkspaceError`,
  `CancelReason`, `CancelSource`, `NoopHooks`, `FailingPersistHooks`, `resume_ritual`,
  `UserEvent`, `RitualOutcome`, `RitualOutcomeStatus`. Downstream code that wildcard-imports
  may see new names; explicit imports are unaffected.

### Added — `ritual` feature

- **`RitualHooks` trait** — single extensibility seam for embedders (rustclaw):
  `try_run_skill`, `try_apply_review`, `notify`, `persist_state`, `resolve_workspace`,
  `stamp_metadata`, `should_cancel`, `on_phase_transition`. (ISS-052 T01.)
- **`NoopHooks` and `FailingPersistHooks`** — both `pub` for downstream test use.
  `FailingPersistHooks` is the canonical fixture for ISS-051 persistence-failure tests.
  (ISS-052 T01.)
- **`resume_ritual(state, UserEvent, config, hooks) -> RitualOutcome`** public API
  with `UserEvent` enum (`Cancel { reason }`, `Retry`, `SkipPhase`, `Clarification { msg }`,
  `Approval { decision }`). Lets embedders resume a paused ritual without touching
  the internal `state_machine::RitualEvent` type. (ISS-055 / ISS-052 T13b polish.)
- **New `RitualEvent` variants** (state-machine):
  - `Cancelled { reason: CancelReason }` — emitted when `hooks.should_cancel()`
    returns `Some(reason)`. (ISS-052 T02d/T02e.)
  - `StatePersisted { kind: SaveStateKind }` and `StatePersistFailed { kind, error }` —
    emitted around every `SaveState` action so embedders can observe persistence health.
    (ISS-052 T05.)
  - `WorkspaceUnresolved { error: WorkspaceError }` — emitted at ritual start if
    `hooks.resolve_workspace()` fails. (ISS-052 T05.)
- **`SaveState` retry wrapper** — bounded backoff (3 attempts, 50ms/200ms/1000ms),
  fail-the-phase on `PhaseBoundary` exhaustion, set `persist_degraded` on `MidPhase`
  exhaustion. Five consecutive `MidPhase` failures abort the ritual. (ISS-052 T03/T04,
  ISS-051 root-fix.)
- **Self-review subloop ported into `V2Executor`** — previously lived only in
  rustclaw. `V2Executor::run_skill` now drives review→apply→re-run iterations bounded
  by the subloop turn limit. Tests: `subloop_turn_limit_all_attempts_fails`. (ISS-052 T07.)
- **SKILL.md `file_policy` frontmatter** — declares post-condition gates
  (`requires_file_writes`, `forbids_file_writes`, `requires_specific_files`).
  `V2Executor::run_skill` enforces the policy via file-snapshot regardless of which
  hook produced the success event; this is the ISS-038 gate that previously bypassed
  rustclaw's parallel dispatcher. (ISS-052 T06.)
- **`should_cancel` polled between actions** in `V2Executor::execute_actions`,
  giving sub-second cancel latency without async cancellation tokens crossing the
  trait boundary. (ISS-052 T02e.)

### Added — non-ritual

- **`gid query impact|deps --confidence`** — confidence-aware traversal that
  exposes per-edge confidence scores in query output. (ISS-035.)
- **`gid repair`** — graph fix command for foreign-key drift, orphan edges, and
  source-tag normalization. (ISS-032.)
- **Configurable Infomap edge weights** — `gid infer --weights ...` and
  `infer.weights` config key. (ISS-002 [revived].)
- **Manual + inferred source nodes counted in `gid tasks` summary.** (ISS-047.)
- **`SqliteStore` foreign-keys assertion** at open + regression suite — closes
  a class of silent FK-disabled bugs. (ISS-033.)
- **`BatchOp::DeleteNode` removes incident edges** — previously left dangling edges.
  (ISS-037.)
- **`gid project` subcommands** exposed via CLI (list/add/remove/show). (ISS-028.)
- **Ritual launcher accepts `WorkUnit`** structured target instead of free-text
  paths; resolves through project registry. (ISS-029.)
- **Adapter PID stamping on `RitualState`** — daemon owns the ritual; orphan-sweep
  uses this. (ISS-030.)
- **Graph-phase mode dispatch + post-validation** in V2Executor. (ISS-039.)
- **`RitualV2Status` terminal disposition** field on `RitualState`. (Pre-052 prep.)

### Changed

- **Ritual implement-phase post-condition** now enforced via filesystem snapshot:
  a skill that claims success but writes zero files is now rejected with
  `SkillFailed { reason: Some(NoFileWrites) }`. (ISS-038 — gate now actually applies
  in production rituals; rustclaw's parallel dispatcher previously bypassed it.)
- **Pytest-id lookup** is path-aware again. (ISS-040.)
- **CLI handlers** packed into option structs to clear `clippy::too_many_arguments`
  and improve forward compatibility. (ISS-046 phase E2.)
- **`code_graph` extractors** refactored around per-language `ExtractCtx` /
  `CallCtx` structs (Rust, Python, TypeScript). Internal cleanup; no public API
  change. (ISS-046 phases A–D.)
- **Workspace clippy clean** — 192 → 0 warnings across `cargo clippy --workspace`.
  (ISS-042.)

### Fixed

- **`SqliteStore::open` PRAGMA `foreign_keys=ON` was inside a transaction**, which
  silently no-oped on some SQLite versions. Now applied before `BEGIN`. (ISS-015.)
- **Confidence ladder collapse** in Python call resolution (same_file == imported == 0.8).
  (ISS-043.)
- **`approx_constant` clippy errors** in test fixtures using `3.14`. (ISS-041.)
- **Telemetry name typo** `on_phase_transition_phase` → `on_phase_transition`.
  (FINDING-7 from ISS-052 design review.)

### Removed

- **Deprecated `COLOCATION_PAIRWISE_LIMIT`** re-export from `infer`. (ISS-045.)

### Migration

For embedders calling `run_ritual` directly:

```rust
// Before (0.3.x):
let executor = V2Executor::new(config, notify_fn);
let final_state = run_ritual(&ritual_id, &executor)?;

// After (0.4.0):
use std::sync::Arc;
use gid_core::ritual::{run_ritual, NoopHooks, RitualHooks, V2ExecutorConfig};

let hooks: Arc<dyn RitualHooks> = Arc::new(NoopHooks::default());
let outcome = run_ritual(initial_state, config, hooks).await;
match outcome.status {
    RitualOutcomeStatus::Done => { /* ok */ }
    RitualOutcomeStatus::Failed { reason } => { /* handle */ }
    RitualOutcomeStatus::Paused { awaiting } => { /* wait for user event */ }
}
```

Embedders that need custom skill dispatch (sub-agent runners), Telegram notifications,
or persistence overrides should implement `RitualHooks` and pass an `Arc<dyn RitualHooks>`
of their own. See `rustclaw/src/ritual/hooks.rs` for the reference implementation.

For `RitualEvent::SkillFailed` matchers, add the new field:

```rust
// Before:
RitualEvent::SkillFailed { skill, error } => { ... }
// After:
RitualEvent::SkillFailed { skill, error, reason } => { ... }
```

### Notes

- gid-core `V2ExecutorConfig.notify` field remains as a `#[deprecated]` shim for one
  release to ease migration. It is bridged into `hooks.notify(...)` internally. Removal
  scheduled for 0.5.0.
- gid-cli (`gid-dev-cli`) bumps to **0.4.0** to track gid-core's minor.

---

## [0.3.2] — 2026-0X-XX

- Switched `infomap-rs` and `agentctl-auth` from path to registry deps to satisfy
  crates.io publish requirements.

## [0.3.1] — 2026-0X-XX

- Added version specifier for `agentctl-auth` dep (publish requirement).

## [0.3.0] — 2026-0X-XX

- Merged `gid-harness` into `gid-core` behind feature flags (`harness`, `ritual`,
  `infomap`, `cli-llm`, `full`).
- Renamed `gid-cli` → `gid-dev-cli` for crates.io.
- Initial crates.io publish.

[0.4.0]: #040--2026-04-2x-tbd
[0.3.2]: #032--2026-0x-xx
[0.3.1]: #031--2026-0x-xx
[0.3.0]: #030--2026-0x-xx
