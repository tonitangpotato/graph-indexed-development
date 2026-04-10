//! LLM-powered semantic labeling for inferred components.
//!
//! Takes [`ClusterResult`] from [`super::clustering`] and enriches it with
//! human-readable names (component titles), feature groupings, and feature-level
//! dependency edges — all driven by LLM inference.
//!
//! ## Architecture
//!
//! ```text
//! ClusterResult ──► assemble_contexts() ──► name_components(llm) ──► infer_features(llm)
//!                                                                         │
//!                                           infer_feature_deps() ◄────────┘
//!                                                  │
//!                                           label() entry point ──► LabelingResult
//! ```

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::graph::{Edge, Graph, Node};
use super::clustering::ClusterResult;

// ── SimpleLlm trait ────────────────────────────────────────────────────────

/// Minimal LLM interface for semantic labeling.
///
/// This is deliberately simpler than the ritual system's `LlmClient` trait —
/// labeling only needs single-turn prompt→response, no tool use or agentic loops.
/// This keeps gid-core's infer module independent of the ritual system.
///
/// # Implementors
///
/// - Production: wraps Anthropic/OpenAI HTTP clients
/// - Testing: returns canned JSON responses
/// - GidHub: wraps the server's shared LLM pool
#[async_trait::async_trait]
pub trait SimpleLlm: Send + Sync {
    /// Send a prompt and receive a text response.
    ///
    /// The implementation should handle:
    /// - Model selection (caller specifies via `LabelingConfig`)
    /// - API key management
    /// - Rate limiting / retries
    ///
    /// Returns the raw response text (typically JSON).
    async fn complete(&self, prompt: &str) -> Result<String>;

    /// Returns the approximate token count for a string.
    ///
    /// Used for GUARD-4 (token budget enforcement).
    /// Default: rough estimate of 1 token per 4 chars.
    fn estimate_tokens(&self, text: &str) -> usize {
        text.len() / 4
    }
}

// ── Configuration ──────────────────────────────────────────────────────────

/// Configuration for the labeling pipeline.
#[derive(Debug, Clone)]
pub struct LabelingConfig {
    /// Maximum components per LLM batch call (default: 10).
    pub batch_size: usize,
    /// Maximum functions to include per component context (default: 20).
    pub max_functions_per_component: usize,
    /// Maximum file briefs per component context (default: 10).
    pub max_briefs_per_component: usize,
    /// Maximum chars for truncated context fields (default: 2000).
    pub max_context_chars: usize,
    /// Total token budget for all LLM calls (GUARD-4, default: 50_000).
    pub token_budget: usize,
    /// Tokens consumed so far (tracked internally).
    tokens_used: usize,
}

impl Default for LabelingConfig {
    fn default() -> Self {
        Self {
            batch_size: 10,
            max_functions_per_component: 20,
            max_briefs_per_component: 10,
            max_context_chars: 2000,
            token_budget: 50_000,
            tokens_used: 0,
        }
    }
}

impl LabelingConfig {
    /// Record token usage and check if budget is exceeded.
    /// Returns `true` if still within budget.
    pub fn record_tokens(&mut self, tokens: usize) -> bool {
        self.tokens_used += tokens;
        self.tokens_used <= self.token_budget
    }

    /// Remaining token budget.
    pub fn tokens_remaining(&self) -> usize {
        self.token_budget.saturating_sub(self.tokens_used)
    }

    /// Total tokens consumed.
    pub fn tokens_used(&self) -> usize {
        self.tokens_used
    }
}

// ── Context types ──────────────────────────────────────────────────────────

/// Assembled context for a single component, ready to be sent to an LLM.
///
/// Built by [`assemble_contexts`] from the clustering result + source graph.
#[derive(Debug, Clone, Serialize)]
pub struct ComponentContext {
    /// The component node ID (e.g., "infer:component:0").
    pub component_id: String,
    /// Auto-generated name from directory paths (fallback if LLM fails).
    pub auto_name: String,
    /// File paths belonging to this component.
    pub files: Vec<String>,
    /// Class/struct names found in this component's files.
    pub class_names: Vec<String>,
    /// Function/method signatures (truncated to `max_functions_per_component`).
    pub function_signatures: Vec<String>,
    /// Brief descriptions of key files (truncated to `max_briefs_per_component`).
    pub file_briefs: Vec<String>,
    /// Names of child components (for hierarchical parent nodes with no direct files).
    pub child_component_names: Vec<String>,
}

/// Project-level context to improve LLM inference quality.
///
/// Extracted from README.md, ARCHITECTURE.md, etc. (GOAL-2.4).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProjectContext {
    /// Project name (from Cargo.toml, package.json, or directory name).
    pub project_name: String,
    /// README content (truncated).
    pub readme: Option<String>,
    /// Architecture doc content (truncated).
    pub architecture_doc: Option<String>,
}

// ── LLM response types ────────────────────────────────────────────────────

/// LLM response for a single component's name + description.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ComponentLabel {
    /// Component ID this label applies to.
    #[serde(alias = "id")]
    pub component_id: String,
    /// Human-readable component title (e.g., "Authentication & Authorization").
    pub title: String,
    /// 1-2 sentence description.
    pub description: String,
}

/// LLM response for a single inferred feature.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InferredFeature {
    /// Generated feature ID (e.g., "infer:feature:auth").
    pub feature_id: String,
    /// Human-readable feature title.
    pub title: String,
    /// 1-2 sentence description.
    pub description: String,
    /// Component IDs that belong to this feature (N:M relationship).
    pub component_ids: Vec<String>,
}

// ── Result types ───────────────────────────────────────────────────────────

/// Token usage tracking for LLM calls.
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    /// Tokens used for component naming.
    pub naming_tokens: usize,
    /// Tokens used for feature inference.
    pub feature_tokens: usize,
    /// Total tokens across all calls.
    pub total_tokens: usize,
}

/// Complete output of the labeling pipeline.
#[derive(Debug, Clone)]
pub struct LabelingResult {
    /// Component labels (names + descriptions) from LLM.
    pub component_labels: Vec<ComponentLabel>,
    /// Inferred feature nodes.
    pub features: Vec<InferredFeature>,
    /// Feature-level dependency edges (algorithmic, not LLM).
    pub feature_edges: Vec<Edge>,
    /// Token usage breakdown.
    pub token_usage: TokenUsage,
}

impl LabelingResult {
    /// Create an empty result (used when llm=None or budget exhausted).
    pub fn empty() -> Self {
        Self {
            component_labels: Vec::new(),
            features: Vec::new(),
            feature_edges: Vec::new(),
            token_usage: TokenUsage::default(),
        }
    }

    /// Whether any labeling was performed.
    pub fn is_empty(&self) -> bool {
        self.component_labels.is_empty() && self.features.is_empty()
    }
}

// ── Function stubs (to be implemented in T2.2b–T2.2f) ─────────────────────

