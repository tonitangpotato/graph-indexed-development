use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use crate::task_graph_knowledge::{KnowledgeNode, KnowledgeGraph, KnowledgeManagement};

/// A complete GID graph with nodes and edges.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Graph {
    #[serde(default)]
    pub project: Option<ProjectMeta>,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

/// Project metadata.
/// Accepts either a string (`project: myproject`) or a struct (`project: {name: myproject}`).
#[derive(Debug, Clone, Serialize)]
pub struct ProjectMeta {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

impl<'de> serde::Deserialize<'de> for ProjectMeta {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;

        struct ProjectMetaVisitor;

        impl<'de> de::Visitor<'de> for ProjectMetaVisitor {
            type Value = ProjectMeta;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string or a map with 'name' field")
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<ProjectMeta, E>
            where
                E: de::Error,
            {
                Ok(ProjectMeta { name: v.to_string(), description: None })
            }

            fn visit_map<M>(self, map: M) -> std::result::Result<ProjectMeta, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                #[derive(serde::Deserialize)]
                struct ProjectMetaInner {
                    name: String,
                    #[serde(default)]
                    description: Option<String>,
                }
                let inner = ProjectMetaInner::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(ProjectMeta { name: inner.name, description: inner.description })
            }
        }

        deserializer.deserialize_any(ProjectMetaVisitor)
    }
}

/// A node in the graph (task, code file, component, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub status: NodeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Priority 0–255. SQLite stores as INTEGER; values outside 0–255 are clamped on read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    /// Node type: task, file, component, feature, layer, etc.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub node_type: Option<String>,
    /// Knowledge storage: findings, file cache, and tool history.
    #[serde(default, skip_serializing_if = "KnowledgeNode::is_empty")]
    pub knowledge: KnowledgeNode,
    /// Additional metadata.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,

    // ── Code-graph fields (populated by `gid extract`, None for task nodes) ──

    /// File path relative to the project root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    /// Programming language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// Start line number in the source file.
    /// Note: stored as INTEGER in SQLite; clamped to usize range on read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    /// End line number in the source file.
    /// Note: stored as INTEGER in SQLite; clamped to usize range on read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    /// Function/method signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Visibility: public, private, crate, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    /// Documentation comment extracted from source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_comment: Option<String>,
    /// Hash of the body content (for change detection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_hash: Option<String>,
    /// Code-level kind: Function, Struct, Impl, Trait, Enum, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_kind: Option<String>,

    // ── Provenance fields ──

    /// Owner of this node (person or team).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Source of this node (e.g., "extract", "manual", "import").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Repository this node belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,

    // ── Hierarchy & structure fields ──

    /// Parent node ID (for hierarchical relationships).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Depth in the node hierarchy (0 = root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,

    // ── Analysis fields ──

    /// Complexity score (e.g., cyclomatic complexity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complexity: Option<f64>,
    /// Whether this node represents a public API surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_public: Option<bool>,
    /// Full body/content of the node (source code, description text, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,

    // ── Timestamps ──

    /// ISO-8601 creation timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// ISO-8601 last-updated timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl Node {
    pub fn new(id: &str, title: &str) -> Self {
        Self {
            id: id.to_string(),
            title: title.to_string(),
            status: NodeStatus::Todo,
            description: None,
            assigned_to: None,
            tags: Vec::new(),
            priority: None,
            node_type: None,
            knowledge: KnowledgeNode::default(),
            metadata: HashMap::new(),
            file_path: None,
            lang: None,
            start_line: None,
            end_line: None,
            signature: None,
            visibility: None,
            doc_comment: None,
            body_hash: None,
            node_kind: None,
            owner: None,
            source: None,
            repo: None,
            parent_id: None,
            depth: None,
            complexity: None,
            is_public: None,
            body: None,
            created_at: None,
            updated_at: None,
        }
    }

    pub fn with_description(mut self, desc: &str) -> Self {
        self.description = Some(desc.to_string());
        self
    }

    pub fn with_status(mut self, status: NodeStatus) -> Self {
        self.status = status;
        self
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = Some(priority);
        self
    }
}

/// Status of a node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeStatus {
    Todo,
    #[serde(alias = "in_progress", alias = "in-progress")]
    InProgress,
    Done,
    Blocked,
    Cancelled,
    /// Task execution failed (verify failed, sub-agent error, etc.)
    Failed,
    /// Task needs human/re-planner intervention (merge conflict, structural issue)
    #[serde(alias = "needs_resolution", alias = "needs-resolution")]
    NeedsResolution,
}

impl Default for NodeStatus {
    fn default() -> Self {
        Self::Todo
    }
}

impl std::fmt::Display for NodeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeStatus::Todo => write!(f, "todo"),
            NodeStatus::InProgress => write!(f, "in_progress"),
            NodeStatus::Done => write!(f, "done"),
            NodeStatus::Blocked => write!(f, "blocked"),
            NodeStatus::Cancelled => write!(f, "cancelled"),
            NodeStatus::Failed => write!(f, "failed"),
            NodeStatus::NeedsResolution => write!(f, "needs_resolution"),
        }
    }
}

impl std::str::FromStr for NodeStatus {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "todo" => Ok(NodeStatus::Todo),
            "in_progress" | "in-progress" => Ok(NodeStatus::InProgress),
            "done" => Ok(NodeStatus::Done),
            "blocked" => Ok(NodeStatus::Blocked),
            "cancelled" => Ok(NodeStatus::Cancelled),
            "failed" => Ok(NodeStatus::Failed),
            "needs_resolution" | "needs-resolution" => Ok(NodeStatus::NeedsResolution),
            _ => Err(anyhow::anyhow!("Unknown status: {}", s)),
        }
    }
}

/// An edge (relationship) between two nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    #[serde(default = "default_relation")]
    pub relation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Additional edge metadata, serialized as JSON in SQLite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

fn default_relation() -> String {
    "depends_on".to_string()
}

impl Edge {
    pub fn new(from: &str, to: &str, relation: &str) -> Self {
        Self {
            from: from.to_string(),
            to: to.to_string(),
            relation: relation.to_string(),
            weight: None,
            confidence: None,
            metadata: None,
        }
    }

    pub fn depends_on(from: &str, to: &str) -> Self {
        Self::new(from, to, "depends_on")
    }

    /// Extract the source from edge metadata (e.g. "extract", "auto-bridge", "project")
    pub fn source(&self) -> Option<&str> {
        self.metadata.as_ref()
            .and_then(|m| m.get("source"))
            .and_then(|v| v.as_str())
    }
}

/// Task specification for `add_feature()`.
#[derive(Debug, Clone)]
pub struct TaskSpec {
    pub title: String,
    pub status: Option<NodeStatus>,  // default: Todo
    pub tags: Vec<String>,           // default: []
    pub deps: Vec<String>,           // titles of tasks this depends on
}

/// Infer the node type from the ID prefix (text before the first `:`).
///
/// Returns `Some(type_name)` if the prefix is recognized, `None` otherwise.
///
/// # Examples
/// ```
/// use gid_core::infer_node_type;
/// assert_eq!(infer_node_type("file:src/main.rs"), Some("file"));
/// assert_eq!(infer_node_type("fn:my_func"), Some("function"));
/// assert_eq!(infer_node_type("struct:MyStruct"), Some("class"));
/// assert_eq!(infer_node_type("unknown-id"), None);
/// ```
pub fn infer_node_type(id: &str) -> Option<&str> {
    let prefix = id.split(':').next()?;
    match prefix {
        "file" => Some("file"),
        "fn" | "func" => Some("function"),
        "struct" | "class" => Some("class"),
        "mod" | "module" => Some("module"),
        "method" => Some("method"),
        "trait" | "interface" => Some("trait"),
        "enum" => Some("enum"),
        "const" | "static" => Some("constant"),
        "test" => Some("test"),
        "impl" => Some("impl"),
        _ => None,
    }
}

