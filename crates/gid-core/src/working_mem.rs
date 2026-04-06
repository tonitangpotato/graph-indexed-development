//! Working Memory — context for code changes
//!
//! Provides GID-based context about edited files and their impact.
//! Used by agents to understand the blast radius of their changes.

use crate::code_graph::{CodeGraph, CodeNode, NodeKind};
use std::collections::HashSet;

// ═══ Data Structures ═══

/// GID-provided structural data about edited nodes.
#[derive(Debug, Clone, Default)]
pub struct GidContext {
    /// Nodes that were touched/modified
    pub nodes_touched: Vec<NodeInfo>,
    /// Maximum number of callers for any touched node
    pub max_callers: usize,
    /// Total blast radius (sum of all callers)
    pub total_blast_radius: usize,
    /// Hub nodes (high connectivity)
    pub hub_nodes: Vec<NodeInfo>,
}

/// Info about a single code node.
#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub id: String,
    pub name: String,
    pub file: String,
    pub kind: String,
    pub callers: usize,
    pub callees: usize,
    pub line: Option<usize>,
}

impl NodeInfo {
    pub fn from_code_node(node: &CodeNode, callers: usize, callees: usize) -> Self {
        Self {
            id: node.id.clone(),
            name: node.name.clone(),
            file: node.file_path.clone(),
            kind: match node.kind {
                NodeKind::File => "file",
                NodeKind::Class | NodeKind::Interface | NodeKind::Enum | NodeKind::TypeAlias | NodeKind::Trait => "class",
                NodeKind::Function | NodeKind::Constant => "function",
                NodeKind::Module => "module",
            }.to_string(),
            callers,
            callees,
            line: node.line,
        }
    }
}

/// Test outcome classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorType {
    Syntax,
    Import,
    Attribute,
    Assertion,
    Type,
    Name,
    Runtime,
    Timeout,
    Unknown,
}

impl std::fmt::Display for ErrorType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErrorType::Syntax => write!(f, "SyntaxError"),
            ErrorType::Import => write!(f, "ImportError"),
            ErrorType::Attribute => write!(f, "AttributeError"),
            ErrorType::Assertion => write!(f, "AssertionError"),
            ErrorType::Type => write!(f, "TypeError"),
            ErrorType::Name => write!(f, "NameError"),
            ErrorType::Runtime => write!(f, "RuntimeError"),
            ErrorType::Timeout => write!(f, "Timeout"),
            ErrorType::Unknown => write!(f, "Unknown"),
        }
    }
}

// ═══ Context Queries ═══

/// Query GID context for changed files.
/// Returns structural data about the nodes in those files.
pub fn query_gid_context(files_changed: &[String], graph: &CodeGraph) -> GidContext {
    let mut nodes = Vec::new();
    let mut max_callers = 0;
    let mut total_blast = 0;
    
    for file in files_changed {
        // Find all function/class nodes in this file
        let file_nodes: Vec<&CodeNode> = graph.nodes.iter()
            .filter(|n| {
                n.file_path == *file
                    && !n.is_test
                    && matches!(n.kind, NodeKind::Function | NodeKind::Class)
            })
            .collect();
        
        for node in file_nodes {
            let callers = graph.get_callers(&node.id).len();
            let callees = graph.get_callees(&node.id).len();
            
            max_callers = max_callers.max(callers);
            total_blast += callers;
            
            nodes.push(NodeInfo::from_code_node(node, callers, callees));
        }
    }
    
    // Sort by caller count descending, keep top 10
    nodes.sort_by(|a, b| b.callers.cmp(&a.callers));
    nodes.truncate(10);
    
    // Identify hub nodes (high connectivity)
    let hub_threshold = 10;
    let hub_nodes: Vec<NodeInfo> = nodes.iter()
        .filter(|n| n.callers >= hub_threshold)
        .cloned()
        .collect();
    
    GidContext {
        nodes_touched: nodes,
        max_callers,
        total_blast_radius: total_blast,
        hub_nodes,
    }
}

