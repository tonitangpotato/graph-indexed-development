//! Task execution harness — planning, topology analysis, context assembly, and execution.
//!
//! This module provides both the pure, deterministic planning functions and the
//! async execution engine for the GID task execution harness.
//!
//! ## Feature Flags
//!
//! - **Base** (no feature): Just types, topology, planner, context, config (pure functions)
//! - **`harness`**: Adds async execution engine (scheduler, executor, worktree, verifier, replanner, telemetry)
//! - **`ritual`**: Implies harness (reserved for future ritual/ceremony features)
//! - **`full`**: Enables all features

pub mod types;
pub mod topology;
pub mod planner;
pub mod context;
pub mod config;

// Re-export key types (always available)
pub use types::{
    ExecutionPlan, ExecutionLayer, TaskInfo, TaskContext, TaskResult,
    ExecutionResult, HarnessConfig, ApprovalMode, ExecutorType, GuardCheck,
    ExecutionEvent, ExecutionStats, VerifyResult, WorktreeInfo,
    ReplanDecision, NewTask,
};
pub use topology::{detect_cycles, compute_layers, critical_path, orphan_tasks};
pub use planner::create_plan;
pub use context::assemble_task_context;
pub use context::{
    ContextQuery, ContextFilters, OutputFormat, ContextStats,
    AssembledContext, assemble_context, format_context,
};
pub use config::load_config;

// ═══════════════════════════════════════════════════════════════════════════════
// Async Execution Engine (requires "harness" feature)
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(feature = "harness")]
pub mod scheduler;
#[cfg(feature = "harness")]
pub mod executor;
#[cfg(feature = "harness")]
pub mod worktree;
#[cfg(feature = "harness")]
pub mod verifier;
#[cfg(feature = "harness")]
pub mod replanner;
#[cfg(feature = "harness")]
pub mod telemetry;
#[cfg(feature = "harness")]
pub mod execution_state;
#[cfg(feature = "harness")]
pub mod log_reader;
#[cfg(feature = "harness")]
pub mod notifier;

// Re-export execution engine types (harness feature)
#[cfg(feature = "harness")]
pub use scheduler::execute_plan;
#[cfg(feature = "harness")]
pub use executor::{TaskExecutor, CliExecutor, ApiExecutor, create_executor};
#[cfg(feature = "harness")]
pub use worktree::{WorktreeManager, GitWorktreeManager};
#[cfg(feature = "harness")]
pub use verifier::{Verifier, GuardResult};
#[cfg(feature = "harness")]
pub use replanner::{Replanner, ReplanAction, ActionType, LlmNewTask};
#[cfg(feature = "harness")]
pub use telemetry::TelemetryLogger;
#[cfg(feature = "harness")]
pub use execution_state::{ExecutionState, ExecutionStatus, ApprovalRequest};
#[cfg(feature = "harness")]
pub use log_reader::{
    ExecutionLogReader, ExecutionSummary, TaskSummary, TaskStatus,
    ApprovalPhase, ApprovalContext, generate_approval_message,
    EXECUTION_LOG_FILENAME,
};
#[cfg(feature = "harness")]
pub use notifier::{
    TelegramNotifier, NotifierConfig, InlineKeyboard, InlineButton,
    ApprovalStatus, escape_html,
};
