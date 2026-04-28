//! Ritual hook trait — adapter-side extension points for `V2Executor`.
//!
//! Per ISS-052 design §4. The `RitualHooks` trait is the *only* mechanism by
//! which an embedder (rustclaw, the gid CLI, integration tests) injects its
//! own behaviour into a ritual run. Implementations are owned by the embedder
//! and passed to `V2Executor` (T02) as `Arc<dyn RitualHooks>`.
//!
//! # Design intent
//!
//! Before this trait existed, rustclaw maintained a parallel
//! `RitualAction` dispatcher (see ISS-052 root cause). Quality gates added to
//! `V2Executor` (file_snapshot, save_state retry, etc.) silently never ran in
//! production. By concentrating *all* dispatch in `V2Executor` and allowing
//! adapters to plug in only at well-defined hook points, gates are guaranteed
//! to run for every embedder.
//!
//! Hook surface deliberately splits into:
//! - **Side-effect channels** (`notify`, `persist_state`) — async, IO-shaped.
//! - **Resolution / configuration** (`resolve_workspace`, `stamp_metadata`) —
//!   sync, called once at boundaries.
//! - **Lifecycle observation** (`on_action_start`/`on_action_finish`/
//!   `on_phase_transition`) — sync, default no-op, used for tracing / Engram
//!   side-writes.
//! - **Cancellation** (`should_cancel`) — sync poll, between-actions only.
//!
//! See ISS-052 design §4 (full signature) and §4.1/4.2 (test doubles).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;

use super::state_machine::{RitualAction, RitualEvent, RitualPhase, RitualState};
use super::work_unit::WorkUnit;

// ═══════════════════════════════════════════════════════════════════════════════
// Trait
// ═══════════════════════════════════════════════════════════════════════════════

/// Adapter hooks invoked by `V2Executor` at side-effect boundaries.
///
/// Implementations are owned by the embedder (rustclaw, gid CLI, tests) and
/// passed to `V2Executor` as `Arc<dyn RitualHooks>`.
///
/// # Threading
/// `V2Executor` calls hooks from its async task. Implementations must be
/// `Send + Sync`. Methods are async only where the embedder is likely to
/// perform IO (`notify`, `persist_state`); pure-data methods stay sync.
///
/// # Failure semantics
/// - `notify` failure: logged by `V2Executor`, ritual continues. Notifications
///   are best-effort.
/// - `persist_state` failure: `V2Executor` emits `StatePersistFailed` event;
///   state machine handles via §6 retry/escalate logic. Hooks MUST return
///   the underlying IO error rather than swallowing.
/// - `resolve_workspace` failure: ritual aborts with `WorkspaceUnresolved`
///   event. No retry — config error, not transient.
#[async_trait]
pub trait RitualHooks: Send + Sync {
    // ── Side-effect channels ───────────────────────────────────────────

    /// Send a user-facing notification (e.g. Telegram message, log line).
    /// Best-effort. Errors logged but do not fail the ritual.
    async fn notify(&self, message: &str);

    /// Durably persist ritual state. MUST return error on IO failure.
    /// `V2Executor` turns errors into `StatePersistFailed` events.
    ///
    /// # Atomicity contract
    /// Implementations MUST write atomically (write-to-tempfile + `rename(2)`,
    /// or equivalent platform primitive). Partial writes are a contract
    /// violation: if the function returns `Err`, on-disk state must be either
    /// the previous successful version or absent — never a truncated or
    /// half-serialized blob. `V2Executor`'s retry loop (§6.2) overwrites on
    /// success, so non-atomic implementations would corrupt the file across
    /// retries (attempt 1 partial → attempt 2 different partial → attempt 3
    /// fail → on-disk file is unparseable, ritual unrecoverable).
    async fn persist_state(&self, state: &RitualState) -> std::io::Result<()>;

    // ── Resolution / configuration ─────────────────────────────────────