/// Find low-coupling alternative nodes near the failed files.
/// Called after high-coupling failures to suggest safer edit targets.
pub fn find_low_risk_alternatives(
    graph: &CodeGraph,
    failed_files: &[String],
    max_callers: usize,
) -> Vec<NodeInfo> {
    let mut alternatives = Vec::new();
    
    // Find packages containing failed files
    let packages: HashSet<String> = failed_files.iter()
        .filter_map(|f| {
            f.rsplitn(2, '/').nth(1).map(|s| s.to_string())
        })
        .collect();
    
    for node in &graph.nodes {
        if node.is_test {
            continue;
        }
        if !matches!(node.kind, NodeKind::Function) {
            continue;
        }
        
        // Must be in a related package
        let in_package = packages.iter().any(|pkg| node.file_path.starts_with(pkg));
        if !in_package {
            continue;
        }
        
        // Must not be in the same files we already tried
        if failed_files.contains(&node.file_path) {
            continue;
        }
        
        let callers = graph.get_callers(&node.id).len();
        if callers <= max_callers {
            let callees = graph.get_callees(&node.id).len();
            alternatives.push(NodeInfo::from_code_node(node, callers, callees));
        }
    }
    
    // Sort by caller count ascending (safest first)
    alternatives.sort_by_key(|n| n.callers);
    alternatives.truncate(5);
    alternatives
}

/// Classify error type from raw test output.
pub fn classify_error(raw_output: &str) -> ErrorType {
    let checks: &[(ErrorType, &[&str])] = &[
        (ErrorType::Syntax, &["SyntaxError:", "SyntaxError("]),
        (ErrorType::Import, &["ImportError:", "ModuleNotFoundError:"]),
        (ErrorType::Attribute, &["AttributeError:"]),
        (ErrorType::Assertion, &["AssertionError:", "AssertionError(", "assert "]),
        (ErrorType::Type, &["TypeError:"]),
        (ErrorType::Name, &["NameError:"]),
        (ErrorType::Timeout, &["TimeoutError", "timed out", "TIMEOUT"]),
    ];
    
    let mut best = ErrorType::Unknown;
    let mut best_count = 0;
    
    for (etype, patterns) in checks {
        let count: usize = patterns.iter()
            .map(|p| raw_output.matches(p).count())
            .sum();
        if count > best_count {
            best_count = count;
            best = etype.clone();
        }
    }
    
    // SyntaxError is usually the root cause
    if best != ErrorType::Syntax && raw_output.contains("SyntaxError:") {
        return ErrorType::Syntax;
    }
    
    best
}

/// Extract the key traceback from test output.
pub fn extract_key_traceback(raw_output: &str, max_chars: usize) -> String {
    let traceback_marker = "Traceback (most recent call last)";
    
    if let Some(pos) = raw_output.find(traceback_marker) {
        let chunk = &raw_output[pos..];
        let end = chunk.find("\n\n")
            .or_else(|| chunk.find("\n====="))
            .or_else(|| chunk.find("\nFAILED"))
            .unwrap_or(chunk.len());
        return chunk[..end.min(max_chars)].to_string();
    }
    
    // Fallback: look for FAILED/ERROR sections
    for marker in &["FAIL:", "ERROR:", "FAILED "] {
        if let Some(pos) = raw_output.find(marker) {
            let start = pos.saturating_sub(200);
            let end = (pos + max_chars).min(raw_output.len());
            return raw_output[start..end].to_string();
        }
    }
    
    // Last resort: tail of output
    let start = raw_output.len().saturating_sub(max_chars);
    raw_output[start..].to_string()
}

// ═══ Impact Analysis ═══

/// Analyze what's affected by changing given files.
#[derive(Debug, Clone)]
pub struct ImpactAnalysis {
    /// Source nodes directly or transitively affected
    pub affected_source: Vec<NodeInfo>,
    /// Test nodes that exercise the changed code
    pub affected_tests: Vec<NodeInfo>,
    /// Risk level (low, medium, high, critical)
    pub risk_level: RiskLevel,
    /// Human-readable summary
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskLevel {
    Low,      // < 5 callers
    Medium,   // 5-20 callers
    High,     // 20-50 callers
    Critical, // > 50 callers
}

impl std::fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RiskLevel::Low => write!(f, "low"),
            RiskLevel::Medium => write!(f, "medium"),
            RiskLevel::High => write!(f, "high"),
            RiskLevel::Critical => write!(f, "critical"),
        }
    }
}

