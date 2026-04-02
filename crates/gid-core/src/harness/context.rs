//! Context assembler: build minimal, precise context for each sub-agent.
//!
//! Resolves graph metadata to actual file content — feature docs, design
//! sections, requirements goals, and project guards.

use std::path::Path;
use anyhow::Result;
use crate::graph::Graph;
use super::types::{TaskContext, TaskInfo};

/// Assemble context for a task by resolving docs via the feature node.
///
/// Resolution chain:
/// 1. Task → `implements` edge → feature node
/// 2. Feature node → `metadata.design_doc` → `.gid/features/{name}/design.md` + `requirements.md`
/// 3. Task `design_ref` → extract matching section from design.md
/// 4. Task `satisfies` → resolve GOAL lines from requirements.md
/// 5. Graph root `metadata.guards` → inject into context
///
/// If the feature has no `design_doc`, falls back to `.gid/design.md` and `.gid/requirements.md`.
/// Missing files produce warnings (logged via tracing) but don't fail the assembly.
pub fn assemble_task_context(
    graph: &Graph,
    task_id: &str,
    gid_root: &Path,
) -> Result<TaskContext> {
    let node = graph.get_node(task_id)
        .ok_or_else(|| anyhow::anyhow!("Task node '{}' not found in graph", task_id))?;

    // Extract TaskInfo
    let task_info = extract_task_info_from_node(node, graph);

    // Resolve feature node via `implements` edge
    let feature_node_id = graph.edges.iter()
        .find(|e| e.from == task_id && e.relation == "implements")
        .map(|e| e.to.as_str());

    // Determine doc paths from feature node
    let (design_path, requirements_path) = resolve_doc_paths(graph, feature_node_id, gid_root);

    // Extract design excerpt from design_ref
    let design_excerpt = if let Some(ref design_ref) = task_info.design_ref {
        match &design_path {
            Some(path) if path.exists() => {
                match std::fs::read_to_string(path) {
                    Ok(content) => extract_design_section(&content, design_ref),
                    Err(e) => {
                        tracing::warn!("Failed to read design doc {}: {}", path.display(), e);
                        None
                    }
                }
            }
            Some(path) => {
                tracing::warn!("Design doc not found: {}", path.display());
                None
            }
            None => None,
        }
    } else {
        None
    };

    // Resolve GOAL text from requirements.md
    let goals_text = if !task_info.satisfies.is_empty() {
        match &requirements_path {
            Some(path) if path.exists() => {
                match std::fs::read_to_string(path) {
                    Ok(content) => resolve_goals(&content, &task_info.satisfies),
                    Err(e) => {
                        tracing::warn!("Failed to read requirements {}: {}", path.display(), e);
                        Vec::new()
                    }
                }
            }
            Some(path) => {
                tracing::warn!("Requirements not found: {}", path.display());
                Vec::new()
            }
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };

    // Collect dependency interface descriptions
    let dependency_interfaces = resolve_dependency_interfaces(graph, &task_info);

    // Inject guards from graph root metadata
    let guards = extract_guards(graph);

    Ok(TaskContext {
        task_info,
        goals_text,
        design_excerpt,
        dependency_interfaces,
        guards,
    })
}

/// Resolve design.md and requirements.md paths from the feature node.
///
/// If the feature has `metadata.design_doc`, maps to `.gid/features/{name}/`.
/// Otherwise falls back to `.gid/design.md` and `.gid/requirements.md`.
fn resolve_doc_paths(
    graph: &Graph,
    feature_node_id: Option<&str>,
    gid_root: &Path,
) -> (Option<std::path::PathBuf>, Option<std::path::PathBuf>) {
    if let Some(feature_id) = feature_node_id {
        if let Some(feature_node) = graph.get_node(feature_id) {
            if let Some(design_doc) = feature_node.metadata.get("design_doc")
                .and_then(|v| v.as_str())
            {
                let feature_dir = gid_root.join("features").join(design_doc);
                return (
                    Some(feature_dir.join("design.md")),
                    Some(feature_dir.join("requirements.md")),
                );
            }
        }
    }

    // Fallback to root-level docs
    (
        Some(gid_root.join("design.md")),
        Some(gid_root.join("requirements.md")),
    )
}

/// Extract a section from a markdown document by section reference.
///
/// Finds a heading whose number prefix matches `design_ref` (e.g., "3.2"),
/// then captures all text until the next heading of same or higher level.
///
/// - "3.2" matches "### 3.2 Execution Planner" or "## 3.2 Something"
/// - "3" captures the heading and all subsections (3.1, 3.2, etc.)
/// - Missing section returns None
/// - Multiple matches returns first match
fn extract_design_section(content: &str, design_ref: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let mut start_idx = None;
    let mut start_level = 0;

    for (i, line) in lines.iter().enumerate() {
        if let Some((level, heading_text)) = parse_heading(line) {
            let trimmed = heading_text.trim();
            if heading_starts_with_ref(trimmed, design_ref) {
                start_idx = Some(i);
                start_level = level;
                break;
            }
        }
    }

    let start = start_idx?;

    // Capture until next heading of same or higher (lower number) level
    let mut end_idx = lines.len();
    for i in (start + 1)..lines.len() {
        if let Some((level, _)) = parse_heading(lines[i]) {
            if level <= start_level {
                end_idx = i;
                break;
            }
        }
    }

    let section: String = lines[start..end_idx].join("\n");
    let trimmed = section.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Parse a markdown heading line. Returns (level, text after #s).
fn parse_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    let level = trimmed.chars().take_while(|&c| c == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &trimmed[level..];
    // Must have a space after #s (standard markdown)
    if !rest.starts_with(' ') {
        return None;
    }
    Some((level, rest[1..].trim()))
}

/// Check if a heading text starts with the given section reference as a number prefix.
///
/// "3.2" matches "3.2 Execution Planner", "3.2. Something"
/// "3" matches "3 Components", "3. Components"
fn heading_starts_with_ref(heading: &str, design_ref: &str) -> bool {
    if !heading.starts_with(design_ref) {
        return false;
    }
    let rest = &heading[design_ref.len()..];
    // After the ref, expect: end of string, space, period, or period+space
    rest.is_empty()
        || rest.starts_with(' ')
        || rest.starts_with('.')
}

/// Resolve GOAL IDs to their full text from requirements.md content.
///
/// Searches for lines containing each GOAL ID (e.g., "GOAL-1.1") and returns
/// the full line text.
fn resolve_goals(content: &str, goal_ids: &[String]) -> Vec<String> {
    let mut results = Vec::new();
    for goal_id in goal_ids {
        for line in content.lines() {
            if line.contains(goal_id.as_str()) {
                results.push(line.trim().to_string());
                break;
            }
        }
    }
    results
}

/// Extract interface/description info from completed dependency tasks.
fn resolve_dependency_interfaces(graph: &Graph, task_info: &TaskInfo) -> Vec<String> {
    let mut interfaces = Vec::new();
    for dep_id in &task_info.depends_on {
        if let Some(dep_node) = graph.get_node(dep_id) {
            let mut info = format!("[{}] {}", dep_node.id, dep_node.title);
            if let Some(ref desc) = dep_node.description {
                let truncated: String = desc.chars().take(200).collect();
                info.push_str(&format!(": {}", truncated));
            }
            interfaces.push(info);
        }
    }
    interfaces
}

/// Extract project-level guards from graph metadata.
///
/// Guards are stored in any node's `metadata.guards` as an array of strings.
/// Convention: the first node with guards (typically a root/project node).
fn extract_guards(graph: &Graph) -> Vec<String> {
    for node in &graph.nodes {
        if let Some(guards_val) = node.metadata.get("guards") {
            if let Some(arr) = guards_val.as_array() {
                return arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
            }
        }
    }
    Vec::new()
}

/// Extract TaskInfo from a graph Node.
fn extract_task_info_from_node(node: &crate::graph::Node, graph: &Graph) -> TaskInfo {
    let description = node.description.clone().unwrap_or_default();

    let verify = node.metadata.get("verify")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let estimated_turns = node.metadata.get("estimated_turns")
        .and_then(|v| v.as_u64())
        .unwrap_or(15) as u32;

    let design_ref = node.metadata.get("design_ref")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let satisfies = node.metadata.get("satisfies")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let goals = node.metadata.get("goals")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let depends_on: Vec<String> = graph.edges.iter()
        .filter(|e| e.from == node.id && e.relation == "depends_on")
        .map(|e| e.to.clone())
        .collect();

    TaskInfo {
        id: node.id.clone(),
        title: node.title.clone(),
        description,
        goals,
        verify,
        estimated_turns,
        depends_on,
        design_ref,
        satisfies,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Node, Edge, NodeStatus};
    use tempfile::TempDir;
    use std::fs;

    fn make_task(id: &str, title: &str) -> Node {
        let mut n = Node::new(id, title);
        n.node_type = Some("task".to_string());
        n
    }

    fn make_feature(id: &str, title: &str, design_doc: &str) -> Node {
        let mut n = Node::new(id, title);
        n.node_type = Some("feature".to_string());
        n.metadata.insert("design_doc".to_string(), serde_json::json!(design_doc));
        n
    }

    fn setup_gid_dir() -> TempDir {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("design.md"), "# 1 Overview\nFallback design.\n").unwrap();
        fs::write(tmp.path().join("requirements.md"), "- GOAL-1: Basic requirement\n").unwrap();
        tmp
    }

    fn setup_feature_docs(gid_root: &Path, feature_name: &str) {
        let feature_dir = gid_root.join("features").join(feature_name);
        fs::create_dir_all(&feature_dir).unwrap();
        fs::write(feature_dir.join("design.md"), concat!(
            "# Design\n\n",
            "## 3 Components\n\n",
            "### 3.1 Topology Analyzer\n\n",
            "Validates graph structure and computes layers.\n\n",
            "### 3.2 Execution Planner\n\n",
            "Generates ExecutionPlan from topology.\n",
            "Key interface: `create_plan(graph) -> ExecutionPlan`\n\n",
            "### 3.3 Context Assembler\n\n",
            "Builds task context from graph metadata.\n\n",
            "## 4 Data Models\n\n",
            "Data model definitions.\n",
        )).unwrap();

        fs::write(feature_dir.join("requirements.md"), concat!(
            "# Requirements\n\n",
            "- GOAL-1.1: Detect cycles in dependency graph\n",
            "- GOAL-1.2: Compute parallelizable layers\n",
            "- GOAL-1.3: Find critical path\n",
            "- GOAL-2.1: Generate execution plan from graph\n",
            "- GOAL-2.2: Support parallel task execution\n",
        )).unwrap();
    }

    #[test]
    fn test_feature_doc_resolution() {
        let gid_root = setup_gid_dir();
        setup_feature_docs(gid_root.path(), "task-harness");

        let mut graph = Graph::new();
        let mut task = make_task("topo", "Implement topology analyzer");
        task.metadata.insert("design_ref".to_string(), serde_json::json!("3.1"));
        task.metadata.insert("satisfies".to_string(), serde_json::json!(["GOAL-1.1", "GOAL-1.2"]));
        graph.add_node(task);
        graph.add_node(make_feature("harness-feature", "Task Harness", "task-harness"));
        graph.add_edge(Edge::new("topo", "harness-feature", "implements"));

        let ctx = assemble_task_context(&graph, "topo", gid_root.path()).unwrap();

        assert!(ctx.design_excerpt.is_some());
        let excerpt = ctx.design_excerpt.unwrap();
        assert!(excerpt.contains("Topology Analyzer"), "excerpt: {}", excerpt);
        assert!(excerpt.contains("Validates graph structure"));
        assert!(!excerpt.contains("Execution Planner"), "excerpt leaked into next section");

        assert_eq!(ctx.goals_text.len(), 2);
        assert!(ctx.goals_text[0].contains("GOAL-1.1"));
        assert!(ctx.goals_text[1].contains("GOAL-1.2"));
    }

    #[test]
    fn test_design_ref_captures_subsections() {
        let content = concat!(
            "## 3 Components\n\n",
            "### 3.1 First\n\n",
            "Content of 3.1.\n\n",
            "### 3.2 Second\n\n",
            "Content of 3.2.\n\n",
            "## 4 Other\n",
        );
        let section = extract_design_section(content, "3").unwrap();
        assert!(section.contains("Components"));
        assert!(section.contains("3.1 First"));
        assert!(section.contains("3.2 Second"));
        assert!(!section.contains("4 Other"));
    }

    #[test]
    fn test_design_ref_missing_section() {
        let content = "# 1 Overview\nSome content.\n## 2 Architecture\nMore content.";
        assert!(extract_design_section(content, "5.3").is_none());
    }

    #[test]
    fn test_fallback_to_root_docs() {
        let gid_root = setup_gid_dir();

        let mut graph = Graph::new();
        let mut task = make_task("standalone", "Standalone task");
        task.metadata.insert("design_ref".to_string(), serde_json::json!("1"));
        task.metadata.insert("satisfies".to_string(), serde_json::json!(["GOAL-1"]));
        graph.add_node(task);

        let ctx = assemble_task_context(&graph, "standalone", gid_root.path()).unwrap();
        assert!(ctx.design_excerpt.is_some());
        assert!(ctx.design_excerpt.unwrap().contains("Fallback design"));
        assert_eq!(ctx.goals_text.len(), 1);
        assert!(ctx.goals_text[0].contains("GOAL-1"));
    }

    #[test]
    fn test_guards_injection() {
        let gid_root = setup_gid_dir();

        let mut graph = Graph::new();
        let mut root = Node::new("project-root", "Project");
        root.node_type = Some("root".to_string());
        root.metadata.insert("guards".to_string(), serde_json::json!([
            "GUARD-1: All file writes are atomic",
            "GUARD-2: Auth tokens never logged"
        ]));
        graph.add_node(root);
        graph.add_node(make_task("task-a", "Task A"));

        let ctx = assemble_task_context(&graph, "task-a", gid_root.path()).unwrap();
        assert_eq!(ctx.guards.len(), 2);
        assert!(ctx.guards[0].contains("GUARD-1"));
        assert!(ctx.guards[1].contains("GUARD-2"));
    }

    #[test]
    fn test_dependency_interfaces() {
        let gid_root = setup_gid_dir();

        let mut graph = Graph::new();
        let mut dep = make_task("dep-task", "Dependency Task");
        dep.description = Some("Provides auth module with login() interface".to_string());
        dep.status = NodeStatus::Done;
        graph.add_node(dep);
        graph.add_node(make_task("main-task", "Main Task"));
        graph.add_edge(Edge::depends_on("main-task", "dep-task"));

        let ctx = assemble_task_context(&graph, "main-task", gid_root.path()).unwrap();
        assert_eq!(ctx.dependency_interfaces.len(), 1);
        assert!(ctx.dependency_interfaces[0].contains("Dependency Task"));
        assert!(ctx.dependency_interfaces[0].contains("auth module"));
    }

    #[test]
    fn test_missing_task_node() {
        let gid_root = setup_gid_dir();
        let graph = Graph::new();
        let result = assemble_task_context(&graph, "nonexistent", gid_root.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_missing_feature_docs_graceful() {
        let gid_root = setup_gid_dir();

        let mut graph = Graph::new();
        let mut task = make_task("task-x", "Task X");
        task.metadata.insert("design_ref".to_string(), serde_json::json!("3.1"));
        task.metadata.insert("satisfies".to_string(), serde_json::json!(["GOAL-99"]));
        graph.add_node(task);
        graph.add_node(make_feature("feat", "Feature", "nonexistent-feature"));
        graph.add_edge(Edge::new("task-x", "feat", "implements"));

        let ctx = assemble_task_context(&graph, "task-x", gid_root.path()).unwrap();
        assert!(ctx.design_excerpt.is_none());
        assert!(ctx.goals_text.is_empty());
    }

    #[test]
    fn test_context_deterministic() {
        let gid_root = setup_gid_dir();
        setup_feature_docs(gid_root.path(), "test-feature");

        let mut graph = Graph::new();
        let mut task = make_task("det-task", "Deterministic");
        task.metadata.insert("design_ref".to_string(), serde_json::json!("3.2"));
        task.metadata.insert("satisfies".to_string(), serde_json::json!(["GOAL-2.1"]));
        graph.add_node(task);
        graph.add_node(make_feature("feat", "Feature", "test-feature"));
        graph.add_edge(Edge::new("det-task", "feat", "implements"));

        let ctx1 = assemble_task_context(&graph, "det-task", gid_root.path()).unwrap();
        let ctx2 = assemble_task_context(&graph, "det-task", gid_root.path()).unwrap();

        assert_eq!(
            serde_json::to_string(&ctx1).unwrap(),
            serde_json::to_string(&ctx2).unwrap(),
            "assemble_task_context must be deterministic (GUARD-2)"
        );
    }

    #[test]
    fn test_heading_parser() {
        assert_eq!(parse_heading("## 3.2 Title"), Some((2, "3.2 Title")));
        assert_eq!(parse_heading("### 3.2.1 Sub"), Some((3, "3.2.1 Sub")));
        assert_eq!(parse_heading("# Top"), Some((1, "Top")));
        assert_eq!(parse_heading("Not a heading"), None);
        assert_eq!(parse_heading("#NoSpace"), None);
    }

    #[test]
    fn test_heading_ref_matching() {
        assert!(heading_starts_with_ref("3.2 Execution Planner", "3.2"));
        assert!(heading_starts_with_ref("3.2. Execution Planner", "3.2"));
        assert!(heading_starts_with_ref("3 Components", "3"));
        assert!(!heading_starts_with_ref("3.2 Execution Planner", "3.20"));
        assert!(!heading_starts_with_ref("13 Something", "3"));
    }
}
