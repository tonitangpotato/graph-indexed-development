//! Graph analysis and advice module.
//!
//! Static analysis to detect issues and suggest improvements.

use std::collections::{HashMap, HashSet};
use serde::{Deserialize, Serialize};
use crate::graph::{Graph, Node, NodeStatus};
use crate::code_graph::{CodeGraph, NodeKind, EdgeRelation};
use crate::query::QueryEngine;
use crate::validator::Validator;

/// Severity level for advice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Info => write!(f, "info"),
            Severity::Warning => write!(f, "warning"),
            Severity::Error => write!(f, "error"),
        }
    }
}

/// Type of advice/issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdviceType {
    CircularDependency,
    OrphanNode,
    HighFanIn,
    HighFanOut,
    MissingDescription,
    LayerViolation,
    DeepDependencyChain,
    MissingRef,
    DuplicateNode,
    SuggestedTaskOrder,
    UnreachableTask,
    BlockedChain,
    DeadCode,
}

impl std::fmt::Display for AdviceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdviceType::CircularDependency => write!(f, "circular-dependency"),
            AdviceType::OrphanNode => write!(f, "orphan-node"),
            AdviceType::HighFanIn => write!(f, "high-fan-in"),
            AdviceType::HighFanOut => write!(f, "high-fan-out"),
            AdviceType::MissingDescription => write!(f, "missing-description"),
            AdviceType::LayerViolation => write!(f, "layer-violation"),
            AdviceType::DeepDependencyChain => write!(f, "deep-dependency-chain"),
            AdviceType::MissingRef => write!(f, "missing-reference"),
            AdviceType::DuplicateNode => write!(f, "duplicate-node"),
            AdviceType::SuggestedTaskOrder => write!(f, "suggested-task-order"),
            AdviceType::UnreachableTask => write!(f, "unreachable-task"),
            AdviceType::BlockedChain => write!(f, "blocked-chain"),
            AdviceType::DeadCode => write!(f, "dead-code"),
        }
    }
}

/// A single piece of advice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Advice {
    /// Type of issue
    pub advice_type: AdviceType,
    /// Severity level
    pub severity: Severity,
    /// Human-readable description
    pub message: String,
    /// Affected node IDs (if any)
    pub nodes: Vec<String>,
    /// Suggested fix (if applicable)
    pub suggestion: Option<String>,
}

impl std::fmt::Display for Advice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let icon = match self.severity {
            Severity::Error => "❌",
            Severity::Warning => "⚠️ ",
            Severity::Info => "ℹ️ ",
        };
        
        write!(f, "{} [{}] {}", icon, self.advice_type, self.message)?;
        
        if !self.nodes.is_empty() {
            write!(f, "\n   📍 Nodes: {}", self.nodes.join(", "))?;
        }
        
        if let Some(ref suggestion) = self.suggestion {
            write!(f, "\n   💡 {}", suggestion)?;
        }
        
        Ok(())
    }
}

/// Analysis result with all advice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisResult {
    /// All advice items
    pub items: Vec<Advice>,
    /// Health score (0-100)
    pub health_score: u8,
    /// Whether the graph passes basic validation
    pub passed: bool,
}

impl AnalysisResult {
    pub fn errors(&self) -> Vec<&Advice> {
        self.items.iter().filter(|a| a.severity == Severity::Error).collect()
    }
    
    pub fn warnings(&self) -> Vec<&Advice> {
        self.items.iter().filter(|a| a.severity == Severity::Warning).collect()
    }
    
    pub fn info(&self) -> Vec<&Advice> {
        self.items.iter().filter(|a| a.severity == Severity::Info).collect()
    }
}

impl std::fmt::Display for AnalysisResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.items.is_empty() {
            return write!(f, "✅ Graph is healthy! Score: {}/100", self.health_score);
        }
        
        writeln!(f, "📊 Analysis Result")?;
        writeln!(f, "═══════════════════════════════════════════════════")?;
        writeln!(f)?;
        
        for item in &self.items {
            writeln!(f, "{}", item)?;
            writeln!(f)?;
        }
        
        writeln!(f, "─────────────────────────────────────────────────────")?;
        writeln!(f, "Summary: {} errors, {} warnings, {} info",
            self.errors().len(),
            self.warnings().len(),
            self.info().len()
        )?;
        write!(f, "Health Score: {}/100", self.health_score)?;
        
        Ok(())
    }
}