/// Assemble LLM-ready contexts from clustering result + source graph.
///
/// For each component in `cluster_result`, gathers file names, class names,
/// function signatures, and file doc comments from the source `graph`.
/// Also extracts project-level context (README, architecture docs).
///
/// GOAL-2.4: README/architecture docs enhance LLM context.
pub fn assemble_contexts(
    graph: &Graph,
    cluster_result: &ClusterResult,
    config: &LabelingConfig,
) -> (Vec<ComponentContext>, ProjectContext) {
    // ── Build lookups ──────────────────────────────────────────────────────

    // node_id → &Node for fast lookup
    let node_map: HashMap<&str, &Node> = graph
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect();

    // component_id → set of member node IDs (from "contains" edges in cluster result)
    let mut component_members: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &cluster_result.edges {
        if edge.relation == "contains" {
            // Component → member: "from" is the component, "to" is the member.
            // Only collect edges where "from" is a component node in this result.
            component_members
                .entry(edge.from.as_str())
                .or_default()
                .push(edge.to.as_str());
        }
    }

    // For resolving children of file nodes (e.g., functions/structs defined in a file):
    // Build file_id → Vec<&Node> from graph edges with relation "defined_in" or "contains"
    let mut file_children: HashMap<&str, Vec<&Node>> = HashMap::new();
    for edge in &graph.edges {
        // "defined_in" edge: from=child, to=file
        if edge.relation == "defined_in" {
            if let Some(child_node) = node_map.get(edge.from.as_str()) {
                file_children
                    .entry(edge.to.as_str())
                    .or_default()
                    .push(child_node);
            }
        }
        // "contains" edge from file to child (used by some extractors)
        if edge.relation == "contains" {
            if let Some(from_node) = node_map.get(edge.from.as_str()) {
                if from_node.node_type.as_deref() == Some("file") {
                    if let Some(child_node) = node_map.get(edge.to.as_str()) {
                        file_children
                            .entry(edge.from.as_str())
                            .or_default()
                            .push(child_node);
                    }
                }
            }
        }
    }

    // ── Build component contexts ───────────────────────────────────────────

    let mut contexts: Vec<ComponentContext> = Vec::new();

    for comp_node in &cluster_result.nodes {
        let comp_id = comp_node.id.as_str();
        let auto_name = comp_node.title.clone();

        let members = component_members
            .get(comp_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);

        let mut files: Vec<String> = Vec::new();
        let mut class_names: Vec<String> = Vec::new();
        let mut function_signatures: Vec<String> = Vec::new();
        let mut file_briefs: Vec<String> = Vec::new();

        // Track seen items to avoid duplicates
        let mut seen_classes: HashSet<String> = HashSet::new();
        let mut seen_functions: HashSet<String> = HashSet::new();

        for &member_id in members {
            let member = match node_map.get(member_id) {
                Some(n) => n,
                None => continue,
            };

            // Collect file paths
            if let Some(ref fp) = member.file_path {
                files.push(fp.clone());
            } else if let Some(fp) = member_id.strip_prefix("file:") {
                files.push(fp.to_string());
            }

            // If this is a file node, gather its children (classes, functions)
            let is_file = member.node_type.as_deref() == Some("file");
            if is_file {
                // Collect file brief from doc_comment
                if file_briefs.len() < config.max_briefs_per_component {
                    if let Some(ref doc) = member.doc_comment {
                        let brief = truncate_str(doc, 200);
                        if !brief.is_empty() {
                            let fp = member.file_path.as_deref()
                                .or_else(|| member_id.strip_prefix("file:"))
                                .unwrap_or(member_id);
                            file_briefs.push(format!("{}: {}", fp, brief));
                        }
                    }
                }

                // Gather children of this file
                if let Some(children) = file_children.get(member_id) {
                    for child in children {
                        collect_code_node(
                            child,
                            &mut class_names,
                            &mut function_signatures,
                            &mut seen_classes,
                            &mut seen_functions,
                            config,
                        );
                    }
                }
            } else {
                // Non-file member (function, struct, etc.) — collect directly
                collect_code_node(
                    member,
                    &mut class_names,
                    &mut function_signatures,
                    &mut seen_classes,
                    &mut seen_functions,
                    config,
                );
            }
        }

        // Sort for deterministic output
        files.sort();
        class_names.sort();
        function_signatures.sort();
        file_briefs.sort();

        // For hierarchical parents with no direct files, collect child component names.
        let child_component_names: Vec<String> = if files.is_empty() {
            // Find child component edges (component → component "contains" edges)
            let child_ids: Vec<&str> = cluster_result
                .edges
                .iter()
                .filter(|e| {
                    e.relation == "contains"
                        && e.from == comp_id
                        && e.to.starts_with("infer:component:")
                })
                .map(|e| e.to.as_str())
                .collect();

            // Look up child component titles from cluster_result.nodes
            let mut names: Vec<String> = Vec::new();
            for child_id in child_ids {
                if let Some(child_node) = cluster_result.nodes.iter().find(|n| n.id == child_id) {
                    names.push(child_node.title.clone());
                }
            }
            names.sort();
            names
        } else {
            Vec::new()
        };

        contexts.push(ComponentContext {
            component_id: comp_id.to_string(),
            auto_name,
            files,
            class_names,
            function_signatures,
            file_briefs,
            child_component_names,
        });
    }

    // ── Build project context ──────────────────────────────────────────────

    let project_ctx = build_project_context(graph, &node_map, config);

    (contexts, project_ctx)
}

/// Classify a code node and collect its name/signature into the appropriate bucket.
fn collect_code_node(
    node: &Node,
    class_names: &mut Vec<String>,
    function_signatures: &mut Vec<String>,
    seen_classes: &mut HashSet<String>,
    seen_functions: &mut HashSet<String>,
    config: &LabelingConfig,
) {
    let kind = node.node_kind.as_deref().unwrap_or("");

    match kind.to_lowercase().as_str() {
        "struct" | "enum" | "trait" | "class" | "interface" | "type" | "typedef" => {
            if !seen_classes.contains(&node.title) {
                seen_classes.insert(node.title.clone());
                class_names.push(node.title.clone());
            }
        }
        "function" | "method" | "fn" | "async_fn" => {
            if function_signatures.len() < config.max_functions_per_component {
                let sig = node.signature.as_deref().unwrap_or(&node.title);
                if !seen_functions.contains(sig) {
                    seen_functions.insert(sig.to_string());
                    function_signatures.push(sig.to_string());
                }
            }
        }
        "impl" | "impl_block" => {
            // Impl blocks: collect the title as a class name (e.g., "impl Foo")
            if !seen_classes.contains(&node.title) {
                seen_classes.insert(node.title.clone());
                class_names.push(node.title.clone());
            }
        }
        _ => {
            // Unknown kind — try to infer from node_type
            match node.node_type.as_deref() {
                Some("class") | Some("struct") | Some("enum") | Some("trait") => {
                    if !seen_classes.contains(&node.title) {
                        seen_classes.insert(node.title.clone());
                        class_names.push(node.title.clone());
                    }
                }
                Some("function") | Some("method") => {
                    if function_signatures.len() < config.max_functions_per_component {
                        let sig = node.signature.as_deref().unwrap_or(&node.title);
                        if !seen_functions.contains(sig) {
                            seen_functions.insert(sig.to_string());
                            function_signatures.push(sig.to_string());
                        }
                    }
                }
                _ => {
                    // Fallback: if it has a signature, treat as function; otherwise skip.
                    if let Some(ref sig) = node.signature {
                        if function_signatures.len() < config.max_functions_per_component
                            && !seen_functions.contains(sig.as_str())
                        {
                            seen_functions.insert(sig.clone());
                            function_signatures.push(sig.clone());
                        }
                    }
                }
            }
        }
    }
}

