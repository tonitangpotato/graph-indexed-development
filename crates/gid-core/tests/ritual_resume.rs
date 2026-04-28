//! ISS-052 T13b prerequisite — `resume_ritual` integration tests.
//!
//! Companion to `ritual_e2e.rs` (which exercises `run_ritual`). These
//! tests prove the second public dispatcher entry point works from
//! outside the crate, with `NoopHooks`. They exist because T13b's
//! single-dispatcher invariant (AC3) requires embedders to drive
//! paused rituals forward via `resume_ritual` instead of re-implementing
//! the event loop themselves.
//!
//! ### What's covered
//!
//! 1. **`UserCancel` from a paused phase** — proves resume_ritual reaches
//!    a terminal state on the first injected event when the (state, event)
//!    pair routes straight to a terminal phase. No event loop iteration
//!    needed; the early-exit branch on `phase.is_terminal()` after the
//!    initial transition fires.
//!
//! 2. **`UserRetry` from `WaitingClarification`** — proves resume_ritual
//!    drives the full event loop when the injected event re-enters a
//!    working phase. Without an LLM client the next phase escalates,
//!    same pattern as `ritual_e2e.rs` proves for `run_ritual`. This
//!    confirms the shared `drive_event_loop` helper handles both entry
//!    points uniformly.
//!
//! 3. **No `stamp_metadata` re-stamp** — proves resume_ritual does NOT
//!    re-fire the once-only stamp_metadata hook. The hook records each
//!    invocation; we assert it stays at zero across resume.
//!
//! ### What's NOT covered here
//!
//! Per-event semantic correctness (e.g. "UserClarification preserves the
//! response in state") is the state machine's job and tested in
//! `state_machine.rs` unit tests. These tests only verify the
//! *dispatcher integration* — that resume_ritual correctly hands the
//! event to `transition`, drives the same event loop, and respects the
//! same terminal/paused break conditions as `run_ritual`.

#![cfg(all(feature = "ritual", feature = "sqlite"))]

use std::sync::Arc;

use gid_core::ritual::hooks::{NoopHooks, RitualHooks};
use gid_core::ritual::state_machine::{RitualPhase, RitualState};
use gid_core::ritual::v2_executor::{resume_ritual, RitualOutcomeStatus, UserEvent, V2ExecutorConfig};
use gid_core::ritual::work_unit::WorkUnit;

/// Build a paused `WaitingClarification` state with workspace already
/// resolved (mimics what would be loaded from disk by an embedder).
fn paused_state(workspace: &std::path::Path) -> RitualState {
    let mut state = RitualState::new();
    state.task = "fix the bug".to_string();
    state.work_unit = Some(WorkUnit::Task {
        project: "resume-test".into(),
        task_id: "T0".into(),
    });
    // target_root carries the already-resolved workspace path. resume_ritual
    // does NOT re-resolve, so this must be populated by the embedder before
    // calling resume.
    state.target_root = Some(workspace.to_string_lossy().into_owned());
    state.phase = RitualPhase::WaitingClarification;
    state
}

/// Resume + UserCancel on a paused ritual must reach `Cancelled`
/// terminal cleanly, draining final actions through the hook
/// dispatcher and producing a `Cancelled` outcome status.
#[tokio::test]
async fn resume_with_user_cancel_reaches_cancelled() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let workspace = tempdir.path().to_path_buf();
    let hooks = Arc::new(NoopHooks::new(workspace.clone(), workspace.clone()));

    let state = paused_state(&workspace);
    let config = V2ExecutorConfig {
        project_root: workspace.clone(),
        ..V2ExecutorConfig::default()
    };

    let outcome = resume_ritual(
        state,
        UserEvent::Cancel,
        config,
        hooks.clone() as Arc<dyn RitualHooks>,
    )
    .await;

    assert_eq!(
        outcome.status,
        RitualOutcomeStatus::Cancelled,
        "UserCancel from paused state must produce Cancelled outcome; got {:?}",
        outcome.status
    );
    assert_eq!(
        outcome.state.phase,
        RitualPhase::Cancelled,
        "final phase must be Cancelled"
    );

    // Final SaveState action must have fired even on the early-exit
    // path (proves the post-initial-transition action drain works).
    let expected_state_file = workspace.join(".gid").join("ritual-state.json");
    assert!(
        expected_state_file.exists(),
        "SaveState action must drain even when first transition lands in terminal phase"
    );
}

/// Resume + UserRetry from `WaitingClarification` must drive the full
/// event loop. Without an LLM client the re-triggered phase escalates,
/// proving `drive_event_loop` is the same loop as `run_ritual` — the
/// AC3 single-dispatcher invariant holds.
#[tokio::test]
async fn resume_with_user_retry_drives_event_loop() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let workspace = tempdir.path().to_path_buf();
    let hooks = Arc::new(NoopHooks::new(workspace.clone(), workspace.clone()));

    let state = paused_state(&workspace);
    let config = V2ExecutorConfig {
        project_root: workspace.clone(),
        ..V2ExecutorConfig::default()
    };

    let outcome = resume_ritual(
        state,
        UserEvent::Retry,
        config,
        hooks.clone() as Arc<dyn RitualHooks>,
    )
    .await;

    // UserRetry from WaitingClarification → re-triage path. With no
    // LLM the triage phase exhausts retries → Escalated. Same terminal
    // shape as ritual_e2e.rs, but reached via the resume entry point.
    assert_eq!(
        outcome.status,
        RitualOutcomeStatus::Escalated,
        "UserRetry without LLM must escalate cleanly through the event loop; \
         got {:?} (state.error_context = {:?})",
        outcome.status,
        outcome.state.error_context
    );

    // ≥1 notification proves Notify actions dispatched through the
    // hook trait during the loop iterations (not just the initial
    // transition's actions).
    let notifications = hooks.notifications_snapshot();
    assert!(
        !notifications.is_empty(),
        "resume_ritual event loop must dispatch Notify actions through hooks.notify()"
    );
}

/// resume_ritual must NOT call `stamp_metadata` — that hook fires
/// exactly once at ritual *start*, not on resumption (FINDING-12 / §4
/// invariant). Assert by checking NoopHooks' stamp counter stays 0.
#[tokio::test]
async fn resume_does_not_restamp_metadata() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let workspace = tempdir.path().to_path_buf();
    let hooks = Arc::new(NoopHooks::new(workspace.clone(), workspace.clone()));

    let state = paused_state(&workspace);
    let config = V2ExecutorConfig {
        project_root: workspace.clone(),
        ..V2ExecutorConfig::default()
    };

    let _ = resume_ritual(
        state,
        UserEvent::Cancel,
        config,
        hooks.clone() as Arc<dyn RitualHooks>,
    )
    .await;

    assert_eq!(
        hooks.stamp_metadata_count(),
        0,
        "resume_ritual must NOT re-stamp metadata; once-only invariant violated"
    );
}
