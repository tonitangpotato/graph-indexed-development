use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Instant, UNIX_EPOCH};

use regex::Regex;
use tree_sitter::Parser;
use walkdir::WalkDir;
use xxhash_rust::xxh64::xxh64;

use super::lang::{python::*, rust_lang::*, typescript::*};
use super::types::*;

// ═══ Current metadata version. Bump on struct changes → triggers full rebuild. ═══
const EXTRACT_META_VERSION: u32 = 1;

// ═══ Shared Helper Types ═══

/// Intermediate state collected during per-file parsing.
/// Holds all the maps needed for cross-file reference resolution.
#[derive(Default)]
struct ExtractState {
    nodes: Vec<CodeNode>,
    edges: Vec<CodeEdge>,
    class_map: HashMap<String, String>,
    func_map: HashMap<String, Vec<String>>,
    module_map: HashMap<String, String>,
    method_to_class: HashMap<String, String>,
    class_methods: HashMap<String, Vec<String>>,
    class_parents: HashMap<String, Vec<String>>,
    file_imported_names: HashMap<String, HashSet<String>>,
    all_struct_field_types: HashMap<String, HashMap<String, String>>,
}

/// Result of parsing a single file.
struct FileParseResult {
    nodes: Vec<CodeNode>,
    edges: Vec<CodeEdge>,
    imports: HashSet<String>,
    struct_field_types: HashMap<String, HashMap<String, String>>,
}

// ═══ Shared Helper Functions ═══

/// Walk a directory and collect source file entries (rel_path, content, language).
/// Also builds the module_map from file paths.
fn collect_source_files(
    dir: &Path,
    module_map: &mut HashMap<String, String>,
) -> Vec<(String, String, Language)> {
    let mut file_entries: Vec<(String, String, Language)> = Vec::new();
    // Collect partial path candidates: partial → Vec<file_id>
    // We defer insertion so we can detect ambiguous partials (same basename in different dirs).
    let mut partial_candidates: HashMap<String, Vec<String>> = HashMap::new();

    for entry in WalkDir::new(dir)
        .follow_links(false)
        .max_depth(20)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_str().unwrap_or("");
            !name.starts_with('.')
                && name != "node_modules"
                && name != "__pycache__"
                && name != "target"
                && name != "build"
                && name != "dist"
                && name != ".git"
                && name != ".eggs"
                && name != ".tox"
        })
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        let lang = Language::from_path(path);
        if lang == Language::Unknown {
            continue;
        }

        let rel_path = path
            .strip_prefix(dir)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        // Skip certain files
        if rel_path == "setup.py" || rel_path == "conftest.py" || rel_path.contains("__pycache__") {
            continue;
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Build module path
        let module_path = rel_path
            .replace('/', ".")
            .trim_end_matches(".py")
            .trim_end_matches(".rs")
            .trim_end_matches(".ts")
            .trim_end_matches(".tsx")
            .trim_end_matches(".js")
            .trim_end_matches(".jsx")
            .to_string();

        let file_id = format!("file:{}", rel_path);
        // Register full module path (always unique since it includes full relative path)
        module_map.insert(module_path.clone(), file_id.clone());

        // Collect partial path candidates (defer insertion to detect ambiguity)
        let parts: Vec<&str> = module_path.split('.').collect();
        for start in 1..parts.len() {
            let partial = parts[start..].join(".");
            partial_candidates.entry(partial).or_default().push(file_id.clone());
        }

        file_entries.push((rel_path, content, lang));
    }

    // Only register unambiguous partials — if two files share the same partial,
    // register neither to avoid ghost nodes (ISS-007).
    for (partial, candidates) in partial_candidates {
        if candidates.len() == 1 {
            module_map.entry(partial).or_insert_with(|| candidates.into_iter().next().unwrap());
        }
        // If len > 1, skip — ambiguous partial, don't register
    }

    file_entries
}

/// Parse a single file and return its nodes, edges, imports, and struct field types.
fn parse_single_file(
    rel_path: &str,
    content: &str,
    lang: &Language,
    parser: &mut Parser,
    class_map: &mut HashMap<String, String>,
) -> Option<FileParseResult> {
    let (file_nodes, file_edges, imports, struct_field_types) = match lang {
        Language::Python => {
            let (nodes, edges, imports) = extract_python_tree_sitter(
                rel_path, content, parser, class_map,
            );
            (nodes, edges, imports, HashMap::new())
        }
        Language::Rust => {
            let (nodes, edges, imports, field_types) = extract_rust_tree_sitter(
                rel_path, content, parser, class_map,
            );
            (nodes, edges, imports, field_types)
        }
        Language::TypeScript => {
            let ext = rel_path.rsplit('.').next().unwrap_or("ts");
            let (nodes, edges, imports) = extract_typescript_tree_sitter(
                rel_path, content, parser, class_map, ext,
            );
            (nodes, edges, imports, HashMap::new())
        }
        Language::Unknown => return None,
    };

    Some(FileParseResult {
        nodes: file_nodes,
        edges: file_edges,
        imports,
        struct_field_types,
    })
}