/// Build project-level context from the graph.
fn build_project_context(
    graph: &Graph,
    node_map: &HashMap<&str, &Node>,
    config: &LabelingConfig,
) -> ProjectContext {
    // Project name from graph.project
    let project_name = graph
        .project
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_default();

    // Look for README.md and ARCHITECTURE.md in graph nodes
    let readme = find_doc_content(graph, node_map, &["readme.md", "README.md"], config.max_context_chars);
    let architecture_doc = find_doc_content(
        graph,
        node_map,
        &["architecture.md", "ARCHITECTURE.md", "DESIGN.md", "design.md"],
        config.max_context_chars,
    );

    ProjectContext {
        project_name,
        readme,
        architecture_doc,
    }
}

/// Search for a documentation file in the graph and extract its content.
///
/// Looks for file nodes whose `file_path` matches any of the given names.
/// Content is extracted from `doc_comment`, `body`, or `description` fields.
fn find_doc_content(
    graph: &Graph,
    node_map: &HashMap<&str, &Node>,
    names: &[&str],
    max_chars: usize,
) -> Option<String> {
    // Strategy 1: Look for file nodes with matching file_path
    for node in &graph.nodes {
        if let Some(ref fp) = node.file_path {
            let filename = fp.rsplit('/').next().unwrap_or(fp);
            for &name in names {
                if filename.eq_ignore_ascii_case(name) {
                    // Try body, then doc_comment, then description
                    if let Some(content) = node
                        .body
                        .as_deref()
                        .or(node.doc_comment.as_deref())
                        .or(node.description.as_deref())
                    {
                        if !content.is_empty() {
                            return Some(truncate_str(content, max_chars));
                        }
                    }
                }
            }
        }
    }

    // Strategy 2: Look for nodes whose ID matches (e.g., "file:README.md")
    for &name in names {
        let file_id = format!("file:{}", name);
        if let Some(node) = node_map.get(file_id.as_str()) {
            if let Some(content) = node
                .body
                .as_deref()
                .or(node.doc_comment.as_deref())
                .or(node.description.as_deref())
            {
                if !content.is_empty() {
                    return Some(truncate_str(content, max_chars));
                }
            }
        }
    }

    None
}

/// Truncate a string to at most `max_chars`, breaking at the last word boundary.
fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        return s.to_string();
    }

    // Find the last space or newline before max_chars to avoid mid-word truncation.
    // Use char_indices for UTF-8 safety.
    let mut last_break = max_chars;
    for (i, c) in s.char_indices() {
        if i > max_chars {
            break;
        }
        if c == ' ' || c == '\n' || c == '\t' {
            last_break = i;
        }
    }

    let truncated = &s[..last_break];
    format!("{}…", truncated.trim_end())
}

/// Build the LLM prompt for naming a batch of components.
///
/// Follows the template from design §3.2.1.
fn build_naming_prompt(
    batch: &[&ComponentContext],
    project_ctx: &ProjectContext,
) -> String {
    let mut prompt = String::with_capacity(4096);

    // Project context header
    if project_ctx.project_name.is_empty() {
        prompt.push_str("You are analyzing a software project.\n");
    } else {
        prompt.push_str(&format!(
            "You are analyzing the software project \"{}\".\n",
            project_ctx.project_name
        ));
    }

    // README excerpt if available
    if let Some(ref readme) = project_ctx.readme {
        prompt.push_str("\nProject README (excerpt):\n");
        prompt.push_str(readme);
        prompt.push('\n');
    }

    prompt.push_str(
        "\nBelow are code components detected by clustering analysis. For each component, \
         provide a concise title (2-5 words) and a one-sentence description.\n\n\
         Components:\n",
    );

    for ctx in batch {
        prompt.push_str("---\n");
        prompt.push_str(&format!("Component ID: {}\n", ctx.component_id));

        if !ctx.files.is_empty() {
            let files_str: String = ctx.files.iter().take(15).cloned().collect::<Vec<_>>().join(", ");
            prompt.push_str(&format!("Files: {}\n", files_str));
        }

        if !ctx.class_names.is_empty() {
            let classes_str: String = ctx.class_names.join(", ");
            prompt.push_str(&format!("Classes/Structs: {}\n", classes_str));
        }

        if !ctx.function_signatures.is_empty() {
            let fns: Vec<&str> = ctx.function_signatures.iter().take(10).map(|s| s.as_str()).collect();
            prompt.push_str(&format!("Key Functions: {}\n", fns.join(", ")));
        }

        if !ctx.file_briefs.is_empty() {
            prompt.push_str("File Descriptions:\n");
            for brief in ctx.file_briefs.iter().take(5) {
                prompt.push_str(&format!("  - {}\n", brief));
            }
        }

        if !ctx.child_component_names.is_empty() {
            let children_str: String = ctx.child_component_names.join(", ");
            prompt.push_str(&format!("Sub-components: {}\n", children_str));
            if ctx.files.is_empty() {
                prompt.push_str("(This is a parent component grouping the above sub-components; it has no direct files.)\n");
            }
        }

        prompt.push('\n');
    }

    prompt.push_str(
        "Respond in JSON only (no explanation, no markdown outside the JSON):\n\
         [\n  {\"id\": \"<component_id>\", \"title\": \"...\", \"description\": \"...\"},\n  ...\n]\n",
    );

    prompt
}

/// Build the LLM prompt for inferring features from named components.
///
/// Follows the template from design §3.3.1.
fn build_feature_prompt(
    labels: &[ComponentLabel],
    project_ctx: &ProjectContext,
) -> String {
    let mut prompt = String::with_capacity(4096);

    // Project context header
    if project_ctx.project_name.is_empty() {
        prompt.push_str("You are analyzing a software project.\n");
    } else {
        prompt.push_str(&format!(
            "You are analyzing the software project \"{}\".\n",
            project_ctx.project_name
        ));
    }

    // README excerpt if available
    if let Some(ref readme) = project_ctx.readme {
        prompt.push_str("\nProject README (excerpt):\n");
        prompt.push_str(readme);
        prompt.push('\n');
    }

    prompt.push_str(
        "\nThe project has these components (detected by code analysis):\n",
    );

    for label in labels {
        prompt.push_str(&format!(
            "- {}: {} — {}\n",
            label.component_id, label.title, label.description
        ));
    }

    prompt.push_str(
        "\nGroup these components into high-level features.\n\n\
         Constraints:\n\
         - Each feature should contain 2-8 components. A feature with 1 component is too granular; more than 8 suggests it should be split.\n\
         - Features must have specific, domain-relevant names. Generic names like \"Core\", \"Utilities\", \"Misc\", \"Common\" are banned.\n\
         - If a component doesn't fit any feature naturally, give it its own single-component feature rather than a catch-all bucket.\n\
         - Features at the same level should be roughly similar in scope/granularity.\n\n\
         A component may belong to multiple features if it serves multiple purposes.\n\n\
         Respond in JSON only (no explanation, no markdown outside the JSON):\n\
         [\n  {\n    \"title\": \"...\",\n    \"description\": \"...\",\n    \
         \"components\": [\"<component_id>\", ...]\n  },\n  ...\n]\n",
    );

    prompt
}

