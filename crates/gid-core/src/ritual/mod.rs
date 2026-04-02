//! Ritual Engine — End-to-end development pipeline orchestration.
//!
//! Rituals are GID's highest-level abstraction: a multi-phase pipeline that orchestrates
//! skills, tools, and the task harness into a complete development workflow.
//!
//! The GID ecosystem has three layers:
//! - Layer 3: Rituals — multi-phase orchestration (this module)
//! - Layer 2: Skills — prompt + tool usage instructions
//! - Layer 1: Tools — MCP servers, CLI commands, Rust crates

pub mod definition;
pub mod engine;
pub mod executor;
pub mod artifact;
pub mod approval;
pub mod template;
pub mod scope;

// Re-export key types
pub use definition::{
    RitualDefinition, PhaseDefinition, PhaseKind, ApprovalRequirement,
    SkipCondition, FailureStrategy, ArtifactRef, ArtifactSpec, PhaseHooks,
    RitualConfig,
};
pub use engine::{RitualEngine, RitualState, RitualStatus, PhaseState, PhaseStatus};
pub use executor::{PhaseExecutor, PhaseResult, PhaseContext};
pub use artifact::ArtifactManager;
pub use approval::{ApprovalGate, ApprovalRequest};
pub use template::{TemplateRegistry, TemplateSummary};
pub use scope::{ToolScope, BashPolicy, ToolNameMapping, default_scope_for_phase, rustclaw_tool_mapping};