/// Analyze impact of changing files.
pub fn analyze_impact(files_changed: &[String], graph: &CodeGraph) -> ImpactAnalysis {
    let gid_ctx = query_gid_context(files_changed, graph);
    
    let mut affected_source = Vec::new();
    let mut affected_tests = Vec::new();
    let mut seen = HashSet::new();
    
    // Get all nodes in changed files
    let changed_node_ids: Vec<String> = graph.nodes.iter()
        .filter(|n| files_changed.contains(&n.file_path))
        .map(|n| n.id.clone())
        .collect();
    
    // Find affected nodes (who calls/depends on changed nodes)
    for node_id in &changed_node_ids {
        for impacted in graph.get_impact(node_id) {
            if seen.insert(impacted.id.clone()) {
                let callers = graph.get_callers(&impacted.id).len();
                let callees = graph.get_callees(&impacted.id).len();
                let info = NodeInfo::from_code_node(impacted, callers, callees);
                
                if impacted.is_test {
                    affected_tests.push(info);
                } else {
                    affected_source.push(info);
                }
            }
        }
    }
    
    // Determine risk level
    let risk_level = match gid_ctx.max_callers {
        0..=5 => RiskLevel::Low,
        6..=20 => RiskLevel::Medium,
        21..=50 => RiskLevel::High,
        _ => RiskLevel::Critical,
    };
    
    // Build summary
    let summary = format!(
        "Changing {} file(s) affects {} source nodes and {} test nodes. Risk: {} (max {} callers, blast radius {}).",
        files_changed.len(),
        affected_source.len(),
        affected_tests.len(),
        risk_level,
        gid_ctx.max_callers,
        gid_ctx.total_blast_radius,
    );
    
    ImpactAnalysis {
        affected_source,
        affected_tests,
        risk_level,
        summary,
    }
}

/// Format impact analysis for LLM consumption.
pub fn format_impact_for_llm(analysis: &ImpactAnalysis) -> String {
    let mut result = String::new();
    
    result.push_str(&format!("## Impact Analysis\n\n{}\n\n", analysis.summary));
    
    if !analysis.affected_source.is_empty() {
        result.push_str("**Affected source code:**\n");
        for node in analysis.affected_source.iter().take(10) {
            result.push_str(&format!(
                "- {} `{}` ({} callers)\n",
                node.kind, node.name, node.callers
            ));
        }
        if analysis.affected_source.len() > 10 {
            result.push_str(&format!("  ...and {} more\n", analysis.affected_source.len() - 10));
        }
        result.push('\n');
    }
    
    if !analysis.affected_tests.is_empty() {
        result.push_str("**Related tests:**\n");
        for node in analysis.affected_tests.iter().take(10) {
            result.push_str(&format!("- `{}` in {}\n", node.name, node.file));
        }
        if analysis.affected_tests.len() > 10 {
            result.push_str(&format!("  ...and {} more\n", analysis.affected_tests.len() - 10));
        }
        result.push('\n');
    }
    
    if analysis.risk_level == RiskLevel::High || analysis.risk_level == RiskLevel::Critical {
        result.push_str("⚠️ **High-risk change!** Consider:\n");
        result.push_str("- Breaking the change into smaller pieces\n");
        result.push_str("- Adding backward compatibility\n");
        result.push_str("- Running full test suite before committing\n\n");
    }
    
    result
}

// ═══ Agent Working Memory ═══

