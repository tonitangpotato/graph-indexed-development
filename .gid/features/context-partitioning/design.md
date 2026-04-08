# Design: Context Assembly (GOALs 4.1–4.13)

## 1. Overview

The context assembly subsystem is responsible for gathering, scoring, ranking, and budget-fitting graph nodes into a coherent context window for one or more target nodes. Starting from a `ContextQuery` with **multiple targets** (GOAL-4.6), the pipeline traverses the dependency graph via multi-hop expansion in both directions (forward for dependencies, reverse for callers and tests), scores each candidate by edge-relation-based relevance (GOAL-4.4), then applies a **category-based budget allocation** (GOAL-4.3) where targets are never truncated and transitive deps are truncated first. For code nodes with `file_path`/`start_line`/`end_line`, actual source code is read from disk (GOAL-4.1b). The result is a categorized `ContextResult` with targets, dependencies, callers, and tests — ready for consumption by downstream LLM prompts. All graph access is mediated through the `GraphStorage` trait and shared types defined in design.md §3.

## 2. ContextQuery

The `ContextQuery` struct captures all parameters needed to drive context assembly.

```rust
/// Filters that narrow which candidate nodes are eligible.
#[derive(Debug, Clone, Default)]
pub struct ContextFilters {
    /// Only include nodes of these kinds (empty = all).
    pub node_kinds: Vec<String>,
    /// Only include nodes matching these edge relationships.
    pub edge_kinds: Vec<String>,
    /// Exclude nodes whose IDs match any of these patterns.
    pub exclude_ids: Vec<String>,
    /// Only include nodes modified after this timestamp (epoch secs).
    pub modified_after: Option<i64>,
    /// GOAL-4.8: --include patterns. Supports file path globs (e.g., "*.rs")
    /// and node type filters (e.g., "type:function").
    pub include_patterns: Vec<String>,
}

/// GOAL-4.9: Output format selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Markdown,  // default: human-readable sections
    Json,      // machine-parseable structured data
    Yaml,      // same structure as JSON, YAML syntax
}

impl Default for OutputFormat {
    fn default() -> Self { Self::Markdown }
}

/// A request for assembled context. **[GOAL-4.1, 4.6]**
#[derive(Debug, Clone)]
pub struct ContextQuery {
    /// GOAL-4.6: One or more target nodes whose context we are assembling.
    /// At least one target must be specified.
    pub targets: Vec<String>,
    /// Maximum token budget for the assembled output. **[GOAL-4.2]**
    pub token_budget: usize,
    /// Maximum traversal depth (hops from any target). **[GOAL-4.7]**
    pub depth: u32,
    /// Optional filters to narrow candidates. **[GOAL-4.8]**
    pub filters: ContextFilters,
    /// Output format. **[GOAL-4.9]**
    pub format: OutputFormat,
}
```

## 3. Context Assembly Pipeline

The pipeline is a linear sequence of seven stages. Each stage is a pure-ish function that transforms its input, making the pipeline easy to test in isolation. **[GOAL-4.2]**

```
query → gather_targets → gather_deps → gather_callers_tests → score → budget_fit → format → output
```

```rust
/// Top-level entry point. **[GOAL-4.2, 4.3]**
pub fn assemble_context(
    storage: &dyn GraphStorage,
    query: &ContextQuery,
) -> Result<ContextResult> {
    // Validate: at least one target (GOAL-4.6)
    if query.targets.is_empty() {
        return Err(anyhow!("--targets: at least one target node ID required"));
    }

    let mut stats = ContextStats::default();

    // Stage 1: Gather target node details + source code from disk.
    let targets = gather_targets(storage, &query.targets)?;
    stats.nodes_visited += targets.len();

    // Stage 2: Multi-source BFS — gather dependency candidates.
    let dep_candidates = gather_dependencies(
        storage, &query.targets, query.depth, &query.filters,
    )?;
    stats.nodes_visited += dep_candidates.len();

    // Stage 3: Reverse-edge traversal — gather callers and tests.
    let (caller_candidates, test_candidates) = gather_callers_and_tests(
        storage, &query.targets,
    )?;
    stats.nodes_visited += caller_candidates.len() + test_candidates.len();

    // Stage 4: Score all candidates by edge-relation relevance (GOAL-4.4).
    let scored_deps = score_candidates(&dep_candidates);
    let scored_callers = score_candidates(&caller_candidates);
    let scored_tests = score_candidates(&test_candidates);

    // Stage 5: Category-based budget allocation (GOAL-4.3).
    let budget_result = budget_fit_by_category(
        &targets,
        scored_deps,
        scored_callers,
        scored_tests,
        query.token_budget,
    );

    // Stage 6: Log traversal stats (GOAL-4.13).
    stats.nodes_included = budget_result.total_included();
    stats.nodes_excluded_by_filter = dep_candidates.len() - scored_deps.len(); // filtered in gather
    stats.budget_used = budget_result.total_tokens;
    stats.budget_total = query.token_budget;
    tracing::info!(
        visited = stats.nodes_visited,
        included = stats.nodes_included,
        excluded_filter = stats.nodes_excluded_by_filter,
        budget = %format!("{}/{}", stats.budget_used, stats.budget_total),
        "context assembly complete"
    );

    Ok(budget_result)
}
```