    /// Resolve the project workspace root for this work unit.
    /// Called once at ritual start. Errors abort the ritual.
    fn resolve_workspace(&self, work_unit: &WorkUnit) -> Result<PathBuf, WorkspaceError>;

    /// Stamp adapter-specific metadata into state at start
    /// (e.g. daemon PID, hostname, adapter version).
    /// Called once on transition into the first non-Idle state.
    ///
    /// # Contract
    ///
    /// The default no-op implementation is **semantically legal**, not a
    /// missing TODO. `V2Executor` never consumes `state.metadata` to make
    /// dispatch decisions — metadata exists purely for post-hoc
    /// observability (debugging, audit, telemetry). Therefore:
    ///
    /// - **Short-lived / CLI embedders** (e.g. `gid` CLI): may rely on the
    ///   default. The process identity is implicit in `state.created_at`
    ///   and the invocation context.
    /// - **Daemon-shaped embedders** (e.g. rustclaw): *should* override to
    ///   stamp PID, hostname, adapter version, and any other context that
    ///   would be useful for crash forensics. This is recommended, not
    ///   required.
    ///
    /// If a future change to `V2Executor` ever needs to read stamped
    /// metadata to drive control flow, this contract becomes a hazard and
    /// the default must be removed (turning `stamp_metadata` into a
    /// required method). At that point the design doc and this comment
    /// should be updated together.
    fn stamp_metadata(&self, _state: &mut RitualState) {}

    // ── Lifecycle observation ──────────────────────────────────────────

    /// Called immediately before each `RitualAction` is dispatched.
    /// Default: no-op. Used for tracing, metrics, Engram side-writes.
    fn on_action_start(&self, _action: &RitualAction, _state: &RitualState) {}

    /// Called immediately after the action's `RitualEvent` is produced
    /// (before it is applied to state). Default: no-op.
    fn on_action_finish(&self, _action: &RitualAction, _event: &RitualEvent) {}

    /// Called on every state phase transition (e.g. `Implementing → Verifying`).
    /// Default: no-op. Receives only the phase enum (not full `RitualState`)
    /// because most embedders only care about transition shape, not state
    /// payload.
    fn on_phase_transition(&self, _from: &RitualPhase, _to: &RitualPhase) {}

    // ── Cancellation ───────────────────────────────────────────────────

