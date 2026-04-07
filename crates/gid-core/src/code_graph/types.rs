//! Type definitions for the code graph module.

use std::collections::HashMap;
use std::path::Path;
use serde::{Deserialize, Serialize};

// ═══ Incremental Extract Types ═══

/// Metadata stored alongside the code graph for change detection.
/// Persisted as `.gid/extract-meta.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractMetadata {
    /// Schema version — bump on struct changes. Mismatch → full rebuild.
    pub version: u32,
    /// When this metadata was last updated (ISO 8601).
    pub updated_at: String,
    /// Per-file tracking: relative path → FileState
    pub files: HashMap<String, FileState>,
}

impl Default for ExtractMetadata {
    fn default() -> Self {
        Self {
            version: 1,
            updated_at: String::new(),
            files: HashMap::new(),
        }
    }
}

/// Per-file state for change detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileState {
    /// File modification time (Unix seconds).
    pub mtime: u64,
    /// xxHash64 of file content.
    pub content_hash: u64,
    /// Node IDs that were extracted from this file.
    pub node_ids: Vec<String>,
    /// Number of edges originating from this file (reporting only).
    pub edge_count: usize,
}

/// Result of comparing current filesystem vs stored metadata.
#[derive(Debug, Clone, Default)]
pub struct FileDelta {
    /// New files not in metadata.
    pub added: Vec<String>,
    /// Files with changed content hash.
    pub modified: Vec<String>,
    /// Files in metadata but not on disk.
    pub deleted: Vec<String>,
    /// Files with unchanged content.
    pub unchanged: Vec<String>,
}

impl FileDelta {
    /// Returns true if there are no changes.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.deleted.is_empty()
    }

    /// Total number of changed files.
    pub fn changed_count(&self) -> usize {
        self.added.len() + self.modified.len() + self.deleted.len()
    }
}

/// Report of an extraction run.
#[derive(Debug, Clone)]
pub struct ExtractReport {
    /// Number of newly added files.
    pub added: usize,
    /// Number of modified files.
    pub modified: usize,
    /// Number of deleted files.
    pub deleted: usize,
    /// Number of unchanged files.
    pub unchanged: usize,
    /// Whether this was a full rebuild (--force or no prior metadata).
    pub full_rebuild: bool,
    /// Duration of the extraction in milliseconds.
    pub duration_ms: u64,
}

impl std::fmt::Display for ExtractReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.full_rebuild {
            write!(
                f,
                "Full rebuild: {} files extracted ({}ms)",
                self.added + self.modified + self.unchanged,
                self.duration_ms
            )
        } else if self.added == 0 && self.modified == 0 && self.deleted == 0 {
            write!(f, "Graph is up to date ({} files, {}ms)", self.unchanged, self.duration_ms)
        } else {
            let total_changed = self.added + self.modified + self.deleted;
            let mut parts = Vec::new();
            if self.modified > 0 {
                parts.push(format!("{} modified", self.modified));
            }
            if self.added > 0 {
                parts.push(format!("{} added", self.added));
            }
            if self.deleted > 0 {
                parts.push(format!("{} deleted", self.deleted));
            }
            write!(
                f,
                "Updated {} files ({}), {} unchanged ({}ms)",
                total_changed,
                parts.join(", "),
                self.unchanged,
                self.duration_ms
            )
        }
    }
}

// ═══ Graph Types ═══

/// A code dependency graph extracted from source files.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodeGraph {
    pub nodes: Vec<CodeNode>,
    pub edges: Vec<CodeEdge>,
    /// Adjacency list: node_id → indices into self.edges (outgoing)
    #[serde(skip)]
    pub outgoing: HashMap<String, Vec<usize>>,
    /// Reverse adjacency list: node_id → indices into self.edges (incoming)
    #[serde(skip)]
    pub incoming: HashMap<String, Vec<usize>>,
    /// Node lookup: node_id → index into self.nodes
    #[serde(skip)]
    pub node_index: HashMap<String, usize>,
}

/// A node in the code graph (file, class, function).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeNode {
    pub id: String,
    pub kind: NodeKind,
    pub name: String,
    pub file_path: String,
    pub line: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decorators: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    #[serde(default)]
    pub line_count: usize,
    #[serde(default)]
    pub is_test: bool,
}

impl CodeNode {
    pub fn new_file(path: &str) -> Self {
        Self {
            id: format!("file:{}", path),
            kind: NodeKind::File,
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            file_path: path.to_string(),
            line: None,
            decorators: Vec::new(),
            signature: None,
            docstring: None,
            line_count: 0,
            is_test: path.contains("/test") || path.contains("_test."),
        }
    }

