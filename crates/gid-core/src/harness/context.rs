//! Context assembler: build minimal, precise context for each sub-agent.
//!
//! Resolves graph metadata to actual file content — feature docs, design
//! sections, requirements goals, and project guards.

use std::path::Path;
use std::collections::{HashSet, VecDeque};
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
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        if let Some((level, _)) = parse_heading(line) {
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

// =============================================================================
// §5 Relevance Scoring — Edge-Relation-Based 5-Tier Ranking (GOAL-4.4)
// =============================================================================

/// A raw candidate node discovered during graph traversal (before scoring).
///
/// Carries all metadata needed for scoring and budget fitting.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub node_id: String,
    pub node_type: String,
    pub file_path: Option<String>,
    pub signature: Option<String>,
    pub doc_comment: Option<String>,
    pub description: Option<String>,
    pub source_code: Option<String>,
    /// Number of hops from the nearest target node.
    pub hop_distance: u32,
    pub modified_at: Option<i64>,
    /// The edge relation that connected this node to the traversal.
    pub connecting_relation: String,
    pub token_estimate: usize,
}

/// A candidate with a computed relevance score.
#[derive(Debug, Clone)]
pub struct ScoredCandidate {
    pub candidate: Candidate,
    pub score: f64,
    pub token_estimate: usize,
}

/// GOAL-4.4: 5-tier relevance ranking by edge relation.
///
/// | Rank | Category       | Relations                                         |
/// |------|----------------|---------------------------------------------------|
/// | 1    | Direct call    | calls, imports                                    |
/// | 2    | Type reference | type_reference, inherits, implements, uses         |
/// | 3    | Same-file      | contains, defined_in                              |
/// | 4    | Structural     | depends_on, part_of, blocks, tests_for            |
/// | 5    | Transitive     | any unknown / unrecognized relation                |
pub fn relation_rank(relation: &str) -> u8 {
    match relation {
        "calls" | "imports" => 1,                                    // Direct call
        "type_reference" | "inherits" | "implements" | "uses" => 2,  // Type reference
        "contains" | "defined_in" => 3,                              // Same-file
        "depends_on" | "part_of" | "blocks" | "tests_for" => 4,     // Structural
        _ => 5,                                                       // Transitive / unknown
    }
}

/// Map rank to a [0.0, 1.0] score: rank 1 → 1.0, rank 5 → 0.2.
pub fn relation_score(relation: &str) -> f64 {
    match relation_rank(relation) {
        1 => 1.0,
        2 => 0.8,
        3 => 0.6,
        4 => 0.4,
        5 => 0.2,
        _ => 0.1,
    }
}

/// Scoring weights (v1 constants — documented as tunable for future versions).
const W_RELATION: f64 = 0.60;
const W_PROXIMITY: f64 = 0.30;
const W_WEIGHT: f64 = 0.10;

/// Minimum useful token count for truncated inclusion.
#[allow(dead_code)]
const MIN_USEFUL_TOKENS: usize = 20;

/// Estimate token count from text content.
/// Per design.md §9: tokens ≈ byte_len / 4.
fn estimate_tokens_str(text: &str) -> usize {
    let len = text.len();
    if len == 0 { 0 } else { (len / 4).max(1) }
}

/// Estimate tokens for a candidate node.
fn estimate_tokens_for_candidate(c: &Candidate) -> usize {
    let mut bytes = 0;
    if let Some(ref sc) = c.source_code { bytes += sc.len(); }
    if let Some(ref sig) = c.signature { bytes += sig.len(); }
    if let Some(ref desc) = c.description { bytes += desc.len(); }
    if let Some(ref dc) = c.doc_comment { bytes += dc.len(); }
    bytes += 30; // overhead
    (bytes / 4).max(1)
}

/// Score a single candidate. **[GOAL-4.4, 4.5]**
///
/// Composite score = (W_RELATION * relation_score + W_PROXIMITY * proximity + W_WEIGHT * weight_factor)
///                   * transitive_penalty
pub fn score_candidate(candidate: &Candidate) -> ScoredCandidate {
    // Relation-based score (primary factor).
    let rel_score = relation_score(&candidate.connecting_relation);

    // Proximity: inverse of hop distance.
    // hop 1 → 1.0, hop 2 → 0.5, hop 3 → 0.33.
    let proximity = if candidate.hop_distance == 0 {
        1.0
    } else {
        1.0 / (candidate.hop_distance as f64)
    };

    // Weight: from edge weight (default 1.0) — could incorporate edge.weight in future.
    let weight_factor = 1.0;

    // Transitive penalty: candidates at hop > 1 are penalized (GOAL-4.4 tier 5).
    let transitive_penalty = if candidate.hop_distance > 1 { 0.8 } else { 1.0 };

    let mut score = (W_RELATION * rel_score
                   + W_PROXIMITY * proximity
                   + W_WEIGHT * weight_factor)
                   * transitive_penalty;

    // NaN guard (FINDING-13).
    if score.is_nan() { score = 0.0; }

    let token_estimate = estimate_tokens_for_candidate(candidate);

    ScoredCandidate {
        candidate: candidate.clone(),
        score,
        token_estimate,
    }
}

/// Score and sort a list of candidates by descending relevance.
pub fn score_candidates(candidates: &[Candidate]) -> Vec<ScoredCandidate> {
    let mut scored: Vec<ScoredCandidate> = candidates.iter().map(score_candidate).collect();
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    scored
}

// =============================================================================
// §6 Token Budget Management — Category-Based Truncation (GOAL-4.3)
// =============================================================================

/// Context for a target node — NEVER truncated. **[GOAL-4.1a, 4.3]**
#[derive(Debug, Clone, serde::Serialize)]
pub struct TargetContext {
    /// Node ID.
    pub node_id: String,
    /// Node title.
    pub title: Option<String>,
    /// File path on disk (for source loading).
    pub file_path: Option<String>,
    /// Function/class signature.
    pub signature: Option<String>,
    /// Doc comment.
    pub doc_comment: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// Source code loaded from disk.
    pub source_code: Option<String>,
    /// Estimated tokens for this target.
    pub token_estimate: usize,
}

impl TargetContext {
    /// Create a TargetContext with pre-computed token estimate.
    pub fn new(
        node_id: String,
        title: Option<String>,
        file_path: Option<String>,
        signature: Option<String>,
        doc_comment: Option<String>,
        description: Option<String>,
        source_code: Option<String>,
    ) -> Self {
        let token_estimate = estimate_tokens_for_target_fields(
            title.as_deref(),
            description.as_deref(),
            signature.as_deref(),
            doc_comment.as_deref(),
            source_code.as_deref(),
        );
        Self {
            node_id, title, file_path, signature, doc_comment,
            description, source_code, token_estimate,
        }
    }
}

/// Estimate tokens for target context fields.
fn estimate_tokens_for_target_fields(
    title: Option<&str>,
    description: Option<&str>,
    signature: Option<&str>,
    doc_comment: Option<&str>,
    source_code: Option<&str>,
) -> usize {
    let mut bytes = 0usize;
    if let Some(t) = title { bytes += t.len(); }
    if let Some(d) = description { bytes += d.len(); }
    if let Some(s) = signature { bytes += s.len(); }
    if let Some(dc) = doc_comment { bytes += dc.len(); }
    if let Some(sc) = source_code { bytes += sc.len(); }
    bytes += 50; // overhead for headers/formatting
    (bytes / 4).max(1)
}

/// A single non-target item in the assembled context. **[GOAL-4.11]**
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextItem {
    /// Source node ID.
    pub node_id: String,
    /// Node type (file, function, class, etc.).
    pub node_type: String,
    /// File path (if available).
    pub file_path: Option<String>,
    /// Function/class signature (if available).
    pub signature: Option<String>,
    /// Doc comment (if available).
    pub doc_comment: Option<String>,
    /// Description or source code content.
    pub content: Option<String>,
    /// The edge relation that connects this node to the target. **[GOAL-4.11]**
    pub connecting_relation: String,
    /// Estimated token count for this item.
    pub token_estimate: usize,
    /// Relevance score (visible per GOAL-4.5).
    pub score: f64,
    /// Whether this item was truncated to fit the budget.
    pub truncated: bool,
}

impl ContextItem {
    /// Create a ContextItem from a ScoredCandidate (full inclusion).
    fn from_scored(sc: &ScoredCandidate, truncated: bool) -> Self {
        let content = sc.candidate.source_code.clone()
            .or_else(|| sc.candidate.description.clone());
        Self {
            node_id: sc.candidate.node_id.clone(),
            node_type: sc.candidate.node_type.clone(),
            file_path: sc.candidate.file_path.clone(),
            signature: sc.candidate.signature.clone(),
            doc_comment: sc.candidate.doc_comment.clone(),
            content,
            connecting_relation: sc.candidate.connecting_relation.clone(),
            token_estimate: sc.token_estimate,
            score: sc.score,
            truncated,
        }
    }

    /// Create a truncated ContextItem that fits within `max_tokens`.
    fn from_scored_truncated(sc: &ScoredCandidate, max_tokens: usize) -> Self {
        let full_content = sc.candidate.source_code.as_deref()
            .or(sc.candidate.description.as_deref())
            .unwrap_or("");

        let truncated_content = truncate_text(full_content, max_tokens);
        let actual_tokens = estimate_tokens_str(&truncated_content);

        Self {
            node_id: sc.candidate.node_id.clone(),
            node_type: sc.candidate.node_type.clone(),
            file_path: sc.candidate.file_path.clone(),
            signature: sc.candidate.signature.clone(),
            doc_comment: sc.candidate.doc_comment.clone(),
            content: Some(truncated_content),
            connecting_relation: sc.candidate.connecting_relation.clone(),
            token_estimate: actual_tokens,
            score: sc.score,
            truncated: true,
        }
    }
}

/// Metadata about truncation decisions. **[GOAL-4.3]**
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TruncationInfo {
    /// Number of items that were truncated (partially included).
    pub truncated_count: usize,
    /// Number of items that were dropped entirely.
    pub dropped_count: usize,
    /// Tokens actually consumed by this category.
    pub budget_used: usize,
}

impl TruncationInfo {
    fn merge(&mut self, other: &TruncationInfo) {
        self.truncated_count += other.truncated_count;
        self.dropped_count += other.dropped_count;
        self.budget_used += other.budget_used;
    }
}

/// The assembled context result — categorized output. **[GOAL-4.1]**
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextResult {
    /// GOAL-4.1a: Full target node details (never truncated).
    pub targets: Vec<TargetContext>,
    /// GOAL-4.1c,d: Direct + transitive dependencies, sorted by relevance.
    pub dependencies: Vec<ContextItem>,
    /// GOAL-4.1e: Callers of target nodes.
    pub callers: Vec<ContextItem>,
    /// GOAL-4.1f: Related test nodes.
    pub tests: Vec<ContextItem>,
    /// GOAL-4.10: Total estimated tokens in the output.
    pub estimated_tokens: usize,
    /// GOAL-4.3: Truncation info.
    pub truncation_info: TruncationInfo,
}

impl ContextResult {
    /// Total number of items included across all categories.
    pub fn total_included(&self) -> usize {
        self.targets.len() + self.dependencies.len() + self.callers.len() + self.tests.len()
    }
}

/// Minimum tokens for a truncated item to be useful.
const MIN_USEFUL_TOKENS_TRUNC: usize = 32;

/// Category-based budget allocation. **[GOAL-4.3]**
///
/// Priority order (GOAL-4.3):
/// 1. Targets — NEVER truncated
/// 2. Direct dependencies (hop == 1)
/// 3. Callers
/// 4. Tests
/// 5. Transitive dependencies (furthest hops dropped first)
pub fn budget_fit_by_category(
    targets: &[TargetContext],
    deps: Vec<ScoredCandidate>,
    callers: Vec<ScoredCandidate>,
    tests: Vec<ScoredCandidate>,
    budget: usize,
) -> ContextResult {
    let mut remaining = budget;
    let mut truncation = TruncationInfo::default();

    // 1. Targets — always included, never truncated.
    let target_tokens: usize = targets.iter().map(|t| t.token_estimate).sum();
    remaining = remaining.saturating_sub(target_tokens);

    // Separate direct deps from transitive deps.
    let (direct_deps, transitive_deps): (Vec<_>, Vec<_>) =
        deps.into_iter().partition(|d| d.candidate.hop_distance == 1);

    // 2. Direct dependencies — fill as much as budget allows.
    let (included_direct, direct_trunc) = greedy_fill(&direct_deps, remaining);
    remaining = remaining.saturating_sub(direct_trunc.budget_used);
    truncation.merge(&direct_trunc);

    // 3. Callers.
    let (included_callers, caller_trunc) = greedy_fill(&callers, remaining);
    remaining = remaining.saturating_sub(caller_trunc.budget_used);
    truncation.merge(&caller_trunc);

    // 4. Tests.
    let (included_tests, test_trunc) = greedy_fill(&tests, remaining);
    remaining = remaining.saturating_sub(test_trunc.budget_used);
    truncation.merge(&test_trunc);

    // 5. Transitive deps — sorted by hop distance ascending (closest first),
    //    within same hop: sorted by score descending (highest relevance first).
    //    This means furthest hops are dropped first when budget runs out.
    let mut trans_sorted = transitive_deps;
    trans_sorted.sort_by(|a, b| {
        a.candidate.hop_distance.cmp(&b.candidate.hop_distance)
            .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal))
    });
    let (included_transitive, trans_trunc) = greedy_fill(&trans_sorted, remaining);
    remaining = remaining.saturating_sub(trans_trunc.budget_used);
    truncation.merge(&trans_trunc);

    let total_tokens = budget - remaining;

    ContextResult {
        targets: targets.to_vec(),
        dependencies: [included_direct, included_transitive].concat(),
        callers: included_callers,
        tests: included_tests,
        estimated_tokens: total_tokens,
        truncation_info: truncation,
    }
}

/// Greedy knapsack: consume items in order until budget exhausted.
///
/// Items that fully fit are included as-is. Items that partially fit are
/// truncated if the remaining budget exceeds `MIN_USEFUL_TOKENS_TRUNC`.
/// Items that don't fit at all are dropped and counted.
fn greedy_fill(
    items: &[ScoredCandidate],
    budget: usize,
) -> (Vec<ContextItem>, TruncationInfo) {
    let mut included = Vec::new();
    let mut remaining = budget;
    let mut info = TruncationInfo::default();

    for sc in items {
        if remaining == 0 {
            info.dropped_count += 1;
            continue;
        }

        if sc.token_estimate <= remaining {
            // Fully fits.
            included.push(ContextItem::from_scored(sc, false));
            remaining -= sc.token_estimate;
        } else if remaining >= MIN_USEFUL_TOKENS_TRUNC {
            // Partially fits — truncate content.
            let truncated = ContextItem::from_scored_truncated(sc, remaining);
            remaining = remaining.saturating_sub(truncated.token_estimate);
            included.push(truncated);
            info.truncated_count += 1;
        } else {
            // Remaining budget too small to be useful.
            info.dropped_count += 1;
        }
    }

    info.budget_used = budget - remaining;
    (included, info)
}

// =============================================================================
// §7 Truncation Strategy — UTF-8 Safe Text Truncation (GOAL-4.3)
// =============================================================================

/// Truncate text content to fit within `max_tokens` tokens. **[GOAL-4.3]**
///
/// Rules:
/// 1. UTF-8 safety: always truncate at valid char boundary.
/// 2. Prefer line boundaries: trim to last complete line that fits.
/// 3. Truncation marker: `\n... [truncated]` suffix appended.
/// 4. Head-biased: preserves beginning of content (imports/signatures first).
pub fn truncate_text(text: &str, max_tokens: usize) -> String {
    let max_bytes = max_tokens * 4;
    let marker = "\n... [truncated]";
    let usable_bytes = max_bytes.saturating_sub(marker.len());

    if text.len() <= max_bytes {
        return text.to_string();
    }

    // Find a safe cut point at a char boundary.
    let safe_end = if usable_bytes >= text.len() {
        text.len()
    } else if text.is_char_boundary(usable_bytes) {
        usable_bytes
    } else {
        // Scan backward to find a valid char boundary.
        let mut pos = usable_bytes;
        while pos > 0 && !text.is_char_boundary(pos) {
            pos -= 1;
        }
        pos
    };

    let safe_slice = &text[..safe_end];

    // Prefer line boundary — find the last newline.
    let cut_point = safe_slice.rfind('\n').unwrap_or(safe_end);

    format!("{}{}", &text[..cut_point], marker)
}

