//! Infomap-based community detection on code graphs.
//!
//! This module takes a [`Graph`] of code nodes (files, functions, classes, etc.),
//! builds a weighted network at file granularity, runs Infomap optimization,
//! and returns a [`ClusterResult`] containing inferred component `Node`s and
//! membership `Edge`s. The input graph is never mutated.

use std::collections::HashMap;

use anyhow::Result;
use infomap_rs::{Infomap, Network};

use crate::graph::{Edge, Graph, Node};

// ── Edge-relation weights ──────────────────────────────────────────────────

/// Weight for "calls" edges — the strongest coupling signal.
pub const WEIGHT_CALLS: f64 = 1.0;
/// Weight for "imports" edges.
pub const WEIGHT_IMPORTS: f64 = 0.8;
/// Weight for type-reference style edges: `type_reference`, `inherits`, `implements`, `uses`.
pub const WEIGHT_TYPE_REF: f64 = 0.5;
/// Weight for structural containment edges: `defined_in`, `contains`, `belongs_to`.
pub const WEIGHT_STRUCTURAL: f64 = 0.2;
/// Weight for generic "depends_on" edges.
pub const WEIGHT_DEPENDS_ON: f64 = 0.4;
/// Weight for synthetic directory co-location edges between files in the same directory.
pub const WEIGHT_DIR_COLOCATION: f64 = 0.3;
/// Maximum directory size for pairwise co-location edges. Directories with more
/// files than this skip co-location to avoid O(n²) edge explosion.
pub const MAX_DIR_SIZE_FOR_COLOCATION: usize = 50;

/// Map an edge relation string to its clustering weight.
///
/// Unknown relations return `0.0` and are effectively ignored.
pub fn relation_weight(relation: &str) -> f64 {
    match relation {
        "calls" => WEIGHT_CALLS,
        "imports" => WEIGHT_IMPORTS,
        "type_reference" | "inherits" | "implements" | "uses" => WEIGHT_TYPE_REF,
        "defined_in" | "contains" | "belongs_to" => WEIGHT_STRUCTURAL,
        "depends_on" => WEIGHT_DEPENDS_ON,
        _ => 0.0,
    }
}

// ── Configuration ──────────────────────────────────────────────────────────

/// Configuration for the clustering algorithm.
#[derive(Debug, Clone)]
pub struct ClusterConfig {
    /// Teleportation rate for the random walker (default: 0.05).
    /// Lower values keep the walker inside communities longer, improving
    /// detection on structured code graphs. Web-scale graphs use 0.15;
    /// code dependency graphs work better at 0.01–0.05.
    pub teleportation_rate: f64,
    /// Number of Infomap optimization trials (default: 10).
    pub num_trials: u32,
    /// Minimum community size; smaller clusters are dissolved (default: 2).
    pub min_community_size: usize,
    /// Whether to run hierarchical decomposition (default: false).
    pub hierarchical: bool,
    /// Weight for synthetic directory co-location edges (default: 0.3).
    /// Set to 0.0 to disable directory co-location.
    pub dir_colocation_weight: f64,
    /// Random seed for reproducibility (default: 42).
    pub seed: u64,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            teleportation_rate: 0.05,
            num_trials: 10,
            min_community_size: 2,
            dir_colocation_weight: WEIGHT_DIR_COLOCATION,
            hierarchical: false,
            seed: 42,
        }
    }
}

// ── Result types ───────────────────────────────────────────────────────────

/// A raw cluster before it is mapped to graph components.
#[derive(Debug, Clone)]
pub struct RawCluster {
    /// Cluster identifier.
    pub id: usize,
    /// IDs of the graph nodes belonging to this cluster.
    pub member_ids: Vec<String>,
    /// Infomap flow through this cluster.
    pub flow: f64,
    /// Parent cluster id for hierarchical mode.
    pub parent: Option<usize>,
    /// Child cluster ids for hierarchical mode.
    pub children: Vec<usize>,
}

/// Summary metrics from a clustering run.
#[derive(Debug, Clone, Default)]
pub struct ClusterMetrics {
    /// Map equation codelength (lower is better).
    pub codelength: f64,
    /// Number of communities detected.
    pub num_communities: usize,
    /// Total number of nodes in the network.
    pub num_total: usize,
    /// Diagnostic: number of Infomap modules with size < min_community_size (before reassignment).
    pub orphan_count_raw: usize,
    /// Diagnostic: orphans successfully merged via edge affinity.
    pub orphans_merged_by_affinity: usize,
    /// Diagnostic: orphans assigned via directory fallback.
    pub orphans_assigned_by_dir: usize,
    /// Diagnostic: final number of size=1 clusters after all reassignment.
    pub singleton_clusters_final: usize,
}

/// The output of clustering: new component nodes, membership edges, and metrics.
#[derive(Debug, Clone)]
pub struct ClusterResult {
    /// Component nodes inferred by clustering.
    pub nodes: Vec<Node>,
    /// Edges connecting components to their member nodes (and parent→child).
    pub edges: Vec<Edge>,
    /// Summary metrics.
    pub metrics: ClusterMetrics,
}

impl ClusterResult {
    /// Create an empty result with zeroed metrics.
    pub fn empty() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            metrics: ClusterMetrics {
                codelength: 0.0,
                num_communities: 0,
                num_total: 0,
                ..Default::default()
            },
        }
    }
}

// ── Network construction ───────────────────────────────────────────────────

/// Build an Infomap [`Network`] from a [`Graph`], collapsing non-file nodes
/// onto their parent files.
///
/// Returns the network and a vec mapping network indices back to node ID strings.
pub fn build_network(graph: &Graph) -> (Network, Vec<String>) {
    // 1. Collect file nodes and assign indices.
    let mut id_to_idx: HashMap<&str, usize> = HashMap::new();
    let mut idx_to_id: Vec<String> = Vec::new();

    for node in &graph.nodes {
        // Accept both `node_type: file` (from gid extract) and
        // `node_type: code, node_kind: File` (from unified codegraph_to_graph_nodes).
        let is_file = node.node_type.as_deref() == Some("file")
            || (node.node_type.as_deref() == Some("code")
                && node.node_kind.as_deref() == Some("File"));
        if is_file {
            let idx = idx_to_id.len();
            id_to_idx.insert(&node.id, idx);
            idx_to_id.push(node.id.clone());
        }
    }

    // 2. Map non-file nodes to their parent file index.
    let mut node_to_file_idx: HashMap<&str, usize> = HashMap::new();
    for node in &graph.nodes {
        let is_file = node.node_type.as_deref() == Some("file")
            || (node.node_type.as_deref() == Some("code")
                && node.node_kind.as_deref() == Some("File"));
        if is_file {
            continue;
        }
        // Try file_path field → construct "file:{file_path}" → look up in id_to_idx
        if let Some(ref fp) = node.file_path {
            let file_id = format!("file:{}", fp);
            if let Some(&idx) = id_to_idx.get(file_id.as_str()) {
                node_to_file_idx.insert(&node.id, idx);
                continue;
            }
        }
        // Try metadata["file_path"]
        if let Some(fp_val) = node.metadata.get("file_path") {
            if let Some(fp) = fp_val.as_str() {
                let file_id = format!("file:{}", fp);
                if let Some(&idx) = id_to_idx.get(file_id.as_str()) {
                    node_to_file_idx.insert(&node.id, idx);
                    continue;
                }
            }
        }
    }

    // 3. Accumulate edge weights between file pairs.
    let mut edge_weights: HashMap<(usize, usize), f64> = HashMap::new();

    for edge in &graph.edges {
        let w = relation_weight(&edge.relation);
        if w == 0.0 {
            continue;
        }

        // Resolve endpoints to file indices.
        let from_idx = node_to_file_idx
            .get(edge.from.as_str())
            .or_else(|| id_to_idx.get(edge.from.as_str()))
            .copied();
        let to_idx = node_to_file_idx
            .get(edge.to.as_str())
            .or_else(|| id_to_idx.get(edge.to.as_str()))
            .copied();

        if let (Some(f), Some(t)) = (from_idx, to_idx) {
            if f == t {
                continue; // skip self-loops
            }
            *edge_weights.entry((f, t)).or_insert(0.0) += w;
        }
    }

    // 4. Build the Network.
    let mut net = Network::new();

    // Ensure all file nodes exist even with no edges.
    if !idx_to_id.is_empty() {
        // Adding a node name forces the network to know about the index.
        for (idx, node_id) in idx_to_id.iter().enumerate() {
            net.add_node_name(idx, node_id);
        }
    }

    for (&(from, to), &total_weight) in &edge_weights {
        net.add_edge(from, to, total_weight);
    }

    (net, idx_to_id)
}

