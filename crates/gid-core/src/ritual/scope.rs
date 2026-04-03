//! ToolScope — Per-phase capability boundaries for agent enforcement.
//!
//! The key insight: instead of telling an LLM "don't use tool X" (prompt-level, unreliable),
//! we remove tool X from the tools array entirely (environment-level, enforced).
//!
//! ToolScope defines what an agent CAN do in a given ritual phase.
//! The agent runtime uses this to filter the tools array before each API call.
//!
//! Two layers of enforcement:
//! 1. Tool visibility — LLM doesn't know a tool exists (tools array filtered)
//! 2. Path constraints — even visible tools are restricted to specific paths

use serde::{Deserialize, Serialize};
use tracing::warn;

use super::state_machine::RitualPhase;

impl RitualPhase {
    /// Map ritual phase to scope category for ToolScope enforcement.
    pub fn scope_category(&self) -> Option<ScopeCategory> {
        match self {
            Self::Designing => Some(ScopeCategory::Design),
            Self::Planning => Some(ScopeCategory::Plan),
            Self::Graphing => Some(ScopeCategory::Design), // graph is doc writing
            Self::Implementing => Some(ScopeCategory::Implement),
            Self::Verifying => Some(ScopeCategory::Verify),
            _ => None, // Idle, Initializing, Done, Escalated, Cancelled — no scope
        }
    }
}

/// Policy for bash/shell command execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BashPolicy {
    /// No shell access.
    Deny,
    /// Unrestricted shell access.
    AllowAll,
    /// Only specific commands allowed (prefix match).
    AllowList(Vec<String>),
}

impl Default for BashPolicy {
    fn default() -> Self {
        BashPolicy::Deny
    }
}

/// Capability boundary for a single ritual phase.
///
/// Defines what tools are visible and what paths are accessible.
/// The agent runtime filters its tools array based on this scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolScope {
    /// Tool names the agent can see (e.g., "Read", "Write", "Edit", "WebSearch").
    /// Tools not in this list are invisible to the LLM.
    pub allowed_tools: Vec<String>,

    /// Glob patterns for paths the agent can write to.
    /// Write/Edit tool calls to paths outside these globs are rejected.
    /// Empty = no write access.
    #[serde(default)]
    pub writable_paths: Vec<String>,

    /// Glob patterns for paths the agent can read.
    /// Empty = read everything (default, since reading is usually safe).
    #[serde(default)]
    pub readable_paths: Vec<String>,

    /// Shell/bash execution policy.
    #[serde(default)]
    pub bash_policy: BashPolicy,
}

impl ToolScope {
    /// Full access — all tools, all paths, unrestricted bash.
    /// Used for phases that need maximum capability (e.g., coding).
    pub fn full() -> Self {
        Self {
            allowed_tools: vec![
                "Read".into(), "Write".into(), "Edit".into(), "Bash".into(),
                "WebSearch".into(), "WebFetch".into(),
            ],
            writable_paths: vec!["**".into()],
            readable_paths: vec![],
            bash_policy: BashPolicy::AllowAll,
        }
    }

    /// Research access — read, write, edit docs, plus web search.
    /// Used for research phases.
    pub fn research() -> Self {
        Self {
            allowed_tools: vec![
                "Read".into(), "Write".into(), "Edit".into(),
                "WebSearch".into(), "WebFetch".into(),
            ],
            writable_paths: vec![
                ".gid/features/**".into(),
                "docs/RESEARCH-*".into(),
            ],
            readable_paths: vec![],
            bash_policy: BashPolicy::Deny,
        }
    }

    /// Documentation writing — can write to .gid/ and docs/ only.
    /// Used for requirements and design phases.
    pub fn documentation() -> Self {
        Self {
            allowed_tools: vec!["Read".into(), "Write".into(), "Edit".into()],
            writable_paths: vec![
                ".gid/features/**".into(),
                ".gid/graph.yml".into(),
                "docs/**".into(),
            ],
            readable_paths: vec![],
            bash_policy: BashPolicy::Deny,
        }
    }