    pub fn new_class(path: &str, name: &str, line: usize) -> Self {
        Self {
            id: format!("class:{}:{}", path, name),
            kind: NodeKind::Class,
            name: name.to_string(),
            file_path: path.to_string(),
            line: Some(line),
            decorators: Vec::new(),
            signature: None,
            docstring: None,
            line_count: 0,
            is_test: name.starts_with("Test") || path.contains("/test"),
        }
    }

    pub fn new_function(path: &str, name: &str, line: usize, is_method: bool) -> Self {
        let prefix = if is_method { "method" } else { "func" };
        Self {
            id: format!("{}:{}:{}", prefix, path, name),
            kind: NodeKind::Function,
            name: name.to_string(),
            file_path: path.to_string(),
            line: Some(line),
            decorators: Vec::new(),
            signature: None,
            docstring: None,
            line_count: 0,
            is_test: name.starts_with("test_") || name.starts_with("Test") || path.contains("/test"),
        }
    }

    pub fn new_constant(path: &str, name: &str, line: usize) -> Self {
        Self {
            id: format!("const:{}:{}", path, name),
            kind: NodeKind::Constant,
            name: name.to_string(),
            file_path: path.to_string(),
            line: Some(line),
            decorators: Vec::new(),
            signature: None,
            docstring: None,
            line_count: 0,
            is_test: false,
        }
    }

    pub fn new_interface(path: &str, name: &str, line: usize) -> Self {
        Self {
            id: format!("interface:{}:{}", path, name),
            kind: NodeKind::Interface,
            name: name.to_string(),
            file_path: path.to_string(),
            line: Some(line),
            decorators: Vec::new(),
            signature: None,
            docstring: None,
            line_count: 0,
            is_test: false,
        }
    }

    pub fn new_enum(path: &str, name: &str, line: usize) -> Self {
        Self {
            id: format!("enum:{}:{}", path, name),
            kind: NodeKind::Enum,
            name: name.to_string(),
            file_path: path.to_string(),
            line: Some(line),
            decorators: Vec::new(),
            signature: None,
            docstring: None,
            line_count: 0,
            is_test: false,
        }
    }

    pub fn new_type_alias(path: &str, name: &str, line: usize) -> Self {
        Self {
            id: format!("type:{}:{}", path, name),
            kind: NodeKind::TypeAlias,
            name: name.to_string(),
            file_path: path.to_string(),
            line: Some(line),
            decorators: Vec::new(),
            signature: None,
            docstring: None,
            line_count: 0,
            is_test: false,
        }
    }

    pub fn new_trait(path: &str, name: &str, line: usize) -> Self {
        Self {
            id: format!("trait:{}:{}", path, name),
            kind: NodeKind::Trait,
            name: name.to_string(),
            file_path: path.to_string(),
            line: Some(line),
            decorators: Vec::new(),
            signature: None,
            docstring: None,
            line_count: 0,
            is_test: false,
        }
    }

    /// Create a module node from a directory path.
    /// Module nodes represent directory-level grouping of source files.
    pub fn new_module(dir_path: &str) -> Self {
        let name = dir_path.rsplit('/').next().unwrap_or(dir_path);
        Self {
            id: format!("module:{}", dir_path),
            kind: NodeKind::Module,
            name: name.to_string(),
            file_path: dir_path.to_string(), // NOTE: stores directory path, not file path
            line: None,
            decorators: Vec::new(),
            signature: None,
            docstring: None,
            line_count: 0,
            is_test: dir_path.contains("/test") || dir_path.contains("/tests"),
        }
    }
}

/// Kind of code node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    File,
    Class,
    Function,
    Module,
    Constant,
    Interface,
    Enum,
    TypeAlias,
    Trait,
}

/// An edge in the code graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeEdge {
    pub from: String,
    pub to: String,
    pub relation: EdgeRelation,
    #[serde(default)]
    pub weight: f32,
    #[serde(default)]
    pub call_count: u32,
    #[serde(default)]
    pub in_error_path: bool,
    #[serde(default)]
    pub confidence: f32,
    /// 0-indexed line of the call site expression (for LSP refinement)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_site_line: Option<u32>,
    /// 0-indexed column of the call site expression (for LSP refinement)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_site_column: Option<u32>,
}