// ─── Graph operations ────────────────────────────────────────

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    // ── Node operations ──

    pub fn get_node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn get_node_mut(&mut self, id: &str) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    pub fn add_node(&mut self, node: Node) {
        if self.get_node(&node.id).is_none() {
            let mut node = node;
            // Auto-infer node_type from ID prefix if not already set
            if node.node_type.is_none() {
                if let Some(inferred) = infer_node_type(&node.id) {
                    node.node_type = Some(inferred.to_string());
                }
            }
            self.nodes.push(node);
        }
    }

    pub fn remove_node(&mut self, id: &str) -> Option<Node> {
        let pos = self.nodes.iter().position(|n| n.id == id)?;
        let node = self.nodes.remove(pos);
        // Remove associated edges
        self.edges.retain(|e| e.from != id && e.to != id);
        Some(node)
    }

    pub fn update_status(&mut self, id: &str, status: NodeStatus) -> bool {
        if let Some(node) = self.get_node_mut(id) {
            node.status = status;
            true
        } else {
            false
        }
    }

    // ── Edge operations ──

    pub fn add_edge(&mut self, edge: Edge) {
        // Avoid duplicates
        let exists = self.edges.iter().any(|e| {
            e.from == edge.from && e.to == edge.to && e.relation == edge.relation
        });
        if !exists {
            self.edges.push(edge);
        }
    }

    pub fn remove_edge(&mut self, from: &str, to: &str, relation: Option<&str>) {
        self.edges.retain(|e| {
            !(e.from == from && e.to == to && relation.map_or(true, |r| e.relation == r))
        });
    }

    /// Add an edge with deduplication check.
    ///
    /// Returns `true` if the edge was added (new), `false` if it already existed.
    /// An edge is considered duplicate if the (from, to, relation) triple matches.
    ///
    /// # Examples
    ///
    /// ```
    /// use gid_core::{Graph, Edge};
    ///
    /// let mut g = Graph::new();
    /// let edge = Edge::new("a", "b", "depends_on");
    /// assert!(g.add_edge_dedup(edge.clone())); // Returns true (new edge)
    /// assert!(!g.add_edge_dedup(edge)); // Returns false (duplicate)
    /// ```
    pub fn add_edge_dedup(&mut self, edge: Edge) -> bool {
        let exists = self.edges.iter().any(|e| {
            e.from == edge.from && e.to == edge.to && e.relation == edge.relation
        });
        if !exists {
            self.edges.push(edge);
            true
        } else {
            false
        }
    }

    /// Ensures a node ID is unique by appending -2, -3, etc. if needed.
    fn ensure_unique_id(&self, base: String) -> String {
        if self.get_node(&base).is_none() {
            return base;
        }
        for i in 2..1000 {
            let candidate = format!("{}-{}", base, i);
            if self.get_node(&candidate).is_none() {
                return candidate;
            }
        }
        format!("{}-overflow", base)
    }

    /// Create a feature node with task nodes and all edges in one operation.
    ///
    /// - Creates `feat-{slug}` feature node
    /// - Creates `task-{feature_slug}-{task_slug}` task nodes
    /// - Adds `implements` edges from each task to the feature
    /// - Adds `depends_on` edges between tasks per TaskSpec.deps (matched by title)
    /// - Returns the feature node ID
    pub fn add_feature(&mut self, name: &str, tasks: &[TaskSpec]) -> String {
        use crate::slugify::slugify;

        let feature_slug = slugify(name);
        let feat_id = self.ensure_unique_id(format!("feat-{}", feature_slug));

        let mut feat = Node::new(&feat_id, name);
        feat.node_type = Some("feature".into());
        feat.status = NodeStatus::Todo;
        self.add_node(feat);

        // Map title -> actual task ID for dep resolution
        let mut task_ids: HashMap<String, String> = HashMap::new();

        for spec in tasks {
            let task_slug = slugify(&spec.title);
            let base_id = format!("task-{}-{}", feature_slug, task_slug);
            let task_id = self.ensure_unique_id(base_id);

            let mut task = Node::new(&task_id, &spec.title);
            task.node_type = Some("task".into());
            task.status = spec.status.clone().unwrap_or(NodeStatus::Todo);
            task.tags = spec.tags.clone();
            self.add_node(task);

            // implements edge: task -> feature
            self.add_edge_dedup(Edge::new(&task_id, &feat_id, "implements"));

            task_ids.insert(spec.title.clone(), task_id);
        }

        // Add dependency edges between tasks
        for spec in tasks {
            if let Some(from_id) = task_ids.get(&spec.title) {
                for dep_title in &spec.deps {
                    if let Some(to_id) = task_ids.get(dep_title.as_str()) {
                        self.add_edge_dedup(Edge::new(from_id, to_id, "depends_on"));
                    }
                }
            }
        }

        feat_id
    }

    /// Add a standalone task node (no parent feature required).
    /// Returns the task node ID.
    pub fn add_task(
        &mut self,
        title: &str,
        for_feature: Option<&str>,
        depends_on: &[String],
        tags: &[String],
        priority: Option<u8>,
    ) -> String {
        use crate::slugify::slugify;

        let task_slug = slugify(title);
        let base_id = if let Some(feat_id) = for_feature {
            // If attached to a feature, prefix with feature slug
            let feat_slug = feat_id.strip_prefix("feat-").unwrap_or(feat_id);
            format!("task-{}-{}", feat_slug, task_slug)
        } else {
            format!("task-{}", task_slug)
        };
        let task_id = self.ensure_unique_id(base_id);

        let mut task = Node::new(&task_id, title);
        task.node_type = Some("task".into());
        task.status = NodeStatus::Todo;
        task.tags = tags.to_vec();
        task.priority = priority;
        self.add_node(task);

        // If for_feature specified, add implements edge
        if let Some(feat_id) = for_feature {
            self.add_edge_dedup(Edge::new(&task_id, feat_id, "implements"));
        }

        // Add depends_on edges - support both exact IDs and fuzzy resolution
        for dep in depends_on {
            let resolved = self.resolve_node(dep);
            if let Some(dep_node) = resolved.first() {
                let dep_id = dep_node.id.clone();
                self.add_edge_dedup(Edge::new(&task_id, &dep_id, "depends_on"));
            } else {
                eprintln!("⚠ Could not resolve dependency: {}", dep);
            }
        }

        task_id
    }

    /// Merge incoming nodes into this graph, scoped to a specific feature.
    ///
    /// 1. Finds all existing task nodes that `implements` the target feature
    /// 2. Removes those old task nodes (cascading edge cleanup via remove_node)
    /// 3. Adds all incoming nodes
    /// 4. Adds `implements` edges from incoming task nodes to the feature
    /// 5. Adds incoming edges with deduplication
    ///
    /// Returns (removed_count, added_count) for reporting.
    pub fn merge_feature_nodes(&mut self, feature_id: &str, incoming: Graph) -> (usize, usize) {
        // Step 1: Find old feature tasks
        let old_task_ids: Vec<String> = self.edges.iter()
            .filter(|e| e.to == feature_id && e.relation == "implements")
            .map(|e| e.from.clone())
            .collect();

        let removed = old_task_ids.len();

        // Step 2: Remove old task nodes (remove_node cascades edges)
        for id in &old_task_ids {
            self.remove_node(id);
        }

        // Step 3: Collect incoming node IDs
        let incoming_node_ids: std::collections::HashSet<String> = incoming.nodes.iter()
            .map(|n| n.id.clone())
            .collect();
        let added = incoming.nodes.len();

        // Step 4: Add all incoming nodes
        for node in incoming.nodes {
            self.add_node(node);
        }

        // Step 5: Add implements edges for each new node
        for id in &incoming_node_ids {
            self.add_edge_dedup(Edge::new(id, feature_id, "implements"));
        }

        // Step 6: Add incoming edges with dedup
        for edge in incoming.edges {
            self.add_edge_dedup(edge);
        }

        (removed, added)
    }

    /// Resolve a node reference to actual node(s) using a 7-tier priority cascade.
    ///
    /// Priority tiers (highest to lowest):
    /// 1. Exact ID match
    /// 2. Exact title match (case-insensitive)
    /// 3. Structural segment match (`:`, `-`, `/` delimiters)
    /// 4. Word segment match (`_` delimiter)
    /// 5. File path match
    /// 6. Title substring match (case-insensitive)
    /// 7. ID substring match (case-insensitive)
    ///
    /// Returns a vector of matching nodes. Empty vector if no match found.
    /// May return multiple nodes if there's ambiguity (e.g., multiple substring matches).
    ///
    /// # Examples
    ///
    /// ```
    /// use gid_core::{Graph, Node};
    ///
    /// let mut g = Graph::new();
    /// g.add_node(Node::new("feat-auth", "Authentication Feature"));
    /// g.add_node(Node::new("impl-jwt", "Implement JWT validation"));
    ///
    /// // Exact ID match
    /// let results = g.resolve_node("feat-auth");
    /// assert_eq!(results.len(), 1);
    /// assert_eq!(results[0].id, "feat-auth");
    ///
    /// // Case-insensitive title match
    /// let results = g.resolve_node("authentication feature");
    /// assert_eq!(results.len(), 1);
    ///
    /// // No match
    /// let results = g.resolve_node("nonexistent");
    /// assert_eq!(results.len(), 0);
    /// ```
    pub fn resolve_node(&self, reference: &str) -> Vec<&Node> {
        let reference_lower = reference.to_lowercase();

        // Tier 1: Exact ID match
        if let Some(node) = self.nodes.iter().find(|n| n.id == reference) {
            return vec![node];
        }

        // Tier 2: Exact title match (case-insensitive)
        let exact_title: Vec<&Node> = self.nodes.iter()
            .filter(|n| n.title.to_lowercase() == reference_lower)
            .collect();
        if !exact_title.is_empty() {
            return exact_title;
        }

        // Tier 3: Structural segment match (: - / delimiters)
        let structural_segments = extract_segments(&reference_lower, &[':', '-', '/']);
        if !structural_segments.is_empty() {
            let matches: Vec<&Node> = self.nodes.iter()
                .filter(|n| {
                    let id_segs = extract_segments(&n.id.to_lowercase(), &[':', '-', '/']);
                    let title_segs = extract_segments(&n.title.to_lowercase(), &[':', '-', '/']);
                    segments_match(&structural_segments, &id_segs) || 
                    segments_match(&structural_segments, &title_segs)
                })
                .collect();
            if !matches.is_empty() {
                return matches;
            }
        }

        // Tier 4: Word segment match (_ delimiter)
        let word_segments = extract_segments(&reference_lower, &['_']);
        if !word_segments.is_empty() {
            let matches: Vec<&Node> = self.nodes.iter()
                .filter(|n| {
                    let id_segs = extract_segments(&n.id.to_lowercase(), &['_']);
                    let title_segs = extract_segments(&n.title.to_lowercase(), &['_']);
                    segments_match(&word_segments, &id_segs) || 
                    segments_match(&word_segments, &title_segs)
                })
                .collect();
            if !matches.is_empty() {
                return matches;
            }
        }

        // Tier 5: File path match
        let matches: Vec<&Node> = self.nodes.iter()
            .filter(|n| {
                n.file_path.as_ref()
                    .map(|fp| fp.to_lowercase().contains(&reference_lower))
                    .unwrap_or(false)
            })
            .collect();
        if !matches.is_empty() {
            return matches;
        }

        // Tier 6: Title substring match (case-insensitive)
        let matches: Vec<&Node> = self.nodes.iter()
            .filter(|n| n.title.to_lowercase().contains(&reference_lower))
            .collect();
        if !matches.is_empty() {
            return matches;
        }

        // Tier 7: ID substring match (case-insensitive)
        let matches: Vec<&Node> = self.nodes.iter()
            .filter(|n| n.id.to_lowercase().contains(&reference_lower))
            .collect();
        
        matches
    }

    pub fn edges_from(&self, id: &str) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.from == id).collect()
    }

    pub fn edges_to(&self, id: &str) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.to == id).collect()
    }

    // ── Layer filtering helpers ──

    /// Get all code nodes (source == "extract")
    pub fn code_nodes(&self) -> Vec<&Node> {
        self.nodes.iter().filter(|n| n.source.as_deref() == Some("extract")).collect()
    }

    /// Get all project nodes (source == "project", "manual", or legacy None).
    /// ISS-047: `manual` is treated as project layer because `gid_add_task`
    /// (rustclaw's preferred way to file issues/tasks) writes nodes with
    /// source="manual". Excluding them caused `gid tasks` to report "0 nodes"
    /// on graphs full of manually-filed issues.
    pub fn project_nodes(&self) -> Vec<&Node> {
        // TODO: after T4.1 migration backfills source on all nodes, remove the None branch
        self.nodes.iter().filter(|n| {
            match n.source.as_deref() {
                None => true,
                Some("project") | Some("manual") => true,
                _ => false,
            }
        }).collect()
    }

    /// Get all inferred nodes (source == "infer"). ISS-047: surfaced in
    /// `summary()` so `gid tasks` can report inferred component/feature
    /// counts without lying about totals.
    pub fn inferred_nodes(&self) -> Vec<&Node> {
        self.nodes.iter().filter(|n| n.source.as_deref() == Some("infer")).collect()
    }

    /// Get nodes whose source doesn't fit project/code/infer buckets.
    /// ISS-047: catch-all so the summary always sums to total node count.
    pub fn other_source_nodes(&self) -> Vec<&Node> {
        self.nodes.iter().filter(|n| {
            match n.source.as_deref() {
                None => false,
                Some("project") | Some("manual") | Some("extract") | Some("infer") => false,
                _ => true,
            }
        }).collect()
    }

    /// Get all code edges (source == "extract")
    pub fn code_edges(&self) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.source() == Some("extract")).collect()
    }

    /// Get all project edges (not code, not bridge)
    pub fn project_edges(&self) -> Vec<&Edge> {
        self.edges.iter().filter(|e| {
            let src = e.source();
            src != Some("extract") && src != Some("auto-bridge")
        }).collect()
    }

    /// Get all bridge edges (source == "auto-bridge")
    pub fn bridge_edges(&self) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.source() == Some("auto-bridge")).collect()
    }

    /// Get tasks that are ready (todo + all depends_on are done).
    /// Only considers project nodes; code nodes are excluded.
    ///
    /// Uses pre-built HashMaps for O(N+M) instead of O(N×M×N).
    pub fn ready_tasks(&self) -> Vec<&Node> {
        // Build O(1) lookup: node_id → status
        let status_map: HashMap<&str, &NodeStatus> = self
            .nodes
            .iter()
            .map(|n| (n.id.as_str(), &n.status))
            .collect();

        // Build O(1) adjacency: from_id → Vec<&Edge> (depends_on only)
        let mut dep_edges: HashMap<&str, Vec<&Edge>> = HashMap::new();
        for e in &self.edges {
            if e.relation == "depends_on" {
                dep_edges.entry(e.from.as_str()).or_default().push(e);
            }
        }

        self.project_nodes()
            .into_iter()
            .filter(|n| n.status == NodeStatus::Todo)
            .filter(|n| {
                match dep_edges.get(n.id.as_str()) {
                    None => true, // no dependencies → ready
                    Some(deps) => deps.iter().all(|e| {
                        status_map
                            .get(e.to.as_str())
                            .map_or(true, |s| **s == NodeStatus::Done)
                    }),
                }
            })
            .collect()
    }

    /// Get tasks by status.
    pub fn tasks_by_status(&self, status: &NodeStatus) -> Vec<&Node> {
        self.nodes.iter().filter(|n| &n.status == status).collect()
    }

    /// Summary statistics. Project-layer counts (status/ready) come from
    /// `project_nodes()`; `code_nodes` count is surfaced separately so callers
    /// (CLI, ritual UI) can report both populations honestly.
    /// ISS-034: previously only counted project nodes, which made
    /// `gid tasks --node-type all|code` say "0 nodes" while listing dozens.
    pub fn summary(&self) -> GraphSummary {
        let project_nodes = self.project_nodes();
        let code_nodes = self.code_nodes();
        let inferred_nodes = self.inferred_nodes();
        let other_nodes = self.other_source_nodes();
        let mut s = GraphSummary {
            total_nodes: project_nodes.len(),
            code_nodes: code_nodes.len(),
            inferred_nodes: inferred_nodes.len(),
            other_nodes: other_nodes.len(),
            total_edges: self.edges.len(),
            ..Default::default()
        };
        for n in &project_nodes {
            match n.status {
                NodeStatus::Todo => s.todo += 1,
                NodeStatus::InProgress => s.in_progress += 1,
                NodeStatus::Done => s.done += 1,
                NodeStatus::Blocked => s.blocked += 1,
                NodeStatus::Cancelled => s.cancelled += 1,
                NodeStatus::Failed => s.failed += 1,
                NodeStatus::NeedsResolution => s.needs_resolution += 1,
            }
        }
        s.ready = self.ready_tasks().len();
        s
    }

    /// Get a human-readable text summary of the graph state.
    /// ISS-034: includes code-node count when present so `--node-type all|code`
    /// doesn't claim 0 nodes while listing many.
    pub fn summary_text(&self) -> String {
        let s = self.summary();
        // ISS-047: build header from all non-zero buckets so manual/inferred
        // nodes don't vanish from the count.
        let mut parts: Vec<String> = Vec::new();
        parts.push(format!("{} project nodes", s.total_nodes));
        if s.code_nodes > 0 {
            parts.push(format!("{} code nodes", s.code_nodes));
        }
        if s.inferred_nodes > 0 {
            parts.push(format!("{} inferred nodes", s.inferred_nodes));
        }
        if s.other_nodes > 0 {
            parts.push(format!("{} other nodes", s.other_nodes));
        }
        parts.push(format!("{} edges", s.total_edges));
        let header = format!("Graph: {}", parts.join(", "));
        let mut lines = vec![header];

        if s.total_nodes > 0 {
            lines.push(format!(
                "Status: {} todo, {} in-progress, {} done, {} blocked, {} cancelled",
                s.todo, s.in_progress, s.done, s.blocked, s.cancelled
            ));
            lines.push(format!("Ready tasks: {}", s.ready));
        }

        // Show project name if available
        if let Some(ref project) = self.project {
            lines.insert(0, format!("Project: {}", project.name));
        }

        lines.join("\n")
    }

    /// Calculate graph health score (0.0 to 1.0).
    /// 
    /// Health is based on:
    /// - Progress: ratio of done tasks to total
    /// - Flow: ratio of ready tasks to remaining (non-blocked) tasks
    /// - Connectivity: graphs with edges are healthier than isolated nodes
    /// 
    /// Returns 1.0 for a fully complete graph, 0.0 for an empty or stuck graph.
    pub fn health(&self) -> f64 {
        if self.nodes.is_empty() {
            return 0.0;
        }

        let s = self.summary();
        let total = s.total_nodes as f64;

        // Progress score: what fraction is done?
        let progress = s.done as f64 / total;

        // Flow score: are there ready tasks to work on? (avoid stuck graphs)
        let remaining = s.todo + s.in_progress;
        let flow = if remaining == 0 {
            1.0 // All done, perfect flow
        } else if s.ready == 0 && s.todo > 0 {
            0.0 // Stuck: todos exist but none are ready (all blocked by dependencies)
        } else {
            (s.ready as f64) / (remaining as f64)
        };

        // Connectivity score: graphs with structure are healthier
        let connectivity = if self.nodes.len() > 1 {
            let max_edges = self.nodes.len() * (self.nodes.len() - 1);
            let actual = self.edges.len().min(max_edges);
            (actual as f64 / max_edges as f64).min(1.0)
        } else {
            1.0 // Single node is "connected"
        };

        // Blocked penalty: heavily blocked graphs are unhealthy
        let blocked_ratio = s.blocked as f64 / total;
        let blocked_penalty = 1.0 - blocked_ratio;

        // Weighted combination
        let health = 0.4 * progress + 0.3 * flow + 0.1 * connectivity + 0.2 * blocked_penalty;
        health.clamp(0.0, 1.0)
    }

    /// Mark a task as done. Returns true if found and updated.
    pub fn mark_task_done(&mut self, node_id: &str) -> bool {
        self.update_status(node_id, NodeStatus::Done)
    }

    /// Get executable tasks (alias for ready_tasks, returns owned Task structs).
    pub fn get_executable_tasks(&self) -> Vec<Task> {
        self.ready_tasks()
            .into_iter()
            .map(|node| Task {
                id: node.id.clone(),
                title: node.title.clone(),
                description: node.description.clone(),
                priority: node.priority,
            })
            .collect()
    }
}