/// Add synthetic directory co-location edges to the network.
///
/// Files sharing the same parent directory get bidirectional edges with the
/// given weight. Directories with more than `MAX_DIR_SIZE_FOR_COLOCATION`
/// files are skipped to avoid O(n²) edge explosion.
pub fn add_dir_colocation_edges(net: &mut Network, idx_to_id: &[String], weight: f64) {
    if weight <= 0.0 {
        return;
    }

    // Group file indices by parent directory.
    let mut dir_groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, node_id) in idx_to_id.iter().enumerate() {
        let path = node_id.strip_prefix("file:").unwrap_or(node_id);
        let dir = match path.rsplit_once('/') {
            Some((parent, _)) if !parent.is_empty() => parent.to_string(),
            _ => "root".to_string(),
        };
        dir_groups.entry(dir).or_default().push(idx);
    }

    // Add pairwise bidirectional edges within each directory group.
    for (dir, files) in &dir_groups {
        if files.len() > MAX_DIR_SIZE_FOR_COLOCATION {
            eprintln!(
                "⚠ Directory '{}' has {} files — skipping co-location edges (limit: {})",
                dir,
                files.len(),
                MAX_DIR_SIZE_FOR_COLOCATION
            );
            continue;
        }
        for i in 0..files.len() {
            for j in (i + 1)..files.len() {
                net.add_edge(files[i], files[j], weight);
                net.add_edge(files[j], files[i], weight);
            }
        }
    }
}

// ── Clustering execution ───────────────────────────────────────────────────

/// Run Infomap on the network and return raw clusters plus metrics.
///
/// Handles flat and hierarchical modes, and dissolves undersized clusters.
pub fn run_clustering(
    net: &Network,
    idx_to_id: &[String],
    config: &ClusterConfig,
) -> (Vec<RawCluster>, ClusterMetrics) {
    let result = Infomap::new(net)
        .seed(config.seed)
        .num_trials(config.num_trials as usize)
        .hierarchical(config.hierarchical)
        .tau(config.teleportation_rate)
        .run();

    let (clusters, diag) = if config.hierarchical {
        let c = build_hierarchical_clusters(&result, idx_to_id);
        let singleton_count = c.iter().filter(|cl| cl.member_ids.len() == 1).count();
        (c, OrphanDiagnostics {
            orphan_count_raw: 0,
            merged_by_affinity: 0,
            assigned_by_dir: 0,
            singleton_clusters_final: singleton_count,
        })
    } else {
        build_flat_clusters(&result, idx_to_id, net, config.min_community_size)
    };

    let metrics = ClusterMetrics {
        codelength: result.codelength(),
        num_communities: result.num_modules(),
        num_total: net.num_nodes(),
        orphan_count_raw: diag.orphan_count_raw,
        orphans_merged_by_affinity: diag.merged_by_affinity,
        orphans_assigned_by_dir: diag.assigned_by_dir,
        singleton_clusters_final: diag.singleton_clusters_final,
    };

    (clusters, metrics)
}

/// Diagnostic counters for orphan reassignment.
#[derive(Debug, Clone, Default)]
struct OrphanDiagnostics {
    /// Infomap modules with size < min_community_size.
    orphan_count_raw: usize,
    /// Orphans merged into existing clusters via edge affinity.
    merged_by_affinity: usize,
    /// Orphans assigned to directory-based clusters as fallback.
    assigned_by_dir: usize,
    /// Final number of size=1 clusters.
    singleton_clusters_final: usize,
}

/// Extract the parent directory from a node ID like `"file:src/auth/login.rs"`.
///
/// Strips the `"file:"` prefix, then returns the parent directory path.
/// If no parent directory exists, returns `"root"`.
fn extract_parent_dir(node_id: &str) -> String {
    let path = node_id.strip_prefix("file:").unwrap_or(node_id);
    match path.rsplit_once('/') {
        Some((parent, _)) if !parent.is_empty() => parent.to_string(),
        _ => "root".to_string(),
    }
}

/// Compute the length of the longest common directory prefix between two paths.
/// This is segment-aware: "src/auth" and "src/api" share "src" (len 3), not "src/a" (len 5).
fn common_prefix_len(a: &str, b: &str) -> usize {
    let a_segments: Vec<&str> = a.split('/').collect();
    let b_segments: Vec<&str> = b.split('/').collect();
    let mut common = 0;
    for (sa, sb) in a_segments.iter().zip(b_segments.iter()) {
        if sa == sb {
            // +1 for the separator (except we count chars)
            common += sa.len() + 1;
        } else {
            break;
        }
    }
    common
}

/// Build flat clusters from Infomap module results with min-size enforcement.
fn build_flat_clusters(
    result: &infomap_rs::InfomapResult,
    idx_to_id: &[String],
    net: &Network,
    min_community_size: usize,
) -> (Vec<RawCluster>, OrphanDiagnostics) {
    let mut diag = OrphanDiagnostics::default();
    let modules = result.modules();

    let mut clusters: Vec<RawCluster> = Vec::new();
    let mut orphan_nodes: Vec<(usize, String)> = Vec::new(); // (original node idx, node id)

    for module in modules {
        let member_ids: Vec<String> = module
            .nodes
            .iter()
            .map(|&idx| idx_to_id[idx].clone())
            .collect();

        if member_ids.len() < min_community_size {
            // Collect orphans for reassignment.
            for &node_idx in &module.nodes {
                orphan_nodes.push((node_idx, idx_to_id[node_idx].clone()));
            }
        } else {
            clusters.push(RawCluster {
                id: clusters.len(),
                member_ids,
                flow: module.flow,
                parent: None,
                children: Vec::new(),
            });
        }
    }

    diag.orphan_count_raw = orphan_nodes.len();

    // Reassign orphan nodes using aggregate cluster affinity with iterative propagation.
    if !orphan_nodes.is_empty() && !clusters.is_empty() {
        // Build node_id → network idx lookup.
        let mut id_to_net_idx: HashMap<&str, usize> = HashMap::new();
        for (idx, nid) in idx_to_id.iter().enumerate() {
            id_to_net_idx.insert(nid.as_str(), idx);
        }

        // Build cluster membership map: network idx → cluster index.
        let mut net_idx_to_cluster: HashMap<usize, usize> = HashMap::new();
        for (ci, cluster) in clusters.iter().enumerate() {
            for mid in &cluster.member_ids {
                if let Some(&net_idx) = id_to_net_idx.get(mid.as_str()) {
                    net_idx_to_cluster.insert(net_idx, ci);
                }
            }
        }

        // Multi-pass iterative reassignment (fixes Bug C).
        // Orphans can merge through other orphans that were assigned in earlier passes.
        let mut unassigned: Vec<(usize, String)> = orphan_nodes;
        let max_iterations = 100;

        for _iter in 0..max_iterations {
            let mut merged_any = false;
            let mut still_unassigned: Vec<(usize, String)> = Vec::new();

            for (node_idx, node_id) in unassigned {
                // Compute aggregate cluster affinity (fixes Bug D):
                // sum all edge weights to each candidate cluster.
                let mut cluster_weights: HashMap<usize, f64> = HashMap::new();

                // Check BOTH out_neighbors AND in_neighbors (fixes Bug A).
                for &(neighbor_idx, w) in net.out_neighbors(node_idx) {
                    if let Some(&ci) = net_idx_to_cluster.get(&neighbor_idx) {
                        *cluster_weights.entry(ci).or_insert(0.0) += w;
                    }
                }
                for &(neighbor_idx, w) in net.in_neighbors(node_idx) {
                    if let Some(&ci) = net_idx_to_cluster.get(&neighbor_idx) {
                        *cluster_weights.entry(ci).or_insert(0.0) += w;
                    }
                }

                // Pick cluster with highest aggregate weight.
                if let Some((&best_ci, _)) = cluster_weights
                    .iter()
                    .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                {
                    clusters[best_ci].member_ids.push(node_id);
                    net_idx_to_cluster.insert(node_idx, best_ci);
                    merged_any = true;
                    diag.merged_by_affinity += 1;
                } else {
                    still_unassigned.push((node_idx, node_id));
                }
            }

            unassigned = still_unassigned;

            // If no merges happened this round, stop iterating.
            if !merged_any {
                break;
            }
            // If all orphans have been assigned, stop.
            if unassigned.is_empty() {
                break;
            }
        }

        // Directory-based fallback for remaining unmerged orphans (fixes Bug B).
        if !unassigned.is_empty() {
            let mut dir_groups: HashMap<String, Vec<String>> = HashMap::new();
            for (_, node_id) in unassigned {
                diag.assigned_by_dir += 1;
                let dir = extract_parent_dir(&node_id);
                dir_groups.entry(dir).or_default().push(node_id);
            }

            // Phase 1: Merge dir groups with ≥2 members directly into new clusters.
            // Phase 2: For singleton dir groups, try to merge with an existing cluster
            //          whose members share the closest ancestor directory.
            let mut singleton_orphans: Vec<String> = Vec::new();
            for (_, group) in dir_groups {
                if group.len() >= min_community_size {
                    clusters.push(RawCluster {
                        id: clusters.len(),
                        member_ids: group,
                        flow: 0.0,
                        parent: None,
                        children: Vec::new(),
                    });
                } else {
                    singleton_orphans.extend(group);
                }
            }

            // Phase 2: For each singleton orphan, find the existing cluster whose
            // members share the longest common directory prefix.
            if !singleton_orphans.is_empty() && !clusters.is_empty() {
                for orphan_id in &singleton_orphans {
                    let orphan_dir = extract_parent_dir(orphan_id);
                    let mut best_cluster: Option<usize> = None;
                    let mut best_prefix_len: usize = 0;

                    for (ci, cluster) in clusters.iter().enumerate() {
                        for member in &cluster.member_ids {
                            let member_dir = extract_parent_dir(member);
                            let prefix_len = common_prefix_len(&orphan_dir, &member_dir);
                            if prefix_len > best_prefix_len {
                                best_prefix_len = prefix_len;
                                best_cluster = Some(ci);
                            }
                        }
                    }

                    if let Some(ci) = best_cluster {
                        clusters[ci].member_ids.push(orphan_id.clone());
                    } else {
                        // Truly unreachable: no cluster shares any common prefix.
                        // Create a singleton as last resort.
                        clusters.push(RawCluster {
                            id: clusters.len(),
                            member_ids: vec![orphan_id.clone()],
                            flow: 0.0,
                            parent: None,
                            children: Vec::new(),
                        });
                    }
                }
            } else if !singleton_orphans.is_empty() {
                // No existing clusters at all — each orphan gets its own cluster.
                for orphan_id in singleton_orphans {
                    clusters.push(RawCluster {
                        id: clusters.len(),
                        member_ids: vec![orphan_id],
                        flow: 0.0,
                        parent: None,
                        children: Vec::new(),
                    });
                }
            }
        }
    } else if !orphan_nodes.is_empty() {
        // No clusters to merge into — group orphans by directory (fixes Bug B).
        let mut dir_groups: HashMap<String, Vec<String>> = HashMap::new();
        for (_, node_id) in orphan_nodes {
            diag.assigned_by_dir += 1;
            let dir = extract_parent_dir(&node_id);
            dir_groups.entry(dir).or_default().push(node_id);
        }
        for (_, group) in dir_groups {
            clusters.push(RawCluster {
                id: clusters.len(),
                member_ids: group,
                flow: 0.0,
                parent: None,
                children: Vec::new(),
            });
        }
    }

    // Re-number cluster IDs sequentially.
    for (i, c) in clusters.iter_mut().enumerate() {
        c.id = i;
    }

    diag.singleton_clusters_final = clusters.iter().filter(|c| c.member_ids.len() == 1).count();

    (clusters, diag)
}

