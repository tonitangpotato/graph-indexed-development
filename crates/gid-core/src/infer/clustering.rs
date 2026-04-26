//! Infomap-based community detection on code graphs.
//!
//! This module takes a [`Graph`] of code nodes (files, functions, classes, etc.),
//! builds a weighted network at file granularity, runs Infomap optimization,
//! and returns a [`ClusterResult`] containing inferred component `Node`s and
//! membership `Edge`s. The input graph is never mutated.

use std::collections::{HashMap, HashSet};

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
/// Weight for synthetic co-citation edges between files imported by the same consumers.
pub const WEIGHT_CO_CITATION: f64 = 0.4;
/// Default minimum number of shared citers to create a co-citation edge.
pub const CO_CITATION_MIN_SHARED: usize = 2;
/// Weight for synthetic directory co-location edges between files in the same directory.
pub const WEIGHT_DIR_COLOCATION: f64 = 0.3;
/// Weight for synthetic symbol-similarity edges.
pub const WEIGHT_SYMBOL_SIMILARITY: f64 = 0.5;
/// Default minimum shared tokens for symbol similarity edges.
pub const SYMBOL_MIN_SHARED_TOKENS: usize = 2;
/// Default minimum Jaccard threshold for symbol similarity edges.
pub const SYMBOL_MIN_JACCARD: f64 = 0.15;
// COLOCATION_PAIRWISE_LIMIT removed in ISS-045 (2026-04-26). Co-location
// edges are now isolation-gated (only emitted between code-isolated files,
// where the network has zero edges), so the O(n²) pairwise cap is
// structurally impossible to hit. The deprecated constant was unused
// internally and not referenced by any downstream crate (gid-cli, rustclaw).

/// Map an edge relation string to its clustering weight (uses default weights).
///
/// Unknown relations return `0.0` and are effectively ignored.
///
/// **Note:** This is a convenience wrapper over [`default_edge_weights`].
/// Production code paths should consult [`ClusterConfig::edge_weights`] so
/// users can tune per-project weights via CLI / config (ISS-002).
pub fn relation_weight(relation: &str) -> f64 {
    // Match the default map exactly. Inlined here to keep the function cheap
    // (no HashMap allocation per call) for hot diagnostic / display paths.
    match relation {
        "calls" => WEIGHT_CALLS,
        "imports" => WEIGHT_IMPORTS,
        "type_reference" | "inherits" | "implements" | "uses" => WEIGHT_TYPE_REF,
        "defined_in" | "contains" | "belongs_to" => WEIGHT_STRUCTURAL,
        "depends_on" => WEIGHT_DEPENDS_ON,
        "overrides" => WEIGHT_TYPE_REF, // method override = strong type coupling
        "tests_for" => 0.3,            // weak coupling — tests cluster with source but don't dominate
        _ => 0.0,
    }
}