/// What action the agent took in a round.
#[derive(Debug, Clone)]
pub enum Action {
    Edit { files: Vec<String>, applied: usize, total: usize },
    Revert,
    Read { file: String },
    Search { pattern: String },
    Query { kind: String, target: String },
    Test,
    Other(String),
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Action::Edit { files, applied, total } => {
                let names: Vec<&str> = files.iter().map(|f| {
                    f.rsplit('/').next().unwrap_or(f.as_str())
                }).collect();
                write!(f, "EDIT {} ({}/{})", names.join(", "), applied, total)
            }
            Action::Revert => write!(f, "REVERT"),
            Action::Read { file } => write!(f, "READ {}", file.rsplit('/').next().unwrap_or(file)),
            Action::Search { pattern } => {
                let display = if pattern.len() > 30 {
                    let mut end = 30;
                    while end > 0 && !pattern.is_char_boundary(end) { end -= 1; }
                    &pattern[..end]
                } else {
                    pattern.as_str()
                };
                write!(f, "SEARCH '{}'", display)
            }
            Action::Query { kind, target } => write!(f, "GID {} {}", kind, target),
            Action::Test => write!(f, "TEST"),
            Action::Other(s) => {
                let display = if s.len() > 30 {
                    let mut end = 30;
                    while end > 0 && !s.is_char_boundary(end) { end -= 1; }
                    &s[..end]
                } else {
                    s.as_str()
                };
                write!(f, "{}", display)
            }
        }
    }
}

/// Test outcome with classified error type.
#[derive(Debug, Clone)]
pub struct TestOutcome {
    /// Error type classified from output
    pub error_type: ErrorType,
    /// (passed, total) for primary test set
    pub primary: (usize, usize),
    /// (passed, total) for secondary/regression test set
    pub secondary: (usize, usize),
    /// Key traceback or error message
    pub key_error_trace: String,
    /// Names of failed secondary tests
    pub failed_secondary_names: Vec<String>,
}

impl TestOutcome {
    pub fn new(
        error_type: ErrorType,
        primary_passed: usize,
        primary_total: usize,
        secondary_passed: usize,
        secondary_total: usize,
    ) -> Self {
        Self {
            error_type,
            primary: (primary_passed, primary_total),
            secondary: (secondary_passed, secondary_total),
            key_error_trace: String::new(),
            failed_secondary_names: Vec::new(),
        }
    }

    pub fn with_trace(mut self, trace: String) -> Self {
        self.key_error_trace = trace;
        self
    }

    pub fn with_failed_names(mut self, names: Vec<String>) -> Self {
        self.failed_secondary_names = names;
        self
    }

    /// Calculate a composite score. Higher is better.
    /// Primary tests are weighted heavily; secondary regressions penalize.
    pub fn score(&self) -> i32 {
        let secondary_clean = if self.secondary.1 == 0 || self.secondary.0 == self.secondary.1 { 1 } else { 0 };
        (self.primary.0 as i32) * 1000 * secondary_clean + self.secondary.0 as i32
    }
}

/// One round's record in working memory.
#[derive(Debug, Clone)]
pub struct AttemptRecord {
    pub round: usize,
    pub action: Action,
    pub gid_context: Option<GidContext>,
    pub test_outcome: Option<TestOutcome>,
    /// Immediate feedback text (edit result, read content, etc.)
    pub feedback: String,
}

/// Accumulated risk data for a node.
#[derive(Debug, Clone)]
pub struct NodeRisk {
    pub callers: usize,
    pub times_tried: usize,
    pub times_failed: usize,
}

/// The complete working state for an agent repair/task session.
/// Generic — tracks what the agent has done, what worked, what failed.
pub struct WorkingMemory {
    pub attempts: Vec<AttemptRecord>,
    pub node_risk_map: std::collections::HashMap<String, NodeRisk>,
    pub best_score: i32,
    pub best_attempt: Option<usize>,
    /// Low-risk alternative nodes found by graph analysis (cached after high-coupling failure).
    pub low_risk_alternatives: Vec<NodeInfo>,
}

impl Default for WorkingMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkingMemory {
    pub fn new() -> Self {
        Self {
            attempts: Vec::new(),
            node_risk_map: std::collections::HashMap::new(),
            best_score: -1,
            best_attempt: None,
            low_risk_alternatives: Vec::new(),
        }
    }

    /// Record an EDIT action with GID context.
    pub fn record_edit(
        &mut self,
        round: usize,
        files: Vec<String>,
        applied: usize,
        total: usize,
        gid_ctx: GidContext,
        feedback: String,
    ) {
        self.attempts.push(AttemptRecord {
            round,
            action: Action::Edit { files, applied, total },
            gid_context: Some(gid_ctx),
            test_outcome: None,
            feedback,
        });
    }