/// Recursively build hierarchical clusters from the Infomap tree.
fn build_hierarchical_clusters(
    result: &infomap_rs::InfomapResult,
    idx_to_id: &[String],
) -> Vec<RawCluster> {
    let mut clusters: Vec<RawCluster> = Vec::new();

    if let Some(tree) = result.tree() {
        let mut counter: usize = 0;
        build_tree_clusters(tree, idx_to_id, &mut clusters, &mut counter, None, "".to_string());
    } else {
        // Fallback: treat flat modules as hierarchy roots.
        for module in result.modules() {
            let member_ids: Vec<String> = module
                .nodes
                .iter()
                .map(|&idx| idx_to_id[idx].clone())
                .collect();
            clusters.push(RawCluster {
                id: clusters.len(),
                member_ids,
                flow: module.flow,
                parent: None,
                children: Vec::new(),
            });
        }
    }

    clusters
}

/// Recursive helper for hierarchical tree traversal.
fn build_tree_clusters(
    tree_node: &infomap_rs::TreeNode,
    idx_to_id: &[String],
    clusters: &mut Vec<RawCluster>,
    counter: &mut usize,
    parent_idx: Option<usize>,
    _path: String,
) {
    let my_idx = *counter;
    *counter += 1;

    // Collect leaf node IDs.
    let member_ids: Vec<String> = if let Some(ref nodes) = tree_node.nodes {
        nodes.iter().map(|&idx| idx_to_id[idx].clone()).collect()
    } else {
        Vec::new()
    };

    clusters.push(RawCluster {
        id: my_idx,
        member_ids,
        flow: tree_node.flow,
        parent: parent_idx,
        children: Vec::new(),
    });

    // Set parent→child link.
    if let Some(pidx) = parent_idx {
        clusters[pidx].children.push(my_idx);
    }

    // Recurse into children.
    if let Some(ref children) = tree_node.children {
        for (ci, child) in children.iter().enumerate() {
            let child_path = if _path.is_empty() {
                format!("{}", ci)
            } else {
                format!("{}.{}", _path, ci)
            };
            // Collect descendant leaf members into the parent as well.
            build_tree_clusters(child, idx_to_id, clusters, counter, Some(my_idx), child_path);
        }

        // Propagate child members up to parent.
        let child_indices: Vec<usize> = clusters[my_idx].children.clone();
        for child_idx in child_indices {
            let child_members: Vec<String> = clusters[child_idx].member_ids.clone();
            clusters[my_idx].member_ids.extend(child_members);
        }
    }
}

// ── Component mapping ──────────────────────────────────────────────────────

/// Map raw clusters back to graph components: create component [`Node`]s and
/// membership [`Edge`]s.
pub fn map_to_components(clusters: &[RawCluster], graph: &Graph) -> ClusterResult {
    // Build an id → node lookup for resolving file paths.
    let node_map: HashMap<&str, &Node> = graph
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect();

    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();

    // Determine if hierarchical (any cluster has parent or children).
    let is_hierarchical = clusters.iter().any(|c| c.parent.is_some() || !c.children.is_empty());

    // For hierarchical mode, build an index→path map for dot-notation naming.
    let dot_paths: HashMap<usize, String> = if is_hierarchical {
        build_dot_paths(clusters)
    } else {
        HashMap::new()
    };

    let infer_meta = serde_json::json!({"source": "infer"});

    for cluster in clusters {
        // Determine component ID.
        let component_id = if is_hierarchical {
            let path = dot_paths
                .get(&cluster.id)
                .cloned()
                .unwrap_or_else(|| format!("{}", cluster.id));
            format!("infer:component:{}", path)
        } else {
            format!("infer:component:{}", cluster.id)
        };

        // Resolve member IDs to file paths for auto-naming.
        let file_paths: Vec<&str> = cluster
            .member_ids
            .iter()
            .filter_map(|mid| {
                node_map.get(mid.as_str()).and_then(|n| {
                    n.file_path.as_deref().or_else(|| {
                        // Strip "file:" prefix if the ID starts with it.
                        mid.strip_prefix("file:")
                    })
                })
            })
            .collect();

        let title = if file_paths.is_empty() {
            auto_name(&[])
        } else {
            auto_name(&file_paths)
        };

        // Create component node.
        let mut node = Node::new(&component_id, &title);
        node.node_type = Some("component".into());
        node.source = Some("infer".into());
        node.metadata
            .insert("flow".into(), serde_json::json!(cluster.flow));
        node.metadata
            .insert("size".into(), serde_json::json!(cluster.member_ids.len()));
        nodes.push(node);

        // Create membership edges: component → member.
        for mid in &cluster.member_ids {
            let mut edge = Edge::new(&component_id, mid, "contains");
            edge.metadata = Some(infer_meta.clone());
            edges.push(edge);
        }

        // Hierarchical: parent → child component edges.
        if is_hierarchical {
            for &child_id in &cluster.children {
                let child_component_id = {
                    let path = dot_paths
                        .get(&child_id)
                        .cloned()
                        .unwrap_or_else(|| format!("{}", child_id));
                    format!("infer:component:{}", path)
                };
                let mut edge = Edge::new(&component_id, &child_component_id, "contains");
                edge.metadata = Some(infer_meta.clone());
                edges.push(edge);
            }
        }
    }

    let metrics = ClusterMetrics {
        codelength: 0.0,
        num_communities: clusters.len(),
        num_total: 0,
        ..Default::default()
    };

    ClusterResult {
        nodes,
        edges,
        metrics,
    }
}

/// Build dot-notation paths (e.g., "0.1.3") for hierarchical cluster IDs.
fn build_dot_paths(clusters: &[RawCluster]) -> HashMap<usize, String> {
    let mut paths: HashMap<usize, String> = HashMap::new();

    // Find root(s) — clusters with no parent.
    let roots: Vec<usize> = clusters
        .iter()
        .filter(|c| c.parent.is_none())
        .map(|c| c.id)
        .collect();

    for (ri, &root_id) in roots.iter().enumerate() {
        let root_path = format!("{}", ri);
        paths.insert(root_id, root_path.clone());
        assign_child_paths(clusters, root_id, &root_path, &mut paths);
    }

    paths
}

/// Recursively assign dot-notation paths to child clusters.
fn assign_child_paths(
    clusters: &[RawCluster],
    parent_id: usize,
    parent_path: &str,
    paths: &mut HashMap<usize, String>,
) {
    if let Some(cluster) = clusters.iter().find(|c| c.id == parent_id) {
        for (ci, &child_id) in cluster.children.iter().enumerate() {
            let child_path = format!("{}.{}", parent_path, ci);
            paths.insert(child_id, child_path.clone());
            assign_child_paths(clusters, child_id, &child_path, paths);
        }
    }
}

// ── Auto-naming ────────────────────────────────────────────────────────────