/// Integrate a parsed file's results into the ExtractState.
fn integrate_file_results(
    state: &mut ExtractState,
    rel_path: &str,
    result: FileParseResult,
) {
    // Update maps
    for node in &result.nodes {
        if node.kind == NodeKind::Class {
            state.class_map.insert(node.name.clone(), node.id.clone());
        } else if node.kind == NodeKind::Function {
            state.func_map
                .entry(node.name.clone())
                .or_default()
                .push(node.id.clone());
        }
    }

    // Track method→class and class→methods relationships
    for edge in &result.edges {
        if edge.relation == EdgeRelation::DefinedIn {
            if edge.from.starts_with("method:") && edge.to.starts_with("class:") {
                state.method_to_class.insert(edge.from.clone(), edge.to.clone());
                state.class_methods
                    .entry(edge.to.clone())
                    .or_default()
                    .push(edge.from.clone());
            }
        }
        if edge.relation == EdgeRelation::Inherits {
            if let Some(parent_id) = state.class_map.get(
                edge.to.strip_prefix("class_ref:").unwrap_or(&edge.to),
            ) {
                state.class_parents
                    .entry(edge.from.clone())
                    .or_default()
                    .push(parent_id.clone());
            }
        }
    }

    // Store imported names
    if !result.imports.is_empty() {
        state.file_imported_names.insert(rel_path.to_string(), result.imports);
    }

    // Store struct field types
    for (struct_name, fields) in result.struct_field_types {
        state.all_struct_field_types.insert(struct_name, fields);
    }

    // Add file node if we found entities
    if !result.nodes.is_empty() {
        state.nodes.push(CodeNode::new_file(rel_path));
    }

    state.nodes.extend(result.nodes);
    state.edges.extend(result.edges);
}

/// Build helper maps needed for call edge extraction (class_init_map, node_pkg_map).
fn build_call_extraction_maps(state: &ExtractState) -> (
    HashMap<String, Vec<(String, String)>>,
    HashMap<String, String>,
) {
    // class_init_map for constructor resolution
    let class_init_map: HashMap<String, Vec<(String, String)>> = {
        let mut map: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for node in &state.nodes {
            if node.kind == NodeKind::Function && node.name == "__init__" && !node.is_test {
                if let Some(class_id) = state.method_to_class.get(&node.id) {
                    if let Some(class_name) = class_id.rsplit(':').next() {
                        map.entry(class_name.to_string())
                            .or_default()
                            .push((node.file_path.clone(), node.id.clone()));
                    }
                }
            }
        }
        map
    };

    // node_pkg_map for package-scoped resolution
    let node_pkg_map: HashMap<String, String> = state.nodes
        .iter()
        .map(|n| {
            let pkg = n.file_path.rsplitn(2, '/').nth(1).unwrap_or("").to_string();
            (n.id.clone(), pkg)
        })
        .collect();

    (class_init_map, node_pkg_map)
}

/// Extract call edges for a specific file (third pass in the pipeline).
fn extract_calls_for_file(
    rel_path: &str,
    content: &str,
    lang: &Language,
    parser: &mut Parser,
    state: &ExtractState,
    class_init_map: &HashMap<String, Vec<(String, String)>>,
    node_pkg_map: &HashMap<String, String>,
    module_map: &HashMap<String, String>,
    edges: &mut Vec<CodeEdge>,
) {
    let file_func_ids: HashSet<String> = state.nodes
        .iter()
        .filter(|n| n.file_path == *rel_path && n.kind == NodeKind::Function)
        .map(|n| n.id.clone())
        .collect();

    let package_dir = rel_path.rsplitn(2, '/').nth(1).unwrap_or("");

    match lang {
        Language::Python => {
            if parser.set_language(&tree_sitter_python::LANGUAGE.into()).is_err() {
                return;
            }

            if let Some(tree) = parser.parse(content, None) {
                let source = content.as_bytes();
                let root = tree.root_node();

                extract_calls_from_tree(
                    root,
                    source,
                    rel_path,
                    &state.func_map,
                    &state.method_to_class,
                    &state.class_parents,
                    &file_func_ids,
                    &state.file_imported_names,
                    package_dir,
                    class_init_map,
                    node_pkg_map,
                    edges,
                );
            }

            // Test-to-source mapping for Python
            let is_test_file = rel_path.contains("/tests/") || rel_path.contains("/test_");
            if is_test_file {
                let file_id = format!("file:{}", rel_path);
                let re_from_import = Regex::new(r"^from\s+([\w.]+)\s+import").unwrap();

                for line in content.lines() {
                    if let Some(cap) = re_from_import.captures(line) {
                        let module = cap[1].to_string();
                        if let Some(source_file_id) = module_map.get(&module) {
                            edges.push(CodeEdge {
                                from: file_id.clone(),
                                to: source_file_id.clone(),
                                relation: EdgeRelation::TestsFor,
                                weight: 0.5,
                                call_count: 1,
                                in_error_path: false,
                                confidence: 1.0,
                                call_site_line: None,
                                call_site_column: None,
                            });
                        }
                    }
                }
            }
        }
        Language::Rust => {
            if parser.set_language(&tree_sitter_rust::LANGUAGE.into()).is_err() {
                return;
            }

            if let Some(tree) = parser.parse(content, None) {
                let source = content.as_bytes();
                let root = tree.root_node();

                extract_calls_rust(
                    root,
                    source,
                    rel_path,
                    &state.func_map,
                    &state.method_to_class,
                    &file_func_ids,
                    node_pkg_map,
                    &state.file_imported_names,
                    &state.all_struct_field_types,
                    edges,
                );
            }
        }
        Language::TypeScript => {
            let extension = rel_path.rsplit('.').next().unwrap_or("");
            let lang_result = match extension {
                "tsx" => parser.set_language(&tree_sitter_typescript::LANGUAGE_TSX.into()),
                "ts" => parser.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
                "jsx" => parser.set_language(&tree_sitter_javascript::LANGUAGE.into()),
                _ => parser.set_language(&tree_sitter_javascript::LANGUAGE.into()),
            };

            if lang_result.is_err() {
                return;
            }

            if let Some(tree) = parser.parse(content, None) {
                let source = content.as_bytes();
                let root = tree.root_node();

                extract_calls_typescript(
                    root,
                    source,
                    rel_path,
                    &state.func_map,
                    &state.method_to_class,
                    &file_func_ids,
                    &state.file_imported_names,
                    node_pkg_map,
                    edges,
                );
            }
        }
        Language::Unknown => {}
    }
}