/// A simplified task representation for execution.
#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<u8>,
}

#[derive(Debug, Default)]
pub struct GraphSummary {
    /// Project-layer node count (source == "project", "manual", or legacy None).
    /// This is what status counts (todo/done/etc.) are computed from.
    /// ISS-047: includes "manual" source so manually-filed issues are counted.
    pub total_nodes: usize,
    /// Code-layer node count (source == "extract"). ISS-034: surfaced
    /// separately so `gid tasks --node-type all|code` can show both
    /// populations without lying about node counts.
    pub code_nodes: usize,
    /// Inferred node count (source == "infer"). ISS-047: surfaced separately
    /// so component/feature inference results don't vanish from the summary.
    pub inferred_nodes: usize,
    /// Catch-all for any source value not in {project, manual, extract, infer, None}.
    /// ISS-047: ensures total node count = sum of all buckets, no silent drops.
    pub other_nodes: usize,
    pub total_edges: usize,
    pub todo: usize,
    pub in_progress: usize,
    pub done: usize,
    pub blocked: usize,
    pub cancelled: usize,
    pub failed: usize,
    pub needs_resolution: usize,
    pub ready: usize,
}

// Implement knowledge management for Graph so users can call
// graph.store_finding(), graph.cache_file(), etc. directly.
impl KnowledgeGraph for Graph {
    fn get_knowledge_mut(&mut self, node_id: &str) -> Option<&mut KnowledgeNode> {
        self.nodes.iter_mut()
            .find(|n| n.id == node_id)
            .map(|n| &mut n.knowledge)
    }