## 4. Target Gathering & Source Code Reading

### 4.1 Target Details

For each target node, gather full metadata and read source code from disk (GOAL-4.1).

```rust
/// Full details of a target node, including source code.
#[derive(Debug, Clone)]
pub struct TargetContext {
    pub id: String,
    pub title: Option<String>,
    pub file_path: Option<String>,
    pub signature: Option<String>,
    pub doc_comment: Option<String>,
    pub description: Option<String>,
    /// Source code read from disk (GOAL-4.1b).
    /// None if file_path is missing or file doesn't exist.
    pub source_code: Option<String>,
    pub source_note: Option<String>,  // e.g., "file not found" or "lines unavailable"
    pub token_estimate: usize,
    /// Edge relation that connects to another target (if any).
    pub connecting_relation: Option<String>,
}

fn gather_targets(
    storage: &dyn GraphStorage,
    target_ids: &[String],
) -> Result<Vec<TargetContext>> {
    let mut targets = Vec::new();

    for id in target_ids {
        let node = storage.get_node(id)?
            .ok_or_else(|| anyhow!("target node not found: {}", id))?;

        // GOAL-4.1b: Read source code from disk if file_path + line range available.
        let (source_code, source_note) = read_source_code(
            node.file_path.as_deref(),
            node.start_line,
            node.end_line,
        );

        let mut tc = TargetContext {
            id: node.id.clone(),
            title: node.title.clone(),
            file_path: node.file_path.clone(),
            signature: node.signature.clone(),
            doc_comment: node.doc_comment.clone(),
            description: node.description.clone(),
            source_code,
            source_note,
            token_estimate: 0,
            connecting_relation: None,
        };
        tc.token_estimate = estimate_tokens_for_target(&tc);
        targets.push(tc);
    }

    Ok(targets)
}

/// Read source file from disk, extracting start_line..end_line.
fn read_source_code(
    file_path: Option<&str>,
    start_line: Option<i32>,
    end_line: Option<i32>,
) -> (Option<String>, Option<String>) {
    let Some(path) = file_path else {
        return (None, Some("no file_path on node".into()));
    };

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return (None, Some(format!("file not found: {e}"))),
    };

    match (start_line, end_line) {
        (Some(start), Some(end)) => {
            let lines: Vec<&str> = content.lines().collect();
            let start_idx = (start as usize).saturating_sub(1);
            let end_idx = (end as usize).min(lines.len());
            if start_idx < end_idx {
                (Some(lines[start_idx..end_idx].join("\n")), None)
            } else {
                (Some(content), Some("line range out of bounds, included full file".into()))
            }
        }
        (Some(start), None) => {
            // Only start line — include from start to end of file
            let lines: Vec<&str> = content.lines().collect();
            let start_idx = (start as usize).saturating_sub(1);
            (Some(lines[start_idx..].join("\n")), None)
        }
        _ => (Some(content), None), // No line info — include full file
    }
}
```

## 4.2 Dependency Gathering (Forward BFS)

Multi-source BFS starting from **all** target nodes simultaneously (GOAL-4.6). At each hop, follows outgoing edges to discover dependencies. **[GOAL-4.3, 4.7]**