/// Resolve placeholder references in edges (class_ref:, module_ref:, func_ref:).
fn resolve_references(
    edges: Vec<CodeEdge>,
    class_map: &HashMap<String, String>,
    func_map: &HashMap<String, Vec<String>>,
    module_map: &HashMap<String, String>,
) -> Vec<CodeEdge> {
    let mut resolved_edges = Vec::new();
    for edge in edges {
        if edge.to.starts_with("class_ref:") {
            let class_name = &edge.to["class_ref:".len()..];
            if let Some(class_id) = class_map.get(class_name) {
                resolved_edges.push(CodeEdge {
                    from: edge.from,
                    to: class_id.clone(),
                    relation: edge.relation,
                    weight: edge.weight,
                    call_count: edge.call_count,
                    in_error_path: edge.in_error_path,
                    confidence: edge.confidence,
                    call_site_line: edge.call_site_line,
                    call_site_column: edge.call_site_column,
                });
            }
        } else if edge.to.starts_with("module_ref:") {
            let module = &edge.to["module_ref:".len()..];
            let resolved_file_id = module_map.get(module).cloned()
                .or_else(|| {
                    let importing_file = edge.from.strip_prefix("file:").unwrap_or(&edge.from);
                    resolve_ts_import(importing_file, module, module_map)
                });

            if let Some(file_id) = resolved_file_id {
                resolved_edges.push(CodeEdge {
                    from: edge.from,
                    to: file_id,
                    relation: edge.relation,
                    weight: edge.weight,
                    call_count: edge.call_count,
                    in_error_path: edge.in_error_path,
                    confidence: edge.confidence,
                    call_site_line: edge.call_site_line,
                    call_site_column: edge.call_site_column,
                });
            }
        } else if edge.to.starts_with("func_ref:") {
            let func_name = &edge.to["func_ref:".len()..];
            if let Some(func_ids) = func_map.get(func_name) {
                if let Some(func_id) = func_ids.first() {
                    resolved_edges.push(CodeEdge {
                        from: edge.from,
                        to: func_id.clone(),
                        relation: edge.relation,
                        weight: edge.weight,
                        call_count: edge.call_count,
                        in_error_path: edge.in_error_path,
                        confidence: edge.confidence,
                        call_site_line: edge.call_site_line,
                        call_site_column: edge.call_site_column,
                    });
                }
            }
        } else {
            resolved_edges.push(edge);
        }
    }
    resolved_edges
}

/// Remove phantom file nodes — nodes with `kind == File` whose `file_path`
/// doesn't exist in the set of actual files we walked. Also removes edges
/// referencing removed nodes. (ISS-007 fix)
fn remove_phantom_nodes(
    nodes: &mut Vec<CodeNode>,
    edges: &mut Vec<CodeEdge>,
    valid_file_paths: &HashSet<&str>,
) {
    let before_nodes = nodes.len();
    nodes.retain(|n| {
        if n.kind == NodeKind::File {
            valid_file_paths.contains(n.file_path.as_str())
        } else {
            true
        }
    });
    let removed = before_nodes - nodes.len();
    if removed > 0 {
        tracing::debug!("Removed {} phantom file node(s)", removed);
        let valid_node_ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        edges.retain(|e| {
            valid_node_ids.contains(e.from.as_str()) && valid_node_ids.contains(e.to.as_str())
        });
    }
}