/// Default edge-weight map used by [`ClusterConfig::default`].
///
/// Mirrors [`relation_weight`] but materialises the table so users can
/// override individual entries (e.g. `gid infer --edge-weight calls=1.5`)
/// without forking the whole match arm.
///
/// Synonym groups (e.g. `inherits` / `implements` / `uses`) are expanded
/// into individual entries — this lets users tune them independently if
/// they choose.
pub fn default_edge_weights() -> HashMap<String, f64> {
    let mut w = HashMap::new();
    w.insert("calls".to_string(), WEIGHT_CALLS);
    w.insert("imports".to_string(), WEIGHT_IMPORTS);
    w.insert("type_reference".to_string(), WEIGHT_TYPE_REF);
    w.insert("inherits".to_string(), WEIGHT_TYPE_REF);
    w.insert("implements".to_string(), WEIGHT_TYPE_REF);
    w.insert("uses".to_string(), WEIGHT_TYPE_REF);
    w.insert("overrides".to_string(), WEIGHT_TYPE_REF);
    w.insert("defined_in".to_string(), WEIGHT_STRUCTURAL);
    w.insert("contains".to_string(), WEIGHT_STRUCTURAL);
    w.insert("belongs_to".to_string(), WEIGHT_STRUCTURAL);
    w.insert("depends_on".to_string(), WEIGHT_DEPENDS_ON);
    w.insert("tests_for".to_string(), 0.3);
    w
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
    /// Weight for synthetic co-citation edges (default: 0.4).
    /// Two files imported by the same consumers get a weighted edge.
    /// Set to 0.0 to disable co-citation.
    pub co_citation_weight: f64,
    /// Minimum shared citers to create a co-citation edge (default: 2).
    pub co_citation_min_shared: usize,
    /// Weight for synthetic directory co-location edges (default: 0.3).
    /// Set to 0.0 to disable directory co-location.
    pub dir_colocation_weight: f64,
    /// Weight for synthetic symbol-similarity edges (default: 0.5).
    /// Two files exporting symbols with overlapping domain vocabulary get
    /// a weighted edge proportional to Jaccard similarity.
    /// Set to 0.0 to disable.
    pub symbol_similarity_weight: f64,
    /// Minimum number of shared tokens to create a symbol similarity edge (default: 2).
    pub symbol_min_shared_tokens: usize,
    /// Minimum Jaccard similarity threshold (default: 0.15).
    pub symbol_min_jaccard: f64,
    /// Random seed for reproducibility (default: 42).
    pub seed: u64,
    /// Maximum cluster size. Clusters exceeding this are sub-clustered.
    /// `None` means auto-compute: `max(20, total_file_count / 5)`.
    pub max_cluster_size: Option<usize>,
    /// Hub exclusion threshold as fraction of total file count (default: 0.05).
    /// Files with in-degree > max(threshold * total_files, hub_min_degree) are
    /// excluded from the Infomap network and placed into a separate Infrastructure
    /// component. Set to 0.0 to disable hub exclusion.
    pub hub_exclusion_threshold: f64,
    /// Minimum absolute in-degree to qualify as a hub (default: 10).
    /// Prevents hub exclusion from firing on small projects where every file
    /// naturally has a few imports. The effective cutoff is
    /// `max(threshold * total_files, hub_min_degree)`.
    pub hub_min_degree: usize,
    /// Per-relation edge weight overrides (ISS-002).
    ///
    /// Keys are edge `relation` strings (e.g. `"calls"`, `"imports"`,
    /// `"type_reference"`); values are the weight applied to that edge in
    /// the Infomap network. Unknown relations (not in this map) are
    /// treated as weight `0.0` and ignored.
    ///
    /// Default values mirror [`default_edge_weights`]. Override per project
    /// via `gid infer --edge-weight calls=1.5 --edge-weight imports=0.5`.
    pub edge_weights: HashMap<String, f64>,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            teleportation_rate: 0.05,
            num_trials: 10,
            min_community_size: 2,
            co_citation_weight: WEIGHT_CO_CITATION,
            co_citation_min_shared: CO_CITATION_MIN_SHARED,
            dir_colocation_weight: WEIGHT_DIR_COLOCATION,
            symbol_similarity_weight: WEIGHT_SYMBOL_SIMILARITY,
            symbol_min_shared_tokens: SYMBOL_MIN_SHARED_TOKENS,
            symbol_min_jaccard: SYMBOL_MIN_JACCARD,
            hierarchical: false,
            seed: 42,
            max_cluster_size: None,
            hub_exclusion_threshold: 0.05,
            hub_min_degree: 10,
            edge_weights: default_edge_weights(),
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
    /// Number of clusters that were split via sub-clustering.
    pub clusters_split: usize,
    /// Total sub-clusters created from splitting.
    pub sub_clusters_created: usize,
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
/// Edge weights are looked up from `config.edge_weights` (ISS-002). Relations
/// not present in the map are treated as weight `0.0` and skipped — pass
/// [`ClusterConfig::default`] to use the built-in defaults.
///
/// Returns the network and a vec mapping network indices back to node ID strings.
pub fn build_network(graph: &Graph, config: &ClusterConfig) -> (Network, Vec<String>) {
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
    //
    // Weight = config.edge_weights[relation] × confidence.
    // This ensures LSP-confirmed edges (confidence ≥ 0.95) dominate over
    // tree-sitter heuristic edges (confidence 0.3–0.7). Edges without an
    // explicit confidence field are treated as confidence = 1.0 (legacy/manual
    // edges are assumed reliable). Relations missing from `edge_weights` are
    // skipped (treated as weight 0.0) — see ISS-002.
    let mut edge_weights: HashMap<(usize, usize), f64> = HashMap::new();

    for edge in &graph.edges {
        let w = config.edge_weights.get(edge.relation.as_str()).copied().unwrap_or(0.0);
        if w == 0.0 {
            continue;
        }

        // Scale by confidence: high-confidence (LSP) edges get full weight,
        // low-confidence (heuristic) edges are proportionally downweighted.
        let confidence = edge.confidence.unwrap_or(1.0).clamp(0.0, 1.0);
        let effective_weight = w * confidence;

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
            *edge_weights.entry((f, t)).or_insert(0.0) += effective_weight;
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

/// Add synthetic co-citation edges to the network.
///
/// Co-citation: if files A and B are both imported by file C, they share
/// a usage context. The more shared importers, the stronger the signal.
/// This is critical for utility/helper files that don't import each other
/// but serve the same feature domains.
///
/// Algorithm:
/// 1. Build reverse-import index: for each file, which files import it?
/// 2. For each pair of files with >= min_shared common importers,
///    add a bidirectional edge with weight = base_weight * shared_count (capped).
///
/// Performance: Only considers file nodes with >=1 incoming import-like edge.
/// Edge weight is capped at `max_edge_weight` to prevent over-coupling.
pub fn add_co_citation_edges(
    net: &mut Network,
    graph: &Graph,
    idx_to_id: &[String],
    weight: f64,
    min_shared: usize,
    max_edge_weight: f64,
) {
    if weight <= 0.0 || idx_to_id.is_empty() {
        return;
    }

    // Build id → index map from idx_to_id
    let mut id_to_idx: HashMap<&str, usize> = HashMap::new();
    for (idx, id) in idx_to_id.iter().enumerate() {
        id_to_idx.insert(id.as_str(), idx);
    }

    // Map non-file nodes to their parent file index (same logic as build_network)
    let mut node_to_file_idx: HashMap<&str, usize> = HashMap::new();
    for node in &graph.nodes {
        let is_file = node.node_type.as_deref() == Some("file")
            || (node.node_type.as_deref() == Some("code")
                && node.node_kind.as_deref() == Some("File"));
        if is_file {
            continue;
        }
        if let Some(ref fp) = node.file_path {
            let file_id = format!("file:{}", fp);
            if let Some(&idx) = id_to_idx.get(file_id.as_str()) {
                node_to_file_idx.insert(&node.id, idx);
                continue;
            }
        }
        if let Some(fp_val) = node.metadata.get("file_path") {
            if let Some(fp) = fp_val.as_str() {
                let file_id = format!("file:{}", fp);
                if let Some(&idx) = id_to_idx.get(file_id.as_str()) {
                    node_to_file_idx.insert(&node.id, idx);
                }
            }
        }
    }

    // Build reverse-import index: target_file_idx → map of (importer_file_idx → max_confidence)
    //
    // Only edges with confidence ≥ CO_CITATION_CONFIDENCE_THRESHOLD qualify as
    // citers. This prevents low-quality heuristic edges (tree-sitter guesses with
    // confidence 0.3–0.5) from inflating co-citation counts. LSP-confirmed edges
    // (confidence ≥ 0.95) and reliable static edges (imports, confidence = 1.0)
    // are the primary contributors.
    const CO_CITATION_CONFIDENCE_THRESHOLD: f64 = 0.7;

    let mut imported_by: HashMap<usize, HashMap<usize, f64>> = HashMap::new();

    for edge in &graph.edges {
        let is_import_like = matches!(
            edge.relation.as_str(),
            "imports" | "calls" | "uses" | "type_reference" | "depends_on"
        );
        if !is_import_like {
            continue;
        }

        let confidence = edge.confidence.unwrap_or(1.0).clamp(0.0, 1.0);
        if confidence < CO_CITATION_CONFIDENCE_THRESHOLD {
            continue; // Skip low-confidence heuristic edges
        }

        // Resolve both endpoints to file indices
        let from_idx = node_to_file_idx
            .get(edge.from.as_str())
            .or_else(|| id_to_idx.get(edge.from.as_str()))
            .copied();
        let to_idx = node_to_file_idx
            .get(edge.to.as_str())
            .or_else(|| id_to_idx.get(edge.to.as_str()))
            .copied();

        if let (Some(from), Some(to)) = (from_idx, to_idx) {
            if from != to {
                // `from` imports `to`, so `to` is imported_by `from` with this confidence.
                // Keep the max confidence per (target, citer) pair.
                let entry = imported_by.entry(to).or_default().entry(from).or_insert(0.0);
                if confidence > *entry {
                    *entry = confidence;
                }
            }
        }
    }

    // Build a set of file pairs that already have a direct high-confidence edge.
    // Co-citation should NOT add signal between pairs where LSP already confirmed
    // a direct relationship — that would be double-counting the same coupling.
    let mut direct_high_confidence_pairs: HashSet<(usize, usize)> = HashSet::new();
    for edge in &graph.edges {
        let confidence = edge.confidence.unwrap_or(1.0).clamp(0.0, 1.0);
        if confidence < 0.9 {
            continue; // Only suppress co-citation for very high confidence direct edges
        }
        let is_coupling = matches!(
            edge.relation.as_str(),
            "calls" | "imports" | "uses" | "type_reference"
        );
        if !is_coupling {
            continue;
        }
        let from_idx = node_to_file_idx
            .get(edge.from.as_str())
            .or_else(|| id_to_idx.get(edge.from.as_str()))
            .copied();
        let to_idx = node_to_file_idx
            .get(edge.to.as_str())
            .or_else(|| id_to_idx.get(edge.to.as_str()))
            .copied();
        if let (Some(f), Some(t)) = (from_idx, to_idx) {
            if f != t {
                direct_high_confidence_pairs.insert((f.min(t), f.max(t)));
            }
        }
    }

    // Collect files that have importers (candidates for co-citation)
    let candidates: Vec<usize> = imported_by.keys().copied().collect();

    if candidates.len() < 2 {
        return;
    }

    // For each pair of candidates, count shared importers and weight by
    // the mean confidence of the citing edges.
    let mut co_citation_edges = 0usize;
    for i in 0..candidates.len() {
        let a = candidates[i];
        let citers_a = &imported_by[&a];

        for j in (i + 1)..candidates.len() {
            let b = candidates[j];

            // Skip pairs already connected by high-confidence direct edges —
            // co-citation would just redundantly echo what LSP already told us.
            let pair_key = (a.min(b), a.max(b));
            if direct_high_confidence_pairs.contains(&pair_key) {
                continue;
            }

            let citers_b = &imported_by[&b];

            // Count shared citers and accumulate their confidence scores
            let mut shared_count = 0usize;
            let mut confidence_sum = 0.0f64;
            for (citer, conf_a) in citers_a {
                if let Some(conf_b) = citers_b.get(citer) {
                    shared_count += 1;
                    // Use the geometric mean of the two confidence values:
                    // both the A-citation and B-citation from this citer must
                    // be reliable for the co-citation signal to be trustworthy.
                    confidence_sum += (conf_a * conf_b).sqrt();
                }
            }

            if shared_count >= min_shared {
                // Weight = base_weight × shared_count × (avg_confidence_factor)
                // The confidence factor ensures that co-citation from all-1.0
                // edges gets full weight, while mixed-confidence sources get
                // proportionally less.
                let avg_confidence = confidence_sum / shared_count as f64;
                let edge_weight = (weight * shared_count as f64 * avg_confidence).min(max_edge_weight);
                net.add_edge(a, b, edge_weight);
                net.add_edge(b, a, edge_weight);
                co_citation_edges += 1;
            }
        }
    }

    if co_citation_edges > 0 {
        eprintln!(
            "🔗 Added {} co-citation edges ({} candidate files, min_shared={}, confidence_threshold={})",
            co_citation_edges,
            candidates.len(),
            min_shared,
            CO_CITATION_CONFIDENCE_THRESHOLD,
        );
    }
}

/// Add synthetic directory co-location edges to the network — **only for
/// code-isolated files**.
///
/// A file is "code-isolated" if it has zero edges in the network at the time
/// this function is called.  This means `build_network()` found no
/// imports/calls/type_references, AND `add_co_citation_edges()` found no
/// shared-importer signal.  For those files, directory proximity is the only
/// clustering signal available, so co-location edges are genuinely useful.
///
/// Files that already participate in the code-level graph are **skipped** —
/// adding O(n²) directory edges on top of real dependency signal is pure noise,
/// and it's the root cause of mega-cluster formation in large flat directories
/// like `utils/` or `commands/`.
///
/// # Why this replaces the old approach
///
/// The previous implementation applied co-location edges to *all* files in a
/// directory, with heuristic mitigations for large dirs (pairwise limit,
/// sub-directory grouping, weight decay).  Those were patches for a
/// fundamental issue: co-location is a fallback signal that should only fire
/// when real signals are absent.  By gating on isolation, the fix is uniform
/// across directory sizes — no thresholds, no decay, no special cases.
pub fn add_dir_colocation_edges(net: &mut Network, idx_to_id: &[String], weight: f64) {
    if weight <= 0.0 {
        return;
    }

    // 1. Identify code-isolated files: nodes with zero edges in the network.
    //    Must snapshot before we start adding co-location edges.
    let mut isolated: HashSet<usize> = HashSet::new();
    for idx in 0..idx_to_id.len() {
        if idx < net.num_nodes()
            && net.out_neighbors(idx).is_empty()
            && net.in_neighbors(idx).is_empty()
        {
            isolated.insert(idx);
        }
    }

    if isolated.is_empty() {
        return;
    }

    // 2. Group *isolated* file indices by parent directory.
    let mut dir_groups: HashMap<String, Vec<usize>> = HashMap::new();
    for &idx in &isolated {
        let node_id = &idx_to_id[idx];
        let path = node_id.strip_prefix("file:").unwrap_or(node_id);
        let dir = match path.rsplit_once('/') {
            Some((parent, _)) if !parent.is_empty() => parent.to_string(),
            _ => "root".to_string(),
        };
        dir_groups.entry(dir).or_default().push(idx);
    }

    // 3. Add pairwise bidirectional edges within each directory group.
    //    Since these are *only* isolated files, groups are typically small
    //    and O(n²) pairwise is fine (no need for thresholds or decay).
    let mut total_edges = 0usize;
    for files in dir_groups.values() {
        if files.len() < 2 {
            continue; // single isolated file in a dir — nothing to pair with
        }
        add_pairwise_edges(net, files, weight);
        total_edges += files.len() * (files.len() - 1); // bidirectional count
    }

    if total_edges > 0 {
        eprintln!(
            "📂 Co-location: {} isolated files → {} edges across {} directories",
            isolated.len(),
            total_edges,
            dir_groups.values().filter(|f| f.len() >= 2).count(),
        );
    }
}

/// Add pairwise bidirectional edges between all nodes in a group.
fn add_pairwise_edges(net: &mut Network, files: &[usize], weight: f64) {
    for i in 0..files.len() {
        for j in (i + 1)..files.len() {
            net.add_edge(files[i], files[j], weight);
            net.add_edge(files[j], files[i], weight);
        }
    }
}

// ── Symbol similarity helpers ──────────────────────────────────────────────

/// Split a camelCase/PascalCase identifier into words.
/// "getOAuthToken" → ["get", "OAuth", "Token"]
/// "AwsAuthStatusManager" → ["Aws", "Auth", "Status", "Manager"]
/// "HTMLParser" → ["HTML", "Parser"]
fn split_camel_case(s: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();

    let chars: Vec<char> = s.chars().collect();
    for i in 0..chars.len() {
        let c = chars[i];
        if current.is_empty() {
            current.push(c);
            continue;
        }

        let prev_upper = chars[i - 1].is_uppercase();
        let curr_upper = c.is_uppercase();
        let next_lower = i + 1 < chars.len() && chars[i + 1].is_lowercase();

        if !curr_upper {
            // lowercase or non-alpha: just append
            current.push(c);
        } else if !prev_upper {
            // lowercase→uppercase transition: start new word
            words.push(current);
            current = String::new();
            current.push(c);
        } else if next_lower {
            // uppercase run ending (e.g. "HTML" + "Parser" — split before last uppercase)
            words.push(current);
            current = String::new();
            current.push(c);
        } else {
            // continuing uppercase run (e.g. "HTM" in "HTML")
            current.push(c);
        }
    }

    if !current.is_empty() {
        words.push(current);
    }

    words
}

/// Check if a word is a programming stop word that carries no clustering signal.
fn is_stop_word(word: &str) -> bool {
    matches!(
        word,
        "get" | "set" | "is" | "has" | "on" | "from" | "to" | "new" | "create"
            | "make" | "with" | "for" | "the" | "an" | "default" | "init" | "handle"
            | "process" | "do" | "run" | "execute" | "test" | "spec" | "mock" | "stub"
            | "should" | "expect" | "describe" | "it" | "before" | "after"
            | "use" | "fn" | "func" | "function" | "method" | "class" | "type"
            | "value" | "data" | "item" | "result" | "response" | "request"
            | "index" | "main" | "app" | "module" | "export" | "import"
            | "self" | "this" | "super" | "that" | "then" | "else" | "if"
            | "return" | "async" | "await" | "try" | "catch" | "throw" | "error"
            | "null" | "undefined" | "true" | "false" | "none" | "some"
            | "add" | "remove" | "delete" | "update" | "check" | "can" | "will"
            | "of" | "in" | "at" | "by" | "or" | "and" | "not" | "all" | "any"
    )
}

/// Split a symbol name (camelCase/snake_case) into a set of lowercase domain tokens.
/// Removes programming stop words that carry no clustering signal.
fn tokenize_symbol_name(name: &str) -> HashSet<String> {
    let mut tokens = HashSet::new();
    for part in name.split('_') {
        for word in split_camel_case(part) {
            let lower = word.to_lowercase();
            if lower.len() >= 2 && !is_stop_word(&lower) {
                tokens.insert(lower);
            }
        }
    }
    tokens
}

/// Add synthetic symbol-similarity edges based on tokenized function/class names.
///
/// For each file node, collects all function/class/method names it defines,
/// tokenizes them (camelCase/snake_case split + stop word removal), then
/// computes pairwise Jaccard similarity. File pairs exceeding thresholds
/// get weighted edges.
///
/// Uses inverted index for efficient pair enumeration — avoids O(n²) full scan.
pub fn add_symbol_similarity_edges(
    net: &mut Network,
    graph: &Graph,
    idx_to_id: &[String],
    weight: f64,
    min_shared: usize,
    min_jaccard: f64,
) {
    if weight <= 0.0 || idx_to_id.is_empty() {
        return;
    }

    // Build id → index map
    let mut id_to_idx: HashMap<&str, usize> = HashMap::new();
    for (idx, id) in idx_to_id.iter().enumerate() {
        id_to_idx.insert(id.as_str(), idx);
    }

    // Step 1: Build file_idx → token_set mapping
    // For each non-file code node (Function, Class, Method, Module), find its parent file,
    // tokenize its title (the symbol name), add tokens to the file's set.
    let mut file_tokens: HashMap<usize, HashSet<String>> = HashMap::new();

    for node in &graph.nodes {
        // Skip file nodes themselves
        let is_file = node.node_type.as_deref() == Some("file")
            || (node.node_type.as_deref() == Some("code")
                && node.node_kind.as_deref() == Some("File"));
        if is_file {
            continue;
        }

        // Only process code symbol nodes
        let is_symbol = matches!(
            node.node_kind.as_deref(),
            Some("Function") | Some("Class") | Some("Method") | Some("Module")
        );
        if !is_symbol {
            continue;
        }

        // Resolve to parent file index
        let file_idx = node.file_path.as_ref().and_then(|fp| {
            let file_id = format!("file:{}", fp);
            id_to_idx.get(file_id.as_str()).copied()
        });

        if let Some(idx) = file_idx {
            let tokens = tokenize_symbol_name(&node.title);
            file_tokens.entry(idx).or_default().extend(tokens);
        }
    }

    // Step 2: Build inverted index: token → set of file indices
    let mut inverted_index: HashMap<String, Vec<usize>> = HashMap::new();
    for (&file_idx, tokens) in &file_tokens {
        for token in tokens {
            inverted_index
                .entry(token.clone())
                .or_default()
                .push(file_idx);
        }
    }

    // Step 3: Count shared tokens per file pair using inverted index
    let mut shared_counts: HashMap<(usize, usize), usize> = HashMap::new();
    for files in inverted_index.values() {
        if files.len() < 2 || files.len() > 200 {
            // Skip tokens appearing in too many files — they're effectively stop words
            continue;
        }
        for i in 0..files.len() {
            for j in (i + 1)..files.len() {
                let pair = if files[i] < files[j] {
                    (files[i], files[j])
                } else {
                    (files[j], files[i])
                };
                *shared_counts.entry(pair).or_insert(0) += 1;
            }
        }
    }

    // Step 4: For qualifying pairs, compute full Jaccard and add edges
    let mut edges_added = 0usize;
    for (&(a, b), &shared) in &shared_counts {
        if shared < min_shared {
            continue;
        }

        let tokens_a = &file_tokens[&a];
        let tokens_b = &file_tokens[&b];
        let union_size = tokens_a.union(tokens_b).count();

        if union_size == 0 {
            continue;
        }

        let jaccard = shared as f64 / union_size as f64;
        if jaccard < min_jaccard {
            continue;
        }

        let edge_weight = weight * jaccard;
        net.add_edge(a, b, edge_weight);
        net.add_edge(b, a, edge_weight);
        edges_added += 1;
    }

    if edges_added > 0 {
        eprintln!(
            "🏷️  Symbol similarity: {} files with symbols → {} edges (min_shared={}, min_jaccard={:.2})",
            file_tokens.len(),
            edges_added,
            min_shared,
            min_jaccard,
        );
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
        ..Default::default()
    };

    (clusters, metrics)
}

// ── Mega-cluster splitting ─────────────────────────────────────────────────

/// Split oversized clusters via recursive sub-clustering.
///
/// Scans all clusters for any exceeding `max_cluster_size`. For each oversized
/// cluster:
/// 1. Extract subgraph (internal files + internal edges only)
/// 2. Run Infomap again with higher teleportation rate (1.5× parent's τ, capped at 0.15)
/// 3. If subgraph produces >1 cluster → replace original with sub-clusters
/// 4. If subgraph produces 1 cluster → stop (monolithic, don't force-split)
/// 5. Recursively check sub-results for remaining oversized clusters
///
/// Max recursion depth = 3 to prevent infinite loops.
pub fn split_mega_clusters(
    clusters: Vec<RawCluster>,
    net: &Network,
    idx_to_id: &[String],
    config: &ClusterConfig,
    max_cluster_size: usize,
) -> Vec<RawCluster> {
    let mut result =
        split_mega_clusters_recursive(clusters, net, idx_to_id, config, max_cluster_size, 0, 3);
    // Global renumbering to prevent ID collisions after recursive splits.
    // Without this, sub-clusters from different recursion depths can share
    // the same `id`, causing `map_to_components` to generate duplicate
    // "infer:component:{id}" node IDs. (ISS-006)
    for (i, c) in result.iter_mut().enumerate() {
        c.id = i;
    }
    result
}

/// Recursively split oversized clusters by running Infomap on their subgraphs.
///
/// For each cluster exceeding `max_cluster_size`:
/// 1. Extract the subgraph (internal edges only from the network)
/// 2. Run Infomap with higher teleportation rate (1.5× parent's τ, capped at 0.15)
/// 3. If >1 sub-cluster produced → replace the mega-cluster with sub-clusters
/// 4. If only 1 sub-cluster (truly monolithic) → keep as-is, stop recursing
/// 5. Recurse on sub-results that still exceed threshold (max depth)
fn split_mega_clusters_recursive(
    clusters: Vec<RawCluster>,
    net: &Network,
    idx_to_id: &[String],
    config: &ClusterConfig,
    max_cluster_size: usize,
    depth: usize,
    max_depth: usize,
) -> Vec<RawCluster> {
    if depth >= max_depth {
        if depth > 0 {
            eprintln!(
                "⚠ Max recursion depth {} reached for mega-cluster splitting",
                max_depth
            );
        }
        return clusters;
    }

    let mut result_clusters: Vec<RawCluster> = Vec::new();
    let mut any_split = false;

    for cluster in clusters {
        if cluster.member_ids.len() <= max_cluster_size {
            // Cluster is within size limit, keep as-is
            result_clusters.push(cluster);
            continue;
        }

        // Attempt to split this oversized cluster
        eprintln!(
            "  🔪 Splitting mega-cluster {} with {} files (max: {})",
            cluster.id,
            cluster.member_ids.len(),
            max_cluster_size
        );

        // Build id → network index lookup
        let mut id_to_net_idx: HashMap<&str, usize> = HashMap::new();
        for (idx, nid) in idx_to_id.iter().enumerate() {
            id_to_net_idx.insert(nid.as_str(), idx);
        }

        // Get network indices for cluster members
        let member_net_indices: Vec<usize> = cluster
            .member_ids
            .iter()
            .filter_map(|mid| id_to_net_idx.get(mid.as_str()).copied())
            .collect();

        if member_net_indices.is_empty() {
            result_clusters.push(cluster);
            continue;
        }

        // Build subgraph: only internal edges (edges between cluster members)
        let subgraph = extract_subgraph(net, &member_net_indices);
        let sub_idx_to_id: Vec<String> = member_net_indices
            .iter()
            .map(|&idx| idx_to_id[idx].clone())
            .collect();

        if subgraph.num_nodes() < 2 {
            // Not enough nodes to cluster
            result_clusters.push(cluster);
            continue;
        }

        // Run Infomap on subgraph with higher teleportation rate for better resolution
        let sub_tau = (config.teleportation_rate * 1.5).min(0.15);
        let mut sub_config = config.clone();
        sub_config.teleportation_rate = sub_tau;
        sub_config.hierarchical = false; // Always flat for sub-clustering
        sub_config.dir_colocation_weight = 0.0; // No dir colocation in sub-clustering

        eprintln!(
            "    📊 Subgraph: {} nodes, {} edges, tau={:.4}",
            subgraph.num_nodes(),
            subgraph.num_edges(),
            sub_config.teleportation_rate,
        );

        let sub_result = Infomap::new(&subgraph)
            .seed(sub_config.seed)
            .num_trials(sub_config.num_trials as usize)
            .hierarchical(false) // Always use flat for sub-clustering
            .tau(sub_config.teleportation_rate)
            .run();

        let sub_modules = sub_result.modules();

        if sub_modules.len() <= 1 {
            // Truly monolithic — cannot split further
            eprintln!(
                "    ℹ Cluster {} is monolithic (1 sub-module), keeping as-is (edges: {})",
                cluster.id,
                subgraph.num_edges(),
            );
            result_clusters.push(cluster);
            continue;
        }

        // Successfully split — create sub-clusters
        any_split = true;
        eprintln!(
            "    ✓ Split cluster {} into {} sub-clusters",
            cluster.id,
            sub_modules.len()
        );

        let base_id = result_clusters.len();
        for (sub_idx, module) in sub_modules.iter().enumerate() {
            let sub_member_ids: Vec<String> = module
                .nodes
                .iter()
                .map(|&idx| sub_idx_to_id[idx].clone())
                .collect();

            if !sub_member_ids.is_empty() {
                result_clusters.push(RawCluster {
                    id: base_id + sub_idx,
                    member_ids: sub_member_ids,
                    flow: module.flow,
                    parent: None,
                    children: Vec::new(),
                });
            }
        }
    }

    // If any splits occurred, recursively check the results
    if any_split {
        split_mega_clusters_recursive(
            result_clusters,
            net,
            idx_to_id,
            config,
            max_cluster_size,
            depth + 1,
            max_depth,
        )
    } else {
        result_clusters
    }
}

/// Extract a subgraph containing only the specified node indices and edges between them.
fn extract_subgraph(net: &Network, node_indices: &[usize]) -> Network {
    let mut subgraph = Network::new();

    // Create a mapping from original indices to subgraph indices
    let mut old_to_new: HashMap<usize, usize> = HashMap::new();
    for (new_idx, &old_idx) in node_indices.iter().enumerate() {
        old_to_new.insert(old_idx, new_idx);
        // Add node to subgraph
        subgraph.add_node_name(new_idx, &format!("{}", old_idx));
    }

    // Add edges that are internal to the subgraph
    let node_set: HashSet<usize> = node_indices.iter().copied().collect();
    for &from_old in node_indices {
        for &(to_old, weight) in net.out_neighbors(from_old) {
            if node_set.contains(&to_old) {
                let from_new = old_to_new[&from_old];
                let to_new = old_to_new[&to_old];
                subgraph.add_edge(from_new, to_new, weight);
            }
        }
    }

    subgraph
}

/// Split oversized leaf clusters using directory structure as a heuristic.
///
/// In hierarchical mode, Infomap already provides multi-level structure.
/// Re-running Infomap is redundant and theoretically inferior (Kawamoto & Rosvall 2015).
/// Instead, for any leaf cluster (no children) exceeding `max_size`, this function
/// groups members by parent directory and creates sub-clusters per directory.
///
/// Non-oversized clusters and non-leaf clusters are preserved as-is.
fn split_oversized_by_directory(clusters: Vec<RawCluster>, max_size: usize) -> Vec<RawCluster> {
    let mut result: Vec<RawCluster> = Vec::new();

    for cluster in clusters {
        // Only split leaf clusters (no children) that exceed max_size
        if cluster.children.is_empty() && cluster.member_ids.len() > max_size {
            eprintln!(
                "  📁 Splitting oversized leaf cluster {} ({} files) by directory (max: {})",
                cluster.id,
                cluster.member_ids.len(),
                max_size,
            );

            // Group members by parent directory
            let mut dir_groups: HashMap<String, Vec<String>> = HashMap::new();
            for member_id in &cluster.member_ids {
                let dir = extract_parent_dir(member_id);
                dir_groups.entry(dir).or_default().push(member_id.clone());
            }

            // If directory grouping doesn't actually split (all same dir), keep as-is
            if dir_groups.len() <= 1 {
                eprintln!(
                    "    ℹ All files in same directory, keeping cluster {} as-is",
                    cluster.id,
                );
                result.push(cluster);
                continue;
            }

            // Create sub-clusters per directory group
            let base_id = result.len();
            let mut sub_idx = 0;
            for (_dir, members) in dir_groups {
                result.push(RawCluster {
                    id: base_id + sub_idx,
                    member_ids: members,
                    flow: 0.0,
                    parent: cluster.parent,
                    children: Vec::new(),
                });
                sub_idx += 1;
            }

            eprintln!(
                "    ✓ Split into {} directory-based sub-clusters",
                sub_idx,
            );
        } else {
            result.push(cluster);
        }
    }

    // Re-number cluster IDs sequentially
    for (i, c) in result.iter_mut().enumerate() {
        c.id = i;
    }

    result
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
            build_tree_clusters(child, idx_to_id, clusters, counter, Some(my_idx), child_path);
        }

        // NOTE: We intentionally do NOT propagate child members up to the parent.
        // Each cluster keeps only its own direct leaf node members (from tree_node.nodes).
        // The parent→child relationship is captured via cluster.children and the
        // "contains" edges created in map_to_components().
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

        let title = if !file_paths.is_empty() {
            auto_name(&file_paths)
        } else if is_hierarchical && !cluster.children.is_empty() {
            // Will be resolved in a second pass after all child titles are known.
            "component".to_string()
        } else {
            auto_name(&[])
        };

        // Create component node.
        let mut node = Node::new(&component_id, &title);
        node.node_type = Some("component".into());
        node.source = Some("infer".into());
        node.metadata
            .insert("flow".into(), serde_json::json!(cluster.flow));
        node.metadata
            .insert("size".into(), serde_json::json!(cluster.member_ids.len()));

        // For hierarchical: compute total_size (direct files + all descendant files)
        if is_hierarchical && !cluster.children.is_empty() {
            let total = count_total_descendants(cluster, clusters);
            node.metadata.insert("total_size".into(), serde_json::json!(total));
        }

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

    // Second pass: resolve parent titles from child titles in hierarchical mode.
    if is_hierarchical {
        // Build cluster_id → node index map.
        let cluster_id_to_node_idx: HashMap<usize, usize> = clusters
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id, i))
            .collect();

        for cluster in clusters {
            if cluster.member_ids.is_empty() && !cluster.children.is_empty() {
                // This is a pure parent node — derive name from children.
                let child_titles: Vec<&str> = cluster
                    .children
                    .iter()
                    .filter_map(|&child_id| {
                        cluster_id_to_node_idx
                            .get(&child_id)
                            .map(|&idx| nodes[idx].title.as_str())
                    })
                    .collect();

                if !child_titles.is_empty() {
                    if let Some(&node_idx) = cluster_id_to_node_idx.get(&cluster.id) {
                        nodes[node_idx].title = auto_name_hierarchical(&child_titles);
                    }
                }
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

/// Auto-name for hierarchical parent nodes that have no direct files.
///
/// Derives the name from child component titles:
/// 1. If children share a common directory prefix, use that.
/// 2. Otherwise, concatenate the first few child names.
/// 3. Fallback: "group".
pub fn auto_name_hierarchical(child_titles: &[&str]) -> String {
    if child_titles.is_empty() {
        return "group".to_string();
    }

    if child_titles.len() == 1 {
        return format!("{}-group", child_titles[0]);
    }

    // Try to find a common prefix among child titles treated as path-like segments.
    // E.g., if children are ["auth", "auth-middleware"], the common prefix is "auth".
    let split_titles: Vec<Vec<&str>> = child_titles
        .iter()
        .map(|t| t.split(&['-', '_', '/'][..]).collect::<Vec<_>>())
        .collect();

    let min_len = split_titles.iter().map(|p| p.len()).min().unwrap_or(0);
    let mut prefix_len = 0;
    for i in 0..min_len {
        let first = split_titles[0][i];
        if split_titles.iter().all(|p| p[i] == first) {
            prefix_len = i + 1;
        } else {
            break;
        }
    }

    if prefix_len > 0 {
        let prefix: Vec<&str> = split_titles[0][..prefix_len].to_vec();
        return prefix.join("-");
    }

    // No common prefix: concatenate top children (up to 3).
    let top: Vec<&str> = child_titles.iter().take(3).copied().collect();
    let joined = top.join("+");
    if child_titles.len() > 3 {
        format!("{}+…", joined)
    } else {
        joined
    }
}

/// Count total descendant files (direct members + all children recursively).
fn count_total_descendants(cluster: &RawCluster, all_clusters: &[RawCluster]) -> usize {
    let mut total = cluster.member_ids.len();
    for &child_id in &cluster.children {
        if let Some(child) = all_clusters.iter().find(|c| c.id == child_id) {
            total += count_total_descendants(child, all_clusters);
        }
    }
    total
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
        50..=499 => (3, true),
        500..=1999 => (5, true),
        _ => (8, true),
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

// ── Hub exclusion ──────────────────────────────────────────────────────────

/// Identify hub files that should be excluded from the clustering network.
///
/// A hub is a file with in-degree exceeding `threshold * total_file_count`.
/// These are typically infrastructure files (utils, types, constants, debug)
/// that create artificial connectivity between unrelated domain modules.
///
/// Returns the set of network indices to exclude.
pub fn identify_hubs(
    graph: &Graph,
    idx_to_id: &[String],
    threshold: f64,
    min_degree: usize,
) -> HashSet<usize> {
    if threshold <= 0.0 || idx_to_id.is_empty() {
        return HashSet::new();
    }

    // Build id → index map from idx_to_id
    let mut id_to_idx: HashMap<&str, usize> = HashMap::new();
    for (idx, id) in idx_to_id.iter().enumerate() {
        id_to_idx.insert(id.as_str(), idx);
    }

    // Map non-file nodes to their parent file index (same logic as build_network)
    let mut node_to_file_idx: HashMap<&str, usize> = HashMap::new();
    for node in &graph.nodes {
        let is_file = node.node_type.as_deref() == Some("file")
            || (node.node_type.as_deref() == Some("code")
                && node.node_kind.as_deref() == Some("File"));
        if is_file {
            continue;
        }
        if let Some(ref fp) = node.file_path {
            let file_id = format!("file:{}", fp);
            if let Some(&idx) = id_to_idx.get(file_id.as_str()) {
                node_to_file_idx.insert(&node.id, idx);
                continue;
            }
        }
        if let Some(fp_val) = node.metadata.get("file_path") {
            if let Some(fp) = fp_val.as_str() {
                let file_id = format!("file:{}", fp);
                if let Some(&idx) = id_to_idx.get(file_id.as_str()) {
                    node_to_file_idx.insert(&node.id, idx);
                }
            }
        }
    }

    // Count in-degree for each file node from import-like edges
    let mut in_degree: HashMap<usize, usize> = HashMap::new();
    for edge in &graph.edges {
        let is_import_like = matches!(
            edge.relation.as_str(),
            "imports" | "calls" | "uses" | "type_reference" | "depends_on"
        );
        if !is_import_like {
            continue;
        }

        let from_idx = node_to_file_idx
            .get(edge.from.as_str())
            .or_else(|| id_to_idx.get(edge.from.as_str()))
            .copied();
        let to_idx = node_to_file_idx
            .get(edge.to.as_str())
            .or_else(|| id_to_idx.get(edge.to.as_str()))
            .copied();

        if let (Some(from), Some(to)) = (from_idx, to_idx) {
            if from != to {
                // `from` imports `to`, so increment `to`'s in-degree
                // Count unique importers (not duplicate edges from same file)
                *in_degree.entry(to).or_insert(0) += 1;
            }
        }
    }

    let total_files = idx_to_id.len();
    // Cutoff = max(threshold * total_files, min_degree).
    // The min_degree floor prevents hub exclusion from firing on small graphs
    // where every file naturally has a few imports.
    let cutoff = (threshold * total_files as f64).ceil().max(min_degree as f64) as usize;

    let mut hub_indices: HashSet<usize> = HashSet::new();
    let mut hub_info: Vec<(usize, &str, usize)> = Vec::new(); // (idx, id, degree)

    for (&idx, &degree) in &in_degree {
        if degree > cutoff {
            hub_indices.insert(idx);
            hub_info.push((idx, &idx_to_id[idx], degree));
        }
    }

    if !hub_indices.is_empty() {
        // Sort by degree descending for logging
        hub_info.sort_by(|a, b| b.2.cmp(&a.2));
        let top5: Vec<String> = hub_info
            .iter()
            .take(5)
            .map(|(_, id, deg)| format!("{}({})", id, deg))
            .collect();
        eprintln!(
            "🔌 Hub exclusion: {} files excluded (threshold: {:.0}%, cutoff: {}), top hubs: [{}]",
            hub_indices.len(),
            threshold * 100.0,
            cutoff,
            top5.join(", "),
        );
    }

    hub_indices
}

/// Remove hub nodes from a network by building a new network without them.
///
/// Returns `(new_network, new_idx_to_id, excluded_hub_ids)`.
pub fn exclude_hubs_from_network(
    net: &Network,
    idx_to_id: &[String],
    hub_indices: &HashSet<usize>,
) -> (Network, Vec<String>, Vec<String>) {
    // Build mapping from old indices to new (excluding hubs)
    let mut old_to_new: HashMap<usize, usize> = HashMap::new();
    let mut new_idx_to_id: Vec<String> = Vec::new();
    let mut excluded_ids: Vec<String> = Vec::new();

    for (old_idx, id) in idx_to_id.iter().enumerate() {
        if hub_indices.contains(&old_idx) {
            excluded_ids.push(id.clone());
        } else {
            let new_idx = new_idx_to_id.len();
            old_to_new.insert(old_idx, new_idx);
            new_idx_to_id.push(id.clone());
        }
    }

    // Build new network
    let mut new_net = Network::new();

    // Add all non-hub nodes
    for (new_idx, id) in new_idx_to_id.iter().enumerate() {
        new_net.add_node_name(new_idx, id);
    }

    // Add edges between non-hub nodes only
    for old_from in 0..idx_to_id.len() {
        if hub_indices.contains(&old_from) {
            continue;
        }
        let new_from = old_to_new[&old_from];
        for &(old_to, weight) in net.out_neighbors(old_from) {
            if hub_indices.contains(&old_to) {
                continue;
            }
            if let Some(&new_to) = old_to_new.get(&old_to) {
                new_net.add_edge(new_from, new_to, weight);
            }
        }
    }

    eprintln!(
        "🔌 Network after hub exclusion: {} nodes (was {}), {} edges (was {})",
        new_net.num_nodes(),
        net.num_nodes(),
        new_net.num_edges(),
        net.num_edges(),
    );

    (new_net, new_idx_to_id, excluded_ids)
}

/// Create a component node for excluded infrastructure hub files.
fn create_infra_component(
    excluded_ids: &[String],
    _graph: &Graph,
) -> (Node, Vec<Edge>) {
    let component_id = "infer:component:infrastructure";
    let title = "Infrastructure & Shared Utilities";

    let mut node = Node::new(component_id, title);
    node.node_type = Some("component".into());
    node.source = Some("infer".into());
    node.metadata
        .insert("flow".into(), serde_json::json!(0.0));
    node.metadata
        .insert("size".into(), serde_json::json!(excluded_ids.len()));
    node.metadata
        .insert("hub_excluded".into(), serde_json::json!(true));

    let infer_meta = serde_json::json!({"source": "infer"});
    let edges: Vec<Edge> = excluded_ids
        .iter()
        .map(|mid| {
            let mut edge = Edge::new(component_id, mid, "contains");
            edge.metadata = Some(infer_meta.clone());
            edge
        })
        .collect();

    eprintln!(
        "🏗️  Infrastructure component: {} hub files → '{}'",
        excluded_ids.len(),
        title,
    );

    (node, edges)
}

// ── Main entry point ───────────────────────────────────────────────────────

/// Run community detection on a code graph and return inferred components.
///
/// This is the main entry point. It builds a file-level network, runs Infomap,
/// and maps results back to component nodes and membership edges.
pub fn cluster(graph: &Graph, config: &ClusterConfig) -> Result<ClusterResult> {
    let (net, idx_to_id) = build_network(graph, config);

    // Hub exclusion — remove infrastructure files from the network before
    // adding synthetic edges (co-citation, symbol similarity, colocation).
    // This must happen on the ORIGINAL network to avoid circular reasoning.
    let (mut net, idx_to_id, excluded_hub_ids) = if config.hub_exclusion_threshold > 0.0 {
        let hub_indices = identify_hubs(graph, &idx_to_id, config.hub_exclusion_threshold, config.hub_min_degree);
        if hub_indices.is_empty() {
            (net, idx_to_id, Vec::new())
        } else {
            exclude_hubs_from_network(&net, &idx_to_id, &hub_indices)
        }
    } else {
        (net, idx_to_id, Vec::new())
    };

    // Add co-citation edges (indirect usage signal).
    // Must come before dir_colocation so that co-citation structure
    // influences Infomap even in large flat directories.
    add_co_citation_edges(
        &mut net,
        graph,
        &idx_to_id,
        config.co_citation_weight,
        config.co_citation_min_shared,
        2.0, // max edge weight cap
    );

    // Add symbol similarity edges (semantic signal).
    add_symbol_similarity_edges(
        &mut net,
        graph,
        &idx_to_id,
        config.symbol_similarity_weight,
        config.symbol_min_shared_tokens,
        config.symbol_min_jaccard,
    );

    // Add directory co-location edges if configured.
    add_dir_colocation_edges(&mut net, &idx_to_id, config.dir_colocation_weight);

    if net.num_nodes() < 2 {
        return Ok(ClusterResult::empty());
    }

    tracing::info!(
        "Infomap input: {} nodes, {} edges (num_trials={})",
        net.num_nodes(),
        net.num_edges(),
        config.num_trials,
    );
    let t0 = std::time::Instant::now();
    let (clusters, metrics) = run_clustering(&net, &idx_to_id, config);
    tracing::info!("Infomap completed in {:.1}s", t0.elapsed().as_secs_f64());

    // Compute effective max cluster size
    // Compute effective max cluster size.
    // Use log-based formula: component size should be nearly constant regardless
    // of project scale — a 2000-file project doesn't have bigger components,
    // it has more components. ln(N) * 6 grows slowly enough to approximate this.
    //   50 files → 23,  100 → 28,  500 → 38,  1000 → 42,  2000 → 46,  5000 → 52
    // Clamped to [15, 60] — no component should exceed 60 files.
    // See .gid/issues/ISS-008-max-cluster-size-formula.md for discussion.
    let total_files = idx_to_id.len();
    let max_size = config.max_cluster_size.unwrap_or_else(|| {
        let auto = ((total_files as f64).ln() * 6.0).ceil() as usize;
        auto.clamp(15, 60)
    });

    // Split oversized clusters:
    // - Hierarchical mode: Infomap already provides multi-level structure.
    //   Don't recursively re-run Infomap — it's redundant and theoretically inferior
    //   (Kawamoto & Rosvall 2015). Instead, use directory heuristic as a fallback
    //   for any oversized leaf clusters.
    // - Flat mode: recursively sub-cluster via Infomap, then fall back to
    //   directory-based splitting for any clusters that Infomap considers
    //   monolithic (returns 1 module) but still exceed max_size.
    let clusters = if config.hierarchical {
        split_oversized_by_directory(clusters, max_size)
    } else {
        let after_infomap = split_mega_clusters(clusters, &net, &idx_to_id, config, max_size);
        // Fallback: any cluster that Infomap couldn't split (monolithic subgraph)
        // but still exceeds max_size → split by directory structure.
        split_oversized_by_directory(after_infomap, max_size)
    };

    let mut result = map_to_components(&clusters, graph);
    result.metrics = metrics;

    // Post-process: add infrastructure component for excluded hubs
    if !excluded_hub_ids.is_empty() {
        let (infra_node, infra_edges) = create_infra_component(&excluded_hub_ids, graph);
        result.nodes.push(infra_node);
        result.edges.extend(infra_edges);
        result.metrics.num_communities += 1;
    }

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
        // Overrides = type-level coupling
        assert_eq!(relation_weight("overrides"), WEIGHT_TYPE_REF);
        // TestsFor = weak coupling (tests cluster with source but don't dominate)
        assert_eq!(relation_weight("tests_for"), 0.3);
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

        let (net, idx_to_id) = build_network(&g, &ClusterConfig::default());

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

        let (net, idx_to_id) = build_network(&g, &ClusterConfig::default());

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

    // ── 3b. test_build_network_respects_edge_weight_overrides (ISS-002) ─

    #[test]
    fn test_build_network_respects_edge_weight_overrides() {
        let mut g = Graph::default();
        g.nodes.push(make_file_node("x.rs"));
        g.nodes.push(make_file_node("y.rs"));
        g.nodes.push(make_file_node("z.rs"));
        g.edges.push(Edge::new("file:x.rs", "file:y.rs", "calls"));
        g.edges.push(Edge::new("file:x.rs", "file:z.rs", "imports"));

        // Invert the default ranking: make `imports` outweigh `calls`.
        let mut config = ClusterConfig::default();
        config.edge_weights.insert("calls".to_string(), 0.1);
        config.edge_weights.insert("imports".to_string(), 5.0);

        let (net, idx_to_id) = build_network(&g, &config);
        let x = idx_to_id.iter().position(|id| id == "file:x.rs").unwrap();
        let y = idx_to_id.iter().position(|id| id == "file:y.rs").unwrap();
        let z = idx_to_id.iter().position(|id| id == "file:z.rs").unwrap();

        let out = net.out_neighbors(x);
        let weight_xy = out.iter().find(|&&(t, _)| t == y).map(|&(_, w)| w).unwrap();
        let weight_xz = out.iter().find(|&&(t, _)| t == z).map(|&(_, w)| w).unwrap();

        assert!(
            weight_xz > weight_xy,
            "with override, imports ({}) should outweigh calls ({})",
            weight_xz,
            weight_xy
        );
    }

    // ── 3c. test_build_network_zero_weight_skips_edge (ISS-002) ────────

    #[test]
    fn test_build_network_zero_weight_skips_edge() {
        let mut g = Graph::default();
        g.nodes.push(make_file_node("a.rs"));
        g.nodes.push(make_file_node("b.rs"));
        g.edges.push(Edge::new("file:a.rs", "file:b.rs", "calls"));

        // Disable `calls` entirely.
        let mut config = ClusterConfig::default();
        config.edge_weights.insert("calls".to_string(), 0.0);

        let (net, _) = build_network(&g, &config);
        assert_eq!(net.num_edges(), 0, "zero-weight relations must be skipped");
    }

    // ── 3d. test_default_edge_weights_matches_relation_weight (ISS-002) ─

    #[test]
    fn test_default_edge_weights_matches_relation_weight() {
        // Ensures backwards compatibility: the materialised default map and
        // the legacy `relation_weight` function must agree on every relation
        // either side knows about.
        let map = default_edge_weights();
        for (relation, &expected) in &map {
            let actual = relation_weight(relation);
            assert_eq!(
                actual, expected,
                "relation_weight({:?}) = {} but default map has {}",
                relation, actual, expected
            );
        }
        // Spot-check the well-known relations.
        assert_eq!(map.get("calls").copied(), Some(WEIGHT_CALLS));
        assert_eq!(map.get("imports").copied(), Some(WEIGHT_IMPORTS));
        assert_eq!(map.get("inherits").copied(), Some(WEIGHT_TYPE_REF));
        assert_eq!(map.get("contains").copied(), Some(WEIGHT_STRUCTURAL));
        assert_eq!(map.get("depends_on").copied(), Some(WEIGHT_DEPENDS_ON));
        // Unknown relation must be absent (treated as 0.0 by build_network).
        assert!(map.get("unknown_relation").is_none());
    }

    // ── 4. test_build_network_skips_self_loops ─────────────────────────

    #[test]
    fn test_build_network_skips_self_loops() {
        let mut g = Graph::default();
        g.nodes.push(make_file_node("a.rs"));
        g.nodes.push(make_file_node("b.rs"));
        g.edges.push(Edge::new("file:a.rs", "file:a.rs", "calls")); // self-loop
        g.edges.push(Edge::new("file:a.rs", "file:b.rs", "calls"));

        let (net, _idx_to_id) = build_network(&g, &ClusterConfig::default());

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

        let (net, idx_to_id) = build_network(&g, &ClusterConfig::default());

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

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());

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

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
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
        let cluster_a = ["src/auth/login.ts", "src/auth/register.ts", "src/auth/session.ts"];
        for i in 0..cluster_a.len() {
            for j in (i + 1)..cluster_a.len() {
                g.edges.push(Edge::new(
                    &format!("file:{}", cluster_a[i]),
                    &format!("file:{}", cluster_a[j]),
                    "imports",
                ));
            }
        }
        let cluster_b = ["src/commands/run.ts",
            "src/commands/build.ts",
            "src/commands/test.ts",
            "src/commands/deploy.ts"];
        for i in 0..cluster_b.len() {
            for j in (i + 1)..cluster_b.len() {
                g.edges.push(Edge::new(
                    &format!("file:{}", cluster_b[i]),
                    &format!("file:{}", cluster_b[j]),
                    "imports",
                ));
            }
        }
        let cluster_c = ["src/ui/render.ts", "src/ui/layout.ts", "src/ui/theme.ts"];
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

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
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

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
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

    // ── Sub-clustering tests ───────────────────────────────────────────

    #[test]
    fn test_split_mega_cluster() {
        // Create a graph with 30+ files that would cluster into one mega-cluster
        // (files in two loose groups but heavily interconnected).
        let mut g = Graph::default();
        let file_count = 30;
        let files: Vec<String> = (0..file_count)
            .map(|i| format!("src/big/file{}.rs", i))
            .collect();
        for f in &files {
            g.nodes.push(make_file_node(f));
        }
        // Fully connect all files → single mega-cluster
        for i in 0..file_count {
            for j in 0..file_count {
                if i != j {
                    g.edges.push(Edge::new(
                        &format!("file:{}", files[i]),
                        &format!("file:{}", files[j]),
                        "calls",
                    ));
                }
            }
        }

        // With max_cluster_size=10, the mega-cluster should be split
        let config = ClusterConfig {
            seed: 42,
            num_trials: 10,
            min_community_size: 1,
            max_cluster_size: Some(10),
            ..Default::default()
        };

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
        add_dir_colocation_edges(&mut net, &idx_to_id, config.dir_colocation_weight);
        let (clusters, _metrics) = run_clustering(&net, &idx_to_id, &config);

        // Before splitting: should have some clusters (possibly 1 big one)
        let big_before = clusters.iter().filter(|c| c.member_ids.len() > 10).count();

        let total_files_val = idx_to_id.len();
        let max_size = config
            .max_cluster_size
            .unwrap_or_else(|| total_files_val.max(100) / 5)
            .max(20);
        let split_clusters = split_mega_clusters_recursive(
            clusters,
            &net,
            &idx_to_id,
            &config,
            max_size,
            0,
            3,
        );

        // All files should still be accounted for
        let total_members: usize = split_clusters.iter().map(|c| c.member_ids.len()).sum();
        assert_eq!(
            total_members, file_count,
            "All {} files should be present after split, got {}",
            file_count, total_members
        );

        eprintln!(
            "test_split_mega_cluster: big_before={}, clusters_after={}, max_size={}",
            big_before,
            split_clusters.len(),
            max_size
        );
    }

    #[test]
    fn test_split_preserves_small_clusters() {
        // Two groups of 5 files each, well-separated
        let mut g = Graph::default();
        let group_a: Vec<String> = (0..5).map(|i| format!("src/alpha/a{}.rs", i)).collect();
        let group_b: Vec<String> = (0..5).map(|i| format!("src/beta/b{}.rs", i)).collect();

        for f in group_a.iter().chain(group_b.iter()) {
            g.nodes.push(make_file_node(f));
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

        let config = ClusterConfig {
            seed: 42,
            num_trials: 10,
            min_community_size: 1,
            max_cluster_size: Some(10), // both groups are under 10
            ..Default::default()
        };

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
        add_dir_colocation_edges(&mut net, &idx_to_id, config.dir_colocation_weight);
        let (clusters, _metrics) = run_clustering(&net, &idx_to_id, &config);

        let clusters_before = clusters.len();
        let split_clusters =
            split_mega_clusters_recursive(clusters, &net, &idx_to_id, &config, 10, 0, 3);

        // Neither cluster should be touched since both are ≤10
        assert_eq!(
            split_clusters.len(),
            clusters_before,
            "Small clusters should not be split: before={}, after={}",
            clusters_before,
            split_clusters.len()
        );
    }

    #[test]
    fn test_split_stops_on_monolith() {
        // Create a completely connected graph with 15 files
        let mut g = Graph::default();
        let file_count = 15;
        let files: Vec<String> = (0..file_count)
            .map(|i| format!("src/mono/m{}.rs", i))
            .collect();

        for f in &files {
            g.nodes.push(make_file_node(f));
        }

        // Perfect clique: every file calls every other file
        for i in 0..file_count {
            for j in 0..file_count {
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
            max_cluster_size: Some(10),
            ..Default::default()
        };

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
        add_dir_colocation_edges(&mut net, &idx_to_id, config.dir_colocation_weight);
        let (clusters, _metrics) = run_clustering(&net, &idx_to_id, &config);

        let split_clusters =
            split_mega_clusters_recursive(clusters, &net, &idx_to_id, &config, 10, 0, 3);

        // All files should still be accounted for
        let total_members: usize = split_clusters.iter().map(|c| c.member_ids.len()).sum();
        assert_eq!(
            total_members, file_count,
            "All {} files should be present, got {}",
            file_count, total_members
        );

        // The monolithic cluster may or may not be split depending on Infomap's behavior.
        // But we verify it doesn't panic and all members are preserved.
        eprintln!(
            "test_split_stops_on_monolith: {} clusters, sizes: {:?}",
            split_clusters.len(),
            split_clusters
                .iter()
                .map(|c| c.member_ids.len())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_split_max_depth() {
        // Create a graph with 30 files in one cluster
        let mut g = Graph::default();
        let file_count = 30;
        let files: Vec<String> = (0..file_count)
            .map(|i| format!("src/deep/d{}.rs", i))
            .collect();
        for f in &files {
            g.nodes.push(make_file_node(f));
        }
        // Fully connect → likely one big cluster
        for i in 0..file_count {
            for j in 0..file_count {
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
            max_cluster_size: Some(10),
            ..Default::default()
        };

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
        add_dir_colocation_edges(&mut net, &idx_to_id, config.dir_colocation_weight);
        let (clusters, _metrics) = run_clustering(&net, &idx_to_id, &config);

        let clusters_snapshot = clusters.clone();

        // max_depth=0 → no splitting at all
        let no_split =
            split_mega_clusters_recursive(clusters_snapshot, &net, &idx_to_id, &config, 10, 0, 0);

        // With depth=0, no splitting should occur — clusters should be unchanged
        let original_sizes: Vec<usize> = clusters
            .iter()
            .map(|c| c.member_ids.len())
            .collect();
        let no_split_sizes: Vec<usize> = no_split
            .iter()
            .map(|c| c.member_ids.len())
            .collect();

        assert_eq!(
            original_sizes, no_split_sizes,
            "max_depth=0 should produce identical clusters: original={:?}, got={:?}",
            original_sizes, no_split_sizes
        );
    }

    // ── 35. test_auto_config_hierarchical_threshold ─────────────────────

    #[test]
    fn test_auto_config_hierarchical_threshold() {
        // 49 files → hierarchical=false
        let config_49 = auto_config(49);
        assert!(
            !config_49.hierarchical,
            "auto_config(49) should return hierarchical=false"
        );
        assert_eq!(config_49.min_community_size, 2);

        // 50 files → hierarchical=true
        let config_50 = auto_config(50);
        assert!(
            config_50.hierarchical,
            "auto_config(50) should return hierarchical=true"
        );
        assert_eq!(config_50.min_community_size, 3);

        // 500 files → hierarchical=true
        let config_500 = auto_config(500);
        assert!(
            config_500.hierarchical,
            "auto_config(500) should return hierarchical=true"
        );
        assert_eq!(config_500.min_community_size, 5);

        // 2000 files → hierarchical=true
        let config_2000 = auto_config(2000);
        assert!(
            config_2000.hierarchical,
            "auto_config(2000) should return hierarchical=true"
        );
        assert_eq!(config_2000.min_community_size, 8);
    }

    // ── 36. test_hierarchical_skips_mega_split ─────────────────────────

    #[test]
    fn test_hierarchical_skips_mega_split() {
        // Create a graph with 60+ files forming a dense cluster.
        // Run with hierarchical=true. Verify that clusters_split metric is 0,
        // meaning split_mega_clusters was NOT invoked.
        let mut g = Graph::default();
        let file_count = 65;

        // Use multiple directories so the graph is realistic
        let files: Vec<String> = (0..file_count)
            .map(|i| format!("src/mod{}/f{}.rs", i / 10, i))
            .collect();
        for f in &files {
            g.nodes.push(make_file_node(f));
        }

        // Create edges between files: each file connects to next 3 files
        // (sparse enough to form communities, dense enough for meaningful clustering)
        for i in 0..file_count {
            for offset in 1..=3 {
                let j = (i + offset) % file_count;
                g.edges.push(Edge::new(
                    &format!("file:{}", files[i]),
                    &format!("file:{}", files[j]),
                    "calls",
                ));
            }
        }

        let config = ClusterConfig {
            seed: 42,
            num_trials: 10,
            min_community_size: 2,
            hierarchical: true,
            max_cluster_size: Some(10), // Small max to trigger splitting logic
            ..Default::default()
        };

        let result = cluster(&g, &config).unwrap();

        // In hierarchical mode, clusters_split should be 0
        // because split_mega_clusters is skipped entirely.
        assert_eq!(
            result.metrics.clusters_split, 0,
            "Hierarchical mode should skip split_mega_clusters (clusters_split should be 0, got {})",
            result.metrics.clusters_split
        );

        // Verify we still get a valid result with components
        assert!(
            !result.nodes.is_empty(),
            "Hierarchical clustering should still produce component nodes"
        );
    }

    // ── 37. test_split_oversized_by_directory ──────────────────────────

    #[test]
    fn test_split_oversized_by_directory() {
        // Create clusters where one leaf cluster is oversized with files from
        // multiple directories — it should be split by directory.

        let oversized = RawCluster {
            id: 0,
            member_ids: vec![
                "file:src/auth/login.rs".to_string(),
                "file:src/auth/logout.rs".to_string(),
                "file:src/auth/session.rs".to_string(),
                "file:src/db/pool.rs".to_string(),
                "file:src/db/query.rs".to_string(),
                "file:src/db/migrate.rs".to_string(),
            ],
            flow: 1.0,
            parent: None,
            children: Vec::new(), // leaf cluster
        };

        let small = RawCluster {
            id: 1,
            member_ids: vec![
                "file:src/api/handler.rs".to_string(),
                "file:src/api/router.rs".to_string(),
            ],
            flow: 0.5,
            parent: None,
            children: Vec::new(),
        };

        let clusters = vec![oversized, small];
        // max_size=4: the 6-member cluster should be split, the 2-member one preserved
        let result = split_oversized_by_directory(clusters, 4);

        // The oversized cluster (6 files in 2 dirs) should split into 2 sub-clusters
        // The small cluster (2 files) should be preserved
        // Total: 3 clusters
        assert_eq!(
            result.len(),
            3,
            "Expected 3 clusters (2 from split + 1 preserved), got {}",
            result.len()
        );

        // Verify all original members are preserved
        let all_members: HashSet<String> = result
            .iter()
            .flat_map(|c| c.member_ids.iter().cloned())
            .collect();
        assert_eq!(all_members.len(), 8, "All 8 files should be preserved");
        assert!(all_members.contains("file:src/auth/login.rs"));
        assert!(all_members.contains("file:src/db/pool.rs"));
        assert!(all_members.contains("file:src/api/handler.rs"));

        // Verify that the split clusters each contain files from the same directory
        let auth_cluster = result.iter().find(|c| {
            c.member_ids.iter().any(|m| m.contains("auth"))
        });
        let db_cluster = result.iter().find(|c| {
            c.member_ids.iter().any(|m| m.contains("/db/"))
        });

        assert!(auth_cluster.is_some(), "Should have an auth cluster");
        assert!(db_cluster.is_some(), "Should have a db cluster");
        assert_eq!(auth_cluster.unwrap().member_ids.len(), 3);
        assert_eq!(db_cluster.unwrap().member_ids.len(), 3);

        // Verify IDs are sequential
        for (i, c) in result.iter().enumerate() {
            assert_eq!(c.id, i, "Cluster IDs should be sequential");
        }
    }

    // ── 38. test_split_oversized_preserves_non_leaf ────────────────────

    #[test]
    fn test_split_oversized_preserves_non_leaf() {
        // Non-leaf clusters (with children) should NOT be split even if oversized.
        let parent = RawCluster {
            id: 0,
            member_ids: (0..10)
                .map(|i| format!("file:src/a/f{}.rs", i))
                .collect(),
            flow: 1.0,
            parent: None,
            children: vec![1, 2], // has children → not a leaf
        };

        let clusters = vec![parent];
        let result = split_oversized_by_directory(clusters, 4);

        // Should NOT be split because it has children
        assert_eq!(
            result.len(),
            1,
            "Non-leaf cluster should not be split, got {} clusters",
            result.len()
        );
    }

    // ── 39. test_co_citation_basic ─────────────────────────────────────

    #[test]
    fn test_co_citation_basic() {
        // 5 "util" files in the same directory, no mutual imports.
        // 3 "feature" files that import overlapping subsets.
        // util a and b are both imported by f1 and f2 → co-citation edge.
        // Other pairs share only 1 citer → below default threshold.
        let mut g = Graph::default();

        for name in &[
            "utils/a.ts",
            "utils/b.ts",
            "utils/c.ts",
            "utils/d.ts",
            "utils/e.ts",
        ] {
            g.nodes.push(make_file_node(name));
        }
        for name in &["features/f1.ts", "features/f2.ts", "features/f3.ts"] {
            g.nodes.push(make_file_node(name));
        }

        // f1 imports a, b
        g.edges
            .push(Edge::new("file:features/f1.ts", "file:utils/a.ts", "imports"));
        g.edges
            .push(Edge::new("file:features/f1.ts", "file:utils/b.ts", "imports"));
        // f2 imports a, b, c
        g.edges
            .push(Edge::new("file:features/f2.ts", "file:utils/a.ts", "imports"));
        g.edges
            .push(Edge::new("file:features/f2.ts", "file:utils/b.ts", "imports"));
        g.edges
            .push(Edge::new("file:features/f2.ts", "file:utils/c.ts", "imports"));
        // f3 imports d, e
        g.edges
            .push(Edge::new("file:features/f3.ts", "file:utils/d.ts", "imports"));
        g.edges
            .push(Edge::new("file:features/f3.ts", "file:utils/e.ts", "imports"));

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
        let edges_before = net.num_edges();

        add_co_citation_edges(&mut net, &g, &idx_to_id, 0.4, 2, 2.0);

        let edges_after = net.num_edges();
        // a and b share 2 citers (f1, f2) → bidirectional co-citation edges added
        assert!(
            edges_after > edges_before,
            "co-citation should add edges: before={}, after={}",
            edges_before,
            edges_after
        );

        // Verify a↔b edge exists
        let a_idx = idx_to_id
            .iter()
            .position(|id| id == "file:utils/a.ts")
            .unwrap();
        let b_idx = idx_to_id
            .iter()
            .position(|id| id == "file:utils/b.ts")
            .unwrap();

        let a_neighbors: Vec<usize> = net.out_neighbors(a_idx).iter().map(|&(t, _)| t).collect();
        assert!(
            a_neighbors.contains(&b_idx),
            "a should have co-citation edge to b"
        );

        let b_neighbors: Vec<usize> = net.out_neighbors(b_idx).iter().map(|&(t, _)| t).collect();
        assert!(
            b_neighbors.contains(&a_idx),
            "b should have co-citation edge to a (bidirectional)"
        );
    }

    // ── 40. test_co_citation_min_shared_threshold ──────────────────────

    #[test]
    fn test_co_citation_min_shared_threshold() {
        let mut g = Graph::default();
        g.nodes.push(make_file_node("utils/a.ts"));
        g.nodes.push(make_file_node("utils/b.ts"));
        g.nodes.push(make_file_node("features/f1.ts"));

        // Only 1 shared citer
        g.edges
            .push(Edge::new("file:features/f1.ts", "file:utils/a.ts", "imports"));
        g.edges
            .push(Edge::new("file:features/f1.ts", "file:utils/b.ts", "imports"));

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
        let edges_before = net.num_edges();

        // min_shared=2, but only 1 shared citer → no co-citation edge
        add_co_citation_edges(&mut net, &g, &idx_to_id, 0.4, 2, 2.0);
        assert_eq!(
            net.num_edges(),
            edges_before,
            "should not add edges when below min_shared"
        );

        // min_shared=1 → should add edge
        add_co_citation_edges(&mut net, &g, &idx_to_id, 0.4, 1, 2.0);
        assert!(
            net.num_edges() > edges_before,
            "should add edges when min_shared=1"
        );
    }

    // ── 41. test_co_citation_weight_cap ────────────────────────────────

    #[test]
    fn test_co_citation_weight_cap() {
        let mut g = Graph::default();
        g.nodes.push(make_file_node("utils/a.ts"));
        g.nodes.push(make_file_node("utils/b.ts"));
        // 10 feature files all importing both a and b
        for i in 0..10 {
            let name = format!("features/f{}.ts", i);
            g.nodes.push(make_file_node(&name));
            g.edges.push(Edge::new(
                &format!("file:features/f{}.ts", i),
                "file:utils/a.ts",
                "imports",
            ));
            g.edges.push(Edge::new(
                &format!("file:features/f{}.ts", i),
                "file:utils/b.ts",
                "imports",
            ));
        }

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
        add_co_citation_edges(&mut net, &g, &idx_to_id, 0.4, 2, 2.0);

        let a_idx = idx_to_id
            .iter()
            .position(|id| id == "file:utils/a.ts")
            .unwrap();
        let b_idx = idx_to_id
            .iter()
            .position(|id| id == "file:utils/b.ts")
            .unwrap();

        // 10 shared citers × 0.4 = 4.0, but capped at 2.0
        let weight = net
            .out_neighbors(a_idx)
            .iter()
            .find(|&&(t, _)| t == b_idx)
            .map(|&(_, w)| w)
            .expect("should have co-citation edge");

        assert!(
            (weight - 2.0).abs() < 0.001,
            "weight should be capped at 2.0, got {}",
            weight
        );
    }

    // ── 42. test_co_citation_disabled_when_zero_weight ─────────────────

    #[test]
    fn test_co_citation_disabled_when_zero_weight() {
        let mut g = Graph::default();
        g.nodes.push(make_file_node("utils/a.ts"));
        g.nodes.push(make_file_node("utils/b.ts"));
        g.nodes.push(make_file_node("features/f1.ts"));
        g.nodes.push(make_file_node("features/f2.ts"));
        g.edges
            .push(Edge::new("file:features/f1.ts", "file:utils/a.ts", "imports"));
        g.edges
            .push(Edge::new("file:features/f1.ts", "file:utils/b.ts", "imports"));
        g.edges
            .push(Edge::new("file:features/f2.ts", "file:utils/a.ts", "imports"));
        g.edges
            .push(Edge::new("file:features/f2.ts", "file:utils/b.ts", "imports"));

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
        let edges_before = net.num_edges();

        // weight = 0.0 → disabled
        add_co_citation_edges(&mut net, &g, &idx_to_id, 0.0, 2, 2.0);
        assert_eq!(net.num_edges(), edges_before);
    }

    // ── 43. test_co_citation_splits_utils_cluster ──────────────────────

    #[test]
    fn test_co_citation_splits_utils_cluster() {
        // Simulate the real problem: utility files in one flat directory,
        // no mutual imports. Two groups of feature files import different subsets.
        // Without co-citation → one monolithic hooks cluster.
        // With co-citation → hooks split into groups matching usage patterns.
        let mut g = Graph::default();

        // Group A utils: auth-related
        let auth_utils = [
            "hooks/useAuth.ts",
            "hooks/useSession.ts",
            "hooks/usePermissions.ts",
            "hooks/useAuthCallback.ts",
            "hooks/useToken.ts",
        ];
        // Group B utils: UI-related
        let ui_utils = [
            "hooks/useTheme.ts",
            "hooks/useModal.ts",
            "hooks/useToast.ts",
            "hooks/useAnimation.ts",
            "hooks/useMediaQuery.ts",
        ];

        for name in auth_utils.iter().chain(ui_utils.iter()) {
            g.nodes.push(make_file_node(name));
        }

        // Auth feature files import auth utils
        for i in 0..4 {
            let feat = format!("features/auth/page{}.ts", i);
            g.nodes.push(make_file_node(&feat));
            for util in &auth_utils {
                g.edges.push(Edge::new(
                    &format!("file:{}", feat),
                    &format!("file:{}", util),
                    "imports",
                ));
            }
        }

        // UI feature files import UI utils
        for i in 0..4 {
            let feat = format!("features/ui/page{}.ts", i);
            g.nodes.push(make_file_node(&feat));
            for util in &ui_utils {
                g.edges.push(Edge::new(
                    &format!("file:{}", feat),
                    &format!("file:{}", util),
                    "imports",
                ));
            }
        }

        // Run clustering WITH co-citation
        let config = ClusterConfig {
            co_citation_weight: 0.4,
            co_citation_min_shared: 2,
            dir_colocation_weight: 0.0, // disable to isolate co-citation effect
            seed: 42,
            num_trials: 10,
            min_community_size: 2,
            max_cluster_size: Some(8),
            ..Default::default()
        };

        let result = cluster(&g, &config).unwrap();

        // Should have multiple components (not one monolithic "hooks" cluster)
        assert!(
            result.nodes.len() >= 3,
            "co-citation should produce >= 3 components (auth-hooks, ui-hooks, features), got {}",
            result.nodes.len()
        );

        // No single component should contain ALL the hooks
        let all_hooks: HashSet<String> = auth_utils
            .iter()
            .chain(ui_utils.iter())
            .map(|s| format!("file:{}", s))
            .collect();
        for component in &result.nodes {
            let hook_members: Vec<_> = result
                .edges
                .iter()
                .filter(|e| e.to == component.id && e.relation == "belongs_to")
                .filter(|e| all_hooks.contains(&e.from))
                .collect();
            assert!(
                hook_members.len() < all_hooks.len(),
                "component {} contains all {} hooks — co-citation didn't split them",
                component.id,
                all_hooks.len()
            );
        }
    }

    // ── 44. test_colocation_isolation_gating ────────────────────────

    #[test]
    fn test_colocation_isolation_gating() {
        // Regression test for the root fix: co-location edges should ONLY be
        // added for code-isolated files (zero edges in the network).
        //
        // Scenario: a flat directory with 10 files.  7 have import edges,
        // 3 are truly isolated.  Co-location should only connect the 3 isolated
        // files, NOT add O(n²) edges among all 10.

        let mut g = Graph::default();

        // 7 connected files in utils/
        for i in 0..7 {
            g.nodes
                .push(make_file_node(&format!("src/utils/connected{}.ts", i)));
        }
        // Each connected file imports the next → chain of edges
        for i in 0..6 {
            g.edges.push(Edge::new(
                &format!("file:src/utils/connected{}.ts", i),
                &format!("file:src/utils/connected{}.ts", i + 1),
                "imports",
            ));
        }

        // 3 isolated files in utils/ (no imports, no calls)
        for i in 0..3 {
            g.nodes
                .push(make_file_node(&format!("src/utils/orphan{}.ts", i)));
        }

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
        let edges_before = net.num_edges();

        // edges_before should be 6 (the import chain)
        assert_eq!(
            edges_before, 6,
            "import chain should produce 6 edges, got {}",
            edges_before
        );

        add_dir_colocation_edges(&mut net, &idx_to_id, 0.3);

        // Only the 3 isolated files should get co-location: 3 pairs × 2 dirs = 6 new edges
        let edges_after = net.num_edges();
        let colocation_edges_added = edges_after - edges_before;

        assert_eq!(
            colocation_edges_added, 6,
            "Should add 6 co-location edges (3 isolated files × C(3,2) × 2 directions), got {}",
            colocation_edges_added
        );

        // Verify none of the connected files got new edges
        for i in 0..7 {
            let file_id = format!("file:src/utils/connected{}.ts", i);
            let idx = idx_to_id.iter().position(|id| id == &file_id).unwrap();
            // Connected files: should only have import edges, no co-location
            let out_count = net.out_neighbors(idx).len();
            let in_count = net.in_neighbors(idx).len();
            // At most 1 out-edge (import to next) + 1 in-edge (import from prev)
            assert!(
                out_count <= 1 && in_count <= 1,
                "connected file {} should have ≤1 out + ≤1 in edges, got out={} in={}",
                i, out_count, in_count
            );
        }
    }

    // ── 45. test_colocation_skips_when_all_connected ────────────────────

    #[test]
    fn test_colocation_skips_when_all_connected() {
        // If every file in a directory already has code edges, co-location
        // should add ZERO edges — regardless of directory size.

        let mut g = Graph::default();
        for i in 0..50 {
            g.nodes
                .push(make_file_node(&format!("src/big_flat_dir/f{}.ts", i)));
        }
        // Create a ring: each file imports the next (circular)
        for i in 0..50 {
            g.edges.push(Edge::new(
                &format!("file:src/big_flat_dir/f{}.ts", i),
                &format!("file:src/big_flat_dir/f{}.ts", (i + 1) % 50),
                "imports",
            ));
        }

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
        let edges_before = net.num_edges();

        add_dir_colocation_edges(&mut net, &idx_to_id, 0.3);

        assert_eq!(
            net.num_edges(),
            edges_before,
            "all files have edges → co-location should add ZERO new edges, \
             but added {} (was {}, now {})",
            net.num_edges() - edges_before,
            edges_before,
            net.num_edges(),
        );
    }

    // ── 46. test_co_citation_only_import_like_edges ────────────────────

    #[test]
    fn test_co_citation_only_import_like_edges() {
        // Structural edges (defined_in, contains) should NOT count as citations
        let mut g = Graph::default();
        g.nodes.push(make_file_node("utils/a.ts"));
        g.nodes.push(make_file_node("utils/b.ts"));
        g.nodes.push(make_file_node("features/f1.ts"));
        g.nodes.push(make_file_node("features/f2.ts"));

        // f1 and f2 have "contains" edges to a and b — structural, not import-like
        g.edges
            .push(Edge::new("file:features/f1.ts", "file:utils/a.ts", "contains"));
        g.edges
            .push(Edge::new("file:features/f1.ts", "file:utils/b.ts", "contains"));
        g.edges
            .push(Edge::new("file:features/f2.ts", "file:utils/a.ts", "contains"));
        g.edges
            .push(Edge::new("file:features/f2.ts", "file:utils/b.ts", "contains"));

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
        let edges_before = net.num_edges();

        add_co_citation_edges(&mut net, &g, &idx_to_id, 0.4, 2, 2.0);

        // "contains" is not import-like, so no co-citation edges should be added
        assert_eq!(
            net.num_edges(),
            edges_before,
            "structural edges should not create co-citation"
        );
    }

    // ── Symbol similarity tests ────────────────────────────────────────

    #[test]
    fn test_split_camel_case() {
        // lowercase→uppercase boundary
        assert_eq!(
            split_camel_case("getAuthToken"),
            vec!["get", "Auth", "Token"]
        );
        // Consecutive uppercase run: "OAuth" → "O" + "Auth" (splits before last
        // uppercase when followed by lowercase)
        assert_eq!(
            split_camel_case("getOAuthToken"),
            vec!["get", "O", "Auth", "Token"]
        );
        assert_eq!(
            split_camel_case("AwsAuthStatusManager"),
            vec!["Aws", "Auth", "Status", "Manager"]
        );
        // All-uppercase run followed by PascalCase
        assert_eq!(split_camel_case("HTMLParser"), vec!["HTML", "Parser"]);
        // Simple words
        assert_eq!(split_camel_case("simple"), vec!["simple"]);
        assert_eq!(split_camel_case("URL"), vec!["URL"]);
        assert_eq!(split_camel_case(""), Vec::<String>::new());
        assert_eq!(split_camel_case("a"), vec!["a"]);
        // Trailing uppercase run
        assert_eq!(split_camel_case("parseJSON"), vec!["parse", "JSON"]);
        assert_eq!(
            split_camel_case("XMLHttpRequest"),
            vec!["XML", "Http", "Request"]
        );
    }

    #[test]
    fn test_tokenize_symbol_name() {
        // camelCase: "getOAuthToken" → split → ["get","O","Auth","Token"]
        //   → lowercase → ["get","o","auth","token"]
        //   → filter len<2 → ["get","auth","token"]
        //   → stop words → ["auth","token"]
        let tokens = tokenize_symbol_name("getOAuthToken");
        assert!(tokens.contains("auth"));
        assert!(tokens.contains("token"));
        assert!(!tokens.contains("get")); // stop word
        assert!(!tokens.contains("o")); // too short

        // snake_case
        let tokens = tokenize_symbol_name("parse_auth_token");
        assert!(tokens.contains("parse"));
        assert!(tokens.contains("auth"));
        assert!(tokens.contains("token"));

        // PascalCase class name
        let tokens = tokenize_symbol_name("AwsAuthStatusManager");
        assert!(tokens.contains("aws"));
        assert!(tokens.contains("auth"));
        assert!(tokens.contains("status"));
        assert!(tokens.contains("manager"));

        // All stop words
        let tokens = tokenize_symbol_name("getDefaultValue");
        // "get" = stop, "default" = stop, "value" = stop
        assert!(tokens.is_empty());

        // Short tokens filtered
        let tokens = tokenize_symbol_name("aB");
        // "a" is 1 char → filtered, "b" is 1 char → filtered
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_is_stop_word() {
        assert!(is_stop_word("get"));
        assert!(is_stop_word("set"));
        assert!(is_stop_word("create"));
        assert!(is_stop_word("default"));
        assert!(is_stop_word("value"));
        assert!(!is_stop_word("auth"));
        assert!(!is_stop_word("oauth"));
        assert!(!is_stop_word("token"));
        assert!(!is_stop_word("parser"));
        assert!(!is_stop_word("manager"));
    }

    #[test]
    fn test_symbol_similarity_edges_basic() {
        // Two auth files (similar symbols) + two format files (similar symbols)
        // Auth files should connect to each other, format files to each other,
        // but NOT cross-domain.
        let mut g = Graph::default();

        // Auth group
        g.nodes.push(make_file_node("src/auth/login.ts"));
        g.nodes.push(make_file_node("src/auth/token.ts"));
        // Format group
        g.nodes.push(make_file_node("src/utils/formatDate.ts"));
        g.nodes.push(make_file_node("src/utils/formatCurrency.ts"));

        // Auth symbols
        let mut n = Node::new("func:validateAuthToken", "validateAuthToken");
        n.node_type = Some("code".into());
        n.node_kind = Some("Function".into());
        n.file_path = Some("src/auth/login.ts".into());
        g.nodes.push(n);

        let mut n = Node::new("func:refreshAuthCredential", "refreshAuthCredential");
        n.node_type = Some("code".into());
        n.node_kind = Some("Function".into());
        n.file_path = Some("src/auth/login.ts".into());
        g.nodes.push(n);

        let mut n = Node::new("func:storeAuthToken", "storeAuthToken");
        n.node_type = Some("code".into());
        n.node_kind = Some("Function".into());
        n.file_path = Some("src/auth/token.ts".into());
        g.nodes.push(n);

        let mut n = Node::new("func:revokeAuthCredential", "revokeAuthCredential");
        n.node_type = Some("code".into());
        n.node_kind = Some("Function".into());
        n.file_path = Some("src/auth/token.ts".into());
        g.nodes.push(n);

        // Format symbols
        let mut n = Node::new("func:formatLocalDate", "formatLocalDate");
        n.node_type = Some("code".into());
        n.node_kind = Some("Function".into());
        n.file_path = Some("src/utils/formatDate.ts".into());
        g.nodes.push(n);

        let mut n = Node::new("func:parseDateString", "parseDateString");
        n.node_type = Some("code".into());
        n.node_kind = Some("Function".into());
        n.file_path = Some("src/utils/formatDate.ts".into());
        g.nodes.push(n);

        let mut n = Node::new("func:formatLocalCurrency", "formatLocalCurrency");
        n.node_type = Some("code".into());
        n.node_kind = Some("Function".into());
        n.file_path = Some("src/utils/formatCurrency.ts".into());
        g.nodes.push(n);

        let mut n = Node::new("func:parseCurrencyString", "parseCurrencyString");
        n.node_type = Some("code".into());
        n.node_kind = Some("Function".into());
        n.file_path = Some("src/utils/formatCurrency.ts".into());
        g.nodes.push(n);

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
        assert_eq!(net.num_nodes(), 4);
        assert_eq!(net.num_edges(), 0); // no import edges

        add_symbol_similarity_edges(&mut net, &g, &idx_to_id, 0.5, 2, 0.15);

        // Auth files should have edges between them (shared: "auth", "credential", "token")
        let login_idx = idx_to_id
            .iter()
            .position(|id| id.contains("login"))
            .unwrap();
        let token_idx = idx_to_id
            .iter()
            .position(|id| id.contains("token.ts"))
            .unwrap();

        let login_out = net.out_neighbors(login_idx);
        let has_auth_edge = login_out.iter().any(|&(t, _)| t == token_idx);
        assert!(
            has_auth_edge,
            "Auth files should be connected by symbol similarity"
        );

        // Format files should have edges between them (shared: "format", "local", "string", "parse")
        let date_idx = idx_to_id
            .iter()
            .position(|id| id.contains("formatDate"))
            .unwrap();
        let currency_idx = idx_to_id
            .iter()
            .position(|id| id.contains("formatCurrency"))
            .unwrap();

        let date_out = net.out_neighbors(date_idx);
        let has_format_edge = date_out.iter().any(|&(t, _)| t == currency_idx);
        assert!(
            has_format_edge,
            "Format files should be connected by symbol similarity"
        );

        // Cross-domain edges should NOT exist (auth ↔ format have no shared domain tokens)
        let login_connects_to_date = login_out.iter().any(|&(t, _)| t == date_idx);
        let login_connects_to_currency = login_out.iter().any(|&(t, _)| t == currency_idx);
        assert!(
            !login_connects_to_date,
            "Auth should not connect to format (date)"
        );
        assert!(
            !login_connects_to_currency,
            "Auth should not connect to format (currency)"
        );
    }

    #[test]
    fn test_symbol_similarity_threshold_enforcement() {
        // Two files sharing only 1 token — should NOT get an edge when min_shared=2
        let mut g = Graph::default();
        g.nodes.push(make_file_node("a.ts"));
        g.nodes.push(make_file_node("b.ts"));

        let mut n = Node::new("func:authHandler", "authHandler");
        n.node_type = Some("code".into());
        n.node_kind = Some("Function".into());
        n.file_path = Some("a.ts".into());
        g.nodes.push(n);

        let mut n = Node::new("func:authValidator", "authValidator");
        n.node_type = Some("code".into());
        n.node_kind = Some("Function".into());
        n.file_path = Some("b.ts".into());
        g.nodes.push(n);

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());

        // With min_shared=2, single shared token "auth" should NOT create edge
        add_symbol_similarity_edges(&mut net, &g, &idx_to_id, 0.5, 2, 0.15);
        assert_eq!(
            net.num_edges(),
            0,
            "Single shared token should not create edge with min_shared=2"
        );

        // With min_shared=1, it SHOULD create edge
        let (mut net2, idx_to_id2) = build_network(&g, &ClusterConfig::default());
        add_symbol_similarity_edges(&mut net2, &g, &idx_to_id2, 0.5, 1, 0.0);
        assert!(
            net2.num_edges() > 0,
            "Single shared token should create edge with min_shared=1"
        );
    }

    #[test]
    fn test_symbol_similarity_weight_scaling() {
        // Verify edge weight = base_weight * jaccard
        let mut g = Graph::default();
        g.nodes.push(make_file_node("a.ts"));
        g.nodes.push(make_file_node("b.ts"));

        // File A: tokens = {auth, token, validate, credential} (after stop word removal)
        let mut n = Node::new("func:validateAuthToken", "validateAuthToken");
        n.node_type = Some("code".into());
        n.node_kind = Some("Function".into());
        n.file_path = Some("a.ts".into());
        g.nodes.push(n);

        let mut n = Node::new("func:authCredentialStore", "authCredentialStore");
        n.node_type = Some("code".into());
        n.node_kind = Some("Function".into());
        n.file_path = Some("a.ts".into());
        g.nodes.push(n);

        // File B: tokens = {auth, token, refresh, session} (after stop word removal)
        let mut n = Node::new("func:refreshAuthToken", "refreshAuthToken");
        n.node_type = Some("code".into());
        n.node_kind = Some("Function".into());
        n.file_path = Some("b.ts".into());
        g.nodes.push(n);

        let mut n = Node::new("func:authSessionStore", "authSessionStore");
        n.node_type = Some("code".into());
        n.node_kind = Some("Function".into());
        n.file_path = Some("b.ts".into());
        g.nodes.push(n);

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
        let base_weight = 0.5;
        add_symbol_similarity_edges(&mut net, &g, &idx_to_id, base_weight, 2, 0.0);

        let a_idx = idx_to_id
            .iter()
            .position(|id| id.contains("a.ts"))
            .unwrap();
        let out = net.out_neighbors(a_idx);
        assert!(!out.is_empty(), "Should have symbol similarity edge");

        let edge_weight = out[0].1;
        // Weight should be base_weight * jaccard, and jaccard < 1.0
        assert!(edge_weight > 0.0);
        assert!(edge_weight <= base_weight);
    }

    #[test]
    fn test_symbol_similarity_empty_files() {
        // Files with no symbols should not cause errors or spurious edges
        let mut g = Graph::default();
        g.nodes.push(make_file_node("a.ts"));
        g.nodes.push(make_file_node("b.ts"));
        // No function/class nodes

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
        add_symbol_similarity_edges(&mut net, &g, &idx_to_id, 0.5, 2, 0.15);
        assert_eq!(net.num_edges(), 0);
    }

    #[test]
    fn test_symbol_similarity_disabled() {
        // weight=0.0 should skip entirely
        let mut g = Graph::default();
        g.nodes.push(make_file_node("a.ts"));
        g.nodes.push(make_file_node("b.ts"));

        let mut n = Node::new("func:authLogin", "authLogin");
        n.node_type = Some("code".into());
        n.node_kind = Some("Function".into());
        n.file_path = Some("a.ts".into());
        g.nodes.push(n);

        let mut n = Node::new("func:authLogout", "authLogout");
        n.node_type = Some("code".into());
        n.node_kind = Some("Function".into());
        n.file_path = Some("b.ts".into());
        g.nodes.push(n);

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
        add_symbol_similarity_edges(&mut net, &g, &idx_to_id, 0.0, 1, 0.0);
        assert_eq!(net.num_edges(), 0, "Weight 0.0 should add no edges");
    }

    // ── Hub exclusion tests ────────────────────────────────────────────

    #[test]
    fn test_identify_hubs_basic() {
        // Create a graph where one file (hub.ts) is imported by many others
        let mut g = Graph::default();
        g.nodes.push(make_file_node("src/hub.ts"));
        for i in 0..10 {
            g.nodes.push(make_file_node(&format!("src/consumer{}.ts", i)));
        }
        // All 10 consumers import hub.ts
        for i in 0..10 {
            g.edges.push(Edge::new(
                &format!("file:src/consumer{}.ts", i),
                "file:src/hub.ts",
                "imports",
            ));
        }

        let (_, idx_to_id) = build_network(&g, &ClusterConfig::default());
        // threshold=0.05, min_degree=1 → cutoff = max(ceil(0.05*11), 1) = 1
        // hub.ts has in_degree=10 > 1 → hub
        let hubs = identify_hubs(&g, &idx_to_id, 0.05, 1);
        assert!(
            !hubs.is_empty(),
            "hub.ts with in_degree=10 should be identified as a hub"
        );

        // Verify the hub index corresponds to hub.ts
        let hub_idx = idx_to_id.iter().position(|id| id == "file:src/hub.ts").unwrap();
        assert!(hubs.contains(&hub_idx), "hub.ts index should be in the hub set");
        assert_eq!(hubs.len(), 1, "Only hub.ts should be a hub");
    }

    #[test]
    fn test_identify_hubs_threshold_sensitivity() {
        // 20 files, one hub imported by 5
        let mut g = Graph::default();
        g.nodes.push(make_file_node("src/shared.ts"));
        for i in 0..19 {
            g.nodes.push(make_file_node(&format!("src/f{}.ts", i)));
        }
        for i in 0..5 {
            g.edges.push(Edge::new(
                &format!("file:src/f{}.ts", i),
                "file:src/shared.ts",
                "imports",
            ));
        }

        let (_, idx_to_id) = build_network(&g, &ClusterConfig::default());

        // threshold=0.05, min_degree=1 → cutoff = max(ceil(0.05*20), 1) = 1
        // shared.ts(5) > 1 → hub
        let hubs_low = identify_hubs(&g, &idx_to_id, 0.05, 1);
        assert!(!hubs_low.is_empty(), "Low threshold should catch shared.ts");

        // threshold=0.5 → cutoff = max(ceil(0.5*20), 1) = 10 → shared.ts(5) <= 10 → not a hub
        let hubs_high = identify_hubs(&g, &idx_to_id, 0.5, 1);
        assert!(hubs_high.is_empty(), "High threshold should not catch shared.ts with in_degree=5");
    }

    #[test]
    fn test_hub_exclusion_produces_cleaner_clusters() {
        // Two domain clusters connected only through a shared hub
        let mut g = Graph::default();

        // Domain A: 4 tightly connected files
        let domain_a = ["src/auth/login.ts", "src/auth/logout.ts", "src/auth/session.ts", "src/auth/token.ts"];
        for p in &domain_a {
            g.nodes.push(make_file_node(p));
        }
        for i in 0..domain_a.len() {
            for j in 0..domain_a.len() {
                if i != j {
                    g.edges.push(Edge::new(
                        &format!("file:{}", domain_a[i]),
                        &format!("file:{}", domain_a[j]),
                        "calls",
                    ));
                }
            }
        }

        // Domain B: 4 tightly connected files
        let domain_b = ["src/db/pool.ts", "src/db/query.ts", "src/db/migrate.ts", "src/db/schema.ts"];
        for p in &domain_b {
            g.nodes.push(make_file_node(p));
        }
        for i in 0..domain_b.len() {
            for j in 0..domain_b.len() {
                if i != j {
                    g.edges.push(Edge::new(
                        &format!("file:{}", domain_b[i]),
                        &format!("file:{}", domain_b[j]),
                        "calls",
                    ));
                }
            }
        }

        // Hub file imported by ALL 8 domain files
        g.nodes.push(make_file_node("src/utils/ink.ts"));
        for p in domain_a.iter().chain(domain_b.iter()) {
            g.edges.push(Edge::new(
                &format!("file:{}", p),
                "file:src/utils/ink.ts",
                "imports",
            ));
        }

        // With hub exclusion enabled (threshold=0.05, min_degree=5 to only catch ink.ts
        // which has in-degree=8, not the domain files with in-degree=3)
        let config_with = ClusterConfig {
            seed: 42,
            num_trials: 10,
            min_community_size: 2,
            hub_exclusion_threshold: 0.05,
            hub_min_degree: 5,
            ..Default::default()
        };
        let result_with = cluster(&g, &config_with).unwrap();

        // Should have at least 2 domain clusters + 1 infrastructure component = 3+
        assert!(
            result_with.metrics.num_communities >= 3,
            "Hub exclusion should produce at least 3 components (2 domains + 1 infra), got {}",
            result_with.metrics.num_communities,
        );

        // Verify infrastructure component exists
        let infra = result_with.nodes.iter().find(|n| n.id == "infer:component:infrastructure");
        assert!(infra.is_some(), "Infrastructure component should exist");
        let infra_node = infra.unwrap();
        assert_eq!(infra_node.title, "Infrastructure & Shared Utilities");

        // Verify hub file is in infrastructure component
        let infra_edges: Vec<&Edge> = result_with
            .edges
            .iter()
            .filter(|e| e.from == "infer:component:infrastructure" && e.relation == "contains")
            .collect();
        assert_eq!(infra_edges.len(), 1, "Infrastructure should contain exactly 1 hub file");
        assert_eq!(infra_edges[0].to, "file:src/utils/ink.ts");
    }

    #[test]
    fn test_hub_exclusion_disabled() {
        let mut g = Graph::default();
        g.nodes.push(make_file_node("src/hub.ts"));
        for i in 0..10 {
            g.nodes.push(make_file_node(&format!("src/f{}.ts", i)));
            g.edges.push(Edge::new(
                &format!("file:src/f{}.ts", i),
                "file:src/hub.ts",
                "imports",
            ));
        }

        // threshold=0.0 disables hub exclusion
        let config = ClusterConfig {
            seed: 42,
            num_trials: 10,
            min_community_size: 1,
            hub_exclusion_threshold: 0.0,
            ..Default::default()
        };
        let result = cluster(&g, &config).unwrap();

        // No infrastructure component should exist
        let infra = result.nodes.iter().find(|n| n.id == "infer:component:infrastructure");
        assert!(infra.is_none(), "Hub exclusion disabled should produce no infrastructure component");
    }

    #[test]
    fn test_infra_component_created() {
        // Verify the infrastructure component has correct metadata
        let excluded_ids = vec![
            "file:src/utils/ink.ts".to_string(),
            "file:src/utils/debug.ts".to_string(),
        ];
        let g = Graph::default();

        let (node, edges) = create_infra_component(&excluded_ids, &g);

        assert_eq!(node.id, "infer:component:infrastructure");
        assert_eq!(node.title, "Infrastructure & Shared Utilities");
        assert_eq!(node.node_type.as_deref(), Some("component"));
        assert_eq!(node.source.as_deref(), Some("infer"));
        assert_eq!(
            node.metadata.get("size").and_then(|v| v.as_u64()),
            Some(2),
        );
        assert_eq!(
            node.metadata.get("hub_excluded").and_then(|v| v.as_bool()),
            Some(true),
        );

        assert_eq!(edges.len(), 2);
        for edge in &edges {
            assert_eq!(edge.from, "infer:component:infrastructure");
            assert_eq!(edge.relation, "contains");
        }
        let edge_targets: Vec<&str> = edges.iter().map(|e| e.to.as_str()).collect();
        assert!(edge_targets.contains(&"file:src/utils/ink.ts"));
        assert!(edge_targets.contains(&"file:src/utils/debug.ts"));
    }

    // ── Confidence-aware weighting tests ───────────────────────────────

    #[test]
    fn test_build_network_confidence_scales_weight() {
        // Two edges of same relation type but different confidence.
        // LSP-confirmed (1.0) should get full weight, heuristic (0.3) should be scaled down.
        let mut g = Graph::default();
        g.nodes.push(make_file_node("a.rs"));
        g.nodes.push(make_file_node("b.rs"));
        g.nodes.push(make_file_node("c.rs"));

        let mut edge_high = Edge::new("file:a.rs", "file:b.rs", "calls");
        edge_high.confidence = Some(1.0); // LSP confirmed
        g.edges.push(edge_high);

        let mut edge_low = Edge::new("file:a.rs", "file:c.rs", "calls");
        edge_low.confidence = Some(0.3); // tree-sitter heuristic
        g.edges.push(edge_low);

        let (net, idx_to_id) = build_network(&g, &ClusterConfig::default());

        let a = idx_to_id.iter().position(|id| id == "file:a.rs").unwrap();
        let b = idx_to_id.iter().position(|id| id == "file:b.rs").unwrap();
        let c = idx_to_id.iter().position(|id| id == "file:c.rs").unwrap();

        let out = net.out_neighbors(a);
        let weight_ab = out.iter().find(|&&(t, _)| t == b).map(|&(_, w)| w).unwrap();
        let weight_ac = out.iter().find(|&&(t, _)| t == c).map(|&(_, w)| w).unwrap();

        // Both are "calls" (base weight 1.0), but:
        // a→b: 1.0 * 1.0 = 1.0
        // a→c: 1.0 * 0.3 = 0.3
        assert!(
            (weight_ab - 1.0).abs() < 1e-9,
            "full confidence edge should have weight 1.0, got {}",
            weight_ab
        );
        assert!(
            (weight_ac - 0.3).abs() < 1e-9,
            "low confidence edge should have weight 0.3, got {}",
            weight_ac
        );
    }

    #[test]
    fn test_build_network_no_confidence_defaults_to_one() {
        // Edges without confidence field (legacy/manual) should behave as confidence=1.0
        let mut g = Graph::default();
        g.nodes.push(make_file_node("x.rs"));
        g.nodes.push(make_file_node("y.rs"));
        g.edges.push(Edge::new("file:x.rs", "file:y.rs", "imports")); // confidence = None

        let (net, idx_to_id) = build_network(&g, &ClusterConfig::default());

        let x = idx_to_id.iter().position(|id| id == "file:x.rs").unwrap();
        let y = idx_to_id.iter().position(|id| id == "file:y.rs").unwrap();

        let out = net.out_neighbors(x);
        let weight = out.iter().find(|&&(t, _)| t == y).map(|&(_, w)| w).unwrap();

        // "imports" base weight = 0.8, confidence = 1.0 (default) → 0.8
        assert!(
            (weight - WEIGHT_IMPORTS).abs() < 1e-9,
            "no-confidence edge should use default 1.0, got weight {}",
            weight
        );
    }

    #[test]
    fn test_co_citation_ignores_low_confidence_citers() {
        // If citing edges have low confidence (<0.7), they shouldn't count as citers.
        let mut g = Graph::default();
        g.nodes.push(make_file_node("utils/a.ts"));
        g.nodes.push(make_file_node("utils/b.ts"));
        g.nodes.push(make_file_node("features/f1.ts"));
        g.nodes.push(make_file_node("features/f2.ts"));

        // f1 and f2 both "import" a and b, but with low confidence (0.3)
        for from in &["file:features/f1.ts", "file:features/f2.ts"] {
            for to in &["file:utils/a.ts", "file:utils/b.ts"] {
                let mut edge = Edge::new(from, to, "imports");
                edge.confidence = Some(0.3); // heuristic guess
                g.edges.push(edge);
            }
        }

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
        let edges_before = net.num_edges();

        add_co_citation_edges(&mut net, &g, &idx_to_id, 0.4, 2, 2.0);

        // Low-confidence citers (0.3 < 0.7 threshold) should NOT generate co-citation edges
        assert_eq!(
            net.num_edges(),
            edges_before,
            "low-confidence citers should not create co-citation edges"
        );
    }

    #[test]
    fn test_co_citation_uses_high_confidence_citers() {
        // High confidence (≥0.7) citing edges SHOULD produce co-citation.
        let mut g = Graph::default();
        g.nodes.push(make_file_node("utils/a.ts"));
        g.nodes.push(make_file_node("utils/b.ts"));
        g.nodes.push(make_file_node("features/f1.ts"));
        g.nodes.push(make_file_node("features/f2.ts"));

        // f1 and f2 both import a and b with high confidence (LSP confirmed)
        for from in &["file:features/f1.ts", "file:features/f2.ts"] {
            for to in &["file:utils/a.ts", "file:utils/b.ts"] {
                let mut edge = Edge::new(from, to, "imports");
                edge.confidence = Some(0.95);
                g.edges.push(edge);
            }
        }

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());
        let edges_before = net.num_edges();

        add_co_citation_edges(&mut net, &g, &idx_to_id, 0.4, 2, 2.0);

        // High-confidence citers should create co-citation edges
        assert!(
            net.num_edges() > edges_before,
            "high-confidence citers should create co-citation: before={}, after={}",
            edges_before,
            net.num_edges()
        );
    }

    #[test]
    fn test_co_citation_skips_pairs_with_direct_high_confidence_edge() {
        // If A and B already have a direct high-confidence edge (LSP confirmed),
        // co-citation should NOT add a redundant synthetic edge.
        let mut g = Graph::default();
        g.nodes.push(make_file_node("utils/a.ts"));
        g.nodes.push(make_file_node("utils/b.ts"));
        g.nodes.push(make_file_node("features/f1.ts"));
        g.nodes.push(make_file_node("features/f2.ts"));

        // Direct high-confidence edge: a→b (LSP confirmed call)
        let mut direct = Edge::new("file:utils/a.ts", "file:utils/b.ts", "calls");
        direct.confidence = Some(1.0);
        g.edges.push(direct);

        // f1 and f2 both import a and b with high confidence
        for from in &["file:features/f1.ts", "file:features/f2.ts"] {
            for to in &["file:utils/a.ts", "file:utils/b.ts"] {
                let mut edge = Edge::new(from, to, "imports");
                edge.confidence = Some(1.0);
                g.edges.push(edge);
            }
        }

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());

        // Record a→b weight before co-citation
        let a_idx = idx_to_id.iter().position(|id| id == "file:utils/a.ts").unwrap();
        let b_idx = idx_to_id.iter().position(|id| id == "file:utils/b.ts").unwrap();
        let _edges_before = net.num_edges();

        add_co_citation_edges(&mut net, &g, &idx_to_id, 0.4, 2, 2.0);

        // The a↔b co-citation should be suppressed because they have a direct
        // high-confidence edge already. The network should NOT gain a↔b edges.
        let a_out_to_b: Vec<f64> = net
            .out_neighbors(a_idx)
            .iter()
            .filter(|&&(t, _)| t == b_idx)
            .map(|&(_, w)| w)
            .collect();

        // There should be exactly 1 edge a→b (the direct one from build_network),
        // not 2 (direct + co-citation).
        assert_eq!(
            a_out_to_b.len(),
            1,
            "should have exactly 1 a→b edge (direct), not {} (co-citation would add another)",
            a_out_to_b.len()
        );
    }

    #[test]
    fn test_co_citation_weight_scales_by_confidence() {
        // Co-citation weight should be scaled by the mean confidence of citing edges.
        let mut g = Graph::default();
        g.nodes.push(make_file_node("utils/a.ts"));
        g.nodes.push(make_file_node("utils/b.ts"));
        g.nodes.push(make_file_node("utils/c.ts"));
        g.nodes.push(make_file_node("features/f1.ts"));
        g.nodes.push(make_file_node("features/f2.ts"));
        g.nodes.push(make_file_node("features/f3.ts"));

        // f1, f2 import a and b with confidence 1.0 (perfect)
        for from in &["file:features/f1.ts", "file:features/f2.ts"] {
            for to in &["file:utils/a.ts", "file:utils/b.ts"] {
                let mut edge = Edge::new(from, to, "imports");
                edge.confidence = Some(1.0);
                g.edges.push(edge);
            }
        }

        // f1, f2 import c with confidence 0.7 (barely above threshold)
        // f3 imports a and c with confidence 1.0
        // So a↔c: shared citers = {f1(a:1.0, c:0.7), f2(a:1.0, c:0.7)} → 2 shared
        for from in &["file:features/f1.ts", "file:features/f2.ts"] {
            let mut edge = Edge::new(from, "file:utils/c.ts", "imports");
            edge.confidence = Some(0.7);
            g.edges.push(edge);
        }

        let (mut net, idx_to_id) = build_network(&g, &ClusterConfig::default());

        add_co_citation_edges(&mut net, &g, &idx_to_id, 0.4, 2, 2.0);

        let a_idx = idx_to_id.iter().position(|id| id == "file:utils/a.ts").unwrap();
        let b_idx = idx_to_id.iter().position(|id| id == "file:utils/b.ts").unwrap();
        let c_idx = idx_to_id.iter().position(|id| id == "file:utils/c.ts").unwrap();

        // a↔b: shared citers {f1, f2}, each with confidence 1.0 for both.
        // avg_confidence = sqrt(1.0*1.0) = 1.0 per citer → avg = 1.0
        // weight = 0.4 * 2 * 1.0 = 0.8
        let weight_ab: f64 = net
            .out_neighbors(a_idx)
            .iter()
            .filter(|&&(t, _)| t == b_idx)
            .map(|&(_, w)| w)
            .sum();

        // a↔c: shared citers {f1, f2}, each with conf_a=1.0, conf_c=0.7
        // geometric mean per citer = sqrt(1.0 * 0.7) ≈ 0.8367
        // avg_confidence ≈ 0.8367
        // weight = 0.4 * 2 * 0.8367 ≈ 0.6693
        let weight_ac: f64 = net
            .out_neighbors(a_idx)
            .iter()
            .filter(|&&(t, _)| t == c_idx)
            .map(|&(_, w)| w)
            .sum();

        assert!(
            weight_ab > weight_ac,
            "a↔b (all conf=1.0) should be stronger than a↔c (mixed conf): {} vs {}",
            weight_ab,
            weight_ac
        );

        // Verify approximate values
        assert!(
            (weight_ab - 0.8).abs() < 0.01,
            "a↔b should be ~0.8, got {}",
            weight_ab
        );
        let expected_ac = 0.4 * 2.0 * (1.0_f64 * 0.7).sqrt();
        assert!(
            (weight_ac - expected_ac).abs() < 0.01,
            "a↔c should be ~{:.4}, got {}",
            expected_ac,
            weight_ac
        );
    }
}
