//! Integration layer — merges [`InferResult`] into an existing [`Graph`].
//!
//! This module bridges the clustering + labeling phases with the graph
//! persistence layer. It handles incremental re-runs (clearing old infer
//! nodes), guard enforcement, and upsert semantics.

use std::collections::HashMap;

use crate::graph::{Edge, Graph, Node, NodeStatus};
use super::clustering::{ClusterMetrics, ClusterResult};
use super::labeling::{ComponentLabel, LabelingResult, TokenUsage};

// ── Constants ──────────────────────────────────────────────────────────────

/// Node types that belong to the code layer and must never be modified by infer.
const CODE_NODE_TYPES: &[&str] = &[
    "file", "class", "function", "module", "constant",
    "interface", "enum", "type_alias", "trait",
];

/// Check whether a node type string refers to a code-layer type.
fn is_code_node_type(node_type: Option<&str>) -> bool {
    node_type.map_or(false, |t| CODE_NODE_TYPES.contains(&t))
}

// ── MergeStats ─────────────────────────────────────────────────────────────

/// Statistics from a [`merge_into_graph`] operation.
#[derive(Debug, Clone, Default)]
pub struct MergeStats {
    /// Number of component nodes added.
    pub components_added: usize,
    /// Number of feature nodes added.
    pub features_added: usize,
    /// Number of edges added.
    pub edges_added: usize,
    /// Number of old infer nodes removed (incremental re-run).
    pub old_nodes_removed: usize,
    /// Number of old infer edges removed.
    pub old_edges_removed: usize,
    /// Number of nodes skipped (user-modified, preserved).
    pub nodes_skipped: usize,
}

// ── InferResult ────────────────────────────────────────────────────────────

/// Combined output of the clustering + labeling pipeline, ready to merge.
#[derive(Debug, Clone)]
pub struct InferResult {
    /// Component nodes (from clustering, optionally relabeled by LLM).
    pub component_nodes: Vec<Node>,
    /// Feature nodes (from LLM labeling).
    pub feature_nodes: Vec<Node>,
    /// All edges: component→code (contains), feature→component (contains),
    /// feature→feature (depends_on).
    pub edges: Vec<Edge>,
    /// Clustering metrics.
    pub cluster_metrics: ClusterMetrics,
    /// Token usage (zero if no LLM).
    pub token_usage: TokenUsage,
}

impl InferResult {
    /// Total number of inferred nodes (components + features).
    pub fn node_count(&self) -> usize {
        self.component_nodes.len() + self.feature_nodes.len()
    }

    /// Create an empty result (e.g. when no communities are detected).
    pub fn empty(reason: &str) -> Self {
        eprintln!("ℹ Empty InferResult: {}", reason);
        Self {
            component_nodes: Vec::new(),
            feature_nodes: Vec::new(),
            edges: Vec::new(),
            cluster_metrics: ClusterMetrics {
                codelength: 0.0,
                num_communities: 0,
                num_total: 0,
                ..Default::default()
            },
            token_usage: TokenUsage::default(),
        }
    }

    /// Build an [`InferResult`] from the two pipeline phases.
    ///
    /// 1. Starts with `cluster_result.nodes` as base component nodes.
    /// 2. Applies component labels from `labeling_result` (title + description).
    /// 3. Creates feature nodes from `labeling_result.features`.
    /// 4. Collects all edges (cluster edges + feature→component + feature→feature).
    pub fn from_phases(
        cluster_result: &ClusterResult,
        labeling_result: &LabelingResult,
    ) -> Self {
        // ── Step 1+2: Build component nodes with labels applied ────────────

        // Index labels by component_id for O(1) lookup.
        let label_map: HashMap<&str, &ComponentLabel> = labeling_result
            .component_labels
            .iter()
            .map(|l| (l.component_id.as_str(), l))
            .collect();

        let component_nodes: Vec<Node> = cluster_result
            .nodes
            .iter()
            .map(|node| {
                let mut n = node.clone();

                // Apply label if found.
                if let Some(label) = label_map.get(n.id.as_str()) {
                    n.title = label.title.clone();
                    n.description = Some(label.description.clone());
                }

                // Ensure source is always "infer".
                n.source = Some("infer".into());

                n
            })
            .collect();

        // ── Step 3: Build feature nodes from InferredFeature ───────────────

        let feature_nodes: Vec<Node> = labeling_result
            .features
            .iter()
            .map(|feat| {
                let mut node = Node::new(&feat.feature_id, &feat.title);
                node.description = Some(feat.description.clone());
                node.node_type = Some("feature".into());
                node.source = Some("infer".into());
                node.status = NodeStatus::Done;
                node.metadata.insert(
                    "components".into(),
                    serde_json::json!(feat.component_ids),
                );
                node
            })
            .collect();

        // ── Step 4: Collect all edges ──────────────────────────────────────

        let mut edges = cluster_result.edges.clone();

        // Feature → component "contains" edges from InferredFeature.component_ids.
        for feat in &labeling_result.features {
            for comp_id in &feat.component_ids {
                let mut edge = Edge::new(&feat.feature_id, comp_id, "contains");
                edge.metadata = Some(serde_json::json!({"source": "infer"}));
                edges.push(edge);
            }
        }

        // Feature → feature dependency edges.
        edges.extend(labeling_result.feature_edges.clone());

        Self {
            component_nodes,
            feature_nodes,
            edges,
            cluster_metrics: cluster_result.metrics.clone(),
            token_usage: labeling_result.token_usage.clone(),
        }
    }
}

