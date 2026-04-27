# Changelog — gid-core

All notable changes to the `gid-core` crate are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Breaking

- **`ritual::run_ritual` signature changed (ISS-052 T08).** The previous
  form `run_ritual(task: &str, executor: &V2Executor) -> Result<RitualState>`
  is gone. The new signature is:

  ```rust
  pub async fn run_ritual(
      initial_state: RitualState,
      config: V2ExecutorConfig,
      hooks: Arc<dyn RitualHooks>,
  ) -> RitualOutcome
  ```

  Migration:
  - Build a `RitualState` with `work_unit` set (`WorkUnit::Issue` /
    `WorkUnit::Feature` / `WorkUnit::Task`) — this is now the required
    way to identify the ritual target (ISS-029).
  - Pass a `V2ExecutorConfig` directly instead of constructing the
    executor yourself.
  - Provide an `Arc<dyn RitualHooks>` implementation. `NoopHooks` is
    available for tests; embedders typically implement their own
    (e.g. rustclaw will provide `RustclawHooks` in ISS-052 T11).
  - Match on `RitualOutcome { state, status }` instead of
    `Result<RitualState>`. The new `RitualOutcomeStatus` enum
    distinguishes `Completed`, `Cancelled`, `Escalated`, `Paused`,
    `IterationLimitExceeded`, and `WorkspaceFailed` — the embedder no
    longer has to inspect `state.phase` to make these decisions.

  Rationale: pre-phase failures (workspace resolution) and terminal
  classification belong in the return type, not in `Result::Err`.
  Hooks are now passed explicitly so the dispatcher is fully
  observable from the call site (G3 — no silent swallow).

### Added

- `ritual::RitualOutcome` and `ritual::RitualOutcomeStatus` — terminal
  classification of a ritual run.
- `RitualHooks::stamp_metadata` is now called exactly once at the
  start of `run_ritual` (FINDING-12 / design §5.6.2). Embedders use
  this to record pid, adapter id, host info.
- `RitualHooks::on_phase_transition(&from, &to)` fires for every
  actual phase change inside `run_ritual`'s main loop, not on every
  state mutation (design §4 / §5.6.2).
- `RitualHooks::resolve_workspace` is now called at the start of
  `run_ritual`; failures route through
  `RitualEvent::WorkspaceUnresolved` and produce a
  `RitualOutcomeStatus::WorkspaceFailed` outcome instead of a panic
  or `Result::Err`.

### Internal

- `V2Executor::execute_actions` was demoted from `pub` to
  `pub(crate)` (design §5.6.3). External callers should drive
  rituals via `run_ritual`; per-action dispatch is an internal
  invariant of gid-core.
- **ISS-052 T09** — §9.1 16-test contract complete. The hook-coverage
  unit tests in `crates/gid-core/src/ritual/v2_executor.rs::tests`
  now use the canonical spec names exactly: `hook_dispatch_called_once`,
  `notify_routed_through_hook`, `cancel_polled_between_actions`,
  `persist_retry_succeeds_on_attempt_3`, `persist_retry_exhausted`,
  `persist_failed_at_phase_boundary`, `skill_required_zero_files_fails`,
  `skill_forbidden_with_files_fails`, `subloop_turn_limit_all_attempts_fails`,
  `subloop_recovers_on_turn_3`, `workspace_unresolved_aborts`,
  `phase_transition_hook_called_on_each_change`, `stamp_metadata_called_once_at_start`,
  `phase_boundary_persist_fail_aborts`, `mid_phase_persist_fail_degrades`,
  `persist_degraded_5_failures_aborts`. Eight pre-existing tests were
  renamed (no body changes); seven new tests close the wrapper↔state-
  machine coverage gaps for §6.3.3 boundary/periodic/5-strike branches.