    /// Polled between actions. Return `Some(reason)` to request cooperative
    /// cancellation. Default: never cancel.
    ///
    /// Cancellation produces a `Cancelled { reason }` event; ritual
    /// transitions to terminal Cancelled state, runs Cleanup action,
    /// persists, exits.
    ///
    /// # Latency
    /// Cancellation is polled **between** actions, not within them. A single
    /// `RunSkill` action can run for the duration of the LLM skill timeout
    /// (typically up to 10 minutes). Therefore the worst-case observed
    /// latency from `/ritual cancel` to ritual termination is bounded by the
    /// longest in-flight action. This is an accepted trade-off for the
    /// simpler poll model; mid-action cancellation is out of scope and
    /// tracked as a follow-up if it surfaces as a real UX problem.
    fn should_cancel(&self) -> Option<CancelReason> { None }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Errors and small types
// ═══════════════════════════════════════════════════════════════════════════════

/// Workspace resolution error from `RitualHooks::resolve_workspace`.
///
/// Treated as terminal by `V2Executor` — there is no retry, because workspace
/// misconfiguration is not transient.
#[derive(Debug, Clone, thiserror::Error)]
pub enum WorkspaceError {
    #[error("project not found in registry: {0}")]
    NotFound(String),
    #[error("registry read failed: {0}")]
    RegistryError(String),
    #[error("path does not exist: {}", .0.display())]
    PathMissing(PathBuf),
}

/// Cancellation request returned from `RitualHooks::should_cancel`.
#[derive(Debug, Clone)]
pub struct CancelReason {
    pub source: CancelSource,
    pub message: String,
}

/// Origin of a cancellation request — used for telemetry / reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelSource {
    /// Explicit user command (e.g. `/ritual cancel`).
    UserCommand,
    /// Adapter-imposed wall-clock limit reached.
    Timeout,
    /// Host process is shutting down.
    DaemonShutdown,
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test doubles (public — also used downstream by integration tests)
// ═══════════════════════════════════════════════════════════════════════════════

/// No-op hooks for `gid-core` integration tests and any embedder that wants
/// the default behaviour.
///
/// - `notify` records messages into `notifications` (so tests can assert).
/// - `persist_state` writes to `<persist_dir>/<ritual_id>.json` atomically.
/// - `resolve_workspace` returns the configured `workspace` regardless of
///   `WorkUnit` contents.
/// - `should_cancel` returns `Some(...)` exactly once after `cancel_requested`
///   is set, then `None` (so a single ritual loop iteration observes the
///   cancellation but the executor does not enter a tight loop). See
///   `request_cancel` for the helper.
pub struct NoopHooks {
    pub workspace: PathBuf,
    pub persist_dir: PathBuf,
    pub notifications: Mutex<Vec<String>>,
    pub cancel_requested: AtomicBool,
    /// Test-only counter: increments on every `stamp_metadata` call.
    /// Exercised by ISS-052 T13b to assert the once-only invariant
    /// (FINDING-12 / §4) — `run_ritual` increments it to 1, `resume_ritual`
    /// must NOT increment it.
    pub stamp_metadata_calls: AtomicUsize,
}

impl NoopHooks {
    /// Construct a new NoopHooks rooted at the given workspace.
    /// `persist_dir` is created if missing on first `persist_state` call.
    pub fn new(workspace: PathBuf, persist_dir: PathBuf) -> Self {
        Self {
            workspace,
            persist_dir,
            notifications: Mutex::new(Vec::new()),
            cancel_requested: AtomicBool::new(false),
            stamp_metadata_calls: AtomicUsize::new(0),
        }
    }

    /// How many times `stamp_metadata` has been invoked on this hook
    /// instance. Used by resume tests to verify the once-only invariant.
    pub fn stamp_metadata_count(&self) -> usize {
        self.stamp_metadata_calls.load(Ordering::SeqCst)
    }

    /// Request cancellation. The next call to `should_cancel` will return
    /// `Some(UserCommand)` and subsequently flip the flag back so further
    /// polls return `None`.
    pub fn request_cancel(&self) {
        self.cancel_requested.store(true, Ordering::SeqCst);
    }

    /// Snapshot collected notifications (clones the inner Vec).
    pub fn notifications_snapshot(&self) -> Vec<String> {
        self.notifications
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl RitualHooks for NoopHooks {
    async fn notify(&self, message: &str) {
        if let Ok(mut g) = self.notifications.lock() {
            g.push(message.to_string());
        }
    }

    async fn persist_state(&self, state: &RitualState) -> std::io::Result<()> {
        // Atomic write: tempfile in same dir + rename.
        std::fs::create_dir_all(&self.persist_dir)?;
        let final_path = self.persist_dir.join(format!("{}.json", state.id));
        let tmp_path = self.persist_dir.join(format!(".{}.json.tmp", state.id));
        let json = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
        std::fs::write(&tmp_path, json)?;
        std::fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }

    fn resolve_workspace(&self, _work_unit: &WorkUnit) -> Result<PathBuf, WorkspaceError> {
        if !self.workspace.exists() {
            return Err(WorkspaceError::PathMissing(self.workspace.clone()));
        }
        Ok(self.workspace.clone())
    }

    fn should_cancel(&self) -> Option<CancelReason> {
        // CAS: read once and clear, so the same cancel request isn't re-emitted.
        if self
            .cancel_requested
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            Some(CancelReason {
                source: CancelSource::UserCommand,
                message: "NoopHooks::request_cancel".into(),
            })
        } else {
            None
        }
    }

    fn stamp_metadata(&self, _state: &mut RitualState) {
        // Test-only counter: lets resume_ritual tests assert the once-only
        // invariant (FINDING-12 / §4). The base trait's default is a no-op;
        // we override only to count.
        self.stamp_metadata_calls.fetch_add(1, Ordering::SeqCst);
    }
}

/// Hooks that succeed N times then fail every subsequent `persist_state` call.
/// Exercises the §6 save_state retry / degrade paths in T03/T04.
///
/// `notify` and `resolve_workspace` behave like `NoopHooks`. Only persistence
/// is rigged.
pub struct FailingPersistHooks {
    pub fail_after_n_calls: AtomicUsize,
    pub call_count: AtomicUsize,
    pub workspace: PathBuf,
}

impl FailingPersistHooks {
    /// `fail_after_n_calls = 0` → fail on the very first call.
    /// `fail_after_n_calls = 3` → calls 1, 2, 3 succeed; call 4 onwards fail.
    pub fn new(workspace: PathBuf, fail_after_n_calls: usize) -> Self {
        Self {
            workspace,
            fail_after_n_calls: AtomicUsize::new(fail_after_n_calls),
            call_count: AtomicUsize::new(0),
        }
    }