    fn get_knowledge(&self, node_id: &str) -> Option<&KnowledgeNode> {
        self.nodes.iter()
            .find(|n| n.id == node_id)
            .map(|n| &n.knowledge)
    }

    fn get_incoming_edges(&self, node_id: &str) -> Vec<String> {
        self.edges.iter()
            .filter(|e| e.to == node_id)
            .map(|e| e.from.clone())
            .collect()
    }
}

impl KnowledgeManagement for Graph {}

impl std::fmt::Display for GraphSummary {
    /// Render summary. ISS-034: when code_nodes > 0, surface them in the header
    /// so that `gid tasks --node-type all|code` doesn't claim "0 nodes" while
    /// listing dozens of code nodes in the body. Status counts always reflect
    /// project-layer nodes only — code nodes have no task status.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // ISS-047: emit each non-zero bucket so manual/inferred populations
        // don't get silently dropped from the header line.
        let has_extras = self.code_nodes > 0 || self.inferred_nodes > 0 || self.other_nodes > 0;
        if has_extras {
            let mut parts: Vec<String> = vec![format!("{} project nodes", self.total_nodes)];
            if self.code_nodes > 0 {
                parts.push(format!("{} code nodes", self.code_nodes));
            }
            if self.inferred_nodes > 0 {
                parts.push(format!("{} inferred nodes", self.inferred_nodes));
            }
            if self.other_nodes > 0 {
                parts.push(format!("{} other nodes", self.other_nodes));
            }
            parts.push(format!("{} edges", self.total_edges));
            write!(
                f,
                "{} | todo={} progress={} done={} blocked={} failed={} cancelled={} | ready={}",
                parts.join(", "),
                self.todo, self.in_progress, self.done, self.blocked, self.failed, self.cancelled,
                self.ready,
            )
        } else {
            write!(
                f,
                "{} nodes, {} edges | todo={} progress={} done={} blocked={} failed={} cancelled={} | ready={}",
                self.total_nodes, self.total_edges,
                self.todo, self.in_progress, self.done, self.blocked, self.failed, self.cancelled,
                self.ready,
            )
        }
    }
}