// ── merge_into_graph ───────────────────────────────────────────────────────

/// Merge infer results into an existing graph.
///
/// Handles incremental re-runs: clears old `source=infer` nodes first.
/// Respects:
/// - **GUARD-1**: Never modifies code-layer nodes.
/// - **GUARD-2**: Never modifies user-created nodes (`source != "infer"`).
pub fn merge_into_graph(
    graph: &mut Graph,
    result: &InferResult,
    incremental: bool,
) -> MergeStats {
    let mut stats = MergeStats::default();

    // ── Step 1: Incremental cleanup ────────────────────────────────────────
    if incremental {
        // Collect IDs of nodes to remove: source == "infer" AND not a code node type.
        let ids_to_remove: Vec<String> = graph
            .nodes
            .iter()
            .filter(|n| {
                // GUARD-2: only remove nodes with source == "infer"
                n.source.as_deref() == Some("infer")
                    // GUARD-1: never remove code-layer nodes
                    && !is_code_node_type(n.node_type.as_deref())
            })
            .map(|n| n.id.clone())
            .collect();

        // Count edges that will be removed as a side-effect.
        // Graph::remove_node auto-removes associated edges.
        for id in &ids_to_remove {
            let edge_count_before = graph.edges.len();
            graph.remove_node(id);
            let edges_removed = edge_count_before - graph.edges.len();
            stats.old_edges_removed += edges_removed;
            stats.old_nodes_removed += 1;
        }
    }

    // ── Step 2: Add component nodes ────────────────────────────────────────
    add_nodes(
        graph,
        &result.component_nodes,
        &mut stats.components_added,
        &mut stats.nodes_skipped,
    );

    // ── Step 3: Add feature nodes ──────────────────────────────────────────
    add_nodes(
        graph,
        &result.feature_nodes,
        &mut stats.features_added,
        &mut stats.nodes_skipped,
    );

    // ── Step 4: Add edges ──────────────────────────────────────────────────
    for edge in &result.edges {
        if graph.add_edge_dedup(edge.clone()) {
            stats.edges_added += 1;
        }
    }

    stats
}

/// Add nodes to the graph with guard checks and upsert semantics.
fn add_nodes(
    graph: &mut Graph,
    nodes: &[Node],
    added: &mut usize,
    skipped: &mut usize,
) {
    for node in nodes {
        // GUARD-1: Refuse to add/overwrite code-layer nodes.
        if is_code_node_type(node.node_type.as_deref()) {
            eprintln!(
                "⚠ Skipping node '{}': node_type '{}' is a code-layer type",
                node.id,
                node.node_type.as_deref().unwrap_or("?"),
            );
            *skipped += 1;
            continue;
        }

        // Check if a node with this ID already exists.
        if let Some(existing) = graph.get_node(&node.id) {
            if existing.source.as_deref() == Some("infer") {
                // Same source — replace entirely (remove old, add new).
                graph.remove_node(&node.id);
                graph.add_node(node.clone());
                *added += 1;
            } else {
                // GUARD-2: User-created node — upsert merge.
                // Infer keys win on conflict, but preserve user metadata keys
                // not present in infer output.
                upsert_node(graph, node);
                *skipped += 1;
            }
        } else {
            // New node — just add.
            graph.add_node(node.clone());
            *added += 1;
        }
    }
}

/// Merge infer data into an existing user-created node.
///
/// Updates title, description, and merges metadata (infer keys win on conflict,
/// but user-only keys are preserved).
fn upsert_node(graph: &mut Graph, infer_node: &Node) {
    if let Some(existing) = graph.get_node_mut(&infer_node.id) {
        // Update title and description from infer.
        existing.title = infer_node.title.clone();
        existing.description = infer_node.description.clone();

        // Merge metadata: infer keys overwrite, user-only keys preserved.
        for (key, value) in &infer_node.metadata {
            existing.metadata.insert(key.clone(), value.clone());
        }
    }
}