```rust
/// A raw candidate before scoring.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub node_id: String,
    pub node_type: String,
    pub file_path: Option<String>,
    pub signature: Option<String>,
    pub doc_comment: Option<String>,
    pub description: Option<String>,
    pub source_code: Option<String>,
    pub hop_distance: u32,
    pub modified_at: Option<i64>,
    /// The edge relation that connected this node to the traversal.
    pub connecting_relation: String,
    pub token_estimate: usize,
}

/// Multi-source BFS with depth limit. **[GOAL-4.7, 4.10]**
fn gather_dependencies(
    storage: &dyn GraphStorage,
    roots: &[String],
    max_depth: u32,
    filters: &ContextFilters,
) -> Result<Vec<Candidate>> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, u32, String)> = VecDeque::new(); // (node_id, hop, relation)
    let mut results: Vec<Candidate> = Vec::new();

    // Initialize from all targets (multi-source BFS).
    for root in roots {
        visited.insert(root.clone());
        // Enqueue neighbors at hop 1 (not the root itself — roots are targets).
        let edges = storage.get_edges(root)?;
        for edge in edges {
            let neighbor = if edge.from == *root { &edge.to } else { continue };
            if !visited.contains(neighbor) {
                visited.insert(neighbor.clone());
                queue.push_back((neighbor.clone(), 1, edge.relation.clone()));
            }
        }
    }

    while let Some((current_id, hop, relation)) = queue.pop_front() {
        if hop > max_depth { continue; }

        let node = match storage.get_node(&current_id)? {
            Some(n) => n,
            None => continue,
        };

        let candidate = Candidate {
            node_id: current_id.clone(),
            node_type: node.node_type.clone(),
            file_path: node.file_path.clone(),
            signature: node.signature.clone(),
            doc_comment: node.doc_comment.clone(),
            description: node.description.clone(),
            source_code: read_source_code(node.file_path.as_deref(), node.start_line, node.end_line).0,
            hop_distance: hop,
            modified_at: None, // from updated_at if available
            connecting_relation: relation,
            token_estimate: 0, // computed in scoring
        };

        // GOAL-4.8: Apply --include filters.
        if passes_filters(&candidate, filters) {
            results.push(candidate);
        }

        // Expand forward for next hop.
        if hop < max_depth {
            let edges = storage.get_edges(&current_id)?;
            for edge in edges {
                let neighbor = if edge.from == current_id { &edge.to } else { continue };
                if !visited.contains(neighbor) {
                    visited.insert(neighbor.clone());
                    queue.push_back((neighbor.clone(), hop + 1, edge.relation.clone()));
                }
            }
        }
    }

    Ok(results)
}

/// GOAL-4.8: Filter by --include patterns.
fn passes_filters(candidate: &Candidate, filters: &ContextFilters) -> bool {
    if filters.include_patterns.is_empty() { return true; }

    for pattern in &filters.include_patterns {
        if let Some(type_filter) = pattern.strip_prefix("type:") {
            // Match by node_type
            if candidate.node_type == type_filter { return true; }
        } else {
            // Match by file path glob
            if let Some(ref path) = candidate.file_path {
                if glob_match(pattern, path) { return true; }
            }
        }
    }

    false // No pattern matched
}
```

## 4.3 Caller and Test Discovery (Reverse BFS)

**GOAL-4.1e,f:** Discover callers and tests by following **reverse** edges (edges pointing TO the targets).

```rust
fn gather_callers_and_tests(
    storage: &dyn GraphStorage,
    target_ids: &[String],
) -> Result<(Vec<Candidate>, Vec<Candidate>)> {
    let mut callers = Vec::new();
    let mut tests = Vec::new();
    let target_set: HashSet<&str> = target_ids.iter().map(|s| s.as_str()).collect();

    for target_id in target_ids {
        // Get all edges TO this target (reverse traversal).
        let all_edges = storage.get_edges(target_id)?;

        for edge in &all_edges {
            // We want edges where to_node == target_id (incoming edges).
            if edge.to != *target_id { continue; }
            if target_set.contains(edge.from.as_str()) { continue; } // Skip other targets

            let node = match storage.get_node(&edge.from)? {
                Some(n) => n,
                None => continue,
            };

            let candidate = Candidate {
                node_id: node.id.clone(),
                node_type: node.node_type.clone(),
                file_path: node.file_path.clone(),
                signature: node.signature.clone(),
                doc_comment: node.doc_comment.clone(),
                description: node.description.clone(),
                source_code: read_source_code(node.file_path.as_deref(), node.start_line, node.end_line).0,
                hop_distance: 1,
                modified_at: None,
                connecting_relation: edge.relation.clone(),
                token_estimate: 0,
            };

            // Categorize: tests_for → test, calls/imports → caller
            match edge.relation.as_str() {
                "tests_for" => tests.push(candidate),
                "calls" | "imports" => callers.push(candidate),
                _ => callers.push(candidate), // default to caller
            }
        }
    }

    Ok((callers, tests))
}
```