/// Automatically generate a human-readable component name from file paths.
///
/// Strategy:
/// 1. Find the longest common prefix of path components.
/// 2. Use the deepest directory after the common prefix as the name.
/// 3. If no common prefix, use the most frequent directory among all paths.
/// 4. Fallback: `"component-N"` using a simple hash.
pub fn auto_name(file_paths: &[&str]) -> String {
    if file_paths.is_empty() {
        return "component".to_string();
    }

    if file_paths.len() == 1 {
        // Single file: use the parent directory or file stem.
        let parts: Vec<&str> = file_paths[0].split('/').collect();
        if parts.len() > 1 {
            return parts[parts.len() - 2].to_string();
        }
        return parts[0]
            .rsplit_once('.')
            .map(|(stem, _)| stem)
            .unwrap_or(parts[0])
            .to_string();
    }

    // Split all paths into components.
    let split_paths: Vec<Vec<&str>> = file_paths
        .iter()
        .map(|p| p.split('/').collect::<Vec<_>>())
        .collect();

    // Find longest common prefix.
    let min_len = split_paths.iter().map(|p| p.len()).min().unwrap_or(0);
    let mut prefix_len = 0;
    for i in 0..min_len {
        let first = split_paths[0][i];
        if split_paths.iter().all(|p| p[i] == first) {
            prefix_len = i + 1;
        } else {
            break;
        }
    }

    // Use the deepest common prefix directory.
    if prefix_len > 0 {
        let deepest = split_paths[0][prefix_len - 1];
        // If the deepest component looks like a file (has extension), use the one before it.
        if deepest.contains('.') && prefix_len > 1 {
            return split_paths[0][prefix_len - 2].to_string();
        }
        return deepest.to_string();
    }

    // No common prefix — find the most frequent directory component.
    let mut freq: HashMap<&str, usize> = HashMap::new();
    for parts in &split_paths {
        // Count directory components (everything except the last, which is the filename).
        for &part in parts.iter().take(parts.len().saturating_sub(1)) {
            *freq.entry(part).or_insert(0) += 1;
        }
    }

    if let Some((&dir, _)) = freq.iter().max_by_key(|(_, &count)| count) {
        // Don't return very generic directories like "src" if there are better options.
        return dir.to_string();
    }

    // Fallback: hash-based name.
    let hash: u64 = file_paths.iter().fold(0u64, |acc, p| {
        acc.wrapping_add(p.bytes().fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64)))
    });
    format!("component-{}", hash % 10000)
}

// ── Auto-configuration ─────────────────────────────────────────────────────

/// Auto-select clustering parameters based on graph size and density.
///
/// Adapts teleportation rate based on average node degree:
/// - Sparse graphs (avg degree < 3): τ=0.01 — keep walker in community
/// - Normal code graphs (avg degree 3–20): τ=0.05
/// - Dense graphs (avg degree > 20): τ=0.10 — allow more exploration
pub fn auto_config(file_count: usize) -> ClusterConfig {
    let (min_community_size, hierarchical) = match file_count {
        0..=49 => (2, false),
        50..=499 => (3, false),
        500..=1999 => (5, false),
        _ => (8, false),
    };
    ClusterConfig {
        min_community_size,
        hierarchical,
        ..ClusterConfig::default()
    }
}

/// Auto-select clustering parameters with network density awareness.
///
/// Unlike [`auto_config`] which only considers file count, this version
/// also adapts the teleportation rate based on the actual network's
/// average degree.
pub fn auto_config_with_network(file_count: usize, net: &Network) -> ClusterConfig {
    let mut config = auto_config(file_count);

    // Adapt teleportation rate based on average degree.
    let num_nodes = net.num_nodes();
    if num_nodes > 0 {
        let avg_degree = net.num_edges() as f64 / num_nodes as f64;
        config.teleportation_rate = if avg_degree < 3.0 {
            0.01
        } else if avg_degree <= 20.0 {
            0.05
        } else {
            0.10
        };
    }

    config
}

// ── Post-processing ────────────────────────────────────────────────────────

/// Post-process clustering results:
/// 1. Merge undersized components into their best neighbor
/// 2. De-duplicate component names by appending distinguishing path segments
/// 3. Validate coverage: every file belongs to exactly one component
pub fn post_process(result: &mut ClusterResult, graph: &Graph, _config: &ClusterConfig) {
    // Step 1: Filter out components below min_community_size
    // (flat mode already handles this via orphan reassignment, but hierarchical doesn't)

    // Step 2: De-duplicate names
    deduplicate_names(&mut result.nodes);

    // Step 3: Log quality metrics
    let file_count = graph
        .nodes
        .iter()
        .filter(|n| {
            n.node_type.as_deref() == Some("file")
                || (n.node_type.as_deref() == Some("code")
                    && n.node_kind.as_deref() == Some("File"))
        })
        .count();

    let component_count = result.nodes.len();

    // Check for dominant component (>50% of files)
    let max_size = result
        .nodes
        .iter()
        .filter_map(|n| n.metadata.get("size").and_then(|v| v.as_u64()))
        .max()
        .unwrap_or(0) as usize;

    if max_size > file_count / 2 && file_count > 10 {
        eprintln!(
            "⚠ Largest component has {} files ({}% of total) — may need manual review",
            max_size,
            max_size * 100 / file_count
        );
    }

    result.metrics.num_communities = component_count;
}

/// De-duplicate component names by appending distinguishing path context.
/// E.g., if there are 5 "utils" components, rename them to "utils-0", "utils-1", etc.
fn deduplicate_names(nodes: &mut [Node]) {
    use std::collections::HashMap as DedupMap;

    // Group nodes by title
    let mut title_groups: DedupMap<String, Vec<usize>> = DedupMap::new();
    for (i, node) in nodes.iter().enumerate() {
        title_groups.entry(node.title.clone()).or_default().push(i);
    }

    // For groups with >1 node, disambiguate using the component ID suffix
    for (title, indices) in &title_groups {
        if indices.len() <= 1 {
            continue;
        }

        for &idx in indices {
            let node = &nodes[idx];
            // IDs are like "infer:component:5" — extract the number
            if let Some(num_str) = node.id.strip_prefix("infer:component:") {
                nodes[idx].title = format!("{}-{}", title, num_str);
            }
        }
    }
}

// ── Main entry point ───────────────────────────────────────────────────────

