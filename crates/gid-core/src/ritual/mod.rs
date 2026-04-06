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
pub mod llm;
pub mod notifier;
#[cfg(feature = "harness")]
pub mod api_llm_client;
pub mod composer;
pub mod state_machine;
pub mod v2_executor;
pub mod gating;

// Re-export key types
pub use definition::{
    RitualDefinition, PhaseDefinition, PhaseKind, ApprovalRequirement,
    SkipCondition, FailureStrategy, ArtifactRef, ArtifactSpec, PhaseHooks,
    RitualConfig,
};
pub use engine::{RitualEngine, RitualState, RitualStatus, PhaseState, PhaseStatus};
pub use executor::{
    PhaseExecutor, PhaseResult, PhaseContext,
    SkillExecutor, GidCommandExecutor, HarnessExecutor, ShellExecutor,
    LlmTaskExecutor,
};
pub use artifact::ArtifactManager;
pub use approval::{ApprovalGate, ApprovalRequest};
pub use template::{TemplateRegistry, TemplateSummary};
pub use scope::{ToolScope, BashPolicy, ToolNameMapping, ScopeCategory, default_scope_for_phase, rustclaw_tool_mapping};
pub use llm::{LlmClient, ToolDefinition, SkillResult};
#[cfg(feature = "harness")]
pub use api_llm_client::ApiLlmClient;
pub use notifier::{RitualNotifier, RitualNotifyConfig, RitualEvent};
pub use composer::{compose_ritual, ProjectState as ComposerProjectState, ProjectLanguage};
pub use v2_executor::{V2Executor, V2ExecutorConfig, NotifyFn, run_ritual, build_triage_prompt};
pub use gating::{GatingConfig, GatingResult, CommandPattern, PatternType, check_gating, load_gating_config, save_gating_config};
pub use state_machine::{
    RitualPhase as V2Phase,
    RitualState as V2State,
    RitualEvent as V2Event,
    RitualAction as V2Action,
    ProjectState as V2ProjectState,
    ImplementStrategy,
    TriageResult,
    generate_ritual_id,
    transition,
    truncate,
};