// ── Output formatting ──────────────────────────────────────────────────────

/// Output format for displaying infer results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Human-readable summary (default CLI output).
    Summary,
    /// Full YAML of generated nodes/edges (--dry-run).
    Yaml,
    /// JSON summary for programmatic use (--format json).
    Json,
}

/// Format a number with comma separators (e.g., 12345 → "12,345").
fn format_number(n: usize) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result
}

/// Format an [`InferResult`] for display.
///
/// - **Summary**: Human-readable text: component/feature counts, feature list, token usage, metrics.
/// - **Yaml**: serde_yaml of the nodes + edges for dry-run preview.
/// - **Json**: Stable schema for batch/programmatic consumers.
pub fn format_output(result: &InferResult, format: OutputFormat) -> String {
    match format {
        OutputFormat::Summary => format_summary(result),
        OutputFormat::Yaml => format_yaml(result),
        OutputFormat::Json => format_json(result),
    }
}

/// Build component size map: component_id → count of "contains" edges from that component.
fn component_sizes(result: &InferResult) -> HashMap<&str, usize> {
    let component_ids: std::collections::HashSet<&str> = result
        .component_nodes
        .iter()
        .map(|n| n.id.as_str())
        .collect();

    let mut sizes: HashMap<&str, usize> = HashMap::new();
    for edge in &result.edges {
        if edge.relation == "contains" && component_ids.contains(edge.from.as_str()) {
            *sizes.entry(edge.from.as_str()).or_insert(0) += 1;
        }
    }
    sizes
}

/// Extract feature component IDs from metadata or edges.
fn feature_components(result: &InferResult, feature_id: &str) -> Vec<String> {
    // Try metadata["components"] first.
    if let Some(feat_node) = result.feature_nodes.iter().find(|n| n.id == feature_id) {
        if let Some(comps) = feat_node.metadata.get("components") {
            if let Some(arr) = comps.as_array() {
                let ids: Vec<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                if !ids.is_empty() {
                    return ids;
                }
            }
        }
    }

    // Fallback: count "contains" edges from this feature to component nodes.
    let component_ids: std::collections::HashSet<&str> = result
        .component_nodes
        .iter()
        .map(|n| n.id.as_str())
        .collect();

    result
        .edges
        .iter()
        .filter(|e| e.from == feature_id && e.relation == "contains" && component_ids.contains(e.to.as_str()))
        .map(|e| e.to.clone())
        .collect()
}

/// Count total code files: count unique "contains" edge targets from component nodes
/// that are not themselves component or feature nodes.
fn count_code_files(result: &InferResult) -> usize {
    let component_ids: std::collections::HashSet<&str> = result
        .component_nodes
        .iter()
        .map(|n| n.id.as_str())
        .collect();
    let feature_ids: std::collections::HashSet<&str> = result
        .feature_nodes
        .iter()
        .map(|n| n.id.as_str())
        .collect();

    let mut code_files = std::collections::HashSet::new();
    for edge in &result.edges {
        if edge.relation == "contains"
            && component_ids.contains(edge.from.as_str())
            && !component_ids.contains(edge.to.as_str())
            && !feature_ids.contains(edge.to.as_str())
        {
            code_files.insert(edge.to.as_str());
        }
    }
    code_files.len()
}

fn format_summary(result: &InferResult) -> String {
    let num_components = result.component_nodes.len();
    let num_features = result.feature_nodes.len();
    let num_files = count_code_files(result);
    let sizes = component_sizes(result);

    let mut out = String::new();

    // Header line
    out.push_str(&format!(
        "Inferred {} component{}, {} feature{} from {} code file{}\n",
        num_components,
        if num_components == 1 { "" } else { "s" },
        num_features,
        if num_features == 1 { "" } else { "s" },
        num_files,
        if num_files == 1 { "" } else { "s" },
    ));

    // Components section
    if !result.component_nodes.is_empty() {
        out.push_str("\nComponents:\n");
        for node in &result.component_nodes {
            let size = sizes.get(node.id.as_str()).copied().unwrap_or(0);
            out.push_str(&format!(
                "  • {} — {} ({} file{})\n",
                node.id,
                node.title,
                size,
                if size == 1 { "" } else { "s" },
            ));
        }
    }

    // Features section
    if !result.feature_nodes.is_empty() {
        out.push_str("\nFeatures:\n");
        for node in &result.feature_nodes {
            let comps = feature_components(result, &node.id);
            let comp_count = comps.len();
            out.push_str(&format!(
                "  • {} — {} ({} component{})\n",
                node.id,
                node.title,
                comp_count,
                if comp_count == 1 { "" } else { "s" },
            ));
        }
    }

    // Clustering metrics
    out.push_str(&format!(
        "\nClustering: {} communit{}, codelength = {:.3}\n",
        result.cluster_metrics.num_communities,
        if result.cluster_metrics.num_communities == 1 {
            "y"
        } else {
            "ies"
        },
        result.cluster_metrics.codelength,
    ));

    // Token usage (omit if total is 0)
    if result.token_usage.total_tokens > 0 {
        out.push_str(&format!(
            "Tokens: {} (naming: {}, features: {})\n",
            format_number(result.token_usage.total_tokens),
            format_number(result.token_usage.naming_tokens),
            format_number(result.token_usage.feature_tokens),
        ));
    }

    out
}