/// Deduplicate call edges, compute call_count, and compute weights.
fn dedup_and_finalize_edges(edges: Vec<CodeEdge>, nodes: &[CodeNode]) -> Vec<CodeEdge> {
    let mut edge_map: HashMap<(String, String), CodeEdge> = HashMap::new();
    let mut other_edges: Vec<CodeEdge> = Vec::new();

    for edge in edges {
        if edge.relation == EdgeRelation::Calls {
            let key = (edge.from.clone(), edge.to.clone());
            let entry = edge_map.entry(key).or_insert_with(|| {
                let mut e = edge.clone();
                e.call_count = 0;
                e
            });
            entry.call_count += 1;
            if edge.confidence > entry.confidence {
                entry.confidence = edge.confidence;
            }
            if edge.in_error_path {
                entry.in_error_path = true;
            }
        } else {
            other_edges.push(edge);
        }
    }

    let mut final_edges: Vec<CodeEdge> = edge_map.into_values().collect();
    final_edges.extend(other_edges);

    // Compute weights for all edges
    for edge in &mut final_edges {
        edge.compute_weight();
    }

    // Add override edges
    add_override_edges(nodes, &mut final_edges);

    final_edges
}

/// Compute the FileDelta between current filesystem and stored metadata.
/// (Hash-only variant, useful for testing without filesystem mtime)
#[allow(dead_code)]
pub fn compute_file_delta(
    current_files: &[(String, String, Language)],
    metadata: &ExtractMetadata,
) -> FileDelta {
    let mut delta = FileDelta::default();

    let current_paths: HashSet<&str> = current_files.iter().map(|(p, _, _)| p.as_str()).collect();
    let stored_paths: HashSet<&str> = metadata.files.keys().map(|p| p.as_str()).collect();

    for (rel_path, content, _lang) in current_files {
        if let Some(stored) = metadata.files.get(rel_path.as_str()) {
            // File exists in both — check if changed
            let content_hash = xxh64(content.as_bytes(), 0);
            if content_hash == stored.content_hash {
                delta.unchanged.push(rel_path.clone());
            } else {
                delta.modified.push(rel_path.clone());
            }
        } else {
            // New file
            delta.added.push(rel_path.clone());
        }
    }

    // Find deleted files
    for stored_path in &stored_paths {
        if !current_paths.contains(*stored_path) {
            delta.deleted.push(stored_path.to_string());
        }
    }

    delta
}

/// Build FileState for a file from its parsed results and content.
#[allow(dead_code)]
fn build_file_state(
    content: &str,
    node_ids: &[String],
    edge_count: usize,
) -> FileState {
    let mtime = 0u64; // Will be set by caller from filesystem metadata
    let content_hash = xxh64(content.as_bytes(), 0);
    FileState {
        mtime,
        content_hash,
        node_ids: node_ids.to_vec(),
        edge_count,
    }
}

/// Get the mtime for a file.
fn get_file_mtime(dir: &Path, rel_path: &str) -> u64 {
    let full_path = dir.join(rel_path);
    std::fs::metadata(&full_path)
        .and_then(|m| m.modified())
        .map(|t| t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs())
        .unwrap_or(0)
}

impl CodeGraph {
    /// Extract with per-repo cache. Cache key = repo_name + base_commit.
    /// If a cached graph exists on disk, returns it instantly.
    /// Otherwise extracts fresh and saves to cache.
    pub fn extract_cached(repo_dir: &Path, repo_name: &str, base_commit: &str) -> Self {
        let cache_dir = repo_dir.parent().unwrap_or(repo_dir).join(".graph-cache");
        let _ = std::fs::create_dir_all(&cache_dir);

        // Cache key: sanitized repo name + first 8 chars of commit
        let safe_repo = repo_name.replace('/', "__");
        let short_commit = &base_commit[..base_commit.len().min(8)];
        let cache_file = cache_dir.join(format!("{}__{}.json", safe_repo, short_commit));

        // Try to load from cache
        if cache_file.exists() {
            if let Ok(data) = std::fs::read_to_string(&cache_file) {
                if let Ok(mut graph) = serde_json::from_str::<CodeGraph>(&data) {
                    graph.build_indexes();
                    tracing::info!(
                        "Loaded code graph from cache: {} ({} nodes, {} edges)",
                        cache_file.display(),
                        graph.nodes.len(),
                        graph.edges.len()
                    );
                    return graph;
                }
            }
            // Cache corrupt, delete and re-extract
            let _ = std::fs::remove_file(&cache_file);
        }

        // Extract fresh
        let graph = Self::extract_from_dir(repo_dir);

        // Save to cache (best-effort, don't fail if write fails)
        if let Ok(json) = serde_json::to_string(&graph) {
            let _ = std::fs::write(&cache_file, json);
            tracing::info!(
                "Saved code graph to cache: {} ({} nodes, {} edges)",
                cache_file.display(),
                graph.nodes.len(),
                graph.edges.len()
            );
        }

        graph
    }

