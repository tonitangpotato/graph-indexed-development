//! # gid-harness — Async Execution Engine for GID Task Harness
//!
//! This crate implements the I/O and async execution layer for the GID task
//! execution pipeline. While `gid-core` provides pure planning functions
//! (topology analysis, execution planning, context assembly), `gid-harness`
//! handles the actual execution:
//!
//! - **Scheduler** — drives plan execution, manages task states, enforces dependencies
//! - **Executor** — spawns sub-agents (e.g., Claude CLI) in isolated worktrees
//! - **Worktree Manager** — creates, merges, and cleans up git worktrees
//! - **Verifier** — runs verification commands at task and layer level
//! - **Replanner** — analyzes failures and decides retry/add-tasks/escalate
//! - **Telemetry** — append-only JSONL event logging with crash recovery
//!
//! # Architecture
//!
//! ```text
//! gid-core (pure)          gid-harness (async I/O)
//! ┌─────────────┐          ┌──────────────────────┐
//! │ Topology     │          │ Scheduler            │
//! │ Planner      │──plan──→│   ├─ Executor (trait) │
//! │ Context Asm  │          │   ├─ WorktreeManager  │
//! └─────────────┘          │   ├─ Verifier         │
//!                          │   ├─ Replanner        │
//!                          │   └─ Telemetry        │
//!                          └──────────────────────┘
//! ```

pub mod scheduler;
pub mod executor;
pub mod worktree;
pub mod verifier;
pub mod replanner;
pub mod telemetry;

// Re-export key types from submodules
pub use scheduler::execute_plan;
pub use executor::{TaskExecutor, CliExecutor};
pub use worktree::{WorktreeManager, GitWorktreeManager};
pub use verifier::Verifier;
pub use replanner::Replanner;
pub use telemetry::TelemetryLogger;

// Re-export harness types from gid-core for convenience
pub use gid_core::harness::types::{
    ExecutionPlan, ExecutionLayer, TaskInfo, TaskContext, TaskResult,
    ExecutionResult, HarnessConfig, ApprovalMode, GuardCheck,
    ExecutionEvent, ExecutionStats, VerifyResult, WorktreeInfo,
    ReplanDecision, NewTask,
};
