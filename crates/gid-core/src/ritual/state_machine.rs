//! Ritual v2 — Pure Function State Machine.
//!
//! Core design: `transition(state, event) -> (new_state, actions)`
//! Zero IO. Zero dependencies. 100% unit-testable.
//!
//! Invariant: every transition produces either a terminal state OR exactly 1 event-producing action.
//! Terminal states: Done, Escalated, Cancelled.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};
use super::hooks::CancelReason;

// ═══════════════════════════════════════════════════════════════════════════════
// States
// ═══════════════════════════════════════════════════════════════════════════════

/// Ritual phase — the current state of the state machine.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RitualPhase {
    Idle,
    Initializing,
    Triaging,
    WaitingClarification,
    /// Writing requirements document (for large tasks).
    WritingRequirements,
    Designing,
    /// Reviewing a document produced by the previous phase (requirements, design, tasks).
    /// `review_target` in state tracks what's being reviewed and where to go next.
    Reviewing,
    /// Waiting for human to approve review findings before applying changes.
    WaitingApproval,
    Planning,
    Graphing,
    Implementing,
    Verifying,
    Done,
    Escalated,
    Cancelled,
}

impl RitualPhase {
    /// Human-readable display name.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Initializing => "Initializing",
            Self::Triaging => "Triage",
            Self::WaitingClarification => "Waiting for Clarification",
            Self::WritingRequirements => "Requirements",
            Self::Designing => "Design",
            Self::Reviewing => "Reviewing",
            Self::WaitingApproval => "Waiting for Approval",
            Self::Planning => "Planning",
            Self::Graphing => "Graph",
            Self::Implementing => "Implement",
            Self::Verifying => "Verify",
            Self::Done => "Done",
            Self::Escalated => "Escalated",
            Self::Cancelled => "Cancelled",
        }
    }

    /// Next phase in normal flow (for skip).
    pub fn next(&self) -> Option<RitualPhase> {
        match self {
            Self::Initializing => Some(Self::Triaging),
            Self::Triaging => Some(Self::Designing),
            Self::WaitingClarification => Some(Self::WritingRequirements),
            Self::WritingRequirements => Some(Self::Reviewing),
            Self::Designing => Some(Self::Reviewing),
            Self::Reviewing => Some(Self::WaitingApproval),
            Self::WaitingApproval => Some(Self::Planning),
            Self::Planning => Some(Self::Graphing),
            Self::Graphing => Some(Self::Reviewing),
            Self::Implementing => Some(Self::Verifying),
            Self::Verifying => Some(Self::Done),
            _ => None,
        }
    }

    /// Whether this is a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Escalated | Self::Cancelled)
    }

    /// Map a terminal phase to its `RitualV2Status` value.
    /// Returns `None` for non-terminal phases (caller should keep `Active`).
    pub fn terminal_status(&self) -> Option<RitualV2Status> {
        match self {
            Self::Done => Some(RitualV2Status::Done),
            Self::Cancelled => Some(RitualV2Status::Cancelled),
            Self::Escalated => Some(RitualV2Status::Failed),
            _ => None,
        }
    }

    /// Whether this is a pause state (waiting for user input, no EP actions expected).
    pub fn is_paused(&self) -> bool {
        matches!(self, Self::WaitingClarification | Self::WaitingApproval)
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Status (terminal disposition)
// ═══════════════════════════════════════════════════════════════════════════════

/// Top-level disposition of a v2 ritual.
///
/// Distinct from [`RitualPhase`]: phase is the FSM position; status is the
/// terminal classification. While the ritual is in flight, status is `Active`.
/// On entry to a terminal phase (`Done`/`Cancelled`/`Escalated`), `with_phase`
/// updates this to the corresponding terminal value.
///
/// Readers (CLI, dashboards, the `RitualRegistry` in rustclaw, external tools)
/// can branch on this single field instead of inspecting the transitions array.
///
/// **Backwards compat**: state files written before this field existed
/// deserialize with `status: Active` via `#[serde(default)]`. Operationally
/// such files are *zombies* — phase is non-terminal, no adapter is alive.
/// The orphan-sweep pass on runner startup is responsible for repairing them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RitualV2Status {
    /// Ritual is in flight. Phase may be any non-terminal value.
    #[default]
    Active,
    /// Ritual completed successfully (phase = Done).
    Done,
    /// Ritual was cancelled — by user, adapter shutdown, or orphan sweep.
    /// `phase = Cancelled`. The latest transition's `event` field carries the
    /// reason (e.g. `"Implementing → Cancelled"`, `"adapter_shutdown"`,
    /// `"orphaned"`).
    Cancelled,
    /// Ritual reached a terminal failure (phase = Escalated, no retry path
    /// taken). Distinct from "in retry": status is only `Failed` once the
    /// ritual stops moving.
    Failed,
}

// ═══════════════════════════════════════════════════════════════════════════════
// State
// ═══════════════════════════════════════════════════════════════════════════════

/// Full ritual state — immutable transitions via builder methods.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RitualState {
    /// Unique ritual identifier (for multi-ritual parallel support).
    #[serde(default = "default_ritual_id")]
    pub id: String,
    pub phase: RitualPhase,
    pub task: String,
    /// The target project directory for this ritual.
    /// Set at ritual start time. All file operations (detect, skill, verify) use this path.
    /// If None, falls back to the runner's workspace root.
    ///
    /// **Deprecated for new code** — prefer `work_unit` (ISS-029). This field
    /// is populated as a resolved cache when `work_unit` is set, and is
    /// maintained for state-file backwards compatibility.
    #[serde(default)]
    pub target_root: Option<String>,
    /// ISS-029: typed work-unit specification. When set, `target_root` is
    /// derived from this via the registry resolver. New callers MUST set this;
    /// `target_root`-only state files are still loadable for backwards compat.
    #[serde(default)]
    pub work_unit: Option<super::work_unit::WorkUnit>,
    pub project: Option<ProjectState>,
    pub strategy: Option<ImplementStrategy>,
    pub verify_retries: u32,
    pub phase_retries: HashMap<String, u32>,
    pub failed_phase: Option<RitualPhase>,
    pub error_context: Option<String>,
    /// Tracks which phase the review is for and where to go after approval.
    /// Format: "design", "graph", "requirements" — maps to review skill name.
    #[serde(default)]
    pub review_target: Option<String>,
    /// Triage-assessed task size: "small", "medium", "large".
    #[serde(default)]
    pub triage_size: Option<String>,
    /// Tokens used per phase (e.g. "design" -> 5432, "implement" -> 28000).
    #[serde(default)]
    pub phase_tokens: HashMap<String, u64>,
    /// Current review round (1-based). Each review target gets 2 rounds:
    /// round 1: review → apply → round 2: review → apply → next phase.
    #[serde(default)]
    pub review_round: u32,
    pub transitions: Vec<TransitionRecord>,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// OS process ID of the ritual adapter/runner that last wrote this state.
    /// Consumers (e.g. `RitualRegistry` in downstream apps) use this to detect
    /// whether the current process is the executor of a SingleLlm ritual.
    /// Populated by the runner on every `save_state` via `std::process::id()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_pid: Option<u32>,
    /// Top-level disposition (in flight / done / cancelled / failed).
    ///
    /// Maintained automatically by `with_phase` based on the destination
    /// phase. Older state files without this field deserialize as `Active`.
    /// See [`RitualV2Status`] for the semantics and the orphan-sweep
    /// remediation for zombie files.
    #[serde(default)]
    pub status: RitualV2Status,

    /// ISS-052 §6.3 — persist degradation side-channel.
    ///
    /// `Some(_)` iff a `Periodic` `SaveState` save has failed and the
    /// ritual is continuing in-memory while subsequent saves are being
    /// retried. Cleared (`None`) on the next successful `StatePersisted`
    /// event. **Never** set at phase boundaries — a failed `Boundary`
    /// save aborts directly to `Failed` (§6.3.3).
    ///
    /// On 5 consecutive `Periodic` failures (`consecutive_failures == 5`
    /// after increment), the ritual transitions to `Failed` per §6.3.3.
    ///
    /// `#[serde(skip_serializing_if = "Option::is_none")]` keeps the
    /// steady-state on-disk JSON unchanged (NG2 / §6.3.5). Older state
    /// files without this field deserialize as `None` via
    /// `#[serde(default)]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist_degraded: Option<PersistDegradedInfo>,
}

fn default_ritual_id() -> String {
    generate_ritual_id()
}

