//! Core types for the task execution harness.
//!
//! These types are shared between gid-core (planning) and gid-harness (execution).

use std::collections::HashMap;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

/// A complete execution plan generated from graph topology.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    /// Ordered layers of parallelizable tasks.
    pub layers: Vec<ExecutionLayer>,
    /// The longest dependency chain (node IDs).
    pub critical_path: Vec<String>,
    /// Total number of tasks in the plan.
    pub total_tasks: usize,
    /// Sum of estimated_turns across all tasks.
    pub estimated_total_turns: u32,
}

/// A single layer of tasks that can execute in parallel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionLayer {
    /// Layer index (0-based).
    pub index: usize,
    /// Tasks in this layer.
    pub tasks: Vec<TaskInfo>,
    /// Verification command to run after all layer tasks merge.
    pub checkpoint: Option<String>,
}

/// Information about a single task extracted from graph nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInfo {
    /// Node ID in the graph.
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// Rich, self-contained description for the sub-agent.
    pub description: String,
    /// Goals this task satisfies (from metadata.satisfies → resolved text).
    pub goals: Vec<String>,
    /// Verification command (from metadata.verify).
    pub verify: Option<String>,
    /// Estimated turns for sub-agent (from metadata.estimated_turns, default 15).
    pub estimated_turns: u32,
    /// IDs of tasks this depends on.
    pub depends_on: Vec<String>,
    /// Design doc section reference (from metadata.design_ref).
    pub design_ref: Option<String>,
    /// GOAL IDs this task satisfies (raw, before resolution).
    pub satisfies: Vec<String>,
}

/// Assembled context for a sub-agent executing a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskContext {
    /// Core task information.
    pub task_info: TaskInfo,
    /// Resolved GOAL text from requirements.md.
    pub goals_text: Vec<String>,
    /// Extracted section from design.md (via design_ref).
    pub design_excerpt: Option<String>,
    /// Interface information from completed dependency tasks.
    pub dependency_interfaces: Vec<String>,
    /// Project-level guards (from graph root metadata).
    pub guards: Vec<String>,
}

impl TaskContext {
    /// Render the assembled context into a prompt-friendly string.
    pub fn render_prompt(&self) -> String {
        let mut parts = Vec::new();

        // Task description
        if !self.task_info.description.is_empty() {
            parts.push(format!("## Task: {}\n{}", self.task_info.title, self.task_info.description));
        } else {
            parts.push(format!("## Task: {}", self.task_info.title));
        }

        // Design excerpt
        if let Some(ref excerpt) = self.design_excerpt {
            parts.push(format!("## Design Reference\n{}", excerpt));
        }

        // Goals
        if !self.goals_text.is_empty() {
            let goals = self.goals_text.join("\n");
            parts.push(format!("## Requirements (GOALs to satisfy)\n{}", goals));
        }

        // Guards
        if !self.guards.is_empty() {
            let guards = self.guards.join("\n");
            parts.push(format!("## Guards (invariants to preserve)\n{}", guards));
        }

        // Dependencies
        if !self.dependency_interfaces.is_empty() {
            let deps = self.dependency_interfaces.join("\n");
            parts.push(format!("## Completed Dependencies\n{}", deps));
        }

        parts.join("\n\n")
    }
}

/// Result of a single task execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    /// Whether the task completed successfully.
    pub success: bool,
    /// Sub-agent's final output/message.
    pub output: String,
    /// Number of turns the sub-agent used.
    pub turns_used: u32,
    /// Total tokens consumed.
    pub tokens_used: u64,
    /// If the sub-agent reported a blocker.
    pub blocker: Option<String>,
}

/// Overall result of executing a plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub tasks_completed: usize,
    pub tasks_failed: usize,
    pub total_turns: u32,
    pub total_tokens: u64,
    pub duration_secs: u64,
}

/// Executor type for sub-agent spawning.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutorType {
    /// Automatically select based on environment (API if auth pool exists, CLI otherwise).
    #[default]
    Auto,
    /// Use CLI executor (`claude` command).
    Cli,
    /// Use API executor (requires agentctl auth pool).
    Api,
}

