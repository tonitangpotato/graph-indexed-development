//! Inference modules — automatic graph enrichment via algorithms + optional LLM.
//!
//! - `clustering`: Infomap-based code→component mapping
//! - `labeling`: LLM-powered semantic labeling for components and features
//! - `integration`: Merging infer results into a graph, output formatting
//!
//! The top-level [`run()`] function orchestrates the full pipeline:
//! clustering → labeling → [`InferResult`].

use std::path::PathBuf;

use anyhow::Result;

use crate::graph::Graph;

#[cfg(feature = "infomap")]
pub mod clustering;

#[cfg(feature = "infomap")]
pub mod labeling;

#[cfg(feature = "infomap")]
pub mod integration;

#[cfg(feature = "infomap")]
pub use clustering::{
    auto_config, auto_config_with_network, auto_name, add_dir_colocation_edges,
    build_network, cluster, map_to_components, relation_weight, run_clustering,
    ClusterConfig, ClusterMetrics, ClusterResult, RawCluster,
    WEIGHT_CALLS, WEIGHT_DEPENDS_ON, WEIGHT_DIR_COLOCATION, WEIGHT_IMPORTS,
    WEIGHT_STRUCTURAL, WEIGHT_TYPE_REF, MAX_DIR_SIZE_FOR_COLOCATION,
};

#[cfg(feature = "infomap")]
pub use integration::{merge_into_graph, format_output, InferResult, MergeStats, OutputFormat};

#[cfg(feature = "infomap")]
pub use labeling::{LabelingConfig, LabelingResult, SimpleLlm};

// ── InferLevel ─────────────────────────────────────────────────────────────

/// Inference depth level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferLevel {
    /// Only Infomap clustering → component layer.
    Component,
    /// Clustering + LLM → component + feature layers.
    Feature,
    /// Same as Feature (alias for completeness).
    All,
}

impl Default for InferLevel {
    fn default() -> Self {
        Self::All
    }
}

// ── InferConfig ────────────────────────────────────────────────────────────

/// Top-level configuration for the infer pipeline.
#[cfg(feature = "infomap")]
#[derive(Debug, Clone)]
pub struct InferConfig {
    /// Clustering configuration.
    pub clustering: ClusterConfig,
    /// Labeling configuration (None means --no-llm).
    pub labeling: Option<LabelingConfig>,
    /// Inference level.
    pub level: InferLevel,
    /// Output format.
    pub format: OutputFormat,
    /// Dry-run mode (don't write to graph).
    pub dry_run: bool,
    /// Source directory (for auto-extract trigger).
    pub source_dir: Option<PathBuf>,
}

#[cfg(feature = "infomap")]
impl Default for InferConfig {
    fn default() -> Self {
        Self {
            clustering: ClusterConfig::default(),
            labeling: Some(LabelingConfig::default()),
            level: InferLevel::All,
            format: OutputFormat::Summary,
            dry_run: false,
            source_dir: None,
        }
    }
}

// ── run() ──────────────────────────────────────────────────────────────────

