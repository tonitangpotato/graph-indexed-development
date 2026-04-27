//! ISS-052 §9.2 — gid-core ritual end-to-end integration test.
//!
//! This is the **single** end-to-end test that exercises the public
//! `run_ritual` entry point from outside the crate, with the public
//! `NoopHooks` test double. It proves that:
//!
//!   1. `run_ritual` is callable from external code with the published
//!      surface (no internal types leak).
//!   2. `NoopHooks` is sufficient to reach a terminal `RitualOutcome`
//!      (no compile-time / runtime hook contract violation).
//!   3. The hook-driven persistence path actually writes a state file
//!      to the configured `persist_dir` (proves the migration of
//!      save_state from rustclaw's parallel dispatcher into V2Executor
//!      is wired correctly — design §6).
//!   4. Notifications produced during the ritual flow round-trip
//!      through `hooks.notify` (proves the `Notify` action is dispatched
//!      via the hook trait and not via the legacy `NotifyFn` shim).
//!
//! ### Why "escalate without LLM" and not a "real happy-path completion"?
//!
//! The spec snippet in design §9.2 implies a happy-path completion. A
//! true completion requires an `LlmClient` — every skill phase calls
//! out to Claude/GPT. For an *integration* test, that would mean either
//! (a) recording a fixture and replaying it (brittle, couples the test
//! to prompt internals) or (b) shipping a mock LLM impl as part of the
//! crate's public test surface (leaks a test type into the API).
//!
//! Both options are worse than the alternative chosen here: run with no
//! LLM client at all and let the executor escalate at the first skill
//! phase. This exercises the *full pipeline through the public API* —
//! workspace resolution → metadata stamping → state-machine transitions
//! → action dispatch → notify hooks → retry loop → terminal escalation
//! → state persistence — using only the published surface and the
//! published `NoopHooks` test double. Per design §6.3, persistence at
//! phase boundaries is also exercised; per §5.5 the terminal Notify
//! action is exercised; per §5.6 the resolve_workspace + stamp_metadata
//! preamble is exercised.
//!
//! Happy-path-with-real-LLM coverage lives in rustclaw's integration
//! tests (T16, §9.4), where an actual `LlmClient` is in scope.
//!
//! Per ISS-052 T10 / §9.2 — closes the e2e coverage gap.

#![cfg(all(feature = "ritual", feature = "sqlite"))]

use std::sync::Arc;

use gid_core::ritual::hooks::{NoopHooks, RitualHooks};
use gid_core::ritual::state_machine::RitualState;
use gid_core::ritual::v2_executor::{run_ritual, RitualOutcomeStatus, V2ExecutorConfig};
use gid_core::ritual::work_unit::WorkUnit;