/// Analyze a graph and return advice.
pub fn analyze(graph: &Graph) -> AnalysisResult {
    let mut items = Vec::new();
    
    // Code node types — auto-extracted, different rules than project nodes
    let code_node_types = ["file", "class", "function", "module"];
    
    // Run validator first
    let validator = Validator::new(graph);
    let validation = validator.validate();
    
    // Convert validation issues to advice
    
    // Cycles
    for cycle in &validation.cycles {
        items.push(Advice {
            advice_type: AdviceType::CircularDependency,
            severity: Severity::Error,
            message: format!("Circular dependency detected: {}", cycle.join(" → ")),
            nodes: cycle.clone(),
            suggestion: Some("Break the cycle by removing one of the dependencies.".to_string()),
        });
    }
    
    // Missing references
    for missing in &validation.missing_refs {
        items.push(Advice {
            advice_type: AdviceType::MissingRef,
            severity: Severity::Error,
            message: format!("Edge references non-existent node '{}'", missing.missing_node),
            nodes: vec![missing.edge_from.clone(), missing.edge_to.clone()],
            suggestion: Some(format!("Add node '{}' or remove the edge.", missing.missing_node)),
        });
    }
    
    // Duplicate nodes
    for dup in &validation.duplicate_nodes {
        items.push(Advice {
            advice_type: AdviceType::DuplicateNode,
            severity: Severity::Error,
            message: format!("Duplicate node ID: {}", dup),
            nodes: vec![dup.clone()],
            suggestion: Some("Rename or remove duplicate nodes.".to_string()),
        });
    }
    
    // Orphan nodes — only warn for project-level nodes, not code nodes
    for orphan in &validation.orphan_nodes {
        let is_code_orphan = orphan.starts_with("code_") 
            || orphan.starts_with("const_") 
            || orphan.starts_with("method_")
            || graph.get_node(orphan)
                .and_then(|n| n.node_type.as_deref())
                .map(|t| code_node_types.contains(&t))
                .unwrap_or(false);
        
        if !is_code_orphan {
            items.push(Advice {
                advice_type: AdviceType::OrphanNode,
                severity: Severity::Warning,
                message: format!("Node '{}' has no connections", orphan),
                nodes: vec![orphan.clone()],
                suggestion: Some("Connect to related nodes or remove if unused.".to_string()),
            });
        }
    }
    
    // Additional analysis
    
    // High fan-in/fan-out analysis — only for project-level nodes
    // Code-level coupling (imports, calls, defined_in) is structural and expected
    let (fan_in, fan_out) = compute_fan_metrics(graph);
    const HIGH_FAN_THRESHOLD: usize = 5;
    
    for (node_id, count) in &fan_in {
        if *count >= HIGH_FAN_THRESHOLD {
            let is_code = node_id.starts_with("code_") || node_id.starts_with("const_");
            if !is_code {
                items.push(Advice {
                    advice_type: AdviceType::HighFanIn,
                    severity: Severity::Warning,
                    message: format!("Node '{}' has {} dependents (high coupling)", node_id, count),
                    nodes: vec![node_id.clone()],
                    suggestion: Some("Consider splitting into smaller components or introducing an abstraction layer.".to_string()),
                });
            }
        }
    }
    
    for (node_id, count) in &fan_out {
        if *count >= HIGH_FAN_THRESHOLD {
            let is_code = node_id.starts_with("code_") || node_id.starts_with("const_");
            if !is_code {
                items.push(Advice {
                    advice_type: AdviceType::HighFanOut,
                    severity: Severity::Warning,
                    message: format!("Node '{}' depends on {} other nodes (high coupling)", node_id, count),
                    nodes: vec![node_id.clone()],
                    suggestion: Some("Consider reducing dependencies or introducing a facade.".to_string()),
                });
            }
        }
    }
    
    // Missing descriptions — only for project-level nodes (task, component, feature)
    // Code nodes (file, class, function, module) are auto-extracted and don't need descriptions
    for node in &graph.nodes {
        let is_code_node = node.node_type.as_deref()
            .map(|t| code_node_types.contains(&t))
            .unwrap_or(false)
            || node.id.starts_with("code_")
            || node.id.starts_with("const_")
            || node.id.starts_with("method_");
        
        if node.description.is_none() && !is_code_node {
            items.push(Advice {
                advice_type: AdviceType::MissingDescription,
                severity: Severity::Info,
                message: format!("Node '{}' has no description", node.id),
                nodes: vec![node.id.clone()],
                suggestion: Some("Add a description to improve documentation.".to_string()),
            });
        }
    }
    
    // Deep dependency chains
    let chain_depths = compute_chain_depths(graph);
    const DEEP_CHAIN_THRESHOLD: usize = 5;
    
    for (node_id, depth) in &chain_depths {
        if *depth >= DEEP_CHAIN_THRESHOLD {
            items.push(Advice {
                advice_type: AdviceType::DeepDependencyChain,
                severity: Severity::Info,
                message: format!("Node '{}' has dependency chain depth of {}", node_id, depth),
                nodes: vec![node_id.clone()],
                suggestion: Some("Consider flattening the dependency structure.".to_string()),
            });
        }
    }
    
    // Layer violation detection
    let layer_violations = detect_layer_violations(graph);
    for (from, to, from_layer, to_layer) in layer_violations {
        items.push(Advice {
            advice_type: AdviceType::LayerViolation,
            severity: Severity::Warning,
            message: format!(
                "Layer violation: '{}' ({}) depends on '{}' ({})",
                from, 
                from_layer.as_deref().unwrap_or("unassigned"), 
                to, 
                to_layer.as_deref().unwrap_or("unassigned")
            ),
            nodes: vec![from.clone(), to.clone()],
            suggestion: Some("Ensure dependencies flow from higher to lower layers.".to_string()),
        });
    }
    
    // Blocked chain detection
    let blocked_chains = detect_blocked_chains(graph);
    for (blocked_node, affected) in blocked_chains {
        if !affected.is_empty() {
            items.push(Advice {
                advice_type: AdviceType::BlockedChain,
                severity: Severity::Warning,
                message: format!(
                    "Blocked node '{}' is blocking {} other tasks",
                    blocked_node, affected.len()
                ),
                nodes: std::iter::once(blocked_node).chain(affected).collect(),
                suggestion: Some("Unblock this task to enable dependent work.".to_string()),
            });
        }
    }
    
    // Suggest task order
    let engine = QueryEngine::new(graph);
    if let Ok(topo_order) = engine.topological_sort() {
        // Only show if there are todo tasks
        let todo_tasks: Vec<&String> = topo_order.iter()
            .filter(|id| {
                graph.get_node(id)
                    .map(|n| n.status == NodeStatus::Todo)
                    .unwrap_or(false)
            })
            .collect();
        
        if todo_tasks.len() > 1 {
            items.push(Advice {
                advice_type: AdviceType::SuggestedTaskOrder,
                severity: Severity::Info,
                message: format!("Suggested order for {} todo tasks based on dependencies", todo_tasks.len()),
                nodes: todo_tasks.iter().take(10).map(|s| s.to_string()).collect(),
                suggestion: Some(format!(
                    "Start with: {}",
                    todo_tasks.iter().take(3).map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                )),
            });
        }
    }
    
    // Dead code detection for code nodes in the unified graph
    let dead_code_items = detect_dead_code(graph);
    items.extend(dead_code_items);
    
    // Sort by severity (errors first)
    items.sort_by(|a, b| b.severity.cmp(&a.severity));
    
    // Calculate health score based on severity
    // NOTE: Dead code (Info) does NOT count towards deductions — it's purely informational
    let error_count = items.iter().filter(|a| a.severity == Severity::Error).count();
    let warning_count = items.iter().filter(|a| a.severity == Severity::Warning).count();
    let info_count = items.iter()
        .filter(|a| a.severity == Severity::Info && a.advice_type != AdviceType::DeadCode)
        .count();
    
    // Scoring: errors are critical, warnings matter, info is advisory
    // Cap deductions so a few info items don't tank the score
    let mut score = 100i32;
    score -= (error_count * 25) as i32;          // -25 per error (critical)
    score -= (warning_count * 10) as i32;        // -10 per warning (significant)
    score -= (info_count.min(10) * 2) as i32;    // -2 per info, max -20 (advisory, capped)
    let health_score = score.max(0).min(100) as u8;
    
    AnalysisResult {
        items,
        health_score,
        passed: validation.is_valid(),
    }
}