    pub fn calls_observed(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl RitualHooks for FailingPersistHooks {
    async fn notify(&self, _message: &str) {}

    async fn persist_state(&self, _state: &RitualState) -> std::io::Result<()> {
        let n = self.call_count.fetch_add(1, Ordering::SeqCst) + 1;
        let threshold = self.fail_after_n_calls.load(Ordering::SeqCst);
        if n > threshold {
            Err(std::io::Error::other(format!(
                "FailingPersistHooks: forced failure on call {n} (threshold {threshold})"
            )))
        } else {
            Ok(())
        }
    }

    fn resolve_workspace(&self, _work_unit: &WorkUnit) -> Result<PathBuf, WorkspaceError> {
        if !self.workspace.exists() {
            return Err(WorkspaceError::PathMissing(self.workspace.clone()));
        }
        Ok(self.workspace.clone())
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// `RitualHooks` must be object-safe — embedders pass `Arc<dyn RitualHooks>`
    /// to `V2Executor`. This test will fail to compile if anyone accidentally
    /// adds a generic / `Self: Sized` method.
    #[test]
    fn ritual_hooks_is_object_safe() {
        let tmp = std::env::temp_dir();
        let hooks: Arc<dyn RitualHooks> = Arc::new(NoopHooks::new(tmp.clone(), tmp));
        // Just touch a method to ensure dispatch compiles.
        let _ = hooks.should_cancel();
    }

    #[tokio::test]
    async fn noop_hooks_records_notifications() {
        let tmp = std::env::temp_dir();
        let hooks = NoopHooks::new(tmp.clone(), tmp);
        hooks.notify("hello").await;
        hooks.notify("world").await;
        let snap = hooks.notifications_snapshot();
        assert_eq!(snap, vec!["hello".to_string(), "world".to_string()]);
    }

    #[tokio::test]
    async fn noop_hooks_persists_state_atomically() {
        let tmpdir = tempfile::tempdir().expect("tempdir");
        let hooks = NoopHooks::new(tmpdir.path().to_path_buf(), tmpdir.path().to_path_buf());
        let mut state = RitualState::new();
        state.id = "r-test01".into();
        hooks.persist_state(&state).await.expect("persist");
        let final_path = tmpdir.path().join("r-test01.json");
        assert!(final_path.exists(), "final json must exist");
        // Tempfile must be cleaned up (renamed away).
        let tmp_path = tmpdir.path().join(".r-test01.json.tmp");
        assert!(!tmp_path.exists(), "temp file must be renamed away");
        // File content must round-trip.
        let body = std::fs::read_to_string(&final_path).expect("read");
        let parsed: RitualState = serde_json::from_str(&body).expect("parse");
        assert_eq!(parsed.id, "r-test01");
    }

    #[test]
    fn noop_hooks_should_cancel_one_shot() {
        let tmp = std::env::temp_dir();
        let hooks = NoopHooks::new(tmp.clone(), tmp);
        assert!(hooks.should_cancel().is_none(), "no cancel by default");
        hooks.request_cancel();
        let first = hooks.should_cancel();
        assert!(first.is_some(), "request_cancel should produce one Some");
        assert_eq!(first.unwrap().source, CancelSource::UserCommand);
        assert!(hooks.should_cancel().is_none(), "subsequent polls return None");
    }

    #[tokio::test]
    async fn noop_hooks_resolve_workspace_returns_configured_path() {
        let tmpdir = tempfile::tempdir().expect("tempdir");
        let hooks = NoopHooks::new(tmpdir.path().to_path_buf(), tmpdir.path().to_path_buf());
        let wu = WorkUnit::Issue {
            project: "rustclaw".into(),
            id: "ISS-051".into(),
        };
        let resolved = hooks.resolve_workspace(&wu).expect("resolve");
        assert_eq!(resolved, tmpdir.path());
    }

    #[tokio::test]
    async fn noop_hooks_resolve_workspace_missing_path_errors() {
        let bogus = PathBuf::from("/nonexistent/path/for/test/12345xyz");
        let hooks = NoopHooks::new(bogus.clone(), bogus.clone());
        let wu = WorkUnit::Issue {
            project: "rustclaw".into(),
            id: "ISS-051".into(),
        };
        match hooks.resolve_workspace(&wu) {
            Err(WorkspaceError::PathMissing(p)) => assert_eq!(p, bogus),
            other => panic!("expected PathMissing, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn failing_persist_hooks_succeeds_then_fails() {
        let tmp = std::env::temp_dir();
        let hooks = FailingPersistHooks::new(tmp, /* fail_after_n_calls */ 2);
        let mut state = RitualState::new();
        state.id = "r-fp01".into();
        // Calls 1 and 2 succeed.
        hooks.persist_state(&state).await.expect("call 1 ok");
        hooks.persist_state(&state).await.expect("call 2 ok");
        // Call 3 fails.
        let err = hooks
            .persist_state(&state)
            .await
            .expect_err("call 3 must fail");
        assert!(err.to_string().contains("forced failure"));
        assert_eq!(hooks.calls_observed(), 3);
    }

    #[tokio::test]
    async fn failing_persist_hooks_zero_threshold_fails_immediately() {
        let tmp = std::env::temp_dir();
        let hooks = FailingPersistHooks::new(tmp, /* fail_after_n_calls */ 0);
        let mut state = RitualState::new();
        state.id = "r-fp02".into();
        let err = hooks.persist_state(&state).await.expect_err("must fail");
        assert!(err.to_string().contains("forced failure"));
    }

    /// Default `on_*` lifecycle hooks must be no-ops and not panic when
    /// invoked through a `dyn RitualHooks`.
    #[test]
    fn default_lifecycle_hooks_are_noops() {
        let tmp = std::env::temp_dir();
        let hooks: Arc<dyn RitualHooks> = Arc::new(NoopHooks::new(tmp.clone(), tmp));
        let action = RitualAction::DetectProject;
        let state = RitualState::new();
        let event = RitualEvent::ProjectDetected(super::super::state_machine::ProjectState {
            has_requirements: false,
            has_design: false,
            has_graph: false,
            has_source: false,
            has_tests: false,
            language: Some("rust".into()),
            source_file_count: 0,
            verify_command: Some("cargo build".into()),
        });
        // None of these should panic.
        hooks.on_action_start(&action, &state);
        hooks.on_action_finish(&action, &event);
        hooks.on_phase_transition(&RitualPhase::Idle, &RitualPhase::Initializing);
        let mut state2 = state.clone();
        hooks.stamp_metadata(&mut state2);
    }
}
