pub mod graph;
pub mod query;
pub mod parser;
pub mod validator;
pub mod code_graph;
pub mod unified;
pub mod working_mem;
pub mod history;
pub mod visual;
pub mod advise;
pub mod design;
pub mod semantify;
pub mod refactor;
pub mod ignore;
pub mod task_graph_knowledge;
pub mod complexity;
pub mod lsp_client;
pub mod harness;

// Ritual module (requires "ritual" feature)
#[cfg(feature = "ritual")]
pub mod ritual;

// Re-export commonly used types
pub use graph::*;
pub use query::QueryEngine;
pub use parser::{load_graph, save_graph};
pub use code_graph::{
    CodeGraph, CodeNode, CodeEdge, NodeKind, EdgeRelation, Language,
    ImpactReport, CausalChain, ChainNode,
    UnifiedGraphResult, UnifiedNode, UnifiedEdge,
};
pub use unified::{build_unified_graph, merge_relevant_code, link_tasks_to_code, UnifiedStats};
pub use working_mem::{
    GidContext, NodeInfo, ErrorType, RiskLevel, ImpactAnalysis,
    query_gid_context, find_low_risk_alternatives, classify_error,
    extract_key_traceback, analyze_impact, format_impact_for_llm,
    // Agent working memory types
    Action, TestOutcome, AttemptRecord, NodeRisk, WorkingMemory,
};
pub use history::{HistoryManager, HistoryEntry, GraphDiff};
pub use visual::{render, render_ascii, render_dot, render_mermaid, VisualFormat};
pub use advise::{analyze, AnalysisResult, Advice, Severity, AdviceType};
pub use design::{
    generate_graph_prompt, generate_features_prompt, generate_components_prompt,
    parse_llm_response, parse_features_response, parse_components_response,
    build_graph_from_proposals, FeatureProposal, ComponentProposal, DesignResult,
};
pub use semantify::{
    generate_semantify_prompt, generate_full_transform_prompt,
    parse_semantify_response, parse_full_transform_response,
    apply_proposals, apply_heuristic_layers, heuristic_assign_layer,
    SemanticProposal, SemantifyResult,
};
pub use refactor::{
    preview_rename, apply_rename,
    preview_merge, apply_merge,
    preview_split, apply_split,
    preview_extract, apply_extract,
    update_title, move_to_layer,
    RefactorPreview, Change, ChangeType, SplitDefinition,
};
pub use ignore::{
    load_ignore_list, IgnoreList, IgnorePattern, is_common_ignore, DEFAULT_IGNORES,
};
pub use task_graph_knowledge::{
    ToolCallRecord, KnowledgeNode, KnowledgeGraph, KnowledgeManagement,
    SimpleKnowledgeGraph,
};
pub use complexity::{
    Complexity, ComplexityReport, assess_complexity_from_graph, assess_complexity,
    is_high_risk_change, assess_risk_level,
};
pub use lsp_client::{
    LspClient, LspEnrichmentStats, LspLocation, LspRefinementStats, LspServerConfig,
};

// Ritual re-exports (requires "ritual" feature)
#[cfg(feature = "ritual")]
pub use ritual::{
    RitualDefinition, PhaseDefinition, PhaseKind, ApprovalRequirement,
    SkipCondition, FailureStrategy, ArtifactRef, ArtifactSpec, PhaseHooks,
    RitualConfig, RitualEngine, RitualState, RitualStatus, PhaseState,
    PhaseStatus, PhaseExecutor, PhaseResult, PhaseContext, ArtifactManager,
    ApprovalGate, ApprovalRequest, TemplateRegistry, TemplateSummary,
};