    /// Verification only — can read everything, run tests, but not modify source.
    /// Forces failed tests to trigger a state transition back to coding.
    pub fn verify() -> Self {
        Self {
            allowed_tools: vec!["Read".into(), "Bash".into()],
            writable_paths: vec![
                ".gid/**".into(),
            ],
            readable_paths: vec![],
            bash_policy: BashPolicy::AllowList(vec![
                "cargo test".into(),
                "cargo check".into(),
                "cargo clippy".into(),
                "npm test".into(),
                "pytest".into(),
                "go test".into(),
            ]),
        }
    }

    /// Graph operations — can modify .gid/ only.
    pub fn graph_ops() -> Self {
        Self {
            allowed_tools: vec!["Read".into(), "Write".into(), "Bash".into()],
            writable_paths: vec![".gid/**".into()],
            readable_paths: vec![],
            bash_policy: BashPolicy::AllowList(vec!["gid ".into()]),
        }
    }

    /// Check if a tool name is allowed in this scope.
    pub fn is_tool_allowed(&self, tool_name: &str) -> bool {
        self.allowed_tools.iter().any(|t| t.eq_ignore_ascii_case(tool_name))
    }

    /// Check if a path is writable in this scope.
    /// Uses simple glob matching (supports * and **).
    pub fn is_path_writable(&self, path: &str) -> bool {
        if self.writable_paths.is_empty() {
            return false;
        }
        self.writable_paths.iter().any(|pattern| glob_match(pattern, path))
    }

    /// Check if a path is readable in this scope.
    /// Empty readable_paths means everything is readable.
    pub fn is_path_readable(&self, path: &str) -> bool {
        if self.readable_paths.is_empty() {
            return true; // No restriction = read everything
        }
        self.readable_paths.iter().any(|pattern| glob_match(pattern, path))
    }

    /// Check if a bash command is allowed.
    pub fn is_bash_allowed(&self, command: &str) -> bool {
        match &self.bash_policy {
            BashPolicy::Deny => false,
            BashPolicy::AllowAll => true,
            BashPolicy::AllowList(prefixes) => {
                let trimmed = command.trim();
                prefixes.iter().any(|prefix| trimmed.starts_with(prefix))
            }
        }
    }
}

/// Scope categories — abstract phase types that map to ToolScope.
/// Used by RitualPhase::scope_category() in the state machine (§2).
/// This decouples scope definitions from phase ID strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeCategory {
    /// Design phase: Read + Write docs only. No bash, no source code.
    Design,
    /// Planning phase: Read only. No writes, no bash.
    Plan,
    /// Implementation phase: Full access — Read, Write, Edit, Bash.
    Implement,
    /// Verification phase: Read + Bash (test/build commands only). No writes.
    Verify,
}

impl ScopeCategory {
    /// Convert a ScopeCategory to the corresponding ToolScope.
    pub fn to_scope(self) -> ToolScope {
        match self {
            ScopeCategory::Design => ToolScope::documentation(),
            ScopeCategory::Plan => ToolScope {
                allowed_tools: vec!["Read".into()],
                writable_paths: vec![],
                readable_paths: vec![],
                bash_policy: BashPolicy::Deny,
            },
            ScopeCategory::Implement => ToolScope::full(),
            ScopeCategory::Verify => ToolScope::verify(),
        }
    }
}

/// Get the default ToolScope for a ritual phase by its ID.
///
/// This maps well-known phase IDs to predefined scopes.
/// Custom phases get full access by default (can be overridden in ritual.yml).
///
/// Tool names are generic (Read, Write, Edit, Bash, WebSearch, WebFetch).
/// Agent runtimes should use `ToolScope::with_tool_mapping()` to translate
/// to their actual tool names (e.g., "Read" → "read_file").
///
/// For the v2 state machine, prefer `ScopeCategory::to_scope()` instead.
pub fn default_scope_for_phase(phase_id: &str) -> ToolScope {
    match phase_id {
        "capture-idea" => ToolScope {
            allowed_tools: vec!["Read".into(), "Write".into(), "WebSearch".into(), "WebFetch".into()],
            writable_paths: vec![".gid/features/**".into(), "docs/**".into()],
            readable_paths: vec![],
            bash_policy: BashPolicy::Deny,
        },
        "research" => ToolScope::research(),
        "draft-requirements" | "draft-design" | "update-design" => ScopeCategory::Design.to_scope(),
        "generate-graph" | "update-graph" | "design-to-graph" => ToolScope::graph_ops(),
        "plan-tasks" => ScopeCategory::Plan.to_scope(),
        "implement" | "execute-tasks" => ScopeCategory::Implement.to_scope(),
        "extract-code" => ToolScope::graph_ops(),
        "verify-quality" | "verify" => ScopeCategory::Verify.to_scope(),
        _ => {
            warn!("No ToolScope defined for phase '{}', using full access", phase_id);
            ToolScope::full()
        }
    }
}