    /// Record a TEST result. Updates best score and node risk map.
    pub fn record_test(&mut self, round: usize, outcome: TestOutcome, raw_feedback: String) {
        let score = outcome.score();

        if score > self.best_score {
            self.best_score = score;
            self.best_attempt = Some(round);
        }

        // Update node risk map from the most recent EDIT's GID context
        if let Some(last_edit) = self.attempts.iter().rev().find(|a| matches!(a.action, Action::Edit { .. })) {
            if let Some(ref gid) = last_edit.gid_context {
                for node in &gid.nodes_touched {
                    let entry = self.node_risk_map.entry(node.name.clone()).or_insert(NodeRisk {
                        callers: node.callers,
                        times_tried: 0,
                        times_failed: 0,
                    });
                    entry.times_tried += 1;
                    if outcome.secondary.0 < outcome.secondary.1 || outcome.primary.0 < outcome.primary.1 {
                        entry.times_failed += 1;
                    }
                }
            }
        }

        self.attempts.push(AttemptRecord {
            round,
            action: Action::Test,
            gid_context: None,
            test_outcome: Some(outcome),
            feedback: raw_feedback,
        });
    }

    /// Record a non-test, non-edit action (READ, SEARCH, REVERT, query).
    pub fn record_action(&mut self, round: usize, action: Action, feedback: String) {
        self.attempts.push(AttemptRecord {
            round,
            action,
            gid_context: None,
            test_outcome: None,
            feedback,
        });
    }

    /// Project working memory to LLM-readable prompt text.
    /// Provides structured data — facts, not conclusions.
    pub fn project_to_prompt(&self) -> String {
        let mut out = String::new();

        // Section 1: Attempt history table
        let test_attempts: Vec<&AttemptRecord> = self.attempts.iter()
            .filter(|a| a.test_outcome.is_some())
            .collect();

        if !test_attempts.is_empty() {
            out.push_str("## Attempt History\n\n");
            out.push_str("| # | Target | Callers | Error | Primary | Secondary |\n");
            out.push_str("|---|--------|---------|-------|---------|------------|\n");

            for test_a in &test_attempts {
                let t = test_a.test_outcome.as_ref().unwrap();

                // Find the last EDIT before this TEST
                let edit_info = self.attempts.iter()
                    .filter(|a| a.round < test_a.round && matches!(a.action, Action::Edit { .. }))
                    .last();

                let (target, callers) = if let Some(edit) = edit_info {
                    let target_str = match &edit.action {
                        Action::Edit { files, .. } => {
                            files.iter()
                                .map(|f| f.rsplit('/').next().unwrap_or(f))
                                .collect::<Vec<_>>()
                                .join(", ")
                        }
                        _ => "-".into(),
                    };
                    let callers_str = edit.gid_context.as_ref()
                        .map(|g| g.max_callers.to_string())
                        .unwrap_or("-".into());
                    (target_str, callers_str)
                } else {
                    ("-".into(), "-".into())
                };

                out.push_str(&format!(
                    "| {} | {} | {} | {} | {}/{} | {}/{} |\n",
                    test_a.round,
                    target,
                    callers,
                    t.error_type,
                    t.primary.0, t.primary.1,
                    t.secondary.0, t.secondary.1,
                ));
            }
            out.push('\n');
        }

        // Section 2: Node risk data
        let mut risky: Vec<(&String, &NodeRisk)> = self.node_risk_map.iter()
            .filter(|(_, r)| r.times_failed > 0)
            .collect();
        risky.sort_by(|a, b| b.1.callers.cmp(&a.1.callers));

        if !risky.is_empty() {
            out.push_str("## Node History\n");
            for (name, risk) in risky.iter().take(10) {
                out.push_str(&format!(
                    "- {} — {} callers, tried {}, failed {}\n",
                    name, risk.callers, risk.times_tried, risk.times_failed
                ));
            }
            out.push('\n');
        }

        // Section 3: Low-risk alternatives
        if !self.low_risk_alternatives.is_empty() {
            out.push_str("## Low-Coupling Alternatives\n");
            for alt in &self.low_risk_alternatives {
                out.push_str(&format!(
                    "- {} ({}) — {} callers\n",
                    alt.name, alt.file.rsplit('/').next().unwrap_or(&alt.file), alt.callers
                ));
            }
            out.push('\n');
        }

        // Section 4: Latest error detail
        if let Some(last_test) = self.attempts.iter().rev().find(|a| a.test_outcome.is_some()) {
            let t = last_test.test_outcome.as_ref().unwrap();
            out.push_str(&format!("## Latest Error (Round {})\n", last_test.round));
            out.push_str(&format!("Type: {}\n", t.error_type));
            out.push_str(&format!("Primary: {}/{}, Secondary: {}/{}\n", 
                t.primary.0, t.primary.1, t.secondary.0, t.secondary.1));

            if !t.key_error_trace.is_empty() {
                out.push_str(&format!("\n```\n{}\n```\n", t.key_error_trace));
            }

            // Show failed secondary test names
            if !t.failed_secondary_names.is_empty() {
                let show: Vec<&str> = t.failed_secondary_names.iter().take(10).map(|s| s.as_str()).collect();
                let remaining = t.failed_secondary_names.len().saturating_sub(10);
                out.push_str(&format!("\nFailed: {}", show.join(", ")));
                if remaining > 0 {
                    out.push_str(&format!(" (+{} more)", remaining));
                }
                out.push('\n');
            }
        }

        // Section 5: Best result
        if let Some(best_round) = self.best_attempt {
            out.push_str(&format!(
                "\n## Best Result: Round {} (score {})\n",
                best_round, self.best_score
            ));
        }

        out
    }

