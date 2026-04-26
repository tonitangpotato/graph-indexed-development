//! Graph phase mode dispatch + ID-collision validation.
//!
//! ISS-039 Fix 3 + Fix 4. Determines whether the ritual `Graphing` phase
//! should run in **PlanNew** (no existing task subtree → invent it from design),
//! **Reconcile** (subtree exists → status updates only, no new tasks), or
//! **NoOp** (single-task work unit, no graph work).
//!
//! Provides snapshot-and-diff validation so a buggy or hallucinating LLM
//! cannot silently pollute the graph: snapshot ID set before, again after,
//! diff = "new IDs created"; check against forbidden / reserved sets.

use crate::graph::{Graph, NodeStatus};
use crate::ritual::work_unit::WorkUnit;
use std::collections::HashSet;
use std::path::Path;

/// Concrete data the prompt template + validation needs about an existing task.
#[derive(Debug, Clone)]
pub struct ExistingTaskInfo {
    pub id: String,
    pub status: NodeStatus,
    pub title: String,
}

/// Mode of the Graphing phase, decided per-ritual from work_unit + graph state.
#[derive(Debug, Clone)]
pub enum GraphPhaseMode {
    /// No existing task subtree for this work unit → LLM plans new tasks from design.
    PlanNew {
        /// IDs reserved by issue.md as planned-but-not-yet-materialized; the LLM
        /// MAY use them only when explicitly materializing those exact tasks.
        reserved_ids: Vec<String>,
    },
    /// Subtree exists → LLM only reconciles status of existing nodes.
    Reconcile {
        existing_nodes: Vec<ExistingTaskInfo>,
        /// IDs reserved by issue.md but not yet in the graph. Forbidden in
        /// reconcile mode regardless of whether the LLM thinks they "belong".
        reserved_ids: Vec<String>,
    },
    /// Work unit references a single existing task — no graph work needed,
    /// the implement phase will flip its status when done.
    NoOp,
}

impl GraphPhaseMode {
    pub fn is_no_op(&self) -> bool {
        matches!(self, GraphPhaseMode::NoOp)
    }

    /// Skill name to load for this mode.
    pub fn skill_name(&self) -> &'static str {
        match self {
            GraphPhaseMode::PlanNew { .. } => "generate-graph",
            GraphPhaseMode::Reconcile { .. } => "update-graph",
            GraphPhaseMode::NoOp => "generate-graph", // unused; phase is skipped
        }
    }
}

/// Decide which graph-phase mode applies for this work unit.
///
/// The decision is structural, not based on file presence:
/// - `WorkUnit::Task { task_id }` → if `task_id` resolves in graph, NoOp.
/// - `WorkUnit::Issue { id }` or `Feature { name }` → look for child task
///   nodes whose IDs prefix-match the issue/feature ID.
///   - children present → Reconcile
///   - none → PlanNew
///
/// `reserved_ids` should be the planned-but-not-yet-materialized IDs parsed
/// from `issue.md`; pass empty Vec if none/unknown.
pub fn determine_graph_mode(
    work_unit: &WorkUnit,
    graph: &Graph,
    reserved_ids: Vec<String>,
) -> GraphPhaseMode {
    match work_unit {
        WorkUnit::Task { task_id, .. } => {
            if graph.nodes.iter().any(|n| n.id == *task_id) {
                GraphPhaseMode::NoOp
            } else {
                // Single-task work unit but the task isn't in the graph yet.
                // Treat as plan-new with the missing task as a reserved ID.
                let mut reserved = reserved_ids;
                if !reserved.contains(task_id) {
                    reserved.push(task_id.clone());
                }
                GraphPhaseMode::PlanNew {
                    reserved_ids: reserved,
                }
            }
        }
        WorkUnit::Issue { id, .. } => mode_from_subtree(graph, id, reserved_ids),
        WorkUnit::Feature { name, .. } => mode_from_subtree(graph, name, reserved_ids),
    }
}