/// Standard tool name mapping from generic names to runtime-specific names.
/// Each tuple is (generic_name, runtime_name).
pub type ToolNameMapping = Vec<(String, String)>;

/// Create a standard mapping for RustClaw tool names.
pub fn rustclaw_tool_mapping() -> ToolNameMapping {
    vec![
        ("Read".into(), "read_file".into()),
        ("Write".into(), "write_file".into()),
        ("Edit".into(), "edit_file".into()),
        ("Bash".into(), "exec".into()),
        ("WebSearch".into(), "web_search".into()),
        ("WebFetch".into(), "web_fetch".into()),
    ]
}

impl ToolScope {
    /// Translate generic tool names to runtime-specific names using a mapping.
    ///
    /// Any generic name not in the mapping is kept as-is (passthrough).
    /// This allows GID tools (gid_tasks, gid_read, etc.) to pass through
    /// without explicit mapping.
    pub fn with_tool_mapping(mut self, mapping: &ToolNameMapping) -> Self {
        self.allowed_tools = self.allowed_tools.iter().map(|generic| {
            mapping.iter()
                .find(|(g, _)| g == generic)
                .map(|(_, runtime)| runtime.clone())
                .unwrap_or_else(|| generic.clone())
        }).collect();
        self
    }

    /// Filter a list of tool definitions, keeping only those allowed by this scope.
    ///
    /// Tool names are matched case-insensitively.
    /// GID tools (names starting with "gid_") are always allowed — they're
    /// needed for ritual management regardless of phase.
    pub fn filter_tools<T, F>(&self, tools: Vec<T>, name_fn: F) -> Vec<T>
    where
        F: Fn(&T) -> &str,
    {
        tools.into_iter().filter(|tool| {
            let name = name_fn(tool);
            // Always allow GID tools — needed for ritual management
            if name.starts_with("gid_") {
                return true;
            }
            // Always allow engram tools — memory is always needed
            if name.starts_with("engram_") {
                return true;
            }
            // Always allow set_voice_mode, tts, stt — communication tools
            if matches!(name, "set_voice_mode" | "tts" | "stt") {
                return true;
            }
            // Check against allowed list
            self.is_tool_allowed(name)
        }).collect()
    }
}

/// Simple glob matching supporting * (one segment) and ** (any depth).
fn glob_match(pattern: &str, path: &str) -> bool {
    // Handle exact match
    if pattern == path {
        return true;
    }

    // Handle ** (match everything)
    if pattern == "**" {
        return true;
    }

    // Split into segments
    let pat_parts: Vec<&str> = pattern.split('/').collect();
    let path_parts: Vec<&str> = path.split('/').collect();

    glob_match_parts(&pat_parts, &path_parts)
}

fn glob_match_parts(pattern: &[&str], path: &[&str]) -> bool {
    if pattern.is_empty() && path.is_empty() {
        return true;
    }
    if pattern.is_empty() {
        return false;
    }

    let pat = pattern[0];

    if pat == "**" {
        // ** matches zero or more path segments
        if pattern.len() == 1 {
            return true; // trailing ** matches everything
        }
        // Try matching ** against 0, 1, 2, ... segments
        for i in 0..=path.len() {
            if glob_match_parts(&pattern[1..], &path[i..]) {
                return true;
            }
        }
        return false;
    }

    if path.is_empty() {
        return false;
    }

    // Check if current segment matches
    if segment_match(pat, path[0]) {
        glob_match_parts(&pattern[1..], &path[1..])
    } else {
        false
    }
}

