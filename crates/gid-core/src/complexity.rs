//! Complexity assessment — classify tasks using code graph structure
//!
//! Instead of guessing complexity from text (unreliable), we use the
//! actual code graph to measure structural complexity. This is the
//! GID way: let the graph tell you.

use crate::code_graph::{CodeGraph, NodeKind, EdgeRelation};
use std::collections::HashSet;

/// Complexity level of a task/issue
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Complexity {
    Simple,
    Medium,
    Complex,
}

impl std::fmt::Display for Complexity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Complexity::Simple => write!(f, "simple"),
            Complexity::Medium => write!(f, "medium"),
            Complexity::Complex => write!(f, "complex"),
        }
    }
}

/// Result of complexity assessment
#[derive(Debug, Clone)]
pub struct ComplexityReport {
    pub complexity: Complexity,
    pub relevant_nodes: usize,
    pub relevant_files: usize,
    pub class_count: usize,
    pub inheritance_edges: usize,
    pub import_edges: usize,
    pub test_count: usize,
    pub summary: String,
}

/// Assess complexity from the code graph structure.
///
/// Uses relevant node count, edge density, and inheritance depth
/// to determine whether the task is simple, medium, or complex.
/// Zero LLM calls — pure structural analysis.
pub fn assess_complexity_from_graph(
    code_graph: &CodeGraph,
    keywords: &[&str],
    test_count: usize,
) -> ComplexityReport {
    let relevant = code_graph.find_relevant_nodes(keywords);
    let relevant_count = relevant.len();

    // Count relevant files (unique file paths)
    let relevant_files: HashSet<&str> = relevant.iter()
        .map(|n| n.file_path.as_str())
        .collect();
    let file_count = relevant_files.len();

    // Count classes in relevant nodes
    let class_count = relevant.iter()
        .filter(|n| n.kind == NodeKind::Class)
        .count();

    // Count inheritance edges involving relevant nodes
    let relevant_ids: HashSet<&str> = relevant.iter()
        .map(|n| n.id.as_str())
        .collect();
    let inheritance_edges = code_graph.edges.iter()
        .filter(|e| {
            e.relation == EdgeRelation::Inherits
                && (relevant_ids.contains(e.from.as_str()) || relevant_ids.contains(e.to.as_str()))
        })
        .count();

    // Count import edges between relevant files
    let import_edges = code_graph.edges.iter()
        .filter(|e| {
            e.relation == EdgeRelation::Imports
                && (relevant_ids.contains(e.from.as_str()) || relevant_ids.contains(e.to.as_str()))
        })
        .count();

    tracing::debug!(
        "Graph complexity metrics: relevant_nodes={}, files={}, classes={}, inheritance={}, imports={}, tests={}",
        relevant_count, file_count, class_count, inheritance_edges, import_edges, test_count
    );

    // Decision logic
    let complexity = if relevant_count <= 2 && file_count <= 1 && class_count == 0 && test_count <= 1 {
        Complexity::Simple
    } else if relevant_count >= 6 || file_count >= 3 || inheritance_edges >= 2 || import_edges >= 4 || test_count > 3 {
        Complexity::Complex
    } else {
        Complexity::Medium
    };

    let summary = format!(
        "Complexity: {:?} (nodes={}, files={}, classes={}, inherit={}, imports={}, tests={})",
        complexity, relevant_count, file_count, class_count, inheritance_edges, import_edges, test_count
    );

    tracing::info!("{}", summary);

    ComplexityReport {
        complexity,
        relevant_nodes: relevant_count,
        relevant_files: file_count,
        class_count,
        inheritance_edges,
        import_edges,
        test_count,
        summary,
    }
}

/// Assess complexity from a problem statement
pub fn assess_complexity(
    code_graph: &CodeGraph,
    problem_statement: &str,
    test_count: usize,
) -> ComplexityReport {
    let keywords = CodeGraph::extract_keywords(problem_statement);
    let keyword_refs: Vec<&str> = keywords.to_vec();
    assess_complexity_from_graph(code_graph, &keyword_refs, test_count)
}

/// Quick check if a change is high-risk based on caller count
pub fn is_high_risk_change(code_graph: &CodeGraph, node_ids: &[&str]) -> bool {
    let total_callers: usize = node_ids.iter()
        .map(|id| code_graph.get_callers(id).len())
        .sum();
    
    // High risk if total callers > 10 or any single node has > 5 callers
    let max_callers = node_ids.iter()
        .map(|id| code_graph.get_callers(id).len())
        .max()
        .unwrap_or(0);
    
    total_callers > 10 || max_callers > 5
}

/// Get risk level for a set of changed files
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    Low,      // < 5 total callers
    Medium,   // 5-20 total callers
    High,     // 20-50 total callers
    Critical, // > 50 total callers
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

pub fn assess_risk_level(code_graph: &CodeGraph, node_ids: &[&str]) -> RiskLevel {
    let total_callers: usize = node_ids.iter()
        .map(|id| code_graph.get_callers(id).len())
        .sum();
    
    match total_callers {
        0..=5 => RiskLevel::Low,
        6..=20 => RiskLevel::Medium,
        21..=50 => RiskLevel::High,
        _ => RiskLevel::Critical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_graph::{CodeGraph, CodeNode, CodeEdge};

    #[test]
    fn test_empty_graph_defaults_simple() {
        let graph = CodeGraph::default();
        let report = assess_complexity_from_graph(&graph, &["test"], 0);
        assert_eq!(report.complexity, Complexity::Simple);
    }

    #[test]
    fn test_complex_with_many_files() {
        let mut graph = CodeGraph::default();
        
        // Add nodes from multiple files
        for i in 0..5 {
            let file_path = format!("file{}.py", i);
            let id = format!("class:{}:TestClass{}", file_path, i);
            graph.nodes.push(CodeNode {
                name: format!("TestClass{}", i),
                file_path,
                line: Some(1),
                line_count: 10,
                ..CodeNode::test_default(&id, NodeKind::Class)
            });
        }
        
        graph.build_indexes();
        
        let report = assess_complexity_from_graph(&graph, &["TestClass"], 0);
        assert_eq!(report.complexity, Complexity::Complex);
        assert!(report.relevant_files >= 3);
    }

    #[test]
    fn test_risk_level() {
        let mut graph = CodeGraph::default();
        
        // Create a function with many callers
        graph.nodes.push(CodeNode {
            name: "hot_func".into(),
            file_path: "core.py".into(),
            line: Some(10),
            line_count: 20,
            ..CodeNode::test_default("func:core.py:hot_func", NodeKind::Function)
        });
        
        // Add many callers
        for i in 0..30 {
            let caller_id = format!("func:caller{}.py:caller_{}", i, i);
            graph.nodes.push(CodeNode {
                name: format!("caller_{}", i),
                file_path: format!("caller{}.py", i),
                line: Some(1),
                line_count: 5,
                ..CodeNode::test_default(&caller_id, NodeKind::Function)
            });
            graph.edges.push(CodeEdge::new(&caller_id, "func:core.py:hot_func", EdgeRelation::Calls));
        }
        
        graph.build_indexes();
        
        let risk = assess_risk_level(&graph, &["func:core.py:hot_func"]);
        assert_eq!(risk, RiskLevel::High);
    }
}