// ── Helper functions for resolve_node ──

/// Extract segments from text using the given delimiters.
fn extract_segments(text: &str, delimiters: &[char]) -> Vec<String> {
    let mut segments = vec![text.to_string()];
    
    for &delimiter in delimiters {
        let mut new_segments = Vec::new();
        for segment in segments {
            new_segments.extend(
                segment.split(delimiter)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            );
        }
        segments = new_segments;
    }
    
    segments
}

/// Check if all query segments appear in target segments (order-independent).
fn segments_match(query_segments: &[String], target_segments: &[String]) -> bool {
    if query_segments.is_empty() {
        return false;
    }
    query_segments.iter().all(|q| target_segments.contains(q))
}

#[cfg(test)]
mod layer_filter_tests {
    use super::*;

    fn mixed_graph() -> Graph {
        let mut g = Graph::new();
        // Project nodes
        let mut task = Node::new("task-1", "My Task");
        task.source = Some("project".to_string());
        g.add_node(task);

        let legacy = Node::new("legacy-1", "Legacy Task");
        // No source (legacy)
        g.add_node(legacy);

        // Code nodes
        let mut code = Node::new("fn:main", "main function");
        code.source = Some("extract".to_string());
        code.node_type = Some("code".to_string());
        code.status = NodeStatus::Done;
        g.add_node(code);

        let mut code2 = Node::new("struct:Config", "Config struct");
        code2.source = Some("extract".to_string());
        code2.node_type = Some("code".to_string());
        code2.status = NodeStatus::Done;
        g.add_node(code2);

        // Edges
        g.add_edge(Edge::new("task-1", "legacy-1", "depends_on"));

        let mut code_edge = Edge::new("fn:main", "struct:Config", "calls");
        code_edge.metadata = Some(serde_json::json!({"source": "extract"}));
        g.add_edge(code_edge);

        let mut bridge = Edge::new("task-1", "fn:main", "maps_to");
        bridge.metadata = Some(serde_json::json!({"source": "auto-bridge"}));
        g.add_edge(bridge);

        g
    }

    #[test]
    fn test_edge_source() {
        let g = mixed_graph();
        // Regular edge has no source
        let proj_edge = g.edges.iter().find(|e| e.relation == "depends_on").unwrap();
        assert_eq!(proj_edge.source(), None);

        let code_edge = g.edges.iter().find(|e| e.relation == "calls").unwrap();
        assert_eq!(code_edge.source(), Some("extract"));

        let bridge_edge = g.edges.iter().find(|e| e.relation == "maps_to").unwrap();
        assert_eq!(bridge_edge.source(), Some("auto-bridge"));
    }

    #[test]
    fn test_code_nodes() {
        let g = mixed_graph();
        let cn = g.code_nodes();
        assert_eq!(cn.len(), 2);
        assert!(cn.iter().all(|n| n.source.as_deref() == Some("extract")));
    }

    #[test]
    fn test_project_nodes() {
        let g = mixed_graph();
        let pn = g.project_nodes();
        assert_eq!(pn.len(), 2); // task-1 + legacy-1
        assert!(pn.iter().any(|n| n.id == "task-1"));
        assert!(pn.iter().any(|n| n.id == "legacy-1"));
    }

    #[test]
    fn test_code_edges() {
        let g = mixed_graph();
        assert_eq!(g.code_edges().len(), 1);
    }

    #[test]
    fn test_project_edges() {
        let g = mixed_graph();
        assert_eq!(g.project_edges().len(), 1); // only depends_on
    }

    #[test]
    fn test_bridge_edges() {
        let g = mixed_graph();
        assert_eq!(g.bridge_edges().len(), 1);
    }

    #[test]
    fn test_summary_excludes_code_nodes() {
        let g = mixed_graph();
        let s = g.summary();
        // Summary should count only project nodes (2), not code nodes (2)
        assert_eq!(s.total_nodes, 2);
    }

    // ── ISS-034: summary surfaces both project and code populations ──

    #[test]
    fn test_iss034_summary_reports_code_nodes_separately() {
        let g = mixed_graph();
        let s = g.summary();
        // total_nodes = project nodes (backward-compat semantics)
        assert_eq!(s.total_nodes, 2);
        // code_nodes is a new field that surfaces the code-layer count
        assert_eq!(s.code_nodes, 2);
        // Status counts come from project nodes only — code nodes have no
        // task status semantics
        assert_eq!(s.done, 0); // legacy-1 + task-1 are both Todo
        assert_eq!(s.todo, 2);
    }

    #[test]
    fn test_iss034_summary_display_includes_code_nodes_when_present() {
        let g = mixed_graph();
        let s = g.summary();
        let rendered = format!("{}", s);
        // Header must mention both populations so user isn't misled
        assert!(rendered.contains("project nodes"),
            "expected 'project nodes' in {}", rendered);
        assert!(rendered.contains("code nodes"),
            "expected 'code nodes' in {}", rendered);
        assert!(rendered.contains("2 project nodes"));
        assert!(rendered.contains("2 code nodes"));
    }

    #[test]
    fn test_iss034_summary_display_omits_code_nodes_when_zero() {
        // Project-only graph keeps the original compact format (backward compat)
        let mut g = Graph::new();
        let mut t = Node::new("task-1", "Only project task");
        t.source = Some("project".to_string());
        g.add_node(t);
        let s = g.summary();
        assert_eq!(s.code_nodes, 0);
        let rendered = format!("{}", s);
        assert!(!rendered.contains("code nodes"),
            "should not mention code nodes when there are none: {}", rendered);
        assert!(rendered.starts_with("1 nodes"),
            "expected legacy compact format, got: {}", rendered);
    }

    #[test]
    fn test_iss034_summary_text_includes_code_nodes() {
        let g = mixed_graph();
        let txt = g.summary_text();
        assert!(txt.contains("2 project nodes"),
            "expected '2 project nodes' in:\n{}", txt);
        assert!(txt.contains("2 code nodes"),
            "expected '2 code nodes' in:\n{}", txt);
    }

    #[test]
    fn test_iss034_code_only_graph_does_not_lie_about_zero_nodes() {
        // Reproduces the original ISS-034 scenario: a v03-style db with
        // only code nodes. Before fix, summary said "0 nodes" while body
        // listed dozens. Now: summary header surfaces the code-node count.
        let mut g = Graph::new();
        let mut c1 = Node::new("fn:foo", "foo");
        c1.source = Some("extract".to_string());
        c1.node_type = Some("code".to_string());
        g.add_node(c1);
        let mut c2 = Node::new("fn:bar", "bar");
        c2.source = Some("extract".to_string());
        c2.node_type = Some("code".to_string());
        g.add_node(c2);

        let s = g.summary();
        assert_eq!(s.total_nodes, 0, "no project nodes");
        assert_eq!(s.code_nodes, 2, "two code nodes");

        let rendered = format!("{}", s);
        // Must NOT claim "0 nodes" when there are 2 code nodes — the
        // whole point of ISS-034
        assert!(rendered.contains("2 code nodes"),
            "expected '2 code nodes' in: {}", rendered);
    }

    #[test]
    fn test_ready_tasks_excludes_code_nodes() {
        let mut g = mixed_graph();
        // Make legacy-1 done so task-1 becomes ready
        g.update_status("legacy-1", NodeStatus::Done);
        let ready = g.ready_tasks();
        // task-1 should be ready, code nodes should NOT appear
        assert!(ready.iter().any(|n| n.id == "task-1"));
        assert!(!ready.iter().any(|n| n.source.as_deref() == Some("extract")));
    }