## 5. Relevance Scoring

Each candidate is assigned a floating-point relevance score. The score is primarily based on the **edge relation** that connected the candidate to the context, matching the 5-tier ranking from GOAL-4.4. **[GOAL-4.4, 4.5]**

### 5.1 Edge-Relation-Based Ranking (GOAL-4.4)

```rust
/// GOAL-4.4: 5-tier relevance ranking by edge relation.
fn relation_rank(relation: &str) -> u8 {
    match relation {
        "calls" | "imports" => 1,                                    // Direct call
        "type_reference" | "inherits" | "implements" | "uses" => 2,  // Type reference
        "contains" | "defined_in" => 3,                              // Same-file
        "depends_on" | "part_of" | "blocks" | "tests_for" => 4,     // Structural
        _ => 5,                                                       // Transitive / unknown
    }
}

/// Score maps rank to [0.0, 1.0]: rank 1 → 1.0, rank 5 → 0.2.
fn relation_score(relation: &str) -> f64 {
    match relation_rank(relation) {
        1 => 1.0,
        2 => 0.8,
        3 => 0.6,
        4 => 0.4,
        5 => 0.2,
        _ => 0.1,
    }
}
```

### 5.2 Composite Scoring

The composite score combines edge-relation rank, hop distance, and edge weight/confidence.

```rust
/// Scoring weights (v1 constants — documented as tunable for future versions).
const W_RELATION: f64 = 0.60;
const W_PROXIMITY: f64 = 0.30;
const W_WEIGHT:    f64 = 0.10;

#[derive(Debug, Clone)]
pub struct ScoredCandidate {
    pub candidate: Candidate,
    pub score: f64,
    pub token_estimate: usize,
}

/// Score a single candidate. **[GOAL-4.4, 4.5]**
fn score_candidate(candidate: &Candidate) -> ScoredCandidate {
    // Relation-based score (primary factor).
    let rel_score = relation_score(&candidate.connecting_relation);

    // Proximity: inverse of hop distance.
    // hop 1 → 1.0, hop 2 → 0.5, hop 3 → 0.33.
    let proximity = 1.0 / (candidate.hop_distance as f64);

    // Weight: from edge weight (default 1.0) and confidence.
    // For now, weight_factor = 1.0 (could incorporate edge.weight in future).
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

fn score_candidates(candidates: &[Candidate]) -> Vec<ScoredCandidate> {
    let mut scored: Vec<ScoredCandidate> = candidates.iter().map(score_candidate).collect();
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    scored
}
```

## 6. Token Budget Management

Token estimation uses the `bytes / 4` heuristic mandated by design.md §9. The budget-fitting stage uses **category-based allocation** per GOAL-4.3: targets are never truncated, then direct deps, then callers, then transitive deps (furthest hops first). **[GOAL-4.2, 4.3]**