fn mode_from_subtree(graph: &Graph, prefix: &str, reserved_ids: Vec<String>) -> GraphPhaseMode {
    // A child task ID conventionally starts with "<prefix>-" (e.g. ISS-021-3).
    // We also accept exact match (the parent issue node itself) as evidence
    // the issue is tracked, but do not include that in `existing_nodes` because
    // it's not a task to reconcile.
    let child_prefix = format!("{}-", prefix);
    let children: Vec<ExistingTaskInfo> = graph
        .nodes
        .iter()
        .filter(|n| n.id.starts_with(&child_prefix))
        .filter(|n| n.node_type.as_deref() == Some("task"))
        .map(|n| ExistingTaskInfo {
            id: n.id.clone(),
            status: n.status.clone(),
            title: n.title.clone(),
        })
        .collect();

    if children.is_empty() {
        GraphPhaseMode::PlanNew { reserved_ids }
    } else {
        GraphPhaseMode::Reconcile {
            existing_nodes: children,
            reserved_ids,
        }
    }
}

/// Render the existing-task list as a markdown table for prompt injection.
pub fn render_existing_nodes(nodes: &[ExistingTaskInfo]) -> String {
    if nodes.is_empty() {
        return "(none)".to_string();
    }
    let mut s = String::new();
    for n in nodes {
        let status = match n.status {
            NodeStatus::Todo => "todo",
            NodeStatus::InProgress => "in_progress",
            NodeStatus::Done => "done",
            NodeStatus::Blocked => "blocked",
            NodeStatus::Cancelled => "cancelled",
            NodeStatus::Failed => "failed",
            NodeStatus::NeedsResolution => "needs_resolution",
        };
        s.push_str(&format!("- {:24} | {:11} | {}\n", n.id, status, n.title));
    }
    s
}

pub fn render_reserved_ids(ids: &[String]) -> String {
    if ids.is_empty() {
        "(none)".to_string()
    } else {
        ids.join(", ")
    }
}

/// Snapshot of all node IDs in the graph for a given prefix (or all nodes if prefix is empty).
/// Used pre-/post-LLM to compute the diff "what did the LLM create".
pub fn snapshot_node_ids(graph: &Graph, prefix: Option<&str>) -> HashSet<String> {
    graph
        .nodes
        .iter()
        .filter(|n| match prefix {
            Some(p) => n.id == *p || n.id.starts_with(&format!("{}-", p)),
            None => true,
        })
        .map(|n| n.id.clone())
        .collect()
}

/// Validate that a graph mutation done by the LLM during the Graphing phase
/// did not violate the contract for `mode`. Returns Err with a diagnostic
/// message on violation; the caller is expected to roll back the new IDs.
pub fn validate_graph_phase_output(
    mode: &GraphPhaseMode,
    nodes_before: &HashSet<String>,
    nodes_after: &HashSet<String>,
) -> Result<(), String> {
    let new_ids: Vec<&String> = nodes_after.difference(nodes_before).collect();

    match mode {
        GraphPhaseMode::Reconcile { reserved_ids, .. } => {
            if !new_ids.is_empty() {
                return Err(format!(
                    "Graph phase produced {} new node(s) in Reconcile mode: {:?}. \
                     Reconcile mode forbids new task creation — only status updates \
                     are permitted. Ritual aborted to prevent graph pollution \
                     (ISS-031-class incident).",
                    new_ids.len(),
                    new_ids
                ));
            }
            // (Edge case: LLM somehow re-added a reserved ID that was already
            // in the graph before this phase. snapshot diff would be empty,
            // so reserved_ids check here is effectively a no-op for the
            // `new_ids` set; left as documentation.)
            let _ = reserved_ids;
        }
        GraphPhaseMode::PlanNew { reserved_ids } => {
            // In PlanNew mode, new nodes are expected. Reserved IDs are
            // permitted ONLY if the LLM is materializing exactly those
            // planned tasks. We can't tell intent from the diff alone,
            // so we use a conservative rule: if a new ID matches a reserved
            // ID, allow it (it's expected); if it's neither in reserved
            // nor matches the work-unit prefix scheme, log but don't fail
            // (deferred to ISS-041 stricter validation).
            let reserved_set: HashSet<&String> = reserved_ids.iter().collect();
            for id in &new_ids {
                if reserved_set.contains(id) {
                    // allowed — materializing a planned ID
                    continue;
                }
                // For ISS-039 v1 we accept any other new ID. Stricter
                // scheme-based validation is ISS-041 follow-up.
            }
        }
        GraphPhaseMode::NoOp => {
            if !new_ids.is_empty() {
                return Err(format!(
                    "Graph phase produced {} new node(s) in NoOp mode: {:?}. \
                     NoOp mode means the work unit references a single existing \
                     task — graphing was supposed to be skipped entirely. \
                     Ritual aborted.",
                    new_ids.len(),
                    new_ids
                ));
            }
        }
    }

    Ok(())
}