/// Compute fan-in and fan-out for each node.
fn compute_fan_metrics(graph: &Graph) -> (HashMap<String, usize>, HashMap<String, usize>) {
    let mut fan_in: HashMap<String, usize> = HashMap::new();
    let mut fan_out: HashMap<String, usize> = HashMap::new();
    
    for edge in &graph.edges {
        if edge.relation == "depends_on" {
            *fan_in.entry(edge.to.clone()).or_default() += 1;
            *fan_out.entry(edge.from.clone()).or_default() += 1;
        }
    }
    
    (fan_in, fan_out)
}

/// Compute maximum dependency chain depth for each node.
fn compute_chain_depths(graph: &Graph) -> HashMap<String, usize> {
    let mut depths: HashMap<String, usize> = HashMap::new();
    
    // Build adjacency list with owned strings
    let mut deps: HashMap<String, Vec<String>> = HashMap::new();
    for edge in &graph.edges {
        if edge.relation == "depends_on" {
            deps.entry(edge.from.clone()).or_default().push(edge.to.clone());
        }
    }
    
    fn compute_depth(
        node: &str,
        deps: &HashMap<String, Vec<String>>,
        cache: &mut HashMap<String, usize>,
        visiting: &mut HashSet<String>,
    ) -> usize {
        if let Some(&depth) = cache.get(node) {
            return depth;
        }
        
        if visiting.contains(node) {
            return 0; // Cycle, avoid infinite recursion
        }
        
        visiting.insert(node.to_string());
        
        let depth = deps.get(node)
            .map(|children| {
                children.iter()
                    .map(|child| compute_depth(child, deps, cache, visiting) + 1)
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        
        visiting.remove(node);
        cache.insert(node.to_string(), depth);
        depth
    }
    
    let mut visiting = HashSet::new();
    for node in &graph.nodes {
        compute_depth(&node.id, &deps, &mut depths, &mut visiting);
    }
    
    depths
}

/// Detect layer violations (lower layer depending on higher layer).
fn detect_layer_violations(graph: &Graph) -> Vec<(String, String, Option<String>, Option<String>)> {
    // Layer hierarchy (higher number = higher layer)
    fn layer_rank(layer: Option<&str>) -> Option<i32> {
        match layer {
            Some("interface") | Some("presentation") => Some(4),
            Some("application") | Some("service") => Some(3),
            Some("domain") | Some("business") => Some(2),
            Some("infrastructure") | Some("data") => Some(1),
            _ => None,
        }
    }
    
    let mut violations = Vec::new();
    
    // Build node layer map
    let node_layers: HashMap<&str, Option<&str>> = graph.nodes.iter()
        .map(|n| (n.id.as_str(), n.node_type.as_deref()))
        .collect();
    
    // Also check for explicit layer metadata
    let node_explicit_layers: HashMap<&str, Option<String>> = graph.nodes.iter()
        .map(|n| {
            let layer = n.metadata.get("layer")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            (n.id.as_str(), layer)
        })
        .collect();
    
    for edge in &graph.edges {
        if edge.relation == "depends_on" {
            let from_layer = node_explicit_layers.get(edge.from.as_str())
                .and_then(|l| l.as_ref())
                .map(|s| s.as_str())
                .or_else(|| node_layers.get(edge.from.as_str()).copied().flatten());
            
            let to_layer = node_explicit_layers.get(edge.to.as_str())
                .and_then(|l| l.as_ref())
                .map(|s| s.as_str())
                .or_else(|| node_layers.get(edge.to.as_str()).copied().flatten());
            
            if let (Some(from_rank), Some(to_rank)) = (layer_rank(from_layer), layer_rank(to_layer)) {
                // Violation: lower layer depends on higher layer
                if from_rank < to_rank {
                    violations.push((
                        edge.from.clone(),
                        edge.to.clone(),
                        from_layer.map(|s| s.to_string()),
                        to_layer.map(|s| s.to_string()),
                    ));
                }
            }
        }
    }
    
    violations
}

/// Detect blocked nodes that are blocking other tasks.
fn detect_blocked_chains(graph: &Graph) -> Vec<(String, Vec<String>)> {
    let engine = QueryEngine::new(graph);
    let mut results = Vec::new();
    
    // Find blocked nodes
    let blocked: Vec<&Node> = graph.nodes.iter()
        .filter(|n| n.status == NodeStatus::Blocked)
        .collect();
    
    for node in blocked {
        // Find all nodes that depend on this blocked node (reverse impact)
        let affected: Vec<String> = engine.impact(&node.id)
            .iter()
            .filter(|n| n.status == NodeStatus::Todo || n.status == NodeStatus::InProgress)
            .map(|n| n.id.clone())
            .collect();
        
        if !affected.is_empty() {
            results.push((node.id.clone(), affected));
        }
    }
    
    results
}

/// Detect dead code (functions with 0 incoming calls that are not entry points).
/// Works on the unified Graph which contains code nodes from CodeGraph.
fn detect_dead_code(graph: &Graph) -> Vec<Advice> {
    let mut items = Vec::new();
    
    // Only proceed if graph has code nodes (function type)
    let code_functions: Vec<&Node> = graph.nodes.iter()
        .filter(|n| n.node_type.as_deref() == Some("function"))
        .collect();
    
    if code_functions.is_empty() {
        return items;
    }
    
    // Build incoming calls map
    let mut incoming_calls: HashMap<&str, usize> = HashMap::new();
    for edge in &graph.edges {
        if edge.relation == "calls" {
            *incoming_calls.entry(&edge.to).or_default() += 1;
        }
    }
    
    // Find functions with 0 incoming calls that are not entry points
    let dead_functions: Vec<&Node> = code_functions
        .into_iter()
        .filter(|node| {
            // Skip if has incoming calls
            if incoming_calls.get(node.id.as_str()).copied().unwrap_or(0) > 0 {
                return false;
            }
            
            // Skip entry points
            if is_code_entry_point(node) {
                return false;
            }
            
            // Skip test functions (check metadata or title pattern)
            if is_test_function(node) {
                return false;
            }
            
            // Skip public API
            if is_public_code(node) {
                return false;
            }
            
            // Skip Python dunder methods
            if is_dunder(&node.title) {
                return false;
            }
            
            // Skip trait implementation methods (called via dynamic dispatch)
            if is_trait_impl_method(node, graph) {
                return false;
            }
            
            // Skip serde default functions
            if is_serde_default(node) {
                return false;
            }
            
            true
        })
        .collect();
    
    if dead_functions.is_empty() {
        return items;
    }
    
    // Group by file for better reporting
    let mut by_file: HashMap<&str, Vec<&str>> = HashMap::new();
    for node in &dead_functions {
        let file_path = node.metadata.get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        by_file.entry(file_path).or_default().push(&node.title);
    }
    
    for (file_path, names) in by_file {
        // Report up to 10 dead functions per file
        let names_to_report: Vec<&str> = names.iter().take(10).copied().collect();
        let remaining = names.len().saturating_sub(10);
        
        let message = if remaining > 0 {
            format!(
                "{} has {} potentially dead functions: {} (and {} more)",
                file_path,
                names.len(),
                names_to_report.join(", "),
                remaining
            )
        } else {
            format!(
                "{} has {} potentially dead function(s): {}",
                file_path,
                names.len(),
                names_to_report.join(", ")
            )
        };
        
        items.push(Advice {
            advice_type: AdviceType::DeadCode,
            severity: Severity::Info,
            message,
            nodes: dead_functions.iter()
                .filter(|n| {
                    n.metadata.get("file_path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("") == file_path
                })
                .map(|n| n.id.clone())
                .collect(),
            suggestion: Some("Consider removing unused code or exposing it if intentionally unused.".to_string()),
        });
    }
    
    items
}

/// Check if a code node is an entry point
fn is_code_entry_point(node: &Node) -> bool {
    let name = &node.title;
    let file_path = node.metadata.get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    
    // Common entry points
    if matches!(name.as_str(), "main" | "lib" | "mod" | "index" | "app" | "run" | "start" | "init" | "setup") {
        return true;
    }
    
    // Rust: functions in main.rs or lib.rs
    if file_path.ends_with("main.rs") || file_path.ends_with("lib.rs") {
        return true;
    }
    
    // TypeScript/JavaScript: common entry files
    if file_path.ends_with("index.ts") 
        || file_path.ends_with("index.js")
        || file_path.ends_with("main.ts")
        || file_path.ends_with("main.js")
    {
        return true;
    }
    
    // Python: __main__ entry
    if name == "__main__" || file_path.ends_with("__main__.py") {
        return true;
    }
    
    // CLI command handlers
    if name.starts_with("cmd_") || name.starts_with("command_") || name.starts_with("handle_") {
        return true;
    }
    
    // Check signature for FFI markers
    if let Some(sig) = node.metadata.get("signature").and_then(|v| v.as_str()) {
        if sig.contains("#[no_mangle]") || sig.contains("extern") {
            return true;
        }
    }
    
    false
}

/// Check if a code node is a test function
fn is_test_function(node: &Node) -> bool {
    let name = &node.title;
    let file_path = node.metadata.get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    
    // Test file patterns
    if file_path.contains("/test") || file_path.contains("_test.") || file_path.contains(".test.") || file_path.contains(".spec.") {
        return true;
    }
    
    // Test function name patterns
    if name.starts_with("test_") || name.starts_with("Test") {
        return true;
    }
    
    // Node in a tests module (Rust pattern)
    if node.id.contains("tests__") || node.id.contains("_tests_") {
        return true;
    }
    
    // Check signature for test attributes
    if let Some(sig) = node.metadata.get("signature").and_then(|v| v.as_str()) {
        if sig.contains("#[test]") || sig.contains("#[tokio::test]") {
            return true;
        }
    }
    
    false
}

/// Check if a code node is public API
fn is_public_code(node: &Node) -> bool {
    // Check signature for pub (Rust)
    if let Some(sig) = node.metadata.get("signature").and_then(|v| v.as_str()) {
        if sig.starts_with("pub ") || sig.starts_with("pub(") {
            return true;
        }
        // TypeScript export
        if sig.starts_with("export ") {
            return true;
        }
    }
    
    false
}

/// Check if a code node is a trait implementation method (called via dynamic dispatch)
fn is_trait_impl_method(node: &Node, graph: &Graph) -> bool {
    // Common trait method names that are called dynamically
    let common_trait_methods = [
        // Rust standard traits
        "fmt", "clone", "default", "eq", "ne", "hash", "cmp", "partial_cmp",
        "drop", "deref", "deref_mut", "index", "index_mut",
        "add", "sub", "mul", "div", "rem", "neg", "not",
        "from", "into", "try_from", "try_into", "as_ref", "as_mut",
        "borrow", "borrow_mut", "to_owned", "to_string",
        // Iterator
        "next", "size_hint", "count", "last", "nth", "fold", "collect",
        // Custom plugin/tool traits
        "name", "description", "execute", "input_schema", "version",
        "on_load", "on_unload", "tools", "hooks", "point", "priority",
        // Serde
        "serialize", "deserialize",
        // Async traits
        "poll", "wake",
    ];
    
    let name = node.title.as_str();
    if common_trait_methods.contains(&name) {
        // Check if this method is in a struct/impl block (has DefinedIn edge to a class)
        let has_parent = graph.edges.iter()
            .any(|e| e.from == node.id && e.relation == "defined_in");
        return has_parent;
    }
    
    false
}

/// Check if a code node is a serde default function
fn is_serde_default(node: &Node) -> bool {
    let name = &node.title;
    // Serde default functions follow the pattern default_* 
    if name.starts_with("default_") {
        return true;
    }
    false
}

/// Check if name is a Python dunder method
fn is_dunder(name: &str) -> bool {
    name.starts_with("__") && name.ends_with("__")
}

// ═══ Code Graph Analysis ═══

/// Analyze a code graph for dead code and return advice.
/// Dead code = functions/methods with 0 incoming Calls edges that are not entry points.
pub fn analyze_code_graph(code_graph: &CodeGraph) -> Vec<Advice> {
    let mut items = Vec::new();
    
    // Build incoming calls map
    let mut incoming_calls: HashMap<&str, usize> = HashMap::new();
    for edge in &code_graph.edges {
        if edge.relation == EdgeRelation::Calls {
            *incoming_calls.entry(&edge.to).or_default() += 1;
        }
    }
    
    // Find function/method nodes with 0 incoming calls
    let dead_code: Vec<&crate::code_graph::CodeNode> = code_graph.nodes
        .iter()
        .filter(|node| {
            // Only check functions/methods
            if node.kind != NodeKind::Function {
                return false;
            }
            
            // Skip if has incoming calls
            if incoming_calls.get(node.id.as_str()).copied().unwrap_or(0) > 0 {
                return false;
            }
            
            // Skip entry points
            if is_entry_point(node) {
                return false;
            }
            
            // Skip test functions
            if node.is_test {
                return false;
            }
            
            // Skip public API (Rust: pub, TypeScript: export)
            if is_public_api(node) {
                return false;
            }
            
            // Skip Python dunder methods
            if is_dunder_method(&node.name) {
                return false;
            }
            
            // Skip trait implementations (Rust)
            if is_trait_impl(node, code_graph) {
                return false;
            }
            
            true
        })
        .collect();
    
    // Group by file for better reporting
    let mut by_file: HashMap<&str, Vec<&str>> = HashMap::new();
    for node in &dead_code {
        by_file.entry(&node.file_path).or_default().push(&node.name);
    }
    
    for (file_path, names) in by_file {
        // Report up to 10 dead functions per file
        let names_to_report: Vec<&str> = names.iter().take(10).copied().collect();
        let remaining = names.len().saturating_sub(10);
        
        let message = if remaining > 0 {
            format!(
                "{} has {} potentially dead functions: {} (and {} more)",
                file_path,
                names.len(),
                names_to_report.join(", "),
                remaining
            )
        } else {
            format!(
                "{} has {} potentially dead function(s): {}",
                file_path,
                names.len(),
                names_to_report.join(", ")
            )
        };
        
        items.push(Advice {
            advice_type: AdviceType::DeadCode,
            severity: Severity::Info,
            message,
            nodes: dead_code.iter()
                .filter(|n| n.file_path == file_path)
                .map(|n| n.id.clone())
                .collect(),
            suggestion: Some("Consider removing unused code or exposing it if intentionally unused.".to_string()),
        });
    }
    
    items
}

/// Check if a node is an entry point (main, lib, etc.)
fn is_entry_point(node: &crate::code_graph::CodeNode) -> bool {
    let name = &node.name;
    
    // Common entry points
    if matches!(name.as_str(), "main" | "lib" | "mod" | "index" | "app" | "run" | "start" | "init" | "setup") {
        return true;
    }
    
    // Rust: functions in main.rs or lib.rs at root
    if node.file_path.ends_with("main.rs") || node.file_path.ends_with("lib.rs") {
        if name == "main" || name.starts_with("pub ") {
            return true;
        }
    }
    
    // TypeScript/JavaScript: common entry files
    if node.file_path.ends_with("index.ts") 
        || node.file_path.ends_with("index.js")
        || node.file_path.ends_with("main.ts")
        || node.file_path.ends_with("main.js")
        || node.file_path.ends_with("app.ts")
        || node.file_path.ends_with("app.js")
    {
        return true;
    }
    
    // Python: __main__ entry
    if name == "__main__" || node.file_path.ends_with("__main__.py") {
        return true;
    }
    
    // CLI command handlers
    if name.starts_with("cmd_") || name.starts_with("command_") || name.starts_with("handle_") {
        return true;
    }
    
    // FFI/no_mangle functions (Rust)
    if node.decorators.iter().any(|d| d.contains("no_mangle") || d.contains("export_name")) {
        return true;
    }
    
    false
}

/// Check if a node is public API
fn is_public_api(node: &crate::code_graph::CodeNode) -> bool {
    // Check signature for pub (Rust)
    if let Some(ref sig) = node.signature {
        if sig.starts_with("pub ") || sig.starts_with("pub(") {
            return true;
        }
    }
    
    // Check decorators for export (TypeScript)
    if node.decorators.iter().any(|d| d == "export" || d.contains("Export")) {
        return true;
    }
    
    // Check if method ID suggests it's in a public trait/interface
    if node.id.starts_with("method:") {
        // Methods in impl blocks for traits are considered public
        // (handled separately in is_trait_impl)
    }
    
    // Python: functions starting with single underscore are private convention
    // Functions without underscore are considered public
    if node.file_path.ends_with(".py") && !node.name.starts_with('_') {
        // But only if it's a top-level function (not method)
        if node.id.starts_with("func:") {
            return true;
        }
    }
    
    false
}

/// Check if name is a Python dunder method
fn is_dunder_method(name: &str) -> bool {
    name.starts_with("__") && name.ends_with("__")
}

/// Check if node is a trait implementation method (Rust)
fn is_trait_impl(node: &crate::code_graph::CodeNode, code_graph: &CodeGraph) -> bool {
    // A method is a trait impl if its parent class/struct has an Inherits edge to a trait
    
    // Find the parent class/struct from DefinedIn edge
    let parent_id = code_graph.edges.iter()
        .find(|e| e.from == node.id && e.relation == EdgeRelation::DefinedIn)
        .map(|e| &e.to);
    
    if let Some(parent) = parent_id {
        // Check if parent has Inherits edges (trait implementation)
        let has_trait_impl = code_graph.edges.iter()
            .any(|e| &e.from == parent && e.relation == EdgeRelation::Inherits);
        
        if has_trait_impl {
            return true;
        }
    }
    
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Node, Edge};
    
    #[test]
    fn test_analyze_empty_graph() {
        let graph = Graph::new();
        let result = analyze(&graph);
        assert!(result.passed);
        assert_eq!(result.health_score, 100);
    }
    
    #[test]
    fn test_analyze_orphan_node() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("orphan", "Orphan Node"));
        
        let result = analyze(&graph);
        assert!(result.items.iter().any(|a| a.advice_type == AdviceType::OrphanNode));
    }
    
    #[test]
    fn test_analyze_cycle() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("a", "A"));
        graph.add_node(Node::new("b", "B"));
        graph.add_edge(Edge::depends_on("a", "b"));
        graph.add_edge(Edge::depends_on("b", "a"));
        
        let result = analyze(&graph);
        assert!(!result.passed);
        assert!(result.items.iter().any(|a| a.advice_type == AdviceType::CircularDependency));
    }
    
    #[test]
    fn test_analyze_high_coupling() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("hub", "Hub Node"));
        for i in 0..6 {
            let id = format!("dep{}", i);
            graph.add_node(Node::new(&id, &format!("Dep {}", i)));
            graph.add_edge(Edge::depends_on(&id, "hub"));
        }
        
        let result = analyze(&graph);
        assert!(result.items.iter().any(|a| a.advice_type == AdviceType::HighFanIn));
    }
}