    // ── ISS-047: summary surfaces manual + inferred + other source nodes ──

    /// Helper: build a graph mixing every source bucket the summary tracks.
    fn multi_source_graph() -> Graph {
        let mut g = Graph::new();

        // 2 project nodes (one explicit, one legacy/None)
        let mut t = Node::new("task-1", "Project task");
        t.source = Some("project".to_string());
        g.add_node(t);
        g.add_node(Node::new("legacy-1", "Legacy task"));

        // 3 manual nodes (gid_add_task default — what rustclaw files issues as)
        for i in 1..=3 {
            let id = format!("iss-{:03}", i);
            let title = format!("Filed issue {}", i);
            let mut m = Node::new(&id, &title);
            m.source = Some("manual".to_string());
            g.add_node(m);
        }

        // 2 code nodes
        let mut c1 = Node::new("fn:foo", "foo");
        c1.source = Some("extract".to_string());
        c1.node_type = Some("code".to_string());
        g.add_node(c1);
        let mut c2 = Node::new("fn:bar", "bar");
        c2.source = Some("extract".to_string());
        c2.node_type = Some("code".to_string());
        g.add_node(c2);

        // 4 inferred nodes (from `gid infer`)
        for i in 1..=4 {
            let id = format!("comp-{}", i);
            let title = format!("Component {}", i);
            let mut inf = Node::new(&id, &title);
            inf.source = Some("infer".to_string());
            g.add_node(inf);
        }

        // 1 unknown-source node — should land in `other_nodes` bucket
        let mut o = Node::new("import-1", "Imported node");
        o.source = Some("imported".to_string());
        g.add_node(o);

        g
    }

    #[test]
    fn test_iss047_project_nodes_includes_manual_source() {
        // Root cause of ISS-047: manual nodes (default for gid_add_task) were
        // dropped from project_nodes(), so `gid tasks` reported 0 nodes on
        // graphs full of manually-filed issues.
        let g = multi_source_graph();
        let pn = g.project_nodes();
        // 1 project + 1 legacy + 3 manual = 5
        assert_eq!(pn.len(), 5, "project bucket must include manual + legacy nodes");
        assert!(pn.iter().any(|n| n.source.as_deref() == Some("manual")),
            "manual nodes must appear in project_nodes()");
        assert!(pn.iter().any(|n| n.source.is_none()),
            "legacy (None) nodes must appear in project_nodes()");
        // Code/infer/other must NOT leak in
        assert!(!pn.iter().any(|n| n.source.as_deref() == Some("extract")));
        assert!(!pn.iter().any(|n| n.source.as_deref() == Some("infer")));
        assert!(!pn.iter().any(|n| n.source.as_deref() == Some("imported")));
    }

    #[test]
    fn test_iss047_inferred_nodes_bucket() {
        let g = multi_source_graph();
        let inf = g.inferred_nodes();
        assert_eq!(inf.len(), 4);
        assert!(inf.iter().all(|n| n.source.as_deref() == Some("infer")));
    }

    #[test]
    fn test_iss047_other_source_nodes_bucket() {
        let g = multi_source_graph();
        let other = g.other_source_nodes();
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].id, "import-1");
    }

    #[test]
    fn test_iss047_summary_buckets_sum_to_total_nodes() {
        // Acceptance criterion: no node is silently dropped from the summary.
        let g = multi_source_graph();
        let s = g.summary();
        let bucket_sum = s.total_nodes + s.code_nodes + s.inferred_nodes + s.other_nodes;
        assert_eq!(
            bucket_sum,
            g.nodes.len(),
            "summary buckets ({} project + {} code + {} inferred + {} other) \
             must sum to total node count ({}) — no silent drops",
            s.total_nodes, s.code_nodes, s.inferred_nodes, s.other_nodes, g.nodes.len()
        );
        // Specifically: 5 project, 2 code, 4 inferred, 1 other = 12 total
        assert_eq!(s.total_nodes, 5);
        assert_eq!(s.code_nodes, 2);
        assert_eq!(s.inferred_nodes, 4);
        assert_eq!(s.other_nodes, 1);
        assert_eq!(g.nodes.len(), 12);
    }

    #[test]
    fn test_iss047_summary_display_lists_all_non_zero_buckets() {
        let g = multi_source_graph();
        let s = g.summary();
        let rendered = format!("{}", s);
        assert!(rendered.contains("5 project nodes"), "got: {}", rendered);
        assert!(rendered.contains("2 code nodes"), "got: {}", rendered);
        assert!(rendered.contains("4 inferred nodes"), "got: {}", rendered);
        assert!(rendered.contains("1 other nodes"), "got: {}", rendered);
    }

    #[test]
    fn test_iss047_summary_text_lists_all_non_zero_buckets() {
        let g = multi_source_graph();
        let txt = g.summary_text();
        assert!(txt.contains("5 project nodes"), "got:\n{}", txt);
        assert!(txt.contains("2 code nodes"), "got:\n{}", txt);
        assert!(txt.contains("4 inferred nodes"), "got:\n{}", txt);
        assert!(txt.contains("1 other nodes"), "got:\n{}", txt);
    }

    #[test]
    fn test_iss047_manual_only_graph_is_not_reported_as_empty() {
        // Reproduces the original ISS-047 scenario: a graph with only
        // gid_add_task-filed issues. Before fix: "0 project nodes" while
        // listing many. After fix: count matches reality.
        let mut g = Graph::new();
        for i in 1..=5 {
            let id = format!("iss-0{:02}", i);
            let title = format!("Issue {}", i);
            let mut n = Node::new(&id, &title);
            n.source = Some("manual".to_string());
            g.add_node(n);
        }
        let s = g.summary();
        assert_eq!(s.total_nodes, 5,
            "manual-only graph must report 5 project nodes, not 0 (ISS-047 regression)");
        assert_eq!(s.code_nodes, 0);
        assert_eq!(s.inferred_nodes, 0);
        assert_eq!(s.other_nodes, 0);
    }

    #[test]
    fn test_iss047_inferred_only_graph_surfaces_count() {
        // After `gid infer` on a fresh graph, only infer-source nodes exist.
        // They must appear in the summary header.
        let mut g = Graph::new();
        for i in 1..=3 {
            let id = format!("comp-{}", i);
            let title = format!("Component {}", i);
            let mut n = Node::new(&id, &title);
            n.source = Some("infer".to_string());
            g.add_node(n);
        }
        let s = g.summary();
        let rendered = format!("{}", s);
        assert_eq!(s.total_nodes, 0);
        assert_eq!(s.inferred_nodes, 3);
        assert!(rendered.contains("3 inferred nodes"),
            "expected '3 inferred nodes' in: {}", rendered);
    }
}

#[cfg(test)]
mod add_edge_dedup_tests {
    use super::*;

    #[test]
    fn test_new_edge_returns_true() {
        let mut g = Graph::new();
        g.add_node(Node::new("a", "A"));
        g.add_node(Node::new("b", "B"));
        let result = g.add_edge_dedup(Edge::new("a", "b", "depends_on"));
        assert!(result);
        assert_eq!(g.edges.len(), 1);
    }

    #[test]
    fn test_duplicate_returns_false() {
        let mut g = Graph::new();
        g.add_node(Node::new("a", "A"));
        g.add_node(Node::new("b", "B"));
        g.add_edge_dedup(Edge::new("a", "b", "depends_on"));
        let result = g.add_edge_dedup(Edge::new("a", "b", "depends_on"));
        assert!(!result);
        assert_eq!(g.edges.len(), 1);
    }

    #[test]
    fn test_same_from_to_different_relation() {
        let mut g = Graph::new();
        g.add_node(Node::new("a", "A"));
        g.add_node(Node::new("b", "B"));
        assert!(g.add_edge_dedup(Edge::new("a", "b", "depends_on")));
        assert!(g.add_edge_dedup(Edge::new("a", "b", "blocks")));
        assert_eq!(g.edges.len(), 2);
    }