```rust
/// Estimate token count from text content. **[GOAL-4.2]**
/// Per design.md §9: tokens ≈ byte_len / 4.
fn estimate_tokens_str(text: &str) -> usize {
    let len = text.len();
    if len == 0 { 0 } else { (len / 4).max(1) }
}

fn estimate_tokens_for_target(target: &TargetContext) -> usize {
    let mut bytes = 0;
    if let Some(ref t) = target.title { bytes += t.len(); }
    if let Some(ref d) = target.description { bytes += d.len(); }
    if let Some(ref s) = target.signature { bytes += s.len(); }
    if let Some(ref dc) = target.doc_comment { bytes += dc.len(); }
    if let Some(ref sc) = target.source_code { bytes += sc.len(); }
    bytes += 50; // overhead for headers/formatting
    (bytes / 4).max(1)
}

fn estimate_tokens_for_candidate(c: &Candidate) -> usize {
    let mut bytes = 0;
    if let Some(ref sc) = c.source_code { bytes += sc.len(); }
    if let Some(ref sig) = c.signature { bytes += sig.len(); }
    if let Some(ref desc) = c.description { bytes += desc.len(); }
    if let Some(ref dc) = c.doc_comment { bytes += dc.len(); }
    bytes += 30; // overhead
    (bytes / 4).max(1)
}

/// Category-based budget allocation. **[GOAL-4.3]**
///
/// Priority order (GOAL-4.3):
/// 1. Targets — NEVER truncated
/// 2. Direct dependencies (hop == 1)
/// 3. Callers
/// 4. Transitive dependencies (furthest hops dropped first)
fn budget_fit_by_category(
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

    // 5. Transitive deps — sorted by hop distance descending (furthest dropped first).
    let mut trans_sorted = transitive_deps;
    trans_sorted.sort_by(|a, b| {
        a.candidate.hop_distance.cmp(&b.candidate.hop_distance)
            .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal))
    });
    let (included_transitive, trans_trunc) = greedy_fill(&trans_sorted, remaining);
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
            included.push(ContextItem::from_scored(sc, false));
            remaining -= sc.token_estimate;
        } else if remaining >= MIN_USEFUL_TOKENS {
            let truncated = ContextItem::from_scored_truncated(sc, remaining);
            remaining -= truncated.token_estimate;
            included.push(truncated);
            info.truncated_count += 1;
        } else {
            info.dropped_count += 1;
        }
    }

    info.budget_used = budget - remaining;
    (included, info)
}

/// Minimum tokens for a truncated item to be useful.
const MIN_USEFUL_TOKENS: usize = 32;
```

## 7. Truncation Strategy

When an item does not fully fit within the remaining budget, the truncation strategy decides how to trim it. **[GOAL-4.3]**

**Rules:**
1. **UTF-8 safety:** Truncation always occurs at a valid UTF-8 character boundary. After computing the cut point, verify with `str::is_char_boundary()`. If the cut point falls mid-character, scan backward to find a valid boundary.
2. **Prefer line boundaries:** The text is trimmed to the last complete line that fits within `remaining_tokens * 4` bytes.
3. **Truncation marker:** A `\n... [truncated]` suffix is appended so consumers know the item is incomplete.
4. **Head-biased:** The beginning of the content is preserved (most relevant for code files where imports/signatures appear first).

```rust
/// Truncate text content to fit within `max_tokens` tokens. **[GOAL-4.3]**
fn truncate_text(text: &str, max_tokens: usize) -> String {
    let max_bytes = max_tokens * 4;
    let marker = "\n... [truncated]";
    let usable_bytes = max_bytes.saturating_sub(marker.len());

    if text.len() <= max_bytes {
        return text.to_string();
    }

    // Find a safe cut point at a line boundary.
    let slice = &text[..usable_bytes.min(text.len())];

    // Ensure we're at a char boundary (UTF-8 safety — FINDING-11).
    let safe_end = if text.is_char_boundary(usable_bytes.min(text.len())) {
        usable_bytes.min(text.len())
    } else {
        // Scan backward to find a valid char boundary.
        let mut pos = usable_bytes.min(text.len());
        while pos > 0 && !text.is_char_boundary(pos) {
            pos -= 1;
        }
        pos
    };

    let safe_slice = &text[..safe_end];

    // Prefer line boundary.
    let cut_point = safe_slice.rfind('\n').unwrap_or(safe_end);

    format!("{}{}", &text[..cut_point], marker)
}
```

## 8. ContextResult

The output struct returned by the assembly pipeline. Categorized per GOAL-4.1. **[GOAL-4.1, 4.10, 4.11]**

```rust
/// A single non-target item in the assembled context. **[GOAL-4.11]**
#[derive(Debug, Clone, Serialize)]
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

/// Metadata about truncation decisions. **[GOAL-4.3]**
#[derive(Debug, Clone, Default, Serialize)]
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

/// Traversal statistics for observability. **[GOAL-4.13]**
#[derive(Debug, Clone, Default, Serialize)]
pub struct ContextStats {
    pub nodes_visited: usize,
    pub nodes_included: usize,
    pub nodes_excluded_by_filter: usize,
    pub budget_used: usize,
    pub budget_total: usize,
    pub elapsed_ms: u64,
}

/// The assembled context result — categorized output. **[GOAL-4.1]**
#[derive(Debug, Clone, Serialize)]
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
    fn total_included(&self) -> usize {
        self.targets.len() + self.dependencies.len() + self.callers.len() + self.tests.len()
    }
}
```