impl CodeEdge {
    pub fn new(from: &str, to: &str, relation: EdgeRelation) -> Self {
        Self {
            from: from.to_string(),
            to: to.to_string(),
            relation,
            weight: 0.5,
            call_count: 1,
            in_error_path: false,
            confidence: 1.0,
            call_site_line: None,
            call_site_column: None,
        }
    }

    pub fn imports(from: &str, to: &str) -> Self {
        Self::new(from, to, EdgeRelation::Imports)
    }

    pub fn calls(from: &str, to: &str) -> Self {
        Self::new(from, to, EdgeRelation::Calls)
    }

    pub fn inherits(from: &str, to: &str) -> Self {
        Self::new(from, to, EdgeRelation::Inherits)
    }

    pub fn defined_in(from: &str, to: &str) -> Self {
        Self::new(from, to, EdgeRelation::DefinedIn)
    }

    /// Create a heuristic edge with explicit confidence (e.g., naming-convention TestsFor).
    pub fn new_heuristic(from: &str, to: &str, relation: EdgeRelation, confidence: f32) -> Self {
        Self {
            from: from.to_string(),
            to: to.to_string(),
            relation,
            weight: 0.5,
            call_count: 1,
            in_error_path: false,
            confidence,
            call_site_line: None,
            call_site_column: None,
        }
    }

    /// Compute composite weight from call_count, in_error_path, and confidence.
    pub fn compute_weight(&mut self) {
        if self.relation == EdgeRelation::Calls {
            let count_norm = (self.call_count as f32 / 10.0).min(1.0);
            let error_factor = if self.in_error_path { 0.8 } else { 0.5 };
            self.weight = 0.4 * count_norm + 0.3 * error_factor + 0.3 * self.confidence;
        } else {
            self.weight = 0.7; // Default for non-call edges
        }
    }
}

/// Edge relationship type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeRelation {
    /// File imports module
    Imports,
    /// Class inherits from parent
    Inherits,
    /// Entity is defined in file/class
    DefinedIn,
    /// Function calls another function
    Calls,
    /// Test file tests source file
    TestsFor,
    /// Method overrides parent method
    Overrides,
    /// Concrete method implements a trait/interface method
    Implements,
    /// Entity belongs to a container (file→module, module→parent module)
    BelongsTo,
}

impl std::fmt::Display for EdgeRelation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EdgeRelation::Imports => write!(f, "imports"),
            EdgeRelation::Inherits => write!(f, "inherits"),
            EdgeRelation::DefinedIn => write!(f, "defined_in"),
            EdgeRelation::Calls => write!(f, "calls"),
            EdgeRelation::TestsFor => write!(f, "tests_for"),
            EdgeRelation::Overrides => write!(f, "overrides"),
            EdgeRelation::Implements => write!(f, "implements"),
            EdgeRelation::BelongsTo => write!(f, "belongs_to"),
        }
    }
}

// ═══ Impact Analysis Types ═══

/// Result of impact analysis — what's affected by a change
#[derive(Debug)]
pub struct ImpactReport<'a> {
    pub affected_source: Vec<&'a CodeNode>,
    pub affected_tests: Vec<&'a CodeNode>,
}

/// A causal chain from symptom to potential root cause
#[derive(Debug, Clone)]
pub struct CausalChain {
    pub symptom_node_id: String,
    pub chain: Vec<ChainNode>,
}

#[derive(Debug, Clone)]
pub struct ChainNode {
    pub node_id: String,
    pub node_name: String,
    pub file_path: String,
    pub line: Option<usize>,
    pub edge_to_next: Option<String>,
}

// ═══ Language Detection ═══

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    TypeScript,
    Python,
    Unknown,
}

impl Language {
    pub fn from_path(path: &Path) -> Self {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        match ext {
            "rs" => Language::Rust,
            "ts" | "tsx" => Language::TypeScript,
            "js" | "jsx" => Language::TypeScript, // JS uses same patterns
            "py" => Language::Python,
            _ => Language::Unknown,
        }
    }
}

// ═══ Unified Graph Types ═══

/// Result of build_unified_graph — a simplified graph structure for task planning
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedGraphResult {
    pub issue_id: String,
    pub description: String,
    pub nodes: Vec<UnifiedNode>,
    pub edges: Vec<UnifiedEdge>,
}

/// A node in the unified graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedNode {
    pub id: String,
    pub node_type: String,
    pub layer: String,
    pub description: String,
    pub path: Option<String>,
    pub line: Option<usize>,
    pub code: Option<String>,
}

/// An edge in the unified graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedEdge {
    pub from: String,
    pub to: String,
    pub relation: String,
}