/// Extract JSON from an LLM response, handling code fences and common issues.
fn extract_json_from_response(response: &str) -> Result<String> {
    // Try ```json ... ``` block first
    if let Some(start) = response.find("```json") {
        let content = &response[start + 7..];
        if let Some(end) = content.find("```") {
            return Ok(content[..end].trim().to_string());
        }
    }

    // Try plain ``` ... ``` block
    if let Some(start) = response.find("```") {
        let content = &response[start + 3..];
        if let Some(end) = content.find("```") {
            let inner = content[..end].trim();
            // Skip language identifier on first line if present
            if let Some(newline) = inner.find('\n') {
                let first_line = &inner[..newline].trim();
                if !first_line.starts_with('[') && !first_line.starts_with('{') {
                    return Ok(inner[newline..].trim().to_string());
                }
            }
            return Ok(inner.to_string());
        }
    }

    // Try raw JSON (starts with [ or {)
    let trimmed = response.trim();
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        return Ok(trimmed.to_string());
    }

    anyhow::bail!("No JSON found in LLM response")
}

/// Attempt to repair common JSON issues from LLM output.
fn repair_json(s: &str) -> String {
    let mut result = s.to_string();

    // Remove trailing commas before ] or }
    loop {
        let cleaned = result
            .replace(",\n]", "\n]")
            .replace(",\n}", "\n}")
            .replace(", ]", "]")
            .replace(", }", "}");
        if cleaned == result {
            break;
        }
        result = cleaned;
    }

    // Ensure the string ends with ] or } (truncated response)
    let trimmed = result.trim_end();
    if !trimmed.ends_with(']') && !trimmed.ends_with('}') {
        // Try to close open brackets
        let open_brackets = trimmed.chars().filter(|&c| c == '[').count();
        let close_brackets = trimmed.chars().filter(|&c| c == ']').count();
        let open_braces = trimmed.chars().filter(|&c| c == '{').count();
        let close_braces = trimmed.chars().filter(|&c| c == '}').count();

        result = trimmed.to_string();
        for _ in 0..(open_braces.saturating_sub(close_braces)) {
            result.push('}');
        }
        for _ in 0..(open_brackets.saturating_sub(close_brackets)) {
            result.push(']');
        }
    }

    result
}

/// Parse naming response: JSON array of [{id, title, description}, ...].
fn parse_naming_response(response: &str) -> Result<Vec<ComponentLabel>> {
    let json_str = extract_json_from_response(response)?;

    // Try direct parse
    if let Ok(labels) = serde_json::from_str::<Vec<ComponentLabel>>(&json_str) {
        return Ok(labels);
    }

    // Try repair + parse
    let repaired = repair_json(&json_str);
    serde_json::from_str::<Vec<ComponentLabel>>(&repaired)
        .context("Failed to parse naming JSON even after repair")
}

/// Parse feature response: JSON array of [{title, description, components}, ...].
fn parse_feature_response(response: &str) -> Result<Vec<RawFeatureResponse>> {
    let json_str = extract_json_from_response(response)?;

    // Try direct parse
    if let Ok(features) = serde_json::from_str::<Vec<RawFeatureResponse>>(&json_str) {
        return Ok(features);
    }

    // Try repair + parse
    let repaired = repair_json(&json_str);
    serde_json::from_str::<Vec<RawFeatureResponse>>(&repaired)
        .context("Failed to parse feature JSON even after repair")
}

/// Raw LLM response for a feature (before validation).
#[derive(Debug, Deserialize)]
struct RawFeatureResponse {
    title: String,
    description: String,
    components: Vec<String>,
}

/// Name components via LLM batching.
///
/// Sends component contexts to LLM in batches of `config.batch_size`.
/// Parses JSON responses, falls back to `auto_name` on failure.
///
/// GOAL-2.1: Every component gets a human-readable name.
pub async fn name_components(
    contexts: &[ComponentContext],
    project_ctx: &ProjectContext,
    llm: &dyn SimpleLlm,
    config: &mut LabelingConfig,
) -> Result<Vec<ComponentLabel>> {
    let mut all_labels: Vec<ComponentLabel> = Vec::new();

    // Process in batches
    for batch_start in (0..contexts.len()).step_by(config.batch_size) {
        let batch_end = (batch_start + config.batch_size).min(contexts.len());
        let batch: Vec<&ComponentContext> = contexts[batch_start..batch_end].iter().collect();

        let prompt = build_naming_prompt(&batch, project_ctx);

        // Check token budget before calling
        let estimated_tokens = llm.estimate_tokens(&prompt) * 2; // prompt + response estimate
        if !config.record_tokens(estimated_tokens) {
            // Budget exceeded — use auto_name fallback for remaining
            for ctx in &batch {
                all_labels.push(ComponentLabel {
                    component_id: ctx.component_id.clone(),
                    title: ctx.auto_name.clone(),
                    description: String::new(),
                });
            }
            continue;
        }

        // Call LLM
        let batch_num = batch_start / config.batch_size + 1;
        let total_batches = (contexts.len() + config.batch_size - 1) / config.batch_size;
        tracing::info!(batch = batch_num, total = total_batches, "Calling LLM for naming batch");
        match llm.complete(&prompt).await {
            Ok(response) => {
                tracing::debug!(response_len = response.len(), "Got LLM response");
                match parse_naming_response(&response) {
                    Ok(labels) => {
                        tracing::debug!(count = labels.len(), "Parsed labels");
                        // Match labels to batch components, fill in missing with fallback
                        let label_map: HashMap<&str, &ComponentLabel> = labels
                            .iter()
                            .map(|l| (l.component_id.as_str(), l))
                            .collect();

                        for ctx in &batch {
                            if let Some(label) = label_map.get(ctx.component_id.as_str()) {
                                all_labels.push((*label).clone());
                            } else {
                                // LLM didn't return this component — use fallback
                                all_labels.push(ComponentLabel {
                                    component_id: ctx.component_id.clone(),
                                    title: ctx.auto_name.clone(),
                                    description: String::new(),
                                });
                            }
                        }
                    }
                    Err(ref _e) => {
                        tracing::warn!(error = %_e, "LLM naming parse failure");
                        tracing::debug!(preview = &response[..response.len().min(500)], "Response preview");
                        // Parse failure — fall back to auto_name for entire batch
                        for ctx in &batch {
                            all_labels.push(ComponentLabel {
                                component_id: ctx.component_id.clone(),
                                title: ctx.auto_name.clone(),
                                description: String::new(),
                            });
                        }
                    }
                }
            }
            Err(ref _e) => {
                tracing::warn!(error = %_e, "LLM call failed, using fallback names");
                // LLM call failure — fall back to auto_name for entire batch
                for ctx in &batch {
                    all_labels.push(ComponentLabel {
                        component_id: ctx.component_id.clone(),
                        title: ctx.auto_name.clone(),
                        description: String::new(),
                    });
                }
            }
        }
    }

    // Ensure every context has a label (should be guaranteed by the loop above,
    // but handle edge cases)
    let labeled_ids: HashSet<String> = all_labels
        .iter()
        .map(|l| l.component_id.clone())
        .collect();

    for ctx in contexts {
        if !labeled_ids.contains(&ctx.component_id) {
            all_labels.push(ComponentLabel {
                component_id: ctx.component_id.clone(),
                title: ctx.auto_name.clone(),
                description: String::new(),
            });
        }
    }

    Ok(all_labels)
}