## 9. Context Freshness

Recently modified nodes can optionally be preferred over stale ones via the `modified_after` filter in `ContextFilters`. **[GOAL-4.7]**

**Mechanism:** The `modified_after` filter provides a hard cutoff: nodes with `updated_at` older than the threshold are excluded entirely during candidate gathering, before scoring even occurs. This is useful for time-boxed context windows (e.g., "only changes in the last sprint").

**Note:** Unlike the previous design, freshness is NOT a scoring factor. The primary ranking is edge-relation-based (GOAL-4.4). Freshness is a binary filter, not a gradient. This simplification matches the requirements — GOAL-4.4 specifies edge relation ranking, not freshness-weighted scoring.

## 10. Configurable Depth

The `depth` parameter on `ContextQuery` controls the maximum number of hops in the BFS traversal from any target node. **[GOAL-4.7]**

| Depth | Breadth | Use Case |
|-------|---------|----------|
| 0     | Focal task only | Minimal context, just the task itself |
| 1     | Direct dependencies and code files | Focused work on a single task |
| 2     | Transitive deps (deps-of-deps) | Understanding broader impact |
| 3+    | Wide neighborhood | Architecture-level context |

**Trade-offs:**
- **Lower depth** → fewer candidates → faster assembly, tighter context, less noise. Ideal when the token budget is small or the task is well-isolated.
- **Higher depth** → more candidates → broader context but more competition for budget slots. The scoring algorithm naturally deprioritizes distant nodes (proximity weight), so increasing depth does not flood the output with irrelevant items.

**Default:** `depth = 2` provides a good balance for most tasks. The CLI or calling code can override this per-query.

## 11. Integration with GraphStorage

The context assembly pipeline interacts with `GraphStorage` (defined in design.md §3) through the following trait methods: **[GOAL-4.12]**

| Trait Method | Used In | Purpose |
|-------------|---------|---------|
| `get_node(&str)` | `gather_targets`, `gather_dependencies`, `gather_callers_and_tests` | Retrieve node metadata |
| `get_edges(&str)` | `gather_dependencies`, `gather_callers_and_tests` | Traverse forward + reverse edges |
| `query_nodes(&NodeFilter)` | (future: batch target resolution) | Find nodes by filter |

**Note:** The pipeline reads source code from disk directly (§4.1) — it does NOT rely on `GraphStorage` to store source code. Nodes store metadata (file_path, start_line, end_line); the context pipeline reads the actual files.

### Transaction Semantics

Context assembly is read-only. It acquires a **single consistent snapshot** of the graph by reading within a single implicit SQLite transaction (WAL mode guarantees snapshot isolation for readers). No explicit transaction management is needed.

**[GOAL-4.12]** — Implemented as a library function in `gid-core/src/storage/context.rs`, callable from CLI, MCP, LSP, and crate consumers with the same interface.

## 12. GOAL Traceability

| GOAL ID | Description | Implementing Section(s) |
|---------|-------------|------------------------|
| 4.1 | Multi-target context with categorized output (targets/deps/callers/tests + source code) | §3 (pipeline), §4 (gathering), §8 (ContextResult) |
| 4.2 | Token budget fitting (bytes/4) | §6 (budget_fit_by_category) |
| 4.3 | Category-based truncation priority (transitive first, targets never) | §6 (budget_fit_by_category), §7 (truncation) |
| 4.4 | Edge-relation-based 5-tier relevance ranking | §5 (relation_rank, relation_score) |
| 4.5 | Relevance score visible in output | §8 (ContextItem.score) |
| 4.6 | Multiple targets via --targets | §2 (ContextQuery.targets), §4.2 (multi-source BFS) |
| 4.7 | Configurable --depth | §10 (Configurable Depth) |
| 4.8 | --include file path glob and node type filter | §2 (ContextFilters.include_patterns), §4.2 (passes_filters) |
| 4.9 | --format json/yaml/markdown | §2 (OutputFormat) |
| 4.10 | estimated_tokens field in output | §8 (ContextResult.estimated_tokens) |
| 4.11 | Node details include id, file_path, signature, doc_comment, connecting_relation | §8 (ContextItem) |
| 4.12 | Library function in gid-core, thin CLI wrapper | §11 (Integration) |
| 4.13 | Observability: visited, included, excluded, budget, elapsed | §3 (tracing::info!), §8 (ContextStats) |