/// Match a single path segment against a pattern segment.
/// Supports * as wildcard within a segment.
fn segment_match(pattern: &str, segment: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    // Handle patterns like "RESEARCH-*" (trailing star)
    if let Some(prefix) = pattern.strip_suffix('*') {
        return segment.starts_with(prefix);
    }

    // Handle patterns like "*.rs" (leading star)
    if let Some(suffix) = pattern.strip_prefix('*') {
        return segment.ends_with(suffix);
    }

    // Exact match
    pattern == segment
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_match_exact() {
        assert!(glob_match("src/main.rs", "src/main.rs"));
        assert!(!glob_match("src/main.rs", "src/lib.rs"));
    }

    #[test]
    fn test_glob_match_star() {
        assert!(glob_match("src/*.rs", "src/main.rs"));
        assert!(glob_match("src/*.rs", "src/lib.rs"));
        assert!(!glob_match("src/*.rs", "src/sub/main.rs"));
    }

    #[test]
    fn test_glob_match_double_star() {
        assert!(glob_match("src/**", "src/main.rs"));
        assert!(glob_match("src/**", "src/sub/deep/file.rs"));
        assert!(glob_match("**", "anything/at/all"));
        assert!(!glob_match("src/**", "tests/main.rs"));
    }

    #[test]
    fn test_glob_match_gid_features() {
        assert!(glob_match(".gid/features/**", ".gid/features/auth/requirements.md"));
        assert!(glob_match(".gid/features/**", ".gid/features/auth/design.md"));
        assert!(!glob_match(".gid/features/**", ".gid/graph.yml"));
    }

    #[test]
    fn test_glob_match_research_prefix() {
        assert!(glob_match("docs/RESEARCH-*", "docs/RESEARCH-auth.md"));
        assert!(glob_match("docs/RESEARCH-*", "docs/RESEARCH-social-media.md"));
        assert!(!glob_match("docs/RESEARCH-*", "docs/DESIGN.md"));
    }

    #[test]
    fn test_tool_scope_research() {
        let scope = ToolScope::research();
        assert!(scope.is_tool_allowed("Read"));
        assert!(scope.is_tool_allowed("Write"));
        assert!(scope.is_tool_allowed("Edit"));
        assert!(scope.is_tool_allowed("WebSearch"));
        assert!(!scope.is_tool_allowed("Bash"));

        assert!(scope.is_path_writable(".gid/features/auth/research.md"));
        assert!(scope.is_path_writable("docs/RESEARCH-auth.md"));
        assert!(!scope.is_path_writable("src/main.rs"));
    }

    #[test]
    fn test_tool_scope_verify() {
        let scope = ToolScope::verify();
        assert!(scope.is_tool_allowed("Read"));
        assert!(scope.is_tool_allowed("Bash"));
        assert!(!scope.is_tool_allowed("Write"));
        assert!(!scope.is_tool_allowed("Edit"));

        assert!(scope.is_bash_allowed("cargo test"));
        assert!(scope.is_bash_allowed("cargo test --release"));
        assert!(scope.is_bash_allowed("pytest -v"));
        assert!(!scope.is_bash_allowed("rm -rf /"));
        assert!(!scope.is_bash_allowed("cargo build"));
    }

    #[test]
    fn test_tool_scope_full() {
        let scope = ToolScope::full();
        assert!(scope.is_tool_allowed("Read"));
        assert!(scope.is_tool_allowed("Write"));
        assert!(scope.is_tool_allowed("Edit"));
        assert!(scope.is_tool_allowed("Bash"));
        assert!(scope.is_path_writable("anything/at/all.rs"));
        assert!(scope.is_bash_allowed("any command"));
    }

    #[test]
    fn test_default_scope_for_phase() {
        let research = default_scope_for_phase("research");
        assert!(!research.is_tool_allowed("Bash"));
        assert!(research.is_tool_allowed("WebSearch"));

        let coding = default_scope_for_phase("execute-tasks");
        assert!(coding.is_tool_allowed("Bash"));
        assert!(coding.is_bash_allowed("anything"));

        let verify = default_scope_for_phase("verify-quality");
        assert!(!verify.is_tool_allowed("Write"));
        assert!(verify.is_bash_allowed("cargo test"));
        assert!(!verify.is_bash_allowed("cargo build"));
    }

    #[test]
    fn test_unknown_phase_gets_full() {
        let scope = default_scope_for_phase("custom-phase-xyz");
        assert!(scope.is_tool_allowed("Bash"));
        assert!(scope.is_path_writable("anything"));
    }

    #[test]
    fn test_readable_paths_empty_means_all() {
        let scope = ToolScope::research();
        assert!(scope.is_path_readable("src/anything.rs"));
        assert!(scope.is_path_readable(".gid/graph.yml"));
    }

    #[test]
    fn test_bash_deny() {
        assert!(!BashPolicy::Deny.eq(&BashPolicy::AllowAll));
        let scope = ToolScope::documentation();
        assert!(!scope.is_bash_allowed("any command"));
    }

    #[test]
    fn test_tool_mapping_rustclaw() {
        let scope = ToolScope::research();
        let mapped = scope.with_tool_mapping(&rustclaw_tool_mapping());
        assert!(mapped.is_tool_allowed("read_file"));
        assert!(mapped.is_tool_allowed("write_file"));
        assert!(mapped.is_tool_allowed("edit_file"));
        assert!(mapped.is_tool_allowed("web_search"));
        assert!(mapped.is_tool_allowed("web_fetch"));
        assert!(!mapped.is_tool_allowed("exec"));
    }

    #[test]
    fn test_filter_tools() {
        let scope = ToolScope::research().with_tool_mapping(&rustclaw_tool_mapping());
        let tools = vec![
            "read_file", "write_file", "edit_file", "exec",
            "web_search", "web_fetch",
            "gid_tasks", "gid_read",  // GID tools always pass
            "engram_recall",           // Engram always passes
            "tts",                     // Communication always passes
        ];

        let filtered = scope.filter_tools(tools, |t| t);
        assert!(filtered.contains(&"read_file"));
        assert!(filtered.contains(&"write_file"));
        assert!(filtered.contains(&"edit_file")); // Research allows Edit
        assert!(filtered.contains(&"web_search"));
        assert!(filtered.contains(&"gid_tasks"));
        assert!(filtered.contains(&"engram_recall"));
        assert!(filtered.contains(&"tts"));
        assert!(!filtered.contains(&"exec"));
    }

    #[test]
    fn test_ritual_phase_scope_category() {
        use super::super::state_machine::RitualPhase;

        assert_eq!(RitualPhase::Designing.scope_category(), Some(ScopeCategory::Design));
        assert_eq!(RitualPhase::Planning.scope_category(), Some(ScopeCategory::Plan));
        assert_eq!(RitualPhase::Graphing.scope_category(), Some(ScopeCategory::Design));
        assert_eq!(RitualPhase::Implementing.scope_category(), Some(ScopeCategory::Implement));
        assert_eq!(RitualPhase::Verifying.scope_category(), Some(ScopeCategory::Verify));
        assert_eq!(RitualPhase::Idle.scope_category(), None);
        assert_eq!(RitualPhase::Done.scope_category(), None);
        assert_eq!(RitualPhase::Escalated.scope_category(), None);
    }

    #[test]
    fn test_filter_tools_verify_phase() {
        let scope = ToolScope::verify().with_tool_mapping(&rustclaw_tool_mapping());
        let tools = vec![
            "read_file", "write_file", "edit_file", "exec",
            "web_search", "gid_tasks",
        ];

        let filtered = scope.filter_tools(tools, |t| t);
        assert!(filtered.contains(&"read_file"));
        assert!(filtered.contains(&"exec"));       // Bash is allowed in verify
        assert!(filtered.contains(&"gid_tasks"));  // GID always passes
        assert!(!filtered.contains(&"write_file")); // No writes in verify!
        assert!(!filtered.contains(&"edit_file"));
        assert!(!filtered.contains(&"web_search"));
    }
}