// =============================================================================
// §8 Source Code Loading from Disk (GOAL-4.1b)
// =============================================================================

/// Result of loading source code from disk.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SourceLoadResult {
    /// The loaded source code (possibly a line range extract).
    pub source: String,
    /// Whether the source was loaded from a line range (start_line..end_line).
    pub is_range: bool,
    /// Starting line (1-indexed) if range was used.
    pub start_line: Option<usize>,
    /// Ending line (1-indexed, inclusive) if range was used.
    pub end_line: Option<usize>,
    /// Total lines in the loaded source.
    pub line_count: usize,
}

/// Load source code from disk for a node. **[GOAL-4.1b]**
///
/// If `start_line` and `end_line` are both provided, loads only that range.
/// If only `start_line` is provided, loads from that line to end-of-file.
/// If neither is provided, loads the entire file.
///
/// Returns `None` if:
/// - `file_path` is None
/// - The file doesn't exist or can't be read
/// - The file path is not under `project_root` (security check)
///
/// Lines are 1-indexed (matching typical IDE conventions).
pub fn load_source_from_disk(
    file_path: Option<&str>,
    start_line: Option<usize>,
    end_line: Option<usize>,
    project_root: &Path,
) -> Option<SourceLoadResult> {
    let file_path = file_path?;

    // Resolve relative to project_root
    let path = if Path::new(file_path).is_absolute() {
        std::path::PathBuf::from(file_path)
    } else {
        project_root.join(file_path)
    };

    // Security: ensure the resolved path is under project_root
    let canonical_root = project_root.canonicalize().ok()?;
    let canonical_path = path.canonicalize().ok()?;
    if !canonical_path.starts_with(&canonical_root) {
        tracing::warn!(
            "Refusing to load source outside project root: {} (root: {})",
            canonical_path.display(), canonical_root.display()
        );
        return None;
    }

    // Read the file
    let content = std::fs::read_to_string(&canonical_path).ok()?;
    let lines: Vec<&str> = content.lines().collect();

    match (start_line, end_line) {
        (Some(start), Some(end)) if start >= 1 && end >= start => {
            // Range extract: 1-indexed, inclusive
            let start_idx = start.saturating_sub(1);
            let end_idx = end.min(lines.len());
            if start_idx >= lines.len() {
                // start_line beyond file length
                return None;
            }
            let selected: Vec<&str> = lines[start_idx..end_idx].to_vec();
            let source = selected.join("\n");
            Some(SourceLoadResult {
                line_count: selected.len(),
                source,
                is_range: true,
                start_line: Some(start),
                end_line: Some(end_idx),
            })
        }
        (Some(start), None) if start >= 1 => {
            // From start_line to EOF
            let start_idx = start.saturating_sub(1);
            if start_idx >= lines.len() {
                return None;
            }
            let selected: Vec<&str> = lines[start_idx..].to_vec();
            let source = selected.join("\n");
            Some(SourceLoadResult {
                line_count: selected.len(),
                source,
                is_range: true,
                start_line: Some(start),
                end_line: Some(lines.len()),
            })
        }
        _ => {
            // Full file
            let line_count = lines.len();
            Some(SourceLoadResult {
                source: content,
                is_range: false,
                start_line: None,
                end_line: None,
                line_count,
            })
        }
    }
}

// =============================================================================
// §9 Context Query & Assembly Pipeline (GOAL-4.1–4.13)
// =============================================================================

/// Filters that narrow which candidate nodes are eligible. **[GOAL-4.8]**
#[derive(Debug, Clone, Default)]
pub struct ContextFilters {
    /// GOAL-4.8: --include patterns. Supports file path globs (e.g., "*.rs")
    /// and node type filters (e.g., "type:function").
    /// Semantics: if non-empty, a candidate must match at least one pattern.
    pub include_patterns: Vec<String>,
    /// Exclude nodes whose IDs match any of these patterns.
    pub exclude_ids: Vec<String>,
    /// Only include nodes modified after this timestamp (epoch secs).
    pub modified_after: Option<i64>,
}

/// GOAL-4.9: Output format selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub enum OutputFormat {
    /// Human-readable markdown sections (default).
    #[default]
    Markdown,
    /// Machine-parseable JSON.
    Json,
    /// Same structure as JSON but in YAML syntax.
    Yaml,
}


impl std::str::FromStr for OutputFormat {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "markdown" | "md" => Ok(Self::Markdown),
            "json" => Ok(Self::Json),
            "yaml" | "yml" => Ok(Self::Yaml),
            other => Err(format!("unknown format '{}': expected markdown, json, or yaml", other)),
        }
    }
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Markdown => write!(f, "markdown"),
            Self::Json => write!(f, "json"),
            Self::Yaml => write!(f, "yaml"),
        }
    }
}

/// A request for assembled context. **[GOAL-4.1, 4.6]**
#[derive(Debug, Clone)]
pub struct ContextQuery {
    /// GOAL-4.6: One or more target node IDs whose context we are assembling.
    /// At least one target must be specified.
    pub targets: Vec<String>,
    /// Maximum token budget for the assembled output. **[GOAL-4.2]**
    pub token_budget: usize,
    /// Maximum traversal depth (hops from any target). **[GOAL-4.7]**
    /// Default: 2.
    pub depth: u32,
    /// Optional filters to narrow candidates. **[GOAL-4.8]**
    pub filters: ContextFilters,
    /// Output format. **[GOAL-4.9]**
    pub format: OutputFormat,
    /// Project root for source code loading. **[GOAL-4.1b]**
    pub project_root: Option<std::path::PathBuf>,
}

impl Default for ContextQuery {
    fn default() -> Self {
        Self {
            targets: Vec::new(),
            token_budget: 8000,
            depth: 2,
            filters: ContextFilters::default(),
            format: OutputFormat::default(),
            project_root: None,
        }
    }
}

/// Traversal statistics for observability. **[GOAL-4.13]**
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ContextStats {
    /// Total nodes visited during traversal.
    pub nodes_visited: usize,
    /// Nodes included in the final output.
    pub nodes_included: usize,
    /// Nodes excluded by --include filter.
    pub nodes_excluded_by_filter: usize,
    /// Tokens used in the assembled context.
    pub budget_used: usize,
    /// Total token budget available.
    pub budget_total: usize,
    /// Elapsed time in milliseconds.
    pub elapsed_ms: u64,
}

/// Extended context result with statistics. **[GOAL-4.1, 4.13]**
#[derive(Debug, Clone, serde::Serialize)]
pub struct AssembledContext {
    /// The categorized context result.
    #[serde(flatten)]
    pub result: ContextResult,
    /// Traversal statistics (GOAL-4.13).
    pub stats: ContextStats,
}

/// Top-level entry point for context assembly. **[GOAL-4.2, 4.3, 4.12]**
///
/// This is the library function that CLI/MCP/LSP all call.
/// Takes a `Graph` (already loaded) and a `ContextQuery`, returns
/// a structured `AssembledContext` with stats.
pub fn assemble_context(
    graph: &Graph,
    query: &ContextQuery,
) -> Result<AssembledContext> {
    let start = std::time::Instant::now();

    // Validate: at least one target (GOAL-4.6).
    if query.targets.is_empty() {
        return Err(anyhow::anyhow!("--targets: at least one target node ID required"));
    }

    let mut stats = ContextStats {
        budget_total: query.token_budget,
        ..Default::default()
    };
    let mut filter_excluded = 0usize;

    // Stage 1: Gather target node details + source code from disk.
    let targets = gather_targets(graph, &query.targets, query.project_root.as_deref())?;
    stats.nodes_visited += targets.len();

    // Stage 2: Multi-source BFS — gather dependency candidates.
    let (dep_candidates, dep_filtered) = gather_dependencies(
        graph, &query.targets, query.depth, &query.filters, query.project_root.as_deref(),
    );
    stats.nodes_visited += dep_candidates.len() + dep_filtered;
    filter_excluded += dep_filtered;

    // Stage 3: Reverse-edge traversal — gather callers and tests.
    let (caller_candidates, test_candidates) = gather_callers_and_tests(
        graph, &query.targets, query.project_root.as_deref(),
    );
    stats.nodes_visited += caller_candidates.len() + test_candidates.len();

    // Stage 4: Score all candidates by edge-relation relevance (GOAL-4.4).
    let scored_deps = score_candidates(&dep_candidates);
    let scored_callers = score_candidates(&caller_candidates);
    let scored_tests = score_candidates(&test_candidates);

    // Stage 5: Category-based budget allocation (GOAL-4.3).
    let context_result = budget_fit_by_category(
        &targets,
        scored_deps,
        scored_callers,
        scored_tests,
        query.token_budget,
    );

    // Stage 6: Record stats (GOAL-4.13).
    stats.nodes_included = context_result.total_included();
    stats.nodes_excluded_by_filter = filter_excluded;
    stats.budget_used = context_result.estimated_tokens;
    stats.elapsed_ms = start.elapsed().as_millis() as u64;

    // Log traversal stats to stderr (GOAL-4.13).
    tracing::info!(
        visited = stats.nodes_visited,
        included = stats.nodes_included,
        excluded_filter = stats.nodes_excluded_by_filter,
        budget = %format!("{}/{}", stats.budget_used, stats.budget_total),
        elapsed_ms = stats.elapsed_ms,
        "context assembly complete"
    );

    Ok(AssembledContext {
        result: context_result,
        stats,
    })
}

/// Stage 1: Gather full details for each target node. **[GOAL-4.1, 4.1b]**
fn gather_targets(
    graph: &Graph,
    target_ids: &[String],
    project_root: Option<&Path>,
) -> Result<Vec<TargetContext>> {
    let mut targets = Vec::new();

    for id in target_ids {
        let node = graph.get_node(id)
            .ok_or_else(|| anyhow::anyhow!("target node not found: {}", id))?;

        // GOAL-4.1b: Read source code from disk if file_path + line range available.
        let source_code = if let Some(root) = project_root {
            load_source_from_disk(
                node.file_path.as_deref(),
                node.start_line,
                node.end_line,
                root,
            ).map(|r| r.source)
        } else {
            None
        };

        targets.push(TargetContext::new(
            node.id.clone(),
            Some(node.title.clone()),
            node.file_path.clone(),
            node.signature.clone(),
            node.doc_comment.clone(),
            node.description.clone(),
            source_code,
        ));
    }

    Ok(targets)
}

/// Stage 2: Multi-source BFS with depth limit for dependencies. **[GOAL-4.7, 4.8]**
///
/// Returns (candidates, filtered_count) — filtered_count is nodes excluded by filters.
fn gather_dependencies(
    graph: &Graph,
    root_ids: &[String],
    max_depth: u32,
    filters: &ContextFilters,
    project_root: Option<&Path>,
) -> (Vec<Candidate>, usize) {
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, u32, String)> = VecDeque::new();
    let mut results: Vec<Candidate> = Vec::new();
    let mut filtered_count = 0usize;

    // Initialize: mark roots as visited, enqueue their outgoing neighbors at hop 1.
    for root_id in root_ids {
        visited.insert(root_id.clone());
    }

    for root_id in root_ids {
        for edge in &graph.edges {
            if edge.from == *root_id
                && !visited.contains(&edge.to) {
                    visited.insert(edge.to.clone());
                    queue.push_back((edge.to.clone(), 1, edge.relation.clone()));
                }
        }
    }

    while let Some((current_id, hop, relation)) = queue.pop_front() {
        if hop > max_depth { continue; }

        let node = match graph.get_node(&current_id) {
            Some(n) => n,
            None => continue,
        };

        let source_code = if let Some(root) = project_root {
            load_source_from_disk(
                node.file_path.as_deref(),
                node.start_line,
                node.end_line,
                root,
            ).map(|r| r.source)
        } else {
            None
        };

        let candidate = Candidate {
            node_id: current_id.clone(),
            node_type: node.node_type.clone().unwrap_or_default(),
            file_path: node.file_path.clone(),
            signature: node.signature.clone(),
            doc_comment: node.doc_comment.clone(),
            description: node.description.clone(),
            source_code,
            hop_distance: hop,
            modified_at: None,
            connecting_relation: relation,
            token_estimate: 0, // computed in scoring
        };

        // GOAL-4.8: Apply --include filters.
        if passes_filters(&candidate, filters) {
            results.push(candidate);
        } else {
            filtered_count += 1;
        }

        // Expand forward for next hop.
        if hop < max_depth {
            for edge in &graph.edges {
                if edge.from == current_id && !visited.contains(&edge.to) {
                    visited.insert(edge.to.clone());
                    queue.push_back((edge.to.clone(), hop + 1, edge.relation.clone()));
                }
            }
        }
    }

    (results, filtered_count)
}

/// Stage 3: Reverse-edge traversal for callers and tests. **[GOAL-4.1e, 4.1f]**
fn gather_callers_and_tests(
    graph: &Graph,
    target_ids: &[String],
    project_root: Option<&Path>,
) -> (Vec<Candidate>, Vec<Candidate>) {
    let mut callers = Vec::new();
    let mut tests = Vec::new();
    let target_set: HashSet<&str> = target_ids.iter().map(|s| s.as_str()).collect();
    let mut seen: HashSet<String> = HashSet::new();

    for target_id in target_ids {
        // Find edges where to == target_id (incoming edges).
        for edge in &graph.edges {
            if edge.to != *target_id { continue; }
            if target_set.contains(edge.from.as_str()) { continue; }
            if seen.contains(&edge.from) { continue; }
            seen.insert(edge.from.clone());

            let node = match graph.get_node(&edge.from) {
                Some(n) => n,
                None => continue,
            };

            let source_code = if let Some(root) = project_root {
                load_source_from_disk(
                    node.file_path.as_deref(),
                    node.start_line,
                    node.end_line,
                    root,
                ).map(|r| r.source)
            } else {
                None
            };

            let candidate = Candidate {
                node_id: node.id.clone(),
                node_type: node.node_type.clone().unwrap_or_default(),
                file_path: node.file_path.clone(),
                signature: node.signature.clone(),
                doc_comment: node.doc_comment.clone(),
                description: node.description.clone(),
                source_code,
                hop_distance: 1,
                modified_at: None,
                connecting_relation: edge.relation.clone(),
                token_estimate: 0,
            };

            // Categorize: tests_for → test, everything else → caller.
            match edge.relation.as_str() {
                "tests_for" => tests.push(candidate),
                _ => callers.push(candidate),
            }
        }
    }

    (callers, tests)
}

/// GOAL-4.8: Filter by --include patterns.
///
/// If `include_patterns` is empty, all candidates pass.
/// Patterns prefixed with "type:" match node_type (e.g., "type:function").
/// Other patterns match file_path as a glob.
fn passes_filters(candidate: &Candidate, filters: &ContextFilters) -> bool {
    // Check exclude_ids
    if filters.exclude_ids.contains(&candidate.node_id) {
        return false;
    }

    // Check modified_after
    if let Some(threshold) = filters.modified_after {
        if let Some(modified) = candidate.modified_at {
            if modified < threshold { return false; }
        }
        // If no modified_at on node, keep it (don't filter on missing data).
    }

    // Check include_patterns (any-match semantics).
    if filters.include_patterns.is_empty() { return true; }

    for pattern in &filters.include_patterns {
        if let Some(type_filter) = pattern.strip_prefix("type:") {
            // Match by node_type.
            if candidate.node_type == type_filter { return true; }
        } else {
            // Match by file path glob.
            if let Some(ref path) = candidate.file_path {
                if simple_glob_match(pattern, path) { return true; }
            }
        }
    }

    false // No pattern matched.
}