/// Heuristic: parse `.gid/issues/<ID>.md` for "planned" task IDs.
///
/// Looks for two patterns (best-effort, ISS-039 v1):
/// 1. Frontmatter line `planned_task_ids: [ISS-X-12, ISS-X-13]`
/// 2. Inline mentions: `ISS-X-NN` where NN is purely numeric and the line
///    contains `plan` or `planned` (case-insensitive)
///
/// Returns empty Vec if no issue file or no planned IDs found. Never fails
/// — this is a hint to the LLM, not a hard contract.
pub fn parse_planned_ids(issue_md_path: &Path, issue_id: &str) -> Vec<String> {
    let content = match std::fs::read_to_string(issue_md_path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut ids: HashSet<String> = HashSet::new();

    // Pattern 1: structured frontmatter `planned_task_ids: [...]`
    for line in content.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("planned_task_ids:") {
            // Expect `[ID1, ID2, ...]` or `ID1, ID2`
            let cleaned = rest
                .trim()
                .trim_start_matches('[')
                .trim_end_matches(']');
            for piece in cleaned.split(',') {
                let id = piece.trim().trim_matches('"').trim_matches('\'');
                if !id.is_empty() && id.starts_with(issue_id) {
                    ids.insert(id.to_string());
                }
            }
        }
    }

    // Pattern 2: heuristic over lines mentioning "plan" or "planned"
    let prefix = format!("{}-", issue_id);
    for line in content.lines() {
        let lower = line.to_lowercase();
        if !(lower.contains("plan") || lower.contains("planned")) {
            continue;
        }
        // Find ISS-X-NN tokens
        let chars = line.char_indices().peekable();
        for (i, c) in chars {
            if c.is_ascii_alphabetic() {
                // Try to match the prefix starting here
                if line[i..].starts_with(&prefix) {
                    let rest = &line[i + prefix.len()..];
                    // Take consecutive ascii digits
                    let n: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                    if !n.is_empty() {
                        ids.insert(format!("{}{}", prefix, n));
                    }
                }
            }
        }
    }

    let mut v: Vec<String> = ids.into_iter().collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Node, NodeStatus};

    fn task_node(id: &str, status: NodeStatus) -> Node {
        let mut n = Node::new(id, &format!("Task {}", id));
        n.status = status;
        n.node_type = Some("task".to_string());
        n
    }

    #[test]
    fn determine_mode_planew_when_no_subtree() {
        let graph = Graph::default();
        let wu = WorkUnit::Issue {
            project: "p".into(),
            id: "ISS-T".into(),
        };
        let mode = determine_graph_mode(&wu, &graph, vec![]);
        assert!(matches!(mode, GraphPhaseMode::PlanNew { .. }));
    }

    #[test]
    fn determine_mode_reconcile_when_children_exist() {
        let mut graph = Graph::default();
        graph.nodes.push(task_node("ISS-T-1", NodeStatus::Done));
        graph.nodes.push(task_node("ISS-T-2", NodeStatus::Todo));
        let wu = WorkUnit::Issue {
            project: "p".into(),
            id: "ISS-T".into(),
        };
        let mode = determine_graph_mode(&wu, &graph, vec!["ISS-T-12".into()]);
        match mode {
            GraphPhaseMode::Reconcile {
                existing_nodes,
                reserved_ids,
            } => {
                assert_eq!(existing_nodes.len(), 2);
                assert_eq!(reserved_ids, vec!["ISS-T-12"]);
            }
            other => panic!("expected Reconcile, got {:?}", other),
        }
    }

    #[test]
    fn determine_mode_noop_when_task_resolves() {
        let mut graph = Graph::default();
        graph.nodes.push(task_node("ISS-T-7", NodeStatus::Todo));
        let wu = WorkUnit::Task {
            project: "p".into(),
            task_id: "ISS-T-7".into(),
        };
        let mode = determine_graph_mode(&wu, &graph, vec![]);
        assert!(matches!(mode, GraphPhaseMode::NoOp));
    }

    #[test]
    fn validate_reconcile_blocks_new_nodes() {
        // ISS-039 Acceptance Criterion 4: simulate Reconcile mode where the LLM
        // tried to add a reserved ID — must hard-fail.
        let mode = GraphPhaseMode::Reconcile {
            existing_nodes: vec![ExistingTaskInfo {
                id: "ISS-T-1".into(),
                status: NodeStatus::Done,
                title: "T1".into(),
            }],
            reserved_ids: vec!["ISS-T-2".into()],
        };
        let before: HashSet<String> = ["ISS-T-1".to_string()].into_iter().collect();
        let after: HashSet<String> =
            ["ISS-T-1".to_string(), "ISS-T-2".to_string()].into_iter().collect();
        let result = validate_graph_phase_output(&mode, &before, &after);
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("Reconcile") && msg.contains("forbid"));
    }

    #[test]
    fn validate_reconcile_passes_when_no_changes() {
        let mode = GraphPhaseMode::Reconcile {
            existing_nodes: vec![],
            reserved_ids: vec![],
        };
        let s: HashSet<String> = ["A".to_string()].into_iter().collect();
        assert!(validate_graph_phase_output(&mode, &s, &s).is_ok());
    }

    #[test]
    fn validate_planew_allows_reserved_materialization() {
        let mode = GraphPhaseMode::PlanNew {
            reserved_ids: vec!["ISS-T-12".into(), "ISS-T-13".into()],
        };
        let before: HashSet<String> = HashSet::new();
        let after: HashSet<String> =
            ["ISS-T-12".to_string(), "ISS-T-13".to_string()].into_iter().collect();
        assert!(validate_graph_phase_output(&mode, &before, &after).is_ok());
    }

    #[test]
    fn validate_noop_blocks_any_new_node() {
        let mode = GraphPhaseMode::NoOp;
        let before: HashSet<String> = HashSet::new();
        let after: HashSet<String> = ["X".to_string()].into_iter().collect();
        assert!(validate_graph_phase_output(&mode, &before, &after).is_err());
    }

    #[test]
    fn parse_planned_ids_from_frontmatter() {
        use std::io::Write;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            tmp.as_file(),
            "# Issue\n\nplanned_task_ids: [ISS-X-12, ISS-X-13]\n\nbody"
        )
        .unwrap();
        let ids = parse_planned_ids(tmp.path(), "ISS-X");
        assert_eq!(ids, vec!["ISS-X-12".to_string(), "ISS-X-13".to_string()]);
    }

    #[test]
    fn parse_planned_ids_from_inline_text() {
        use std::io::Write;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            tmp.as_file(),
            "# Issue\n\nPhase 5b planned IDs: ISS-X-12 and ISS-X-13 will be materialized later.\n"
        )
        .unwrap();
        let ids = parse_planned_ids(tmp.path(), "ISS-X");
        assert_eq!(ids, vec!["ISS-X-12".to_string(), "ISS-X-13".to_string()]);
    }

    #[test]
    fn parse_planned_ids_returns_empty_when_no_file() {
        let ids = parse_planned_ids(Path::new("/nonexistent/path.md"), "ISS-X");
        assert!(ids.is_empty());
    }

    #[test]
    fn render_helpers_produce_readable_output() {
        let nodes = vec![ExistingTaskInfo {
            id: "ISS-X-1".into(),
            status: NodeStatus::Todo,
            title: "First task".into(),
        }];
        let s = render_existing_nodes(&nodes);
        assert!(s.contains("ISS-X-1"));
        assert!(s.contains("todo"));
        assert!(s.contains("First task"));

        assert_eq!(render_reserved_ids(&[]), "(none)");
        assert_eq!(
            render_reserved_ids(&["a".into(), "b".into()]),
            "a, b"
        );
    }
}