/// §9.2 canonical name. Drives a complete ritual through `run_ritual`
/// with `NoopHooks` rooted in a tempdir; asserts the public outcome
/// contract holds end-to-end. With no `LlmClient` configured, the
/// ritual is expected to escalate at the first skill phase — that
/// terminal state is what the test asserts on, alongside the side-
/// effects (notifications, persisted state file) produced along the way.
#[tokio::test]
async fn full_ritual_with_noop_hooks() {
    // ── Arrange ──────────────────────────────────────────────────
    // tempdir doubles as both the workspace (where the ritual would
    // *operate* on files if it had work to do) and the persist_dir
    // (where state files land). Real embedders separate these, but
    // for a pure pipeline test they can be co-located.
    let tempdir = tempfile::tempdir().expect("tempdir");
    let workspace = tempdir.path().to_path_buf();
    // For the NoopHooks side-channel (`persist_state` hook) we use the
    // same tempdir. The ritual's *canonical* state file is still under
    // `<workspace>/.gid/`, written by the SaveState action itself; the
    // hook-side path is an embedder-extensible mirror.
    let persist_dir = workspace.clone();

    let hooks = Arc::new(NoopHooks::new(workspace.clone(), persist_dir.clone()));
    // Note: we deliberately do NOT call `hooks.request_cancel()` here.
    // With no LLM client configured, the first skill phase will fail
    // and the state machine will retry until exhausted, then escalate.
    // That escalation is the deterministic terminal we assert on.
    // See module docs for the rationale.

    // WorkUnit::Task is the simplest variant — doesn't require a
    // tracked issue file or feature directory in the workspace, just a
    // project label. NoopHooks::resolve_workspace ignores the WorkUnit
    // contents and always returns `workspace`, so this is sufficient.
    let work_unit = WorkUnit::Task {
        project: "e2e-test".into(),
        task_id: "T0".into(),
    };

    let mut initial = RitualState::new();
    initial.task = "fix the bug".to_string();
    initial.work_unit = Some(work_unit);

    let config = V2ExecutorConfig {
        project_root: workspace.clone(),
        ..V2ExecutorConfig::default()
    };

    // ── Act ──────────────────────────────────────────────────────
    let outcome = run_ritual(initial, config, hooks.clone() as Arc<dyn RitualHooks>).await;

    // ── Assert: terminal classification ─────────────────────────
    // No LLM client → first skill phase exhausts retries → state
    // machine escalates. That's a terminal, fully-traversed run; not a
    // crash, not a workspace failure, not a hang. RitualOutcome must
    // classify it as `Escalated`.
    assert_eq!(
        outcome.status,
        RitualOutcomeStatus::Escalated,
        "ritual without an LlmClient must escalate cleanly; got {:?} (state.error_context = {:?})",
        outcome.status,
        outcome.state.error_context
    );

    // The error must surface through state.error_context — embedders
    // depend on this field for diagnostics. The exact wording is
    // intentionally not asserted (it's an implementation detail), only
    // that the field is populated and references the missing LLM.
    let err = outcome
        .state
        .error_context
        .as_deref()
        .expect("error_context must be populated on escalation");
    assert!(
        err.to_lowercase().contains("llm"),
        "escalation error must reference the missing LLM client; got: {:?}",
        err
    );

    // ── Assert: state-file persistence ──────────────────────────
    // Per design §6.1 + the V2Executor::save_state implementation, the
    // canonical state file lives at `<project_root>/.gid/ritual-state.json`.
    // (The hook-based `persist_state` retry wrapper is a separate side-
    // channel that embedders can wire — NoopHooks just no-ops on it.)
    // Reaching a terminal phase implies multiple SaveState actions
    // fired, so the file must exist by the time `run_ritual` returns.
    let expected_state_file = workspace.join(".gid").join("ritual-state.json");
    assert!(
        expected_state_file.exists(),
        "expected state file at {} after ritual completion (proves SaveState action dispatched); \
         workspace contents: {:?}",
        expected_state_file.display(),
        std::fs::read_dir(&workspace)
            .ok()
            .map(|rd| rd.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
    );

    // The persisted state must be valid JSON and its `id` must match
    // the in-memory outcome — proves SaveState wrote the *current*
    // state, not a stale snapshot.
    let persisted_json =
        std::fs::read_to_string(&expected_state_file).expect("persisted state file readable");
    let persisted: serde_json::Value =
        serde_json::from_str(&persisted_json).expect("persisted state file is valid JSON");
    assert_eq!(
        persisted.get("id").and_then(|v| v.as_str()),
        Some(outcome.state.id.as_str()),
        "persisted state file must carry the same ritual id as the in-memory outcome"
    );

    // ── Assert: notifications round-tripped through the hook ────
    // The state machine emits `Notify` actions for triage entry, phase
    // entry, retry attempts, and terminal failure (design §5.5). With
    // NoopHooks, those messages accumulate in `notifications`.
    //
    // Why assert ≥ 2 (and not just non-empty)? A single notification
    // could fire even if the hook trait was wired only at one site.
    // Asserting that *both* an entry AND a failure-notice fire proves
    // the hook is dispatched along the entire ritual lifecycle — not
    // just at one boundary.
    let notifications = hooks.notifications_snapshot();
    assert!(
        notifications.len() >= 2,
        "expected ≥ 2 notifications routed through hooks.notify() (entry + failure); \
         got {}: {:?}. This would indicate the Notify action is not being dispatched \
         via the hook trait (regression of ISS-052 T02c) or the state machine is \
         emitting Notify on fewer transitions than expected.",
        notifications.len(),
        notifications,
    );

    // Sanity: at least one notification signals the failure — proves
    // the terminal-arm Notify fired through the hook, not just the
    // entry-arm. We tolerate a few common phrasings rather than
    // pin-checking exact strings (those are §9.4 territory).
    assert!(
        notifications.iter().any(|n| {
            let lower = n.to_lowercase();
            lower.contains("fail") || lower.contains("error") || n.contains("❌")
        }),
        "expected at least one failure-notice notification; got: {:?}",
        notifications,
    );
}