/// Run the full infer pipeline: clustering → labeling → result.
///
/// Does NOT modify the input graph. Returns [`InferResult`] for the caller to merge.
/// - CLI calls this, then `merge_into_graph()`.
/// - GidHub calls this directly with its own merge strategy.
///
/// # Auto-extract (GOAL-5.5)
/// If the graph has no code nodes AND `config.source_dir` is `Some`, runs
/// `CodeGraph::extract_from_dir()` on a temporary clone. The caller's graph
/// is never mutated.
///
/// # Level behavior (GOAL-4.3)
/// - `Component`: clustering only, LLM skipped entirely.
/// - `Feature` / `All`: clustering + LLM labeling (if `llm` is provided).
///
/// # No-LLM mode (GUARD-3)
/// If `llm` is `None`, labeling is skipped. Component nodes retain their
/// auto-generated names from clustering.
#[cfg(feature = "infomap")]
pub async fn run(
    graph: &Graph,
    config: &InferConfig,
    llm: Option<&dyn SimpleLlm>,
) -> Result<InferResult> {
    // Step 0: Auto-extract if needed (GOAL-5.5)
    // If graph has no code nodes and source_dir is provided, extract into a working copy.
    #[allow(unused_assignments)]
    let auto_extracted: Option<Graph>;
    let effective_graph = if graph.code_nodes().is_empty() {
        if let Some(source_dir) = &config.source_dir {
            use crate::code_graph::CodeGraph;
            use crate::unify::codegraph_to_graph_nodes;

            eprintln!(
                "ℹ No code nodes found, auto-extracting from {:?}",
                source_dir
            );
            let code_graph = CodeGraph::extract_from_dir(source_dir);
            let (code_nodes, code_edges) = codegraph_to_graph_nodes(&code_graph, source_dir);

            let mut wg = graph.clone();
            for node in code_nodes {
                wg.add_node(node);
            }
            for edge in code_edges {
                wg.add_edge_dedup(edge);
            }
            auto_extracted = Some(wg);
            auto_extracted.as_ref().unwrap()
        } else {
            return Err(anyhow::anyhow!(
                "No code layer in graph. Run `gid extract` first or pass --source <dir>."
            ));
        }
    } else {
        auto_extracted = None; // keep `auto_extracted` alive for the borrow in the `if` branch
        _ = &auto_extracted;
        graph
    };

    // Step 1: Clustering (always runs)
    // Auto-tune config if using defaults
    let effective_clustering_config = {
        let file_count = effective_graph
            .nodes
            .iter()
            .filter(|n| {
                n.node_type.as_deref() == Some("file")
                    || (n.node_type.as_deref() == Some("code")
                        && n.node_kind.as_deref() == Some("File"))
            })
            .count();
        // If user specified default min_community_size, auto-tune based on graph properties
        if config.clustering.min_community_size == ClusterConfig::default().min_community_size {
            // Build network early to compute density-aware config
            let (net, _) = clustering::build_network(effective_graph);
            let mut auto = clustering::auto_config_with_network(file_count, &net);
            // Preserve user-specified overrides that auto_config doesn't know about
            if config.clustering.max_cluster_size.is_some() {
                auto.max_cluster_size = config.clustering.max_cluster_size;
            }
            if config.clustering.hierarchical {
                auto.hierarchical = true;
            }
            auto
        } else {
            config.clustering.clone()
        }
    };
    let cluster_result = clustering::cluster(effective_graph, &effective_clustering_config)?;

    if cluster_result.nodes.is_empty() {
        return Ok(InferResult::empty("No communities detected"));
    }

    // Step 2: Labeling (conditional on level + LLM availability)
    let labeling_config = config.labeling.clone().unwrap_or_default();
    let labeling_result = match config.level {
        InferLevel::Component => LabelingResult::empty(),
        InferLevel::Feature | InferLevel::All => {
            labeling::label(effective_graph, &cluster_result, llm, labeling_config).await?
        }
    };

    // Step 3: Build InferResult from both phases
    eprintln!("  📊 Labeling result: {} labels, {} features, {} feature_edges",
        labeling_result.component_labels.len(),
        labeling_result.features.len(),
        labeling_result.feature_edges.len());
    let result = InferResult::from_phases(&cluster_result, &labeling_result);
    eprintln!("  📊 InferResult: {} components, {} features, {} edges",
        result.component_nodes.len(),
        result.feature_nodes.len(),
        result.edges.len());
    Ok(result)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_level_default() {
        assert_eq!(InferLevel::default(), InferLevel::All);
    }

    #[cfg(feature = "infomap")]
    #[test]
    fn test_infer_config_default() {
        let config = InferConfig::default();
        assert_eq!(config.level, InferLevel::All);
        assert!(!config.dry_run);
        assert!(config.source_dir.is_none());
        assert!(config.labeling.is_some());
        assert_eq!(config.format, OutputFormat::Summary);
    }

    #[cfg(feature = "infomap")]
    #[tokio::test]
    async fn test_auto_extract_trigger() {
        use std::path::PathBuf;

        // An empty graph (no code nodes) + a real source_dir should trigger auto-extract.
        // Use the crate's own src directory as a source — it has .rs files.
        let graph = crate::graph::Graph::new();
        let config = InferConfig {
            source_dir: Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")),
            level: InferLevel::Component,
            labeling: None,
            ..InferConfig::default()
        };

        // run() should succeed (auto-extract finds .rs files, clusters them).
        let result = run(&graph, &config, None).await;
        assert!(
            result.is_ok(),
            "run() with empty graph + source_dir should auto-extract: {:?}",
            result.err(),
        );

        let infer_result = result.unwrap();
        // After auto-extract, clustering should find at least some components
        // (or return an empty result if <2 files — both are acceptable, not an error).
        // The key assertion: it did NOT return the "No code layer" error.
        assert!(
            infer_result.component_nodes.is_empty()
                || infer_result.component_nodes.iter().all(|n| n.source.as_deref() == Some("infer")),
            "All component nodes should have source=infer",
        );
    }

    #[cfg(feature = "infomap")]
    #[tokio::test]
    async fn test_auto_extract_no_source() {
        // An empty graph with NO source_dir should error.
        let graph = crate::graph::Graph::new();
        let config = InferConfig {
            source_dir: None,
            ..InferConfig::default()
        };

        let result = run(&graph, &config, None).await;
        assert!(result.is_err(), "run() with empty graph + no source_dir should error");

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("No code layer"),
            "Error should mention 'No code layer', got: {}",
            err_msg,
        );
    }
}