/// Generate a short human-readable ritual ID (e.g., "r-a3f81b").
/// Uses lower 24 bits of millisecond timestamp (~4.6 hour cycle) for uniqueness.
pub fn generate_ritual_id() -> String {
    use std::time::SystemTime;
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("r-{:06x}", ts & 0xFFFFFF)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransitionRecord {
    pub from: RitualPhase,
    pub to: RitualPhase,
    pub event: String,
    pub timestamp: DateTime<Utc>,
}

/// ISS-052 §6.3.1 — side-channel info recorded when persist saves are
/// failing mid-phase but the ritual is allowed to continue in-memory.
///
/// Populated/cleared by the state-machine arms for `StatePersisted` and
/// `StatePersistFailed` events (see `transition` in this file). Inspected
/// by post-mortem tooling and the `Notify` messages emitted by those
/// arms.
///
/// **Lifecycle:**
/// - `None` — steady state. No degraded persist.
/// - `Some(info)` — at least one `Periodic` `SaveState` save has failed
///   without an intervening success. `info.consecutive_failures` ranges
///   `1..=4`; the *next* `StatePersistFailed` (which would make it 5) is
///   the failure-mode trigger that aborts the ritual to `Failed`.
/// - Cleared back to `None` by `StatePersisted` (recovery) or replaced
///   by a transition to `Failed` (escalation).
///
/// **Why not a wrapping phase?** Per design NG1, the `RitualPhase` enum
/// must not gain a wrapping `Degraded(_)` variant. The transition table
/// is unchanged; degradation is observable only through this field and
/// the dedicated event-handling arms.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistDegradedInfo {
    /// The phase the ritual was in when degradation began. Reported in
    /// notifications and post-mortems; **not** used for control flow.
    pub since_phase: RitualPhase,
    /// Human-readable last error message from the most recent failed
    /// attempt. Updated on each `StatePersistFailed` while the ritual is
    /// already degraded.
    pub last_error: String,
    /// Number of consecutive `StatePersistFailed` events without an
    /// intervening `StatePersisted`. Reset to 0 (and the whole field to
    /// `None`) on success. On reaching 5 (the *next* failure after
    /// `consecutive_failures == 4`), the ritual transitions to `Failed`
    /// per §6.3.3.
    pub consecutive_failures: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectState {
    pub has_requirements: bool,
    pub has_design: bool,
    pub has_graph: bool,
    pub has_source: bool,
    pub has_tests: bool,
    pub language: Option<String>,
    pub source_file_count: usize,
    pub verify_command: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ImplementStrategy {
    SingleLlm,
    MultiAgent { tasks: Vec<String> },
}

impl RitualState {
    pub fn new() -> Self {
        let now = Utc::now();
        Self {
            id: generate_ritual_id(),
            phase: RitualPhase::Idle,
            task: String::new(),
            target_root: None,
            work_unit: None,
            project: None,
            strategy: None,
            verify_retries: 0,
            phase_retries: HashMap::new(),
            failed_phase: None,
            error_context: None,
            review_target: None,
            review_round: 0,
            phase_tokens: HashMap::new(),
            transitions: Vec::new(),
            started_at: now,
            triage_size: None,
            updated_at: now,
            adapter_pid: None,
            status: RitualV2Status::Active,
            persist_degraded: None,
        }
    }

    pub fn with_phase(mut self, phase: RitualPhase) -> Self {
        self.transitions.push(TransitionRecord {
            from: self.phase.clone(),
            to: phase.clone(),
            event: format!("{:?} → {:?}", self.phase, phase),
            timestamp: Utc::now(),
        });
        // Maintain `status` invariant: terminal phases set the matching
        // terminal status; non-terminal phases imply `Active` (handles the
        // unusual case of re-entering the FSM, e.g. WaitingApproval ↔
        // Reviewing cycles, without leaving stale terminal status from a
        // prior path that never happened).
        self.status = phase
            .terminal_status()
            .unwrap_or(RitualV2Status::Active);
        self.phase = phase;
        self.updated_at = Utc::now();
        self
    }

    pub fn with_task(mut self, task: String) -> Self {
        self.task = task;
        self
    }

    pub fn with_target_root(mut self, root: String) -> Self {
        self.target_root = Some(root);
        self
    }

    /// ISS-029: set both the typed work unit and its resolved `target_root`.
    ///
    /// Prefer this over `with_target_root` for new code — it records the
    /// explicit intent (which project + which issue/feature/task) so the
    /// information survives state-file round-trips and can be re-resolved
    /// if a project path changes.
    pub fn with_work_unit(
        mut self,
        unit: super::work_unit::WorkUnit,
        resolved_root: std::path::PathBuf,
    ) -> Self {
        self.target_root = Some(resolved_root.to_string_lossy().into_owned());
        self.work_unit = Some(unit);
        self
    }

    /// Set the adapter PID to the current process.
    /// Called by runners so downstream consumers can detect
    /// whether they are executing the ritual themselves.
    pub fn with_current_adapter_pid(mut self) -> Self {
        self.adapter_pid = Some(std::process::id());
        self
    }

    pub fn with_project(mut self, ps: ProjectState) -> Self {
        self.project = Some(ps);
        self
    }

    pub fn with_strategy(mut self, strategy: ImplementStrategy) -> Self {
        self.strategy = Some(strategy);
        self
    }

    pub fn with_review_target(mut self, target: &str) -> Self {
        self.review_target = Some(target.to_string());
        self
    }

    /// Set review round (1-based). Used for multi-round review cycles.
    pub fn with_review_round(mut self, round: u32) -> Self {
        self.review_round = round;
        self
    }

    /// Increment review round counter.
    pub fn inc_review_round(mut self) -> Self {
        self.review_round += 1;
        self
    }

    /// Record tokens used by a phase (additive — multiple calls accumulate).
    pub fn add_phase_tokens(mut self, phase: &str, tokens: u64) -> Self {
        *self.phase_tokens.entry(phase.to_string()).or_insert(0) += tokens;
        self
    }

    /// Total tokens used across all phases.
    pub fn total_tokens(&self) -> u64 {
        self.phase_tokens.values().sum()
    }

    pub fn inc_verify_retries(mut self) -> Self {
        self.verify_retries += 1;
        self
    }

    pub fn inc_phase_retry(mut self, phase_key: &str) -> Self {
        *self.phase_retries.entry(phase_key.to_string()).or_insert(0) += 1;
        self
    }

    pub fn with_failed_phase(mut self, phase: RitualPhase) -> Self {
        self.failed_phase = Some(phase);
        self
    }

    pub fn with_error_context(mut self, error: String) -> Self {
        self.error_context = Some(error);
        self
    }

    /// Get retry count for a specific phase.
    pub fn retries_for(&self, phase_key: &str) -> u32 {
        *self.phase_retries.get(phase_key).unwrap_or(&0)
    }

    /// Get the configured verify command.
    pub fn verify_command(&self) -> &str {
        self.project
            .as_ref()
            .and_then(|p| p.verify_command.as_deref())
            .unwrap_or("echo 'No verify command configured'")
    }
}

impl Default for RitualState {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Triage result
// ═══════════════════════════════════════════════════════════════════════════════

/// Output of the triage LLM call (haiku, ~200 tokens, ~$0.001).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TriageResult {
    /// "clear" or "ambiguous"
    pub clarity: String,
    /// Questions to ask user if ambiguous
    #[serde(default)]
    pub clarify_questions: Vec<String>,
    /// "small", "medium", "large"
    pub size: String,
    /// Skip design phase for trivial tasks
    #[serde(default)]
    pub skip_design: bool,
    /// Skip graph update for trivial tasks
    #[serde(default)]
    pub skip_graph: bool,
}

// ═══════════════════════════════════════════════════════════════════════════════
// Events
// ═══════════════════════════════════════════════════════════════════════════════

/// Events that drive state transitions.
#[derive(Clone, Debug)]
pub enum RitualEvent {
    // User events
    Start { task: String },
    UserCancel,
    UserRetry,
    UserSkipPhase,

    // System events
    ProjectDetected(ProjectState),
    TriageCompleted(TriageResult),
    UserClarification { response: String },
    /// User approves specific review findings (e.g., "FINDING-1,3,5" or "all").
    UserApproval { approved: String },
    PlanDecided(ImplementStrategy),
    SkillCompleted { phase: String, artifacts: Vec<String> },
    /// A skill execution failed.
    ///
    /// `phase` and `error` are the legacy human-facing fields (kept for
    /// back-compat with all existing match arms and emit sites).
    ///
    /// `reason` (ISS-052 T02b) is `None` for legacy emit sites and
    /// `Some(SkillFailureReason)` when emitted by the new gates
    /// (zero-file, forbidden-file, turn-limit-no-verdict, review-rejected,
    /// explicit-claim). State-machine arms that don't care about the
    /// structured reason can continue to use `..` rest patterns; arms that
    /// branch on it use `if let Some(r) = reason`.
    SkillFailed {
        phase: String,
        error: String,
        reason: Option<SkillFailureReason>,
    },
    ShellCompleted { stdout: String, exit_code: i32 },
    ShellFailed { stderr: String, exit_code: i32 },

    /// Cancellation requested mid-action by the embedder via
    /// `RitualHooks::should_cancel` (ISS-052 T02d).
    ///
    /// Distinct from `UserCancel`, which represents an explicit user-issued
    /// cancel command at the *engine* surface (e.g. dispatched directly into
    /// `transition()` by an external handler before any action runs). Both
    /// route to the same terminal `Cancelled` phase, but `Cancelled` carries a
    /// structured `CancelReason` (source + message) for telemetry and the
    /// notification text, whereas `UserCancel` is reasonless by construction.
    ///
    /// Emitted by `V2Executor::execute` when `hooks.should_cancel()` returns
    /// `Some(reason)` while polling between actions.
    Cancelled { reason: CancelReason },

    // ── ISS-052 T05: persistence + lifecycle events ─────────────────────────
    //
    // These variants are introduced ahead of the V2Executor wrappers that
    // emit them (T03 for persist, T08 for run_ritual workspace resolution)
    // so the enum and the state-machine match arms become available before
    // the call sites land. Until those tasks ship, no production code emits
    // these variants — they are tested via direct `transition()` calls.
    //
    // Formal handling (e.g. `persist_degraded` side-channel mutation) is
    // deferred to T04. The arms added in this task are the minimum needed
    // to keep `transition()` total: each event produces a deterministic
    // (state, action) outcome with no panic / no unhandled-event silent
    // drop.

    /// Emitted by V2Executor's `persist_state` retry wrapper (T03) when
    /// `hooks.persist_state` succeeds. `attempt` is the 1-based index of
    /// the successful attempt (1 = first try, 2 = succeeded after one
    /// retry, …). `kind` is propagated from the originating
    /// `RitualAction::SaveState { kind }` so the `transition` arm can
    /// emit the right notify (silent for ordinary `Boundary`/`Periodic`
    /// success, recovery message when `persist_degraded` was `Some(_)`).
    StatePersisted { attempt: u32, kind: SaveStateKind },

    /// Emitted by V2Executor's `persist_state` retry wrapper (T03) after
    /// `MAX_ATTEMPTS` consecutive `hooks.persist_state` failures.
    /// Carries the 1-based attempt count actually run (always equal to
    /// `MAX_ATTEMPTS` today, but kept as a field for future-proofing the
    /// retry policy), the human-readable last error string, and the
    /// `SaveStateKind` of the action that failed. T04 branches on
    /// `kind`: `Boundary` aborts to `Escalated` immediately; `Periodic`
    /// flips the `persist_degraded` side-channel and lets the ritual
    /// continue, escalating only after 5 consecutive failures.
    StatePersistFailed {
        attempt: u32,
        error: String,
        kind: SaveStateKind,
    },

    /// Emitted by `run_ritual` (T08) when `hooks.resolve_workspace`
    /// returns an error before any phase action runs. Terminates the
    /// ritual to `Failed` — we cannot operate without a workspace path.
    /// Distinct from `SkillFailed` because no skill has executed yet.
    WorkspaceUnresolved { error: String },

    /// Self-review subloop produced an `Accept` verdict (ISS-052 T07, §8.2).
    ///
    /// Emitted by `V2Executor::run_self_review_subloop` when the LLM tags
    /// its output with an explicit accept signal within the turn budget.
    /// `skill` is the skill name being reviewed (e.g. `"implement"`,
    /// `"draft-design"`); `turns_used` is the 1-based turn index on which
    /// the verdict appeared (1..=MAX_TURNS).
    ///
    /// Transition semantics: forwards to the same `SkillCompleted{phase}`
    /// arm so the ritual progresses just as if the skill had succeeded
    /// without a self-review. The distinct event exists so observers
    /// (telemetry, future gates) can tell self-reviewed completions
    /// apart from raw skill completions, and so the state-machine arms
    /// stay total when subloop runs are introduced. See §8 of ISS-052
    /// design.
    ///
    /// `Reject` verdicts and turn-limit exhaustion never produce this
    /// variant — they emit `SkillFailed { reason: ReviewRejected }` and
    /// `SkillFailed { reason: LlmTurnLimitNoVerdict }` respectively.
    /// `NeedsChanges` continues the subloop (no event emitted yet).
    SelfReviewCompleted {
        skill: String,
        verdict: ReviewVerdict,
        turns_used: u32,
    },
}

/// Verdict produced by the self-review subloop (ISS-052 T07, §8.2).
///
/// Populated from the reviewing LLM's tagged output. Currently only
/// `Accept` reaches the state machine via `SelfReviewCompleted` —
/// `Reject` is folded into `SkillFailed { reason: ReviewRejected }`
/// inside the subloop, and `NeedsChanges` causes a retry without
/// emitting an event. The variant is retained on the public event so
/// future gates (e.g. surfacing `NeedsChanges` for human review) can
/// be added without re-shaping the enum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewVerdict {
    /// LLM emitted an explicit accept tag (e.g. `REVIEW_PASS`,
    /// `verdict: accept`). The subloop terminates and the outer
    /// `run_skill` returns `SelfReviewCompleted`.
    Accept,
    /// LLM emitted an explicit reject tag (e.g. `REVIEW_REJECT`,
    /// `verdict: reject`). The subloop terminates and the outer
    /// `run_skill` returns `SkillFailed { reason: ReviewRejected }`.
    Reject,
    /// LLM signalled it made changes and wants another review pass
    /// (e.g. `verdict: needs-changes`). The subloop continues to the
    /// next turn (if budget allows). Reserved for future surfacing.
    NeedsChanges,
}

/// Structured reason a skill execution failed.
///
/// Added by ISS-052 T02b as part of `SkillFailed.reason: Option<_>`.
/// Existing emit sites pass `None`; new quality gates added in
/// later T02 sub-tasks (zero-file gate, forbidden-file gate, self-review
/// turn-limit, etc.) emit `Some(reason)` so the state machine and
/// observers can differentiate failure modes without parsing `error`
/// strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkillFailureReason {
    /// Skill claimed success but produced no file changes when the
    /// configured `file_policy` required at least one file change
    /// (ISS-038 file_snapshot gate).
    ZeroFileChanges,
    /// Skill produced file changes that the configured `file_policy`
    /// forbids (e.g. a read-only / review skill wrote files).
    UnexpectedFileChanges,
    /// Self-review subloop (§8) hit the LLM turn limit without
    /// producing a verdict.
    LlmTurnLimitNoVerdict,
    /// Self-review subloop returned an explicit `reject` verdict.
    ReviewRejected,
    /// The skill itself reported failure via its normal output channel.
    /// The carried `String` is the raw claim from the skill, preserved
    /// separately from the human-readable `error` field on `SkillFailed`.
    ExplicitClaim(String),
}

// ═══════════════════════════════════════════════════════════════════════════════
// Actions
// ═══════════════════════════════════════════════════════════════════════════════

/// Actions produced by transitions. Executor handles side effects.
#[derive(Clone, Debug)]
pub enum RitualAction {
    /// Detect project state (filesystem scan + config read).
    DetectProject,
    /// Run triage (lightweight haiku LLM call to assess task clarity/size).
    RunTriage { task: String },
    /// Run a skill phase with LLM.
    RunSkill { name: String, context: String },
    /// Run a shell command (verify build/test).
    RunShell { command: String },
    /// Run harness (multi-agent parallel).
    RunHarness { tasks: Vec<String> },
    /// Run planning (LLM reads DESIGN.md, decides strategy).
    RunPlanning,
    /// Update graph node status.
    UpdateGraph { description: String },
    /// Send notification (fire-and-forget).
    Notify { message: String },
    /// Persist state to disk.
    ///
    /// ISS-052 §6.3.2 — `kind` distinguishes phase-boundary saves
    /// (the first save emitted right after a phase transition; failure
    /// aborts to `Failed`) from periodic mid-phase saves (failure flips
    /// the `persist_degraded` side-channel and lets the ritual continue).
    /// The state machine emits `Boundary` everywhere today; `Periodic`
    /// emission sites are added in T07 (self-review subloop port).
    SaveState { kind: SaveStateKind },
    /// Cleanup temporary files.
    Cleanup,
    /// Apply approved review findings (fire-and-forget, runs apply-review skill).
    ApplyReview { approved: String },
}

/// ISS-052 §6.3.2 — classifier for `RitualAction::SaveState`.
///
/// The state-machine arms for `StatePersistFailed` (see `transition`)
/// branch on this tag to decide whether a persist failure is fatal
/// (`Boundary`) or recoverable via the `persist_degraded` side-channel
/// (`Periodic`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveStateKind {
    /// Emitted exactly once after each phase-transition event, before
    /// the new phase produces any other actions. A failed `Boundary`
    /// save aborts the ritual to `Failed` (§6.3.3) — we cannot guarantee
    /// the new phase starts from a known-persisted state.
    Boundary,
    /// Emitted as a periodic checkpoint inside a long-running phase
    /// (e.g. `Implementing` between LLM turns). A failed `Periodic`
    /// save flips the `persist_degraded` side-channel and lets the
    /// ritual continue. Wired in T07 (self-review subloop port); the
    /// state machine itself does not emit `Periodic` today.
    Periodic,
}

impl RitualAction {
    /// Whether this action produces an event (executor must return an Event after running it).
    pub fn is_event_producing(&self) -> bool {
        matches!(
            self,
            RitualAction::DetectProject
                | RitualAction::RunTriage { .. }
                | RitualAction::RunSkill { .. }
                | RitualAction::RunShell { .. }
                | RitualAction::RunHarness { .. }
                | RitualAction::RunPlanning
        )
    }