/// Run community detection on a code graph and return inferred components.
///
/// This is the main entry point. It builds a file-level network, runs Infomap,
/// and maps results back to component nodes and membership edges.
pub fn cluster(graph: &Graph, config: &ClusterConfig) -> Result<ClusterResult> {
    let (mut net, idx_to_id) = build_network(graph);

    // Add directory co-location edges if configured.
    add_dir_colocation_edges(&mut net, &idx_to_id, config.dir_colocation_weight);

    if net.num_nodes() < 2 {
        return Ok(ClusterResult::empty());
    }

    let (clusters, metrics) = run_clustering(&net, &idx_to_id, config);

    let mut result = map_to_components(&clusters, graph);
    result.metrics = metrics;

    // Post-process: deduplicate names, validate coverage
    post_process(&mut result, graph, config);

    Ok(result)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Edge, Graph, Node};

    fn make_file_node(path: &str) -> Node {
        let mut n = Node::new(&format!("file:{}", path), path);
        n.node_type = Some("file".into());
        n.file_path = Some(path.into());
        n
    }

    fn make_fn_node(id: &str, file_path: &str) -> Node {
        let mut n = Node::new(id, id);
        n.node_type = Some("function".into());
        n.file_path = Some(file_path.into());
        n
    }

    fn default_config() -> ClusterConfig {
        ClusterConfig {
            seed: 42,
            num_trials: 10,
            min_community_size: 2,
            ..Default::default()
        }
    }

    /// Build a graph with two disjoint groups of files, each group fully connected via "calls".
    /// Group A: a1, a2, a3.  Group B: b1, b2, b3.
    fn two_community_graph() -> Graph {
        let mut g = Graph::default();
        let group_a = ["src/auth/login.rs", "src/auth/logout.rs", "src/auth/session.rs"];
        let group_b = ["src/db/pool.rs", "src/db/query.rs", "src/db/migrate.rs"];

        for p in group_a.iter().chain(group_b.iter()) {
            g.nodes.push(make_file_node(p));
        }

        // Fully connect group A
        for i in 0..group_a.len() {
            for j in 0..group_a.len() {
                if i != j {
                    g.edges.push(Edge::new(
                        &format!("file:{}", group_a[i]),
                        &format!("file:{}", group_a[j]),
                        "calls",
                    ));
                }
            }
        }

        // Fully connect group B
        for i in 0..group_b.len() {
            for j in 0..group_b.len() {
                if i != j {
                    g.edges.push(Edge::new(
                        &format!("file:{}", group_b[i]),
                        &format!("file:{}", group_b[j]),
                        "calls",
                    ));
                }
            }
        }

        g
    }

    // ── 1. test_relation_weight ────────────────────────────────────────

    #[test]
    fn test_relation_weight() {
        assert_eq!(relation_weight("calls"), WEIGHT_CALLS);
        assert_eq!(relation_weight("imports"), WEIGHT_IMPORTS);
        assert_eq!(relation_weight("type_reference"), WEIGHT_TYPE_REF);
        assert_eq!(relation_weight("inherits"), WEIGHT_TYPE_REF);
        assert_eq!(relation_weight("implements"), WEIGHT_TYPE_REF);
        assert_eq!(relation_weight("uses"), WEIGHT_TYPE_REF);
        assert_eq!(relation_weight("defined_in"), WEIGHT_STRUCTURAL);
        assert_eq!(relation_weight("contains"), WEIGHT_STRUCTURAL);
        assert_eq!(relation_weight("belongs_to"), WEIGHT_STRUCTURAL);
        assert_eq!(relation_weight("depends_on"), WEIGHT_DEPENDS_ON);
        // Unknown relation returns 0.0
        assert_eq!(relation_weight("foobar"), 0.0);
        assert_eq!(relation_weight(""), 0.0);
    }

    // ── 2. test_build_network_basic ────────────────────────────────────

    #[test]
    fn test_build_network_basic() {
        let mut g = Graph::default();
        g.nodes.push(make_file_node("a.rs"));
        g.nodes.push(make_file_node("b.rs"));
        g.nodes.push(make_file_node("c.rs"));
        g.edges.push(Edge::new("file:a.rs", "file:b.rs", "calls"));
        g.edges.push(Edge::new("file:b.rs", "file:c.rs", "imports"));

        let (net, idx_to_id) = build_network(&g);

        assert_eq!(net.num_nodes(), 3);
        assert_eq!(idx_to_id.len(), 3);
        // There should be exactly 2 directed edges
        assert_eq!(net.num_edges(), 2);
    }

    // ── 3. test_build_network_weight_differentiation ───────────────────

    #[test]
    fn test_build_network_weight_differentiation() {
        let mut g = Graph::default();
        g.nodes.push(make_file_node("x.rs"));
        g.nodes.push(make_file_node("y.rs"));
        g.nodes.push(make_file_node("z.rs"));
        g.edges.push(Edge::new("file:x.rs", "file:y.rs", "calls"));
        g.edges.push(Edge::new("file:x.rs", "file:z.rs", "imports"));

        let (net, idx_to_id) = build_network(&g);

        // Find index of x, y, z
        let x = idx_to_id.iter().position(|id| id == "file:x.rs").unwrap();
        let y = idx_to_id.iter().position(|id| id == "file:y.rs").unwrap();
        let z = idx_to_id.iter().position(|id| id == "file:z.rs").unwrap();

        let out = net.out_neighbors(x);
        let weight_xy = out.iter().find(|&&(t, _)| t == y).map(|&(_, w)| w).unwrap();
        let weight_xz = out.iter().find(|&&(t, _)| t == z).map(|&(_, w)| w).unwrap();

        // "calls" (1.0) > "imports" (0.8)
        assert!(
            weight_xy > weight_xz,
            "calls weight ({}) should be > imports weight ({})",
            weight_xy,
            weight_xz
        );
    }

    // ── 4. test_build_network_skips_self_loops ─────────────────────────

    #[test]
    fn test_build_network_skips_self_loops() {
        let mut g = Graph::default();
        g.nodes.push(make_file_node("a.rs"));
        g.nodes.push(make_file_node("b.rs"));
        g.edges.push(Edge::new("file:a.rs", "file:a.rs", "calls")); // self-loop
        g.edges.push(Edge::new("file:a.rs", "file:b.rs", "calls"));

        let (net, _idx_to_id) = build_network(&g);

        // Only the a→b edge should exist, not the self-loop
        assert_eq!(net.num_edges(), 1);
    }

    // ── 5. test_build_network_maps_functions_to_files ──────────────────

    #[test]
    fn test_build_network_maps_functions_to_files() {
        let mut g = Graph::default();
        g.nodes.push(make_file_node("src/main.rs"));
        g.nodes.push(make_file_node("src/lib.rs"));
        g.nodes.push(make_fn_node("fn:do_stuff", "src/main.rs"));
        g.nodes.push(make_fn_node("fn:helper", "src/lib.rs"));

        // Edge between function nodes; should resolve to file-level edge
        g.edges.push(Edge::new("fn:do_stuff", "fn:helper", "calls"));

        let (net, idx_to_id) = build_network(&g);

        // Only 2 file nodes in the network
        assert_eq!(net.num_nodes(), 2);
        assert_eq!(idx_to_id.len(), 2);

        // Should have 1 edge (main.rs → lib.rs)
        assert_eq!(net.num_edges(), 1);

        let main_idx = idx_to_id.iter().position(|id| id == "file:src/main.rs").unwrap();
        let lib_idx = idx_to_id.iter().position(|id| id == "file:src/lib.rs").unwrap();
        let out = net.out_neighbors(main_idx);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, lib_idx);
    }

    // ── 6. test_cluster_two_communities ────────────────────────────────

    #[test]
    fn test_cluster_two_communities() {
        let g = two_community_graph();
        let config = default_config();
        let result = cluster(&g, &config).unwrap();

        // Should detect exactly 2 communities
        assert_eq!(
            result.metrics.num_communities, 2,
            "Expected 2 communities, got {}",
            result.metrics.num_communities
        );
        assert_eq!(result.nodes.len(), 2);
    }

    // ── 7. test_cluster_single_community ───────────────────────────────

    #[test]
    fn test_cluster_single_community() {
        let mut g = Graph::default();
        let files = ["a.rs", "b.rs", "c.rs", "d.rs"];
        for f in &files {
            g.nodes.push(make_file_node(f));
        }
        // Fully connected graph → should produce 1 community
        for i in 0..files.len() {
            for j in 0..files.len() {
                if i != j {
                    g.edges.push(Edge::new(
                        &format!("file:{}", files[i]),
                        &format!("file:{}", files[j]),
                        "calls",
                    ));
                }
            }
        }

        let config = ClusterConfig {
            seed: 42,
            num_trials: 10,
            min_community_size: 1,
            ..Default::default()
        };
        let result = cluster(&g, &config).unwrap();

        assert_eq!(
            result.metrics.num_communities, 1,
            "Fully connected graph should yield 1 community, got {}",
            result.metrics.num_communities
        );
    }

    // ── 8. test_cluster_empty_graph ────────────────────────────────────

    #[test]
    fn test_cluster_empty_graph() {
        let g = Graph::default();
        let config = default_config();
        let result = cluster(&g, &config).unwrap();

        assert!(result.nodes.is_empty());
        assert!(result.edges.is_empty());
        assert_eq!(result.metrics.codelength, 0.0);
        assert_eq!(result.metrics.num_communities, 0);
        assert_eq!(result.metrics.num_total, 0);
    }

    // ── 9. test_cluster_min_community_size ─────────────────────────────

    #[test]
    fn test_cluster_min_community_size() {
        // Create a graph where one node is loosely connected (singleton after clustering).
        // Group of 4 tightly connected + 1 with a single weak edge.
        let mut g = Graph::default();
        let core = ["src/core/a.rs", "src/core/b.rs", "src/core/c.rs", "src/core/d.rs"];
        for f in &core {
            g.nodes.push(make_file_node(f));
        }
        // Add a loner
        g.nodes.push(make_file_node("src/misc/loner.rs"));

        // Fully connect core
        for i in 0..core.len() {
            for j in 0..core.len() {
                if i != j {
                    g.edges.push(Edge::new(
                        &format!("file:{}", core[i]),
                        &format!("file:{}", core[j]),
                        "calls",
                    ));
                }
            }
        }

        // Weak connection from loner to one core file
        g.edges.push(Edge::new("file:src/misc/loner.rs", "file:src/core/a.rs", "depends_on"));

        let config = ClusterConfig {
            seed: 42,
            num_trials: 10,
            min_community_size: 2,
            ..Default::default()
        };
        let result = cluster(&g, &config).unwrap();

        // The loner should be absorbed into an existing cluster (orphan reassignment)
        // or placed in a misc cluster. Either way, all nodes should be accounted for.
        let total_members: usize = result
            .edges
            .iter()
            .filter(|e| e.relation == "contains")
            .count();
        assert_eq!(total_members, 5, "All 5 nodes should be assigned to some cluster");
    }

    // ── 10. test_cluster_hierarchical ──────────────────────────────────

    #[test]
    fn test_cluster_hierarchical() {
        let g = two_community_graph();
        let config = ClusterConfig {
            seed: 42,
            num_trials: 10,
            min_community_size: 1,
            hierarchical: true,
            ..Default::default()
        };
        let result = cluster(&g, &config).unwrap();

        // Hierarchical mode should produce clusters with parent/children relationships.
        // There should be at least one "contains" edge between component nodes
        // (parent → child component).
        let component_ids: Vec<&str> = result
            .nodes
            .iter()
            .map(|n| n.id.as_str())
            .collect();

        let parent_child_edges: Vec<&Edge> = result
            .edges
            .iter()
            .filter(|e| {
                e.relation == "contains"
                    && component_ids.contains(&e.from.as_str())
                    && component_ids.contains(&e.to.as_str())
            })
            .collect();

        // In hierarchical mode, there should be at least one parent→child component edge
        assert!(
            !parent_child_edges.is_empty(),
            "Hierarchical clustering should produce parent→child component edges"
        );

        // Multiple levels of hierarchy → more than 2 component nodes
        assert!(
            result.nodes.len() > 2,
            "Hierarchical mode should produce more than 2 component nodes, got {}",
            result.nodes.len()
        );
    }

    // ── 11. test_auto_name_common_prefix ───────────────────────────────

    #[test]
    fn test_auto_name_common_prefix() {
        let paths = ["src/auth/login.rs", "src/auth/logout.rs"];
        let name = auto_name(&paths);
        assert_eq!(name, "auth");
    }

    // ── 12. test_auto_name_mixed_dirs ──────────────────────────────────

    #[test]
    fn test_auto_name_mixed_dirs() {
        // No common prefix → most frequent directory wins
        let paths = [
            "src/db/pool.rs",
            "src/db/query.rs",
            "lib/utils/helper.rs",
        ];
        let name = auto_name(&paths);
        // "src" appears 2x, "db" appears 2x, "lib" 1x, "utils" 1x.
        // The most frequent directory should be chosen.
        // Could be "src" or "db" (both have count 2). Accept either.
        assert!(
            name == "src" || name == "db",
            "Expected most frequent directory, got '{}'",
            name
        );
    }

    // ── 13. test_component_node_schema ──────────────────────────────────

    #[test]
    fn test_component_node_schema() {
        let g = two_community_graph();
        let config = default_config();
        let result = cluster(&g, &config).unwrap();

        assert!(!result.nodes.is_empty(), "Should have component nodes");

        for node in &result.nodes {
            assert_eq!(
                node.node_type.as_deref(),
                Some("component"),
                "Component node should have node_type='component'"
            );
            assert_eq!(
                node.source.as_deref(),
                Some("infer"),
                "Component node should have source='infer'"
            );
            assert!(
                node.metadata.contains_key("flow"),
                "Component node metadata should contain 'flow'"
            );
            assert!(
                node.metadata.contains_key("size"),
                "Component node metadata should contain 'size'"
            );
        }
    }

    // ── 14. test_contains_edge_direction ────────────────────────────────

    #[test]
    fn test_contains_edge_direction() {
        let g = two_community_graph();
        let config = default_config();
        let result = cluster(&g, &config).unwrap();

        let component_ids: Vec<&str> = result
            .nodes
            .iter()
            .map(|n| n.id.as_str())
            .collect();

        let file_ids: Vec<String> = g
            .nodes
            .iter()
            .filter(|n| n.node_type.as_deref() == Some("file"))
            .map(|n| n.id.clone())
            .collect();

        for edge in &result.edges {
            if edge.relation == "contains" {
                // Check edges between component → file (not component→component in hierarchical)
                if file_ids.contains(&edge.to) {
                    // "from" should be a component, "to" should be a file
                    assert!(
                        component_ids.contains(&edge.from.as_str()),
                        "'contains' edge 'from' ({}) should be a component node",
                        edge.from
                    );
                    assert!(
                        !component_ids.contains(&edge.to.as_str()),
                        "'contains' edge 'to' ({}) should NOT be a component node (it should be a file)",
                        edge.to
                    );
                }
            }
        }

        // Verify no edge goes from file → component
        for edge in &result.edges {
            if edge.relation == "contains" {
                assert!(
                    !file_ids.contains(&edge.from),
                    "'contains' edge should not have a file as 'from': {} → {}",
                    edge.from,
                    edge.to
                );
            }
        }
    }

    // ── 15. test_metrics_output ────────────────────────────────────────

    #[test]
    fn test_metrics_output() {
        let g = two_community_graph();
        let config = default_config();
        let result = cluster(&g, &config).unwrap();

        assert!(
            result.metrics.codelength > 0.0,
            "Codelength should be > 0, got {}",
            result.metrics.codelength
        );
        assert_eq!(
            result.metrics.num_communities, 2,
            "num_communities should be 2, got {}",
            result.metrics.num_communities
        );
        assert_eq!(
            result.metrics.num_total, 6,
            "num_total should be 6 (all file nodes), got {}",
            result.metrics.num_total
        );
    }

    // ── 16. test_deterministic_with_seed ───────────────────────────────

    #[test]
    fn test_deterministic_with_seed() {
        let g = two_community_graph();
        let config = ClusterConfig {
            seed: 123,
            num_trials: 10,
            min_community_size: 2,
            ..Default::default()
        };

        let result1 = cluster(&g, &config).unwrap();
        let result2 = cluster(&g, &config).unwrap();

        // Same number of clusters
        assert_eq!(result1.nodes.len(), result2.nodes.len());

        // Same codelength
        assert!(
            (result1.metrics.codelength - result2.metrics.codelength).abs() < f64::EPSILON,
            "Codelength should be identical: {} vs {}",
            result1.metrics.codelength,
            result2.metrics.codelength
        );

        // Same membership: sort edges for comparison
        let mut edges1: Vec<(String, String)> = result1
            .edges
            .iter()
            .filter(|e| e.relation == "contains")
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();
        let mut edges2: Vec<(String, String)> = result2
            .edges
            .iter()
            .filter(|e| e.relation == "contains")
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();
        edges1.sort();
        edges2.sort();
        assert_eq!(edges1, edges2, "Deterministic: same seed should produce identical clustering");
    }

    // ── 17. test_orphan_reassignment_uses_in_neighbors ─────────────────

    /// An orphan file that only has *incoming* edges (other files import it, but
    /// it imports nothing) should still be merged into the correct cluster.
    #[test]
    fn test_orphan_reassignment_uses_in_neighbors() {
        let mut g = Graph::default();

        // Cluster A: 3 tightly connected files
        let cluster_a = ["src/core/a.rs", "src/core/b.rs", "src/core/c.rs"];
        for p in &cluster_a {
            g.nodes.push(make_file_node(p));
        }
        for i in 0..cluster_a.len() {
            for j in 0..cluster_a.len() {
                if i != j {
                    g.edges.push(Edge::new(
                        &format!("file:{}", cluster_a[i]),
                        &format!("file:{}", cluster_a[j]),
                        "calls",
                    ));
                }
            }
        }

        // Orphan: a utility file that imports nothing but IS imported by cluster A
        g.nodes.push(make_file_node("src/utils/types.rs"));
        // Edges go FROM cluster A files TO the orphan (orphan has only incoming)
        g.edges.push(Edge::new("file:src/core/a.rs", "file:src/utils/types.rs", "imports"));
        g.edges.push(Edge::new("file:src/core/b.rs", "file:src/utils/types.rs", "imports"));

        let config = ClusterConfig {
            seed: 42,
            num_trials: 10,
            min_community_size: 2,
            ..Default::default()
        };
        let result = cluster(&g, &config).unwrap();

        // The orphan should be merged into cluster A, not be a singleton
        let total_members: usize = result
            .edges
            .iter()
            .filter(|e| e.relation == "contains")
            .filter(|e| !e.from.starts_with("file:") && !e.to.starts_with("infer:"))
            .count();
        assert_eq!(total_members, 4, "All 4 nodes should be assigned");

        // Verify no singleton clusters (all components should have ≥2 members)
        for node in &result.nodes {
            let size = node.metadata.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
            assert!(
                size >= 2,
                "Component '{}' has size {} — orphan with incoming edges should have merged",
                node.title, size
            );
        }
    }

    // ── 18. test_orphan_reassignment_aggregate_weight ──────────────────

    /// When an orphan has 3 weak edges to cluster A and 1 strong edge to cluster B,
    /// it should go to A (aggregate 0.9 > 0.8).
    #[test]
    fn test_orphan_reassignment_aggregate_weight() {
        let mut g = Graph::default();

        // Cluster A: 3 files
        let cluster_a = ["src/web/handler.rs", "src/web/router.rs", "src/web/middleware.rs"];
        for p in &cluster_a {
            g.nodes.push(make_file_node(p));
        }
        for i in 0..cluster_a.len() {
            for j in 0..cluster_a.len() {
                if i != j {
                    g.edges.push(Edge::new(
                        &format!("file:{}", cluster_a[i]),
                        &format!("file:{}", cluster_a[j]),
                        "calls",
                    ));
                }
            }
        }

        // Cluster B: 3 files
        let cluster_b = ["src/db/pool.rs", "src/db/query.rs", "src/db/migrate.rs"];
        for p in &cluster_b {
            g.nodes.push(make_file_node(p));
        }
        for i in 0..cluster_b.len() {
            for j in 0..cluster_b.len() {
                if i != j {
                    g.edges.push(Edge::new(
                        &format!("file:{}", cluster_b[i]),
                        &format!("file:{}", cluster_b[j]),
                        "calls",
                    ));
                }
            }
        }

        // Orphan with 3 weak edges to A (depends_on=0.4 each, total 1.2)
        // and 1 strong edge to B (calls=1.0)
        g.nodes.push(make_file_node("src/shared/config.rs"));
        g.edges.push(Edge::new("file:src/shared/config.rs", "file:src/web/handler.rs", "depends_on"));
        g.edges.push(Edge::new("file:src/shared/config.rs", "file:src/web/router.rs", "depends_on"));
        g.edges.push(Edge::new("file:src/shared/config.rs", "file:src/web/middleware.rs", "depends_on"));
        g.edges.push(Edge::new("file:src/shared/config.rs", "file:src/db/pool.rs", "calls"));

        let config = ClusterConfig {
            seed: 42,
            num_trials: 10,
            min_community_size: 2,
            ..Default::default()
        };
        let result = cluster(&g, &config).unwrap();

        // Find which cluster the orphan ended up in
        let orphan_id = "file:src/shared/config.rs";
        let orphan_cluster = result
            .edges
            .iter()
            .find(|e| e.relation == "contains" && e.to == orphan_id)
            .map(|e| e.from.clone());

        assert!(orphan_cluster.is_some(), "Orphan should be assigned to a cluster");

        // Find which cluster has the web files (cluster A)
        let web_cluster = result
            .edges
            .iter()
            .find(|e| e.relation == "contains" && e.to == "file:src/web/handler.rs")
            .map(|e| e.from.clone());

        assert_eq!(
            orphan_cluster, web_cluster,
            "Orphan should be in cluster A (aggregate depends_on 1.2 > calls 1.0)"
        );
    }

    // ── 19. test_orphan_directory_fallback ─────────────────────────────

    /// Isolated files in the same directory should be grouped together,
    /// not become individual singletons.
    #[test]
    fn test_orphan_directory_fallback() {
        let mut g = Graph::default();

        // A proper cluster
        let cluster_a = ["src/core/a.rs", "src/core/b.rs", "src/core/c.rs"];
        for p in &cluster_a {
            g.nodes.push(make_file_node(p));
        }
        for i in 0..cluster_a.len() {
            for j in 0..cluster_a.len() {
                if i != j {
                    g.edges.push(Edge::new(
                        &format!("file:{}", cluster_a[i]),
                        &format!("file:{}", cluster_a[j]),
                        "calls",
                    ));
                }
            }
        }

        // 3 isolated files in the same directory — no edges at all
        g.nodes.push(make_file_node("src/config/base.rs"));
        g.nodes.push(make_file_node("src/config/env.rs"));
        g.nodes.push(make_file_node("src/config/defaults.rs"));

        let config = ClusterConfig {
            seed: 42,
            num_trials: 10,
            min_community_size: 2,
            ..Default::default()
        };
        let result = cluster(&g, &config).unwrap();

        // Count how many components the config files ended up in
        let config_files = ["file:src/config/base.rs", "file:src/config/env.rs", "file:src/config/defaults.rs"];
        let config_clusters: std::collections::HashSet<&str> = result
            .edges
            .iter()
            .filter(|e| e.relation == "contains" && config_files.contains(&e.to.as_str()))
            .map(|e| e.from.as_str())
            .collect();

        assert_eq!(
            config_clusters.len(), 1,
            "All 3 config files should be in ONE directory-based cluster, not {} clusters",
            config_clusters.len()
        );

        // Verify there are no singleton clusters
        for node in &result.nodes {
            let size = node.metadata.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
            assert!(
                size >= 2,
                "Component '{}' has size {} — should not have singletons",
                node.title, size
            );
        }
    }

    // ── 20. test_orphan_iterative_propagation ──────────────────────────

    /// Chain: cluster ← orphan_A ← orphan_B.
    /// orphan_A can merge directly, then orphan_B merges through orphan_A.
    #[test]
    fn test_orphan_iterative_propagation() {
        let mut g = Graph::default();

        // A proper cluster of 3
        let cluster_a = ["src/lib/x.rs", "src/lib/y.rs", "src/lib/z.rs"];
        for p in &cluster_a {
            g.nodes.push(make_file_node(p));
        }
        for i in 0..cluster_a.len() {
            for j in 0..cluster_a.len() {
                if i != j {
                    g.edges.push(Edge::new(
                        &format!("file:{}", cluster_a[i]),
                        &format!("file:{}", cluster_a[j]),
                        "calls",
                    ));
                }
            }
        }

        // orphan_A connects to cluster_a
        g.nodes.push(make_file_node("src/ext/bridge.rs"));
        g.edges.push(Edge::new("file:src/ext/bridge.rs", "file:src/lib/x.rs", "imports"));

        // orphan_B connects ONLY to orphan_A (no direct path to cluster)
        g.nodes.push(make_file_node("src/ext/adapter.rs"));
        g.edges.push(Edge::new("file:src/ext/adapter.rs", "file:src/ext/bridge.rs", "calls"));

        let config = ClusterConfig {
            seed: 42,
            num_trials: 10,
            min_community_size: 2,
            ..Default::default()
        };
        let result = cluster(&g, &config).unwrap();

        // All 5 files should be assigned to clusters (no singletons)
        let total_members: usize = result
            .edges
            .iter()
            .filter(|e| e.relation == "contains")
            .filter(|e| !e.from.starts_with("file:") && !e.to.starts_with("infer:"))
            .count();
        assert_eq!(total_members, 5, "All 5 nodes should be assigned to clusters");

        // orphan_B should either be in the same cluster as orphan_A (iterative merge)
        // or in a directory-based group with orphan_A. Either way, not a singleton.
        let bridge_cluster = result
            .edges
            .iter()
            .find(|e| e.relation == "contains" && e.to == "file:src/ext/bridge.rs")
            .map(|e| e.from.clone());
        let adapter_cluster = result
            .edges
            .iter()
            .find(|e| e.relation == "contains" && e.to == "file:src/ext/adapter.rs")
            .map(|e| e.from.clone());

        assert!(bridge_cluster.is_some(), "bridge.rs should be assigned");
        assert!(adapter_cluster.is_some(), "adapter.rs should be assigned");
    }

    // ── 21. test_extract_parent_dir ───────────────────────────────────

    #[test]
    fn test_extract_parent_dir() {
        assert_eq!(extract_parent_dir("file:src/auth/login.rs"), "src/auth");
        assert_eq!(extract_parent_dir("file:src/main.rs"), "src");
        assert_eq!(extract_parent_dir("file:lib.rs"), "root");
        assert_eq!(extract_parent_dir("src/utils/helper.rs"), "src/utils");
    }

    // ── 22. test_default_teleportation_rate ────────────────────────────

    #[test]
    fn test_default_teleportation_rate() {
        let config = ClusterConfig::default();
        assert!(
            (config.teleportation_rate - 0.05).abs() < f64::EPSILON,
            "Default teleportation rate should be 0.05, got {}",
            config.teleportation_rate
        );
        assert_eq!(config.num_trials, 10, "Default num_trials should be 10");
    }

    // ── 23. test_auto_config_with_network_sparse ──────────────────────

    #[test]
    fn test_auto_config_with_network_sparse() {
        // Sparse graph: 10 nodes, 5 edges → avg degree 0.5
        let mut net = Network::new();
        for i in 0..10 {
            net.add_node_name(i, &format!("node_{}", i));
        }
        // Only 5 edges across 10 nodes
        net.add_edge(0, 1, 1.0);
        net.add_edge(2, 3, 1.0);
        net.add_edge(4, 5, 1.0);
        net.add_edge(6, 7, 1.0);
        net.add_edge(8, 9, 1.0);

        let config = auto_config_with_network(10, &net);
        assert!(
            (config.teleportation_rate - 0.01).abs() < f64::EPSILON,
            "Sparse graph (avg degree < 3) should get τ=0.01, got {}",
            config.teleportation_rate
        );
    }

    // ── 24. test_auto_config_with_network_dense ───────────────────────

    #[test]
    fn test_auto_config_with_network_dense() {
        // Dense graph: 5 nodes, fully connected = 20 edges → avg degree 4.0
        let mut net = Network::new();
        for i in 0..5 {
            net.add_node_name(i, &format!("node_{}", i));
        }
        for i in 0..5 {
            for j in 0..5 {
                if i != j {
                    net.add_edge(i, j, 1.0);
                }
            }
        }

        let config = auto_config_with_network(5, &net);
        assert!(
            (config.teleportation_rate - 0.05).abs() < f64::EPSILON,
            "Normal density graph (avg degree 3-20) should get τ=0.05, got {}",
            config.teleportation_rate
        );
    }

    // ── 25. test_auto_config_with_network_very_dense ──────────────────

    #[test]
    fn test_auto_config_with_network_very_dense() {
        // Very dense: 3 nodes, many edges → avg degree > 20
        let mut net = Network::new();
        for i in 0..3 {
            net.add_node_name(i, &format!("node_{}", i));
        }
        // Add lots of edges (multi-edges to inflate degree)
        for _ in 0..30 {
            net.add_edge(0, 1, 1.0);
            net.add_edge(1, 2, 1.0);
            net.add_edge(2, 0, 1.0);
        }

        let config = auto_config_with_network(3, &net);
        assert!(
            (config.teleportation_rate - 0.10).abs() < f64::EPSILON,
            "Very dense graph (avg degree > 20) should get τ=0.10, got {}",
            config.teleportation_rate
        );
    }

    // ── 26. test_dir_colocation_edges_basic ────────────────────────────

    #[test]
    fn test_dir_colocation_edges_basic() {
        let mut g = Graph::default();
        // 3 files in same directory, no explicit edges
        g.nodes.push(make_file_node("src/models/user.rs"));
        g.nodes.push(make_file_node("src/models/post.rs"));
        g.nodes.push(make_file_node("src/models/comment.rs"));
        // 1 file in different directory
        g.nodes.push(make_file_node("src/utils/helper.rs"));

        let (mut net, idx_to_id) = build_network(&g);

        // Before co-location: no edges at all
        assert_eq!(net.num_edges(), 0, "Should have no edges before co-location");

        add_dir_colocation_edges(&mut net, &idx_to_id, 0.3);

        // After: 3 files in src/models → 3 pairs × 2 directions = 6 edges
        // helper.rs is alone in src/utils → 0 new edges
        assert_eq!(
            net.num_edges(), 6,
            "3 files in same dir should produce 6 directed co-location edges"
        );
    }

    // ── 27. test_dir_colocation_improves_clustering ────────────────────

    #[test]
    fn test_dir_colocation_improves_clustering() {
        let mut g = Graph::default();

        // Cluster A: tightly connected
        let cluster_a = ["src/core/a.rs", "src/core/b.rs", "src/core/c.rs"];
        for p in &cluster_a {
            g.nodes.push(make_file_node(p));
        }
        for i in 0..cluster_a.len() {
            for j in 0..cluster_a.len() {
                if i != j {
                    g.edges.push(Edge::new(
                        &format!("file:{}", cluster_a[i]),
                        &format!("file:{}", cluster_a[j]),
                        "calls",
                    ));
                }
            }
        }

        // 3 isolated files in same directory — no explicit edges
        // With co-location, they should cluster together instead of being singletons
        g.nodes.push(make_file_node("src/config/base.rs"));
        g.nodes.push(make_file_node("src/config/env.rs"));
        g.nodes.push(make_file_node("src/config/defaults.rs"));

        // With co-location enabled (default)
        let config = ClusterConfig {
            seed: 42,
            num_trials: 10,
            min_community_size: 2,
            ..Default::default()
        };
        let result = cluster(&g, &config).unwrap();

        // No singletons allowed
        for node in &result.nodes {
            let size = node.metadata.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
            assert!(
                size >= 2,
                "Component '{}' has size {} — co-location should prevent singletons",
                node.title, size
            );
        }
    }

    // ── 28. test_dir_colocation_disabled ───────────────────────────────

    #[test]
    fn test_dir_colocation_disabled() {
        let mut g = Graph::default();
        g.nodes.push(make_file_node("src/models/a.rs"));
        g.nodes.push(make_file_node("src/models/b.rs"));

        let (mut net, idx_to_id) = build_network(&g);
        assert_eq!(net.num_edges(), 0);

        // Weight = 0.0 should disable co-location
        add_dir_colocation_edges(&mut net, &idx_to_id, 0.0);
        assert_eq!(net.num_edges(), 0, "Weight 0.0 should add no edges");
    }

    // ── 29. test_sink_file_diagnostics — Verifies orphan metrics for sink patterns ──

    #[test]
    fn test_sink_file_diagnostics() {
        // Simulate the "sink file" pattern:
        // errors.ts is imported by files in 3 different semantic clusters
        // but errors.ts itself doesn't import anything.
        // Question: does orphan reassignment correctly merge it, or does it stay singleton?

        let mut g = Graph::default();

        // 3 distinct clusters of files (tight internal coupling)
        // Cluster A: auth module
        for name in &["auth/login.ts", "auth/register.ts", "auth/session.ts"] {
            g.nodes.push(make_file_node(&format!("src/{}", name)));
        }
        // Cluster B: commands module
        for name in &[
            "commands/run.ts",
            "commands/build.ts",
            "commands/test.ts",
            "commands/deploy.ts",
        ] {
            g.nodes.push(make_file_node(&format!("src/{}", name)));
        }
        // Cluster C: ui module
        for name in &["ui/render.ts", "ui/layout.ts", "ui/theme.ts"] {
            g.nodes.push(make_file_node(&format!("src/{}", name)));
        }

        // The sink file — imported by everyone, imports nothing
        g.nodes.push(make_file_node("src/utils/errors.ts"));

        // Internal cluster edges (strong coupling within clusters)
        let cluster_a = vec!["src/auth/login.ts", "src/auth/register.ts", "src/auth/session.ts"];
        for i in 0..cluster_a.len() {
            for j in (i + 1)..cluster_a.len() {
                g.edges.push(Edge::new(
                    &format!("file:{}", cluster_a[i]),
                    &format!("file:{}", cluster_a[j]),
                    "imports",
                ));
            }
        }
        let cluster_b = vec![
            "src/commands/run.ts",
            "src/commands/build.ts",
            "src/commands/test.ts",
            "src/commands/deploy.ts",
        ];
        for i in 0..cluster_b.len() {
            for j in (i + 1)..cluster_b.len() {
                g.edges.push(Edge::new(
                    &format!("file:{}", cluster_b[i]),
                    &format!("file:{}", cluster_b[j]),
                    "imports",
                ));
            }
        }
        let cluster_c = vec!["src/ui/render.ts", "src/ui/layout.ts", "src/ui/theme.ts"];
        for i in 0..cluster_c.len() {
            for j in (i + 1)..cluster_c.len() {
                g.edges.push(Edge::new(
                    &format!("file:{}", cluster_c[i]),
                    &format!("file:{}", cluster_c[j]),
                    "imports",
                ));
            }
        }

        // Sink edges: multiple files import errors.ts
        // More importers from cluster B (4 files) than A (2) or C (1)
        for src in &["src/auth/login.ts", "src/auth/register.ts"] {
            g.edges.push(Edge::new(
                &format!("file:{}", src),
                "file:src/utils/errors.ts",
                "imports",
            ));
        }
        for src in &[
            "src/commands/run.ts",
            "src/commands/build.ts",
            "src/commands/test.ts",
            "src/commands/deploy.ts",
        ] {
            g.edges.push(Edge::new(
                &format!("file:{}", src),
                "file:src/utils/errors.ts",
                "imports",
            ));
        }
        g.edges.push(Edge::new(
            "file:src/ui/render.ts",
            "file:src/utils/errors.ts",
            "imports",
        ));

        let config = ClusterConfig {
            min_community_size: 2,
            ..Default::default()
        };

        let (mut net, idx_to_id) = build_network(&g);
        add_dir_colocation_edges(&mut net, &idx_to_id, config.dir_colocation_weight);
        let (clusters, metrics) = run_clustering(&net, &idx_to_id, &config);

        eprintln!("=== Sink File Diagnostic Test ===");
        eprintln!("Total nodes: {}", metrics.num_total);
        eprintln!("Infomap communities: {}", metrics.num_communities);
        eprintln!("Orphans (raw): {}", metrics.orphan_count_raw);
        eprintln!("Merged by affinity: {}", metrics.orphans_merged_by_affinity);
        eprintln!("Assigned by dir: {}", metrics.orphans_assigned_by_dir);
        eprintln!("Singleton clusters (final): {}", metrics.singleton_clusters_final);
        eprintln!("Total clusters: {}", clusters.len());
        for (i, c) in clusters.iter().enumerate() {
            eprintln!("  Cluster {}: {} members: {:?}", i, c.member_ids.len(), c.member_ids);
        }

        // errors.ts SHOULD be merged into some cluster (not be a singleton)
        let errors_cluster = clusters
            .iter()
            .find(|c| c.member_ids.iter().any(|m| m.contains("errors.ts")));
        assert!(
            errors_cluster.is_some(),
            "errors.ts should be in a cluster"
        );
        let errors_cluster = errors_cluster.unwrap();

        // Key assertion: errors.ts should NOT be alone
        assert!(
            errors_cluster.member_ids.len() > 1,
            "errors.ts should be merged with its importers, not be a singleton. Cluster: {:?}",
            errors_cluster.member_ids
        );

        // Diagnostic: which cluster did it join? Ideally cluster B (most importers)
        let joined_commands = errors_cluster
            .member_ids
            .iter()
            .any(|m| m.contains("commands"));
        eprintln!(
            "errors.ts joined commands cluster: {} (expected: true, since 4/7 importers are commands)",
            joined_commands
        );
    }

    // ── 30. test_truly_isolated_files — Files with zero edges become dir-fallback singletons ──

    #[test]
    fn test_truly_isolated_files() {
        // Simulate files that have NO import/call edges — only dir colocation.
        // These are the real source of singletons: standalone tool files, test fixtures, etc.

        let mut g = Graph::default();

        // One real cluster
        for name in &["core/engine.ts", "core/parser.ts", "core/lexer.ts"] {
            g.nodes.push(make_file_node(&format!("src/{}", name)));
        }
        g.edges.push(Edge::new("file:src/core/engine.ts", "file:src/core/parser.ts", "imports"));
        g.edges.push(Edge::new("file:src/core/engine.ts", "file:src/core/lexer.ts", "imports"));
        g.edges.push(Edge::new("file:src/core/parser.ts", "file:src/core/lexer.ts", "imports"));

        // Isolated files in different directories (each alone in its dir)
        g.nodes.push(make_file_node("src/tools/bash.ts"));
        g.nodes.push(make_file_node("src/tools/glob.ts"));
        g.nodes.push(make_file_node("src/tools/grep.ts"));
        g.nodes.push(make_file_node("src/fixtures/sample.ts"));
        g.nodes.push(make_file_node("src/generated/schema.ts"));

        let config = ClusterConfig {
            min_community_size: 2,
            ..Default::default()
        };

        let (mut net, idx_to_id) = build_network(&g);
        add_dir_colocation_edges(&mut net, &idx_to_id, config.dir_colocation_weight);
        let (clusters, metrics) = run_clustering(&net, &idx_to_id, &config);

        eprintln!("=== Truly Isolated Files Test ===");
        eprintln!("Total nodes: {}", metrics.num_total);
        eprintln!("Infomap communities: {}", metrics.num_communities);
        eprintln!("Orphans (raw): {}", metrics.orphan_count_raw);
        eprintln!("Merged by affinity: {}", metrics.orphans_merged_by_affinity);
        eprintln!("Assigned by dir: {}", metrics.orphans_assigned_by_dir);
        eprintln!("Singleton clusters (final): {}", metrics.singleton_clusters_final);
        eprintln!("Total clusters: {}", clusters.len());
        for (i, c) in clusters.iter().enumerate() {
            eprintln!("  Cluster {}: {} members: {:?}", i, c.member_ids.len(), c.member_ids);
        }

        // tools/ directory has 3 files → should be grouped by dir colocation
        let tools_cluster = clusters
            .iter()
            .find(|c| c.member_ids.iter().any(|m| m.contains("tools/")));
        if let Some(tc) = tools_cluster {
            eprintln!("Tools cluster size: {} (expected: 3 from dir colocation)", tc.member_ids.len());
        }

        // fixtures/ and generated/ are each alone → will be singletons
        let singleton_count = metrics.singleton_clusters_final;
        eprintln!("Singletons: {} (these are truly isolated files)", singleton_count);
    }
}