/// Infer feature groupings via LLM.
///
/// Takes all component labels and asks LLM to group them into business features.
/// N:M relationship: one component can belong to multiple features.
///
/// GOAL-2.2: Components → features.
pub async fn infer_features(
    labels: &[ComponentLabel],
    project_ctx: &ProjectContext,
    llm: &dyn SimpleLlm,
    config: &mut LabelingConfig,
) -> Result<Vec<InferredFeature>> {
    if labels.is_empty() {
        return Ok(Vec::new());
    }

    let prompt = build_feature_prompt(labels, project_ctx);

    // Check token budget
    let estimated_tokens = llm.estimate_tokens(&prompt) * 2;
    tracing::debug!(
        prompt_tokens = estimated_tokens / 2,
        budget_remaining = config.tokens_remaining(),
        "Feature inference token estimate"
    );
    if !config.record_tokens(estimated_tokens) {
        tracing::warn!("Token budget exceeded for feature inference, skipping");
        return Ok(Vec::new()); // Budget exceeded, graceful degradation
    }

    // Collect valid component IDs for validation
    // Also build a map for fuzzy matching (LLMs sometimes return just the number)
    let valid_ids: HashSet<&str> = labels
        .iter()
        .map(|l| l.component_id.as_str())
        .collect();
    
    // Build fuzzy lookup: "42" → "infer:component:42", etc.
    let mut fuzzy_map: HashMap<String, &str> = HashMap::new();
    for id in &valid_ids {
        fuzzy_map.insert(id.to_string(), id);
        // Extract trailing number: "infer:component:42" → "42"
        if let Some(num) = id.rsplit(':').next() {
            fuzzy_map.insert(num.to_string(), id);
        }
        // Also try without prefix: "component:42"
        if let Some(rest) = id.strip_prefix("infer:") {
            fuzzy_map.insert(rest.to_string(), id);
        }
    }

    // Call LLM
    tracing::info!("Calling LLM for feature inference");
    let response = match llm.complete(&prompt).await {
        Ok(r) => r,
        Err(ref _e) => {
            tracing::warn!(error = %_e, "Feature LLM call failed");
            return Ok(Vec::new());
        }
    };
    tracing::debug!(response_len = response.len(), "Feature response received");

    // Parse response
    let raw_features = match parse_feature_response(&response) {
        Ok(f) => {
            tracing::debug!(count = f.len(), "Parsed features");
            for rf in &f {
                tracing::trace!(
                    title = %rf.title,
                    component_count = rf.components.len(),
                    "Feature detail"
                );
            }
            f
        },
        Err(ref _e) => {
            tracing::warn!(error = %_e, "Feature parse failure");
            tracing::debug!(preview = &response[..response.len().min(500)], "Response preview");
            return Ok(Vec::new());
        }
    };

    // Validate and convert to InferredFeature
    let mut features: Vec<InferredFeature> = Vec::new();
    for (i, raw) in raw_features.into_iter().enumerate() {
        // Filter out invalid component IDs
        let valid_component_ids: Vec<String> = raw
            .components
            .iter()
            .filter_map(|id| {
                // Try exact match first, then fuzzy
                if valid_ids.contains(id.as_str()) {
                    Some(id.clone())
                } else if let Some(&canonical) = fuzzy_map.get(id.as_str()) {
                    Some(canonical.to_string())
                } else {
                    None
                }
            })
            .collect();

        let dropped = raw.components.len() - valid_component_ids.len();
        if dropped > 0 {
            let invalid: Vec<_> = raw.components.iter()
                .filter(|id| !valid_ids.contains(id.as_str()))
                .take(3)
                .collect();
            tracing::warn!(
                feature = %raw.title,
                dropped = dropped,
                kept = valid_component_ids.len(),
                invalid_sample = ?invalid,
                "Feature has invalid component refs"
            );
        }

        // Skip features with no valid components
        if valid_component_ids.is_empty() {
            tracing::warn!(feature = %raw.title, "Feature dropped: 0 valid component refs");
            continue;
        }

        // Generate slug from title for the feature ID
        let slug: String = raw
            .title
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-");

        let feature_id = if slug.is_empty() {
            format!("infer:feature:{}", i)
        } else {
            format!("infer:feature:{}", slug)
        };

        features.push(InferredFeature {
            feature_id,
            title: raw.title,
            description: raw.description,
            component_ids: valid_component_ids,
        });
    }

    // Cap at 10 features (sorted by component count, descending)
    if features.len() > 10 {
        features.sort_by(|a, b| b.component_ids.len().cmp(&a.component_ids.len()));
        features.truncate(10);
    }

    Ok(features)
}

/// Infer feature-level dependency edges (algorithmic, no LLM).
///
/// Counts cross-component edges between feature groups.
/// If ≥2 edges cross from feature A's components to feature B's → depends_on.
///
/// GOAL-2.3: Feature dependency inference.
pub fn infer_feature_deps(
    features: &[InferredFeature],
    graph: &Graph,
    cluster_result: &ClusterResult,
) -> Vec<Edge> {
    if features.len() < 2 {
        return Vec::new();
    }

    // Build component_id → set of feature IDs (N:M)
    let mut comp_to_features: HashMap<&str, Vec<&str>> = HashMap::new();
    for feature in features {
        for comp_id in &feature.component_ids {
            comp_to_features
                .entry(comp_id.as_str())
                .or_default()
                .push(feature.feature_id.as_str());
        }
    }

    // Build component_id → set of member file IDs (from cluster result contains edges)
    let mut comp_members: HashMap<&str, HashSet<&str>> = HashMap::new();
    for edge in &cluster_result.edges {
        if edge.relation == "contains" {
            comp_members
                .entry(edge.from.as_str())
                .or_default()
                .insert(edge.to.as_str());
        }
    }

    // Invert: file_id → component_id
    let mut file_to_comp: HashMap<&str, &str> = HashMap::new();
    for (comp_id, members) in &comp_members {
        for &member_id in members {
            file_to_comp.insert(member_id, comp_id);
        }
    }

    // Count cross-feature edges from the source graph
    // (feature_a, feature_b) → count of cross-component edges
    let mut cross_counts: HashMap<(&str, &str), usize> = HashMap::new();

    for edge in &graph.edges {
        // Resolve both endpoints to their component
        let from_comp = file_to_comp.get(edge.from.as_str());
        let to_comp = file_to_comp.get(edge.to.as_str());

        if let (Some(&from_c), Some(&to_c)) = (from_comp, to_comp) {
            // Skip intra-component edges
            if from_c == to_c {
                continue;
            }

            // Get features for each component
            let from_features = comp_to_features.get(from_c);
            let to_features = comp_to_features.get(to_c);

            if let (Some(ff), Some(tf)) = (from_features, to_features) {
                for &f_from in ff {
                    for &f_to in tf {
                        // Skip self-dependencies and shared-membership pairs
                        if f_from != f_to {
                            *cross_counts.entry((f_from, f_to)).or_insert(0) += 1;
                        }
                    }
                }
            }
        }
    }

    // Create depends_on edges for pairs with ≥2 cross-component edges
    let mut dep_edges: Vec<Edge> = Vec::new();
    for (&(from_feature, to_feature), &count) in &cross_counts {
        if count >= 2 {
            let mut edge = Edge::new(from_feature, to_feature, "depends_on");
            edge.weight = Some(count as f64);
            edge.metadata = Some(serde_json::json!({
                "source": "infer",
                "cross_edge_count": count,
            }));
            dep_edges.push(edge);
        }
    }

    // Sort for deterministic output
    dep_edges.sort_by(|a, b| a.from.cmp(&b.from).then(a.to.cmp(&b.to)));

    dep_edges
}

