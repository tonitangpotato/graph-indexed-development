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
    Designing,
    /// Reviewing a document produced by the previous phase (design, graph, requirements).
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
            Self::WaitingClarification => Some(Self::Designing),
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

    /// Whether this is a pause state (waiting for user input, no EP actions expected).
    pub fn is_paused(&self) -> bool {
        matches!(self, Self::WaitingClarification | Self::WaitingApproval)
    }
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
    /// Tokens used per phase (e.g. "design" -> 5432, "implement" -> 28000).
    #[serde(default)]
    pub phase_tokens: HashMap<String, u64>,
    pub transitions: Vec<TransitionRecord>,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectState {
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
            project: None,
            strategy: None,
            verify_retries: 0,
            phase_retries: HashMap::new(),
            failed_phase: None,
            error_context: None,
            review_target: None,
            phase_tokens: HashMap::new(),
            transitions: Vec::new(),
            started_at: now,
            updated_at: now,
        }
    }

    pub fn with_phase(mut self, phase: RitualPhase) -> Self {
        self.transitions.push(TransitionRecord {
            from: self.phase.clone(),
            to: phase.clone(),
            event: format!("{:?} → {:?}", self.phase, phase),
            timestamp: Utc::now(),
        });
        self.phase = phase;
        self.updated_at = Utc::now();
        self
    }

    pub fn with_task(mut self, task: String) -> Self {
        self.task = task;
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
    SkillFailed { phase: String, error: String },
    ShellCompleted { stdout: String, exit_code: i32 },
    ShellFailed { stderr: String, exit_code: i32 },
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
    SaveState,
    /// Cleanup temporary files.
    Cleanup,
    /// Apply approved review findings (fire-and-forget, runs apply-review skill).
    ApplyReview { approved: String },
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
                SaveState,
                DetectProject,
            ],
        ),

        // Project detected → Triage
        (Initializing, ProjectDetected(ps)) => (
            state.clone().with_phase(Triaging).with_project(ps),
            vec![
                Notify { message: "🔍 Triaging task...".into() },
                SaveState,
                RunTriage { task: state.task.clone() },
            ],
        ),

        // Triage complete (clear) → skip or proceed based on result
        (Triaging, TriageCompleted(result)) if result.clarity == "clear" => {
            let skip_design = result.skip_design;
            let skip_graph = result.skip_graph;

            // Store triage result for later reference
            let mut new_state = state.clone();
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
                        SaveState,
                        RunPlanning,
                    ],
                )
            } else if skip_design {
                // Medium task: skip design, do graph
                (
                    new_state.with_phase(Planning),
                    vec![
                        Notify { message: format!("📋 Medium task ({}). Skipping design.", result.size) },
                        SaveState,
                        RunPlanning,
                    ],
                )
            } else {
                // Large task: full flow
                let skill = if state.project.as_ref().map_or(false, |p| p.has_design) {
                    "update-design"
                } else {
                    "draft-design"
                };
                (
                    new_state.with_phase(Designing),
                    vec![
                        Notify { message: format!("📝 Phase 1/4: {}...", skill) },
                        SaveState,
                        RunSkill { name: skill.into(), context: state.task.clone() },
                    ],
                )
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
                    SaveState,
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
                    SaveState,
                    RunTriage { task: enriched_task },
                ],
            )
        }

        // UserRetry from WaitingClarification → re-triage
        (WaitingClarification, UserRetry) => (
            state.clone().with_phase(Triaging),
            vec![
                Notify { message: "🔍 Re-triaging...".into() },
                SaveState,
                RunTriage { task: state.task.clone() },
            ],
        ),

        // Design done → Review design document
        (Designing, SkillCompleted { .. }) => (
            state.clone().with_phase(Reviewing).with_review_target("design"),
            vec![
                Notify { message: "📝 Reviewing design document...".into() },
                SaveState,
                RunSkill { name: "review-design".into(), context: state.task.clone() },
            ],
        ),

        // Planning decided → Graphing
        (Planning, PlanDecided(strategy)) => {
            let skill = if state.project.as_ref().map_or(false, |p| p.has_graph) {
                "update-graph"
            } else {
                "generate-graph"
            };
            (
                state.clone().with_phase(Graphing).with_strategy(strategy),
                vec![
                    Notify { message: format!("📊 Phase 2/4: {}...", skill) },
                    SaveState,
                    RunSkill { name: skill.into(), context: state.task.clone() },
                ],
            )
        }

        // Graph done → Review tasks
        (Graphing, SkillCompleted { .. }) => (
            state.clone().with_phase(Reviewing).with_review_target("tasks"),
            vec![
                Notify { message: "📝 Reviewing task breakdown...".into() },
                SaveState,
                RunSkill { name: "review-tasks".into(), context: state.task.clone() },
            ],
        ),

        // ─── Review cycle transitions ─────────────────────────────────────

        // Review skill completed → WaitingApproval (pause for human)
        (Reviewing, SkillCompleted { .. }) => (
            state.clone().with_phase(WaitingApproval),
            vec![
                Notify { message: "📋 Review complete. Check `.gid/reviews/` for findings.\nWhich findings should I apply? (e.g., 'apply FINDING-1,3,5' or 'apply all' or 'skip')".into() },
                SaveState,
            ],
        ),

        // User approves findings → apply changes (fire-and-forget), then continue to next phase
        (WaitingApproval, UserApproval { approved }) => {
            let review_target = state.review_target.clone().unwrap_or_default();
            match review_target.as_str() {
                "design" => (
                    state.clone().with_phase(Planning),
                    vec![
                        ApplyReview { approved },
                        Notify { message: "🧠 Planning implementation strategy...".into() },
                        SaveState,
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
                            ApplyReview { approved },
                            Notify { message: "💻 Implementing...".into() },
                            SaveState,
                            action,
                        ],
                    )
                }
                _ => (
                    state.clone().with_phase(Planning),
                    vec![
                        ApplyReview { approved },
                        SaveState,
                        RunPlanning,
                    ],
                ),
            }
        }

        // User skips review → continue to next phase without applying
        (WaitingApproval, UserSkipPhase) => {
            let review_target = state.review_target.clone().unwrap_or_default();
            match review_target.as_str() {
                "design" => (
                    state.clone().with_phase(Planning),
                    vec![
                        Notify { message: "⏭️ Skipping review, moving to planning...".into() },
                        SaveState,
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
                            Notify { message: "⏭️ Skipping review, moving to implementation...".into() },
                            SaveState,
                            action,
                        ],
                    )
                }
                _ => (
                    state.clone().with_phase(Planning),
                    vec![
                        Notify { message: "⏭️ Skipping review...".into() },
                        SaveState,
                        RunPlanning,
                    ],
                ),
            }
        }

        // Review failed → log and continue (don't block the ritual for review failure)
        (Reviewing, SkillFailed { error, .. }) => {
            let review_target = state.review_target.clone().unwrap_or_default();
            let next = match review_target.as_str() {
                "design" => (
                    state.clone().with_phase(Planning),
                    vec![
                        Notify { message: format!("⚠️ Review failed ({}), continuing to planning...", error) },
                        SaveState,
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
                            SaveState,
                            action,
                        ],
                    )
                }
                _ => (
                    state.clone().with_phase(Planning),
                    vec![
                        Notify { message: format!("⚠️ Review failed ({}), continuing...", error) },
                        SaveState,
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
                    SaveState,
                    RunShell { command: cmd },
                ],
            )
        }

        // Verify success → Done
        (Verifying, ShellCompleted { exit_code, .. }) if exit_code == 0 => (
            state.clone().with_phase(Done),
            vec![
                Notify { message: "🎉 Ritual complete!".into() },
                UpdateGraph { description: state.task.clone() },
                SaveState,
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
                SaveState,
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
                SaveState,
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
                SaveState,
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
                SaveState,
            ],
        ),

        // Design failed → retry once
        (Designing, SkillFailed { error, .. }) if state.retries_for("designing") < 1 => (
            state.clone().with_phase(Designing).inc_phase_retry("designing"),
            vec![
                Notify { message: format!("🔄 Design failed, retrying... ({})", truncate(&error, 100)) },
                SaveState,
                RunSkill {
                    name: if state.project.as_ref().map_or(false, |p| p.has_design) {
                        "update-design"
                    } else {
                        "draft-design"
                    }.into(),
                    context: format!("RETRY — previous error: {}\n\nOriginal task: {}", error, state.task),
                },
            ],
        ),

        // Graphing failed → retry once
        (Graphing, SkillFailed { error, .. }) if state.retries_for("graphing") < 1 => (
            state.clone().with_phase(Graphing).inc_phase_retry("graphing"),
            vec![
                Notify { message: format!("🔄 Graph generation failed, retrying... ({})", truncate(&error, 100)) },
                SaveState,
                RunSkill {
                    name: if state.project.as_ref().map_or(false, |p| p.has_graph) {
                        "update-graph"
                    } else {
                        "generate-graph"
                    }.into(),
                    context: format!("RETRY — previous error: {}\n\nOriginal task: {}", error, state.task),
                },
            ],
        ),

        // Planning failed → retry once
        (Planning, SkillFailed { error, .. }) if state.retries_for("planning") < 1 => (
            state.clone().with_phase(Planning).inc_phase_retry("planning"),
            vec![
                Notify { message: format!("🔄 Planning failed, retrying... ({})", truncate(&error, 100)) },
                SaveState,
                RunPlanning,
            ],
        ),

        // Implementing failed → retry once
        (Implementing, SkillFailed { error, .. }) if state.retries_for("implementing") < 1 => (
            state.clone().with_phase(Implementing).inc_phase_retry("implementing"),
            vec![
                Notify { message: format!("🔄 Implementation failed, retrying... ({})", truncate(&error, 100)) },
                SaveState,
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
                SaveState,
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
                SaveState,
            ],
        ),

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
                        SaveState,
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
                    SaveState,
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
                                        SaveState,
                                        DetectProject,
                                    ],
                                );
                            }
                            // Run triage (can't skip to Designing without project detection)
                            RunTriage { task: state.task.clone() }
                        }
                        Designing => {
                            let skill = if state.project.as_ref().map_or(false, |p| p.has_design) {
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
                                    SaveState,
                                ],
                            );
                        }
                        // Skip review → go to the phase after review
                        Reviewing | WaitingApproval => {
                            // Determine where to go based on current phase
                            return match phase {
                                Designing => (
                                    state.clone().with_phase(Planning),
                                    vec![
                                        Notify { message: "⏭️ Skipping review, moving to planning...".into() },
                                        SaveState,
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
                                            SaveState,
                                            action,
                                        ],
                                    )
                                }
                                _ => (
                                    state.clone().with_phase(Planning),
                                    vec![
                                        Notify { message: "⏭️ Skipping review...".into() },
                                        SaveState,
                                        RunPlanning,
                                    ],
                                ),
                            };
                        }
                        Planning => RunPlanning,
                        Graphing => {
                            let skill = if state.project.as_ref().map_or(false, |p| p.has_graph) {
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
                                    SaveState,
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
                                    SaveState,
                                ],
                            );
                        }
                    };
                    (
                        state.clone().with_phase(next_phase.clone()),
                        vec![
                            Notify { message: format!("⏭️ Skipped {}. Moving to {}...", phase.display_name(), next_phase.display_name()) },
                            SaveState,
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
                        SaveState,
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
                SaveState,
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

        // Review → WaitingApproval
        let (s2, a2) = transition(&s, RitualEvent::SkillCompleted { phase: "review-design".into(), artifacts: vec![] });
        assert_eq!(s2.phase, RitualPhase::WaitingApproval);
        assert_invariant(&s2, &a2);

        // Approval → Planning
        let (s3, a3) = transition(&s2, RitualEvent::UserApproval { approved: "all".into() });
        assert_eq!(s3.phase, RitualPhase::Planning);
        assert_invariant(&s3, &a3);
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

        // Review → WaitingApproval → Approval → Implementing
        let (s2, _) = transition(&s, RitualEvent::SkillCompleted { phase: "review-tasks".into(), artifacts: vec![] });
        assert_eq!(s2.phase, RitualPhase::WaitingApproval);
        let (s3, a3) = transition(&s2, RitualEvent::UserApproval { approved: "all".into() });
        assert_eq!(s3.phase, RitualPhase::Implementing);
        assert_invariant(&s3, &a3);
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
        let (s, a) = transition(&state, RitualEvent::SkillFailed { phase: "design".into(), error: "oops".into() });
        assert_eq!(s.phase, RitualPhase::Designing);
        assert_eq!(s.retries_for("designing"), 1);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_design_fail_escalate_after_retry() {
        let mut state = idle_state()
            .with_phase(RitualPhase::Designing)
            .with_task("test".into());
        state.phase_retries.insert("designing".into(), 1);
        let (s, a) = transition(&state, RitualEvent::SkillFailed { phase: "design".into(), error: "oops".into() });
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
        let (s, a) = transition(&state, RitualEvent::SkillFailed { phase: "planning".into(), error: "oops".into() });
        assert_eq!(s.phase, RitualPhase::Planning);
        assert_eq!(s.retries_for("planning"), 1);
        assert_invariant(&s, &a);
    }

    #[test]
    fn test_planning_escalate_after_retry() {
        let mut state = idle_state()
            .with_phase(RitualPhase::Planning)
            .with_task("test".into());
        state.phase_retries.insert("planning".into(), 1);
        let (s, a) = transition(&state, RitualEvent::SkillFailed { phase: "planning".into(), error: "oops".into() });
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

        // Review → WaitingApproval → Approve → Implementing with Harness
        let (s2, _) = transition(&s, RitualEvent::SkillCompleted { phase: "review-tasks".into(), artifacts: vec![] });
        let (s3, a3) = transition(&s2, RitualEvent::UserApproval { approved: "all".into() });
        assert_eq!(s3.phase, RitualPhase::Implementing);
        let has_harness = a3.iter().any(|a| matches!(a, RitualAction::RunHarness { .. }));
        assert!(has_harness);
        assert_invariant(&s3, &a3);
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
        assert_eq!(s.phase, RitualPhase::Designing);
        let has_draft = a.iter().any(|a| matches!(a, RitualAction::RunSkill { name, .. } if name == "draft-design"));
        assert!(has_draft);
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
        // Idle → Start → Init → ProjectDetected → Triage → TriageCompleted(large)
        // → Designing → SkillCompleted → Reviewing(design) → SkillCompleted → WaitingApproval
        // → UserApproval → Planning → PlanDecided → Graphing → SkillCompleted
        // → Reviewing(tasks) → SkillCompleted → WaitingApproval → UserApproval
        // → Implementing → SkillCompleted → Verifying → ShellCompleted(0) → Done
        let mut state = idle_state();

        let (s, a) = transition(&state, RitualEvent::Start { task: "add X".into() });
        assert_eq!(s.phase, RitualPhase::Initializing);
        assert_invariant(&s, &a);
        state = s;

        let (s, a) = transition(&state, RitualEvent::ProjectDetected(project_greenfield()));
        assert_eq!(s.phase, RitualPhase::Triaging);
        assert_invariant(&s, &a);
        state = s;

        // Triage says large task, full flow
        let (s, a) = transition(&state, RitualEvent::TriageCompleted(TriageResult {
            clarity: "clear".into(),
            clarify_questions: vec![],
            size: "large".into(),
            skip_design: false,
            skip_graph: false,
        }));
        assert_eq!(s.phase, RitualPhase::Designing);
        assert_invariant(&s, &a);
        state = s;

        // Design complete → Review design
        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "draft-design".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::Reviewing);
        assert_invariant(&s, &a);
        state = s;

        // Review complete → WaitingApproval
        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "review-design".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::WaitingApproval);
        assert_invariant(&s, &a);
        state = s;

        // User approves → Planning
        let (s, a) = transition(&state, RitualEvent::UserApproval { approved: "all".into() });
        assert_eq!(s.phase, RitualPhase::Planning);
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

        // Review → WaitingApproval → Approve → Implementing
        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "review-tasks".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::WaitingApproval);
        assert_invariant(&s, &a);
        state = s;

        let (s, a) = transition(&state, RitualEvent::UserApproval { approved: "all".into() });
        assert_eq!(s.phase, RitualPhase::Implementing);
        assert_invariant(&s, &a);
        state = s;

        let (s, a) = transition(&state, RitualEvent::SkillCompleted { phase: "implement".into(), artifacts: vec![] });
        assert_eq!(s.phase, RitualPhase::Verifying);
        assert_invariant(&s, &a);
        state = s;

        let (s, a) = transition(&state, RitualEvent::ShellCompleted { stdout: "all tests passed".into(), exit_code: 0 });
        assert_eq!(s.phase, RitualPhase::Done);
        assert_invariant(&s, &a);

        // Transitions: Init→Triage→Design→Review→WaitApproval→Planning→Graphing→Review→WaitApproval→Implement→Verify→Done = 12
        assert_eq!(s.transitions.len(), 12);
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
}