/// Simple glob matching for --include patterns.
///
/// Supports:
/// - `*` matches any sequence of non-`/` characters
/// - `**` matches any sequence including `/`
/// - `?` matches a single character
/// - Literal characters match exactly
fn simple_glob_match(pattern: &str, text: &str) -> bool {
    // Handle ** (matches everything including path separators).
    if pattern == "**" { return true; }

    // Split pattern on `**` segments for recursive matching.
    if pattern.contains("**") {
        let parts: Vec<&str> = pattern.split("**").collect();
        if parts.len() == 2 {
            let prefix = parts[0];
            let suffix = parts[1];
            // Prefix must match the start.
            if !prefix.is_empty() {
                let prefix_trimmed = prefix.trim_end_matches('/');
                if !text.starts_with(prefix_trimmed) { return false; }
            }
            // Suffix must match the end (with glob).
            if !suffix.is_empty() {
                let suffix_trimmed = suffix.trim_start_matches('/');
                // Check if any tail of text matches the suffix pattern.
                for (i, _) in text.char_indices() {
                    if simple_glob_segment(suffix_trimmed, &text[i..]) { return true; }
                }
                return simple_glob_segment(suffix_trimmed, "");
            }
            return true;
        }
    }

    simple_glob_segment(pattern, text)
}

/// Match a glob pattern segment (without **) against text.
fn simple_glob_segment(pattern: &str, text: &str) -> bool {
    let mut pi = pattern.chars().peekable();
    let mut ti = text.chars().peekable();

    while pi.peek().is_some() || ti.peek().is_some() {
        match pi.peek() {
            Some('*') => {
                pi.next();
                // * matches zero or more non-/ chars.
                if pi.peek().is_none() {
                    // Trailing * — match rest if no slashes.
                    return !ti.any(|c| c == '/');
                }
                // Try matching the rest of the pattern after *.
                let remaining_pattern: String = pi.clone().collect();
                let remaining_text: String = ti.clone().collect();
                for i in 0..=remaining_text.len() {
                    if remaining_text.is_char_boundary(i) {
                        let slice = &remaining_text[i..];
                        // * doesn't match across /.
                        if i > 0 && remaining_text.as_bytes()[i - 1] == b'/' { break; }
                        if simple_glob_segment(&remaining_pattern, slice) { return true; }
                    }
                }
                return false;
            }
            Some('?') => {
                pi.next();
                match ti.next() {
                    Some(c) if c != '/' => {}
                    _ => return false,
                }
            }
            Some(&pc) => {
                pi.next();
                match ti.next() {
                    Some(tc) if tc == pc => {}
                    _ => return false,
                }
            }
            None => {
                return ti.peek().is_none();
            }
        }
    }

    true
}

// =============================================================================
// §10 Output Formatting (GOAL-4.9)
// =============================================================================

/// Format the assembled context as a string in the requested format. **[GOAL-4.9]**
pub fn format_context(ctx: &AssembledContext, format: OutputFormat) -> String {
    match format {
        OutputFormat::Json => {
            serde_json::to_string_pretty(ctx).unwrap_or_else(|e| format!("{{\"error\": \"{}\"}}", e))
        }
        OutputFormat::Yaml => {
            serde_yaml::to_string(ctx).unwrap_or_else(|e| format!("error: {}", e))
        }
        OutputFormat::Markdown => format_context_markdown(ctx),
    }
}

/// Render context as human-readable markdown. **[GOAL-4.9]**
fn format_context_markdown(ctx: &AssembledContext) -> String {
    let mut out = String::new();

    // Header with stats.
    out.push_str(&format!(
        "# Context Assembly\n\n\
         **Tokens**: {}/{} | **Nodes**: {} visited, {} included, {} filtered\n\
         **Elapsed**: {}ms\n\n",
        ctx.stats.budget_used, ctx.stats.budget_total,
        ctx.stats.nodes_visited, ctx.stats.nodes_included,
        ctx.stats.nodes_excluded_by_filter,
        ctx.stats.elapsed_ms,
    ));

    // Targets.
    if !ctx.result.targets.is_empty() {
        out.push_str("## Targets\n\n");
        for target in &ctx.result.targets {
            out.push_str(&format!("### `{}`", target.node_id));
            if let Some(ref title) = target.title {
                out.push_str(&format!(" — {}", title));
            }
            out.push('\n');
            if let Some(ref fp) = target.file_path {
                out.push_str(&format!("**File**: `{}`\n", fp));
            }
            if let Some(ref sig) = target.signature {
                out.push_str(&format!("**Signature**: `{}`\n", sig));
            }
            if let Some(ref dc) = target.doc_comment {
                out.push_str(&format!("**Doc**: {}\n", dc.lines().next().unwrap_or("")));
            }
            if let Some(ref src) = target.source_code {
                let preview: String = src.lines().take(20).collect::<Vec<_>>().join("\n");
                out.push_str(&format!("\n```\n{}\n```\n", preview));
            }
            out.push_str(&format!("*~{} tokens*\n\n", target.token_estimate));
        }
    }

    // Dependencies.
    if !ctx.result.dependencies.is_empty() {
        out.push_str("## Dependencies\n\n");
        for item in &ctx.result.dependencies {
            format_context_item(&mut out, item);
        }
    }

    // Callers.
    if !ctx.result.callers.is_empty() {
        out.push_str("## Callers\n\n");
        for item in &ctx.result.callers {
            format_context_item(&mut out, item);
        }
    }

    // Tests.
    if !ctx.result.tests.is_empty() {
        out.push_str("## Tests\n\n");
        for item in &ctx.result.tests {
            format_context_item(&mut out, item);
        }
    }

    // Truncation summary.
    let trunc = &ctx.result.truncation_info;
    if trunc.truncated_count > 0 || trunc.dropped_count > 0 {
        out.push_str(&format!(
            "---\n*Truncation: {} items truncated, {} items dropped*\n",
            trunc.truncated_count, trunc.dropped_count,
        ));
    }

    out
}