    /// Get the last tool feedback for inclusion in the next prompt.
    pub fn last_feedback(&self) -> &str {
        self.attempts.last()
            .map(|a| a.feedback.as_str())
            .unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_graph::{CodeEdge, EdgeRelation};
    
    #[test]
    fn test_classify_error() {
        assert_eq!(classify_error("SyntaxError: invalid syntax"), ErrorType::Syntax);
        assert_eq!(classify_error("ImportError: No module named 'foo'"), ErrorType::Import);
        assert_eq!(classify_error("AssertionError: 1 != 2"), ErrorType::Assertion);
    }
    
    #[test]
    fn test_classify_syntax_overrides() {
        let output = "ImportError: ...\nSyntaxError: invalid syntax\nImportError: ...";
        assert_eq!(classify_error(output), ErrorType::Syntax);
    }
    
    #[test]
    fn test_risk_level() {
        let mut graph = CodeGraph::default();
        
        // Create a function with many callers
        graph.nodes.push(CodeNode {
            id: "func:core.py:hot_func".into(),
            kind: NodeKind::Function,
            name: "hot_func".into(),
            file_path: "core.py".into(),
            line: Some(10),
            decorators: vec![],
            signature: None,
            docstring: None,
            line_count: 20,
            is_test: false,
        });
        
        // Add many callers
        for i in 0..30 {
            let caller_id = format!("func:caller{}.py:caller_{}", i, i);
            graph.nodes.push(CodeNode {
                id: caller_id.clone(),
                kind: NodeKind::Function,
                name: format!("caller_{}", i),
                file_path: format!("caller{}.py", i),
                line: Some(1),
                decorators: vec![],
                signature: None,
                docstring: None,
                line_count: 5,
                is_test: false,
            });
            graph.edges.push(CodeEdge::new(&caller_id, "func:core.py:hot_func", EdgeRelation::Calls));
        }
        
        graph.build_indexes();
        
        let analysis = analyze_impact(&["core.py".into()], &graph);
        assert_eq!(analysis.risk_level, RiskLevel::High);
    }
    
    #[test]
    fn test_extract_traceback() {
        let output = r#"
FAILED tests/test_foo.py::test_bar
Traceback (most recent call last):
  File "tests/test_foo.py", line 10, in test_bar
    assert result == expected
AssertionError: 1 != 2

FAILED tests/test_other.py::test_baz
"#;
        let tb = extract_key_traceback(output, 500);
        assert!(tb.contains("Traceback (most recent call last)"));
        assert!(tb.contains("AssertionError: 1 != 2"));
    }
}