    /// Extract code graph from a directory.
    pub fn extract_from_dir(dir: &Path) -> Self {
        let mut state = ExtractState::default();

        // First pass: collect files and build module map
        let file_entries = collect_source_files(dir, &mut state.module_map);

        // Second pass: parse each file
        let mut parser = Parser::new();
        let python_language = tree_sitter_python::LANGUAGE;
        parser.set_language(&python_language.into()).ok();

        for (rel_path, content, lang) in &file_entries {
            if let Some(result) = parse_single_file(rel_path, content, lang, &mut parser, &mut state.class_map) {
                integrate_file_results(&mut state, rel_path, result);
            }
        }

        // Build helper maps for call extraction
        let (class_init_map, node_pkg_map) = build_call_extraction_maps(&state);

        // Third pass: extract call edges
        // Take edges out to avoid simultaneous immutable borrow of `state` + mutable borrow of `state.edges`
        let mut edges = std::mem::take(&mut state.edges);
        for (rel_path, content, lang) in &file_entries {
            extract_calls_for_file(
                rel_path, content, lang, &mut parser, &state,
                &class_init_map, &node_pkg_map, &state.module_map, &mut edges,
            );
        }
        state.edges = edges;

        // Resolve placeholder references
        let resolved = resolve_references(
            state.edges,
            &state.class_map,
            &state.func_map,
            &state.module_map,
        );

        // Deduplicate and finalize
        let mut final_edges = dedup_and_finalize_edges(resolved, &state.nodes);

        // Remove phantom file nodes — files that don't exist on disk (ISS-007)
        let valid_file_paths: HashSet<&str> = file_entries.iter().map(|(p, _, _)| p.as_str()).collect();
        remove_phantom_nodes(&mut state.nodes, &mut final_edges, &valid_file_paths);

        let mut graph = CodeGraph {
            nodes: state.nodes,
            edges: final_edges,
            outgoing: HashMap::new(),
            incoming: HashMap::new(),
            node_index: HashMap::new(),
        };
        graph.build_indexes();
        graph
    }

    /// Incremental extraction: only re-parse changed files.
    /// Falls back to full extraction if no prior metadata exists or if force=true.
    ///
    /// Returns the updated CodeGraph and an ExtractReport describing what changed.
    pub fn extract_incremental(
        dir: &Path,
        graph_path: &Path,
        meta_path: &Path,
        force: bool,
    ) -> anyhow::Result<(Self, ExtractReport)> {
        let start = Instant::now();

        // If force, do a full rebuild
        if force {
            tracing::info!("Force flag set, performing full rebuild");
            return Self::do_full_rebuild(dir, graph_path, meta_path, start);
        }

        // Try to load existing metadata
        let metadata = match Self::load_metadata(meta_path) {
            Some(meta) => {
                if meta.version != EXTRACT_META_VERSION {
                    tracing::info!(
                        "Metadata version mismatch (got {}, expected {}), performing full rebuild",
                        meta.version, EXTRACT_META_VERSION
                    );
                    return Self::do_full_rebuild(dir, graph_path, meta_path, start);
                }
                meta
            }
            None => {
                tracing::info!("No prior metadata found, performing full rebuild");
                return Self::do_full_rebuild(dir, graph_path, meta_path, start);
            }
        };

        // Try to load existing graph
        let existing_graph = match Self::load_graph_json(graph_path) {
            Some(g) => g,
            None => {
                tracing::info!("No prior graph found, performing full rebuild");
                return Self::do_full_rebuild(dir, graph_path, meta_path, start);
            }
        };

        // Collect current files
        let mut module_map: HashMap<String, String> = HashMap::new();
        let file_entries = collect_source_files(dir, &mut module_map);

        // Compute delta using content hash (mtime is checked first for speed)
        let delta = compute_file_delta_with_mtime(dir, &file_entries, &metadata);

        tracing::info!(
            "File delta: {} added, {} modified, {} deleted, {} unchanged",
            delta.added.len(), delta.modified.len(), delta.deleted.len(), delta.unchanged.len()
        );

        // If no changes, return existing graph
        if delta.is_empty() {
            let report = ExtractReport {
                added: 0,
                modified: 0,
                deleted: 0,
                unchanged: delta.unchanged.len(),
                full_rebuild: false,
                duration_ms: start.elapsed().as_millis() as u64,
            };
            return Ok((existing_graph, report));
        }

        // Phase 1: Remove stale data from deleted/modified files
        let changed_files: HashSet<&str> = delta.modified.iter()
            .chain(delta.deleted.iter())
            .map(|s| s.as_str())
            .collect();

        let mut graph = existing_graph;

        // Collect stale node IDs from deleted/modified files
        let mut stale_node_ids: HashSet<String> = HashSet::new();
        for file_path in &changed_files {
            if let Some(file_state) = metadata.files.get(*file_path) {
                for node_id in &file_state.node_ids {
                    stale_node_ids.insert(node_id.clone());
                }
            }
            // Also remove the file node itself
            stale_node_ids.insert(format!("file:{}", file_path));
        }

        // Remove stale nodes and their edges
        graph.nodes.retain(|n| !stale_node_ids.contains(&n.id));
        graph.edges.retain(|e| {
            !stale_node_ids.contains(&e.from) && !stale_node_ids.contains(&e.to)
        });

        // Dangling edge cleanup: remove edges pointing to non-existent nodes
        let valid_node_ids: HashSet<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
        graph.edges.retain(|e| {
            valid_node_ids.contains(e.from.as_str()) && valid_node_ids.contains(e.to.as_str())
        });

        tracing::debug!(
            "After stale removal: {} nodes, {} edges",
            graph.nodes.len(), graph.edges.len()
        );

        // Phase 2: Parse only added/modified files
        let files_to_parse: HashSet<&str> = delta.added.iter()
            .chain(delta.modified.iter())
            .map(|s| s.as_str())
            .collect();

        // Build state from existing graph nodes for reference resolution
        let mut state = ExtractState::default();
        state.module_map = module_map;

        // Populate maps from existing (unchanged) nodes
        for node in &graph.nodes {
            if node.kind == NodeKind::Class {
                state.class_map.insert(node.name.clone(), node.id.clone());
            } else if node.kind == NodeKind::Function {
                state.func_map
                    .entry(node.name.clone())
                    .or_default()
                    .push(node.id.clone());
            }
        }

        // Populate method_to_class and class_methods from existing edges
        for edge in &graph.edges {
            if edge.relation == EdgeRelation::DefinedIn {
                if edge.from.starts_with("method:") && edge.to.starts_with("class:") {
                    state.method_to_class.insert(edge.from.clone(), edge.to.clone());
                    state.class_methods
                        .entry(edge.to.clone())
                        .or_default()
                        .push(edge.from.clone());
                }
            }
            if edge.relation == EdgeRelation::Inherits {
                if let Some(parent_id) = state.class_map.get(
                    edge.to.strip_prefix("class_ref:").unwrap_or(&edge.to),
                ) {
                    state.class_parents
                        .entry(edge.from.clone())
                        .or_default()
                        .push(parent_id.clone());
                }
            }
        }

        // Parse changed files
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_python::LANGUAGE.into()).ok();