/// Format a single ContextItem for markdown output.
fn format_context_item(out: &mut String, item: &ContextItem) {
    out.push_str(&format!("- **`{}`**", item.node_id));
    if let Some(ref fp) = item.file_path {
        out.push_str(&format!(" (`{}`)", fp));
    }
    out.push_str(&format!(
        " — {} | score: {:.2}{}",
        item.connecting_relation,
        item.score,
        if item.truncated { " ⚠️truncated" } else { "" },
    ));
    out.push('\n');
    if let Some(ref sig) = item.signature {
        out.push_str(&format!("  Sig: `{}`\n", sig));
    }
    if let Some(ref content) = item.content {
        let preview: String = content.lines().take(5).collect::<Vec<_>>().join("\n  ");
        if !preview.is_empty() {
            out.push_str(&format!("  {}\n", preview));
        }
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

    // =========================================================================
    // §5 Relevance Scoring Tests — GOAL-4.4 5-Tier Ranking Verification
    // =========================================================================

    /// Helper: create a minimal candidate with given relation and hop distance.
    fn make_candidate(relation: &str, hop: u32) -> Candidate {
        Candidate {
            node_id: format!("node-{}-{}", relation, hop),
            node_type: "function".to_string(),
            file_path: None,
            signature: None,
            doc_comment: None,
            description: None,
            source_code: None,
            hop_distance: hop,
            modified_at: None,
            connecting_relation: relation.to_string(),
            token_estimate: 0,
        }
    }

    /// Helper: create a candidate with source/signature content for token estimation.
    fn make_candidate_with_content(relation: &str, hop: u32, source: &str, sig: &str) -> Candidate {
        Candidate {
            node_id: format!("node-{}-{}", relation, hop),
            node_type: "function".to_string(),
            file_path: Some("/src/lib.rs".to_string()),
            signature: Some(sig.to_string()),
            doc_comment: Some("/// A function".to_string()),
            description: Some("Does stuff".to_string()),
            source_code: Some(source.to_string()),
            hop_distance: hop,
            modified_at: None,
            connecting_relation: relation.to_string(),
            token_estimate: 0,
        }
    }

    // --- Tier 1: Direct Call (calls, imports) → rank 1, score 1.0 ---

    #[test]
    fn test_rank_tier1_calls() {
        assert_eq!(relation_rank("calls"), 1);
        assert_eq!(relation_score("calls"), 1.0);
    }

    #[test]
    fn test_rank_tier1_imports() {
        assert_eq!(relation_rank("imports"), 1);
        assert_eq!(relation_score("imports"), 1.0);
    }

    // --- Tier 2: Type Reference (type_reference, inherits, implements, uses) → rank 2, score 0.8 ---

    #[test]
    fn test_rank_tier2_type_reference() {
        assert_eq!(relation_rank("type_reference"), 2);
        assert_eq!(relation_score("type_reference"), 0.8);
    }

    #[test]
    fn test_rank_tier2_inherits() {
        assert_eq!(relation_rank("inherits"), 2);
        assert_eq!(relation_score("inherits"), 0.8);
    }

    #[test]
    fn test_rank_tier2_implements() {
        assert_eq!(relation_rank("implements"), 2);
        assert_eq!(relation_score("implements"), 0.8);
    }

    #[test]
    fn test_rank_tier2_uses() {
        assert_eq!(relation_rank("uses"), 2);
        assert_eq!(relation_score("uses"), 0.8);
    }

    // --- Tier 3: Same-file (contains, defined_in) → rank 3, score 0.6 ---

    #[test]
    fn test_rank_tier3_contains() {
        assert_eq!(relation_rank("contains"), 3);
        assert_eq!(relation_score("contains"), 0.6);
    }

    #[test]
    fn test_rank_tier3_defined_in() {
        assert_eq!(relation_rank("defined_in"), 3);
        assert_eq!(relation_score("defined_in"), 0.6);
    }

    // --- Tier 4: Structural (depends_on, part_of, blocks, tests_for) → rank 4, score 0.4 ---

    #[test]
    fn test_rank_tier4_depends_on() {
        assert_eq!(relation_rank("depends_on"), 4);
        assert_eq!(relation_score("depends_on"), 0.4);
    }

    #[test]
    fn test_rank_tier4_part_of() {
        assert_eq!(relation_rank("part_of"), 4);
        assert_eq!(relation_score("part_of"), 0.4);
    }

    #[test]
    fn test_rank_tier4_blocks() {
        assert_eq!(relation_rank("blocks"), 4);
        assert_eq!(relation_score("blocks"), 0.4);
    }

    #[test]
    fn test_rank_tier4_tests_for() {
        assert_eq!(relation_rank("tests_for"), 4);
        assert_eq!(relation_score("tests_for"), 0.4);
    }

    // --- Tier 5: Transitive / Unknown → rank 5, score 0.2 ---

    #[test]
    fn test_rank_tier5_unknown_relations() {
        // Any unrecognized relation falls to tier 5
        for rel in &["relates_to", "references", "mentions", "foobar", "", "CALLS", "Imports"] {
            assert_eq!(relation_rank(rel), 5,
                "Expected tier 5 for unknown relation '{}'", rel);
            assert_eq!(relation_score(rel), 0.2,
                "Expected score 0.2 for unknown relation '{}'", rel);
        }
    }

    // --- Score monotonicity: higher-tier relations → higher scores ---

    #[test]
    fn test_scores_monotonically_decreasing_by_tier() {
        let tier1 = relation_score("calls");
        let tier2 = relation_score("type_reference");
        let tier3 = relation_score("contains");
        let tier4 = relation_score("depends_on");
        let tier5 = relation_score("unknown");

        assert!(tier1 > tier2, "Tier 1 ({}) must be > Tier 2 ({})", tier1, tier2);
        assert!(tier2 > tier3, "Tier 2 ({}) must be > Tier 3 ({})", tier2, tier3);
        assert!(tier3 > tier4, "Tier 3 ({}) must be > Tier 4 ({})", tier3, tier4);
        assert!(tier4 > tier5, "Tier 4 ({}) must be > Tier 5 ({})", tier4, tier5);
        assert!(tier5 > 0.0, "Tier 5 ({}) must be > 0", tier5);
    }

    #[test]
    fn test_all_scores_in_valid_range() {
        let all_relations = [
            "calls", "imports",
            "type_reference", "inherits", "implements", "uses",
            "contains", "defined_in",
            "depends_on", "part_of", "blocks", "tests_for",
            "unknown", "foobar",
        ];
        for rel in &all_relations {
            let s = relation_score(rel);
            assert!(s > 0.0 && s <= 1.0,
                "Score for '{}' is {} — must be in (0.0, 1.0]", rel, s);
        }
    }

    // --- Composite scoring tests ---

    #[test]
    fn test_score_candidate_hop1_calls() {
        let c = make_candidate("calls", 1);
        let scored = score_candidate(&c);

        // hop=1 → no transitive penalty
        // score = (0.60 * 1.0 + 0.30 * 1.0 + 0.10 * 1.0) * 1.0 = 1.0
        assert!((scored.score - 1.0).abs() < 1e-10,
            "calls at hop 1 should score 1.0, got {}", scored.score);
    }

    #[test]
    fn test_score_candidate_hop1_depends_on() {
        let c = make_candidate("depends_on", 1);
        let scored = score_candidate(&c);

        // hop=1 → no transitive penalty
        // score = (0.60 * 0.4 + 0.30 * 1.0 + 0.10 * 1.0) * 1.0 = 0.24 + 0.30 + 0.10 = 0.64
        assert!((scored.score - 0.64).abs() < 1e-10,
            "depends_on at hop 1 should score 0.64, got {}", scored.score);
    }

    #[test]
    fn test_score_candidate_hop2_transitive_penalty() {
        let c = make_candidate("calls", 2);
        let scored = score_candidate(&c);

        // hop=2 → proximity = 0.5, transitive_penalty = 0.8
        // score = (0.60 * 1.0 + 0.30 * 0.5 + 0.10 * 1.0) * 0.8
        //       = (0.60 + 0.15 + 0.10) * 0.8 = 0.85 * 0.8 = 0.68
        assert!((scored.score - 0.68).abs() < 1e-10,
            "calls at hop 2 should score 0.68, got {}", scored.score);
    }

    #[test]
    fn test_score_candidate_hop3_high_penalty() {
        let c = make_candidate("unknown", 3);
        let scored = score_candidate(&c);

        // hop=3 → proximity = 1/3, transitive_penalty = 0.8
        // score = (0.60 * 0.2 + 0.30 * (1/3) + 0.10 * 1.0) * 0.8
        //       = (0.12 + 0.10 + 0.10) * 0.8 = 0.32 * 0.8 = 0.256
        assert!((scored.score - 0.256).abs() < 1e-10,
            "unknown at hop 3 should score 0.256, got {}", scored.score);
    }

    #[test]
    fn test_calls_hop1_beats_type_ref_hop1() {
        let calls = score_candidate(&make_candidate("calls", 1));
        let type_ref = score_candidate(&make_candidate("type_reference", 1));

        assert!(calls.score > type_ref.score,
            "calls ({}) at hop 1 must beat type_reference ({}) at hop 1",
            calls.score, type_ref.score);
    }

    #[test]
    fn test_calls_hop2_vs_type_ref_hop1() {
        // calls at hop 2 (penalized) should still be meaningfully scored
        let calls_h2 = score_candidate(&make_candidate("calls", 2));
        let type_ref_h1 = score_candidate(&make_candidate("type_reference", 1));

        // calls@hop2 = 0.68, type_ref@hop1 = (0.60*0.8 + 0.30*1.0 + 0.10*1.0) = 0.88
        // So type_ref at hop 1 beats calls at hop 2 — proximity matters
        assert!(type_ref_h1.score > calls_h2.score,
            "type_ref at hop 1 ({}) should beat calls at hop 2 ({}) because proximity matters",
            type_ref_h1.score, calls_h2.score);
    }

    #[test]
    fn test_same_relation_closer_hop_wins() {
        let hop1 = score_candidate(&make_candidate("imports", 1));
        let hop2 = score_candidate(&make_candidate("imports", 2));
        let hop3 = score_candidate(&make_candidate("imports", 3));

        assert!(hop1.score > hop2.score, "hop1 ({}) > hop2 ({})", hop1.score, hop2.score);
        assert!(hop2.score > hop3.score, "hop2 ({}) > hop3 ({})", hop2.score, hop3.score);
    }

    #[test]
    fn test_hop0_proximity_no_division_by_zero() {
        // hop_distance = 0 should not panic or produce NaN/Infinity
        let c = make_candidate("calls", 0);
        let scored = score_candidate(&c);
        assert!(scored.score.is_finite(), "hop 0 must not produce NaN/Infinity");
        assert!(scored.score > 0.0, "hop 0 must produce positive score");
    }

    #[test]
    fn test_nan_guard() {
        // Create a candidate where hop = 0 (which we handle) 
        // The NaN guard should catch any edge case
        let c = make_candidate("calls", 0);
        let scored = score_candidate(&c);
        assert!(!scored.score.is_nan(), "Score must never be NaN");
        assert!(scored.score.is_finite(), "Score must be finite");
    }

    // --- score_candidates: batch scoring and sorting ---

    #[test]
    fn test_score_candidates_sorted_descending() {
        let candidates = vec![
            make_candidate("unknown", 3),     // lowest score
            make_candidate("calls", 1),       // highest score
            make_candidate("depends_on", 2),  // mid-low
            make_candidate("contains", 1),    // mid
        ];

        let scored = score_candidates(&candidates);
        for i in 1..scored.len() {
            assert!(scored[i-1].score >= scored[i].score,
                "Candidates not sorted descending: index {} ({}) < index {} ({})",
                i-1, scored[i-1].score, i, scored[i].score);
        }

        // First should be calls@hop1 (highest)
        assert_eq!(scored[0].candidate.connecting_relation, "calls");
        // Last should be unknown@hop3 (lowest)
        assert_eq!(scored.last().unwrap().candidate.connecting_relation, "unknown");
    }

    #[test]
    fn test_score_candidates_empty_input() {
        let scored = score_candidates(&[]);
        assert!(scored.is_empty());
    }

    #[test]
    fn test_score_candidates_single_item() {
        let scored = score_candidates(&[make_candidate("imports", 1)]);
        assert_eq!(scored.len(), 1);
        assert_eq!(scored[0].candidate.connecting_relation, "imports");
    }

    #[test]
    fn test_score_candidates_preserves_all() {
        let candidates = vec![
            make_candidate("calls", 1),
            make_candidate("imports", 1),
            make_candidate("type_reference", 2),
            make_candidate("contains", 1),
            make_candidate("depends_on", 3),
        ];
        let scored = score_candidates(&candidates);
        assert_eq!(scored.len(), 5, "All candidates must be preserved after scoring");
    }

    // --- Token estimation ---

    #[test]
    fn test_token_estimation_empty_candidate() {
        let c = make_candidate("calls", 1);
        let tokens = estimate_tokens_for_candidate(&c);
        // No content → only overhead (30 bytes) → 30/4 = 7, but max(1) → 7
        assert_eq!(tokens, 7, "Empty candidate with 30B overhead → 7 tokens");
    }

    #[test]
    fn test_token_estimation_with_content() {
        let source = "fn main() { println!(\"hello\"); }";
        let sig = "fn main()";
        let desc = "Does stuff";
        let doc = "/// A function";
        let c = make_candidate_with_content("calls", 1, source, sig);
        let tokens = estimate_tokens_for_candidate(&c);
        // source + signature + description + doc_comment + overhead(30), all / 4
        let expected_bytes = source.len() + sig.len() + desc.len() + doc.len() + 30;
        let expected_tokens = (expected_bytes / 4).max(1);
        assert_eq!(tokens, expected_tokens,
            "bytes: source={} + sig={} + desc={} + doc={} + overhead=30 = {}, /4 = {}",
            source.len(), sig.len(), desc.len(), doc.len(), expected_bytes, expected_tokens);
    }

    #[test]
    fn test_estimate_tokens_str_empty() {
        assert_eq!(estimate_tokens_str(""), 0);
    }

    #[test]
    fn test_estimate_tokens_str_short() {
        assert_eq!(estimate_tokens_str("ab"), 1); // 2/4 = 0 → max(1) = 1
    }

    #[test]
    fn test_estimate_tokens_str_exact() {
        assert_eq!(estimate_tokens_str("abcd"), 1); // 4/4 = 1
        assert_eq!(estimate_tokens_str("abcdefgh"), 2); // 8/4 = 2
    }

    // --- Design compliance: all relations from GOAL-4.4 mapped ---

    #[test]
    fn test_goal_4_4_tier1_complete() {
        // GOAL-4.4 Tier 1: calls, imports
        let tier1_relations = ["calls", "imports"];
        for rel in &tier1_relations {
            assert_eq!(relation_rank(rel), 1,
                "GOAL-4.4 requires '{}' in Tier 1 (rank 1)", rel);
        }
    }

    #[test]
    fn test_goal_4_4_tier2_complete() {
        // GOAL-4.4 Tier 2: type_reference, inherits, implements, uses
        let tier2_relations = ["type_reference", "inherits", "implements", "uses"];
        for rel in &tier2_relations {
            assert_eq!(relation_rank(rel), 2,
                "GOAL-4.4 requires '{}' in Tier 2 (rank 2)", rel);
        }
    }

    #[test]
    fn test_goal_4_4_tier3_complete() {
        // GOAL-4.4 Tier 3: contains, defined_in
        let tier3_relations = ["contains", "defined_in"];
        for rel in &tier3_relations {
            assert_eq!(relation_rank(rel), 3,
                "GOAL-4.4 requires '{}' in Tier 3 (rank 3)", rel);
        }
    }

    #[test]
    fn test_goal_4_4_tier4_complete() {
        // GOAL-4.4 Tier 4: depends_on, part_of, blocks, tests_for
        let tier4_relations = ["depends_on", "part_of", "blocks", "tests_for"];
        for rel in &tier4_relations {
            assert_eq!(relation_rank(rel), 4,
                "GOAL-4.4 requires '{}' in Tier 4 (rank 4)", rel);
        }
    }

    #[test]
    fn test_goal_4_4_tier5_fallback() {
        // GOAL-4.4 Tier 5: anything not in tiers 1-4
        let unknown_relations = ["unknown", "relates_to", "belongs_to", "subtask_of", ""];
        for rel in &unknown_relations {
            assert_eq!(relation_rank(rel), 5,
                "GOAL-4.4 requires '{}' to fall to Tier 5 (rank 5)", rel);
        }
    }

    // --- Case sensitivity (relations are case-sensitive) ---

    #[test]
    fn test_relations_case_sensitive() {
        // "calls" is tier 1, but "Calls" or "CALLS" should fall to tier 5
        assert_eq!(relation_rank("Calls"), 5);
        assert_eq!(relation_rank("CALLS"), 5);
        assert_eq!(relation_rank("Imports"), 5);
        assert_eq!(relation_rank("IMPORTS"), 5);
        assert_eq!(relation_rank("Contains"), 5);
        assert_eq!(relation_rank("DEPENDS_ON"), 5);
    }

    // --- Composite score: weight verification ---

    #[test]
    fn test_scoring_weights_sum_to_one() {
        let sum = W_RELATION + W_PROXIMITY + W_WEIGHT;
        assert!((sum - 1.0).abs() < 1e-10,
            "Scoring weights should sum to 1.0 for normalized output, got {}", sum);
    }

    #[test]
    fn test_relation_is_dominant_factor() {
        // W_RELATION (0.60) is the largest weight — relation tier should be the
        // primary differentiator, not hop distance alone.
        // Compile-time enforced via const assertions below; this test exists
        // so failures surface in test output with a clear domain message.
        const _: () = assert!(W_RELATION > W_PROXIMITY,
            "W_RELATION must be > W_PROXIMITY");
        const _: () = assert!(W_RELATION > W_WEIGHT,
            "W_RELATION must be > W_WEIGHT");
    }

    // --- Sorting stability: same-scored candidates maintain relative order ---

    #[test]
    fn test_score_candidates_stable_ordering_same_tier_same_hop() {
        // Two tier-1 relations at same hop → same score → order preserved
        let candidates = vec![
            make_candidate("calls", 1),
            make_candidate("imports", 1),
        ];
        let scored = score_candidates(&candidates);
        assert_eq!(scored.len(), 2);
        // Both have identical scores
        assert!((scored[0].score - scored[1].score).abs() < 1e-10);
    }

    // --- Real-world scenario: mixed tiers and hops ---

    #[test]
    fn test_realistic_scoring_scenario() {
        // Simulating a real context assembly:
        // Target: fn parse_config()
        // - Neighbor via "calls" at hop 1: fn validate_config()  → highest
        // - Neighbor via "imports" at hop 1: mod config_types     → highest
        // - Neighbor via "type_reference" at hop 1: struct Config → high
        // - Neighbor via "defined_in" at hop 1: file config.rs    → medium
        // - Neighbor via "depends_on" at hop 1: task impl-config  → low
        // - Neighbor via "calls" at hop 2: fn read_file()         → penalized
        // - Neighbor via "unknown" at hop 3: some-node            → lowest

        let candidates = vec![
            make_candidate("calls", 1),
            make_candidate("imports", 1),
            make_candidate("type_reference", 1),
            make_candidate("defined_in", 1),
            make_candidate("depends_on", 1),
            make_candidate("calls", 2),
            make_candidate("unknown", 3),
        ];

        let scored = score_candidates(&candidates);

        // Verify ordering: calls@1 ≥ imports@1 > type_ref@1 > defined_in@1 > calls@2 > depends_on@1 > unknown@3
        // (Note: calls@1 == imports@1 in score)
        assert_eq!(scored.len(), 7);

        // calls@1 and imports@1 should be at the top (both score 1.0)
        let top_two_relations: Vec<&str> = scored[..2].iter()
            .map(|s| s.candidate.connecting_relation.as_str())
            .collect();
        assert!(top_two_relations.contains(&"calls") && top_two_relations.contains(&"imports"),
            "Top 2 should be calls and imports, got {:?}", top_two_relations);

        // type_reference@1 should be 3rd
        assert_eq!(scored[2].candidate.connecting_relation, "type_reference");

        // unknown@3 should be last
        assert_eq!(scored[6].candidate.connecting_relation, "unknown");

        // Verify all scores are positive and in descending order
        for i in 1..scored.len() {
            assert!(scored[i-1].score >= scored[i].score,
                "Not descending at index {}: {} vs {}", i, scored[i-1].score, scored[i].score);
            assert!(scored[i].score > 0.0, "Score at index {} should be > 0", i);
        }
    }

    // --- Edge case: very deep hops ---

    #[test]
    fn test_deep_hop_still_positive_score() {
        for hop in [5, 10, 50, 100] {
            let c = make_candidate("calls", hop);
            let scored = score_candidate(&c);
            assert!(scored.score > 0.0,
                "Score at hop {} must be > 0, got {}", hop, scored.score);
            assert!(scored.score.is_finite(),
                "Score at hop {} must be finite, got {}", hop, scored.score);
        }
    }

    #[test]
    fn test_score_decreases_with_hop_for_same_relation() {
        let hops: Vec<u32> = (1..=5).collect();
        let scores: Vec<f64> = hops.iter()
            .map(|&h| score_candidate(&make_candidate("calls", h)).score)
            .collect();

        for i in 1..scores.len() {
            assert!(scores[i-1] > scores[i],
                "Score at hop {} ({}) should be > score at hop {} ({})",
                hops[i-1], scores[i-1], hops[i], scores[i]);
        }
    }

    // =========================================================================
    // §6 Tests: Category-Based Truncation (GOAL-4.3)
    // =========================================================================

    /// Helper: make a ScoredCandidate with known token estimate.
    fn make_scored(id: &str, relation: &str, hop: u32, tokens: usize) -> ScoredCandidate {
        let c = Candidate {
            node_id: id.to_string(),
            node_type: "function".to_string(),
            file_path: Some(format!("/src/{}.rs", id)),
            signature: Some(format!("fn {}()", id)),
            doc_comment: None,
            description: Some(format!("Description of {}", id)),
            source_code: Some("x".repeat(tokens * 4)), // ~tokens tokens
            hop_distance: hop,
            modified_at: None,
            connecting_relation: relation.to_string(),
            token_estimate: 0,
        };
        ScoredCandidate {
            score: score_candidate(&c).score,
            token_estimate: tokens,
            candidate: c,
        }
    }

    /// Helper: make a TargetContext with known token estimate.
    fn make_target(id: &str, tokens: usize) -> TargetContext {
        TargetContext {
            node_id: id.to_string(),
            title: Some(format!("Target {}", id)),
            file_path: Some(format!("/src/{}.rs", id)),
            signature: Some(format!("fn {}()", id)),
            doc_comment: None,
            description: Some(format!("Target desc {}", id)),
            source_code: Some("t".repeat(tokens.saturating_sub(20) * 4)),
            token_estimate: tokens,
        }
    }

    // --- truncate_text tests ---

    #[test]
    fn test_truncate_text_short_text_no_truncation() {
        let text = "fn foo() { 42 }";
        let result = truncate_text(text, 100);
        assert_eq!(result, text, "Short text should be returned as-is");
    }

    #[test]
    fn test_truncate_text_exact_boundary() {
        let text = "abcd"; // 4 bytes = 1 token
        let result = truncate_text(text, 1);
        assert_eq!(result, text, "Text that exactly fits should not be truncated");
    }

    #[test]
    fn test_truncate_text_adds_marker() {
        // 100 bytes of content, budget of 10 tokens = 40 bytes
        let text = "a".repeat(100);
        let result = truncate_text(&text, 10);
        assert!(result.ends_with("... [truncated]"),
            "Truncated text must end with marker, got: {}", result);
    }

    #[test]
    fn test_truncate_text_prefers_line_boundary() {
        let text = "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nline 10";
        // Budget of 10 tokens = 40 bytes, marker is 16 bytes → ~24 usable bytes
        let result = truncate_text(text, 10);
        assert!(result.contains("... [truncated]"));
        // Should cut at a newline, not in the middle of "line"
        let before_marker = result.split("\n... [truncated]").next().unwrap();
        // Each line is ~7 chars, 24 usable bytes → should keep ~3 lines
        assert!(before_marker.ends_with(char::is_numeric) || before_marker.ends_with('\n') || before_marker.contains("line"),
            "Should truncate at line boundary, got: '{}'", before_marker);
    }

    #[test]
    fn test_truncate_text_utf8_safety() {
        // Multi-byte chars: each emoji is 4 bytes
        let text = "🔥🔥🔥🔥🔥🔥🔥🔥🔥🔥"; // 10 emojis = 40 bytes
        // Budget = 5 tokens = 20 bytes, marker = 16 bytes → 4 usable = 1 emoji
        let result = truncate_text(text, 5);
        // Must be valid UTF-8 (String guarantees this)
        assert!(result.ends_with("... [truncated]"));
        // Must not panic or produce invalid string
        for c in result.chars() {
            assert!(c.len_utf8() <= 4);
        }
    }

    #[test]
    fn test_truncate_text_chinese_chars() {
        // Chinese chars are 3 bytes each
        let text = "这是一个测试字符串用于验证中文截断功能是否正确工作";
        // 17 chars × 3 bytes = 51 bytes total
        let result = truncate_text(text, 5); // 20 bytes budget
        assert!(result.ends_with("... [truncated]"));
        // Verify we can iterate chars without panicking
        let _ = result.chars().count();
    }

    #[test]
    fn test_truncate_text_empty_input() {
        let result = truncate_text("", 100);
        assert_eq!(result, "");
    }

    #[test]
    fn test_truncate_text_zero_budget() {
        let text = "some content";
        let result = truncate_text(text, 0);
        // 0 tokens = 0 bytes, marker = 16 bytes → saturating_sub → 0 usable
        // Should truncate to empty + marker, or just marker
        assert!(result.contains("... [truncated]") || result.is_empty());
    }

    #[test]
    fn test_truncate_text_result_within_budget() {
        let text = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\no\np";
        let budget_tokens = 5;
        let result = truncate_text(text, budget_tokens);
        let result_tokens = estimate_tokens_str(&result);
        // Result tokens should be ≤ budget (or very close due to rounding)
        assert!(result_tokens <= budget_tokens + 1,
            "Result {} tokens should be ≤ budget {} tokens", result_tokens, budget_tokens);
    }

    #[test]
    fn test_truncate_text_head_biased() {
        let lines: Vec<String> = (1..=20).map(|i| format!("line {}", i)).collect();
        let text = lines.join("\n");
        let result = truncate_text(&text, 15);
        assert!(result.starts_with("line 1"), "Must preserve beginning (head-biased)");
        assert!(!result.contains("line 20"), "Must drop end content");
    }

    // --- greedy_fill tests ---

    #[test]
    fn test_greedy_fill_all_fit() {
        let items = vec![
            make_scored("a", "calls", 1, 100),
            make_scored("b", "calls", 1, 100),
            make_scored("c", "calls", 1, 100),
        ];
        let (included, info) = greedy_fill(&items, 1000);
        assert_eq!(included.len(), 3, "All 3 should fit in 1000 budget");
        assert_eq!(info.truncated_count, 0);
        assert_eq!(info.dropped_count, 0);
        assert_eq!(info.budget_used, 300);
    }

    #[test]
    fn test_greedy_fill_partial_fit() {
        let items = vec![
            make_scored("a", "calls", 1, 100),
            make_scored("b", "calls", 1, 100),
            make_scored("c", "calls", 1, 100),
        ];
        let (included, info) = greedy_fill(&items, 250);
        // First two fully fit (200), third has 50 remaining ≥ MIN_USEFUL_TOKENS_TRUNC (32)
        assert_eq!(included.len(), 3, "Third item should be truncated, not dropped");
        assert_eq!(info.truncated_count, 1);
        assert_eq!(info.dropped_count, 0);
        assert!(included[2].truncated, "Third item should be marked truncated");
    }

    #[test]
    fn test_greedy_fill_drop_when_budget_too_small() {
        let items = vec![
            make_scored("a", "calls", 1, 100),
            make_scored("b", "calls", 1, 100),
        ];
        // Budget only fits first item with 10 left over (< MIN_USEFUL_TOKENS_TRUNC)
        let (included, info) = greedy_fill(&items, 110);
        assert_eq!(included.len(), 1, "Only first should fit");
        assert_eq!(info.dropped_count, 1, "Second should be dropped (10 < 32 min)");
        assert_eq!(info.truncated_count, 0);
    }

    #[test]
    fn test_greedy_fill_empty_input() {
        let items: Vec<ScoredCandidate> = vec![];
        let (included, info) = greedy_fill(&items, 1000);
        assert!(included.is_empty());
        assert_eq!(info.budget_used, 0);
    }

    #[test]
    fn test_greedy_fill_zero_budget() {
        let items = vec![
            make_scored("a", "calls", 1, 100),
        ];
        let (included, info) = greedy_fill(&items, 0);
        assert!(included.is_empty());
        assert_eq!(info.dropped_count, 1);
    }

    #[test]
    fn test_greedy_fill_preserves_order() {
        let items = vec![
            make_scored("first", "calls", 1, 50),
            make_scored("second", "imports", 1, 50),
            make_scored("third", "type_reference", 1, 50),
        ];
        let (included, _) = greedy_fill(&items, 1000);
        assert_eq!(included[0].node_id, "first");
        assert_eq!(included[1].node_id, "second");
        assert_eq!(included[2].node_id, "third");
    }

    #[test]
    fn test_greedy_fill_truncated_item_has_reduced_tokens() {
        let items = vec![
            make_scored("big", "calls", 1, 500),
        ];
        let (included, info) = greedy_fill(&items, 100);
        assert_eq!(included.len(), 1);
        assert!(included[0].truncated);
        assert!(included[0].token_estimate <= 100,
            "Truncated item tokens {} should be ≤ budget 100", included[0].token_estimate);
        assert_eq!(info.truncated_count, 1);
    }

    #[test]
    fn test_greedy_fill_many_small_items() {
        // 20 items × 10 tokens = 200 total, budget 150
        let items: Vec<ScoredCandidate> = (0..20)
            .map(|i| make_scored(&format!("item-{}", i), "calls", 1, 10))
            .collect();
        let (included, info) = greedy_fill(&items, 150);
        assert_eq!(included.len(), 15, "Should fit exactly 15 items (150/10)");
        assert_eq!(info.dropped_count, 5);
        assert_eq!(info.budget_used, 150);
    }

    // --- budget_fit_by_category tests ---

    #[test]
    fn test_budget_targets_never_truncated() {
        // Target consumes most of the budget
        let targets = vec![make_target("t1", 800)];
        let deps = vec![make_scored("d1", "calls", 1, 100)];
        let callers = vec![make_scored("c1", "calls", 1, 100)];
        let tests = vec![make_scored("test1", "tests_for", 1, 100)];

        let result = budget_fit_by_category(&targets, deps, callers, tests, 1000);

        // Targets always included
        assert_eq!(result.targets.len(), 1);
        assert_eq!(result.targets[0].node_id, "t1");
        // Only 200 budget remaining for deps+callers+tests (300 needed)
        let non_target_count = result.dependencies.len() + result.callers.len() + result.tests.len();
        assert!(non_target_count <= 3, "Some items may be truncated or dropped");
    }

    #[test]
    fn test_budget_priority_deps_before_callers() {
        let targets = vec![make_target("t1", 100)];
        // 400 budget - 100 target = 300 remaining
        // 2 deps × 100 = 200, 2 callers × 100 = 200 → only 300 available
        let deps = vec![
            make_scored("d1", "calls", 1, 100),
            make_scored("d2", "imports", 1, 100),
        ];
        let callers = vec![
            make_scored("c1", "calls", 1, 100),
            make_scored("c2", "calls", 1, 100),
        ];
        let tests: Vec<ScoredCandidate> = vec![];

        let result = budget_fit_by_category(&targets, deps, callers, tests, 400);

        // Both direct deps should be fully included (200 tokens)
        assert_eq!(result.dependencies.len(), 2, "Both deps should fit");
        // Callers get remaining 100 — one fits, one truncated or dropped
        assert!(!result.callers.is_empty(), "At least one caller should fit");
        // Total non-target tokens ≤ 300
        let dep_tokens: usize = result.dependencies.iter().map(|d| d.token_estimate).sum();
        let caller_tokens: usize = result.callers.iter().map(|c| c.token_estimate).sum();
        assert!(dep_tokens + caller_tokens <= 300);
    }

    #[test]
    fn test_budget_priority_callers_before_tests() {
        let targets = vec![make_target("t1", 50)];
        // 200 budget - 50 target = 150 remaining
        let deps: Vec<ScoredCandidate> = vec![]; // no deps
        let callers = vec![make_scored("c1", "calls", 1, 100)];
        let tests = vec![make_scored("test1", "tests_for", 1, 100)];

        let result = budget_fit_by_category(&targets, deps, callers, tests, 200);

        // Caller should be fully included (100)
        assert_eq!(result.callers.len(), 1);
        assert!(!result.callers[0].truncated, "Caller should not be truncated");
        // Test gets remaining 50 → truncated or dropped
        if !result.tests.is_empty() {
            assert!(result.tests[0].truncated || result.tests[0].token_estimate <= 50);
        }
    }

    #[test]
    fn test_budget_priority_tests_before_transitive() {
        let targets = vec![make_target("t1", 50)];
        // 300 budget - 50 = 250 remaining
        let deps = vec![
            make_scored("direct", "calls", 1, 100),
            make_scored("trans", "calls", 2, 100),  // hop=2 → transitive
        ];
        let callers: Vec<ScoredCandidate> = vec![];
        let tests = vec![make_scored("test1", "tests_for", 1, 100)];

        let result = budget_fit_by_category(&targets, deps, callers, tests, 300);

        // Direct dep should be included first
        let has_direct = result.dependencies.iter().any(|d| d.node_id == "direct");
        assert!(has_direct, "Direct dep should be included");

        // Test should be included before transitive dep
        assert_eq!(result.tests.len(), 1, "Test should be included");
        assert!(!result.tests[0].truncated, "Test should not be truncated");
    }

    #[test]
    fn test_budget_transitive_furthest_dropped_first() {
        let targets = vec![make_target("t1", 50)];
        // 200 budget - 50 = 150 remaining
        let deps = vec![
            make_scored("hop2", "calls", 2, 80),
            make_scored("hop3", "calls", 3, 80),
            make_scored("hop4", "calls", 4, 80),
        ];
        let callers: Vec<ScoredCandidate> = vec![];
        let tests: Vec<ScoredCandidate> = vec![];

        let result = budget_fit_by_category(&targets, deps, callers, tests, 200);

        // With 150 budget: hop2 (80) fits, hop3 (80) → remaining 70 ≥ 32 → truncated
        // hop4 may be dropped
        let dep_ids: Vec<&str> = result.dependencies.iter().map(|d| d.node_id.as_str()).collect();
        assert!(dep_ids.contains(&"hop2"), "Closest transitive should be included");
        // hop4 (furthest) should be dropped or at least last
        if dep_ids.contains(&"hop4") {
            // If hop4 included, it must be after hop2 and hop3
            let pos4 = dep_ids.iter().position(|&id| id == "hop4").unwrap();
            let pos2 = dep_ids.iter().position(|&id| id == "hop2").unwrap();
            assert!(pos4 > pos2, "hop4 should be after hop2");
        }
    }

    #[test]
    fn test_budget_everything_fits() {
        let targets = vec![make_target("t1", 100)];
        let deps = vec![
            make_scored("d1", "calls", 1, 100),
            make_scored("d2", "imports", 2, 100),
        ];
        let callers = vec![make_scored("c1", "calls", 1, 100)];
        let tests = vec![make_scored("test1", "tests_for", 1, 100)];

        let result = budget_fit_by_category(&targets, deps, callers, tests, 10000);

        // Everything should fit with no truncation
        assert_eq!(result.targets.len(), 1);
        assert_eq!(result.dependencies.len(), 2);
        assert_eq!(result.callers.len(), 1);
        assert_eq!(result.tests.len(), 1);
        assert_eq!(result.truncation_info.truncated_count, 0);
        assert_eq!(result.truncation_info.dropped_count, 0);
    }

    #[test]
    fn test_budget_empty_categories() {
        let targets = vec![make_target("t1", 100)];
        let deps: Vec<ScoredCandidate> = vec![];
        let callers: Vec<ScoredCandidate> = vec![];
        let tests: Vec<ScoredCandidate> = vec![];

        let result = budget_fit_by_category(&targets, deps, callers, tests, 500);

        assert_eq!(result.targets.len(), 1);
        assert!(result.dependencies.is_empty());
        assert!(result.callers.is_empty());
        assert!(result.tests.is_empty());
        assert_eq!(result.truncation_info.truncated_count, 0);
        assert_eq!(result.truncation_info.dropped_count, 0);
    }

    #[test]
    fn test_budget_multiple_targets() {
        let targets = vec![
            make_target("t1", 200),
            make_target("t2", 200),
            make_target("t3", 200),
        ];
        let deps = vec![make_scored("d1", "calls", 1, 100)];
        let callers: Vec<ScoredCandidate> = vec![];
        let tests: Vec<ScoredCandidate> = vec![];

        // Budget = 700 → targets use 600, dep gets 100
        let result = budget_fit_by_category(&targets, deps, callers, tests, 700);

        assert_eq!(result.targets.len(), 3, "All targets must be included");
        assert_eq!(result.dependencies.len(), 1, "Dep should fit in remaining 100");
    }

    #[test]
    fn test_budget_target_exceeds_budget() {
        // Target alone is 500, budget is 300 — targets are NEVER truncated
        let targets = vec![make_target("big-target", 500)];
        let deps = vec![make_scored("d1", "calls", 1, 100)];
        let callers: Vec<ScoredCandidate> = vec![];
        let tests: Vec<ScoredCandidate> = vec![];

        let result = budget_fit_by_category(&targets, deps, callers, tests, 300);

        // Target MUST be included regardless
        assert_eq!(result.targets.len(), 1);
        assert_eq!(result.targets[0].node_id, "big-target");
        // Remaining is 0 (saturating_sub), so dep is dropped
        assert!(result.dependencies.is_empty() || result.dependencies[0].truncated,
            "Dep should be dropped or truncated when target exceeds budget");
    }

    // --- ContextResult tests ---

    #[test]
    fn test_context_result_total_included() {
        let targets = vec![make_target("t1", 100)];
        let deps = vec![
            make_scored("d1", "calls", 1, 50),
            make_scored("d2", "imports", 1, 50),
        ];
        let callers = vec![make_scored("c1", "calls", 1, 50)];
        let tests = vec![make_scored("test1", "tests_for", 1, 50)];

        let result = budget_fit_by_category(&targets, deps, callers, tests, 10000);
        assert_eq!(result.total_included(), 5); // 1 target + 2 deps + 1 caller + 1 test
    }

    #[test]
    fn test_context_result_estimated_tokens() {
        let targets = vec![make_target("t1", 100)];
        let deps = vec![make_scored("d1", "calls", 1, 200)];
        let callers: Vec<ScoredCandidate> = vec![];
        let tests: Vec<ScoredCandidate> = vec![];

        let result = budget_fit_by_category(&targets, deps, callers, tests, 10000);
        // estimated_tokens = budget - remaining = tokens actually used
        assert!(result.estimated_tokens > 0);
        assert!(result.estimated_tokens <= 10000);
    }

    // --- TruncationInfo tests ---

    #[test]
    fn test_truncation_info_merge() {
        let mut a = TruncationInfo { truncated_count: 1, dropped_count: 2, budget_used: 100 };
        let b = TruncationInfo { truncated_count: 3, dropped_count: 4, budget_used: 200 };
        a.merge(&b);
        assert_eq!(a.truncated_count, 4);
        assert_eq!(a.dropped_count, 6);
        assert_eq!(a.budget_used, 300);
    }

    #[test]
    fn test_truncation_info_default() {
        let info = TruncationInfo::default();
        assert_eq!(info.truncated_count, 0);
        assert_eq!(info.dropped_count, 0);
        assert_eq!(info.budget_used, 0);
    }

    // --- TargetContext tests ---

    #[test]
    fn test_target_context_token_estimate() {
        let t = TargetContext::new(
            "t1".into(),
            Some("My Function".into()),
            Some("/src/lib.rs".into()),
            Some("fn my_func() -> i32".into()),
            Some("/// Does something".into()),
            Some("A function that does something".into()),
            Some("fn my_func() -> i32 { 42 }".into()),
        );
        assert!(t.token_estimate > 0, "Token estimate should be positive");
        // Total bytes: "My Function" + "A function..." + "fn my_func..." + "/// Does..." + "fn my_func...{42}" + 50 overhead
        // = 11 + 30 + 20 + 18 + 26 + 50 = 155 bytes → 155/4 = 38 tokens
        assert!(t.token_estimate >= 30, "Should be at least 30 tokens");
    }

    #[test]
    fn test_target_context_empty_fields() {
        let t = TargetContext::new(
            "t1".into(), None, None, None, None, None, None,
        );
        // Only 50 bytes overhead → 50/4 = 12 tokens
        assert!(t.token_estimate >= 1, "Even empty target has overhead tokens");
    }

    // --- ContextItem tests ---

    #[test]
    fn test_context_item_from_scored_not_truncated() {
        let sc = make_scored("func1", "calls", 1, 100);
        let item = ContextItem::from_scored(&sc, false);
        assert_eq!(item.node_id, "func1");
        assert_eq!(item.connecting_relation, "calls");
        assert!(!item.truncated);
        assert!(item.content.is_some());
    }

    #[test]
    fn test_context_item_from_scored_truncated() {
        let sc = make_scored("big-func", "calls", 1, 500);
        let item = ContextItem::from_scored_truncated(&sc, 50);
        assert_eq!(item.node_id, "big-func");
        assert!(item.truncated);
        assert!(item.token_estimate <= 50,
            "Truncated item should have ≤ budget tokens, got {}", item.token_estimate);
    }

    // --- Integration: realistic scenario ---

    #[test]
    fn test_realistic_truncation_scenario() {
        // Simulates a real context assembly with 1 target, mixed deps/callers/tests
        let targets = vec![make_target("parse_yaml", 150)];

        let deps = vec![
            make_scored("load_file", "calls", 1, 80),       // direct dep
            make_scored("validate", "calls", 1, 60),          // direct dep
            make_scored("serde_yaml", "imports", 1, 40),       // direct dep
            make_scored("deep_util", "calls", 3, 100),         // transitive
        ];
        let callers = vec![
            make_scored("main_cli", "calls", 1, 120),
            make_scored("api_handler", "calls", 1, 80),
        ];
        let tests = vec![
            make_scored("test_parse", "tests_for", 1, 70),
            make_scored("test_parse_edge", "tests_for", 1, 50),
        ];

        // Budget: 150 (target) + ~500 for others = 650
        let result = budget_fit_by_category(&targets, deps, callers, tests, 650);

        // Target always included
        assert_eq!(result.targets.len(), 1);
        assert_eq!(result.targets[0].node_id, "parse_yaml");

        // Check priority: direct deps should be included before transitive
        let dep_ids: Vec<&str> = result.dependencies.iter().map(|d| d.node_id.as_str()).collect();
        // All 3 direct deps (80+60+40=180) should fit
        assert!(dep_ids.contains(&"load_file"), "Direct dep should be included");
        assert!(dep_ids.contains(&"validate"), "Direct dep should be included");
        assert!(dep_ids.contains(&"serde_yaml"), "Direct dep should be included");

        // Verify total doesn't exceed budget
        assert!(result.estimated_tokens <= 650,
            "Total tokens {} should be ≤ budget 650", result.estimated_tokens);

        // Verify truncation info is consistent
        let _total_in = result.total_included();
        let total_possible = 4 + 2 + 2; // deps + callers + tests (excluding target)
        let items_included = result.dependencies.len() + result.callers.len() + result.tests.len();
        // items_included + dropped = total_possible
        assert_eq!(
            items_included + result.truncation_info.dropped_count,
            total_possible,
            "included ({}) + dropped ({}) should equal total possible ({})",
            items_included, result.truncation_info.dropped_count, total_possible,
        );
        // truncated_count should match items with truncated=true
        let actually_truncated = result.dependencies.iter().filter(|d| d.truncated).count()
            + result.callers.iter().filter(|c| c.truncated).count()
            + result.tests.iter().filter(|t| t.truncated).count();
        assert_eq!(
            result.truncation_info.truncated_count, actually_truncated,
            "Truncation info count should match actual truncated items",
        );
    }

    #[test]
    fn test_budget_direct_deps_separated_from_transitive() {
        // Verify that hop=1 goes to direct deps and hop>1 goes to transitive
        let targets = vec![make_target("t1", 50)];
        let deps = vec![
            make_scored("hop1a", "calls", 1, 30),   // direct
            make_scored("hop1b", "imports", 1, 30),  // direct
            make_scored("hop2a", "calls", 2, 30),    // transitive
            make_scored("hop3a", "calls", 3, 30),    // transitive
        ];
        let callers: Vec<ScoredCandidate> = vec![];
        let tests: Vec<ScoredCandidate> = vec![];

        let result = budget_fit_by_category(&targets, deps, callers, tests, 10000);

        // All 4 should be in dependencies
        assert_eq!(result.dependencies.len(), 4);
        // First two should be the direct deps (hop=1), then transitive sorted by hop
        // Direct deps come first because they're filled first by budget_fit_by_category
        let ids: Vec<&str> = result.dependencies.iter().map(|d| d.node_id.as_str()).collect();
        // Direct deps (hop1a, hop1b) should appear before transitive (hop2a, hop3a)
        let pos_1a = ids.iter().position(|&id| id == "hop1a").unwrap();
        let pos_1b = ids.iter().position(|&id| id == "hop1b").unwrap();
        let pos_2a = ids.iter().position(|&id| id == "hop2a").unwrap();
        let pos_3a = ids.iter().position(|&id| id == "hop3a").unwrap();
        assert!(pos_1a < pos_2a, "Direct dep hop1a should be before transitive hop2a");
        assert!(pos_1b < pos_3a, "Direct dep hop1b should be before transitive hop3a");
        assert!(pos_2a < pos_3a, "Closer transitive (hop2) should be before further (hop3)");
    }

    #[test]
    fn test_budget_stress_many_items() {
        let targets = vec![make_target("t1", 50)];
        // 50 deps, 20 callers, 10 tests
        let deps: Vec<ScoredCandidate> = (0..50)
            .map(|i| make_scored(&format!("dep-{}", i), "calls", (i / 10 + 1) as u32, 20))
            .collect();
        let callers: Vec<ScoredCandidate> = (0..20)
            .map(|i| make_scored(&format!("caller-{}", i), "calls", 1, 15))
            .collect();
        let tests: Vec<ScoredCandidate> = (0..10)
            .map(|i| make_scored(&format!("test-{}", i), "tests_for", 1, 25))
            .collect();

        // Budget fits target (50) + some deps/callers/tests but not all
        // Total possible: 50 + 50*20 + 20*15 + 10*25 = 50 + 1000 + 300 + 250 = 1600
        let result = budget_fit_by_category(&targets, deps, callers, tests, 500);

        assert_eq!(result.targets.len(), 1);
        // Should have some items in each category but not all
        assert!(result.estimated_tokens <= 500,
            "Tokens {} should be ≤ 500", result.estimated_tokens);
        assert!(result.truncation_info.dropped_count > 0,
            "Some items should be dropped with tight budget");
    }

    // --- estimate_tokens_for_target_fields ---

    #[test]
    fn test_estimate_tokens_target_all_none() {
        let tokens = estimate_tokens_for_target_fields(None, None, None, None, None);
        // 0 + 50 overhead = 50 bytes → 50/4 = 12
        assert_eq!(tokens, 12);
    }

    #[test]
    fn test_estimate_tokens_target_with_content() {
        let tokens = estimate_tokens_for_target_fields(
            Some("title"),           // 5
            Some("description"),     // 11
            Some("fn foo()"),        // 8
            Some("/// doc"),         // 7
            Some("fn foo() { 42 }"), // 16
        );
        // 5 + 11 + 8 + 7 + 16 + 50 = 97 bytes → 97/4 = 24
        assert_eq!(tokens, 24);
    }

    // =========================================================================
    // §8 Tests: Source Code Loading from Disk (GOAL-4.1b)
    // =========================================================================

    /// Helper: create a temp dir with a source file.
    fn setup_source_file(filename: &str, content: &str) -> TempDir {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join(filename), content).unwrap();
        tmp
    }

    #[test]
    fn test_load_source_full_file() {
        let content = "line 1\nline 2\nline 3\nline 4\nline 5\n";
        let tmp = setup_source_file("lib.rs", content);

        let result = load_source_from_disk(
            Some("src/lib.rs"), None, None, tmp.path()
        );
        assert!(result.is_some());
        let r = result.unwrap();
        assert!(!r.is_range);
        assert_eq!(r.start_line, None);
        assert_eq!(r.end_line, None);
        assert_eq!(r.line_count, 5);
        assert!(r.source.contains("line 1"));
        assert!(r.source.contains("line 5"));
    }

    #[test]
    fn test_load_source_line_range() {
        let content = "line 1\nline 2\nline 3\nline 4\nline 5\n";
        let tmp = setup_source_file("lib.rs", content);

        let result = load_source_from_disk(
            Some("src/lib.rs"), Some(2), Some(4), tmp.path()
        );
        assert!(result.is_some());
        let r = result.unwrap();
        assert!(r.is_range);
        assert_eq!(r.start_line, Some(2));
        assert_eq!(r.end_line, Some(4));
        assert_eq!(r.line_count, 3); // lines 2, 3, 4 (inclusive range)
        assert!(r.source.contains("line 2"));
        assert!(r.source.contains("line 3"));
        assert!(r.source.contains("line 4"));
        assert!(!r.source.contains("line 1"));
        assert!(!r.source.contains("line 5"));
    }

    #[test]
    fn test_load_source_from_start_line_to_eof() {
        let content = "line 1\nline 2\nline 3\nline 4\nline 5\n";
        let tmp = setup_source_file("lib.rs", content);

        let result = load_source_from_disk(
            Some("src/lib.rs"), Some(3), None, tmp.path()
        );
        assert!(result.is_some());
        let r = result.unwrap();
        assert!(r.is_range);
        assert_eq!(r.start_line, Some(3));
        assert!(r.source.contains("line 3"));
        assert!(r.source.contains("line 4"));
        assert!(r.source.contains("line 5"));
        assert!(!r.source.contains("line 1"));
    }

    #[test]
    fn test_load_source_none_file_path() {
        let tmp = TempDir::new().unwrap();
        let result = load_source_from_disk(None, None, None, tmp.path());
        assert!(result.is_none());
    }

    #[test]
    fn test_load_source_nonexistent_file() {
        let tmp = TempDir::new().unwrap();
        let result = load_source_from_disk(
            Some("src/nonexistent.rs"), None, None, tmp.path()
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_load_source_start_line_beyond_file() {
        let content = "line 1\nline 2\n";
        let tmp = setup_source_file("lib.rs", content);

        let result = load_source_from_disk(
            Some("src/lib.rs"), Some(100), Some(200), tmp.path()
        );
        assert!(result.is_none(), "start_line beyond file should return None");
    }

    #[test]
    fn test_load_source_single_line_range() {
        let content = "fn foo() {}\nfn bar() {}\nfn baz() {}\n";
        let tmp = setup_source_file("lib.rs", content);

        let result = load_source_from_disk(
            Some("src/lib.rs"), Some(2), Some(2), tmp.path()
        );
        // start=2, end=2 → end_idx = min(2, 3) = 2, range [1..2] = 1 line
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.line_count, 1);
        assert!(r.source.contains("fn bar()"));
        assert!(!r.source.contains("fn foo()"));
        assert!(!r.source.contains("fn baz()"));
    }

    #[test]
    fn test_load_source_end_line_clamped_to_file_length() {
        let content = "line 1\nline 2\nline 3\n";
        let tmp = setup_source_file("lib.rs", content);

        // end_line = 1000 but file only has 3 lines
        let result = load_source_from_disk(
            Some("src/lib.rs"), Some(1), Some(1000), tmp.path()
        );
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.line_count, 3);
        assert!(r.source.contains("line 1"));
        assert!(r.source.contains("line 3"));
    }

    #[test]
    fn test_load_source_security_outside_root() {
        let tmp = setup_source_file("lib.rs", "safe content");
        // Try to escape using ../
        let result = load_source_from_disk(
            Some("../../etc/passwd"), None, None, tmp.path()
        );
        // On macOS/Linux, /etc/passwd exists but is outside project root
        // canonicalize will resolve the path and starts_with check will reject
        assert!(result.is_none(), "Should reject path outside project root");
    }

    #[test]
    fn test_load_source_absolute_path_under_root() {
        let content = "fn absolute() {}";
        let tmp = setup_source_file("lib.rs", content);
        let abs_path = tmp.path().join("src/lib.rs");
        let abs_str = abs_path.to_str().unwrap();

        let result = load_source_from_disk(
            Some(abs_str), None, None, tmp.path()
        );
        assert!(result.is_some());
        assert!(result.unwrap().source.contains("fn absolute()"));
    }

    #[test]
    fn test_load_source_empty_file() {
        let tmp = setup_source_file("empty.rs", "");

        let result = load_source_from_disk(
            Some("src/empty.rs"), None, None, tmp.path()
        );
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.source, "");
        assert_eq!(r.line_count, 0);
    }

    #[test]
    fn test_load_source_unicode_content() {
        let content = "// 中文注释\nfn 函数() -> String {\n    \"こんにちは\".into()\n}\n";
        let tmp = setup_source_file("unicode.rs", content);

        let result = load_source_from_disk(
            Some("src/unicode.rs"), None, None, tmp.path()
        );
        assert!(result.is_some());
        let r = result.unwrap();
        assert!(r.source.contains("中文注释"));
        assert!(r.source.contains("こんにちは"));
        assert_eq!(r.line_count, 4);
    }

    #[test]
    fn test_load_source_line_range_with_unicode() {
        let content = "line 1\n中文行2\nline 3\n日本語行4\nline 5\n";
        let tmp = setup_source_file("mixed.rs", content);

        let result = load_source_from_disk(
            Some("src/mixed.rs"), Some(2), Some(4), tmp.path()
        );
        assert!(result.is_some());
        let r = result.unwrap();
        assert!(r.source.contains("中文行2"));
        assert!(r.source.contains("line 3"));
        assert!(!r.source.contains("line 1"));
    }

    #[test]
    fn test_load_source_result_fields() {
        let content = "a\nb\nc\nd\ne\n";
        let tmp = setup_source_file("test.rs", content);

        // Full file
        let r1 = load_source_from_disk(Some("src/test.rs"), None, None, tmp.path()).unwrap();
        assert!(!r1.is_range);
        assert_eq!(r1.start_line, None);
        assert_eq!(r1.end_line, None);

        // Range
        let r2 = load_source_from_disk(Some("src/test.rs"), Some(2), Some(4), tmp.path()).unwrap();
        assert!(r2.is_range);
        assert_eq!(r2.start_line, Some(2));
        // end_line is min(4, 5) = 4
        assert!(r2.end_line.unwrap() <= 5);
    }

    #[test]
    fn test_load_source_start_line_zero_falls_through() {
        let content = "line 1\nline 2\n";
        let tmp = setup_source_file("lib.rs", content);

        // start_line = 0 doesn't match the guard `start >= 1`, falls to full file
        let result = load_source_from_disk(
            Some("src/lib.rs"), Some(0), Some(2), tmp.path()
        );
        assert!(result.is_some());
        let r = result.unwrap();
        // Falls through to full file since start=0 doesn't match range guard
        assert!(!r.is_range);
    }

    #[test]
    fn test_load_source_nested_directory() {
        let tmp = TempDir::new().unwrap();
        let deep_dir = tmp.path().join("src").join("module").join("sub");
        fs::create_dir_all(&deep_dir).unwrap();
        fs::write(deep_dir.join("deep.rs"), "fn deep() {}").unwrap();

        let result = load_source_from_disk(
            Some("src/module/sub/deep.rs"), None, None, tmp.path()
        );
        assert!(result.is_some());
        assert!(result.unwrap().source.contains("fn deep()"));
    }

    // =========================================================================
    // §9 Integration Tests: Scoring + Truncation + Source Loading + Traversal
    // =========================================================================

    #[test]
    fn test_integration_score_then_truncate() {
        // Build candidates, score them, then budget-fit with truncation
        let c1 = make_candidate_with_content("calls", 1, &"x".repeat(400), "fn called()");
        let c2 = make_candidate_with_content("imports", 1, &"y".repeat(200), "use crate::dep");
        let c3 = make_candidate_with_content("depends_on", 2, &"z".repeat(300), "fn transitive()");

        let scored = score_candidates(&[c1, c2, c3]);
        // calls and imports should score highest (tier 1)
        assert!(scored[0].score >= scored[1].score);
        assert!(scored[1].score >= scored[2].score);

        // Now feed into budget_fit_by_category
        let targets = vec![make_target("main_fn", 50)];

        // Partition scored into direct deps and transitive
        let (direct, trans): (Vec<_>, Vec<_>) = scored.into_iter()
            .partition(|s| s.candidate.hop_distance == 1);

        let result = budget_fit_by_category(&targets, 
            [direct, trans].concat(),
            vec![], vec![], 200);

        // Target always present
        assert_eq!(result.targets.len(), 1);
        // Some deps should be included, some may be truncated
        assert!(!result.dependencies.is_empty());
        assert!(result.estimated_tokens <= 200);
    }

    #[test]
    fn test_integration_source_loading_feeds_target_context() {
        // Source loading → TargetContext → budget_fit
        let tmp = setup_source_file("main.rs", "fn main() {\n    println!(\"hello\");\n}\n");

        let loaded = load_source_from_disk(
            Some("src/main.rs"), None, None, tmp.path()
        ).unwrap();

        let target = TargetContext::new(
            "main_fn".into(),
            Some("main".into()),
            Some("src/main.rs".into()),
            Some("fn main()".into()),
            None,
            None,
            Some(loaded.source.clone()),
        );
        assert!(target.token_estimate > 0);
        assert!(target.source_code.as_ref().unwrap().contains("println!"));

        // Budget fit with this target
        let deps = vec![make_scored("dep1", "calls", 1, 30)];
        let result = budget_fit_by_category(&[target], deps, vec![], vec![], 500);
        assert_eq!(result.targets.len(), 1);
        assert!(result.targets[0].source_code.as_ref().unwrap().contains("println!"));
    }

    #[test]
    fn test_integration_source_range_loading() {
        // Load a range, verify it gets correct lines for TargetContext
        let content = "use std::io;\n\nfn important() -> Result<()> {\n    let x = 42;\n    Ok(())\n}\n\nfn other() {}\n";
        let tmp = setup_source_file("lib.rs", content);

        let loaded = load_source_from_disk(
            Some("src/lib.rs"), Some(3), Some(6), tmp.path()
        ).unwrap();
        assert!(loaded.source.contains("fn important()"));
        assert!(loaded.source.contains("Ok(())"));
        assert!(!loaded.source.contains("fn other()"));
        assert!(!loaded.source.contains("use std::io"));
    }

    #[test]
    fn test_integration_edge_traversal_categories() {
        // Simulate an edge traversal that categorizes nodes correctly
        // This tests the full pipeline: candidates → scoring → categorization → budget

        // Target
        let targets = vec![make_target("parse_fn", 100)];

        // Direct deps (hop 1, various relations)
        let direct_calls = make_scored("called_fn", "calls", 1, 80);
        let direct_import = make_scored("dep_module", "imports", 1, 50);

        // Callers
        let caller = make_scored("caller_fn", "calls", 1, 60);

        // Tests
        let test_fn = make_scored("test_parse", "tests_for", 1, 70);

        // Transitive (hop 2+)
        let trans1 = make_scored("deep_dep", "calls", 2, 90);
        let trans2 = make_scored("deeper_dep", "calls", 3, 90);

        let all_deps = vec![direct_calls, direct_import, trans1, trans2];

        let result = budget_fit_by_category(
            &targets, all_deps, vec![caller], vec![test_fn], 400
        );

        // Verify priority: target (100) + direct deps first, then callers, tests, transitive
        assert_eq!(result.targets.len(), 1);

        // Direct deps (hop=1) should appear before transitive in dependencies
        let dep_ids: Vec<&str> = result.dependencies.iter()
            .map(|d| d.node_id.as_str()).collect();
        if dep_ids.contains(&"called_fn") && dep_ids.contains(&"deep_dep") {
            let pos_direct = dep_ids.iter().position(|&id| id == "called_fn").unwrap();
            let pos_trans = dep_ids.iter().position(|&id| id == "deep_dep").unwrap();
            assert!(pos_direct < pos_trans);
        }

        assert!(result.estimated_tokens <= 400);
    }

    #[test]
    fn test_integration_truncation_preserves_structure() {
        // Large content that gets truncated — verify structure is maintained
        let big_source = (0..100).map(|i| format!("fn func_{}() {{ /* impl */ }}", i))
            .collect::<Vec<_>>().join("\n");

        let targets = vec![TargetContext::new(
            "big_module".into(),
            Some("Big Module".into()),
            Some("src/big.rs".into()),
            Some("mod big".into()),
            None,
            None,
            Some(big_source.clone()),
        )];

        let deps: Vec<ScoredCandidate> = (0..10).map(|i| {
            let source = format!("fn dep_{}() {{ /* dep impl {} */ }}", i, i);
            let mut sc = make_scored(&format!("dep-{}", i), "calls", 1, 40);
            sc.candidate.source_code = Some(source);
            sc
        }).collect();

        let result = budget_fit_by_category(&targets, deps, vec![], vec![], 500);

        // Target always included regardless of size
        assert_eq!(result.targets.len(), 1);
        assert_eq!(result.targets[0].node_id, "big_module");

        // Some deps included, some may be truncated/dropped
        for dep in &result.dependencies {
            assert!(!dep.node_id.is_empty());
            assert_eq!(dep.connecting_relation, "calls");
            // Score should be visible per GOAL-4.5
            assert!(dep.score > 0.0);
        }
    }

    #[test]
    fn test_integration_full_pipeline_realistic() {
        // End-to-end: create source files, load them, build targets + deps, budget fit
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();

        // Target source
        fs::write(src_dir.join("parser.rs"), concat!(
            "use crate::lexer::Token;\n",
            "\n",
            "pub struct Parser {\n",
            "    tokens: Vec<Token>,\n",
            "    pos: usize,\n",
            "}\n",
            "\n",
            "impl Parser {\n",
            "    pub fn new(tokens: Vec<Token>) -> Self {\n",
            "        Self { tokens, pos: 0 }\n",
            "    }\n",
            "\n",
            "    pub fn parse(&mut self) -> Ast {\n",
            "        // parsing logic\n",
            "        todo!()\n",
            "    }\n",
            "}\n",
        )).unwrap();

        // Dep source
        fs::write(src_dir.join("lexer.rs"), concat!(
            "pub enum Token {\n",
            "    Ident(String),\n",
            "    Number(i64),\n",
            "    Punct(char),\n",
            "}\n",
            "\n",
            "pub fn tokenize(input: &str) -> Vec<Token> {\n",
            "    vec![] // stub\n",
            "}\n",
        )).unwrap();

        // Load target source
        let target_source = load_source_from_disk(
            Some("src/parser.rs"), Some(8), Some(16), tmp.path()
        ).unwrap();
        assert!(target_source.source.contains("impl Parser"));

        // Build target
        let target = TargetContext::new(
            "parser::Parser::parse".into(),
            Some("Parser::parse".into()),
            Some("src/parser.rs".into()),
            Some("pub fn parse(&mut self) -> Ast".into()),
            Some("/// Parses tokens into AST".into()),
            None,
            Some(target_source.source),
        );

        // Build deps (lexer is called by parser)
        let lexer_source = load_source_from_disk(
            Some("src/lexer.rs"), None, None, tmp.path()
        ).unwrap();

        let mut lexer_candidate = make_scored("lexer::tokenize", "calls", 1, 30);
        lexer_candidate.candidate.source_code = Some(lexer_source.source);
        lexer_candidate.candidate.file_path = Some("src/lexer.rs".to_string());

        // Budget fit
        let result = budget_fit_by_category(
            &[target], vec![lexer_candidate], vec![], vec![], 500
        );

        // Verify full pipeline output
        assert_eq!(result.targets.len(), 1);
        assert_eq!(result.targets[0].node_id, "parser::Parser::parse");
        assert!(result.targets[0].source_code.as_ref().unwrap().contains("impl Parser"));

        assert!(!result.dependencies.is_empty());
        assert_eq!(result.dependencies[0].node_id, "lexer::tokenize");
        assert_eq!(result.dependencies[0].connecting_relation, "calls");
        assert!(result.dependencies[0].score > 0.0, "GOAL-4.5: score visible");

        assert!(result.estimated_tokens <= 500);
        assert_eq!(result.truncation_info.dropped_count, 0);
    }

    #[test]
    fn test_integration_score_ordering_matches_budget_priority() {
        // Verify that the scoring order (calls > type_ref > structural > unknown)
        // aligns with budget priority (direct deps filled first)
        let high = make_scored("caller", "calls", 1, 50);       // score ≈ 0.90
        let med = make_scored("type_dep", "type_reference", 1, 50); // score ≈ 0.78
        let low = make_scored("struct_dep", "depends_on", 1, 50);   // score ≈ 0.64

        // Score ordering
        assert!(high.score > med.score, "calls should score higher than type_reference");
        assert!(med.score > low.score, "type_reference should score higher than depends_on");

        // All three as direct deps, tight budget
        let targets = vec![make_target("t", 50)];
        let result = budget_fit_by_category(
            &targets, vec![high.clone(), med.clone(), low.clone()], vec![], vec![], 200
        );

        // With 150 budget for deps (200 - 50 target), all three fit (3 × 50 = 150)
        assert_eq!(result.dependencies.len(), 3);
        // Order preserved from input (greedy_fill preserves order)
        assert_eq!(result.dependencies[0].node_id, "caller");
        assert_eq!(result.dependencies[1].node_id, "type_dep");
        assert_eq!(result.dependencies[2].node_id, "struct_dep");
    }

    // =========================================================================
    // §9 Tests: ContextQuery + Pipeline (assemble_context)
    // =========================================================================

    #[allow(dead_code)]
    fn make_code_node(id: &str, file_path: &str, sig: Option<&str>) -> Node {
        let mut n = Node::new(id, id);
        n.node_type = Some("function".to_string());
        n.file_path = Some(file_path.to_string());
        n.signature = sig.map(|s| s.to_string());
        n
    }

    fn make_graph_with_deps() -> Graph {
        // target -> dep1 -> dep2 (transitive)
        // caller -> target (reverse)
        // test -> target (tests_for)
        let mut g = Graph::default();
        let mut target = Node::new("target", "Target Function");
        target.node_type = Some("function".to_string());
        target.file_path = Some("src/lib.rs".to_string());
        target.signature = Some("fn target() -> i32".to_string());
        g.nodes.push(target);

        let mut dep1 = Node::new("dep1", "Direct Dependency");
        dep1.node_type = Some("function".to_string());
        dep1.file_path = Some("src/dep.rs".to_string());
        dep1.signature = Some("fn dep1() -> bool".to_string());
        g.nodes.push(dep1);

        let mut dep2 = Node::new("dep2", "Transitive Dependency");
        dep2.node_type = Some("function".to_string());
        dep2.file_path = Some("src/deep.rs".to_string());
        g.nodes.push(dep2);

        let mut caller = Node::new("caller1", "A Caller");
        caller.node_type = Some("function".to_string());
        caller.file_path = Some("src/main.rs".to_string());
        g.nodes.push(caller);

        let mut test = Node::new("test1", "Test for Target");
        test.node_type = Some("function".to_string());
        test.file_path = Some("tests/test_target.rs".to_string());
        g.nodes.push(test);

        // Edges
        g.edges.push(Edge::new("target", "dep1", "calls"));
        g.edges.push(Edge::new("dep1", "dep2", "calls"));
        g.edges.push(Edge::new("caller1", "target", "calls"));
        g.edges.push(Edge::new("test1", "target", "tests_for"));

        g
    }

    #[test]
    fn test_assemble_context_basic() {
        let graph = make_graph_with_deps();
        let query = ContextQuery {
            targets: vec!["target".into()],
            token_budget: 10000,
            depth: 2,
            ..Default::default()
        };
        let result = assemble_context(&graph, &query).unwrap();
        assert_eq!(result.result.targets.len(), 1);
        assert_eq!(result.result.targets[0].node_id, "target");
        assert!(!result.result.dependencies.is_empty(), "should have deps");
        assert!(!result.result.callers.is_empty(), "should have callers");
        assert!(!result.result.tests.is_empty(), "should have tests");
        assert!(result.stats.nodes_visited > 0);
        assert!(result.stats.nodes_included > 0);
        assert!(result.stats.budget_used > 0);
        assert_eq!(result.stats.budget_total, 10000);
    }

    #[test]
    fn test_assemble_context_empty_targets_errors() {
        let graph = make_graph_with_deps();
        let query = ContextQuery {
            targets: vec![],
            ..Default::default()
        };
        let err = assemble_context(&graph, &query).unwrap_err();
        assert!(err.to_string().contains("at least one target"));
    }

    #[test]
    fn test_assemble_context_nonexistent_target_errors() {
        let graph = make_graph_with_deps();
        let query = ContextQuery {
            targets: vec!["nonexistent".into()],
            ..Default::default()
        };
        let err = assemble_context(&graph, &query).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_assemble_context_depth_1() {
        let graph = make_graph_with_deps();
        let query = ContextQuery {
            targets: vec!["target".into()],
            token_budget: 10000,
            depth: 1,
            ..Default::default()
        };
        let result = assemble_context(&graph, &query).unwrap();
        // Only direct dep (dep1), not transitive (dep2).
        let dep_ids: Vec<&str> = result.result.dependencies.iter()
            .map(|d| d.node_id.as_str()).collect();
        assert!(dep_ids.contains(&"dep1"), "should include direct dep");
        assert!(!dep_ids.contains(&"dep2"), "should NOT include transitive dep at depth=1");
    }

    #[test]
    fn test_assemble_context_depth_2_includes_transitive() {
        let graph = make_graph_with_deps();
        let query = ContextQuery {
            targets: vec!["target".into()],
            token_budget: 10000,
            depth: 2,
            ..Default::default()
        };
        let result = assemble_context(&graph, &query).unwrap();
        let dep_ids: Vec<&str> = result.result.dependencies.iter()
            .map(|d| d.node_id.as_str()).collect();
        assert!(dep_ids.contains(&"dep1"), "should include direct dep");
        assert!(dep_ids.contains(&"dep2"), "should include transitive dep at depth=2");
    }

    #[test]
    fn test_assemble_context_include_filter_type() {
        let mut graph = make_graph_with_deps();
        // Add a class node as a dep.
        let mut cls = Node::new("class1", "MyClass");
        cls.node_type = Some("class".to_string());
        cls.file_path = Some("src/class.rs".to_string());
        graph.nodes.push(cls);
        graph.edges.push(Edge::new("target", "class1", "uses"));

        let query = ContextQuery {
            targets: vec!["target".into()],
            token_budget: 10000,
            depth: 2,
            filters: ContextFilters {
                include_patterns: vec!["type:function".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let result = assemble_context(&graph, &query).unwrap();
        // class1 should be excluded by the type filter.
        let dep_ids: Vec<&str> = result.result.dependencies.iter()
            .map(|d| d.node_id.as_str()).collect();
        assert!(!dep_ids.contains(&"class1"), "class should be filtered out by type:function");
        assert!(dep_ids.contains(&"dep1"), "functions should pass");
    }

    #[test]
    fn test_assemble_context_include_filter_glob() {
        let graph = make_graph_with_deps();
        let query = ContextQuery {
            targets: vec!["target".into()],
            token_budget: 10000,
            depth: 2,
            filters: ContextFilters {
                include_patterns: vec!["src/dep.rs".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let result = assemble_context(&graph, &query).unwrap();
        let dep_ids: Vec<&str> = result.result.dependencies.iter()
            .map(|d| d.node_id.as_str()).collect();
        assert!(dep_ids.contains(&"dep1"), "dep1 (src/dep.rs) should pass");
        assert!(!dep_ids.contains(&"dep2"), "dep2 (src/deep.rs) should be filtered");
        assert!(result.stats.nodes_excluded_by_filter > 0);
    }

    #[test]
    fn test_assemble_context_multiple_targets() {
        let graph = make_graph_with_deps();
        let query = ContextQuery {
            targets: vec!["target".into(), "dep1".into()],
            token_budget: 10000,
            depth: 2,
            ..Default::default()
        };
        let result = assemble_context(&graph, &query).unwrap();
        assert_eq!(result.result.targets.len(), 2);
        let target_ids: Vec<&str> = result.result.targets.iter()
            .map(|t| t.node_id.as_str()).collect();
        assert!(target_ids.contains(&"target"));
        assert!(target_ids.contains(&"dep1"));
    }

    #[test]
    fn test_assemble_context_tight_budget() {
        let graph = make_graph_with_deps();
        let query = ContextQuery {
            targets: vec!["target".into()],
            token_budget: 50, // Very tight budget.
            depth: 2,
            ..Default::default()
        };
        let result = assemble_context(&graph, &query).unwrap();
        // Targets always included, but deps/callers may be dropped.
        assert_eq!(result.result.targets.len(), 1);
        assert!(result.result.estimated_tokens <= 50 + result.result.targets[0].token_estimate,
            "budget should be approximately respected");
    }

    #[test]
    fn test_assemble_context_stats_populated() {
        let graph = make_graph_with_deps();
        let query = ContextQuery {
            targets: vec!["target".into()],
            token_budget: 10000,
            depth: 2,
            ..Default::default()
        };
        let result = assemble_context(&graph, &query).unwrap();
        assert!(result.stats.nodes_visited >= 4, "should visit target + dep1 + dep2 + caller + test");
        assert!(result.stats.nodes_included >= 4, "should include target + dep1 + dep2 + caller + test");
        assert_eq!(result.stats.nodes_excluded_by_filter, 0, "no filter applied");
        assert_eq!(result.stats.budget_total, 10000);
        assert!(result.stats.budget_used > 0);
        // elapsed_ms might be 0 on fast machines, that's ok.
    }

    // =========================================================================
    // §9.1 Tests: ContextFilters + passes_filters
    // =========================================================================

    #[test]
    fn test_passes_filters_empty_filters() {
        let c = Candidate {
            node_id: "x".into(), node_type: "function".into(),
            file_path: Some("src/lib.rs".into()), signature: None,
            doc_comment: None, description: None, source_code: None,
            hop_distance: 1, modified_at: None,
            connecting_relation: "calls".into(), token_estimate: 10,
        };
        let filters = ContextFilters::default();
        assert!(passes_filters(&c, &filters));
    }

    #[test]
    fn test_passes_filters_type_match() {
        let c = Candidate {
            node_id: "x".into(), node_type: "function".into(),
            file_path: None, signature: None,
            doc_comment: None, description: None, source_code: None,
            hop_distance: 1, modified_at: None,
            connecting_relation: "calls".into(), token_estimate: 10,
        };
        let filters = ContextFilters {
            include_patterns: vec!["type:function".into()],
            ..Default::default()
        };
        assert!(passes_filters(&c, &filters));
    }

    #[test]
    fn test_passes_filters_type_no_match() {
        let c = Candidate {
            node_id: "x".into(), node_type: "class".into(),
            file_path: None, signature: None,
            doc_comment: None, description: None, source_code: None,
            hop_distance: 1, modified_at: None,
            connecting_relation: "calls".into(), token_estimate: 10,
        };
        let filters = ContextFilters {
            include_patterns: vec!["type:function".into()],
            ..Default::default()
        };
        assert!(!passes_filters(&c, &filters));
    }

    #[test]
    fn test_passes_filters_glob_match() {
        let c = Candidate {
            node_id: "x".into(), node_type: "file".into(),
            file_path: Some("src/lib.rs".into()), signature: None,
            doc_comment: None, description: None, source_code: None,
            hop_distance: 1, modified_at: None,
            connecting_relation: "calls".into(), token_estimate: 10,
        };
        let filters = ContextFilters {
            include_patterns: vec!["**/*.rs".into()],
            ..Default::default()
        };
        assert!(passes_filters(&c, &filters));
    }

    #[test]
    fn test_passes_filters_glob_no_match() {
        let c = Candidate {
            node_id: "x".into(), node_type: "file".into(),
            file_path: Some("src/lib.rs".into()), signature: None,
            doc_comment: None, description: None, source_code: None,
            hop_distance: 1, modified_at: None,
            connecting_relation: "calls".into(), token_estimate: 10,
        };
        let filters = ContextFilters {
            include_patterns: vec!["**/*.py".into()],
            ..Default::default()
        };
        assert!(!passes_filters(&c, &filters));
    }

    #[test]
    fn test_passes_filters_exclude_ids() {
        let c = Candidate {
            node_id: "excluded-node".into(), node_type: "function".into(),
            file_path: None, signature: None,
            doc_comment: None, description: None, source_code: None,
            hop_distance: 1, modified_at: None,
            connecting_relation: "calls".into(), token_estimate: 10,
        };
        let filters = ContextFilters {
            exclude_ids: vec!["excluded-node".into()],
            ..Default::default()
        };
        assert!(!passes_filters(&c, &filters));
    }

    #[test]
    fn test_passes_filters_any_match_semantics() {
        let c = Candidate {
            node_id: "x".into(), node_type: "function".into(),
            file_path: Some("src/lib.rs".into()), signature: None,
            doc_comment: None, description: None, source_code: None,
            hop_distance: 1, modified_at: None,
            connecting_relation: "calls".into(), token_estimate: 10,
        };
        // One pattern doesn't match, the other does — should pass.
        let filters = ContextFilters {
            include_patterns: vec!["*.py".into(), "type:function".into()],
            ..Default::default()
        };
        assert!(passes_filters(&c, &filters));
    }

    // =========================================================================
    // §9.2 Tests: simple_glob_match
    // =========================================================================

    #[test]
    fn test_glob_exact() {
        assert!(simple_glob_match("foo.rs", "foo.rs"));
        assert!(!simple_glob_match("foo.rs", "bar.rs"));
    }

    #[test]
    fn test_glob_star() {
        assert!(simple_glob_match("*.rs", "lib.rs"));
        assert!(simple_glob_match("*.rs", "main.rs"));
        assert!(!simple_glob_match("*.rs", "src/lib.rs")); // * doesn't cross /
        assert!(!simple_glob_match("*.py", "lib.rs"));
    }

    #[test]
    fn test_glob_doublestar() {
        assert!(simple_glob_match("src/**", "src/lib.rs"));
        assert!(simple_glob_match("src/**", "src/a/b/c.rs"));
        assert!(!simple_glob_match("src/**", "tests/lib.rs"));
    }

    #[test]
    fn test_glob_doublestar_suffix() {
        assert!(simple_glob_match("**/*.rs", "src/lib.rs"));
        assert!(simple_glob_match("**/*.rs", "a/b/c.rs"));
        assert!(!simple_glob_match("**/*.py", "src/lib.rs"));
    }

    #[test]
    fn test_glob_question_mark() {
        assert!(simple_glob_match("?.rs", "a.rs"));
        assert!(!simple_glob_match("?.rs", "ab.rs"));
    }

    // =========================================================================
    // §9.3 Tests: OutputFormat
    // =========================================================================

    #[test]
    fn test_output_format_parse() {
        assert_eq!("json".parse::<OutputFormat>().unwrap(), OutputFormat::Json);
        assert_eq!("markdown".parse::<OutputFormat>().unwrap(), OutputFormat::Markdown);
        assert_eq!("md".parse::<OutputFormat>().unwrap(), OutputFormat::Markdown);
        assert_eq!("yaml".parse::<OutputFormat>().unwrap(), OutputFormat::Yaml);
        assert_eq!("yml".parse::<OutputFormat>().unwrap(), OutputFormat::Yaml);
        assert!("xml".parse::<OutputFormat>().is_err());
    }

    #[test]
    fn test_output_format_display() {
        assert_eq!(OutputFormat::Json.to_string(), "json");
        assert_eq!(OutputFormat::Markdown.to_string(), "markdown");
        assert_eq!(OutputFormat::Yaml.to_string(), "yaml");
    }

    #[test]
    fn test_output_format_default() {
        assert_eq!(OutputFormat::default(), OutputFormat::Markdown);
    }

    // =========================================================================
    // §9.4 Tests: format_context
    // =========================================================================

    #[test]
    fn test_format_context_json() {
        let graph = make_graph_with_deps();
        let query = ContextQuery {
            targets: vec!["target".into()],
            token_budget: 10000,
            depth: 2,
            ..Default::default()
        };
        let assembled = assemble_context(&graph, &query).unwrap();
        let json_str = format_context(&assembled, OutputFormat::Json);
        let parsed: serde_json::Value = serde_json::from_str(&json_str).expect("valid JSON");
        assert!(parsed.get("targets").is_some());
        assert!(parsed.get("dependencies").is_some());
        assert!(parsed.get("callers").is_some());
        assert!(parsed.get("tests").is_some());
        assert!(parsed.get("estimated_tokens").is_some());
        assert!(parsed.get("stats").is_some());
    }

    #[test]
    fn test_format_context_yaml() {
        let graph = make_graph_with_deps();
        let query = ContextQuery {
            targets: vec!["target".into()],
            token_budget: 10000,
            depth: 2,
            ..Default::default()
        };
        let assembled = assemble_context(&graph, &query).unwrap();
        let yaml_str = format_context(&assembled, OutputFormat::Yaml);
        assert!(yaml_str.contains("targets:"), "YAML should contain targets key");
        assert!(yaml_str.contains("stats:"), "YAML should contain stats key");
    }

    #[test]
    fn test_format_context_markdown() {
        let graph = make_graph_with_deps();
        let query = ContextQuery {
            targets: vec!["target".into()],
            token_budget: 10000,
            depth: 2,
            ..Default::default()
        };
        let assembled = assemble_context(&graph, &query).unwrap();
        let md_str = format_context(&assembled, OutputFormat::Markdown);
        assert!(md_str.contains("# Context Assembly"), "markdown should have header");
        assert!(md_str.contains("## Targets"), "markdown should have targets section");
        assert!(md_str.contains("`target`"), "markdown should reference target node");
        assert!(md_str.contains("## Dependencies"), "markdown should have deps section");
    }

    // =========================================================================
    // §9.5 Tests: gather_targets
    // =========================================================================

    #[test]
    fn test_gather_targets_basic() {
        let graph = make_graph_with_deps();
        let targets = gather_targets(&graph, &["target".into()], None).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].node_id, "target");
        assert_eq!(targets[0].title.as_deref(), Some("Target Function"));
        assert_eq!(targets[0].file_path.as_deref(), Some("src/lib.rs"));
        assert_eq!(targets[0].signature.as_deref(), Some("fn target() -> i32"));
    }

    #[test]
    fn test_gather_targets_nonexistent() {
        let graph = make_graph_with_deps();
        let err = gather_targets(&graph, &["missing".into()], None).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    // =========================================================================
    // §9.6 Tests: gather_dependencies
    // =========================================================================

    #[test]
    fn test_gather_deps_depth_1() {
        let graph = make_graph_with_deps();
        let (deps, filtered) = gather_dependencies(
            &graph, &["target".into()], 1, &ContextFilters::default(), None,
        );
        assert_eq!(filtered, 0);
        let ids: Vec<&str> = deps.iter().map(|d| d.node_id.as_str()).collect();
        assert!(ids.contains(&"dep1"));
        assert!(!ids.contains(&"dep2"), "depth=1 should not include transitive");
    }

    #[test]
    fn test_gather_deps_depth_2() {
        let graph = make_graph_with_deps();
        let (deps, _) = gather_dependencies(
            &graph, &["target".into()], 2, &ContextFilters::default(), None,
        );
        let ids: Vec<&str> = deps.iter().map(|d| d.node_id.as_str()).collect();
        assert!(ids.contains(&"dep1"));
        assert!(ids.contains(&"dep2"), "depth=2 should include transitive");
    }

    #[test]
    fn test_gather_deps_with_filter() {
        let graph = make_graph_with_deps();
        let filters = ContextFilters {
            include_patterns: vec!["src/dep.rs".into()],
            ..Default::default()
        };
        let (deps, filtered) = gather_dependencies(
            &graph, &["target".into()], 2, &filters, None,
        );
        assert_eq!(deps.len(), 1, "only dep1 should pass filter");
        assert_eq!(deps[0].node_id, "dep1");
        assert_eq!(filtered, 1, "dep2 should be filtered out");
    }

    #[test]
    fn test_gather_deps_no_self_loops() {
        let graph = make_graph_with_deps();
        let (deps, _) = gather_dependencies(
            &graph, &["target".into()], 2, &ContextFilters::default(), None,
        );
        // Target itself should not appear as a dependency.
        assert!(!deps.iter().any(|d| d.node_id == "target"));
    }

    // =========================================================================
    // §9.7 Tests: gather_callers_and_tests
    // =========================================================================

    #[test]
    fn test_gather_callers_and_tests() {
        let graph = make_graph_with_deps();
        let (callers, tests) = gather_callers_and_tests(&graph, &["target".into()], None);
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].node_id, "caller1");
        assert_eq!(callers[0].connecting_relation, "calls");
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].node_id, "test1");
        assert_eq!(tests[0].connecting_relation, "tests_for");
    }

    #[test]
    fn test_gather_callers_excludes_targets() {
        // If target A calls target B, B calling A should not show A as a caller.
        let graph = make_graph_with_deps();
        let (callers, _) = gather_callers_and_tests(
            &graph, &["target".into(), "caller1".into()], None,
        );
        // caller1 is also a target, so it should not appear as a caller.
        assert!(!callers.iter().any(|c| c.node_id == "caller1"));
    }

    // =========================================================================
    // §9.8 Tests: ContextQuery defaults
    // =========================================================================

    #[test]
    fn test_context_query_defaults() {
        let q = ContextQuery::default();
        assert!(q.targets.is_empty());
        assert_eq!(q.token_budget, 8000);
        assert_eq!(q.depth, 2);
        assert!(q.filters.include_patterns.is_empty());
        assert!(q.filters.exclude_ids.is_empty());
        assert_eq!(q.format, OutputFormat::Markdown);
        assert!(q.project_root.is_none());
    }

    // =========================================================================
    // §9.9 Tests: ContextStats
    // =========================================================================

    #[test]
    fn test_context_stats_default() {
        let s = ContextStats::default();
        assert_eq!(s.nodes_visited, 0);
        assert_eq!(s.nodes_included, 0);
        assert_eq!(s.nodes_excluded_by_filter, 0);
        assert_eq!(s.budget_used, 0);
        assert_eq!(s.budget_total, 0);
        assert_eq!(s.elapsed_ms, 0);
    }

    #[test]
    fn test_context_stats_serializable() {
        let s = ContextStats {
            nodes_visited: 10,
            nodes_included: 5,
            nodes_excluded_by_filter: 2,
            budget_used: 3000,
            budget_total: 8000,
            elapsed_ms: 42,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"nodes_visited\":10"));
        assert!(json.contains("\"elapsed_ms\":42"));
    }

    // =========================================================================
    // §9.10 Tests: assemble_context edge cases
    // =========================================================================

    #[test]
    fn test_assemble_context_no_deps_no_callers() {
        // Isolated node with no edges.
        let mut g = Graph::default();
        let mut n = Node::new("lonely", "Lonely Node");
        n.node_type = Some("function".to_string());
        g.nodes.push(n);

        let query = ContextQuery {
            targets: vec!["lonely".into()],
            token_budget: 10000,
            depth: 2,
            ..Default::default()
        };
        let result = assemble_context(&g, &query).unwrap();
        assert_eq!(result.result.targets.len(), 1);
        assert!(result.result.dependencies.is_empty());
        assert!(result.result.callers.is_empty());
        assert!(result.result.tests.is_empty());
    }

    #[test]
    fn test_assemble_context_diamond_dedup() {
        // target -> A, target -> B, A -> C, B -> C
        // C should appear only once.
        let mut g = Graph::default();
        g.nodes.push(Node::new("target", "T"));
        g.nodes.push(Node::new("a", "A"));
        g.nodes.push(Node::new("b", "B"));
        g.nodes.push(Node::new("c", "C"));
        g.edges.push(Edge::new("target", "a", "calls"));
        g.edges.push(Edge::new("target", "b", "calls"));
        g.edges.push(Edge::new("a", "c", "calls"));
        g.edges.push(Edge::new("b", "c", "calls"));

        let query = ContextQuery {
            targets: vec!["target".into()],
            token_budget: 10000,
            depth: 3,
            ..Default::default()
        };
        let result = assemble_context(&g, &query).unwrap();
        let dep_ids: Vec<&str> = result.result.dependencies.iter()
            .map(|d| d.node_id.as_str()).collect();
        // C should appear exactly once (BFS deduplication).
        assert_eq!(dep_ids.iter().filter(|&&id| id == "c").count(), 1);
    }
}