/// Main labeling entry point.
///
/// Orchestrates: assemble_contexts → name_components → infer_features → infer_feature_deps.
/// If `llm` is None, returns empty result (GOAL-2.5 / GUARD-3 graceful degradation).
///
/// GOAL-2.5, GUARD-3, GUARD-4.
pub async fn label(
    graph: &Graph,
    cluster_result: &ClusterResult,
    llm: Option<&dyn SimpleLlm>,
    mut config: LabelingConfig,
) -> Result<LabelingResult> {
    // GUARD-3 / GOAL-2.5: no LLM → return empty result
    let llm = match llm {
        Some(l) => l,
        None => return Ok(LabelingResult::empty()),
    };

    // No components → nothing to label
    if cluster_result.nodes.is_empty() {
        return Ok(LabelingResult::empty());
    }

    // Step 1: Assemble contexts
    let (contexts, project_ctx) = assemble_contexts(graph, cluster_result, &config);

    if contexts.is_empty() {
        return Ok(LabelingResult::empty());
    }

    // Step 2: Name components (LLM call 1, batched)
    let component_labels = name_components(&contexts, &project_ctx, llm, &mut config).await?;

    let naming_tokens = config.tokens_used();

    // Step 3: Infer features (LLM call 2)
    let features = infer_features(&component_labels, &project_ctx, llm, &mut config).await?;

    let feature_tokens = config.tokens_used() - naming_tokens;

    // Step 4: Derive feature dependencies (no LLM)
    let feature_edges = infer_feature_deps(&features, graph, cluster_result);

    let token_usage = TokenUsage {
        naming_tokens,
        feature_tokens,
        total_tokens: config.tokens_used(),
    };

    Ok(LabelingResult {
        component_labels,
        features,
        feature_edges,
        token_usage,
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Edge, Graph, Node};
    use super::super::clustering::{ClusterMetrics, ClusterResult};

    /// Helper: build a file node with given id and file_path.
    fn file_node(id: &str, path: &str) -> Node {
        let mut n = Node::new(id, path);
        n.node_type = Some("file".into());
        n.file_path = Some(path.into());
        n
    }

    /// Helper: build a code node (struct/function) belonging to a file.
    fn code_node(id: &str, title: &str, kind: &str, sig: Option<&str>) -> Node {
        let mut n = Node::new(id, title);
        n.node_type = Some(if kind == "struct" || kind == "class" { "class" } else { "function" }.into());
        n.node_kind = Some(kind.into());
        if let Some(s) = sig {
            n.signature = Some(s.into());
        }
        n
    }

    /// Helper: build a component node for ClusterResult.
    fn component_node(id: &str, title: &str) -> Node {
        let mut n = Node::new(id, title);
        n.node_type = Some("component".into());
        n.source = Some("infer".into());
        n
    }

    /// Helper: build a simple ClusterResult with given components and membership edges.
    fn make_cluster_result(
        components: Vec<Node>,
        edges: Vec<Edge>,
    ) -> ClusterResult {
        let num = components.len();
        ClusterResult {
            nodes: components,
            edges,
            metrics: ClusterMetrics {
                codelength: 0.0,
                num_communities: num,
                num_total: 0,
                ..Default::default()
            },
        }
    }

    // ── Mock LLM ───────────────────────────────────────────────────────────

    struct MockLlm {
        responses: std::sync::Mutex<Vec<String>>,
        call_count: std::sync::atomic::AtomicUsize,
    }

    impl MockLlm {
        fn new(responses: Vec<String>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses),
                call_count: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.call_count.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl SimpleLlm for MockLlm {
        async fn complete(&self, _prompt: &str) -> anyhow::Result<String> {
            self.call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                anyhow::bail!("No more mock responses")
            }
            Ok(responses.remove(0))
        }
    }

    struct FailingLlm;

    #[async_trait::async_trait]
    impl SimpleLlm for FailingLlm {
        async fn complete(&self, _: &str) -> anyhow::Result<String> {
            anyhow::bail!("LLM service unavailable")
        }
    }

    // ── 1. test_assemble_context_basic ─────────────────────────────────────

    #[test]
    fn test_assemble_context_basic() {
        let mut graph = Graph::new();

        // File nodes
        let mut f1 = file_node("file:src/auth.rs", "src/auth.rs");
        f1.doc_comment = Some("Authentication module".into());
        graph.add_node(f1);
        graph.add_node(file_node("file:src/db.rs", "src/db.rs"));

        // Code nodes (children of files)
        let auth_struct = code_node("struct:AuthService", "AuthService", "struct", None);
        let db_fn = code_node("fn:connect", "connect", "function", Some("fn connect(url: &str)"));
        graph.add_node(auth_struct);
        graph.add_node(db_fn);

        // defined_in edges: code node → file
        graph.add_edge(Edge::new("struct:AuthService", "file:src/auth.rs", "defined_in"));
        graph.add_edge(Edge::new("fn:connect", "file:src/db.rs", "defined_in"));

        // ClusterResult: 2 components
        let comp0 = component_node("infer:component:0", "auth");
        let comp1 = component_node("infer:component:1", "db");
        let cluster = ClusterResult {
            nodes: vec![comp0, comp1],
            edges: vec![
                Edge::new("infer:component:0", "file:src/auth.rs", "contains"),
                Edge::new("infer:component:1", "file:src/db.rs", "contains"),
            ],
            metrics: ClusterMetrics {
                codelength: 0.0,
                num_communities: 2,
                num_total: 2,
                ..Default::default()
            },
        };

        let config = LabelingConfig::default();
        let (contexts, _project_ctx) = assemble_contexts(&graph, &cluster, &config);

        assert_eq!(contexts.len(), 2);

        // Component 0: auth
        let ctx0 = contexts.iter().find(|c| c.component_id == "infer:component:0").unwrap();
        assert!(ctx0.files.contains(&"src/auth.rs".to_string()));
        assert!(ctx0.class_names.contains(&"AuthService".to_string()));

        // Component 1: db
        let ctx1 = contexts.iter().find(|c| c.component_id == "infer:component:1").unwrap();
        assert!(ctx1.files.contains(&"src/db.rs".to_string()));
        assert!(ctx1.function_signatures.contains(&"fn connect(url: &str)".to_string()));
    }

    // ── 2. test_assemble_context_with_readme ───────────────────────────────

    #[test]
    fn test_assemble_context_with_readme() {
        let mut graph = Graph::new();

        // Add a README node
        let mut readme = Node::new("file:README.md", "README.md");
        readme.node_type = Some("file".into());
        readme.file_path = Some("README.md".into());
        readme.body = Some("# My Project\nThis is a cool project.".into());
        graph.add_node(readme);

        // A file node so we have something to cluster
        graph.add_node(file_node("file:src/main.rs", "src/main.rs"));

        let cluster = ClusterResult {
            nodes: vec![component_node("infer:component:0", "main")],
            edges: vec![
                Edge::new("infer:component:0", "file:src/main.rs", "contains"),
            ],
            metrics: ClusterMetrics { codelength: 0.0, num_communities: 1, num_total: 1, ..Default::default() },
        };

        let config = LabelingConfig::default();
        let (_contexts, project_ctx) = assemble_contexts(&graph, &cluster, &config);

        assert!(project_ctx.readme.is_some());
        assert!(project_ctx.readme.unwrap().contains("My Project"));
    }

    // ── 3. test_assemble_context_no_readme ─────────────────────────────────

    #[test]
    fn test_assemble_context_no_readme() {
        let mut graph = Graph::new();
        graph.add_node(file_node("file:src/lib.rs", "src/lib.rs"));

        let cluster = ClusterResult {
            nodes: vec![component_node("infer:component:0", "lib")],
            edges: vec![
                Edge::new("infer:component:0", "file:src/lib.rs", "contains"),
            ],
            metrics: ClusterMetrics { codelength: 0.0, num_communities: 1, num_total: 1, ..Default::default() },
        };

        let config = LabelingConfig::default();
        let (_contexts, project_ctx) = assemble_contexts(&graph, &cluster, &config);

        assert!(project_ctx.readme.is_none());
    }

    // ── 4. test_parse_naming_response ──────────────────────────────────────

    #[test]
    fn test_parse_naming_response() {
        let json = r#"[
            {"component_id": "infer:component:0", "title": "Auth Module", "description": "Handles authentication"},
            {"component_id": "infer:component:1", "title": "Database Layer", "description": "DB access"}
        ]"#;

        let labels = parse_naming_response(json).unwrap();
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0].component_id, "infer:component:0");
        assert_eq!(labels[0].title, "Auth Module");
        assert_eq!(labels[1].component_id, "infer:component:1");
        assert_eq!(labels[1].title, "Database Layer");
    }

    // ── 5. test_parse_naming_response_invalid ──────────────────────────────

    #[test]
    fn test_parse_naming_response_invalid() {
        let bad_json = "this is not json at all";
        let result = parse_naming_response(bad_json);
        assert!(result.is_err());
    }

    // ── 6. test_parse_feature_response ─────────────────────────────────────

    #[test]
    fn test_parse_feature_response() {
        let json = r#"[
            {"title": "User Management", "description": "All user-related features", "components": ["infer:component:0", "infer:component:1"]},
            {"title": "Data Pipeline", "description": "ETL and processing", "components": ["infer:component:2"]}
        ]"#;

        let features = parse_feature_response(json).unwrap();
        assert_eq!(features.len(), 2);
        assert_eq!(features[0].title, "User Management");
        assert_eq!(features[0].components.len(), 2);
        assert_eq!(features[1].title, "Data Pipeline");
        assert_eq!(features[1].components, vec!["infer:component:2"]);
    }

    // ── 7. test_parse_feature_response_invalid ─────────────────────────────

    #[test]
    fn test_parse_feature_response_invalid() {
        let bad = "not valid json {{[";
        let result = parse_feature_response(bad);
        assert!(result.is_err());
    }

    // ── 8. test_infer_feature_deps ─────────────────────────────────────────

    #[test]
    fn test_infer_feature_deps() {
        // Graph: 4 file nodes, with cross-component "calls" edges
        let mut graph = Graph::new();
        graph.add_node(file_node("file:a1.rs", "a1.rs"));
        graph.add_node(file_node("file:a2.rs", "a2.rs"));
        graph.add_node(file_node("file:b1.rs", "b1.rs"));
        graph.add_node(file_node("file:b2.rs", "b2.rs"));

        // 2 cross-component edges: a1→b1, a2→b2
        graph.add_edge(Edge::new("file:a1.rs", "file:b1.rs", "calls"));
        graph.add_edge(Edge::new("file:a2.rs", "file:b2.rs", "calls"));

        // ClusterResult: 2 components
        let cluster = ClusterResult {
            nodes: vec![
                component_node("infer:component:0", "comp-a"),
                component_node("infer:component:1", "comp-b"),
            ],
            edges: vec![
                Edge::new("infer:component:0", "file:a1.rs", "contains"),
                Edge::new("infer:component:0", "file:a2.rs", "contains"),
                Edge::new("infer:component:1", "file:b1.rs", "contains"),
                Edge::new("infer:component:1", "file:b2.rs", "contains"),
            ],
            metrics: ClusterMetrics { codelength: 0.0, num_communities: 2, num_total: 4, ..Default::default() },
        };

        // Features: each maps to one component
        let features = vec![
            InferredFeature {
                feature_id: "infer:feature:alpha".into(),
                title: "Alpha".into(),
                description: "Feature alpha".into(),
                component_ids: vec!["infer:component:0".into()],
            },
            InferredFeature {
                feature_id: "infer:feature:beta".into(),
                title: "Beta".into(),
                description: "Feature beta".into(),
                component_ids: vec!["infer:component:1".into()],
            },
        ];

        let deps = infer_feature_deps(&features, &graph, &cluster);

        // Should have a depends_on edge: alpha → beta (2 cross-component edges)
        assert!(!deps.is_empty(), "Expected at least one depends_on edge");
        let has_dep = deps.iter().any(|e| {
            e.relation == "depends_on"
                && ((e.from == "infer:feature:alpha" && e.to == "infer:feature:beta")
                    || (e.from == "infer:feature:beta" && e.to == "infer:feature:alpha"))
        });
        assert!(has_dep, "Expected depends_on edge between alpha and beta");
    }

    // ── 9. test_infer_feature_deps_threshold ───────────────────────────────

    #[test]
    fn test_infer_feature_deps_threshold() {
        // Only 1 cross-component edge → should NOT produce depends_on
        let mut graph = Graph::new();
        graph.add_node(file_node("file:a.rs", "a.rs"));
        graph.add_node(file_node("file:b.rs", "b.rs"));

        // Only 1 cross-component edge
        graph.add_edge(Edge::new("file:a.rs", "file:b.rs", "calls"));

        let cluster = ClusterResult {
            nodes: vec![
                component_node("infer:component:0", "comp-a"),
                component_node("infer:component:1", "comp-b"),
            ],
            edges: vec![
                Edge::new("infer:component:0", "file:a.rs", "contains"),
                Edge::new("infer:component:1", "file:b.rs", "contains"),
            ],
            metrics: ClusterMetrics { codelength: 0.0, num_communities: 2, num_total: 2, ..Default::default() },
        };

        let features = vec![
            InferredFeature {
                feature_id: "infer:feature:x".into(),
                title: "X".into(),
                description: "".into(),
                component_ids: vec!["infer:component:0".into()],
            },
            InferredFeature {
                feature_id: "infer:feature:y".into(),
                title: "Y".into(),
                description: "".into(),
                component_ids: vec!["infer:component:1".into()],
            },
        ];

        let deps = infer_feature_deps(&features, &graph, &cluster);
        assert!(deps.is_empty(), "1 cross-component edge should NOT produce depends_on (threshold is 2)");
    }

    // ── 10. test_token_budget_enforcement ──────────────────────────────────

    #[test]
    fn test_token_budget_enforcement() {
        let mut config = LabelingConfig {
            token_budget: 100,
            ..LabelingConfig::default()
        };

        // Use 80 tokens → still within budget
        assert!(config.record_tokens(80));
        assert_eq!(config.tokens_remaining(), 20);

        // Use 30 more → exceeds budget (110 > 100)
        assert!(!config.record_tokens(30));
        assert_eq!(config.tokens_remaining(), 0);
        assert_eq!(config.tokens_used(), 110);
    }

    // ── 11. test_no_llm_mode ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_no_llm_mode() {
        let graph = Graph::new();
        let cluster = ClusterResult::empty();
        let config = LabelingConfig::default();

        let result = label(&graph, &cluster, None, config).await.unwrap();

        assert!(result.is_empty());
        assert!(result.component_labels.is_empty());
        assert!(result.features.is_empty());
        assert!(result.feature_edges.is_empty());
    }

    // ── 12. test_full_labeling_pipeline ────────────────────────────────────

    #[tokio::test]
    async fn test_full_labeling_pipeline() {
        let mut graph = Graph::new();
        graph.add_node(file_node("file:src/auth.rs", "src/auth.rs"));
        graph.add_node(file_node("file:src/db.rs", "src/db.rs"));

        let cluster = ClusterResult {
            nodes: vec![
                component_node("infer:component:0", "auth"),
                component_node("infer:component:1", "db"),
            ],
            edges: vec![
                Edge::new("infer:component:0", "file:src/auth.rs", "contains"),
                Edge::new("infer:component:1", "file:src/db.rs", "contains"),
            ],
            metrics: ClusterMetrics { codelength: 0.0, num_communities: 2, num_total: 2, ..Default::default() },
        };

        // Naming response
        let naming_json = r#"[
            {"component_id": "infer:component:0", "title": "Authentication", "description": "Auth logic"},
            {"component_id": "infer:component:1", "title": "Database", "description": "DB layer"}
        ]"#;

        // Feature response
        let feature_json = r#"[
            {"title": "User Auth", "description": "Authentication feature", "components": ["infer:component:0", "infer:component:1"]}
        ]"#;

        let llm = MockLlm::new(vec![naming_json.to_string(), feature_json.to_string()]);
        let config = LabelingConfig::default();

        let result = label(&graph, &cluster, Some(&llm), config).await.unwrap();

        // Should have 2 component labels
        assert_eq!(result.component_labels.len(), 2);
        let auth_label = result.component_labels.iter().find(|l| l.component_id == "infer:component:0").unwrap();
        assert_eq!(auth_label.title, "Authentication");

        // Should have 1 feature
        assert_eq!(result.features.len(), 1);
        assert_eq!(result.features[0].title, "User Auth");
        assert_eq!(result.features[0].component_ids.len(), 2);

        // LLM was called twice (once for naming, once for features)
        assert_eq!(llm.calls(), 2);
    }

    // ── 13. test_llm_failure_degradation ───────────────────────────────────

    #[tokio::test]
    async fn test_llm_failure_degradation() {
        let mut graph = Graph::new();
        graph.add_node(file_node("file:src/main.rs", "src/main.rs"));

        let cluster = ClusterResult {
            nodes: vec![component_node("infer:component:0", "main-auto")],
            edges: vec![
                Edge::new("infer:component:0", "file:src/main.rs", "contains"),
            ],
            metrics: ClusterMetrics { codelength: 0.0, num_communities: 1, num_total: 1, ..Default::default() },
        };

        let llm = FailingLlm;
        let config = LabelingConfig::default();

        let result = label(&graph, &cluster, Some(&llm), config).await.unwrap();

        // Should fall back to auto_name, not error
        assert_eq!(result.component_labels.len(), 1);
        assert_eq!(result.component_labels[0].title, "main-auto");
        // Features should be empty (LLM failed)
        assert!(result.features.is_empty());
    }

    // ── 14. test_batch_naming ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_batch_naming() {
        // 15 components with batch_size=5 → 3 LLM calls
        let mut graph = Graph::new();
        let mut comp_nodes = Vec::new();
        let mut comp_edges = Vec::new();

        for i in 0..15 {
            let file_id = format!("file:src/mod{}.rs", i);
            let file_path = format!("src/mod{}.rs", i);
            graph.add_node(file_node(&file_id, &file_path));

            let comp_id = format!("infer:component:{}", i);
            comp_nodes.push(component_node(&comp_id, &format!("mod{}", i)));
            comp_edges.push(Edge::new(&comp_id, &file_id, "contains"));
        }

        let cluster = ClusterResult {
            nodes: comp_nodes,
            edges: comp_edges,
            metrics: ClusterMetrics { codelength: 0.0, num_communities: 15, num_total: 15, ..Default::default() },
        };

        // Build 3 naming responses (batch_size=5 → batches of 5 components each)
        let mut responses = Vec::new();
        for batch_start in (0..15).step_by(5) {
            let batch_end = (batch_start + 5).min(15);
            let labels: Vec<String> = (batch_start..batch_end)
                .map(|i| {
                    format!(
                        r#"{{"component_id": "infer:component:{}", "title": "Module {}", "description": "desc {}"}}"#,
                        i, i, i
                    )
                })
                .collect();
            responses.push(format!("[{}]", labels.join(",")));
        }

        let llm = MockLlm::new(responses);

        let project_ctx = ProjectContext::default();
        let mut config = LabelingConfig {
            batch_size: 5,
            ..LabelingConfig::default()
        };

        let (contexts, _) = assemble_contexts(&graph, &cluster, &config);
        let labels = name_components(&contexts, &project_ctx, &llm, &mut config).await.unwrap();

        // All 15 components should be labeled
        assert_eq!(labels.len(), 15);

        // Exactly 3 LLM calls (15 / 5 = 3)
        assert_eq!(llm.calls(), 3, "Expected exactly 3 LLM batch calls for 15 components with batch_size=5");

        // Verify labels have the right titles
        for i in 0..15 {
            let label = labels.iter().find(|l| l.component_id == format!("infer:component:{}", i)).unwrap();
            assert_eq!(label.title, format!("Module {}", i));
        }
    }
}
