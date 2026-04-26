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
    ModuleSuggestion,
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
            AdviceType::ModuleSuggestion => write!(f, "module-suggestion"),
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
    
    // Module grouping suggestion via Infomap community detection
    #[cfg(feature = "infomap")]
    {
        let module_items = detect_modules(graph);
        items.extend(module_items);
    }
    
    // Sort by severity (errors first)
    items.sort_by(|a, b| b.severity.cmp(&a.severity));
    
    // Calculate health score based on severity
    // NOTE: Dead code (Info) does NOT count towards deductions — it's purely informational
    let error_count = items.iter().filter(|a| a.severity == Severity::Error).count();
    let warning_count = items.iter().filter(|a| a.severity == Severity::Warning).count();
    let info_count = items.iter()
        .filter(|a| a.severity == Severity::Info
            && a.advice_type != AdviceType::DeadCode
            && a.advice_type != AdviceType::ModuleSuggestion)
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
            
            // Skip trait definition methods (they define the interface, not called directly)
            if is_trait_definition_method(node, graph) {
                return false;
            }
            
            // Skip methods in structs that have ANY trait impl
            // (dynamic dispatch means any method could be called via trait object)
            if is_method_in_trait_implementing_struct(node, graph) {
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
    
    // CLI command handlers and framework patterns
    if name.starts_with("cmd_") || name.starts_with("command_") || name.starts_with("handle_") {
        return true;
    }
    
    // Web framework route handlers (axum, actix, rocket, express)
    if name.starts_with("get_") || name.starts_with("post_") || name.starts_with("put_") 
        || name.starts_with("delete_") || name.starts_with("patch_") {
        return true;
    }
    
    // Common callback/hook/middleware patterns
    if name.ends_with("_handler") || name.ends_with("_callback") || name.ends_with("_hook")
        || name.ends_with("_middleware") || name.ends_with("_listener") {
        return true;
    }
    
    // Serenity/Discord event handlers
    if matches!(name.as_str(), "ready" | "message" | "interaction_create" | "guild_member_addition") {
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
    // Check 1: Is this node the target of an "overrides" edge?
    // (trait_method --overrides--> impl_method means impl_method is a trait impl)
    let is_override_target = graph.edges.iter()
        .any(|e| e.relation == "overrides" && e.to == node.id);
    if is_override_target {
        return true;
    }
    
    // Check 2: Is this method defined in a class/struct that implements a trait?
    // (struct --inherits--> trait means all methods in struct could be trait impls)
    let parent_id = graph.edges.iter()
        .find(|e| e.from == node.id && e.relation == "defined_in")
        .map(|e| &e.to);
    
    if let Some(parent) = parent_id {
        let parent_has_trait = graph.edges.iter()
            .any(|e| e.from == *parent && e.relation == "inherits");
        if parent_has_trait {
            return true;
        }
    }
    
    // Check 3: Common trait method names as fallback
    let common_trait_methods = [
        // Rust standard traits
        "fmt", "clone", "default", "eq", "ne", "hash", "cmp", "partial_cmp",
        "drop", "deref", "deref_mut", "from", "into", "try_from", "try_into",
        "as_ref", "as_mut", "to_owned", "to_string",
        // Iterator
        "next", "size_hint",
        // Serde
        "serialize", "deserialize",
        // Async
        "poll", "wake",
    ];
    
    if common_trait_methods.contains(&node.title.as_str()) {
        let has_parent = graph.edges.iter()
            .any(|e| e.from == node.id && e.relation == "defined_in");
        if has_parent {
            return true;
        }
    }
    
    false
}

/// Check if a code node is a method defined inside a trait (trait definition, not impl)
fn is_trait_definition_method(node: &Node, graph: &Graph) -> bool {
    // Find the parent via defined_in edge
    let parent_id = graph.edges.iter()
        .find(|e| e.from == node.id && e.relation == "defined_in")
        .map(|e| &e.to);
    
    if let Some(parent) = parent_id {
        // Check parent node's signature for "trait" keyword
        if let Some(parent_node) = graph.get_node(parent) {
            if let Some(sig) = parent_node.metadata.get("signature").and_then(|v| v.as_str()) {
                if sig.contains("trait ") {
                    return true;
                }
            }
        }
        
        // Check if parent is a trait (has nodes that inherit FROM it)
        let is_trait = graph.edges.iter()
            .any(|e| e.to == *parent && e.relation == "inherits");
        if is_trait {
            return true;
        }
        
        // Also check overrides: if any overrides edge targets methods of this parent
        let is_overrides_source = graph.edges.iter()
            .any(|e| e.relation == "overrides" && e.from.starts_with(&format!("{}.", parent.rsplit('_').next().unwrap_or(""))));
        if is_overrides_source {
            return true;
        }
    }
    
    false
}

/// Check if a method belongs to a struct that implements any trait
/// (methods could be called via dynamic dispatch even if we can't see the call)
fn is_method_in_trait_implementing_struct(node: &Node, graph: &Graph) -> bool {
    // Only applies to methods (defined_in a class)
    let parent_id = graph.edges.iter()
        .find(|e| e.from == node.id && e.relation == "defined_in")
        .map(|e| e.to.clone());
    
    if let Some(parent) = parent_id {
        // Check if this parent has any inherits edge (implements a trait)
        let has_trait = graph.edges.iter()
            .any(|e| e.from == parent && e.relation == "inherits");
        if has_trait {
            return true;
        }
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
    if (node.file_path.ends_with("main.rs") || node.file_path.ends_with("lib.rs"))
        && (name == "main" || name.starts_with("pub ")) {
            return true;
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
    
    // CLI command handlers and framework patterns
    if name.starts_with("cmd_") || name.starts_with("command_") || name.starts_with("handle_") {
        return true;
    }
    
    // Web framework route handlers
    if name.starts_with("get_") || name.starts_with("post_") || name.starts_with("put_") 
        || name.starts_with("delete_") || name.starts_with("patch_") {
        return true;
    }
    
    // Common callback/hook/middleware patterns
    if name.ends_with("_handler") || name.ends_with("_callback") || name.ends_with("_hook")
        || name.ends_with("_middleware") || name.ends_with("_listener") {
        return true;
    }
    
    // Serenity/Discord event handlers
    if matches!(name.as_str(), "ready" | "message" | "interaction_create" | "guild_member_addition") {
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

/// Detect module groupings in the code dependency graph using Infomap community detection.
///
/// Converts code-layer nodes and their edges (calls, imports, defined_in) into an
/// Infomap Network, runs community detection, and suggests module groupings where
/// files in the same community should be co-located.
///
/// Only produces suggestions when:
/// - There are at least 4 code nodes (below this, results are trivial)
/// - Infomap finds at least 2 modules
/// - At least one module contains files from different directories (indicating potential reorganization)
#[cfg(feature = "infomap")]
fn detect_modules(graph: &Graph) -> Vec<Advice> {
    use infomap_rs::{Network, Infomap};
    
    let mut items = Vec::new();
    
    // Collect code-layer file nodes — these are the units we want to group
    let code_files: Vec<&Node> = graph.nodes.iter()
        .filter(|n| n.node_type.as_deref() == Some("file"))
        .collect();
    
    if code_files.len() < 4 {
        return items; // Too few files for meaningful community detection
    }
    
    // Build node ID → index mapping for the Infomap network
    let id_to_idx: HashMap<&str, usize> = code_files.iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();
    
    // Also map non-file code nodes (functions, classes, methods) to their parent file
    // so we can translate calls/imports between functions into file-level edges
    let mut node_to_file_idx: HashMap<&str, usize> = HashMap::new();
    for (i, file_node) in code_files.iter().enumerate() {
        node_to_file_idx.insert(file_node.id.as_str(), i);
    }
    // Map function/class/method nodes to their file via file_path metadata or defined_in edges
    for node in &graph.nodes {
        if node.node_type.as_deref() == Some("file") {
            continue;
        }
        // Try to find the file this node belongs to via file_path
        if let Some(fp) = node.file_path.as_deref()
            .or_else(|| node.metadata.get("file_path").and_then(|v| v.as_str()))
        {
            // Look for matching file node by file_path
            let file_id = format!("file:{}", fp);
            if let Some(&idx) = id_to_idx.get(file_id.as_str()) {
                node_to_file_idx.insert(node.id.as_str(), idx);
            }
        }
    }
    
    // Build the Infomap network from code edges
    let mut net = Network::new();
    // Ensure all file nodes exist in the network
    if !code_files.is_empty() {
        net.add_edge(0, 0, 0.0); // no-op (zero weight ignored), but let's use ensure_capacity
    }
    
    // Edge relations that indicate structural coupling between code units
    let coupling_relations = ["calls", "imports", "depends_on", "inherits", "implements"];
    
    for edge in &graph.edges {
        if !coupling_relations.contains(&edge.relation.as_str()) {
            continue;
        }
        
        // Map both endpoints to file-level indices
        let from_idx = node_to_file_idx.get(edge.from.as_str())
            .or_else(|| id_to_idx.get(edge.from.as_str()));
        let to_idx = node_to_file_idx.get(edge.to.as_str())
            .or_else(|| id_to_idx.get(edge.to.as_str()));
        
        if let (Some(&from), Some(&to)) = (from_idx, to_idx) {
            if from != to {
                // Weight: explicit weight if present, else 1.0
                let w = edge.weight.unwrap_or(1.0);
                net.add_edge(from, to, w);
            }
        }
    }
    
    // Need at least some edges for meaningful detection
    if net.num_edges() < 2 {
        return items;
    }
    
    // Add node names for readable output
    for (i, file_node) in code_files.iter().enumerate() {
        let display_name = file_node.file_path.as_deref()
            .unwrap_or(&file_node.title);
        net.add_node_name(i, display_name);
    }
    
    // Run Infomap with sensible defaults for code graphs
    let result = Infomap::new(&net)
        .seed(42)
        .num_trials(5)          // Fewer trials since code graphs are smaller
        .hierarchical(false)    // Flat grouping is most useful for module suggestions
        .run();
    
    if result.num_modules() < 2 {
        return items; // Everything in one module — no reorganization needed
    }
    
    // Analyze each detected module: extract file paths and check if they span directories
    let mut modules_with_files: Vec<(usize, Vec<&str>)> = Vec::new();
    
    for module_info in result.modules() {
        let file_paths: Vec<&str> = module_info.nodes.iter()
            .filter_map(|&node_idx| {
                code_files.get(node_idx)
                    .and_then(|n| n.file_path.as_deref()
                        .or_else(|| n.metadata.get("file_path").and_then(|v| v.as_str())))
            })
            .collect();
        
        if file_paths.len() >= 2 {
            modules_with_files.push((module_info.id, file_paths));
        }
    }
    
    if modules_with_files.is_empty() {
        return items;
    }
    
    // Check for cross-directory modules (files from different dirs in same community)
    // These are the interesting suggestions — files that are tightly coupled but scattered
    for (module_id, files) in &modules_with_files {
        let directories: HashSet<&str> = files.iter()
            .filter_map(|fp| {
                let p = std::path::Path::new(fp);
                p.parent().and_then(|d| d.to_str())
            })
            .collect();
        
        if directories.len() > 1 {
            // Files from multiple directories are tightly coupled — suggest grouping
            let file_list: Vec<String> = files.iter()
                .take(8)
                .map(|f| f.to_string())
                .collect();
            let remaining = files.len().saturating_sub(8);
            
            let dir_list: Vec<&str> = directories.iter().take(5).copied().collect();
            
            let message = if remaining > 0 {
                format!(
                    "Module {} ({} files): tightly coupled files span {} directories ({}). Shown: {} (and {} more)",
                    module_id,
                    files.len(),
                    directories.len(),
                    dir_list.join(", "),
                    file_list.join(", "),
                    remaining
                )
            } else {
                format!(
                    "Module {} ({} files): tightly coupled files span {} directories ({}): {}",
                    module_id,
                    files.len(),
                    directories.len(),
                    dir_list.join(", "),
                    file_list.join(", ")
                )
            };
            
            items.push(Advice {
                advice_type: AdviceType::ModuleSuggestion,
                severity: Severity::Info,
                message,
                nodes: files.iter().map(|f| format!("file:{}", f)).collect(),
                suggestion: Some("Consider co-locating these files into a dedicated module — they form a cohesive unit based on dependency analysis.".to_string()),
            });
        }
    }
    
    // If no cross-directory suggestions, provide a summary of detected modules
    if items.is_empty() && modules_with_files.len() >= 2 {
        let summary_parts: Vec<String> = modules_with_files.iter()
            .take(5)
            .map(|(id, files)| {
                let sample: Vec<&&str> = files.iter().take(3).collect();
                let names: Vec<String> = sample.iter()
                    .filter_map(|fp| std::path::Path::new(fp).file_name()
                        .and_then(|f| f.to_str())
                        .map(String::from))
                    .collect();
                format!("Module {}: {} files ({})", id, files.len(), names.join(", "))
            })
            .collect();
        
        let remaining_modules = modules_with_files.len().saturating_sub(5);
        let mut message = format!(
            "Infomap detected {} code modules (codelength {:.3}): {}",
            result.num_modules(),
            result.codelength(),
            summary_parts.join("; ")
        );
        if remaining_modules > 0 {
            message.push_str(&format!(" (and {} more)", remaining_modules));
        }
        
        items.push(Advice {
            advice_type: AdviceType::ModuleSuggestion,
            severity: Severity::Info,
            message,
            nodes: vec![],
            suggestion: Some(
                "Code modules are well-organized within their directories.".to_string()
            ),
        });
    }
    
    items
}

/// Public API for running Infomap module detection independently.
/// Returns a list of detected modules with their file paths and metadata.
///
/// Delegates to [`crate::infer::clustering::build_network`] for shared
/// network-construction logic, then runs Infomap directly and maps results
/// back to `DetectedModule` structs (preserving the existing public API).
#[cfg(feature = "infomap")]
pub fn detect_code_modules(graph: &Graph) -> Vec<DetectedModule> {
    use infomap_rs::Infomap;
    use crate::infer::clustering::{build_network, ClusterConfig};

    let (net, idx_to_id) = build_network(graph, &ClusterConfig::default());

    if net.num_nodes() < 2 || net.num_edges() < 1 {
        return vec![];
    }

    // Build a reverse lookup: node_id → &Node for file-path resolution.
    let node_map: HashMap<&str, &Node> = graph
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect();

    let result = Infomap::new(&net)
        .seed(42)
        .num_trials(5)
        .hierarchical(false)
        .run();

    result
        .modules()
        .iter()
        .map(|m| {
            let files: Vec<String> = m
                .nodes
                .iter()
                .filter_map(|&idx| idx_to_id.get(idx))
                .filter_map(|nid| node_map.get(nid.as_str()))
                .map(|n| {
                    n.file_path
                        .as_deref()
                        .or_else(|| {
                            n.metadata
                                .get("file_path")
                                .and_then(|v| v.as_str())
                        })
                        .unwrap_or(&n.title)
                        .to_string()
                })
                .collect();
            let node_ids: Vec<String> = m
                .nodes
                .iter()
                .filter_map(|&idx| idx_to_id.get(idx)).cloned()
                .collect();
            DetectedModule {
                id: m.id,
                files,
                node_ids,
                flow: m.flow,
                size: m.num_nodes,
            }
        })
        .collect()
}

/// A detected code module (community) from Infomap analysis.
#[cfg(feature = "infomap")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedModule {
    /// Module ID (0-based)
    pub id: usize,
    /// File paths in this module
    pub files: Vec<String>,
    /// Node IDs in this module
    pub node_ids: Vec<String>,
    /// Flow proportion (higher = more internal traffic)
    pub flow: f64,
    /// Number of files
    pub size: usize,
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
    
    /// Helper to create a file node for testing
    fn make_file_node(path: &str) -> Node {
        let mut node = Node::new(&format!("file:{}", path), path);
        node.node_type = Some("file".to_string());
        node.file_path = Some(path.to_string());
        node
    }
    
    /// Helper to create a function node for testing
    fn make_func_node(path: &str, name: &str) -> Node {
        let id = format!("func:{}:{}", path, name);
        let mut node = Node::new(&id, name);
        node.node_type = Some("function".to_string());
        node.file_path = Some(path.to_string());
        node
    }
    
    #[cfg(feature = "infomap")]
    #[test]
    fn test_detect_modules_too_few_files() {
        // Less than 4 file nodes → no module suggestions
        let mut graph = Graph::new();
        graph.add_node(make_file_node("src/a.rs"));
        graph.add_node(make_file_node("src/b.rs"));
        graph.add_edge(Edge::new("file:src/a.rs", "file:src/b.rs", "imports"));
        
        let result = detect_modules(&graph);
        assert!(result.is_empty(), "Should return nothing for < 4 files");
    }
    
    #[cfg(feature = "infomap")]
    #[test]
    fn test_detect_modules_no_edges() {
        // Files with no coupling edges → no suggestions
        let mut graph = Graph::new();
        for i in 0..6 {
            graph.add_node(make_file_node(&format!("src/file{}.rs", i)));
        }
        
        let result = detect_modules(&graph);
        assert!(result.is_empty(), "Should return nothing with no edges");
    }
    
    #[cfg(feature = "infomap")]
    #[test]
    fn test_detect_modules_two_clusters() {
        // Two clearly separated clusters of files
        let mut graph = Graph::new();
        
        // Cluster 1: auth module (in src/auth/)
        graph.add_node(make_file_node("src/auth/login.rs"));
        graph.add_node(make_file_node("src/auth/token.rs"));
        graph.add_node(make_file_node("src/auth/middleware.rs"));
        
        // Cluster 2: api module (in src/api/)
        graph.add_node(make_file_node("src/api/routes.rs"));
        graph.add_node(make_file_node("src/api/handlers.rs"));
        graph.add_node(make_file_node("src/api/response.rs"));
        
        // Strong coupling within cluster 1
        graph.add_edge(Edge::new("file:src/auth/login.rs", "file:src/auth/token.rs", "imports"));
        graph.add_edge(Edge::new("file:src/auth/token.rs", "file:src/auth/login.rs", "imports"));
        graph.add_edge(Edge::new("file:src/auth/middleware.rs", "file:src/auth/token.rs", "imports"));
        graph.add_edge(Edge::new("file:src/auth/login.rs", "file:src/auth/middleware.rs", "imports"));
        
        // Strong coupling within cluster 2
        graph.add_edge(Edge::new("file:src/api/routes.rs", "file:src/api/handlers.rs", "imports"));
        graph.add_edge(Edge::new("file:src/api/handlers.rs", "file:src/api/routes.rs", "imports"));
        graph.add_edge(Edge::new("file:src/api/response.rs", "file:src/api/handlers.rs", "imports"));
        graph.add_edge(Edge::new("file:src/api/routes.rs", "file:src/api/response.rs", "imports"));
        
        // Weak cross-cluster connection
        graph.add_edge({
            let mut e = Edge::new("file:src/api/handlers.rs", "file:src/auth/middleware.rs", "imports");
            e.weight = Some(0.1);
            e
        });
        
        let _result = detect_modules(&graph);
        // Should detect at least 2 modules — the clusters are well-organized in their dirs
        // so it might not produce cross-dir suggestions, but the public API should find them
        let modules = detect_code_modules(&graph);
        assert!(modules.len() >= 2, "Should detect at least 2 modules, got {}", modules.len());
    }
    
    #[cfg(feature = "infomap")]
    #[test]
    fn test_detect_modules_cross_directory_suggestion() {
        // Files that are tightly coupled but live in DIFFERENT directories
        // → should suggest grouping
        let mut graph = Graph::new();
        
        // Tightly coupled files scattered across dirs
        graph.add_node(make_file_node("src/models/user.rs"));
        graph.add_node(make_file_node("src/handlers/user_handler.rs"));
        graph.add_node(make_file_node("src/validators/user_validator.rs"));
        
        // Another group, cleanly in one dir
        graph.add_node(make_file_node("src/util/hash.rs"));
        graph.add_node(make_file_node("src/util/crypto.rs"));
        graph.add_node(make_file_node("src/util/encoding.rs"));
        
        // Tight coupling in the user-related group (cross-dir)
        graph.add_edge(Edge::new("file:src/models/user.rs", "file:src/handlers/user_handler.rs", "imports"));
        graph.add_edge(Edge::new("file:src/handlers/user_handler.rs", "file:src/models/user.rs", "imports"));
        graph.add_edge(Edge::new("file:src/validators/user_validator.rs", "file:src/models/user.rs", "imports"));
        graph.add_edge(Edge::new("file:src/handlers/user_handler.rs", "file:src/validators/user_validator.rs", "imports"));
        
        // Tight coupling in util group (same dir)
        graph.add_edge(Edge::new("file:src/util/hash.rs", "file:src/util/crypto.rs", "imports"));
        graph.add_edge(Edge::new("file:src/util/crypto.rs", "file:src/util/hash.rs", "imports"));
        graph.add_edge(Edge::new("file:src/util/encoding.rs", "file:src/util/crypto.rs", "imports"));
        graph.add_edge(Edge::new("file:src/util/encoding.rs", "file:src/util/hash.rs", "imports"));
        
        let result = detect_modules(&graph);
        // Should have at least one cross-directory suggestion for the user-related files
        let cross_dir = result.iter()
            .any(|a| a.advice_type == AdviceType::ModuleSuggestion
                && a.message.contains("span"));
        assert!(cross_dir, "Should suggest grouping for cross-directory coupled files. Items: {:?}", 
            result.iter().map(|a| &a.message).collect::<Vec<_>>());
    }
    
    #[cfg(feature = "infomap")]
    #[test]
    fn test_detect_modules_function_level_edges() {
        // Edges between functions should be mapped to file-level connections
        let mut graph = Graph::new();
        
        // Files
        graph.add_node(make_file_node("src/a.rs"));
        graph.add_node(make_file_node("src/b.rs"));
        graph.add_node(make_file_node("src/c.rs"));
        graph.add_node(make_file_node("src/d.rs"));
        
        // Functions in files
        graph.add_node(make_func_node("src/a.rs", "foo"));
        graph.add_node(make_func_node("src/b.rs", "bar"));
        graph.add_node(make_func_node("src/c.rs", "baz"));
        graph.add_node(make_func_node("src/d.rs", "qux"));
        
        // Function-level call edges
        graph.add_edge(Edge::new("func:src/a.rs:foo", "func:src/b.rs:bar", "calls"));
        graph.add_edge(Edge::new("func:src/b.rs:bar", "func:src/a.rs:foo", "calls"));
        graph.add_edge(Edge::new("func:src/c.rs:baz", "func:src/d.rs:qux", "calls"));
        graph.add_edge(Edge::new("func:src/d.rs:qux", "func:src/c.rs:baz", "calls"));
        
        let modules = detect_code_modules(&graph);
        // Function-level edges should still be detected and mapped to files
        assert!(!modules.is_empty(), "Should detect modules from function-level edges");
    }
    
    #[cfg(feature = "infomap")]
    #[test]
    fn test_detect_code_modules_public_api() {
        let mut graph = Graph::new();
        
        for i in 0..6 {
            graph.add_node(make_file_node(&format!("src/mod{}.rs", i)));
        }
        
        // Two clusters: {0,1,2} and {3,4,5}
        graph.add_edge(Edge::new("file:src/mod0.rs", "file:src/mod1.rs", "imports"));
        graph.add_edge(Edge::new("file:src/mod1.rs", "file:src/mod2.rs", "imports"));
        graph.add_edge(Edge::new("file:src/mod2.rs", "file:src/mod0.rs", "imports"));
        
        graph.add_edge(Edge::new("file:src/mod3.rs", "file:src/mod4.rs", "imports"));
        graph.add_edge(Edge::new("file:src/mod4.rs", "file:src/mod5.rs", "imports"));
        graph.add_edge(Edge::new("file:src/mod5.rs", "file:src/mod3.rs", "imports"));
        
        let modules = detect_code_modules(&graph);
        assert!(modules.len() >= 2, "Public API should detect 2+ modules");
        
        // Each module should have files and flow info
        for m in &modules {
            assert!(!m.files.is_empty());
            assert!(!m.node_ids.is_empty());
            assert!(m.size > 0);
            assert!(m.flow >= 0.0);
        }
    }
    
    #[cfg(feature = "infomap")]
    #[test]
    fn test_module_suggestion_does_not_affect_health_score() {
        // ModuleSuggestion is informational — should NOT deduct from health score
        let mut graph = Graph::new();
        
        // Create a graph with two clusters in different dirs
        for i in 0..3 {
            graph.add_node(make_file_node(&format!("src/alpha/f{}.rs", i)));
            graph.add_node(make_file_node(&format!("src/beta/g{}.rs", i)));
        }
        
        // Coupling within clusters
        graph.add_edge(Edge::new("file:src/alpha/f0.rs", "file:src/alpha/f1.rs", "imports"));
        graph.add_edge(Edge::new("file:src/alpha/f1.rs", "file:src/alpha/f2.rs", "imports"));
        graph.add_edge(Edge::new("file:src/alpha/f2.rs", "file:src/alpha/f0.rs", "imports"));
        graph.add_edge(Edge::new("file:src/beta/g0.rs", "file:src/beta/g1.rs", "imports"));
        graph.add_edge(Edge::new("file:src/beta/g1.rs", "file:src/beta/g2.rs", "imports"));
        graph.add_edge(Edge::new("file:src/beta/g2.rs", "file:src/beta/g0.rs", "imports"));
        
        let result = analyze(&graph);
        
        // Module suggestions should not tank the score
        let has_module_suggestion = result.items.iter()
            .any(|a| a.advice_type == AdviceType::ModuleSuggestion);
        
        // Score should still be high (only orphan warnings for file nodes without project connections)
        // The point is: ModuleSuggestion doesn't subtract from health_score
        if has_module_suggestion {
            // Even with suggestions, health shouldn't be tanked by them
            assert!(result.health_score >= 50, 
                "ModuleSuggestion should not heavily impact health score, got {}", result.health_score);
        }
    }
}