fn format_yaml(result: &InferResult) -> String {
    #[derive(serde::Serialize)]
    struct YamlOutput {
        nodes: Vec<Node>,
        edges: Vec<Edge>,
    }

    let output = YamlOutput {
        nodes: result
            .component_nodes
            .iter()
            .chain(&result.feature_nodes)
            .cloned()
            .collect(),
        edges: result.edges.clone(),
    };

    serde_yaml::to_string(&output).unwrap_or_else(|e| format!("YAML serialization error: {e}"))
}

fn format_json(result: &InferResult) -> String {
    let sizes = component_sizes(result);

    let component_list: Vec<serde_json::Value> = result
        .component_nodes
        .iter()
        .map(|n| {
            let size = sizes.get(n.id.as_str()).copied().unwrap_or(0);
            serde_json::json!({
                "id": n.id,
                "title": n.title,
                "size": size,
            })
        })
        .collect();

    let feature_list: Vec<serde_json::Value> = result
        .feature_nodes
        .iter()
        .map(|n| {
            let comps = feature_components(result, &n.id);
            serde_json::json!({
                "id": n.id,
                "title": n.title,
                "components": comps,
            })
        })
        .collect();

    let json = serde_json::json!({
        "components": result.component_nodes.len(),
        "features": result.feature_nodes.len(),
        "edges": result.edges.len(),
        "metrics": {
            "codelength": result.cluster_metrics.codelength,
            "num_communities": result.cluster_metrics.num_communities,
        },
        "token_usage": {
            "naming_tokens": result.token_usage.naming_tokens,
            "feature_tokens": result.token_usage.feature_tokens,
            "total_tokens": result.token_usage.total_tokens,
        },
        "component_list": component_list,
        "feature_list": feature_list,
    });

    serde_json::to_string_pretty(&json).unwrap_or_else(|e| format!("JSON serialization error: {e}"))
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Edge, Graph, Node, NodeStatus};
    use crate::infer::clustering::{ClusterMetrics, ClusterResult};
    use crate::infer::labeling::{
        ComponentLabel, InferredFeature, LabelingResult, TokenUsage,
    };

    /// Helper: build a minimal ClusterResult with N components.
    fn make_cluster_result(n: usize) -> ClusterResult {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        for i in 0..n {
            let id = format!("infer:component:{}", i);
            let mut node = Node::new(&id, &format!("component-{}", i));
            node.node_type = Some("component".into());
            node.source = Some("infer".into());
            nodes.push(node);

            // One membership edge per component.
            let member_id = format!("file:src/mod_{}.rs", i);
            let mut edge = Edge::new(&id, &member_id, "contains");
            edge.metadata = Some(serde_json::json!({"source": "infer"}));
            edges.push(edge);
        }

        ClusterResult {
            nodes,
            edges,
            metrics: ClusterMetrics {
                codelength: 3.14,
                num_communities: n,
                num_total: n * 5,
                ..Default::default()
            },
        }
    }

    /// Helper: build a minimal LabelingResult.
    fn make_labeling_result() -> LabelingResult {
        LabelingResult {
            component_labels: vec![
                ComponentLabel {
                    component_id: "infer:component:0".into(),
                    title: "Auth Module".into(),
                    description: "Handles authentication".into(),
                },
            ],
            features: vec![
                InferredFeature {
                    feature_id: "infer:feature:auth".into(),
                    title: "Authentication".into(),
                    description: "User authentication and authorization".into(),
                    component_ids: vec!["infer:component:0".into()],
                },
            ],
            feature_edges: vec![],
            token_usage: TokenUsage {
                naming_tokens: 100,
                feature_tokens: 200,
                total_tokens: 300,
            },
        }
    }

    #[test]
    fn test_infer_result_empty() {
        let r = InferResult::empty("no clusters");
        assert_eq!(r.node_count(), 0);
        assert!(r.edges.is_empty());
        assert_eq!(r.cluster_metrics.num_communities, 0);
    }

    #[test]
    fn test_infer_result_from_phases() {
        let cluster = make_cluster_result(2);
        let labeling = make_labeling_result();
        let result = InferResult::from_phases(&cluster, &labeling);

        // 2 component nodes.
        assert_eq!(result.component_nodes.len(), 2);
        // Component 0 should have LLM label applied.
        assert_eq!(result.component_nodes[0].title, "Auth Module");
        assert_eq!(
            result.component_nodes[0].description.as_deref(),
            Some("Handles authentication")
        );
        // Component 1 keeps auto-generated name.
        assert_eq!(result.component_nodes[1].title, "component-1");
        // All components have source = "infer".
        for n in &result.component_nodes {
            assert_eq!(n.source.as_deref(), Some("infer"));
        }

        // 1 feature node.
        assert_eq!(result.feature_nodes.len(), 1);
        assert_eq!(result.feature_nodes[0].title, "Authentication");
        assert_eq!(result.feature_nodes[0].status, NodeStatus::Done);
        assert_eq!(
            result.feature_nodes[0].node_type.as_deref(),
            Some("feature")
        );

        // Edges: 2 cluster contains + 1 feature→component contains.
        assert_eq!(result.edges.len(), 3);
        assert_eq!(result.node_count(), 3);
    }

    #[test]
    fn test_merge_adds_nodes_to_empty_graph() {
        let mut graph = Graph::new();
        let cluster = make_cluster_result(2);
        let labeling = make_labeling_result();
        let result = InferResult::from_phases(&cluster, &labeling);

        let stats = merge_into_graph(&mut graph, &result, false);

        assert_eq!(stats.components_added, 2);
        assert_eq!(stats.features_added, 1);
        assert_eq!(stats.edges_added, 3);
        assert_eq!(stats.old_nodes_removed, 0);
        assert_eq!(stats.nodes_skipped, 0);

        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.edges.len(), 3);
    }

    #[test]
    fn test_merge_preserves_existing_user_nodes() {
        let mut graph = Graph::new();
        // Add a user-created task node.
        let mut user_node = Node::new("my-task", "My Task");
        user_node.node_type = Some("task".into());
        user_node.source = Some("manual".into());
        graph.add_node(user_node);

        let cluster = make_cluster_result(1);
        let labeling = LabelingResult::empty();
        let result = InferResult::from_phases(&cluster, &labeling);

        let stats = merge_into_graph(&mut graph, &result, true);

        // User node should be preserved.
        assert!(graph.get_node("my-task").is_some());
        assert_eq!(
            graph.get_node("my-task").unwrap().source.as_deref(),
            Some("manual")
        );
        assert_eq!(stats.components_added, 1);
    }

    #[test]
    fn test_merge_incremental_clears_old_infer_nodes() {
        let mut graph = Graph::new();

        // First merge.
        let cluster1 = make_cluster_result(2);
        let labeling1 = LabelingResult::empty();
        let result1 = InferResult::from_phases(&cluster1, &labeling1);
        merge_into_graph(&mut graph, &result1, false);
        assert_eq!(graph.nodes.len(), 2);

        // Second merge (incremental) — old nodes should be removed first.
        let cluster2 = make_cluster_result(3);
        let labeling2 = LabelingResult::empty();
        let result2 = InferResult::from_phases(&cluster2, &labeling2);
        let stats = merge_into_graph(&mut graph, &result2, true);

        assert_eq!(stats.old_nodes_removed, 2);
        assert_eq!(stats.components_added, 3);
        // Only the 3 new nodes should remain.
        assert_eq!(graph.nodes.len(), 3);
    }

    #[test]
    fn test_merge_skips_code_nodes() {
        let mut graph = Graph::new();

        // Craft a result that accidentally has a code-type node.
        let mut bad_node = Node::new("file:src/main.rs", "main.rs");
        bad_node.node_type = Some("file".into());
        bad_node.source = Some("infer".into());

        let result = InferResult {
            component_nodes: vec![bad_node],
            feature_nodes: Vec::new(),
            edges: Vec::new(),
            cluster_metrics: ClusterMetrics {
                codelength: 0.0,
                num_communities: 0,
                num_total: 0,
                ..Default::default()
            },
            token_usage: TokenUsage::default(),
        };

        let stats = merge_into_graph(&mut graph, &result, false);

        assert_eq!(stats.nodes_skipped, 1);
        assert_eq!(stats.components_added, 0);
        assert!(graph.nodes.is_empty());
    }

    #[test]
    fn test_merge_skips_user_nodes_on_incremental() {
        let mut graph = Graph::new();

        // Add a user node that happens to share an ID with an infer node.
        let mut user_node = Node::new("infer:component:0", "User Override");
        user_node.node_type = Some("component".into());
        user_node.source = Some("manual".into());
        graph.add_node(user_node);

        // Incremental merge should NOT remove the user node.
        let cluster = make_cluster_result(1);
        let labeling = LabelingResult::empty();
        let result = InferResult::from_phases(&cluster, &labeling);

        let stats = merge_into_graph(&mut graph, &result, true);

        // User node preserved, but upserted (title/desc updated).
        assert_eq!(stats.old_nodes_removed, 0);
        assert_eq!(stats.nodes_skipped, 1);
        let node = graph.get_node("infer:component:0").unwrap();
        assert_eq!(node.source.as_deref(), Some("manual"));
    }

    #[test]
    fn test_is_code_node_type() {
        assert!(is_code_node_type(Some("file")));
        assert!(is_code_node_type(Some("function")));
        assert!(is_code_node_type(Some("class")));
        assert!(is_code_node_type(Some("trait")));
        assert!(!is_code_node_type(Some("component")));
        assert!(!is_code_node_type(Some("feature")));
        assert!(!is_code_node_type(Some("task")));
        assert!(!is_code_node_type(None));
    }

    #[test]
    fn test_edge_dedup_with_incremental() {
        let mut graph = Graph::new();

        // Add a file node so edges have a valid target.
        let mut file_node = Node::new("file:src/mod_0.rs", "mod_0.rs");
        file_node.node_type = Some("file".into());
        file_node.source = Some("extract".into());
        graph.add_node(file_node);

        let cluster = make_cluster_result(1);
        let labeling = LabelingResult::empty();
        let result = InferResult::from_phases(&cluster, &labeling);

        // First merge (incremental).
        merge_into_graph(&mut graph, &result, true);
        let edge_count_after_first = graph.edges.len();

        // Second merge (incremental) — old infer nodes removed, then re-added.
        // Edges should not accumulate.
        merge_into_graph(&mut graph, &result, true);
        assert_eq!(graph.edges.len(), edge_count_after_first);
    }

    #[test]
    fn test_edge_dedup_same_edges_not_duplicated() {
        let mut graph = Graph::new();

        // Add a user node that won't be removed.
        let mut user_node = Node::new("user:comp", "User Component");
        user_node.node_type = Some("component".into());
        user_node.source = Some("manual".into());
        graph.add_node(user_node);

        // Add an edge manually.
        graph.add_edge(Edge::new("user:comp", "some-target", "depends_on"));

        // Try to add the same edge via merge — should be deduped.
        let result = InferResult {
            component_nodes: Vec::new(),
            feature_nodes: Vec::new(),
            edges: vec![Edge::new("user:comp", "some-target", "depends_on")],
            cluster_metrics: ClusterMetrics {
                codelength: 0.0,
                num_communities: 0,
                num_total: 0,
                ..Default::default()
            },
            token_usage: TokenUsage::default(),
        };

        let stats = merge_into_graph(&mut graph, &result, false);
        assert_eq!(stats.edges_added, 0);
        assert_eq!(graph.edges.len(), 1);
    }

    // ── format_output tests ────────────────────────────────────────────────

    /// Helper: build an InferResult with 2 components and 1 feature for format tests.
    fn make_format_result() -> InferResult {
        let cluster = make_cluster_result(2);
        let labeling = make_labeling_result();
        InferResult::from_phases(&cluster, &labeling)
    }

    #[test]
    fn test_format_summary_basic() {
        let result = make_format_result();
        let summary = format_output(&result, OutputFormat::Summary);

        assert!(summary.contains("2 components"));
        assert!(summary.contains("1 feature"));
        assert!(summary.contains("Auth Module"));
        assert!(summary.contains("Authentication"));
        assert!(summary.contains("3.140"));
        assert!(summary.contains("infer:component:0"));
        assert!(summary.contains("infer:component:1"));
        assert!(summary.contains("infer:feature:auth"));
    }

    #[test]
    fn test_format_yaml_parseable() {
        let result = make_format_result();
        let yaml_str = format_output(&result, OutputFormat::Yaml);

        let parsed: serde_yaml::Value =
            serde_yaml::from_str(&yaml_str).expect("YAML should be parseable");
        assert!(parsed.get("nodes").is_some(), "YAML should have 'nodes' key");
        assert!(parsed.get("edges").is_some(), "YAML should have 'edges' key");

        // Verify node count: 2 components + 1 feature = 3
        let nodes = parsed.get("nodes").unwrap().as_sequence().unwrap();
        assert_eq!(nodes.len(), 3);
    }

    #[test]
    fn test_format_json_schema() {
        let result = make_format_result();
        let json_str = format_output(&result, OutputFormat::Json);

        let parsed: serde_json::Value =
            serde_json::from_str(&json_str).expect("JSON should be parseable");

        // Verify top-level keys
        assert!(parsed.get("components").is_some());
        assert!(parsed.get("features").is_some());
        assert!(parsed.get("edges").is_some());
        assert!(parsed.get("metrics").is_some());
        assert!(parsed.get("token_usage").is_some());
        assert!(parsed.get("component_list").is_some());
        assert!(parsed.get("feature_list").is_some());

        // Verify counts
        assert_eq!(parsed["components"].as_u64().unwrap(), 2);
        assert_eq!(parsed["features"].as_u64().unwrap(), 1);
        assert_eq!(parsed["edges"].as_u64().unwrap(), 3);

        // Verify metrics
        assert!(parsed["metrics"]["codelength"].as_f64().unwrap() > 3.0);
        assert_eq!(parsed["metrics"]["num_communities"].as_u64().unwrap(), 2);

        // Verify token usage
        assert_eq!(parsed["token_usage"]["naming_tokens"].as_u64().unwrap(), 100);
        assert_eq!(parsed["token_usage"]["feature_tokens"].as_u64().unwrap(), 200);
        assert_eq!(parsed["token_usage"]["total_tokens"].as_u64().unwrap(), 300);

        // Verify component_list
        let comp_list = parsed["component_list"].as_array().unwrap();
        assert_eq!(comp_list.len(), 2);
        assert!(comp_list[0].get("id").is_some());
        assert!(comp_list[0].get("title").is_some());
        assert!(comp_list[0].get("size").is_some());

        // Verify feature_list
        let feat_list = parsed["feature_list"].as_array().unwrap();
        assert_eq!(feat_list.len(), 1);
        assert!(feat_list[0].get("id").is_some());
        assert!(feat_list[0].get("title").is_some());
        assert!(feat_list[0].get("components").is_some());
    }

    #[test]
    fn test_format_summary_empty() {
        let result = InferResult::empty("test");
        let summary = format_output(&result, OutputFormat::Summary);

        assert!(summary.contains("0 components"));
        assert!(summary.contains("0 features"));
        // Should not contain Components: or Features: sections
        assert!(!summary.contains("Components:"));
        assert!(!summary.contains("Features:"));
    }

    #[test]
    fn test_format_summary_no_tokens() {
        let result = InferResult {
            component_nodes: vec![],
            feature_nodes: vec![],
            edges: vec![],
            cluster_metrics: ClusterMetrics {
                codelength: 1.0,
                num_communities: 0,
                num_total: 0,
                ..Default::default()
            },
            token_usage: TokenUsage {
                naming_tokens: 0,
                feature_tokens: 0,
                total_tokens: 0,
            },
        };
        let summary = format_output(&result, OutputFormat::Summary);

        // Token line should be omitted when total is 0
        assert!(!summary.contains("Tokens:"));
    }

    // ── Schema tests ───────────────────────────────────────────────────────

    #[test]
    fn test_schema_component() {
        // Build a cluster result with metadata (as map_to_components would produce).
        let mut cluster = make_cluster_result(2);
        for (i, node) in cluster.nodes.iter_mut().enumerate() {
            node.metadata
                .insert("flow".into(), serde_json::json!(0.5));
            node.metadata
                .insert("size".into(), serde_json::json!(i + 1));
        }

        let labeling = make_labeling_result();
        let result = InferResult::from_phases(&cluster, &labeling);

        // Component 0 got an LLM label applied.
        let comp0 = &result.component_nodes[0];
        assert_eq!(comp0.id, "infer:component:0");
        assert_eq!(comp0.node_type.as_deref(), Some("component"));
        assert_eq!(comp0.source.as_deref(), Some("infer"));
        assert_eq!(comp0.title, "Auth Module");
        assert_eq!(
            comp0.description.as_deref(),
            Some("Handles authentication"),
        );
        // Clustering metadata should be preserved through from_phases.
        assert!(comp0.metadata.contains_key("flow"));
        assert!(comp0.metadata.contains_key("size"));

        // Component 1 kept auto-generated name (no label match).
        let comp1 = &result.component_nodes[1];
        assert_eq!(comp1.node_type.as_deref(), Some("component"));
        assert_eq!(comp1.source.as_deref(), Some("infer"));
        assert_eq!(comp1.title, "component-1");
    }

    #[test]
    fn test_schema_feature() {
        let cluster = make_cluster_result(2);
        let labeling = make_labeling_result();
        let result = InferResult::from_phases(&cluster, &labeling);

        assert_eq!(result.feature_nodes.len(), 1);
        let feat = &result.feature_nodes[0];
        assert_eq!(feat.id, "infer:feature:auth");
        assert_eq!(feat.node_type.as_deref(), Some("feature"));
        assert_eq!(feat.source.as_deref(), Some("infer"));
        assert_eq!(feat.status, NodeStatus::Done);
        assert_eq!(feat.title, "Authentication");
        assert_eq!(
            feat.description.as_deref(),
            Some("User authentication and authorization"),
        );
        // Feature metadata should contain component IDs.
        let comps = feat.metadata.get("components").expect("should have components key");
        let comp_arr = comps.as_array().expect("components should be an array");
        assert_eq!(comp_arr.len(), 1);
        assert_eq!(comp_arr[0].as_str(), Some("infer:component:0"));
    }

    // ── Level tests ────────────────────────────────────────────────────────

    #[test]
    fn test_level_component_only() {
        // Simulate InferLevel::Component: labeling is empty (no LLM call).
        let cluster = make_cluster_result(3);
        let labeling = LabelingResult::empty();
        let result = InferResult::from_phases(&cluster, &labeling);

        // Should have components but NO features.
        assert_eq!(result.component_nodes.len(), 3);
        assert!(result.feature_nodes.is_empty(), "Component-only level should produce no features");
        // Edges: 3 cluster contains only (no feature→component edges).
        assert_eq!(result.edges.len(), 3);
        for node in &result.component_nodes {
            assert_eq!(node.node_type.as_deref(), Some("component"));
            assert_eq!(node.source.as_deref(), Some("infer"));
        }
    }

    #[test]
    fn test_level_feature_auto_chains() {
        // Simulate InferLevel::Feature/All: labeling has both labels and features.
        let cluster = make_cluster_result(2);
        let labeling = LabelingResult {
            component_labels: vec![
                ComponentLabel {
                    component_id: "infer:component:0".into(),
                    title: "Auth Module".into(),
                    description: "Authentication logic".into(),
                },
                ComponentLabel {
                    component_id: "infer:component:1".into(),
                    title: "API Layer".into(),
                    description: "HTTP API routes".into(),
                },
            ],
            features: vec![
                InferredFeature {
                    feature_id: "infer:feature:auth".into(),
                    title: "Authentication".into(),
                    description: "User auth flow".into(),
                    component_ids: vec!["infer:component:0".into()],
                },
                InferredFeature {
                    feature_id: "infer:feature:api".into(),
                    title: "REST API".into(),
                    description: "REST endpoints".into(),
                    component_ids: vec!["infer:component:1".into()],
                },
            ],
            feature_edges: vec![
                Edge::new("infer:feature:api", "infer:feature:auth", "depends_on"),
            ],
            token_usage: TokenUsage {
                naming_tokens: 50,
                feature_tokens: 150,
                total_tokens: 200,
            },
        };

        let result = InferResult::from_phases(&cluster, &labeling);

        // Should have both components and features.
        assert_eq!(result.component_nodes.len(), 2);
        assert_eq!(result.feature_nodes.len(), 2);

        // Components should have LLM labels applied.
        assert_eq!(result.component_nodes[0].title, "Auth Module");
        assert_eq!(result.component_nodes[1].title, "API Layer");

        // Features should exist with correct types.
        for feat in &result.feature_nodes {
            assert_eq!(feat.node_type.as_deref(), Some("feature"));
            assert_eq!(feat.source.as_deref(), Some("infer"));
            assert_eq!(feat.status, NodeStatus::Done);
        }

        // Edges: 2 cluster contains + 2 feature→component contains + 1 feature→feature depends_on = 5
        assert_eq!(result.edges.len(), 5);

        // Verify the dependency edge exists.
        let dep_edges: Vec<_> = result.edges.iter()
            .filter(|e| e.relation == "depends_on")
            .collect();
        assert_eq!(dep_edges.len(), 1);
        assert_eq!(dep_edges[0].from, "infer:feature:api");
        assert_eq!(dep_edges[0].to, "infer:feature:auth");

        // Token usage should be propagated.
        assert_eq!(result.token_usage.total_tokens, 200);
    }
}