        // Track per-file node IDs for metadata
        let mut new_file_states: HashMap<String, FileState> = HashMap::new();

        for (rel_path, content, lang) in &file_entries {
            if !files_to_parse.contains(rel_path.as_str()) {
                continue;
            }

            if let Some(result) = parse_single_file(rel_path, content, lang, &mut parser, &mut state.class_map) {
                let node_ids: Vec<String> = result.nodes.iter().map(|n| n.id.clone()).collect();
                let node_ids_with_file = {
                    let mut ids = vec![format!("file:{}", rel_path)];
                    ids.extend(node_ids);
                    ids
                };

                integrate_file_results(&mut state, rel_path, result);

                // We'll compute edge_count after call extraction
                let mtime = get_file_mtime(dir, rel_path);
                let content_hash = xxh64(content.as_bytes(), 0);
                new_file_states.insert(rel_path.clone(), FileState {
                    mtime,
                    content_hash,
                    node_ids: node_ids_with_file,
                    edge_count: 0,
                });
            }
        }

        // Merge new nodes into graph
        graph.nodes.extend(state.nodes.drain(..));

        // Re-populate maps from ALL nodes (existing + new) for reference resolution
        state.class_map.clear();
        state.func_map.clear();
        state.method_to_class.clear();
        state.class_methods.clear();
        state.class_parents.clear();

        for node in &graph.nodes {
            if node.kind == NodeKind::Class {
                state.class_map.insert(node.name.clone(), node.id.clone());
            } else if node.kind == NodeKind::Function {
                state.func_map
                    .entry(node.name.clone())
                    .or_default()
                    .push(node.id.clone());
            }
        }

        // Rebuild method_to_class etc from all edges (existing + newly added file edges)
        let all_edges_for_maps: Vec<&CodeEdge> = graph.edges.iter()
            .chain(state.edges.iter())
            .collect();

        for edge in &all_edges_for_maps {
            if edge.relation == EdgeRelation::DefinedIn {
                if edge.from.starts_with("method:") && edge.to.starts_with("class:") {
                    state.method_to_class.insert(edge.from.clone(), edge.to.clone());
                    state.class_methods
                        .entry(edge.to.clone())
                        .or_default()
                        .push(edge.from.clone());
                }
            }
            if edge.relation == EdgeRelation::Inherits {
                if let Some(parent_id) = state.class_map.get(
                    edge.to.strip_prefix("class_ref:").unwrap_or(&edge.to),
                ) {
                    state.class_parents
                        .entry(edge.from.clone())
                        .or_default()
                        .push(parent_id.clone());
                }
            }
        }