    #[test]
    fn test_same_from_relation_different_to() {
        let mut g = Graph::new();
        g.add_node(Node::new("a", "A"));
        g.add_node(Node::new("b", "B"));
        g.add_node(Node::new("c", "C"));
        assert!(g.add_edge_dedup(Edge::new("a", "b", "depends_on")));
        assert!(g.add_edge_dedup(Edge::new("a", "c", "depends_on")));
        assert_eq!(g.edges.len(), 2);
    }
}

#[cfg(test)]
mod resolve_node_tests {
    use super::*;

    fn test_graph() -> Graph {
        let mut g = Graph::new();
        g.add_node(Node::new("task-auth", "Auth Module"));
        g.add_node(Node::new("feat:auth:login", "Login Feature"));
        g.add_node(Node::new("validate_auth_token", "Token Validator"));
        g.add_node(Node::new("file:src/main.rs", "Main Entry"));
        g.add_node(Node::new("impl-auth-middleware", "User Login Flow"));
        g.add_node(Node::new("task-db", "Database Setup"));
        g
    }

    #[test]
    fn test_exact_id_match() {
        let g = test_graph();
        let results = g.resolve_node("task-auth");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "task-auth");
    }

    #[test]
    fn test_exact_title_match_case_insensitive() {
        let g = test_graph();
        let results = g.resolve_node("auth module");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "task-auth");
    }

    #[test]
    fn test_structural_segment_colon() {
        let g = test_graph();
        // "auth" appears as a segment in "feat:auth:login" when split on ':'
        // But it also appears in "task-auth" split on '-', and "validate_auth_token" split on '_'
        // and in ID substrings. Tier 3 (structural) should find feat:auth:login and task-auth (split on '-')
        let results = g.resolve_node("login");
        // "login" is a structural segment of "feat:auth:login" (colon-split)
        assert!(results.iter().any(|n| n.id == "feat:auth:login"));
    }

    #[test]
    fn test_word_segment_underscore() {
        let g = test_graph();
        // "validate" splits on '_' in "validate_auth_token"
        let results = g.resolve_node("validate");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "validate_auth_token");
    }

    #[test]
    fn test_file_path_match() {
        let g = test_graph();
        let results = g.resolve_node("main.rs");
        assert!(results.iter().any(|n| n.id == "file:src/main.rs"));
    }

    #[test]
    fn test_title_substring() {
        let g = test_graph();
        // "Login Flow" is a substring of "User Login Flow"
        let results = g.resolve_node("Login Flow");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "impl-auth-middleware");
    }

    #[test]
    fn test_id_substring() {
        let g = test_graph();
        // "middleware" is a substring of "impl-auth-middleware"
        // But it's also a structural segment (split on '-'), so tier 3 catches it
        let results = g.resolve_node("middleware");
        assert!(results.iter().any(|n| n.id == "impl-auth-middleware"));
    }

    #[test]
    fn test_zero_matches() {
        let g = test_graph();
        let results = g.resolve_node("nonexistent_xyz");
        assert!(results.is_empty());
    }

    #[test]
    fn test_tier_priority() {
        // If query matches exact ID (tier 1), should NOT return title substring matches (tier 6)
        let mut g = Graph::new();
        g.add_node(Node::new("auth", "Something"));
        g.add_node(Node::new("other", "auth related"));
        let results = g.resolve_node("auth");
        // Only tier 1 (exact ID) should match
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "auth");
    }

    #[test]
    fn test_multiple_matches_same_tier() {
        let mut g = Graph::new();
        g.add_node(Node::new("node-1", "Auth Login"));
        g.add_node(Node::new("node-2", "Auth Signup"));
        // "auth" doesn't match any exact ID or title, structural segment matches both on '-' split? No.
        // "auth" as structural segment: "node-1" splits to ["node", "1"], "node-2" splits to ["node", "2"]
        // None match. Underscore split: no underscores. File path: no "file:" prefix.
        // Title substring (tier 6): both contain "auth" (case-insensitive)
        let results = g.resolve_node("auth");
        assert_eq!(results.len(), 2);
    }
}

#[cfg(test)]
mod add_feature_tests {
    use super::*;

    #[test]
    fn test_basic_feature_with_tasks() {
        let mut g = Graph::new();
        let tasks = vec![
            TaskSpec { title: "Design API".into(), status: None, tags: vec![], deps: vec![] },
            TaskSpec { title: "Write Tests".into(), status: None, tags: vec![], deps: vec![] },
        ];
        let feat_id = g.add_feature("User Auth", &tasks);
        assert_eq!(feat_id, "feat-user-auth");

        // Feature node exists
        let feat = g.get_node("feat-user-auth").unwrap();
        assert_eq!(feat.title, "User Auth");
        assert_eq!(feat.node_type.as_deref(), Some("feature"));
        assert_eq!(feat.status, NodeStatus::Todo);

        // Task nodes exist
        assert!(g.get_node("task-user-auth-design-api").is_some());
        assert!(g.get_node("task-user-auth-write-tests").is_some());

        // Implements edges exist
        let implements: Vec<_> = g.edges.iter()
            .filter(|e| e.relation == "implements" && e.to == "feat-user-auth")
            .collect();
        assert_eq!(implements.len(), 2);
    }

    #[test]
    fn test_feature_with_deps() {
        let mut g = Graph::new();
        let tasks = vec![
            TaskSpec { title: "Setup DB".into(), status: None, tags: vec![], deps: vec![] },
            TaskSpec { title: "Write Schema".into(), status: None, tags: vec![], deps: vec!["Setup DB".into()] },
            TaskSpec { title: "Add Migrations".into(), status: None, tags: vec![], deps: vec!["Write Schema".into()] },
        ];
        let feat_id = g.add_feature("Database", &tasks);
        assert_eq!(feat_id, "feat-database");

        // depends_on edges
        let deps: Vec<_> = g.edges.iter()
            .filter(|e| e.relation == "depends_on")
            .collect();
        assert_eq!(deps.len(), 2);

        // Write Schema depends on Setup DB
        assert!(g.edges.iter().any(|e| {
            e.from == "task-database-write-schema" && e.to == "task-database-setup-db" && e.relation == "depends_on"
        }));

        // Add Migrations depends on Write Schema
        assert!(g.edges.iter().any(|e| {
            e.from == "task-database-add-migrations" && e.to == "task-database-write-schema" && e.relation == "depends_on"
        }));
    }

    #[test]
    fn test_feature_id_collision() {
        let mut g = Graph::new();
        let tasks = vec![
            TaskSpec { title: "Task A".into(), status: None, tags: vec![], deps: vec![] },
        ];
        let id1 = g.add_feature("Auth", &tasks);
        assert_eq!(id1, "feat-auth");

        let id2 = g.add_feature("Auth", &[]);
        assert_eq!(id2, "feat-auth-2");

        // Both exist
        assert!(g.get_node("feat-auth").is_some());
        assert!(g.get_node("feat-auth-2").is_some());
    }
}

#[cfg(test)]
mod add_task_tests {
    use super::*;

    #[test]
    fn test_standalone_task() {
        let mut g = Graph::new();
        let task_id = g.add_task("Fix login bug", None, &[], &[], None);
        assert_eq!(task_id, "task-fix-login-bug");

        let node = g.get_node(&task_id).unwrap();
        assert_eq!(node.title, "Fix login bug");
        assert_eq!(node.node_type.as_deref(), Some("task"));
        assert_eq!(node.status, NodeStatus::Todo);

        // No edges
        assert!(g.edges.is_empty());
    }

    #[test]
    fn test_task_with_feature() {
        let mut g = Graph::new();
        // Create a feature first
        g.add_feature("Auth", &[]);

        let task_id = g.add_task("Add OAuth", Some("feat-auth"), &[], &["backend".into()], Some(1));
        assert_eq!(task_id, "task-auth-add-oauth");

        let node = g.get_node(&task_id).unwrap();
        assert_eq!(node.tags, vec!["backend".to_string()]);
        assert_eq!(node.priority, Some(1));

        // implements edge
        assert!(g.edges.iter().any(|e| {
            e.from == "task-auth-add-oauth" && e.to == "feat-auth" && e.relation == "implements"
        }));
    }