    /// Whether this action is fire-and-forget (no event produced).
    pub fn is_fire_and_forget(&self) -> bool {
        !self.is_event_producing()
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Transition — the core pure function
// ═══════════════════════════════════════════════════════════════════════════════

/// Pure function. Input (state, event), output (new_state, actions).
/// Zero IO, zero side effects. 100% unit-testable.
///
/// Invariant: every transition produces either:
/// - A terminal/paused state (Done/Cancelled/Escalated/WaitingClarification) with 0 EP actions, OR
/// - A non-terminal state with exactly 1 event-producing action.
pub fn transition(state: &RitualState, event: RitualEvent) -> (RitualState, Vec<RitualAction>) {
    use RitualPhase::*;
    use RitualEvent::*;
    use RitualAction::*;

    match (&state.phase, event) {
        // ═══════════════════════════════════════
        // Normal flow
        // ═══════════════════════════════════════

        // Start
        (Idle, Start { task }) => (
            state.clone().with_phase(Initializing).with_task(task.clone()),
            vec![
                Notify { message: format!("🔧 Ritual started: \"{}\"", task) },
                SaveState { kind: SaveStateKind::Boundary },
                DetectProject,
            ],
        ),

        // Project detected → Triage
        (Initializing, ProjectDetected(ps)) => (
            state.clone().with_phase(Triaging).with_project(ps),
            vec![
                Notify { message: "🔍 Triaging task...".into() },
                SaveState { kind: SaveStateKind::Boundary },
                RunTriage { task: state.task.clone() },
            ],
        ),

        // Triage complete (clear) → skip or proceed based on result
        (Triaging, TriageCompleted(result)) if result.clarity == "clear" => {
            let skip_design = result.skip_design;
            let skip_graph = result.skip_graph;

            // Store triage result for later reference
            let mut new_state = state.clone();
            new_state.triage_size = Some(result.size.clone());
            new_state.error_context = Some(format!(
                "triage: size={}, skip_design={}, skip_graph={}",
                result.size, skip_design, skip_graph
            ));

            if skip_design && skip_graph {
                // Small task: skip directly to implementing
                (
                    new_state.with_phase(Planning),
                    vec![
                        Notify { message: format!("⚡ Small task ({}). Skipping design & graph.", result.size) },
                        SaveState { kind: SaveStateKind::Boundary },
                        RunPlanning,
                    ],
                )
            } else if skip_design {
                // Medium task: skip design, do graph
                (
                    new_state.with_phase(Planning),
                    vec![
                        Notify { message: format!("📋 Medium task ({}). Skipping design.", result.size) },
                        SaveState { kind: SaveStateKind::Boundary },
                        RunPlanning,
                    ],
                )
            } else {
                // Large task: start with requirements
                let has_requirements = state.project.as_ref().is_some_and(|p| p.has_requirements);
                if has_requirements {
                    // Requirements exist → skip to design
                    let skill = if state.project.as_ref().is_some_and(|p| p.has_design) {
                        "update-design"
                    } else {
                        "draft-design"
                    };
                    (
                        new_state.with_phase(Designing),
                        vec![
                            Notify { message: format!("📝 Phase 2/5: {}...", skill) },
                            SaveState { kind: SaveStateKind::Boundary },
                            RunSkill { name: skill.into(), context: state.task.clone() },
                        ],
                    )
                } else {
                    // No requirements → write them first
                    (
                        new_state.with_phase(WritingRequirements),
                        vec![
                            Notify { message: "📋 Phase 1/5: Writing requirements...".into() },
                            SaveState { kind: SaveStateKind::Boundary },
                            RunSkill { name: "draft-requirements".into(), context: state.task.clone() },
                        ],
                    )
                }
            }
        }

        // Triage: ambiguous → ask user for clarification
        (Triaging, TriageCompleted(result)) => {
            let questions = result.clarify_questions.join("\n• ");
            (
                state.clone().with_phase(WaitingClarification),
                vec![
                    Notify { message: format!(
                        "❓ Task needs clarification:\n• {}\n\nPlease reply with details, then /ritual retry.",
                        questions
                    )},
                    SaveState { kind: SaveStateKind::Boundary },
                ],
            )
        }

        // User provides clarification → re-triage with enriched task
        (WaitingClarification, UserClarification { response }) => {
            let enriched_task = format!("{}\n\nClarification: {}", state.task, response);
            (
                state.clone().with_phase(Triaging).with_task(enriched_task.clone()),
                vec![
                    Notify { message: "🔍 Re-triaging with clarification...".into() },
                    SaveState { kind: SaveStateKind::Boundary },
                    RunTriage { task: enriched_task },
                ],
            )
        }

        // UserRetry from WaitingClarification → re-triage
        (WaitingClarification, UserRetry) => (
            state.clone().with_phase(Triaging),
            vec![
                Notify { message: "🔍 Re-triaging...".into() },
                SaveState { kind: SaveStateKind::Boundary },
                RunTriage { task: state.task.clone() },
            ],
        ),

        // Requirements done → Review requirements
        (WritingRequirements, SkillCompleted { .. }) => (
            state.clone().with_phase(Reviewing).with_review_target("requirements"),
            vec![
                Notify { message: "📝 Reviewing requirements...".into() },
                SaveState { kind: SaveStateKind::Boundary },
                RunSkill { name: "review-requirements".into(), context: state.task.clone() },
            ],
        ),

        // Design done → Review or skip review based on whether design was updated vs created
        (Designing, SkillCompleted { .. }) => {
            let design_was_updated = state.project.as_ref().is_some_and(|p| p.has_design);
            let is_large = state.triage_size.as_deref() == Some("large");
            if design_was_updated && !is_large {
                // Design already existed + not a large task — incremental update, skip review
                (
                    state.clone().with_phase(Planning),
                    vec![
                        Notify { message: "📝 Design updated (incremental). Skipping review → Planning...".into() },
                        SaveState { kind: SaveStateKind::Boundary },
                        RunPlanning,
                    ],
                )
            } else {
                // New design created — review it
                (
                    state.clone().with_phase(Reviewing).with_review_target("design"),
                    vec![
                        Notify { message: "📝 Reviewing design document...".into() },
                        SaveState { kind: SaveStateKind::Boundary },
                        RunSkill { name: "review-design".into(), context: state.task.clone() },
                    ],
                )
            }
        }

        // Planning decided → Graphing
        (Planning, PlanDecided(strategy)) => {
            let skill = if state.project.as_ref().is_some_and(|p| p.has_graph) {
                "update-graph"
            } else {
                "generate-graph"
            };
            (
                state.clone().with_phase(Graphing).with_strategy(strategy),
                vec![
                    Notify { message: format!("📊 Phase 2/4: {}...", skill) },
                    SaveState { kind: SaveStateKind::Boundary },
                    RunSkill { name: skill.into(), context: state.task.clone() },
                ],
            )
        }

        // Graph done → Review or skip review based on whether graph was updated vs created
        (Graphing, SkillCompleted { .. }) => {
            let graph_was_updated = state.project.as_ref().is_some_and(|p| p.has_graph);
            let is_large = state.triage_size.as_deref() == Some("large");
            if graph_was_updated && !is_large {
                // Graph already existed + not large — incremental update, skip review → implement
                let action = match &state.strategy {
                    Some(ImplementStrategy::MultiAgent { tasks }) => RunHarness { tasks: tasks.clone() },
                    _ => RunSkill { name: "implement".into(), context: state.task.clone() },
                };
                (
                    state.clone().with_phase(Implementing),
                    vec![
                        Notify { message: "📊 Graph updated (incremental). Skipping review → Implementing...".into() },
                        SaveState { kind: SaveStateKind::Boundary },
                        action,
                    ],
                )
            } else {
                // New graph created — review it
                (
                    state.clone().with_phase(Reviewing).with_review_target("tasks"),
                    vec![
                        Notify { message: "📝 Reviewing task breakdown...".into() },
                        SaveState { kind: SaveStateKind::Boundary },
                        RunSkill { name: "review-tasks".into(), context: state.task.clone() },
                    ],
                )
            }
        }

        // ─── Review cycle transitions ─────────────────────────────────────

        // Review skill completed → WaitingApproval (pause for human / auto-approve after timeout)
        (Reviewing, SkillCompleted { .. }) => {
            let round = state.review_round + 1; // 1-based for display
            let target = state.review_target.as_deref().unwrap_or("unknown");
            (
                state.clone().with_phase(WaitingApproval).inc_review_round(),
                vec![
                    Notify { message: format!(
                        "📋 Review complete ({} round {}/2). Check `.gid/reviews/` for findings.\n\
                         Auto-applying all findings in 3 minutes if no response.",
                        target, round
                    )},
                    SaveState { kind: SaveStateKind::Boundary },
                ],
            )
        }

        // User approves findings → apply changes, then either loop for round 2 or proceed.
        // Two-round review: round 1 apply → re-review same target; round 2 apply → next phase.
        (WaitingApproval, UserApproval { approved }) => {
            let review_target = state.review_target.clone().unwrap_or_default();

            // Round 1: apply findings, then loop back to Reviewing (same target) for round 2
            if state.review_round < 2 {
                let review_skill = match review_target.as_str() {
                    "requirements" => "review-requirements",
                    "design" => "review-design",
                    "tasks" => "review-tasks",
                    _ => "review-design",
                };
                return (
                    state.clone().with_phase(Reviewing),
                    vec![
                        ApplyReview { approved },
                        Notify { message: format!(
                            "🔄 Applied round 1 findings. Starting review round 2/2 for {}...",
                            review_target
                        )},
                        SaveState { kind: SaveStateKind::Boundary },
                        RunSkill { name: review_skill.into(), context: state.task.clone() },
                    ],
                );
            }

            // Round 2 (or higher): apply findings and proceed to next phase, reset review_round
            match review_target.as_str() {
                "requirements" => {
                    let skill = if state.project.as_ref().is_some_and(|p| p.has_design) {
                        "update-design"
                    } else {
                        "draft-design"
                    };
                    (
                        state.clone().with_phase(Designing).with_review_round(0),
                        vec![
                            ApplyReview { approved },
                            Notify { message: format!("✅ 2 review rounds complete. Phase 2/5: {}...", skill) },
                            SaveState { kind: SaveStateKind::Boundary },
                            RunSkill { name: skill.into(), context: state.task.clone() },
                        ],
                    )
                }
                "design" => (
                    state.clone().with_phase(Planning).with_review_round(0),
                    vec![
                        ApplyReview { approved },
                        Notify { message: "✅ 2 review rounds complete. 🧠 Planning implementation strategy...".into() },
                        SaveState { kind: SaveStateKind::Boundary },
                        RunPlanning,
                    ],
                ),
                "tasks" => {
                    let action = match &state.strategy {
                        Some(ImplementStrategy::MultiAgent { tasks }) => RunHarness { tasks: tasks.clone() },
                        _ => RunSkill { name: "implement".into(), context: state.task.clone() },
                    };
                    (
                        state.clone().with_phase(Implementing).with_review_round(0),
                        vec![
                            ApplyReview { approved },
                            Notify { message: "✅ 2 review rounds complete. 💻 Implementing...".into() },
                            SaveState { kind: SaveStateKind::Boundary },
                            action,
                        ],
                    )
                }
                other => (
                    state.clone().with_phase(Planning).with_review_round(0),
                    vec![
                        ApplyReview { approved },
                        Notify { message: format!("⚠️ Unknown review_target '{}', defaulting to Planning", other) },
                        SaveState { kind: SaveStateKind::Boundary },
                        RunPlanning,
                    ],
                ),
            }
        }

        // User skips review → continue to next phase without applying (resets review_round)
        (WaitingApproval, UserSkipPhase) => {
            let review_target = state.review_target.clone().unwrap_or_default();
            match review_target.as_str() {
                "requirements" => {
                    let skill = if state.project.as_ref().is_some_and(|p| p.has_design) {
                        "update-design"
                    } else {
                        "draft-design"
                    };
                    (
                        state.clone().with_phase(Designing).with_review_round(0),
                        vec![
                            Notify { message: "⏭️ Skipping review, moving to design...".into() },
                            SaveState { kind: SaveStateKind::Boundary },
                            RunSkill { name: skill.into(), context: state.task.clone() },
                        ],
                    )
                }
                "design" => (
                    state.clone().with_phase(Planning).with_review_round(0),
                    vec![
                        Notify { message: "⏭️ Skipping review, moving to planning...".into() },
                        SaveState { kind: SaveStateKind::Boundary },
                        RunPlanning,
                    ],
                ),
                "tasks" => {
                    let action = match &state.strategy {
                        Some(ImplementStrategy::MultiAgent { tasks }) => RunHarness { tasks: tasks.clone() },
                        _ => RunSkill { name: "implement".into(), context: state.task.clone() },
                    };
                    (
                        state.clone().with_phase(Implementing).with_review_round(0),
                        vec![
                            Notify { message: "⏭️ Skipping review, moving to implementation...".into() },
                            SaveState { kind: SaveStateKind::Boundary },
                            action,
                        ],
                    )
                }
                other => (
                    state.clone().with_phase(Planning).with_review_round(0),
                    vec![
                        Notify { message: format!("⚠️ Skipping review (unknown review_target '{}'), defaulting to Planning", other) },
                        SaveState { kind: SaveStateKind::Boundary },
                        RunPlanning,
                    ],
                ),
            }
        }

        // Review failed → log and continue (don't block the ritual for review failure)
        (Reviewing, SkillFailed { error, .. }) => {
            let review_target = state.review_target.clone().unwrap_or_default();
            let next = match review_target.as_str() {
                "requirements" => {
                    let skill = if state.project.as_ref().is_some_and(|p| p.has_design) {
                        "update-design"
                    } else {
                        "draft-design"
                    };
                    (
                        state.clone().with_phase(Designing),
                        vec![
                            Notify { message: format!("⚠️ Review failed ({}), continuing to design...", error) },
                            SaveState { kind: SaveStateKind::Boundary },
                            RunSkill { name: skill.into(), context: state.task.clone() },
                        ],
                    )
                }
                "design" => (
                    state.clone().with_phase(Planning),
                    vec![
                        Notify { message: format!("⚠️ Review failed ({}), continuing to planning...", error) },
                        SaveState { kind: SaveStateKind::Boundary },
                        RunPlanning,
                    ],
                ),
                "tasks" => {
                    let action = match &state.strategy {
                        Some(ImplementStrategy::MultiAgent { tasks }) => RunHarness { tasks: tasks.clone() },
                        _ => RunSkill { name: "implement".into(), context: state.task.clone() },
                    };
                    (
                        state.clone().with_phase(Implementing),
                        vec![
                            Notify { message: format!("⚠️ Review failed ({}), continuing to implementation...", error) },
                            SaveState { kind: SaveStateKind::Boundary },
                            action,
                        ],
                    )
                }
                other => (
                    state.clone().with_phase(Planning),
                    vec![
                        Notify { message: format!("⚠️ Review failed ({}) for unknown target '{}', defaulting to Planning", error, other) },
                        SaveState { kind: SaveStateKind::Boundary },
                        RunPlanning,
                    ],
                ),
            };
            next
        }

        // ─── End review cycle transitions ────────────────────────────────

        // Implement done → Verifying
        (Implementing, SkillCompleted { .. }) => {
            let cmd = state.verify_command().to_string();
            (
                state.clone().with_phase(Verifying),
                vec![
                    Notify { message: "✅ Phase 4/4: Verifying...".into() },
                    SaveState { kind: SaveStateKind::Boundary },
                    RunShell { command: cmd },
                ],
            )
        }

        // Verify success → Done
        (Verifying, ShellCompleted { exit_code: 0, .. }) => (
            state.clone().with_phase(Done),
            vec![
                Notify { message: "🎉 Ritual complete!".into() },
                UpdateGraph { description: state.task.clone() },
                SaveState { kind: SaveStateKind::Boundary },
                Cleanup,
            ],
        ),

        // ═══════════════════════════════════════
        // Failures & retries
        // ═══════════════════════════════════════

        // Verify failed → back to Implementing (max 3)
        (Verifying, ShellFailed { stderr, .. }) if state.verify_retries < 3 => (
            state.clone()
                .with_phase(Implementing)
                .inc_verify_retries()
                .with_error_context(stderr.clone()),
            vec![
                Notify { message: format!(
                    "🔄 Build failed (attempt {}/3), fixing...",
                    state.verify_retries + 1
                )},
                SaveState { kind: SaveStateKind::Boundary },
                RunSkill {
                    name: "implement".into(),
                    context: format!(
                        "FIX BUILD/TEST ERROR:\n{}\n\nOriginal task: {}",
                        stderr, state.task
                    ),
                },
            ],
        ),

        // Verify retries exhausted → Escalate
        (Verifying, ShellFailed { stderr, .. }) => (
            state.clone()
                .with_phase(Escalated)
                .with_failed_phase(Verifying)
                .with_error_context(stderr.clone()),
            vec![
                Notify { message: format!(
                    "❌ Build failed after 3 attempts.\nLast error: {}",
                    truncate(&stderr, 200)
                )},
                SaveState { kind: SaveStateKind::Boundary },
            ],
        ),

        // Defensive: ShellCompleted with non-zero exit (retries < 3)
        (Verifying, ShellCompleted { exit_code, stdout }) if exit_code != 0 && state.verify_retries < 3 => (
            state.clone()
                .with_phase(Implementing)
                .inc_verify_retries()
                .with_error_context(stdout.clone()),
            vec![
                Notify { message: format!(
                    "🔄 Tests returned exit code {} (attempt {}/3), fixing...",
                    exit_code, state.verify_retries + 1
                )},
                SaveState { kind: SaveStateKind::Boundary },
                RunSkill {
                    name: "implement".into(),
                    context: format!(
                        "FIX: verify exited with code {}\nOutput:\n{}\n\nOriginal task: {}",
                        exit_code, stdout, state.task
                    ),
                },
            ],
        ),

        // Defensive: ShellCompleted non-zero, retries exhausted
        (Verifying, ShellCompleted { exit_code, stdout }) if exit_code != 0 => (
            state.clone()
                .with_phase(Escalated)
                .with_failed_phase(Verifying)
                .with_error_context(stdout.clone()),
            vec![
                Notify { message: format!("❌ Verify failed (exit {}) after 3 attempts.", exit_code) },
                SaveState { kind: SaveStateKind::Boundary },
            ],
        ),

        // Requirements failed → retry up to 2 times, then escalate
        (WritingRequirements, SkillFailed { error, .. }) if state.retries_for("requirements") < 2 => (
            state.clone().with_phase(WritingRequirements).inc_phase_retry("requirements"),
            vec![
                Notify { message: format!("🔄 Requirements failed, retrying... ({})", truncate(&error, 100)) },
                SaveState { kind: SaveStateKind::Boundary },
                RunSkill {
                    name: "draft-requirements".into(),
                    context: format!("RETRY — previous error: {}\n\nOriginal task: {}", error, state.task),
                },
            ],
        ),

        (Designing, SkillFailed { error, .. }) if state.retries_for("designing") < 2 => (
            state.clone().with_phase(Designing).inc_phase_retry("designing"),
            vec![
                Notify { message: format!("🔄 Design failed, retrying... ({})", truncate(&error, 100)) },
                SaveState { kind: SaveStateKind::Boundary },
                RunSkill {
                    name: if state.project.as_ref().is_some_and(|p| p.has_design) {
                        "update-design"
                    } else {
                        "draft-design"
                    }.into(),
                    context: format!("RETRY — previous error: {}\n\nOriginal task: {}", error, state.task),
                },
            ],
        ),

        // Graphing failed → retry up to 2 times
        (Graphing, SkillFailed { error, .. }) if state.retries_for("graphing") < 2 => (
            state.clone().with_phase(Graphing).inc_phase_retry("graphing"),
            vec![
                Notify { message: format!("🔄 Graph generation failed, retrying... ({})", truncate(&error, 100)) },
                SaveState { kind: SaveStateKind::Boundary },
                RunSkill {
                    name: if state.project.as_ref().is_some_and(|p| p.has_graph) {
                        "update-graph"
                    } else {
                        "generate-graph"
                    }.into(),
                    context: format!("RETRY — previous error: {}\n\nOriginal task: {}", error, state.task),
                },
            ],
        ),

        // Planning failed → retry up to 2 times
        (Planning, SkillFailed { error, .. }) if state.retries_for("planning") < 2 => (
            state.clone().with_phase(Planning).inc_phase_retry("planning"),
            vec![
                Notify { message: format!("🔄 Planning failed, retrying... ({})", truncate(&error, 100)) },
                SaveState { kind: SaveStateKind::Boundary },
                RunPlanning,
            ],
        ),

        // Implementing failed → retry up to 2 times
        (Implementing, SkillFailed { error, .. }) if state.retries_for("implementing") < 2 => (
            state.clone().with_phase(Implementing).inc_phase_retry("implementing"),
            vec![
                Notify { message: format!("🔄 Implementation failed, retrying... ({})", truncate(&error, 100)) },
                SaveState { kind: SaveStateKind::Boundary },
                RunSkill {
                    name: "implement".into(),
                    context: format!("RETRY — previous error: {}\n\nOriginal task: {}", error, state.task),
                },
            ],
        ),

        // Any phase SkillFailed (retries exhausted) → Escalate
        (phase, SkillFailed { error, .. }) => (
            state.clone()
                .with_phase(Escalated)
                .with_failed_phase(phase.clone())
                .with_error_context(error.clone()),
            vec![
                Notify { message: format!(
                    "❌ {} failed: {}",
                    phase.display_name(),
                    truncate(&error, 200)
                )},
                SaveState { kind: SaveStateKind::Boundary },
            ],
        ),

        // ═══════════════════════════════════════
        // User interaction
        // ═══════════════════════════════════════

        // Cancel (any state)
        (_, UserCancel) => (
            state.clone().with_phase(Cancelled),
            vec![
                Notify { message: "🛑 Ritual cancelled.".into() },
                SaveState { kind: SaveStateKind::Boundary },
            ],
        ),

        // Hook-driven cancellation (ISS-052 T02d).
        //
        // `RitualEvent::Cancelled` is emitted by `V2Executor::execute` when
        // `hooks.should_cancel()` returns `Some(reason)`. Routes to the same
        // terminal `Cancelled` phase as `UserCancel`, but carries the
        // structured `CancelReason` so the notify message reflects the
        // actual cancel source (user command, timeout, daemon shutdown).
        (_, RitualEvent::Cancelled { reason }) => (
            state.clone().with_phase(Cancelled),
            vec![
                Notify { message: format!("🛑 Ritual cancelled: {}", reason.message) },
                SaveState { kind: SaveStateKind::Boundary },
            ],
        ),

        // ── ISS-052 T04: persistence + lifecycle arms (final policy) ───────
        //
        // Implements the §6.3.3 transition table. Six logical branches
        // across two events:
        //
        //   StatePersisted (success):
        //     1. persist_degraded == None  → no-op (steady-state success)
        //     2. persist_degraded == Some  → clear flag + recovery Notify
        //
        //   StatePersistFailed (retries exhausted):
        //     3. Boundary, any flag state                    → Escalated
        //          (cannot guarantee new phase starts known-persisted)
        //     4. Periodic, persist_degraded == None          → flip flag
        //          (consecutive=1) + "ritual continuing in-memory" Notify
        //     5. Periodic, persist_degraded == Some, count<4 → increment
        //          counter + update last_error + warn Notify
        //     6. Periodic, persist_degraded == Some, count==4 → 5th
        //          consecutive failure → Escalated
        //
        // Critical invariants (preserved from the conservative T05 arm):
        //   - Neither arm emits SaveState. Persistence is what failed; a
        //     re-save would loop until the disk recovered, drowning the
        //     user in notifications. The retry happens inside
        //     V2Executor::persist_state (T03), not here.
        //   - Periodic-failure paths leave `phase` unchanged so the
        //     dispatcher continues whatever long-running phase is in
        //     flight (Implementing/Verifying). Only Boundary failure and
        //     the 5-strike escalation move to Escalated.
        //
        //   WorkspaceUnresolved: terminal Escalated. T08 emits this
        //   from `run_ritual` before any phase action runs.

        (_, StatePersisted { kind: _, .. }) => match &state.persist_degraded {
            None => {
                // §6.3.3 row 1 — steady-state success. No phase change,
                // no actions. Both Boundary and Periodic successes look
                // the same; routine save success is intentionally silent.
                (state.clone(), vec![])
            }
            Some(info) => {
                // §6.3.3 row 2 — recovery from a degraded run. Clear the
                // side-channel and surface a recovery message.
                let failures = info.consecutive_failures;
                let mut new_state = state.clone();
                new_state.persist_degraded = None;
                (
                    new_state,
                    vec![Notify {
                        message: format!(
                            "✅ Persistence recovered after {} failed attempt{}.",
                            failures,
                            if failures == 1 { "" } else { "s" }
                        ),
                    }],
                )
            }
        },

        (_, StatePersistFailed { attempt, error, kind }) => match kind {
            SaveStateKind::Boundary => {
                // §6.3.3 row 3 — boundary save failed. Cannot guarantee
                // the next phase starts from a known-persisted state, so
                // abort to Escalated regardless of any prior degradation.
                (
                    state.clone()
                        .with_phase(Escalated)
                        .with_failed_phase(state.phase.clone())
                        .with_error_context(format!(
                            "boundary persist failed after {attempt} attempts: {error}"
                        )),
                    vec![
                        Notify { message: format!(
                            "❌ Boundary persist failed after {attempt} attempts: {}",
                            truncate(&error, 200)
                        )},
                        // No SaveState — persistence is what just failed.
                    ],
                )
            }
            SaveStateKind::Periodic => match &state.persist_degraded {
                None => {
                    // §6.3.3 row 4 — first periodic failure. Flip the
                    // side-channel and let the ritual continue in-memory.
                    // Phase unchanged — the dispatcher resumes whatever
                    // long-running phase was in flight.
                    let mut new_state = state.clone();
                    new_state.persist_degraded = Some(PersistDegradedInfo {
                        since_phase: state.phase.clone(),
                        last_error: error.clone(),
                        consecutive_failures: 1,
                    });
                    (
                        new_state,
                        vec![Notify {
                            message: format!(
                                "⚠️ Persist failed (attempt {attempt}); ritual continuing \
                                 in-memory only: {}",
                                truncate(&error, 200)
                            ),
                        }],
                    )
                }
                Some(info) if info.consecutive_failures < 4 => {
                    // §6.3.3 row 5 — already degraded, still under the
                    // 5-strike threshold. Increment + refresh last_error.
                    let mut new_info = info.clone();
                    new_info.consecutive_failures += 1;
                    new_info.last_error = error.clone();
                    let new_count = new_info.consecutive_failures;
                    let mut new_state = state.clone();
                    new_state.persist_degraded = Some(new_info);
                    (
                        new_state,
                        vec![Notify {
                            message: format!(
                                "⚠️ Persist failed again ({} consecutive); still continuing \
                                 in-memory: {}",
                                new_count,
                                truncate(&error, 200)
                            ),
                        }],
                    )
                }
                Some(info) => {
                    // §6.3.3 row 6 — already at 4 consecutive failures;
                    // this event is the 5th. Escalate to terminal.
                    // The arm above guarantees consecutive_failures == 4.
                    debug_assert_eq!(
                        info.consecutive_failures, 4,
                        "5-strike escalation expected exactly 4 prior failures"
                    );
                    (
                        state.clone()
                            .with_phase(Escalated)
                            .with_failed_phase(state.phase.clone())
                            .with_error_context(format!(
                                "5 consecutive periodic persist failures: {error}"
                            )),
                        vec![
                            Notify { message: format!(
                                "❌ 5 consecutive persist failures; aborting ritual: {}",
                                truncate(&error, 200)
                            )},
                            // No SaveState — persistence is what failed.
                        ],
                    )
                }
            },
        },

        (_, WorkspaceUnresolved { error }) => (
            state.clone()
                .with_phase(Escalated)
                .with_failed_phase(state.phase.clone())
                .with_error_context(format!("workspace resolution: {error}")),
            vec![
                Notify { message: format!(
                    "❌ Workspace resolution failed: {}",
                    truncate(&error, 200)
                )},
                // No SaveState: ritual never reached an executable phase;
                // there's no useful checkpoint to write.
            ],
        ),

        // ── ISS-052 T07: self-review subloop completion ────────────────────
        //
        // Forward `Accept` verdicts to the corresponding `SkillCompleted`
        // arm so the ritual progresses identically to a non-self-reviewed
        // skill. Prepend a notify so the subloop's outcome is visible in
        // the channel. `Reject` and turn-limit exhaustion never reach this
        // arm — the subloop converts them to `SkillFailed` directly. See
        // §8.2 of ISS-052 design.
        //
        // `NeedsChanges` is reserved (currently the subloop continues
        // without emitting an event); if a future gate ever surfaces it
        // here, the catch-all below routes it to `SkillFailed` so the
        // ritual fails closed rather than silently skipping the verdict.
        (_, SelfReviewCompleted { skill, verdict, turns_used }) => match verdict {
            ReviewVerdict::Accept => {
                let (next_state, mut actions) = transition(
                    state,
                    SkillCompleted {
                        phase: skill.clone(),
                        artifacts: vec![],
                    },
                );
                let notify = Notify {
                    message: format!(
                        "✅ Self-review accepted `{skill}` (turn {turns_used})."
                    ),
                };
                // Prepend the notify so it lands before whatever the
                // forwarded SkillCompleted arm emitted (typically a
                // phase-transition notify).
                actions.insert(0, notify);
                (next_state, actions)
            }
            ReviewVerdict::Reject | ReviewVerdict::NeedsChanges => {
                // Defensive: subloop folds Reject into SkillFailed and
                // re-loops on NeedsChanges. Reaching this arm means a
                // future gate surfaced the variant directly. Fail closed
                // by routing through SkillFailed with ReviewRejected
                // (verdict-specific reason was added in T05).
                let reason = match verdict {
                    ReviewVerdict::Reject => SkillFailureReason::ReviewRejected,
                    ReviewVerdict::NeedsChanges => SkillFailureReason::ReviewRejected,
                    ReviewVerdict::Accept => unreachable!(),
                };
                transition(
                    state,
                    SkillFailed {
                        phase: skill,
                        error: format!(
                            "self-review returned {verdict:?} after {turns_used} turn(s)"
                        ),
                        reason: Some(reason),
                    },
                )
            }
        },

        // Retry from Escalated
        // Root fix: reset retry counters + re-detect project state for Design/Graph phases
        (Escalated, UserRetry) => {
            let retry_phase = state.failed_phase.clone().unwrap_or(Implementing);
            let context = format!(
                "RETRY after escalation.\nPrevious error: {}\n\nOriginal task: {}",
                state.error_context.as_deref().unwrap_or("unknown"),
                state.task
            );

            // Reset retry counters for the phase being retried
            let mut new_state = state.clone()
                .with_error_context(String::new());
            match &retry_phase {
                Verifying => { new_state.verify_retries = 0; }
                Designing => { new_state.phase_retries.remove("designing"); }
                Graphing => { new_state.phase_retries.remove("graphing"); }
                Implementing => { new_state.phase_retries.remove("implementing"); }
                Planning => { new_state.phase_retries.remove("planning"); }
                Triaging => { new_state.phase_retries.remove("triaging"); }
                _ => {}
            }

            // Design/Graph/Triage retry: re-detect project state
            if matches!(retry_phase, Designing | Graphing | Triaging) {
                return (
                    new_state.with_phase(Initializing),
                    vec![
                        Notify { message: "🔄 Retrying — re-detecting project state...".into() },
                        SaveState { kind: SaveStateKind::Boundary },
                        DetectProject,
                    ],
                );
            }

            let action = match &retry_phase {
                Planning => RunPlanning,
                Implementing => RunSkill {
                    name: "implement".into(),
                    context,
                },
                Verifying => RunShell {
                    command: state.verify_command().to_string(),
                },
                _ => RunSkill {
                    name: "implement".into(),
                    context,
                },
            };
            (
                new_state.with_phase(retry_phase),
                vec![
                    Notify { message: "🔄 Retrying...".into() },
                    SaveState { kind: SaveStateKind::Boundary },
                    action,
                ],
            )
        }

        // Skip current phase
        (phase, UserSkipPhase) => {
            match phase.next() {
                Some(next_phase) => {
                    let action = match &next_phase {
                        Triaging => {
                            // Skip triage → go straight to designing
                            // Need project state, so re-detect if missing
                            if state.project.is_none() {
                                return (
                                    state.clone().with_phase(Initializing),
                                    vec![
                                        Notify { message: format!("⏭️ Skipped {}. Detecting project...", phase.display_name()) },
                                        SaveState { kind: SaveStateKind::Boundary },
                                        DetectProject,
                                    ],
                                );
                            }
                            // Run triage (can't skip to Designing without project detection)
                            RunTriage { task: state.task.clone() }
                        }
                        Designing => {
                            let skill = if state.project.as_ref().is_some_and(|p| p.has_design) {
                                "update-design"
                            } else {
                                "draft-design"
                            };
                            RunSkill { name: skill.into(), context: state.task.clone() }
                        }
                        WaitingClarification => {
                            return (
                                state.clone()
                                    .with_phase(Escalated)
                                    .with_failed_phase(phase.clone()),
                                vec![
                                    Notify { message: "❌ Cannot skip to WaitingClarification.".into() },
                                    SaveState { kind: SaveStateKind::Boundary },
                                ],
                            );
                        }
                        // Skip review → go to the phase after review
                        Reviewing | WaitingApproval => {
                            // Determine where to go based on current phase
                            return match phase {
                                WritingRequirements => {
                                    let skill = if state.project.as_ref().is_some_and(|p| p.has_design) {
                                        "update-design"
                                    } else {
                                        "draft-design"
                                    };
                                    (
                                        state.clone().with_phase(Designing),
                                        vec![
                                            Notify { message: "⏭️ Skipping review, moving to design...".into() },
                                            SaveState { kind: SaveStateKind::Boundary },
                                            RunSkill { name: skill.into(), context: state.task.clone() },
                                        ],
                                    )
                                }
                                Designing => (
                                    state.clone().with_phase(Planning),
                                    vec![
                                        Notify { message: "⏭️ Skipping review, moving to planning...".into() },
                                        SaveState { kind: SaveStateKind::Boundary },
                                        RunPlanning,
                                    ],
                                ),
                                Graphing => {
                                    let action = match &state.strategy {
                                        Some(ImplementStrategy::MultiAgent { tasks }) => RunHarness { tasks: tasks.clone() },
                                        _ => RunSkill { name: "implement".into(), context: state.task.clone() },
                                    };
                                    (
                                        state.clone().with_phase(Implementing),
                                        vec![
                                            Notify { message: "⏭️ Skipping review, moving to implementation...".into() },
                                            SaveState { kind: SaveStateKind::Boundary },
                                            action,
                                        ],
                                    )
                                }
                                _ => (
                                    state.clone().with_phase(Planning),
                                    vec![
                                        Notify { message: "⏭️ Skipping review...".into() },
                                        SaveState { kind: SaveStateKind::Boundary },
                                        RunPlanning,
                                    ],
                                ),
                            };
                        }
                        Planning => RunPlanning,
                        Graphing => {
                            let skill = if state.project.as_ref().is_some_and(|p| p.has_graph) {
                                "update-graph"
                            } else {
                                "generate-graph"
                            };
                            RunSkill { name: skill.into(), context: state.task.clone() }
                        }
                        Implementing => {
                            match &state.strategy {
                                Some(ImplementStrategy::MultiAgent { tasks }) =>
                                    RunHarness { tasks: tasks.clone() },
                                _ =>
                                    RunSkill { name: "implement".into(), context: state.task.clone() },
                            }
                        }
                        Verifying => RunShell { command: state.verify_command().to_string() },
                        Done => {
                            return (
                                state.clone().with_phase(Done),
                                vec![
                                    Notify { message: format!("⏭️ Skipped {}. Ritual complete.", phase.display_name()) },
                                    SaveState { kind: SaveStateKind::Boundary },
                                ],
                            );
                        }
                        _ => {
                            return (
                                state.clone()
                                    .with_phase(Escalated)
                                    .with_failed_phase(phase.clone()),
                                vec![
                                    Notify { message: format!("❌ Cannot skip to {:?}.", next_phase) },
                                    SaveState { kind: SaveStateKind::Boundary },
                                ],
                            );
                        }
                    };
                    (
                        state.clone().with_phase(next_phase.clone()),
                        vec![
                            Notify { message: format!("⏭️ Skipped {}. Moving to {}...", phase.display_name(), next_phase.display_name()) },
                            SaveState { kind: SaveStateKind::Boundary },
                            action,
                        ],
                    )
                }
                None => (
                    state.clone()
                        .with_phase(Escalated)
                        .with_failed_phase(phase.clone()),
                    vec![
                        Notify { message: format!("❌ Cannot skip {} — no next phase.", phase.display_name()) },
                        SaveState { kind: SaveStateKind::Boundary },
                    ],
                ),
            }
        }

        // ═══════════════════════════════════════
        // Catch-all → Escalated
        // ═══════════════════════════════════════

        // Invariant: every transition → terminal OR 1 EP action. No silent no-ops.
        (phase, event) => (
            state.clone()
                .with_phase(Escalated)
                .with_failed_phase(phase.clone())
                .with_error_context(format!(
                    "Unexpected event {:?} in phase {}",
                    std::mem::discriminant(&event),
                    phase.display_name()
                )),
            vec![
                Notify { message: format!(
                    "❌ Unexpected event in {}. Ritual paused — use /ritual retry or /ritual cancel.",
                    phase.display_name()
                )},
                SaveState { kind: SaveStateKind::Boundary },
            ],
        ),
    }
}

/// UTF-8 safe truncation.
pub fn truncate(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn idle_state() -> RitualState {
        RitualState::new()
    }

    fn project_with_design() -> ProjectState {
        ProjectState {
            has_requirements: true,
            has_design: true,
            has_graph: false,
            has_source: true,
            has_tests: false,
            language: Some("rust".into()),
            source_file_count: 10,
            verify_command: Some("cargo build 2>&1 && cargo test 2>&1".into()),
        }
    }

    fn project_greenfield() -> ProjectState {
        ProjectState {
            has_requirements: false,
            has_design: false,
            has_graph: false,
            has_source: false,
            has_tests: false,
            language: None,
            source_file_count: 0,
            verify_command: None,
        }
    }

    // ── Invariant checks ──

    fn assert_invariant(state: &RitualState, actions: &[RitualAction]) {
        let ep_count = actions.iter().filter(|a| a.is_event_producing()).count();
        if state.phase.is_terminal() || state.phase.is_paused() {
            assert_eq!(ep_count, 0,
                "Terminal/paused state {:?} must have 0 EP actions, got {}",
                state.phase, ep_count);
        } else {
            assert_eq!(ep_count, 1,
                "Non-terminal state {:?} must have exactly 1 EP action, got {}",
                state.phase, ep_count);
        }
    }

    // ── Happy path ──

    #[test]
    fn test_happy_path_start() {
        let (s, a) = transition(&idle_state(), RitualEvent::Start { task: "add feature".into() });
        assert_eq!(s.phase, RitualPhase::Initializing);
        assert_eq!(s.task, "add feature");
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_happy_path_project_detected_greenfield() {
        let state = idle_state().with_phase(RitualPhase::Initializing).with_task("test".into());
        let (s, a) = transition(&state, RitualEvent::ProjectDetected(project_greenfield()));
        assert_eq!(s.phase, RitualPhase::Triaging);
        let has_triage = a.iter().any(|a| matches!(a, RitualAction::RunTriage { .. }));
        assert!(has_triage);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_happy_path_project_detected_existing() {
        let state = idle_state().with_phase(RitualPhase::Initializing).with_task("test".into());
        let (s, a) = transition(&state, RitualEvent::ProjectDetected(project_with_design()));
        assert_eq!(s.phase, RitualPhase::Triaging);
        let has_triage = a.iter().any(|a| matches!(a, RitualAction::RunTriage { .. }));
        assert!(has_triage);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_happy_path_design_complete() {
        // Design → Reviewing
        let state = idle_state().with_phase(RitualPhase::Designing);
        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "design".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::Reviewing);
        assert_eq!(s.review_target, Some("design".to_string()));
        assert_invariant(&s, &a);

        // Review round 1 → WaitingApproval
        let (s2, a2) = transition(&s, RitualEvent::SkillCompleted { phase: "review-design".into(), artifacts: vec![] });
        assert_eq!(s2.phase, RitualPhase::WaitingApproval);
        assert_eq!(s2.review_round, 1);
        assert_invariant(&s2, &a2);

        // Approval round 1 → back to Reviewing (round 2)
        let (s3, a3) = transition(&s2, RitualEvent::UserApproval { approved: "all".into() });
        assert_eq!(s3.phase, RitualPhase::Reviewing);
        assert_invariant(&s3, &a3);

        // Review round 2 → WaitingApproval
        let (s4, a4) = transition(&s3, RitualEvent::SkillCompleted { phase: "review-design".into(), artifacts: vec![] });
        assert_eq!(s4.phase, RitualPhase::WaitingApproval);
        assert_eq!(s4.review_round, 2);
        assert_invariant(&s4, &a4);

        // Approval round 2 → Planning (proceed to next phase)
        let (s5, a5) = transition(&s4, RitualEvent::UserApproval { approved: "all".into() });
        assert_eq!(s5.phase, RitualPhase::Planning);
        assert_eq!(s5.review_round, 0); // reset
        assert_invariant(&s5, &a5);
    }

    #[test]
    fn test_happy_path_plan_decided() {
        let state = idle_state()
            .with_phase(RitualPhase::Planning)
            .with_project(project_greenfield());
        let (s, a) = transition(&state, RitualEvent::PlanDecided(ImplementStrategy::SingleLlm));
        assert_eq!(s.phase, RitualPhase::Graphing);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_happy_path_graph_complete() {
        // Graph → Reviewing
        let state = idle_state().with_phase(RitualPhase::Graphing);
        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "graph".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::Reviewing);
        assert_eq!(s.review_target, Some("tasks".to_string()));
        assert_invariant(&s, &a);

        // Review round 1 → WaitingApproval → Approval → back to Reviewing
        let (s2, _) = transition(&s, RitualEvent::SkillCompleted { phase: "review-tasks".into(), artifacts: vec![] });
        assert_eq!(s2.phase, RitualPhase::WaitingApproval);
        assert_eq!(s2.review_round, 1);
        let (s3, a3) = transition(&s2, RitualEvent::UserApproval { approved: "all".into() });
        assert_eq!(s3.phase, RitualPhase::Reviewing); // back for round 2
        assert_invariant(&s3, &a3);

        // Review round 2 → WaitingApproval → Approval → Implementing
        let (s4, _) = transition(&s3, RitualEvent::SkillCompleted { phase: "review-tasks".into(), artifacts: vec![] });
        assert_eq!(s4.phase, RitualPhase::WaitingApproval);
        assert_eq!(s4.review_round, 2);
        let (s5, a5) = transition(&s4, RitualEvent::UserApproval { approved: "all".into() });
        assert_eq!(s5.phase, RitualPhase::Implementing);
        assert_eq!(s5.review_round, 0);
        assert_invariant(&s5, &a5);
    }

    #[test]
    fn test_happy_path_implement_complete() {
        let state = idle_state()
            .with_phase(RitualPhase::Implementing)
            .with_project(project_with_design());
        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "impl".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::Verifying);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_happy_path_verify_success() {
        let state = idle_state().with_phase(RitualPhase::Verifying).with_task("test".into());
        let (s, a) = transition(&state, RitualEvent::ShellCompleted { stdout: "ok".into(), exit_code: 0 });
        assert_eq!(s.phase, RitualPhase::Done);
        assert!(s.phase.is_terminal());
        assert_invariant(&s, &a);
    }

    // ── Failure paths ──

    #[test]
    fn test_verify_fail_retry() {
        let state = idle_state()
            .with_phase(RitualPhase::Verifying)
            .with_task("test".into());
        let (s, a) = transition(&state, RitualEvent::ShellFailed { stderr: "error".into(), exit_code: 1 });
        assert_eq!(s.phase, RitualPhase::Implementing);
        assert_eq!(s.verify_retries, 1);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_verify_fail_escalate_after_3() {
        let mut state = idle_state()
            .with_phase(RitualPhase::Verifying)
            .with_task("test".into());
        state.verify_retries = 3;
        let (s, a) = transition(&state, RitualEvent::ShellFailed { stderr: "error".into(), exit_code: 1 });
        assert_eq!(s.phase, RitualPhase::Escalated);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_design_fail_retry_once() {
        let state = idle_state()
            .with_phase(RitualPhase::Designing)
            .with_task("test".into())
            .with_project(project_greenfield());
        let (s, a) = transition(&state, RitualEvent::SkillFailed { phase: "design".into(), error: "oops".into(), reason: None });
        assert_eq!(s.phase, RitualPhase::Designing);
        assert_eq!(s.retries_for("designing"), 1);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_design_fail_escalate_after_retry() {
        let mut state = idle_state()
            .with_phase(RitualPhase::Designing)
            .with_task("test".into());
        state.phase_retries.insert("designing".into(), 2);
        let (s, a) = transition(&state, RitualEvent::SkillFailed { phase: "design".into(), error: "oops".into(), reason: None });
        assert_eq!(s.phase, RitualPhase::Escalated);
        assert_invariant(&s, &a);
    }

    // ── User interaction ──

    #[test]
    fn test_cancel() {
        let state = idle_state().with_phase(RitualPhase::Implementing);
        let (s, a) = transition(&state, RitualEvent::UserCancel);
        assert_eq!(s.phase, RitualPhase::Cancelled);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_retry_from_escalated() {
        let state = idle_state()
            .with_phase(RitualPhase::Escalated)
            .with_failed_phase(RitualPhase::Implementing)
            .with_task("test".into())
            .with_project(project_with_design());
        let (s, a) = transition(&state, RitualEvent::UserRetry);
        assert_eq!(s.phase, RitualPhase::Implementing);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_retry_resets_verify_retries() {
        let mut state = idle_state()
            .with_phase(RitualPhase::Escalated)
            .with_failed_phase(RitualPhase::Verifying)
            .with_task("test".into())
            .with_project(project_with_design());
        state.verify_retries = 3;
        let (s, a) = transition(&state, RitualEvent::UserRetry);
        assert_eq!(s.phase, RitualPhase::Verifying);
        assert_eq!(s.verify_retries, 0, "UserRetry must reset verify_retries");
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_retry_resets_phase_retries() {
        let mut state = idle_state()
            .with_phase(RitualPhase::Escalated)
            .with_failed_phase(RitualPhase::Implementing)
            .with_task("test".into());
        state.phase_retries.insert("implementing".into(), 1);
        let (s, a) = transition(&state, RitualEvent::UserRetry);
        assert_eq!(s.phase, RitualPhase::Implementing);
        assert_eq!(s.retries_for("implementing"), 0, "UserRetry must reset phase_retries");
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_retry_design_re_detects_project() {
        let state = idle_state()
            .with_phase(RitualPhase::Escalated)
            .with_failed_phase(RitualPhase::Designing)
            .with_task("test".into())
            .with_project(project_greenfield());
        let (s, a) = transition(&state, RitualEvent::UserRetry);
        // Should go to Initializing to re-detect (DESIGN.md may now exist)
        assert_eq!(s.phase, RitualPhase::Initializing);
        let has_detect = a.iter().any(|a| matches!(a, RitualAction::DetectProject));
        assert!(has_detect, "Design retry must re-detect project state");
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_planning_retry_once() {
        let state = idle_state()
            .with_phase(RitualPhase::Planning)
            .with_task("test".into());
        let (s, a) = transition(&state, RitualEvent::SkillFailed { phase: "planning".into(), error: "oops".into(), reason: None });
        assert_eq!(s.phase, RitualPhase::Planning);
        assert_eq!(s.retries_for("planning"), 1);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_planning_escalate_after_retry() {
        let mut state = idle_state()
            .with_phase(RitualPhase::Planning)
            .with_task("test".into());
        state.phase_retries.insert("planning".into(), 2);
        let (s, a) = transition(&state, RitualEvent::SkillFailed { phase: "planning".into(), error: "oops".into(), reason: None });
        assert_eq!(s.phase, RitualPhase::Escalated);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_skip_design_to_planning() {
        // Skip from Designing → skips review → goes to Planning
        let state = idle_state().with_phase(RitualPhase::Designing).with_task("test".into());
        let (s, a) = transition(&state, RitualEvent::UserSkipPhase);
        assert_eq!(s.phase, RitualPhase::Planning);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_skip_review_approval() {
        // Skip from WaitingApproval (design) → Planning
        let state = idle_state()
            .with_phase(RitualPhase::WaitingApproval)
            .with_review_target("design")
            .with_task("test".into());
        let (s, a) = transition(&state, RitualEvent::UserSkipPhase);
        assert_eq!(s.phase, RitualPhase::Planning);
        assert_invariant(&s, &a);

        // Skip from WaitingApproval (tasks) → Implementing
        let state2 = idle_state()
            .with_phase(RitualPhase::WaitingApproval)
            .with_review_target("tasks")
            .with_task("test".into());
        let (s2, a2) = transition(&state2, RitualEvent::UserSkipPhase);
        assert_eq!(s2.phase, RitualPhase::Implementing);
        assert_invariant(&s2, &a2);
    }

    #[test]
    fn test_skip_initializing_to_designing() {
        let state = idle_state().with_phase(RitualPhase::Initializing).with_task("test".into());
        let (s, a) = transition(&state, RitualEvent::UserSkipPhase);
        // Should go to Initializing (to run DetectProject) rather than Designing directly
        assert_eq!(s.phase, RitualPhase::Initializing);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_skip_verifying_to_done() {
        let state = idle_state().with_phase(RitualPhase::Verifying).with_task("test".into());
        let (s, a) = transition(&state, RitualEvent::UserSkipPhase);
        assert_eq!(s.phase, RitualPhase::Done);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_skip_idle_escalates() {
        let state = idle_state();
        let (s, a) = transition(&state, RitualEvent::UserSkipPhase);
        assert_eq!(s.phase, RitualPhase::Escalated);
        assert_invariant(&s, &a);
    }

    // ── Catch-all ──

    #[test]
    fn test_unexpected_event_escalates() {
        let state = idle_state().with_phase(RitualPhase::Designing);
        // ShellCompleted doesn't happen in Designing
        let (s, a) = transition(&state, RitualEvent::ShellCompleted { stdout: "x".into(), exit_code: 0 });
        assert_eq!(s.phase, RitualPhase::Escalated);
        assert_invariant(&s, &a);
    }

    // ── Multi-agent path ──

    #[test]
    fn test_multi_agent_strategy() {
        let state = idle_state()
            .with_phase(RitualPhase::Graphing)
            .with_strategy(ImplementStrategy::MultiAgent { tasks: vec!["task1".into(), "task2".into()] });
        // Graph → Reviewing
        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "graph".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::Reviewing);
        assert_invariant(&s, &a);

        // Review round 1 → WaitingApproval → Approve → back to Reviewing
        let (s2, _) = transition(&s, RitualEvent::SkillCompleted { phase: "review-tasks".into(), artifacts: vec![] });
        let (s3, a3) = transition(&s2, RitualEvent::UserApproval { approved: "all".into() });
        assert_eq!(s3.phase, RitualPhase::Reviewing); // round 2
        assert_invariant(&s3, &a3);

        // Review round 2 → WaitingApproval → Approve → Implementing with Harness
        let (s4, _) = transition(&s3, RitualEvent::SkillCompleted { phase: "review-tasks".into(), artifacts: vec![] });
        let (s5, a5) = transition(&s4, RitualEvent::UserApproval { approved: "all".into() });
        assert_eq!(s5.phase, RitualPhase::Implementing);
        let has_harness = a5.iter().any(|a| matches!(a, RitualAction::RunHarness { .. }));
        assert!(has_harness);
        assert_invariant(&s5, &a5);
    }

    // ── Triage ──

    fn triage_clear_small() -> TriageResult {
        TriageResult {
            clarity: "clear".into(),
            clarify_questions: vec![],
            size: "small".into(),
            skip_design: true,
            skip_graph: true,
        }
    }

    fn triage_clear_medium() -> TriageResult {
        TriageResult {
            clarity: "clear".into(),
            clarify_questions: vec![],
            size: "medium".into(),
            skip_design: true,
            skip_graph: false,
        }
    }

    fn triage_ambiguous() -> TriageResult {
        TriageResult {
            clarity: "ambiguous".into(),
            clarify_questions: vec!["What file?".into(), "Which module?".into()],
            size: "medium".into(),
            skip_design: false,
            skip_graph: false,
        }
    }

    #[test]
    fn test_triage_small_skips_design_and_graph() {
        let state = idle_state()
            .with_phase(RitualPhase::Triaging)
            .with_task("fix typo".into())
            .with_project(project_with_design());
        let (s, a) = transition(&state, RitualEvent::TriageCompleted(triage_clear_small()));
        // Small task: skip to Planning (which decides SingleLlm → implement directly)
        assert_eq!(s.phase, RitualPhase::Planning);
        let has_planning = a.iter().any(|a| matches!(a, RitualAction::RunPlanning));
        assert!(has_planning);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_triage_medium_skips_design() {
        let state = idle_state()
            .with_phase(RitualPhase::Triaging)
            .with_task("add endpoint".into())
            .with_project(project_with_design());
        let (s, a) = transition(&state, RitualEvent::TriageCompleted(triage_clear_medium()));
        assert_eq!(s.phase, RitualPhase::Planning);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_triage_large_full_flow() {
        let state = idle_state()
            .with_phase(RitualPhase::Triaging)
            .with_task("new subsystem".into())
            .with_project(project_greenfield());
        let (s, a) = transition(&state, RitualEvent::TriageCompleted(TriageResult {
            clarity: "clear".into(),
            clarify_questions: vec![],
            size: "large".into(),
            skip_design: false,
            skip_graph: false,
        }));
        // Greenfield + large → starts with requirements
        assert_eq!(s.phase, RitualPhase::WritingRequirements);
        let has_draft_req = a.iter().any(|a| matches!(a, RitualAction::RunSkill { name, .. } if name == "draft-requirements"));
        assert!(has_draft_req);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_triage_ambiguous_waits() {
        let state = idle_state()
            .with_phase(RitualPhase::Triaging)
            .with_task("fix the bug".into())
            .with_project(project_with_design());
        let (s, a) = transition(&state, RitualEvent::TriageCompleted(triage_ambiguous()));
        assert_eq!(s.phase, RitualPhase::WaitingClarification);
        // Terminal-like (no EP action — waits for user)
        let ep_count = a.iter().filter(|a| a.is_event_producing()).count();
        assert_eq!(ep_count, 0, "WaitingClarification is a pause state with 0 EP actions");
    }

    #[test]
    fn test_clarification_re_triages() {
        let state = idle_state()
            .with_phase(RitualPhase::WaitingClarification)
            .with_task("fix the bug".into())
            .with_project(project_with_design());
        let (s, a) = transition(&state, RitualEvent::UserClarification {
            response: "the auth retry bug in llm.rs".into(),
        });
        assert_eq!(s.phase, RitualPhase::Triaging);
        assert!(s.task.contains("auth retry bug"));
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_skip_triage() {
        let state = idle_state()
            .with_phase(RitualPhase::Triaging)
            .with_task("test".into())
            .with_project(project_with_design());
        let (s, a) = transition(&state, RitualEvent::UserSkipPhase);
        // Skip triage → go to Designing
        assert_eq!(s.phase, RitualPhase::Designing);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_waiting_clarification_cancel() {
        let state = idle_state()
            .with_phase(RitualPhase::WaitingClarification)
            .with_task("test".into());
        let (s, a) = transition(&state, RitualEvent::UserCancel);
        assert_eq!(s.phase, RitualPhase::Cancelled);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_waiting_clarification_retry_re_triages() {
        let state = idle_state()
            .with_phase(RitualPhase::WaitingClarification)
            .with_task("fix something".into())
            .with_project(project_with_design());
        let (s, a) = transition(&state, RitualEvent::UserRetry);
        assert_eq!(s.phase, RitualPhase::Triaging);
        assert_invariant(&s, &a);
    }

    // ── Truncate ──

    #[test]
    fn test_truncate_ascii() {
        assert_eq!(truncate("hello world", 5), "hello");
        assert_eq!(truncate("hi", 10), "hi");
    }

    #[test]
    fn test_truncate_utf8() {
        assert_eq!(truncate("你好世界", 2), "你好");
        assert_eq!(truncate("hello你好", 6), "hello你");
    }

    // ── Full path trace ──

    #[test]
    fn test_full_happy_path_trace() {
        // Full large task path with 2-round reviews per target (requirements, design, tasks).
        // Each review target: write → review R1 → wait → approve → review R2 → wait → approve → next
        let mut state = idle_state();

        let (s, a) = transition(&state, RitualEvent::Start { task: "add X".into() });
        assert_eq!(s.phase, RitualPhase::Initializing);
        assert_invariant(&s, &a);
        state = s;

        let (s, a) = transition(&state, RitualEvent::ProjectDetected(project_greenfield()));
        assert_eq!(s.phase, RitualPhase::Triaging);
        assert_invariant(&s, &a);
        state = s;

        // Triage says large task, full flow → starts with requirements (greenfield)
        let (s, a) = transition(&state, RitualEvent::TriageCompleted(TriageResult {
            clarity: "clear".into(),
            clarify_questions: vec![],
            size: "large".into(),
            skip_design: false,
            skip_graph: false,
        }));
        assert_eq!(s.phase, RitualPhase::WritingRequirements);
        assert_invariant(&s, &a);
        state = s;

        // Requirements complete → Review requirements
        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "draft-requirements".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::Reviewing);
        assert_invariant(&s, &a);
        state = s;

        // Requirements review round 1 → WaitingApproval → Approve → back to Reviewing
        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "review-requirements".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::WaitingApproval);
        assert_eq!(s.review_round, 1);
        assert_invariant(&s, &a);
        state = s;

        let (s, a) = transition(&state, RitualEvent::UserApproval { approved: "all".into() });
        assert_eq!(s.phase, RitualPhase::Reviewing); // round 2
        assert_invariant(&s, &a);
        state = s;

        // Requirements review round 2 → WaitingApproval → Approve → Designing
        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "review-requirements".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::WaitingApproval);
        assert_eq!(s.review_round, 2);
        assert_invariant(&s, &a);
        state = s;

        let (s, a) = transition(&state, RitualEvent::UserApproval { approved: "all".into() });
        assert_eq!(s.phase, RitualPhase::Designing);
        assert_eq!(s.review_round, 0); // reset
        assert_invariant(&s, &a);
        state = s;

        // Design complete → Review design
        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "draft-design".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::Reviewing);
        assert_invariant(&s, &a);
        state = s;

        // Design review round 1 → WaitingApproval → Approve → back to Reviewing
        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "review-design".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::WaitingApproval);
        assert_eq!(s.review_round, 1);
        assert_invariant(&s, &a);
        state = s;

        let (s, a) = transition(&state, RitualEvent::UserApproval { approved: "all".into() });
        assert_eq!(s.phase, RitualPhase::Reviewing); // round 2
        assert_invariant(&s, &a);
        state = s;

        // Design review round 2 → WaitingApproval → Approve → Planning
        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "review-design".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::WaitingApproval);
        assert_eq!(s.review_round, 2);
        assert_invariant(&s, &a);
        state = s;

        let (s, a) = transition(&state, RitualEvent::UserApproval { approved: "all".into() });
        assert_eq!(s.phase, RitualPhase::Planning);
        assert_eq!(s.review_round, 0);
        assert_invariant(&s, &a);
        state = s;

        let (s, a) = transition(&state, RitualEvent::PlanDecided(ImplementStrategy::SingleLlm));
        assert_eq!(s.phase, RitualPhase::Graphing);
        assert_invariant(&s, &a);
        state = s;

        // Graph complete → Review tasks
        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "generate-graph".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::Reviewing);
        assert_invariant(&s, &a);
        state = s;

        // Tasks review round 1 → WaitingApproval → Approve → back to Reviewing
        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "review-tasks".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::WaitingApproval);
        assert_eq!(s.review_round, 1);
        assert_invariant(&s, &a);
        state = s;

        let (s, a) = transition(&state, RitualEvent::UserApproval { approved: "all".into() });
        assert_eq!(s.phase, RitualPhase::Reviewing); // round 2
        assert_invariant(&s, &a);
        state = s;

        // Tasks review round 2 → WaitingApproval → Approve → Implementing
        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "review-tasks".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::WaitingApproval);
        assert_eq!(s.review_round, 2);
        assert_invariant(&s, &a);
        state = s;

        let (s, a) = transition(&state, RitualEvent::UserApproval { approved: "all".into() });
        assert_eq!(s.phase, RitualPhase::Implementing);
        assert_eq!(s.review_round, 0);
        assert_invariant(&s, &a);
        state = s;

        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "implement".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::Verifying);
        assert_invariant(&s, &a);
        state = s;

        let (s, a) = transition(&state, RitualEvent::ShellCompleted { stdout: "all tests passed".into(), exit_code: 0 });
        assert_eq!(s.phase, RitualPhase::Done);
        assert_invariant(&s, &a);

        // With 2-round reviews: 3 review targets × 2 extra transitions each = 6 extra = 21 total
        assert_eq!(s.transitions.len(), 21);
    }

    #[test]
    fn test_verify_retry_loop_trace() {
        // Guard is `verify_retries < 3`, so retries 0,1,2 → Implementing, retry 3 → Escalated.
        // That's 4 verify attempts total (initial + 3 retries).
        let mut state = idle_state()
            .with_phase(RitualPhase::Implementing)
            .with_task("test".into())
            .with_project(project_with_design());

        // 3 rounds of: implement → verify → fail → back to implement
        for i in 0..3 {
            let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "impl".into(), artifacts: vec![] });
            assert_eq!(s.phase, RitualPhase::Verifying);
            assert_invariant(&s, &a);
            state = s;

            let (s, a) = transition(&state, RitualEvent::ShellFailed { stderr: format!("error {}", i), exit_code: 1 });
            assert_eq!(s.phase, RitualPhase::Implementing, "retry {} should go back to Implementing", i);
            assert_invariant(&s, &a);
            state = s;
        }

        assert_eq!(state.verify_retries, 3);

        // 4th attempt: implement succeeds → verify fails → escalate
        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "impl".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::Verifying);
        assert_invariant(&s, &a);
        state = s;

        let (s, a) = transition(&state, RitualEvent::ShellFailed { stderr: "still broken".into(), exit_code: 1 });
        assert_eq!(s.phase, RitualPhase::Escalated);
        assert_invariant(&s, &a);
    }

    // ── ISS-019: status field invariants ──────────────────────────────────

    #[test]
    fn test_status_default_is_active_for_new_state() {
        let s = RitualState::new();
        assert_eq!(s.status, RitualV2Status::Active);
        assert!(!s.phase.is_terminal());
    }

    #[test]
    fn test_status_active_for_in_flight_phases() {
        // Walking through the normal happy path, status must remain Active
        // until a terminal phase is entered.
        let s = idle_state()
            .with_phase(RitualPhase::Initializing)
            .with_phase(RitualPhase::Triaging)
            .with_phase(RitualPhase::Designing)
            .with_phase(RitualPhase::Reviewing)
            .with_phase(RitualPhase::WaitingApproval)
            .with_phase(RitualPhase::Planning)
            .with_phase(RitualPhase::Graphing)
            .with_phase(RitualPhase::Implementing)
            .with_phase(RitualPhase::Verifying);
        assert_eq!(s.status, RitualV2Status::Active);
    }

    #[test]
    fn test_status_done_when_phase_done() {
        let s = idle_state()
            .with_phase(RitualPhase::Verifying)
            .with_phase(RitualPhase::Done);
        assert_eq!(s.phase, RitualPhase::Done);
        assert_eq!(s.status, RitualV2Status::Done);
    }

    #[test]
    fn test_status_cancelled_when_phase_cancelled() {
        let state = idle_state().with_phase(RitualPhase::Implementing);
        let (s, _a) = transition(&state, RitualEvent::UserCancel);
        assert_eq!(s.phase, RitualPhase::Cancelled);
        assert_eq!(s.status, RitualV2Status::Cancelled);
    }

    #[test]
    fn test_status_failed_when_phase_escalated() {
        // Designing allows 2 retries before escalating (state_machine.rs:1051).
        // Hammer it 3× to reach the catch-all → Escalated arm at line 1108.
        let mut state = idle_state().with_phase(RitualPhase::Designing);
        for i in 0..3 {
            let (s, _a) = transition(&state, RitualEvent::SkillFailed {
                phase: "design".into(),
                error: format!("fatal {}", i),
                reason: None,
            });
            state = s;
        }
        assert_eq!(state.phase, RitualPhase::Escalated,
            "expected escalation after 3 failures, got {:?}", state.phase);
        assert_eq!(state.status, RitualV2Status::Failed);
    }

    #[test]
    fn test_status_idempotent_double_cancel() {
        // Cancelling a Cancelled ritual must not corrupt status (it should
        // remain Cancelled — no upgrade to Active, no spurious change).
        let state = idle_state().with_phase(RitualPhase::Implementing);
        let (s1, _) = transition(&state, RitualEvent::UserCancel);
        assert_eq!(s1.status, RitualV2Status::Cancelled);
        let (s2, _) = transition(&s1, RitualEvent::UserCancel);
        assert_eq!(s2.phase, RitualPhase::Cancelled);
        assert_eq!(s2.status, RitualV2Status::Cancelled);
    }

    // ════════════════════════════════════════════════════════════════════
    // ISS-052 T02d — hook-driven cancellation
    // ════════════════════════════════════════════════════════════════════

    #[test]
    fn test_hook_cancelled_event_routes_to_cancelled_phase() {
        // `RitualEvent::Cancelled { reason }` (emitted by V2Executor when
        // hooks.should_cancel() returns Some) must transition any phase to
        // terminal `Cancelled` with `Cancelled` status — same end state as
        // `UserCancel`, but with a reason-derived notify message.
        let state = idle_state().with_phase(RitualPhase::Implementing);
        let reason = crate::ritual::hooks::CancelReason {
            source: crate::ritual::hooks::CancelSource::UserCommand,
            message: "user requested /ritual cancel".into(),
        };
        let (s, actions) = transition(
            &state,
            RitualEvent::Cancelled { reason: reason.clone() },
        );
        assert_eq!(s.phase, RitualPhase::Cancelled);
        assert_eq!(s.status, RitualV2Status::Cancelled);
        // Notify carries the reason message; SaveState persists terminal phase.
        assert_eq!(actions.len(), 2);
        match &actions[0] {
            RitualAction::Notify { message } => {
                assert!(message.contains(&reason.message),
                    "notify must include reason message, got {:?}", message);
            }
            other => panic!("expected Notify, got {:?}", other),
        }
        assert!(matches!(&actions[1], RitualAction::SaveState { kind: SaveStateKind::Boundary }));
    }

    #[test]
    fn test_hook_cancelled_event_works_from_any_phase() {
        // The transition arm uses `(_, Cancelled { .. })` — must work from
        // every non-terminal phase. Spot-check a representative sample.
        let phases = [
            RitualPhase::Idle,
            RitualPhase::Designing,
            RitualPhase::Graphing,
            RitualPhase::Implementing,
            RitualPhase::Verifying,
        ];
        for phase in phases {
            let state = idle_state().with_phase(phase.clone());
            let reason = crate::ritual::hooks::CancelReason {
                source: crate::ritual::hooks::CancelSource::Timeout,
                message: "wall-clock budget exhausted".into(),
            };
            let (s, _a) = transition(&state, RitualEvent::Cancelled { reason });
            assert_eq!(s.phase, RitualPhase::Cancelled,
                "Cancelled event from {:?} must reach terminal Cancelled phase", phase);
        }
    }

    #[test]
    fn test_hook_cancelled_distinct_from_user_cancel_message() {
        // `UserCancel` (engine-surface event) emits a generic "🛑 Ritual cancelled."
        // notify. `Cancelled { reason }` (hook-driven event) emits a reason-
        // specific notify. Both produce the same terminal state, but observers
        // can distinguish *why* via the message.
        let state = idle_state().with_phase(RitualPhase::Implementing);

        let (_, user_actions) = transition(&state, RitualEvent::UserCancel);
        let user_msg = match &user_actions[0] {
            RitualAction::Notify { message } => message.clone(),
            _ => panic!("expected Notify"),
        };

        let reason = crate::ritual::hooks::CancelReason {
            source: crate::ritual::hooks::CancelSource::DaemonShutdown,
            message: "daemon shutting down".into(),
        };
        let (_, hook_actions) = transition(
            &state,
            RitualEvent::Cancelled { reason },
        );
        let hook_msg = match &hook_actions[0] {
            RitualAction::Notify { message } => message.clone(),
            _ => panic!("expected Notify"),
        };

        assert_ne!(user_msg, hook_msg,
            "UserCancel and Cancelled{{reason}} must produce distinguishable notify messages");
        assert!(hook_msg.contains("daemon shutting down"),
            "hook-driven cancel must surface reason in notify, got {:?}", hook_msg);
    }

    #[test]
    fn test_status_recovers_to_active_on_re_entry() {
        // If, hypothetically, with_phase is called with a non-terminal phase
        // after a terminal one (e.g. via UserRetry from Escalated), status
        // must follow the new phase. UserRetry is the real-world trigger.
        let state = idle_state()
            .with_phase(RitualPhase::Designing)
            .with_failed_phase(RitualPhase::Designing)
            .with_phase(RitualPhase::Escalated);
        assert_eq!(state.status, RitualV2Status::Failed);

        let (s, _a) = transition(&state, RitualEvent::UserRetry);
        // Retry from Escalated brings us back to a non-terminal phase.
        assert!(!s.phase.is_terminal(),
            "retry should leave terminal phase, was {:?}", s.phase);
        assert_eq!(s.status, RitualV2Status::Active,
            "status must revert to Active when re-entering non-terminal phase");
    }

    #[test]
    fn test_status_serde_default_for_legacy_state_files() {
        // A state file written before `status` existed (no field in JSON)
        // must deserialize as `Active`. This is the contract that protects
        // backwards compat for in-flight rituals across an upgrade boundary.
        let json = serde_json::json!({
            "id": "r-legacy",
            "phase": "Implementing",
            "task": "old ritual",
            "verify_retries": 0,
            "phase_retries": {},
            "phase_tokens": {},
            "transitions": [],
            "started_at": "2026-04-25T00:00:00Z",
            "updated_at": "2026-04-25T00:00:00Z",
            "project": null,
            "strategy": null,
            "failed_phase": null,
            "error_context": null,
        });
        let s: RitualState = serde_json::from_value(json).unwrap();
        assert_eq!(s.status, RitualV2Status::Active);
        assert_eq!(s.phase, RitualPhase::Implementing);
    }

    #[test]
    fn test_status_serde_roundtrip_preserves_terminal() {
        // A Done ritual serialized and reloaded must keep `Done` status.
        let s = idle_state()
            .with_phase(RitualPhase::Verifying)
            .with_phase(RitualPhase::Done);
        let json = serde_json::to_string(&s).unwrap();
        let s2: RitualState = serde_json::from_str(&json).unwrap();
        assert_eq!(s2.status, RitualV2Status::Done);
        assert_eq!(s2.phase, RitualPhase::Done);
    }

    #[test]
    fn test_terminal_status_mapping() {
        // The mapping is a contract — codify it.
        assert_eq!(RitualPhase::Done.terminal_status(), Some(RitualV2Status::Done));
        assert_eq!(RitualPhase::Cancelled.terminal_status(), Some(RitualV2Status::Cancelled));
        assert_eq!(RitualPhase::Escalated.terminal_status(), Some(RitualV2Status::Failed));
        assert_eq!(RitualPhase::Idle.terminal_status(), None);
        assert_eq!(RitualPhase::Implementing.terminal_status(), None);
        assert_eq!(RitualPhase::WaitingApproval.terminal_status(), None);
    }

    // ───────────────────────────────────────────────────────────────────
    // ISS-052 T02b: SkillFailureReason wiring
    // ───────────────────────────────────────────────────────────────────

    #[test]
    fn test_skill_failed_reason_default_none_legacy_emit() {
        // Legacy emit sites pass `reason: None`. The state machine MUST
        // accept this and behave identically to pre-T02b (one retry per
        // failure, escalate when retries exhausted).
        let state = idle_state()
            .with_phase(RitualPhase::Designing)
            .with_task("test".into())
            .with_project(project_greenfield());
        let (s, a) = transition(
            &state,
            RitualEvent::SkillFailed {
                phase: "design".into(),
                error: "legacy emit, no structured reason".into(),
                reason: None,
            },
        );
        // Behavior is unchanged: stay in Designing, increment retry counter.
        assert_eq!(s.phase, RitualPhase::Designing);
        assert_eq!(s.retries_for("designing"), 1);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_skill_failed_reason_some_zero_file_changes_routes_same_as_none() {
        // T02b is purely additive — the state machine does NOT yet branch
        // on `reason`. A `Some(ZeroFileChanges)` failure must produce the
        // same (state, actions) as `None`. This pins the behavior so
        // future changes that branch on `reason` (T02d, T02e) are
        // detected as intentional behavior changes, not silent regressions.
        let state = idle_state()
            .with_phase(RitualPhase::Designing)
            .with_task("test".into())
            .with_project(project_greenfield());
        let (s_none, _) = transition(
            &state,
            RitualEvent::SkillFailed {
                phase: "design".into(),
                error: "x".into(),
                reason: None,
            },
        );
        let (s_some, _) = transition(
            &state,
            RitualEvent::SkillFailed {
                phase: "design".into(),
                error: "x".into(),
                reason: Some(SkillFailureReason::ZeroFileChanges),
            },
        );
        assert_eq!(s_none.phase, s_some.phase);
        assert_eq!(s_none.retries_for("designing"), s_some.retries_for("designing"));
    }

    #[test]
    fn test_skill_failure_reason_variants_constructible() {
        // Codify the contract: all five variants exist and are
        // distinguishable. Acts as a tripwire if a variant is renamed or
        // removed without considering the gates that consume it
        // (ISS-038 file_snapshot, T02d/T02e self-review).
        let variants = [
            SkillFailureReason::ZeroFileChanges,
            SkillFailureReason::UnexpectedFileChanges,
            SkillFailureReason::LlmTurnLimitNoVerdict,
            SkillFailureReason::ReviewRejected,
            SkillFailureReason::ExplicitClaim("skill said: failed".into()),
        ];
        // All variants must be Eq-distinct from each other.
        for (i, v1) in variants.iter().enumerate() {
            for (j, v2) in variants.iter().enumerate() {
                if i == j {
                    assert_eq!(v1, v2);
                } else {
                    assert_ne!(v1, v2);
                }
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // ISS-052 T04: persistence + lifecycle event arms
    //
    // These tests pin the §6.3.3 transition table for `StatePersisted`
    // and `StatePersistFailed`. Six logical branches are exercised:
    //
    //   StatePersisted:
    //     1. persist_degraded == None   → no-op, no actions
    //        → test_state_persisted_steady_state_is_noop
    //     2. persist_degraded == Some   → clear flag + recovery Notify
    //        → test_state_persisted_clears_degraded_flag
    //
    //   StatePersistFailed:
    //     3. Boundary, any flag         → Escalated
    //        → test_boundary_persist_failed_escalates_immediately
    //        → test_boundary_persist_failed_escalates_even_when_degraded
    //     4. Periodic, flag == None     → flip flag, count=1, continue
    //        → test_first_periodic_persist_failure_flips_degraded_flag
    //     5. Periodic, flag == Some     → increment count, refresh err
    //          (count<4)                  → test_subsequent_periodic_failure_increments_count
    //     6. Periodic, flag == Some     → 5th consecutive failure → Escalated
    //          (count==4)                 → test_fifth_consecutive_periodic_failure_escalates
    //
    // Plus invariants preserved across all branches:
    //   - No StatePersist* arm emits SaveState (would loop)
    //   - WorkspaceUnresolved → Escalated (T08 pre-phase failure)
    //   - Variant Debug repr distinguishes StatePersisted /
    //     StatePersistFailed / WorkspaceUnresolved (tripwire test).
    // ─────────────────────────────────────────────────────────────────────

    // ── Branch 1: StatePersisted, no degradation → silent no-op ─────────
    #[test]
    fn test_state_persisted_steady_state_is_noop() {
        // §6.3.3 row 1 — when `persist_degraded` is None (the steady
        // state), a successful persist is silent: no phase change, no
        // actions. Routine save success would otherwise be spammy.
        let state = idle_state().with_phase(RitualPhase::Implementing);
        let phase_before = state.phase.clone();
        assert!(state.persist_degraded.is_none(),
            "precondition: idle_state has no degraded flag set");

        let (s, actions) = transition(
            &state,
            RitualEvent::StatePersisted {
                attempt: 1,
                kind: SaveStateKind::Boundary,
            },
        );
        assert_eq!(s.phase, phase_before,
            "StatePersisted (steady state) must not change phase");
        assert!(s.persist_degraded.is_none(),
            "StatePersisted (steady state) must leave persist_degraded unchanged");
        assert!(actions.is_empty(),
            "StatePersisted (steady state) must produce no actions; got {:?}", actions);

        // Same expected behavior for Periodic kind — both are silent
        // when not recovering from degradation.
        let (s2, actions2) = transition(
            &state,
            RitualEvent::StatePersisted {
                attempt: 1,
                kind: SaveStateKind::Periodic,
            },
        );
        assert_eq!(s2.phase, phase_before);
        assert!(s2.persist_degraded.is_none());
        assert!(actions2.is_empty());
    }

    #[test]
    fn test_state_persisted_attempt_n_steady_state_still_noop() {
        // The successful-after-retry case (`attempt > 1`) is structurally
        // identical to first-try success when the steady-state flag is
        // None — the attempt count is informational only.
        let state = idle_state().with_phase(RitualPhase::Verifying);
        let (s, actions) = transition(
            &state,
            RitualEvent::StatePersisted {
                attempt: 3,
                kind: SaveStateKind::Boundary,
            },
        );
        assert_eq!(s.phase, RitualPhase::Verifying);
        assert!(s.persist_degraded.is_none());
        assert!(actions.is_empty());
    }

    // ── Branch 2: StatePersisted while degraded → recovery ──────────────
    #[test]
    fn test_state_persisted_clears_degraded_flag() {
        // §6.3.3 row 2 — recovery path. After at least one Periodic
        // failure flipped the flag, a successful persist clears it back
        // to None and surfaces a recovery Notify so the operator knows
        // persistence is healthy again.
        let mut state = idle_state().with_phase(RitualPhase::Implementing);
        state.persist_degraded = Some(PersistDegradedInfo {
            since_phase: RitualPhase::Implementing,
            last_error: "ENOSPC".into(),
            consecutive_failures: 2,
        });

        let (s, actions) = transition(
            &state,
            RitualEvent::StatePersisted {
                attempt: 1,
                kind: SaveStateKind::Periodic,
            },
        );

        assert_eq!(s.phase, RitualPhase::Implementing,
            "recovery must not change phase — only clear the flag");
        assert!(s.persist_degraded.is_none(),
            "recovery must clear persist_degraded back to None");

        assert_eq!(actions.len(), 1, "recovery emits exactly one Notify");
        match &actions[0] {
            RitualAction::Notify { message } => {
                assert!(message.contains("recovered"),
                    "recovery message must mention recovery, got {:?}", message);
                assert!(message.contains("2"),
                    "recovery message must surface failure count, got {:?}", message);
            }
            other => panic!("expected Notify, got {:?}", other),
        }
        assert!(!actions.iter().any(|a| matches!(a, RitualAction::SaveState { .. })),
            "recovery must NOT emit SaveState — the persist that just succeeded              was triggered upstream, no need to re-save here");
    }

    #[test]
    fn test_state_persisted_recovery_singular_grammar_for_one_failure() {
        // Edge: count==1 should produce "1 failed attempt." (singular),
        // not "1 failed attempts." This is mostly a UX nicety but worth
        // pinning so the format string doesn't regress.
        let mut state = idle_state().with_phase(RitualPhase::Implementing);
        state.persist_degraded = Some(PersistDegradedInfo {
            since_phase: RitualPhase::Implementing,
            last_error: "EIO".into(),
            consecutive_failures: 1,
        });
        let (_, actions) = transition(
            &state,
            RitualEvent::StatePersisted {
                attempt: 2,
                kind: SaveStateKind::Periodic,
            },
        );
        match &actions[0] {
            RitualAction::Notify { message } => {
                assert!(message.contains("1 failed attempt."),
                    "singular grammar for count=1, got {:?}", message);
                assert!(!message.contains("attempts"),
                    "must not pluralize for count=1, got {:?}", message);
            }
            other => panic!("expected Notify, got {:?}", other),
        }
    }

    // ── Branch 3: Boundary persist failure → immediate escalation ───────
    #[test]
    fn test_boundary_persist_failed_escalates_immediately() {
        // §6.3.3 row 3 — boundary save failed. We cannot guarantee the
        // next phase starts from a known-persisted state, so abort to
        // Escalated. This is the "fail loud" branch.
        let state = idle_state().with_phase(RitualPhase::Implementing);
        let (s, actions) = transition(
            &state,
            RitualEvent::StatePersistFailed {
                attempt: 3,
                error: "ENOSPC: disk full".into(),
                kind: SaveStateKind::Boundary,
            },
        );
        assert_eq!(s.phase, RitualPhase::Escalated);
        assert_eq!(s.failed_phase, Some(RitualPhase::Implementing),
            "failed_phase must capture the phase that was running when persist died");
        let err_ctx = s.error_context.as_deref().unwrap_or("");
        assert!(err_ctx.starts_with("boundary persist failed"),
            "error_context must namespace boundary failures, got {:?}", err_ctx);
        assert!(err_ctx.contains("3 attempts"),
            "error_context must record attempt count, got {:?}", err_ctx);
        assert!(err_ctx.contains("ENOSPC"),
            "error_context must propagate underlying error, got {:?}", err_ctx);

        assert_eq!(actions.len(), 1, "expected single Notify, got {:?}", actions);
        match &actions[0] {
            RitualAction::Notify { message } => {
                assert!(message.starts_with("❌ Boundary persist failed"),
                    "notify must mark this as boundary failure, got {:?}", message);
                assert!(message.contains("3 attempts"),
                    "notify must surface attempt count, got {:?}", message);
                assert!(message.contains("ENOSPC"),
                    "notify must surface underlying error, got {:?}", message);
            }
            other => panic!("expected Notify, got {:?}", other),
        }
        assert!(!actions.iter().any(|a| matches!(a, RitualAction::SaveState { .. })),
            "StatePersistFailed must NOT emit SaveState (would loop)");
    }

    #[test]
    fn test_boundary_persist_failed_escalates_even_when_degraded() {
        // §6.3.3 row 3 — Boundary failure escalates regardless of any
        // prior periodic-degradation state. Even mid-degradation, if a
        // boundary save fails, we cannot enter the next phase safely.
        let mut state = idle_state().with_phase(RitualPhase::Verifying);
        state.persist_degraded = Some(PersistDegradedInfo {
            since_phase: RitualPhase::Implementing,
            last_error: "previous".into(),
            consecutive_failures: 2,
        });
        let (s, _actions) = transition(
            &state,
            RitualEvent::StatePersistFailed {
                attempt: 3,
                error: "ENOSPC".into(),
                kind: SaveStateKind::Boundary,
            },
        );
        assert_eq!(s.phase, RitualPhase::Escalated,
            "Boundary failure must escalate even when already degraded");
    }

    #[test]
    fn test_boundary_persist_failed_works_from_any_phase() {
        // Boundary saves can fail at any non-terminal phase. The arm is
        // unconditional on phase; verify representative phases all
        // escalate consistently.
        let phases = [
            RitualPhase::Designing,
            RitualPhase::Graphing,
            RitualPhase::Implementing,
            RitualPhase::Verifying,
        ];
        for phase in phases {
            let state = idle_state().with_phase(phase.clone());
            let (s, _a) = transition(
                &state,
                RitualEvent::StatePersistFailed {
                    attempt: 3,
                    error: "io error".into(),
                    kind: SaveStateKind::Boundary,
                },
            );
            assert_eq!(s.phase, RitualPhase::Escalated,
                "Boundary StatePersistFailed from {:?} must escalate", phase);
            assert_eq!(s.failed_phase, Some(phase));
        }
    }

    // ── Branch 4: First Periodic failure → flip degraded flag ───────────
    #[test]
    fn test_first_periodic_persist_failure_flips_degraded_flag() {
        // §6.3.3 row 4 — first periodic failure. The ritual continues
        // in-memory, the flag is flipped to count=1, and the operator
        // is notified. Phase is unchanged so the dispatcher resumes
        // whatever long-running phase was in flight.
        let state = idle_state().with_phase(RitualPhase::Implementing);
        assert!(state.persist_degraded.is_none(),
            "precondition: clean steady state");

        let (s, actions) = transition(
            &state,
            RitualEvent::StatePersistFailed {
                attempt: 3,
                error: "ENOSPC: disk full".into(),
                kind: SaveStateKind::Periodic,
            },
        );

        assert_eq!(s.phase, RitualPhase::Implementing,
            "Periodic failure must NOT change phase — ritual continues in-memory");
        let info = s.persist_degraded.as_ref()
            .expect("Periodic failure must set persist_degraded");
        assert_eq!(info.since_phase, RitualPhase::Implementing,
            "since_phase records the phase at which degradation began");
        assert_eq!(info.consecutive_failures, 1,
            "first failure → counter starts at 1");
        assert_eq!(info.last_error, "ENOSPC: disk full",
            "last_error captures the failing error");

        assert_eq!(actions.len(), 1, "expected single Notify, got {:?}", actions);
        match &actions[0] {
            RitualAction::Notify { message } => {
                assert!(message.starts_with("⚠️"),
                    "first-failure notify is a warning, got {:?}", message);
                assert!(message.contains("in-memory"),
                    "notify must explain ritual is continuing in-memory, got {:?}", message);
                assert!(message.contains("ENOSPC"),
                    "notify must surface error, got {:?}", message);
            }
            other => panic!("expected Notify, got {:?}", other),
        }
        assert!(!actions.iter().any(|a| matches!(a, RitualAction::SaveState { .. })),
            "Periodic-failure path must NOT emit SaveState (would loop)");
    }

    // ── Branch 5: Subsequent Periodic failure → increment counter ───────
    #[test]
    fn test_subsequent_periodic_failure_increments_count() {
        // §6.3.3 row 5 — already degraded, still under the 5-strike
        // threshold. Increment counter, refresh last_error, continue.
        let mut state = idle_state().with_phase(RitualPhase::Implementing);
        state.persist_degraded = Some(PersistDegradedInfo {
            since_phase: RitualPhase::Implementing,
            last_error: "first error".into(),
            consecutive_failures: 1,
        });

        let (s, actions) = transition(
            &state,
            RitualEvent::StatePersistFailed {
                attempt: 3,
                error: "second error".into(),
                kind: SaveStateKind::Periodic,
            },
        );

        assert_eq!(s.phase, RitualPhase::Implementing,
            "still under threshold — phase unchanged");
        let info = s.persist_degraded.as_ref().expect("flag still set");
        assert_eq!(info.consecutive_failures, 2,
            "counter incremented from 1 to 2");
        assert_eq!(info.last_error, "second error",
            "last_error refreshed to current failure");
        assert_eq!(info.since_phase, RitualPhase::Implementing,
            "since_phase preserved across consecutive failures");

        match &actions[0] {
            RitualAction::Notify { message } => {
                assert!(message.contains("2 consecutive"),
                    "notify must surface running count, got {:?}", message);
            }
            other => panic!("expected Notify, got {:?}", other),
        }
        assert!(!actions.iter().any(|a| matches!(a, RitualAction::SaveState { .. })));
    }

    #[test]
    fn test_periodic_failure_counter_walks_2_3_4_without_escalating() {
        // Walks the counter through 2 → 3 → 4 to prove the threshold is
        // strictly `> 4` (i.e. count==4 is still continue, count would
        // reach 5 → escalate, see test_fifth_consecutive_periodic_failure).
        let mut state = idle_state().with_phase(RitualPhase::Verifying);
        state.persist_degraded = Some(PersistDegradedInfo {
            since_phase: RitualPhase::Verifying,
            last_error: "e1".into(),
            consecutive_failures: 1,
        });

        for expected in 2..=4 {
            let (next, _actions) = transition(
                &state,
                RitualEvent::StatePersistFailed {
                    attempt: 3,
                    error: format!("e{}", expected),
                    kind: SaveStateKind::Periodic,
                },
            );
            assert_eq!(next.phase, RitualPhase::Verifying,
                "count={} must not escalate", expected);
            let info = next.persist_degraded.as_ref().unwrap();
            assert_eq!(info.consecutive_failures, expected,
                "count must reach exactly {}", expected);
            state = next;
        }
        // After 4 failures we are still Verifying, still degraded, ready
        // for the 5-strike test below to escalate on the *next* failure.
        assert_eq!(state.persist_degraded.as_ref().unwrap().consecutive_failures, 4);
    }

    // ── Branch 6: 5th consecutive Periodic failure → escalate ───────────
    #[test]
    fn test_fifth_consecutive_periodic_failure_escalates() {
        // §6.3.3 row 6 — already at 4 consecutive failures; this event
        // is the 5th. Escalate to terminal Escalated.
        let mut state = idle_state().with_phase(RitualPhase::Implementing);
        state.persist_degraded = Some(PersistDegradedInfo {
            since_phase: RitualPhase::Implementing,
            last_error: "prev".into(),
            consecutive_failures: 4,
        });

        let (s, actions) = transition(
            &state,
            RitualEvent::StatePersistFailed {
                attempt: 3,
                error: "the fifth strike".into(),
                kind: SaveStateKind::Periodic,
            },
        );

        assert_eq!(s.phase, RitualPhase::Escalated,
            "5th consecutive periodic failure must escalate");
        assert_eq!(s.failed_phase, Some(RitualPhase::Implementing),
            "failed_phase records the phase that was running");
        let err_ctx = s.error_context.as_deref().unwrap_or("");
        assert!(err_ctx.contains("5 consecutive"),
            "error_context must mention the 5-strike rule, got {:?}", err_ctx);
        assert!(err_ctx.contains("the fifth strike"),
            "error_context must propagate the triggering error, got {:?}", err_ctx);

        assert_eq!(actions.len(), 1, "expected single Notify, got {:?}", actions);
        match &actions[0] {
            RitualAction::Notify { message } => {
                assert!(message.starts_with("❌"),
                    "5-strike notify is fatal, got {:?}", message);
                assert!(message.contains("5 consecutive"),
                    "notify must mention the 5-strike rule, got {:?}", message);
            }
            other => panic!("expected Notify, got {:?}", other),
        }
        assert!(!actions.iter().any(|a| matches!(a, RitualAction::SaveState { .. })),
            "5-strike escalation must NOT emit SaveState (would loop)");
    }

    // ── Cross-branch invariants ─────────────────────────────────────────
    #[test]
    fn test_workspace_unresolved_terminates_to_escalated() {
        // T05 arm: WorkspaceUnresolved is a pre-phase failure — emitted
        // by run_ritual (T08) before any phase action runs. Routes to
        // Escalated with workspace error in error_context.
        let state = idle_state();  // typically Idle when this fires
        let (s, actions) = transition(
            &state,
            RitualEvent::WorkspaceUnresolved {
                error: "project 'engram' not found in registry".into(),
            },
        );
        assert_eq!(s.phase, RitualPhase::Escalated);
        let err_ctx = s.error_context.as_deref().unwrap_or("");
        assert!(err_ctx.starts_with("workspace resolution:"),
            "error_context must namespace workspace failures, got {:?}", err_ctx);
        assert!(err_ctx.contains("project 'engram' not found in registry"),
            "error_context must propagate underlying error, got {:?}", err_ctx);

        assert_eq!(actions.len(), 1, "expected single Notify, got {:?}", actions);
        assert!(matches!(&actions[0], RitualAction::Notify { .. }));
        assert!(!actions.iter().any(|a| matches!(a, RitualAction::SaveState { .. })),
            "WorkspaceUnresolved must NOT emit SaveState (no executable phase reached)");
    }

    #[test]
    fn test_t04_persist_event_variants_are_distinct() {
        // Tripwire: if these variants are merged, renamed, or restructured,
        // this test fails — forcing a deliberate update of the T03/T04/T08
        // call sites that emit them. RitualEvent doesn't derive PartialEq,
        // so we compare via Debug repr.
        let a = format!("{:?}", RitualEvent::StatePersisted {
            attempt: 1,
            kind: SaveStateKind::Boundary,
        });
        let b = format!("{:?}", RitualEvent::StatePersistFailed {
            attempt: 1,
            error: "x".into(),
            kind: SaveStateKind::Boundary,
        });
        let c = format!("{:?}", RitualEvent::WorkspaceUnresolved {
            error: "y".into(),
        });
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
        // Variant tags are present in Debug output — guards against the
        // case where someone reduces all three to a single carrier struct.
        assert!(a.contains("StatePersisted"));
        assert!(b.contains("StatePersistFailed"));
        assert!(c.contains("WorkspaceUnresolved"));
    }
}