        // Populate file_imported_names from both existing unchanged files and newly parsed
        // For unchanged files, we need to re-read their imports (they're not stored in metadata)
        // Actually, for the call extraction pass, we only extract calls for CHANGED files,
        // and those files' imports are already in state.file_imported_names
        // Unchanged files' existing call edges are already in the graph.

        // Build helper maps for call extraction
        // Note: We need nodes from BOTH the existing graph and new state
        // Temporarily set state.nodes to all graph nodes for building maps
        let saved_nodes = std::mem::take(&mut state.nodes);
        state.nodes = graph.nodes.clone();
        let (class_init_map, node_pkg_map) = build_call_extraction_maps(&state);
        state.nodes = saved_nodes;

        // Phase 2b: Extract call edges for changed files only
        let mut new_call_edges: Vec<CodeEdge> = Vec::new();
        for (rel_path, content, lang) in &file_entries {
            if !files_to_parse.contains(rel_path.as_str()) {
                continue;
            }
            extract_calls_for_file(
                rel_path, content, lang, &mut parser, &state,
                &class_init_map, &node_pkg_map, &state.module_map,
                &mut new_call_edges,
            );
        }

        // Count edges per file for metadata
        for edge in &new_call_edges {
            // Determine which file this edge belongs to by looking at the source node's file
            let source_file = graph.nodes.iter()
                .find(|n| n.id == edge.from)
                .map(|n| n.file_path.clone());
            if let Some(fp) = source_file {
                if let Some(fs) = new_file_states.get_mut(&fp) {
                    fs.edge_count += 1;
                }
            }
        }

        // Phase 3: Merge new edges and resolve references
        let mut all_new_edges = state.edges;
        all_new_edges.extend(new_call_edges);

        let resolved_new = resolve_references(
            all_new_edges,
            &state.class_map,
            &state.func_map,
            &state.module_map,
        );

        // Add resolved new edges to existing graph edges
        graph.edges.extend(resolved_new);

        // Deduplicate and finalize ALL edges
        let final_edges = dedup_and_finalize_edges(graph.edges, &graph.nodes);
        graph.edges = final_edges;

        // Remove phantom file nodes — files that don't exist on disk (ISS-007)
        let valid_file_paths: HashSet<&str> = file_entries.iter().map(|(p, _, _)| p.as_str()).collect();
        remove_phantom_nodes(&mut graph.nodes, &mut graph.edges, &valid_file_paths);

        // Rebuild indexes
        graph.outgoing.clear();
        graph.incoming.clear();
        graph.node_index.clear();
        graph.build_indexes();

        // Phase 5: Save graph + update metadata
        Self::save_graph_json(graph_path, &graph);

        // Build updated metadata
        let mut new_metadata = ExtractMetadata {
            version: EXTRACT_META_VERSION,
            updated_at: chrono::Utc::now().to_rfc3339(),
            files: HashMap::new(),
        };

        // Copy unchanged file states from old metadata
        for path in &delta.unchanged {
            if let Some(old_state) = metadata.files.get(path) {
                new_metadata.files.insert(path.clone(), old_state.clone());
            }
        }

        // Add new/modified file states
        for (path, file_state) in new_file_states {
            new_metadata.files.insert(path, file_state);
        }

        // Save metadata
        Self::save_metadata(meta_path, &new_metadata);

        let report = ExtractReport {
            added: delta.added.len(),
            modified: delta.modified.len(),
            deleted: delta.deleted.len(),
            unchanged: delta.unchanged.len(),
            full_rebuild: false,
            duration_ms: start.elapsed().as_millis() as u64,
        };

        tracing::info!("{}", report);