/// Harness configuration with cascading precedence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessConfig {
    /// Approval mode: mixed, manual, auto.
    #[serde(default = "default_approval_mode")]
    pub approval_mode: ApprovalMode,
    /// Maximum concurrent sub-agents per layer.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    /// Maximum retries per failed task.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Maximum re-planning attempts.
    #[serde(default = "default_max_replans")]
    pub max_replans: u32,
    /// Default checkpoint command (auto-detected if None).
    #[serde(default)]
    pub default_checkpoint: Option<String>,
    /// Model for sub-agents.
    #[serde(default = "default_model")]
    pub model: String,
    /// Maximum iterations per sub-agent.
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    /// Invariant checks mapping GUARD IDs to commands.
    #[serde(default)]
    pub invariant_checks: HashMap<String, GuardCheck>,
    /// Executor type for sub-agent spawning.
    #[serde(default)]
    pub executor: ExecutorType,
    /// Custom path to agentctl auth pool (for API executor).
    #[serde(default)]
    pub auth_pool_path: Option<std::path::PathBuf>,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            approval_mode: ApprovalMode::Mixed,
            max_concurrent: 3,
            max_retries: 1,
            max_replans: 3,
            default_checkpoint: None,
            model: "claude-sonnet-4-5".to_string(),
            max_iterations: 80,
            invariant_checks: HashMap::new(),
            executor: ExecutorType::Auto,
            auth_pool_path: None,
        }
    }
}

/// Approval mode for phase gates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalMode {
    /// Phase 1-3 pause, Phase 4-7 auto.
    Mixed,
    /// All phases pause for approval.
    Manual,
    /// Phase 1-3 collaborative (no gate), Phase 4-7 auto.
    Auto,
}

/// A guard check definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardCheck {
    /// Shell command to run.
    pub command: String,
    /// Expected output (exact match after trim).
    pub expect: String,
}

/// Events logged during execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum ExecutionEvent {
    #[serde(rename = "plan")]
    Plan {
        total_tasks: usize,
        layers: usize,
        #[serde(rename = "ts")]
        timestamp: DateTime<Utc>,
    },
    #[serde(rename = "task_start")]
    TaskStart {
        task_id: String,
        layer: usize,
        #[serde(rename = "ts")]
        timestamp: DateTime<Utc>,
    },
    #[serde(rename = "task_done")]
    TaskDone {
        task_id: String,
        turns: u32,
        tokens: u64,
        duration_s: u64,
        verify: String,
        #[serde(rename = "ts")]
        timestamp: DateTime<Utc>,
    },
    #[serde(rename = "task_failed")]
    TaskFailed {
        task_id: String,
        reason: String,
        turns: u32,
        #[serde(rename = "ts")]
        timestamp: DateTime<Utc>,
    },
    #[serde(rename = "checkpoint")]
    Checkpoint {
        layer: usize,
        command: String,
        result: String,
        #[serde(rename = "ts")]
        timestamp: DateTime<Utc>,
    },
    #[serde(rename = "replan")]
    Replan {
        new_tasks: Vec<String>,
        new_edges: Vec<(String, String)>,
        #[serde(rename = "ts")]
        timestamp: DateTime<Utc>,
    },
    #[serde(rename = "cancel")]
    Cancel {
        tasks_done: usize,
        tasks_remaining: usize,
        #[serde(rename = "ts")]
        timestamp: DateTime<Utc>,
    },
    #[serde(rename = "advise")]
    Advise {
        passed: bool,
        score: u8,
        issues: usize,
        #[serde(rename = "ts")]
        timestamp: DateTime<Utc>,
    },
    #[serde(rename = "complete")]
    Complete {
        total_turns: u32,
        total_tokens: u64,
        duration_s: u64,
        failed: usize,
        #[serde(rename = "ts")]
        timestamp: DateTime<Utc>,
    },
}

/// Execution statistics computed from telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionStats {
    pub tasks_completed: usize,
    pub tasks_failed: usize,
    pub total_turns: u32,
    pub avg_turns_per_task: f32,
    pub total_tokens: u64,
    pub duration_secs: u64,
}

/// Verification result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VerifyResult {
    Pass,
    Fail { output: String, exit_code: i32 },
}

/// Information about a git worktree.
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub task_id: String,
    pub path: PathBuf,
    pub branch: String,
}

/// Decision from the re-planner.
#[derive(Debug, Clone)]
pub enum ReplanDecision {
    /// Simple failure, try again.
    Retry,
    /// Structural issue, add missing tasks.
    AddTasks(Vec<NewTask>),
    /// Can't resolve, notify human.
    Escalate(String),
}

/// A new task to add during re-planning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTask {
    pub id: String,
    pub title: String,
    pub description: String,
    pub depends_on: Vec<String>,
    pub metadata: HashMap<String, serde_json::Value>,
}

// ── Default functions for serde ──

fn default_approval_mode() -> ApprovalMode { ApprovalMode::Mixed }
fn default_max_concurrent() -> usize { 3 }
fn default_max_retries() -> u32 { 1 }
fn default_max_replans() -> u32 { 3 }
fn default_model() -> String { "claude-sonnet-4-5".to_string() }
fn default_max_iterations() -> u32 { 80 }