    #[test]
    fn test_task_with_deps() {
        let mut g = Graph::new();
        // Create some nodes to depend on
        g.add_node(Node::new("task-setup", "Setup Environment"));
        g.add_node(Node::new("task-config", "Write Config"));

        let task_id = g.add_task("Deploy App", None, &["task-setup".into(), "Write Config".into()], &[], None);
        assert_eq!(task_id, "task-deploy-app");

        // depends_on edges
        let deps: Vec<_> = g.edges.iter()
            .filter(|e| e.from == "task-deploy-app" && e.relation == "depends_on")
            .collect();
        assert_eq!(deps.len(), 2);
    }
}

#[cfg(test)]
mod merge_feature_nodes_tests {
    use super::*;

    #[test]
    fn test_basic_merge() {
        let mut g = Graph::new();
        // Create feature with tasks
        g.add_feature("Auth", &[
            TaskSpec { title: "Old Task 1".into(), status: None, tags: vec![], deps: vec![] },
            TaskSpec { title: "Old Task 2".into(), status: None, tags: vec![], deps: vec![] },
        ]);

        // Build incoming graph with new tasks
        let mut incoming = Graph::new();
        incoming.add_node({
            let mut n = Node::new("new-task-a", "New Task A");
            n.node_type = Some("task".into());
            n
        });
        incoming.add_node({
            let mut n = Node::new("new-task-b", "New Task B");
            n.node_type = Some("task".into());
            n
        });

        let (removed, added) = g.merge_feature_nodes("feat-auth", incoming);
        assert_eq!(removed, 2);
        assert_eq!(added, 2);

        // Old tasks gone
        assert!(g.get_node("task-auth-old-task-1").is_none());
        assert!(g.get_node("task-auth-old-task-2").is_none());

        // New tasks present
        assert!(g.get_node("new-task-a").is_some());
        assert!(g.get_node("new-task-b").is_some());

        // Feature still exists
        assert!(g.get_node("feat-auth").is_some());

        // New implements edges
        let implements: Vec<_> = g.edges.iter()
            .filter(|e| e.relation == "implements" && e.to == "feat-auth")
            .collect();
        assert_eq!(implements.len(), 2);
    }

    #[test]
    fn test_edge_cascade() {
        let mut g = Graph::new();
        g.add_feature("Auth", &[
            TaskSpec { title: "Task X".into(), status: None, tags: vec![], deps: vec![] },
        ]);

        // Add extra edge to the old task
        g.add_edge(Edge::new("task-auth-task-x", "some-other-node", "related_to"));
        g.add_node(Node::new("some-other-node", "Other"));

        // Before merge: task-auth-task-x has edges
        assert!(g.edges.iter().any(|e| e.from == "task-auth-task-x"));

        let (removed, _added) = g.merge_feature_nodes("feat-auth", Graph::new());

        assert_eq!(removed, 1);
        // Old task and all its edges removed
        assert!(g.get_node("task-auth-task-x").is_none());
        assert!(!g.edges.iter().any(|e| e.from == "task-auth-task-x" || e.to == "task-auth-task-x"));
    }

    #[test]
    fn test_empty_merge() {
        let mut g = Graph::new();
        g.add_feature("Auth", &[
            TaskSpec { title: "Task 1".into(), status: None, tags: vec![], deps: vec![] },
            TaskSpec { title: "Task 2".into(), status: None, tags: vec![], deps: vec![] },
        ]);

        let (removed, added) = g.merge_feature_nodes("feat-auth", Graph::new());
        assert_eq!(removed, 2);
        assert_eq!(added, 0);

        // Feature still exists
        assert!(g.get_node("feat-auth").is_some());
        // No implements edges left
        let implements: Vec<_> = g.edges.iter()
            .filter(|e| e.relation == "implements" && e.to == "feat-auth")
            .collect();
        assert_eq!(implements.len(), 0);
    }

    #[test]
    fn test_edge_dedup_on_merge() {
        let mut g = Graph::new();
        g.add_feature("Auth", &[]);

        let mut incoming = Graph::new();
        incoming.add_node({
            let mut n = Node::new("task-new", "New Task");
            n.node_type = Some("task".into());
            n
        });

        // First merge
        g.merge_feature_nodes("feat-auth", incoming.clone());

        // Second merge of same nodes (simulate re-merge)
        // Remove the node first so add_node can re-add it
        g.remove_node("task-new");
        let mut incoming2 = Graph::new();
        incoming2.add_node({
            let mut n = Node::new("task-new", "New Task");
            n.node_type = Some("task".into());
            n
        });
        g.merge_feature_nodes("feat-auth", incoming2);

        // Should not have duplicate implements edges
        let implements: Vec<_> = g.edges.iter()
            .filter(|e| e.from == "task-new" && e.to == "feat-auth" && e.relation == "implements")
            .collect();
        assert_eq!(implements.len(), 1);
    }

    #[test]
    fn test_infer_node_type_known_prefixes() {
        assert_eq!(infer_node_type("file:src/main.rs"), Some("file"));
        assert_eq!(infer_node_type("fn:my_func"), Some("function"));
        assert_eq!(infer_node_type("func:my_func"), Some("function"));
        assert_eq!(infer_node_type("struct:MyStruct"), Some("class"));
        assert_eq!(infer_node_type("class:MyClass"), Some("class"));
        assert_eq!(infer_node_type("mod:mymod"), Some("module"));
        assert_eq!(infer_node_type("module:mymod"), Some("module"));
        assert_eq!(infer_node_type("method:do_thing"), Some("method"));
        assert_eq!(infer_node_type("trait:MyTrait"), Some("trait"));
        assert_eq!(infer_node_type("interface:IFoo"), Some("trait"));
        assert_eq!(infer_node_type("enum:Color"), Some("enum"));
        assert_eq!(infer_node_type("const:MAX_SIZE"), Some("constant"));
        assert_eq!(infer_node_type("static:INSTANCE"), Some("constant"));
        assert_eq!(infer_node_type("test:test_foo"), Some("test"));
        assert_eq!(infer_node_type("impl:MyStruct"), Some("impl"));
    }

    #[test]
    fn test_infer_node_type_unknown_prefix() {
        assert_eq!(infer_node_type("task-auth-login"), None);
        assert_eq!(infer_node_type("feat-pipeline"), None);
        assert_eq!(infer_node_type("random-id"), None);
        assert_eq!(infer_node_type(""), None);
    }

    #[test]
    fn test_infer_node_type_no_colon() {
        // Without a colon the whole ID is the "prefix" — should not match known types
        // unless the ID itself happens to be e.g. "file" (unlikely but valid)
        assert_eq!(infer_node_type("file"), Some("file"));
        assert_eq!(infer_node_type("something"), None);
    }

    #[test]
    fn test_add_node_auto_infers_type() {
        let mut g = Graph::new();
        let node = Node::new("fn:process_data", "Process Data");
        assert!(node.node_type.is_none()); // not set on Node::new
        g.add_node(node);
        let added = g.get_node("fn:process_data").unwrap();
        assert_eq!(added.node_type.as_deref(), Some("function"));
    }

    #[test]
    fn test_add_node_does_not_override_explicit_type() {
        let mut g = Graph::new();
        let mut node = Node::new("fn:process_data", "Process Data");
        node.node_type = Some("custom".to_string());
        g.add_node(node);
        let added = g.get_node("fn:process_data").unwrap();
        assert_eq!(added.node_type.as_deref(), Some("custom"));
    }

    #[test]
    fn test_add_node_no_infer_for_unknown_prefix() {
        let mut g = Graph::new();
        let node = Node::new("task-auth-login", "Login task");
        g.add_node(node);
        let added = g.get_node("task-auth-login").unwrap();
        assert!(added.node_type.is_none());
    }
}