        Ok((graph, report))
    }

    /// Full rebuild with metadata generation.
    fn do_full_rebuild(
        dir: &Path,
        graph_path: &Path,
        meta_path: &Path,
        start: Instant,
    ) -> anyhow::Result<(Self, ExtractReport)> {
        let mut state = ExtractState::default();

        // First pass: collect files and build module map
        let file_entries = collect_source_files(dir, &mut state.module_map);
        let total_files = file_entries.len();

        // Second pass: parse each file
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_python::LANGUAGE.into()).ok();

        let mut per_file_node_ids: HashMap<String, Vec<String>> = HashMap::new();

        for (rel_path, content, lang) in &file_entries {
            if let Some(result) = parse_single_file(rel_path, content, lang, &mut parser, &mut state.class_map) {
                let mut node_ids: Vec<String> = result.nodes.iter().map(|n| n.id.clone()).collect();
                if !result.nodes.is_empty() {
                    node_ids.insert(0, format!("file:{}", rel_path));
                }
                per_file_node_ids.insert(rel_path.clone(), node_ids);
                integrate_file_results(&mut state, rel_path, result);
            }
        }

        // Build helper maps for call extraction
        let (class_init_map, node_pkg_map) = build_call_extraction_maps(&state);

        // Third pass: extract call edges
        // Take edges out to avoid simultaneous immutable borrow of `state` + mutable borrow of `state.edges`
        let mut edges = std::mem::take(&mut state.edges);
        for (rel_path, content, lang) in &file_entries {
            extract_calls_for_file(
                rel_path, content, lang, &mut parser, &state,
                &class_init_map, &node_pkg_map, &state.module_map, &mut edges,
            );
        }
        state.edges = edges;

        // Resolve, dedup, finalize
        let resolved = resolve_references(
            state.edges,
            &state.class_map,
            &state.func_map,
            &state.module_map,
        );
        let mut final_edges = dedup_and_finalize_edges(resolved, &state.nodes);

        // Remove phantom file nodes — files that don't exist on disk (ISS-007)
        let valid_file_paths: HashSet<&str> = file_entries.iter().map(|(p, _, _)| p.as_str()).collect();
        remove_phantom_nodes(&mut state.nodes, &mut final_edges, &valid_file_paths);

        let mut graph = CodeGraph {
            nodes: state.nodes,
            edges: final_edges,
            outgoing: HashMap::new(),
            incoming: HashMap::new(),
            node_index: HashMap::new(),
        };
        graph.build_indexes();

        // Save graph
        Self::save_graph_json(graph_path, &graph);

        // Build and save metadata
        let mut metadata = ExtractMetadata {
            version: EXTRACT_META_VERSION,
            updated_at: chrono::Utc::now().to_rfc3339(),
            files: HashMap::new(),
        };

        for (rel_path, content, _lang) in &file_entries {
            let mtime = get_file_mtime(dir, rel_path);
            let content_hash = xxh64(content.as_bytes(), 0);
            let node_ids = per_file_node_ids.get(rel_path).cloned().unwrap_or_default();

            // Count edges originating from nodes in this file
            let file_node_ids: HashSet<&str> = node_ids.iter().map(|s| s.as_str()).collect();
            let edge_count = graph.edges.iter()
                .filter(|e| file_node_ids.contains(e.from.as_str()))
                .count();

            metadata.files.insert(rel_path.clone(), FileState {
                mtime,
                content_hash,
                node_ids,
                edge_count,
            });
        }

        Self::save_metadata(meta_path, &metadata);

        let report = ExtractReport {
            added: total_files,
            modified: 0,
            deleted: 0,
            unchanged: 0,
            full_rebuild: true,
            duration_ms: start.elapsed().as_millis() as u64,
        };

        tracing::info!("{}", report);

        Ok((graph, report))
    }

    /// Load extract metadata from disk.
    fn load_metadata(meta_path: &Path) -> Option<ExtractMetadata> {
        let data = std::fs::read_to_string(meta_path).ok()?;
        serde_json::from_str(&data).ok()
    }

    /// Save extract metadata to disk.
    fn save_metadata(meta_path: &Path, metadata: &ExtractMetadata) {
        if let Some(parent) = meta_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(metadata) {
            if let Err(e) = std::fs::write(meta_path, json) {
                tracing::warn!("Failed to save extract metadata: {}", e);
            }
        }
    }

    /// Load a graph from JSON format.
    fn load_graph_json(graph_path: &Path) -> Option<Self> {
        let data = std::fs::read_to_string(graph_path).ok()?;
        let mut graph: Self = serde_json::from_str(&data).ok()?;
        graph.build_indexes();
        Some(graph)
    }

    /// Save graph as JSON.
    fn save_graph_json(graph_path: &Path, graph: &Self) {
        if let Some(parent) = graph_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(graph) {
            if let Err(e) = std::fs::write(graph_path, json) {
                tracing::warn!("Failed to save graph: {}", e);
            }
        }
    }
}

/// Compute file delta with mtime-first, hash-second strategy.
fn compute_file_delta_with_mtime(
    dir: &Path,
    current_files: &[(String, String, Language)],
    metadata: &ExtractMetadata,
) -> FileDelta {
    let mut delta = FileDelta::default();

    let current_paths: HashSet<&str> = current_files.iter().map(|(p, _, _)| p.as_str()).collect();

    for (rel_path, content, _lang) in current_files {
        if let Some(stored) = metadata.files.get(rel_path.as_str()) {
            // File exists in both — check if changed
            // Quick check: mtime
            let mtime = get_file_mtime(dir, rel_path);
            if mtime == stored.mtime {
                delta.unchanged.push(rel_path.clone());
            } else {
                // mtime changed — verify with content hash
                let content_hash = xxh64(content.as_bytes(), 0);
                if content_hash == stored.content_hash {
                    // Content same despite mtime change (e.g. touch)
                    delta.unchanged.push(rel_path.clone());
                } else {
                    delta.modified.push(rel_path.clone());
                }
            }
        } else {
            // New file
            delta.added.push(rel_path.clone());
        }
    }

    // Find deleted files
    for stored_path in metadata.files.keys() {
        if !current_paths.contains(stored_path.as_str()) {
            delta.deleted.push(stored_path.clone());
        }
    }

    delta
}
