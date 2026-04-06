//! Code Graph Extraction — extract code dependencies from source files
//!
//! Multi-language support with tree-sitter AST parsing for Python, Rust, and TypeScript.
//! Builds a code structure graph:
//! - Nodes: files, classes/structs/traits, functions/methods
//! - Edges: imports, calls, inherits, defined_in
//!
//! Rust extraction handles: structs, enums, traits, impl blocks (with method-type association),
//! functions, modules, type aliases, const/static items, and macros.
//!
//! TypeScript/JavaScript extraction handles: classes, interfaces, functions, arrow functions,
//! enums, type aliases, namespaces, and export statements.

use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::path::Path;
use serde::{Deserialize, Serialize};
use regex::Regex;
use walkdir::WalkDir;
use tree_sitter::Parser;

// ═══ Graph Types ═══

/// A code dependency graph extracted from source files.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodeGraph {
    pub nodes: Vec<CodeNode>,
    pub edges: Vec<CodeEdge>,
    /// Adjacency list: node_id → indices into self.edges (outgoing)
    #[serde(skip)]
    pub outgoing: HashMap<String, Vec<usize>>,
    /// Reverse adjacency list: node_id → indices into self.edges (incoming)
    #[serde(skip)]
    pub incoming: HashMap<String, Vec<usize>>,
    /// Node lookup: node_id → index into self.nodes
    #[serde(skip)]
    pub node_index: HashMap<String, usize>,
}

/// A node in the code graph (file, class, function).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeNode {
    pub id: String,
    pub kind: NodeKind,
    pub name: String,
    pub file_path: String,
    pub line: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decorators: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    #[serde(default)]
    pub line_count: usize,
    #[serde(default)]
    pub is_test: bool,
}

impl CodeNode {
    pub fn new_file(path: &str) -> Self {
        Self {
            id: format!("file:{}", path),
            kind: NodeKind::File,
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            file_path: path.to_string(),
            line: None,
            decorators: Vec::new(),
            signature: None,
            docstring: None,
            line_count: 0,
            is_test: path.contains("/test") || path.contains("_test."),
        }
    }

    pub fn new_class(path: &str, name: &str, line: usize) -> Self {
        Self {
            id: format!("class:{}:{}", path, name),
            kind: NodeKind::Class,
            name: name.to_string(),
            file_path: path.to_string(),
            line: Some(line),
            decorators: Vec::new(),
            signature: None,
            docstring: None,
            line_count: 0,
            is_test: name.starts_with("Test") || path.contains("/test"),
        }
    }

    pub fn new_function(path: &str, name: &str, line: usize, is_method: bool) -> Self {
        let prefix = if is_method { "method" } else { "func" };
        Self {
            id: format!("{}:{}:{}", prefix, path, name),
            kind: NodeKind::Function,
            name: name.to_string(),
            file_path: path.to_string(),
            line: Some(line),
            decorators: Vec::new(),
            signature: None,
            docstring: None,
            line_count: 0,
            is_test: name.starts_with("test_") || name.starts_with("Test") || path.contains("/test"),
        }
    }
}

/// Kind of code node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    File,
    Class,
    Function,
    Module,
}

/// An edge in the code graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeEdge {
    pub from: String,
    pub to: String,
    pub relation: EdgeRelation,
    #[serde(default)]
    pub weight: f32,
    #[serde(default)]
    pub call_count: u32,
    #[serde(default)]
    pub in_error_path: bool,
    #[serde(default)]
    pub confidence: f32,
    /// 0-indexed line of the call site expression (for LSP refinement)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_site_line: Option<u32>,
    /// 0-indexed column of the call site expression (for LSP refinement)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_site_column: Option<u32>,
}

impl CodeEdge {
    pub fn new(from: &str, to: &str, relation: EdgeRelation) -> Self {
        Self {
            from: from.to_string(),
            to: to.to_string(),
            relation,
            weight: 0.5,
            call_count: 1,
            in_error_path: false,
            confidence: 1.0,
            call_site_line: None,
            call_site_column: None,
        }
    }

    pub fn imports(from: &str, to: &str) -> Self {
        Self::new(from, to, EdgeRelation::Imports)
    }

    pub fn calls(from: &str, to: &str) -> Self {
        Self::new(from, to, EdgeRelation::Calls)
    }

    pub fn inherits(from: &str, to: &str) -> Self {
        Self::new(from, to, EdgeRelation::Inherits)
    }

    pub fn defined_in(from: &str, to: &str) -> Self {
        Self::new(from, to, EdgeRelation::DefinedIn)
    }

    /// Compute composite weight from call_count, in_error_path, and confidence.
    pub fn compute_weight(&mut self) {
        if self.relation == EdgeRelation::Calls {
            let count_norm = (self.call_count as f32 / 10.0).min(1.0);
            let error_factor = if self.in_error_path { 0.8 } else { 0.5 };
            self.weight = 0.4 * count_norm + 0.3 * error_factor + 0.3 * self.confidence;
        } else {
            self.weight = 0.7; // Default for non-call edges
        }
    }
}

/// Edge relationship type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeRelation {
    /// File imports module
    Imports,
    /// Class inherits from parent
    Inherits,
    /// Entity is defined in file/class
    DefinedIn,
    /// Function calls another function
    Calls,
    /// Test file tests source file
    TestsFor,
    /// Method overrides parent method
    Overrides,
    /// Concrete method implements a trait/interface method
    Implements,
}

impl std::fmt::Display for EdgeRelation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EdgeRelation::Imports => write!(f, "imports"),
            EdgeRelation::Inherits => write!(f, "inherits"),
            EdgeRelation::DefinedIn => write!(f, "defined_in"),
            EdgeRelation::Calls => write!(f, "calls"),
            EdgeRelation::TestsFor => write!(f, "tests_for"),
            EdgeRelation::Overrides => write!(f, "overrides"),
            EdgeRelation::Implements => write!(f, "implements"),
        }
    }
}

// ═══ Impact Analysis Types ═══

/// Result of impact analysis — what's affected by a change
#[derive(Debug)]
pub struct ImpactReport<'a> {
    pub affected_source: Vec<&'a CodeNode>,
    pub affected_tests: Vec<&'a CodeNode>,
}

/// A causal chain from symptom to potential root cause
#[derive(Debug, Clone)]
pub struct CausalChain {
    pub symptom_node_id: String,
    pub chain: Vec<ChainNode>,
}

#[derive(Debug, Clone)]
pub struct ChainNode {
    pub node_id: String,
    pub node_name: String,
    pub file_path: String,
    pub line: Option<usize>,
    pub edge_to_next: Option<String>,
}

// ═══ Language Detection ═══

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    TypeScript,
    Python,
    Unknown,
}

impl Language {
    pub fn from_path(path: &Path) -> Self {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        match ext {
            "rs" => Language::Rust,
            "ts" | "tsx" => Language::TypeScript,
            "js" | "jsx" => Language::TypeScript, // JS uses same patterns
            "py" => Language::Python,
            _ => Language::Unknown,
        }
    }
}

// ═══ Extraction ═══

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
        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        // Maps for resolving references
        let mut class_map: HashMap<String, String> = HashMap::new();
        let mut func_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut module_map: HashMap<String, String> = HashMap::new();

        // Method to class mapping for scoped self.method() resolution
        let mut method_to_class: HashMap<String, String> = HashMap::new();
        let mut class_methods: HashMap<String, Vec<String>> = HashMap::new();

        // Class inheritance map for parent method resolution
        let mut class_parents: HashMap<String, Vec<String>> = HashMap::new();

        // File → imported function/module names
        let mut file_imported_names: HashMap<String, HashSet<String>> = HashMap::new();

        // Struct name → { field_name → type_name } for receiver type heuristics
        let mut all_struct_field_types: HashMap<String, HashMap<String, String>> = HashMap::new();

        // Collect file entries first
        let mut file_entries: Vec<(String, String, Language)> = Vec::new();

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
            module_map.insert(module_path.clone(), file_id.clone());

            // Also map partial paths
            let parts: Vec<&str> = module_path.split('.').collect();
            for start in 1..parts.len() {
                let partial = parts[start..].join(".");
                module_map.entry(partial).or_insert_with(|| file_id.clone());
            }

            file_entries.push((rel_path, content, lang));
        }

        // Second pass: parse each file
        let mut parser = Parser::new();
        let python_language = tree_sitter_python::LANGUAGE;
        parser.set_language(&python_language.into()).ok();

        for (rel_path, content, lang) in &file_entries {
            let _file_id = format!("file:{}", rel_path);

            let (file_nodes, file_edges, imports) = match lang {
                Language::Python => {
                    extract_python_tree_sitter(
                        rel_path,
                        content,
                        &mut parser,
                        &mut class_map,
                    )
                }
                Language::Rust => {
                    let (nodes, edges, imports, field_types) = extract_rust_tree_sitter(
                        rel_path,
                        content,
                        &mut parser,
                        &mut class_map,
                    );
                    // Store struct field types for receiver type heuristics
                    for (struct_name, fields) in field_types {
                        all_struct_field_types.insert(struct_name, fields);
                    }
                    (nodes, edges, imports)
                }
                Language::TypeScript => {
                    let ext = rel_path.rsplit('.').next().unwrap_or("ts");
                    extract_typescript_tree_sitter(
                        rel_path,
                        content,
                        &mut parser,
                        &mut class_map,
                        ext,
                    )
                }
                Language::Unknown => continue,
            };

            // Update maps
            for node in &file_nodes {
                if node.kind == NodeKind::Class {
                    class_map.insert(node.name.clone(), node.id.clone());
                } else if node.kind == NodeKind::Function {
                    func_map
                        .entry(node.name.clone())
                        .or_default()
                        .push(node.id.clone());
                }
            }

            // Track method→class and class→methods relationships
            for edge in &file_edges {
                if edge.relation == EdgeRelation::DefinedIn {
                    if edge.from.starts_with("method:") && edge.to.starts_with("class:") {
                        method_to_class.insert(edge.from.clone(), edge.to.clone());
                        class_methods
                            .entry(edge.to.clone())
                            .or_default()
                            .push(edge.from.clone());
                    }
                }
                if edge.relation == EdgeRelation::Inherits {
                    if let Some(parent_id) = class_map.get(
                        edge.to.strip_prefix("class_ref:").unwrap_or(&edge.to),
                    ) {
                        class_parents
                            .entry(edge.from.clone())
                            .or_default()
                            .push(parent_id.clone());
                    }
                }
            }

            // Store imported names
            if !imports.is_empty() {
                file_imported_names.insert(rel_path.clone(), imports);
            }

            // Add file node if we found entities
            if !file_nodes.is_empty() {
                nodes.push(CodeNode::new_file(rel_path));
            }

            nodes.extend(file_nodes);
            edges.extend(file_edges);
        }

        // Build class_init_map for constructor resolution
        let class_init_map: HashMap<String, Vec<(String, String)>> = {
            let mut map: HashMap<String, Vec<(String, String)>> = HashMap::new();
            for node in &nodes {
                if node.kind == NodeKind::Function && node.name == "__init__" && !node.is_test {
                    if let Some(class_id) = method_to_class.get(&node.id) {
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

        // Build node_pkg_map for package-scoped resolution
        let node_pkg_map: HashMap<String, String> = nodes
            .iter()
            .map(|n| {
                let pkg = n.file_path.rsplitn(2, '/').nth(1).unwrap_or("").to_string();
                (n.id.clone(), pkg)
            })
            .collect();

        // Third pass: extract call edges for all languages with tree-sitter
        for (rel_path, content, lang) in &file_entries {
            let file_func_ids: HashSet<String> = nodes
                .iter()
                .filter(|n| n.file_path == *rel_path && n.kind == NodeKind::Function)
                .map(|n| n.id.clone())
                .collect();

            let package_dir = rel_path.rsplitn(2, '/').nth(1).unwrap_or("");

            match lang {
                Language::Python => {
                    // Set Python language for parser
                    if parser.set_language(&tree_sitter_python::LANGUAGE.into()).is_err() {
                        continue;
                    }
                    
                    if let Some(tree) = parser.parse(content, None) {
                        let source = content.as_bytes();
                        let root = tree.root_node();

                        extract_calls_from_tree(
                            root,
                            source,
                            rel_path,
                            &func_map,
                            &method_to_class,
                            &class_parents,
                            &file_func_ids,
                            &file_imported_names,
                            package_dir,
                            &class_init_map,
                            &node_pkg_map,
                            &mut edges,
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
                    // Set Rust language for parser
                    if parser.set_language(&tree_sitter_rust::LANGUAGE.into()).is_err() {
                        continue;
                    }

                    if let Some(tree) = parser.parse(content, None) {
                        let source = content.as_bytes();
                        let root = tree.root_node();

                        extract_calls_rust(
                            root,
                            source,
                            rel_path,
                            &func_map,
                            &method_to_class,
                            &file_func_ids,
                            &node_pkg_map,
                            &file_imported_names,
                            &all_struct_field_types,
                            &mut edges,
                        );
                    }
                }
                Language::TypeScript => {
                    // Set TypeScript language for parser based on extension
                    let extension = rel_path.rsplit('.').next().unwrap_or("");
                    let lang_result = match extension {
                        "tsx" => parser.set_language(&tree_sitter_typescript::LANGUAGE_TSX.into()),
                        "ts" => parser.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
                        "jsx" => parser.set_language(&tree_sitter_javascript::LANGUAGE.into()),
                        _ => parser.set_language(&tree_sitter_javascript::LANGUAGE.into()),
                    };
                    
                    if lang_result.is_err() {
                        continue;
                    }

                    if let Some(tree) = parser.parse(content, None) {
                        let source = content.as_bytes();
                        let root = tree.root_node();

                        extract_calls_typescript(
                            root,
                            source,
                            rel_path,
                            &func_map,
                            &method_to_class,
                            &file_func_ids,
                            &file_imported_names,
                            &node_pkg_map,
                            &mut edges,
                        );
                    }
                }
                Language::Unknown => {
                    // No call extraction for unknown languages
                }
            }
        }

        // Resolve placeholder references
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
                        call_site_line: None,
                        call_site_column: None,
                    });
                }
            } else if edge.to.starts_with("module_ref:") {
                let module = &edge.to["module_ref:".len()..];
                // Try direct lookup first
                let resolved_file_id = module_map.get(module).cloned()
                    // If direct lookup fails, try resolving as TypeScript relative import
                    .or_else(|| {
                        // edge.from is like "file:src/pages/Dashboard.tsx"
                        // Extract the path after "file:"
                        let importing_file = edge.from.strip_prefix("file:").unwrap_or(&edge.from);
                        resolve_ts_import(importing_file, module, &module_map)
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
                        call_site_line: None,
                        call_site_column: None,
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
                            call_site_line: None,
                            call_site_column: None,
                        });
                    }
                }
            } else {
                resolved_edges.push(edge);
            }
        }

        // Deduplicate call edges and compute call_count
        let mut edge_map: HashMap<(String, String), CodeEdge> = HashMap::new();
        let mut other_edges: Vec<CodeEdge> = Vec::new();

        for edge in resolved_edges {
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
        add_override_edges(&nodes, &mut final_edges);

        let mut graph = CodeGraph {
            nodes,
            edges: final_edges,
            outgoing: HashMap::new(),
            incoming: HashMap::new(),
            node_index: HashMap::new(),
        };
        graph.build_indexes();
        graph
    }

    /// Build adjacency indexes for O(1) lookups.
    pub fn build_indexes(&mut self) {
        self.node_index.clear();
        self.outgoing.clear();
        self.incoming.clear();

        for (i, node) in self.nodes.iter().enumerate() {
            self.node_index.insert(node.id.clone(), i);
        }

        for (i, edge) in self.edges.iter().enumerate() {
            self.outgoing.entry(edge.from.clone()).or_default().push(i);
            self.incoming.entry(edge.to.clone()).or_default().push(i);
        }
    }

    // ═══ Query Methods ═══

    /// Get outgoing edges from a node.
    #[inline]
    pub fn outgoing_edges(&self, node_id: &str) -> impl Iterator<Item = &CodeEdge> {
        self.outgoing
            .get(node_id)
            .map(|indices| indices.as_slice())
            .unwrap_or(&[])
            .iter()
            .map(move |&i| &self.edges[i])
    }

    /// Get incoming edges to a node.
    #[inline]
    pub fn incoming_edges(&self, node_id: &str) -> impl Iterator<Item = &CodeEdge> {
        self.incoming
            .get(node_id)
            .map(|indices| indices.as_slice())
            .unwrap_or(&[])
            .iter()
            .map(move |&i| &self.edges[i])
    }

    /// Find node by id.
    #[inline]
    pub fn node_by_id(&self, node_id: &str) -> Option<&CodeNode> {
        self.node_index.get(node_id).map(|&i| &self.nodes[i])
    }

    /// Get all callers of a function/method.
    pub fn get_callers(&self, node_id: &str) -> Vec<&CodeNode> {
        self.incoming_edges(node_id)
            .filter(|e| e.relation == EdgeRelation::Calls)
            .filter_map(|e| self.node_by_id(&e.from))
            .collect()
    }

    /// Get all callees of a function/method.
    pub fn get_callees(&self, node_id: &str) -> Vec<&CodeNode> {
        self.outgoing_edges(node_id)
            .filter(|e| e.relation == EdgeRelation::Calls)
            .filter_map(|e| self.node_by_id(&e.to))
            .collect()
    }

    /// Get dependencies of a node (what it depends on)
    pub fn get_dependencies(&self, node_id: &str) -> Vec<&CodeNode> {
        self.outgoing_edges(node_id)
            .filter_map(|e| self.node_by_id(&e.to))
            .collect()
    }

    /// Get nodes that depend on this node (impact analysis).
    pub fn get_impact(&self, node_id: &str) -> Vec<&CodeNode> {
        let mut impacted = Vec::new();
        let mut visited = HashSet::new();
        self.collect_dependents(node_id, &mut impacted, &mut visited);
        impacted
    }

    fn collect_dependents<'a>(
        &'a self,
        node_id: &str,
        result: &mut Vec<&'a CodeNode>,
        visited: &mut HashSet<String>,
    ) {
        if !visited.insert(node_id.to_string()) {
            return;
        }

        for edge in self.incoming_edges(node_id) {
            if let Some(node) = self.node_by_id(&edge.from) {
                result.push(node);
                self.collect_dependents(&edge.from, result, visited);
            }
        }
    }

    /// Find nodes matching keywords in name or path.
    pub fn find_relevant_nodes(&self, keywords: &[&str]) -> Vec<&CodeNode> {
        let mut scored: Vec<(usize, &CodeNode)> = self
            .nodes
            .iter()
            .map(|n| {
                let score: usize = keywords
                    .iter()
                    .filter(|kw| {
                        let kw_lower = kw.to_lowercase();
                        let name_lower = n.name.to_lowercase();
                        let path_lower = n.file_path.to_lowercase();
                        name_lower.contains(&kw_lower)
                            || path_lower.contains(&kw_lower)
                            || (name_lower.len() >= 5
                                && kw_lower.contains(name_lower.trim_start_matches('_')))
                    })
                    .count();
                (score, n)
            })
            .filter(|(score, _)| *score > 0)
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0));
        let mut results: Vec<&CodeNode> = scored.into_iter().map(|(_, n)| n).collect();

        // Same-file expansion
        let relevant_files: HashSet<String> = results.iter().map(|n| n.file_path.clone()).collect();

        for node in &self.nodes {
            if relevant_files.contains(&node.file_path) && !results.iter().any(|r| r.id == node.id) {
                results.push(node);
            }
        }

        // Inheritance chain expansion
        let mut inheritance_additions: Vec<&CodeNode> = Vec::new();
        let result_ids: HashSet<String> = results.iter().map(|n| n.id.clone()).collect();

        for node in &results {
            if node.kind == NodeKind::Class {
                let chain = self.get_inheritance_chain(&node.id);
                for ancestor_id in &chain {
                    if !result_ids.contains(ancestor_id) {
                        if let Some(ancestor) = self.node_by_id(ancestor_id) {
                            inheritance_additions.push(ancestor);
                        }
                    }
                }
                for edge in self.incoming_edges(&node.id) {
                    if edge.relation == EdgeRelation::Inherits && !result_ids.contains(&edge.from) {
                        if let Some(child) = self.node_by_id(&edge.from) {
                            inheritance_additions.push(child);
                        }
                    }
                }
            }
        }

        let mut extra_files: HashSet<String> = HashSet::new();
        for node in &inheritance_additions {
            if !results.iter().any(|r| r.id == node.id) {
                extra_files.insert(node.file_path.clone());
                results.push(node);
            }
        }
        for node in &self.nodes {
            if extra_files.contains(&node.file_path) && !results.iter().any(|r| r.id == node.id) {
                results.push(node);
            }
        }

        // Import chain expansion: for relevant files, follow their imports to find related files
        // Only expand one level deep to avoid pulling in the entire codebase
        let mut import_additions: Vec<&CodeNode> = Vec::new();
        let current_ids: HashSet<String> = results.iter().map(|n| n.id.clone()).collect();

        for node in &results {
            if node.kind == NodeKind::File {
                for edge in self.outgoing_edges(&node.id) {
                    if edge.relation == EdgeRelation::Imports {
                        if !current_ids.contains(&edge.to) {
                            if let Some(imported) = self.node_by_id(&edge.to) {
                                import_additions.push(imported);
                            }
                        }
                    }
                }
            }
        }

        // Only add import expansions for files that have classes/functions matching keywords
        for node in &import_additions {
            if node.kind == NodeKind::File {
                let has_keyword_match = self
                    .nodes
                    .iter()
                    .filter(|n| n.file_path == node.file_path && n.kind != NodeKind::File)
                    .any(|n| {
                        let name_lower = n.name.to_lowercase();
                        keywords.iter().any(|kw| {
                            let kw_lower = kw.to_lowercase();
                            name_lower.contains(&kw_lower) || kw_lower.contains(&name_lower)
                        })
                    });
                if has_keyword_match && !results.iter().any(|r| r.id == node.id) {
                    results.push(node);
                    // Also add entities from that file
                    for entity in &self.nodes {
                        if entity.file_path == node.file_path
                            && !results.iter().any(|r| r.id == entity.id)
                        {
                            results.push(entity);
                        }
                    }
                }
            }
        }

        results
    }

    /// Full impact analysis: given nodes to change, return affected nodes + tests
    pub fn impact_analysis(&self, changed_node_ids: &[&str]) -> ImpactReport<'_> {
        let mut affected_nodes = Vec::new();
        let mut affected_tests = Vec::new();
        let mut seen = HashSet::new();

        for node_id in changed_node_ids {
            let impacted = self.get_impact(node_id);
            for node in impacted {
                if seen.insert(node.id.clone()) {
                    if node.file_path.contains("/tests/") || node.file_path.contains("/test_") {
                        affected_tests.push(node);
                    } else {
                        affected_nodes.push(node);
                    }
                }
            }
        }

        let related_tests = self.find_related_tests(changed_node_ids);
        for test in related_tests {
            if seen.insert(test.id.clone()) {
                affected_tests.push(test);
            }
        }

        ImpactReport {
            affected_source: affected_nodes,
            affected_tests,
        }
    }

    /// Find test files/functions related to given source nodes.
    pub fn find_related_tests(&self, source_node_ids: &[&str]) -> Vec<&CodeNode> {
        let mut test_nodes = Vec::new();
        let mut seen = HashSet::new();

        let source_files: HashSet<String> = source_node_ids
            .iter()
            .filter_map(|id| self.node_by_id(id))
            .map(|n| n.file_path.clone())
            .collect();

        let source_file_ids: HashSet<String> = source_files.iter().map(|f| format!("file:{}", f)).collect();

        // Find tests via TestsFor edges
        for source_fid in &source_file_ids {
            for edge in self.incoming_edges(source_fid.as_str()) {
                if edge.relation == EdgeRelation::TestsFor {
                    if let Some(test_node) = self.node_by_id(&edge.from) {
                        if seen.insert(test_node.id.clone()) {
                            test_nodes.push(test_node);
                        }
                        for node in &self.nodes {
                            if node.file_path == test_node.file_path
                                && node.kind != NodeKind::File
                                && seen.insert(node.id.clone())
                            {
                                test_nodes.push(node);
                            }
                        }
                    }
                }
            }
        }

        // Find tests via Calls edges
        for source_id in source_node_ids.iter() {
            for edge in self.incoming_edges(source_id) {
                if edge.relation == EdgeRelation::Calls {
                    if let Some(caller) = self.node_by_id(&edge.from) {
                        if caller.file_path.contains("/tests/") || caller.file_path.contains("/test_") {
                            if seen.insert(caller.id.clone()) {
                                test_nodes.push(caller);
                            }
                        }
                    }
                }
            }
        }

        test_nodes
    }

    /// Format impact analysis as context string for LLM
    pub fn format_impact_for_llm(&self, changed_node_ids: &[&str], repo_dir: &Path) -> String {
        let report = self.impact_analysis(changed_node_ids);
        let mut result = String::new();

        if !report.affected_source.is_empty() {
            result.push_str("**⚠️ Impact Analysis — Code affected by your change:**\n");
            for node in &report.affected_source {
                let prefix = match node.kind {
                    NodeKind::File => "📄",
                    NodeKind::Class => "🔷",
                    NodeKind::Function => "🔹",
                    NodeKind::Module => "📦",
                };
                result.push_str(&format!("{} {} (`{}`)\n", prefix, node.name, node.file_path));
            }
            result.push('\n');
        }

        if !report.affected_tests.is_empty() {
            result.push_str("**🧪 Tests that exercise the code you're changing:**\n");
            result.push_str("DO NOT break these tests! Make minimal changes.\n\n");

            let mut test_files: HashSet<String> = HashSet::new();
            for node in &report.affected_tests {
                test_files.insert(node.file_path.clone());
            }

            for test_file in &test_files {
                result.push_str(&format!("📋 `{}`\n", test_file));
                let funcs: Vec<&str> = report
                    .affected_tests
                    .iter()
                    .filter(|n| n.file_path == *test_file && n.kind == NodeKind::Function)
                    .map(|n| n.name.as_str())
                    .collect();
                if !funcs.is_empty() {
                    for func in funcs.iter().take(10) {
                        result.push_str(&format!("  - {}\n", func));
                    }
                    if funcs.len() > 10 {
                        result.push_str(&format!("  ... and {} more\n", funcs.len() - 10));
                    }
                }
            }
            result.push('\n');

            let test_nodes_refs: Vec<&CodeNode> = report
                .affected_tests
                .iter()
                .filter(|n| n.kind == NodeKind::Function)
                .take(10)
                .copied()
                .collect();

            if !test_nodes_refs.is_empty() {
                let test_snippets = self.extract_snippets(&test_nodes_refs, repo_dir, 30);
                if !test_snippets.is_empty() {
                    result.push_str("**Key test code (DO NOT break these):**\n```python\n");
                    for (node_id, snippet) in test_snippets.iter().take(5) {
                        let name = self.node_name(node_id);
                        result.push_str(&format!("# --- {} ---\n{}\n\n", name, snippet));
                    }
                    result.push_str("```\n");
                }
            }
        }

        result
    }

    /// Trace causal chains from symptom nodes to potential root causes.
    pub fn trace_causal_chains_from_symptoms(
        &self,
        symptom_node_ids: &[&str],
        max_depth: usize,
        max_chains: usize,
    ) -> Vec<CausalChain> {
        #[derive(Clone)]
        struct WeightedPath {
            node_id: String,
            accumulated_weight: f32,
            chain: Vec<ChainNode>,
        }

        impl PartialEq for WeightedPath {
            fn eq(&self, other: &Self) -> bool {
                self.accumulated_weight
                    .total_cmp(&other.accumulated_weight)
                    == std::cmp::Ordering::Equal
            }
        }
        impl Eq for WeightedPath {}
        impl PartialOrd for WeightedPath {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }
        impl Ord for WeightedPath {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                self.accumulated_weight.total_cmp(&other.accumulated_weight)
            }
        }

        let mut all_chains: Vec<CausalChain> = Vec::new();

        for symptom_id in symptom_node_ids {
            let symptom_node = match self.node_by_id(symptom_id) {
                Some(n) => n,
                None => continue,
            };

            // Forward search
            {
                let mut heap: BinaryHeap<WeightedPath> = BinaryHeap::new();
                let mut visited = HashSet::new();
                visited.insert(symptom_id.to_string());

                let start_chain_node = ChainNode {
                    node_id: symptom_id.to_string(),
                    node_name: symptom_node.name.clone(),
                    file_path: symptom_node.file_path.clone(),
                    line: symptom_node.line,
                    edge_to_next: None,
                };
                heap.push(WeightedPath {
                    node_id: symptom_id.to_string(),
                    accumulated_weight: 1.0,
                    chain: vec![start_chain_node],
                });

                while let Some(current) = heap.pop() {
                    if current.chain.len() > max_depth {
                        continue;
                    }

                    for edge in self.outgoing_edges(&current.node_id) {
                        let (target_id, edge_label) = match edge.relation {
                            EdgeRelation::Calls => (&edge.to, "calls"),
                            EdgeRelation::Inherits => (&edge.to, "inherits"),
                            EdgeRelation::Imports => (&edge.to, "imports"),
                            EdgeRelation::Overrides => (&edge.to, "overrides"),
                            EdgeRelation::TestsFor => (&edge.to, "tests"),
                            _ => continue,
                        };
                        if visited.contains(target_id) {
                            continue;
                        }
                        if let Some(target_node) = self.node_by_id(target_id) {
                            visited.insert(target_node.id.clone());
                            let new_weight = current.accumulated_weight * edge.weight;

                            let mut new_chain = current.chain.clone();
                            if let Some(last) = new_chain.last_mut() {
                                last.edge_to_next = Some(edge_label.to_string());
                            }
                            new_chain.push(ChainNode {
                                node_id: target_node.id.clone(),
                                node_name: target_node.name.clone(),
                                file_path: target_node.file_path.clone(),
                                line: target_node.line,
                                edge_to_next: None,
                            });

                            if new_chain.len() >= 2 {
                                all_chains.push(CausalChain {
                                    symptom_node_id: symptom_id.to_string(),
                                    chain: new_chain.clone(),
                                });
                            }

                            if new_chain.len() < max_depth {
                                heap.push(WeightedPath {
                                    node_id: target_node.id.clone(),
                                    accumulated_weight: new_weight,
                                    chain: new_chain,
                                });
                            }
                        }
                    }
                }
            }

            // Reverse search
            {
                let mut heap: BinaryHeap<WeightedPath> = BinaryHeap::new();
                let mut visited = HashSet::new();
                visited.insert(symptom_id.to_string());

                let start_chain_node = ChainNode {
                    node_id: symptom_id.to_string(),
                    node_name: symptom_node.name.clone(),
                    file_path: symptom_node.file_path.clone(),
                    line: symptom_node.line,
                    edge_to_next: None,
                };
                heap.push(WeightedPath {
                    node_id: symptom_id.to_string(),
                    accumulated_weight: 1.0,
                    chain: vec![start_chain_node],
                });

                while let Some(current) = heap.pop() {
                    if current.chain.len() > max_depth {
                        continue;
                    }

                    for edge in self.incoming_edges(&current.node_id) {
                        if edge.relation != EdgeRelation::Calls
                            && edge.relation != EdgeRelation::Imports
                            && edge.relation != EdgeRelation::Overrides
                        {
                            continue;
                        }
                        if visited.contains(&edge.from) {
                            continue;
                        }
                        if let Some(caller) = self.node_by_id(&edge.from) {
                            if caller.file_path.contains("/tests/")
                                || caller.file_path.contains("/test_")
                            {
                                continue;
                            }
                            visited.insert(caller.id.clone());
                            let new_weight = current.accumulated_weight * edge.weight;

                            let edge_label = match edge.relation {
                                EdgeRelation::Imports => "imported_by",
                                EdgeRelation::Overrides => "overridden_by",
                                _ => "called_by",
                            };
                            let mut new_chain = current.chain.clone();
                            if let Some(last) = new_chain.last_mut() {
                                last.edge_to_next = Some(edge_label.to_string());
                            }
                            new_chain.push(ChainNode {
                                node_id: caller.id.clone(),
                                node_name: caller.name.clone(),
                                file_path: caller.file_path.clone(),
                                line: caller.line,
                                edge_to_next: None,
                            });

                            if new_chain.len() >= 2 {
                                all_chains.push(CausalChain {
                                    symptom_node_id: symptom_id.to_string(),
                                    chain: new_chain.clone(),
                                });
                            }

                            if new_chain.len() < max_depth {
                                heap.push(WeightedPath {
                                    node_id: caller.id.clone(),
                                    accumulated_weight: new_weight,
                                    chain: new_chain,
                                });
                            }
                        }
                    }
                }
            }
        }

        // Sort and deduplicate
        all_chains.sort_by(|a, b| {
            let len_cmp = a.chain.len().cmp(&b.chain.len());
            if len_cmp != std::cmp::Ordering::Equal {
                return len_cmp;
            }
            let a_source = a
                .chain
                .iter()
                .filter(|n| !n.file_path.contains("/tests/") && !n.file_path.contains("/test_"))
                .count();
            let b_source = b
                .chain
                .iter()
                .filter(|n| !n.file_path.contains("/tests/") && !n.file_path.contains("/test_"))
                .count();
            b_source.cmp(&a_source)
        });

        let mut deduped: Vec<CausalChain> = Vec::new();
        for chain in &all_chains {
            let is_prefix = deduped.iter().any(|existing| {
                existing.chain.len() > chain.chain.len()
                    && chain
                        .chain
                        .iter()
                        .zip(existing.chain.iter())
                        .all(|(a, b)| a.node_id == b.node_id)
            });
            if is_prefix {
                continue;
            }
            deduped.retain(|existing| {
                !(existing.chain.len() < chain.chain.len()
                    && existing
                        .chain
                        .iter()
                        .zip(chain.chain.iter())
                        .all(|(a, b)| a.node_id == b.node_id))
            });
            deduped.push(chain.clone());
        }

        deduped.truncate(max_chains);
        deduped
    }

    /// Trace causal chains from changed nodes to failed tests.
    pub fn trace_causal_chains(
        &self,
        changed_node_ids: &[&str],
        failed_p2p_tests: &[String],
        failed_f2p_tests: &[String],
    ) -> String {
        if failed_p2p_tests.is_empty() && failed_f2p_tests.is_empty() {
            return String::new();
        }

        let mut result = String::new();

        if !failed_p2p_tests.is_empty() {
            result.push_str("## 🚨 CAUSAL ANALYSIS — Why Your Fix Broke Existing Tests\n\n");
            result.push_str(
                "These tests PASSED before your change and now FAIL. You MUST fix these regressions.\n\n",
            );

            for test_name in failed_p2p_tests {
                let short_name = test_name.split("::").last().unwrap_or(test_name);
                result.push_str(&format!("### ❌ REGRESSION: `{}`\n", short_name));

                let test_node = self.nodes.iter().find(|n| {
                    n.name == short_name
                        || n.name.ends_with(short_name)
                        || (n.file_path.contains("/test") && n.name == short_name)
                });

                if let Some(test) = test_node {
                    let chains = self.find_paths_to_test(changed_node_ids, &test.id);

                    if !chains.is_empty() {
                        result.push_str("**Causal chain(s):**\n");
                        for chain in chains.iter().take(3) {
                            let chain_str: Vec<String> = chain
                                .iter()
                                .map(|id| {
                                    self.nodes
                                        .iter()
                                        .find(|n| n.id == *id)
                                        .map(|n| format!("`{}` ({})", n.name, n.file_path))
                                        .unwrap_or_else(|| id.to_string())
                                })
                                .collect();
                            result.push_str(&format!("  🔗 {}\n", chain_str.join(" → ")));
                        }
                        result.push_str("\n**What this means:** Your change propagated through the dependency chain above and broke this test.\n");
                        result.push_str("**How to fix:** Make your change more surgical — ensure the modified function's behavior is backward-compatible for the callers in this chain.\n\n");
                    } else {
                        // No direct graph path — check file-level connection
                        let changed_files: HashSet<String> = changed_node_ids
                            .iter()
                            .filter_map(|id| self.node_by_id(id))
                            .map(|n| n.file_path.clone())
                            .collect();

                        if changed_files
                            .iter()
                            .any(|f| test.file_path.contains(f.as_str()))
                            || self.shares_import(&test.id, changed_node_ids)
                        {
                            result.push_str("**Connection:** Indirect — test imports or uses a module you changed.\n");
                            result.push_str("**How to fix:** Check that your change doesn't alter the public API or default behavior of the module.\n\n");
                        } else {
                            result.push_str("**Connection:** Could not trace via graph (may be via dynamic dispatch, monkey-patching, or shared global state).\n");
                            result.push_str("**How to fix:** Read the test's assertion error carefully — it will tell you what behavior changed.\n\n");
                        }
                    }
                } else {
                    result.push_str(
                        "**Note:** Test not found in code graph. Read the error output to understand what broke.\n\n",
                    );
                }
            }

            result.push_str("### 🎯 Overall Regression Fix Strategy\n");
            result.push_str(
                "1. **Don't change your approach** — your bug fix logic is likely correct\n",
            );
            result.push_str("2. **Narrow the scope** — guard your change with a condition so it only applies to the bug case\n");
            result.push_str("3. **Add backward compatibility** — if you changed a return type/value, ensure callers still get what they expect\n");
            result.push_str("4. **Check default parameters** — if you changed defaults, existing callers rely on the old defaults\n\n");
        }

        if !failed_f2p_tests.is_empty() {
            result.push_str("## ⚠️ Original Bug Not Fixed\n");
            result.push_str("These tests still fail — your fix is incomplete or incorrect:\n");
            for test_name in failed_f2p_tests {
                let short_name = test_name.split("::").last().unwrap_or(test_name);
                result.push_str(&format!("- `{}`\n", short_name));
            }
            result.push('\n');
        }

        result
    }

    fn find_paths_to_test(&self, changed_node_ids: &[&str], test_node_id: &str) -> Vec<Vec<String>> {
        let mut paths = Vec::new();

        for changed_id in changed_node_ids {
            if let Some(path) = self.bfs_path(test_node_id, changed_id, 5) {
                let mut p = path;
                p.reverse();
                paths.push(p);
            }
        }

        paths
    }

    /// BFS shortest path from `from` to `to`.
    pub fn bfs_path(&self, from: &str, to: &str, max_depth: usize) -> Option<Vec<String>> {
        let mut queue: VecDeque<(String, Vec<String>)> = VecDeque::new();
        let mut visited = HashSet::new();

        queue.push_back((from.to_string(), vec![from.to_string()]));
        visited.insert(from.to_string());

        while let Some((current, path)) = queue.pop_front() {
            if path.len() > max_depth {
                continue;
            }

            for edge in self.outgoing_edges(&current) {
                if edge.to == to {
                    let mut final_path = path.clone();
                    final_path.push(edge.to.clone());
                    return Some(final_path);
                }
                if !visited.contains(&edge.to) {
                    visited.insert(edge.to.clone());
                    let mut new_path = path.clone();
                    new_path.push(edge.to.clone());
                    queue.push_back((edge.to.clone(), new_path));
                }
            }
        }
        None
    }

    /// Get a summary of a node: name, file, line, and first 15 lines of code.
    pub fn get_node_summary(&self, node_id: &str, repo_dir: &Path) -> String {
        let node = match self.node_by_id(node_id) {
            Some(n) => n,
            None => return format!("[unknown node: {}]", node_id),
        };

        let mut result = format!(
            "{} ({}:{})",
            node.name,
            node.file_path,
            node.line.map(|l| l.to_string()).unwrap_or_else(|| "?".to_string()),
        );

        let full_path = repo_dir.join(&node.file_path);
        if let Ok(content) = std::fs::read_to_string(&full_path) {
            let lines: Vec<&str> = content.lines().collect();
            if let Some(start_line) = node.line {
                if start_line > 0 && start_line <= lines.len() {
                    let start_idx = start_line - 1;
                    let end_idx = (start_idx + 15).min(lines.len());
                    let preview: String = lines[start_idx..end_idx]
                        .iter()
                        .map(|l| *l)
                        .collect::<Vec<_>>()
                        .join("\n");
                    result.push('\n');
                    result.push_str(&preview);
                }
            }
        }

        result
    }

    /// Extract code snippets for nodes.
    pub fn extract_snippets(
        &self,
        nodes: &[&CodeNode],
        repo_dir: &Path,
        max_lines: usize,
    ) -> HashMap<String, String> {
        let mut snippets = HashMap::new();
        let mut file_cache: HashMap<String, Vec<String>> = HashMap::new();

        for node in nodes {
            if node.kind == NodeKind::File {
                continue;
            }

            let file_path = repo_dir.join(&node.file_path);
            let lines = file_cache.entry(node.file_path.clone()).or_insert_with(|| {
                std::fs::read_to_string(&file_path)
                    .unwrap_or_default()
                    .lines()
                    .map(|l| l.to_string())
                    .collect()
            });

            if let Some(start_line) = node.line {
                if start_line == 0 || start_line > lines.len() {
                    continue;
                }
                let start_idx = start_line - 1;

                let base_indent = lines[start_idx]
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .count();

                let mut end_idx = start_idx + 1;
                while end_idx < lines.len() && end_idx < start_idx + max_lines {
                    let line = &lines[end_idx];
                    if line.trim().is_empty() {
                        end_idx += 1;
                        continue;
                    }
                    let indent = line.chars().take_while(|c| c.is_whitespace()).count();
                    if indent <= base_indent && !line.trim().is_empty() {
                        break;
                    }
                    end_idx += 1;
                }

                let snippet: String = lines[start_idx..end_idx.min(lines.len())]
                    .iter()
                    .map(|l| l.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");

                if !snippet.trim().is_empty() {
                    snippets.insert(node.id.clone(), snippet);
                }
            }
        }

        snippets
    }

    /// Format graph for LLM context.
    pub fn format_for_llm(&self, keywords: &[&str], max_chars: usize) -> String {
        let relevant = self.find_relevant_nodes(keywords);

        if relevant.is_empty() {
            return self.format_file_summary(max_chars);
        }

        let mut result = String::from("**Code structure (relevant to issue):**\n");

        result.push_str("\nRelevant files/classes/functions:\n");
        let relevant_ids: HashSet<&str> = relevant.iter().map(|n| n.id.as_str()).collect();

        for node in relevant.iter().take(20) {
            let prefix = match node.kind {
                NodeKind::File => "📄",
                NodeKind::Class => "🔷",
                NodeKind::Function => "🔹",
                NodeKind::Module => "📦",
            };
            let line_info = node.line.map(|l| format!(" (line {})", l)).unwrap_or_default();
            result.push_str(&format!(
                "{} {} — `{}`{}\n",
                prefix, node.name, node.file_path, line_info
            ));

            if result.len() > max_chars / 2 {
                break;
            }
        }

        let relevant_edges: Vec<&CodeEdge> = self
            .edges
            .iter()
            .filter(|e| {
                relevant_ids.contains(e.from.as_str()) || relevant_ids.contains(e.to.as_str())
            })
            .filter(|e| e.relation != EdgeRelation::DefinedIn)
            .collect();

        if !relevant_edges.is_empty() {
            result.push_str("\nRelationships:\n");
            for edge in relevant_edges.iter().take(15) {
                let from_name = self.node_name(&edge.from);
                let to_name = self.node_name(&edge.to);
                result.push_str(&format!(
                    "  {} --[{}]--> {}\n",
                    from_name, edge.relation, to_name
                ));

                if result.len() > max_chars {
                    break;
                }
            }
        }

        let relevant_classes: Vec<&&CodeNode> = relevant
            .iter()
            .filter(|n| n.kind == NodeKind::Class)
            .collect();

        if !relevant_classes.is_empty() {
            result.push_str("\nInheritance:\n");
            for cls in relevant_classes.iter().take(5) {
                let chain = self.get_inheritance_chain(&cls.id);
                if chain.len() > 1 {
                    let names: Vec<String> =
                        chain.iter().map(|id| self.node_name(id)).collect();
                    result.push_str(&format!("  {} \n", names.join(" → ")));
                }
            }
        }

        let file_count = self.nodes.iter().filter(|n| n.kind == NodeKind::File).count();
        let class_count = self.nodes.iter().filter(|n| n.kind == NodeKind::Class).count();
        let import_count = self
            .edges
            .iter()
            .filter(|e| e.relation == EdgeRelation::Imports)
            .count();
        let inherit_count = self
            .edges
            .iter()
            .filter(|e| e.relation == EdgeRelation::Inherits)
            .count();

        result.push_str(&format!(
            "\nGraph: {} files, {} classes, {} imports, {} inheritance edges\n",
            file_count, class_count, import_count, inherit_count
        ));

        if result.len() > max_chars {
            result.truncate(max_chars);
            result.push_str("\n...[truncated]\n");
        }

        result
    }

    fn format_file_summary(&self, max_chars: usize) -> String {
        let mut result = String::from("**Repository files:**\n");

        let files: Vec<&CodeNode> = self
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::File)
            .collect();

        for file in &files {
            let classes: Vec<String> = self
                .nodes
                .iter()
                .filter(|n| n.kind == NodeKind::Class && n.file_path == file.file_path)
                .map(|n| n.name.clone())
                .collect();

            let mut line = format!("- `{}`", file.file_path);
            if !classes.is_empty() {
                line.push_str(&format!(" — {}", classes.join(", ")));
            }
            line.push('\n');

            if result.len() + line.len() > max_chars {
                result.push_str(&format!("... and {} more files\n", files.len()));
                break;
            }
            result.push_str(&line);
        }

        result
    }

    fn node_name(&self, id: &str) -> String {
        self.nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    fn get_inheritance_chain(&self, class_id: &str) -> Vec<String> {
        let mut chain = vec![class_id.to_string()];
        let mut current = class_id.to_string();

        for _ in 0..10 {
            let parent = self
                .edges
                .iter()
                .find(|e| e.from == current && e.relation == EdgeRelation::Inherits);
            match parent {
                Some(edge) => {
                    chain.push(edge.to.clone());
                    current = edge.to.clone();
                }
                None => break,
            }
        }

        chain
    }

    /// Check if a test node shares imports with any of the changed nodes.
    /// Returns true if the test imports a file/module that contains a changed node.
    fn shares_import(&self, test_node_id: &str, changed_node_ids: &[&str]) -> bool {
        let test_imports: HashSet<String> = self
            .edges
            .iter()
            .filter(|e| e.from == test_node_id && e.relation == EdgeRelation::Imports)
            .map(|e| e.to.clone())
            .collect();

        let changed_files: HashSet<String> = changed_node_ids
            .iter()
            .filter_map(|id| self.node_by_id(id))
            .flat_map(|n| {
                let file_id = format!("file:{}", n.file_path);
                vec![n.id.clone(), file_id]
            })
            .collect();

        test_imports.intersection(&changed_files).next().is_some()
    }

    /// Search for identifiers in repo via grep
    pub fn grep_for_identifiers(&self, repo_dir: &Path, identifiers: &[&str]) -> Vec<CodeNode> {
        let mut found_nodes = Vec::new();
        let existing_names: HashSet<String> = self.nodes.iter().map(|n| n.name.clone()).collect();

        for ident in identifiers {
            if existing_names.contains(*ident) {
                continue;
            }

            let patterns = [
                format!("class {}[:(]", ident),
                format!("def {}[(]", ident),
                format!("class {}\\b", ident),
            ];

            for pattern in &patterns {
                if let Ok(output) = std::process::Command::new("grep")
                    .args(["-rn", pattern, "--include=*.py", "-l"])
                    .current_dir(repo_dir)
                    .output()
                {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    for file_path in stdout.lines().take(3) {
                        let file_path = file_path.trim();
                        if file_path.is_empty()
                            || file_path.contains("/tests/")
                            || file_path.contains("/test_")
                        {
                            continue;
                        }

                        if let Ok(line_output) = std::process::Command::new("grep")
                            .args(["-n", pattern, file_path])
                            .current_dir(repo_dir)
                            .output()
                        {
                            let line_stdout = String::from_utf8_lossy(&line_output.stdout);
                            if let Some(first_line) = line_stdout.lines().next() {
                                let line_num: usize = first_line
                                    .split(':')
                                    .next()
                                    .unwrap_or("0")
                                    .parse()
                                    .unwrap_or(0);

                                let is_class = first_line.contains("class ");
                                found_nodes.push(CodeNode {
                                    id: format!("grep:{}:{}", file_path, ident),
                                    kind: if is_class {
                                        NodeKind::Class
                                    } else {
                                        NodeKind::Function
                                    },
                                    name: ident.to_string(),
                                    file_path: file_path.to_string(),
                                    line: if line_num > 0 { Some(line_num) } else { None },
                                    decorators: Vec::new(),
                                    signature: None,
                                    docstring: None,
                                    line_count: 0,
                                    is_test: false,
                                });
                                break;
                            }
                        }
                    }
                }
                if found_nodes.iter().any(|n| n.name == *ident) {
                    break;
                }
            }
        }

        found_nodes
    }

    /// Extract keywords from a problem statement
    pub fn extract_keywords(problem_statement: &str) -> Vec<&str> {
        let mut keywords = Vec::new();

        for word in
            problem_statement.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '.')
        {
            let trimmed = word.trim();
            if trimmed.len() < 3 {
                continue;
            }
            let lower = trimmed.to_lowercase();
            if [
                "the", "and", "for", "that", "this", "with", "from", "not", "but", "are", "was",
                "has", "have", "can", "should", "would", "when", "what", "how", "does", "bug",
                "fix", "issue", "error", "problem", "description",
            ]
            .contains(&lower.as_str())
            {
                continue;
            }
            if trimmed.contains('_')
                || trimmed.contains('.')
                || trimmed.chars().any(|c| c.is_uppercase())
                || trimmed.ends_with(".py")
            {
                keywords.push(trimmed);
            }
        }

        keywords.dedup();
        keywords.truncate(20);
        keywords
    }

    /// Check if graph has a node with given file and name
    pub fn has_node(&self, file_path: &str, name: &str) -> bool {
        let needle = file_path.strip_prefix("./").unwrap_or(file_path);
        self.nodes.iter().any(|n| {
            let hay = n.file_path.strip_prefix("./").unwrap_or(&n.file_path);
            hay == needle && n.name == name
        })
    }

    /// Find a node by file and name
    pub fn find_node(&self, file_path: &str, name: &str) -> Option<&CodeNode> {
        let needle = file_path.strip_prefix("./").unwrap_or(file_path);
        self.nodes.iter().find(|n| {
            let hay = n.file_path.strip_prefix("./").unwrap_or(&n.file_path);
            hay == needle && n.name == name
        })
    }

    /// Add nodes from a specific file
    pub fn add_file_nodes(
        &mut self,
        repo_dir: &Path,
        file_path: &Path,
        target_names: Option<&[String]>,
    ) -> anyhow::Result<()> {
        use anyhow::Context;

        let full_path = repo_dir.join(file_path);
        if !full_path.exists() {
            anyhow::bail!("File not found: {:?}", full_path);
        }

        let source = std::fs::read_to_string(&full_path)
            .context(format!("Failed to read {:?}", full_path))?;

        let mut parser = Parser::new();
        let language = tree_sitter_python::LANGUAGE;
        parser
            .set_language(&language.into())
            .context("Failed to set Python language")?;

        let tree = parser
            .parse(&source, None)
            .context("Failed to parse Python file")?;

        let file_path_str = file_path.to_string_lossy().to_string();

        let root = tree.root_node();

        fn extract_from_node(
            node: tree_sitter::Node,
            source: &str,
            file_path: &str,
            nodes: &mut Vec<CodeNode>,
            target_names: Option<&[String]>,
        ) {
            if node.kind() == "function_definition" {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = &source[name_node.byte_range()];
                    let matched =
                        target_names.map_or(true, |targets| targets.iter().any(|t| t == name));
                    if matched {
                        let line = name_node.start_position().row + 1;
                        let id = format!("func:{}:{}", file_path, name);
                        nodes.push(CodeNode {
                            id,
                            kind: NodeKind::Function,
                            name: name.to_string(),
                            file_path: file_path.to_string(),
                            line: Some(line),
                            decorators: vec![],
                            signature: None,
                            docstring: None,
                            line_count: 0,
                            is_test: false,
                        });
                    }
                }
            } else if node.kind() == "class_definition" {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = &source[name_node.byte_range()];
                    let matched =
                        target_names.map_or(true, |targets| targets.iter().any(|t| t == name));
                    if matched {
                        let line = name_node.start_position().row + 1;
                        let id = format!("class:{}:{}", file_path, name);
                        nodes.push(CodeNode {
                            id,
                            kind: NodeKind::Class,
                            name: name.to_string(),
                            file_path: file_path.to_string(),
                            line: Some(line),
                            decorators: vec![],
                            signature: None,
                            docstring: None,
                            line_count: 0,
                            is_test: false,
                        });
                    }
                }
            }

            for child in node.children(&mut node.walk()) {
                extract_from_node(child, source, file_path, nodes, target_names);
            }
        }

        extract_from_node(root, &source, &file_path_str, &mut self.nodes, target_names);
        self.build_indexes();

        Ok(())
    }

    /// Return graph schema information
    pub fn get_schema(&self) -> String {
        let node_kinds: HashSet<&str> = self.nodes.iter().map(|n| match n.kind {
            NodeKind::File => "File",
            NodeKind::Class => "Class",
            NodeKind::Function => "Function",
            NodeKind::Module => "Module",
        }).collect();

        let edge_relations: HashSet<&str> = self.edges.iter().map(|e| match e.relation {
            EdgeRelation::Imports => "imports",
            EdgeRelation::Inherits => "inherits",
            EdgeRelation::DefinedIn => "defined_in",
            EdgeRelation::Calls => "calls",
            EdgeRelation::TestsFor => "tests_for",
            EdgeRelation::Overrides => "overrides",
            EdgeRelation::Implements => "implements",
        }).collect();

        format!(
            "Schema:\n  Node kinds: {:?}\n  Edge relations: {:?}\n  Total nodes: {}\n  Total edges: {}",
            node_kinds,
            edge_relations,
            self.nodes.len(),
            self.edges.len()
        )
    }

    /// Get file-level summary
    pub fn get_file_summary(&self, file_path: &str) -> String {
        let file_nodes: Vec<&CodeNode> = self.nodes.iter()
            .filter(|n| n.file_path == file_path)
            .collect();

        if file_nodes.is_empty() {
            return format!("No nodes found for file: {}", file_path);
        }

        let classes: Vec<&str> = file_nodes.iter()
            .filter(|n| n.kind == NodeKind::Class)
            .map(|n| n.name.as_str())
            .collect();

        let functions: Vec<&str> = file_nodes.iter()
            .filter(|n| n.kind == NodeKind::Function)
            .map(|n| n.name.as_str())
            .collect();

        format!(
            "File: {}\n  Classes ({}): {}\n  Functions ({}): {}",
            file_path,
            classes.len(),
            classes.join(", "),
            functions.len(),
            functions.join(", ")
        )
    }

    // ═══ Failure Analysis ═══

    /// Analyze test failures using graph structure.
    /// Given changed nodes and failed test names, trace call chains and explain WHY tests failed.
    pub fn analyze_test_failures(
        &self,
        changed_node_ids: &[&str],
        failed_test_names: &[String],
        _repo_dir: &Path,
    ) -> String {
        let mut analysis = String::new();
        analysis.push_str("## 🔍 Graph-based Failure Analysis\n\n");

        // Map changed node IDs to names for readable output
        let changed_names: Vec<String> = changed_node_ids.iter()
            .filter_map(|id| self.node_by_id(id))
            .map(|n| n.name.clone())
            .collect();

        let changed_files: HashSet<String> = changed_node_ids.iter()
            .filter_map(|id| self.node_by_id(id))
            .map(|n| n.file_path.clone())
            .collect();

        // For each failed test, trace the connection to our changes
        for test_name in failed_test_names {
            // Extract the short function name from test ID
            // e.g., "tests/test_foo.py::test_bar" → "test_bar"
            let short_name = test_name.split("::").last().unwrap_or(test_name);
            
            // Find this test in the graph
            let test_node = self.nodes.iter().find(|n| {
                n.name == short_name
                    || n.name.ends_with(short_name)
                    || (n.file_path.contains("/test") && n.name == short_name)
            });

            analysis.push_str(&format!("### ❌ {}\n", short_name));

            if let Some(test) = test_node {
                // Trace: what does this test call that we changed?
                let callees = self.get_callees(&test.id);
                let mut found_connection = false;

                for callee in &callees {
                    if changed_node_ids.contains(&callee.id.as_str())
                        || changed_names.contains(&callee.name)
                    {
                        analysis.push_str(&format!(
                            "**Direct call chain:** `{}` → `{}` (YOU CHANGED THIS)\n",
                            short_name, callee.name
                        ));
                        found_connection = true;

                        // Show other callers of the changed function
                        let other_callers = self.get_callers(&callee.id);
                        let other_caller_names: Vec<&str> = other_callers.iter()
                            .filter(|c| c.id != test.id)
                            .map(|c| c.name.as_str())
                            .take(5)
                            .collect();
                        if !other_caller_names.is_empty() {
                            analysis.push_str(&format!(
                                "**Other callers of `{}`:** {}\n",
                                callee.name,
                                other_caller_names.join(", ")
                            ));
                        }
                    }
                }

                // If no direct connection, check indirect (2-hop)
                if !found_connection {
                    for callee in &callees {
                        let sub_callees = self.get_callees(&callee.id);
                        for sub in &sub_callees {
                            if changed_node_ids.contains(&sub.id.as_str())
                                || changed_names.contains(&sub.name)
                            {
                                analysis.push_str(&format!(
                                    "**Indirect chain:** `{}` → `{}` → `{}` (YOU CHANGED THIS)\n",
                                    short_name, callee.name, sub.name
                                ));
                                found_connection = true;
                                break;
                            }
                        }
                        if found_connection { break; }
                    }
                }

                // If still no connection, check file-level TestsFor edges
                if !found_connection {
                    let test_file = &test.file_path;
                    let test_file_id = format!("file:{}", test_file);
                    
                    for edge in self.outgoing_edges(&test_file_id) {
                        if edge.relation == EdgeRelation::TestsFor {
                            if let Some(target) = self.node_by_id(&edge.to) {
                                if changed_files.contains(&target.file_path) {
                                    analysis.push_str(&format!(
                                        "**File-level connection:** test file `{}` tests `{}` which you modified\n",
                                        test_file, target.file_path
                                    ));
                                    found_connection = true;
                                    break;
                                }
                            }
                        }
                    }
                }

                if !found_connection {
                    analysis.push_str("**Connection:** Could not trace via graph (may be indirect import)\n");
                }
            } else {
                analysis.push_str("**Note:** Test not found in code graph\n");
            }
            analysis.push('\n');
        }

        // Summary
        if !changed_names.is_empty() {
            analysis.push_str("### Summary\n");
            analysis.push_str(&format!("**You changed:** {}\n", changed_names.join(", ")));
            
            let total_callers: usize = changed_node_ids.iter()
                .map(|id| self.get_callers(id).len())
                .sum();
            analysis.push_str(&format!(
                "**Total callers of changed code:** {}\n",
                total_callers
            ));
            analysis.push_str("**Repair strategy:** Keep the fix but make it backward-compatible with all callers.\n");
        }

        analysis
    }

    /// Find symptom nodes from test names and issue text.
    ///
    /// Parses test names (JSON array or newline-separated), finds matching test nodes.
    /// Also finds nodes mentioned in issue text (functions/classes in error messages/tracebacks).
    /// Returns combined list, tests first.
    pub fn find_symptom_nodes(&self, problem_statement: &str, test_names: &str) -> Vec<&CodeNode> {
        let mut result: Vec<&CodeNode> = Vec::new();
        let mut seen = HashSet::new();

        // 1. Parse test names (try JSON first, then newline-separated)
        let test_list: Vec<String> = serde_json::from_str(test_names)
            .unwrap_or_else(|_| {
                test_names.lines()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            });

        for test_id in &test_list {
            // Extract short test function name from various formats:
            // "tests/test_foo.py::TestClass::test_method" → "test_method"
            // "test_method (module.TestClass)" → "test_method"
            let short_name = if test_id.contains("::") {
                test_id.split("::").last().unwrap_or(test_id)
            } else if test_id.contains(" (") {
                test_id.split(" (").next().unwrap_or(test_id).trim()
            } else {
                test_id.as_str()
            };

            // Find matching test node in graph
            for node in &self.nodes {
                if node.kind == NodeKind::Function
                    && (node.name == short_name || node.name.ends_with(short_name))
                    && (node.file_path.contains("/tests/")
                        || node.file_path.contains("/test_")
                        || node.name.starts_with("test_"))
                {
                    if seen.insert(node.id.clone()) {
                        result.push(node);
                    }
                }
            }
        }

        // 2. Find nodes mentioned in issue text (functions/classes in tracebacks)
        for line in problem_statement.lines() {
            let trimmed = line.trim();

            // Python traceback: "File \"path\", line N, in <function_name>"
            if trimmed.contains(", in ") {
                if let Some(func_part) = trimmed.rsplit(", in ").next() {
                    let func_name = func_part.trim().trim_start_matches('<').trim_end_matches('>');
                    if func_name.len() >= 3 && func_name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        for node in &self.nodes {
                            if node.name == func_name && node.kind == NodeKind::Function {
                                if seen.insert(node.id.clone()) {
                                    result.push(node);
                                }
                            }
                        }
                    }
                }
            }

            // Look for quoted identifiers
            for quote in &['\'', '"', '`'] {
                let parts: Vec<&str> = trimmed.split(*quote).collect();
                for i in (1..parts.len()).step_by(2) {
                    let word = parts[i].trim();
                    if word.len() >= 3
                        && word.len() <= 60
                        && word.chars().all(|c| c.is_alphanumeric() || c == '_')
                    {
                        for node in &self.nodes {
                            if node.name == word && (node.kind == NodeKind::Function || node.kind == NodeKind::Class) {
                                if seen.insert(node.id.clone()) {
                                    result.push(node);
                                }
                            }
                        }
                    }
                }
            }
        }

        // 3. Match CamelCase class names from issue text
        for word in problem_statement.split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')' || c == '\'' || c == '"' || c == '`') {
            let word = word.trim_matches(|c: char| c == '.' || c == ':' || c == ';');
            if word.len() < 4 { continue; }
            let has_upper = word.chars().filter(|c| c.is_uppercase()).count() >= 2;
            let has_lower = word.chars().any(|c| c.is_lowercase());
            let is_ident = word.chars().all(|c| c.is_alphanumeric() || c == '_');
            if has_upper && has_lower && is_ident {
                for node in &self.nodes {
                    if node.name == word && node.kind == NodeKind::Class {
                        if seen.insert(node.id.clone()) {
                            result.push(node);
                        }
                    }
                }
            }
        }

        // 4. Fuzzy keyword matching from test names if we found nothing
        if result.is_empty() {
            for test_id in &test_list {
                let short_name = if test_id.contains("::") {
                    test_id.split("::").last().unwrap_or(test_id)
                } else if test_id.contains(" (") {
                    test_id.split(" (").next().unwrap_or(test_id).trim()
                } else {
                    test_id.as_str()
                };
                
                // Extract keywords: test_fast_delete_all → ["fast", "delete"]
                let kws: Vec<&str> = short_name.split('_')
                    .filter(|w| w.len() >= 3 && *w != "test" && *w != "tests")
                    .collect();
                if kws.is_empty() { continue; }
                
                // Find source (non-test) nodes that match keywords
                for node in &self.nodes {
                    if node.file_path.contains("/tests/") || node.file_path.contains("/test_") {
                        continue;
                    }
                    let name_lower = node.name.to_lowercase();
                    let match_count = kws.iter()
                        .filter(|kw| name_lower.contains(&kw.to_lowercase()))
                        .count();
                    if match_count >= 2 || (match_count >= 1 && kws.len() == 1) {
                        if seen.insert(node.id.clone()) {
                            result.push(node);
                        }
                    }
                }

                // Also try matching the test class name to find the test file → source imports
                // "test_method (module.tests.TestClass)" → "TestClass" → find the test file
                if test_id.contains(" (") {
                    let class_part = test_id
                        .split(" (")
                        .nth(1)
                        .unwrap_or("")
                        .trim_end_matches(')');
                    let class_name = class_part.rsplit('.').next().unwrap_or("");
                    if !class_name.is_empty() {
                        for node in &self.nodes {
                            if node.kind == NodeKind::Class && node.name == class_name {
                                let file_id = format!("file:{}", node.file_path);
                                for edge in self.outgoing_edges(&file_id) {
                                    if edge.relation == EdgeRelation::TestsFor {
                                        if let Some(target) = self.node_by_id(&edge.to) {
                                            if target.kind != NodeKind::File {
                                                if seen.insert(target.id.clone()) {
                                                    result.push(target);
                                                }
                                            }
                                        }
                                        for src_node in &self.nodes {
                                            if format!("file:{}", src_node.file_path) == edge.to
                                                && src_node.kind != NodeKind::File
                                            {
                                                if seen.insert(src_node.id.clone()) {
                                                    result.push(src_node);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        result
    }

    /// Build a unified graph combining code nodes with task structure.
    /// Returns a simplified representation suitable for task planning.
    pub fn build_unified_graph(
        &self,
        relevant_nodes: &[&CodeNode],
        snippets: &HashMap<String, String>,
        issue_id: &str,
        issue_description: &str,
    ) -> UnifiedGraphResult {
        let relevant_ids: HashSet<&str> = relevant_nodes.iter()
            .map(|n| n.id.as_str())
            .collect();

        // Build nodes
        let mut nodes: Vec<UnifiedNode> = Vec::new();
        for code_node in relevant_nodes {
            let node_id = code_node.name.replace(|c: char| !c.is_alphanumeric() && c != '_', "_");
            
            let (node_type, layer) = match code_node.kind {
                NodeKind::File => ("File".to_string(), "infrastructure"),
                NodeKind::Class => ("Component".to_string(), "domain"),
                NodeKind::Function | NodeKind::Module => ("Component".to_string(), "application"),
            };
            
            let snippet = snippets.get(&code_node.id).cloned();
            
            nodes.push(UnifiedNode {
                id: node_id,
                node_type,
                layer: layer.to_string(),
                description: format!("{} in {}", code_node.name, code_node.file_path),
                path: Some(code_node.file_path.clone()),
                line: code_node.line,
                code: snippet,
            });
        }

        // Build edges using adjacency indexes
        let mut edges: Vec<UnifiedEdge> = Vec::new();
        let mut seen_keys: HashSet<(String, String, String)> = HashSet::new();
        
        for rel_id in &relevant_ids {
            for edge in self.outgoing_edges(rel_id) {
                if let (Some(from), Some(to)) = (self.node_by_id(&edge.from), self.node_by_id(&edge.to)) {
                    let from_id = from.name.replace(|c: char| !c.is_alphanumeric() && c != '_', "_");
                    let to_id = to.name.replace(|c: char| !c.is_alphanumeric() && c != '_', "_");
                    let rel = edge.relation.to_string();
                    let key = (from_id.clone(), to_id.clone(), rel.clone());
                    
                    if nodes.iter().any(|n| n.id == from_id) 
                        && nodes.iter().any(|n| n.id == to_id)
                        && seen_keys.insert(key)
                    {
                        edges.push(UnifiedEdge {
                            from: from_id,
                            to: to_id,
                            relation: rel,
                        });
                    }
                }
            }
        }

        let description = if issue_description.len() > 100 {
            let mut end = 100;
            while end > 0 && !issue_description.is_char_boundary(end) { end -= 1; }
            format!("{}...", &issue_description[..end])
        } else {
            issue_description.to_string()
        };

        UnifiedGraphResult {
            issue_id: issue_id.to_string(),
            description,
            nodes,
            edges,
        }
    }

    /// Refine call edges using LSP servers for precise definition resolution.
    ///
    /// For each call edge with confidence < 1.0, queries the language server's
    /// `textDocument/definition` to resolve the exact target. This replaces
    /// name-matching heuristics with compiler-level precision.
    ///
    /// Requires language servers to be installed (tsserver, rust-analyzer, pyright).
    /// Falls back gracefully: if no LSP is available for a language, keeps the
    /// tree-sitter edges with their original confidence.
    pub fn refine_with_lsp(
        &mut self,
        root_dir: &Path,
    ) -> anyhow::Result<crate::lsp_client::LspRefinementStats> {
        use crate::lsp_client::*;

        let mut stats = LspRefinementStats::default();

        // Find the actual project root by looking for config files
        // The user passes the source directory (e.g., src/), but LSP needs the project root
        // (where tsconfig.json, package.json, Cargo.toml, etc. live)
        let project_root = find_project_root(root_dir);
        let extract_dir = root_dir.canonicalize().unwrap_or_else(|_| root_dir.to_path_buf());
        let project_root_canon = project_root.canonicalize().unwrap_or_else(|_| project_root.clone());

        // Compute prefix: if extract_dir is a subdirectory of project_root, this is the relative path
        // e.g., project_root=/tmp/project, extract_dir=/tmp/project/src → prefix = "src"
        let dir_prefix = extract_dir
            .strip_prefix(&project_root_canon)
            .ok()
            .and_then(|p| {
                let s = p.to_string_lossy().to_string();
                if s.is_empty() { None } else { Some(s) }
            });

        // Detect available language servers
        let configs = LspServerConfig::detect_available();
        if configs.is_empty() {
            // No LSP servers available; mark all call edges as skipped
            stats.skipped = self
                .edges
                .iter()
                .filter(|e| e.relation == EdgeRelation::Calls)
                .count();
            stats.total_call_edges = stats.skipped;
            return Ok(stats);
        }

        // Build definition target index: (file_path, line) → node_id
        let def_index = build_definition_target_index(&self.nodes);

        // Collect file contents by language (we need them to open in LSP)
        // file_path in nodes is relative to extract dir (e.g., "math.ts")
        // For LSP, we need paths relative to project root (e.g., "src/math.ts")
        let to_lsp_path = |graph_path: &str| -> String {
            match &dir_prefix {
                Some(prefix) => format!("{}/{}", prefix, graph_path),
                None => graph_path.to_string(),
            }
        };
        let from_lsp_path = |lsp_path: &str| -> String {
            match &dir_prefix {
                Some(prefix) => {
                    let prefix_slash = format!("{}/", prefix);
                    lsp_path.strip_prefix(&prefix_slash).unwrap_or(lsp_path).to_string()
                }
                None => lsp_path.to_string(),
            }
        };

        let mut files_by_lang: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for node in &self.nodes {
            if node.kind == NodeKind::File {
                let ext = node
                    .file_path
                    .rsplit('.')
                    .next()
                    .unwrap_or("");
                let lang_id = extension_to_language_id(ext).to_string();

                if !files_by_lang.contains_key(&lang_id) || lang_id != "plaintext" {
                    let full_path = root_dir.join(&node.file_path);
                    if let Ok(content) = std::fs::read_to_string(&full_path) {
                        // Store with graph-relative path (node.file_path), content
                        files_by_lang
                            .entry(lang_id)
                            .or_default()
                            .push((node.file_path.clone(), content));
                    }
                }
            }
        }

        // Group call edges by source language for batch processing
        let call_edge_indices: Vec<usize> = self
            .edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.relation == EdgeRelation::Calls)
            .map(|(i, _)| i)
            .collect();

        stats.total_call_edges = call_edge_indices.len();

        // Process each language that has a server config
        for config in &configs {
            let lang_id = &config.language_id;

            // Find call edges for this language
            let lang_edge_indices: Vec<usize> = call_edge_indices
                .iter()
                .filter(|&&idx| {
                    let edge = &self.edges[idx];
                    let caller_file = self
                        .node_by_id(&edge.from)
                        .map(|n| &n.file_path)
                        .unwrap_or(&String::new())
                        .clone();
                    let ext = caller_file.rsplit('.').next().unwrap_or("");
                    let edge_lang = extension_to_language_id(ext);
                    // TS/JS share tsserver
                    (edge_lang == lang_id.as_str())
                        || (lang_id == "typescript"
                            && (edge_lang == "javascript" || edge_lang == "typescript"))
                })
                .copied()
                .collect();


            if lang_edge_indices.is_empty() {
                continue;
            }

            // Start LSP server
            let mut client = match LspClient::start(config, &project_root) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("[LSP] Failed to start {} server: {}", lang_id, e);
                    stats.failed += lang_edge_indices.len();
                    continue;
                }
            };

            stats.languages_used.push(lang_id.clone());

            // Open all files for this language
            if let Some(files) = files_by_lang.get(lang_id) {
                for (path, content) in files {
                    let lsp_path = to_lsp_path(path);
                    let ext = path.rsplit('.').next().unwrap_or("");
                    let file_lang = extension_to_language_id(ext);
                    if let Err(e) = client.open_file(&lsp_path, content, file_lang) {
                        tracing::warn!("[LSP] Failed to open {}: {}", lsp_path, e);
                    }
                }
            }
            // Also open JS files if we're using tsserver
            if lang_id == "typescript" {
                if let Some(files) = files_by_lang.get("javascript") {
                    for (path, content) in files {
                        let lsp_path = to_lsp_path(path);
                        if let Err(e) = client.open_file(&lsp_path, content, "javascript") {
                            tracing::warn!("[LSP] Failed to open {}: {}", lsp_path, e);
                        }
                    }
                }
            }

            // Give LSP a moment to index
            std::thread::sleep(std::time::Duration::from_millis(2000));

            // Build source content map for finding call sites
            let mut source_map: HashMap<String, String> = HashMap::new();
            if let Some(files) = files_by_lang.get(lang_id) {
                for (path, content) in files {
                    source_map.insert(path.clone(), content.clone());
                }
            }
            if lang_id == "typescript" {
                if let Some(files) = files_by_lang.get("javascript") {
                    for (path, content) in files {
                        source_map.insert(path.clone(), content.clone());
                    }
                }
            }

            // Process each call edge
            let mut edges_to_update: Vec<(usize, Option<String>, f32)> = Vec::new();
            let mut edges_to_remove: Vec<usize> = Vec::new();

            for &idx in &lang_edge_indices {
                let edge = &self.edges[idx];

                // Skip already high-confidence edges
                if edge.confidence >= 0.95 {
                    continue;
                }

                // Find call site position
                let (file_path, call_line, call_col) =
                    if let (Some(line), Some(col)) = (edge.call_site_line, edge.call_site_column) {
                        // Have exact position from extraction
                        let caller = self.node_by_id(&edge.from);
                        let fp = caller.map(|n| n.file_path.clone()).unwrap_or_default();
                        (fp, line, col)
                    } else {
                        // Need to find position by searching source
                        let caller = match self.node_by_id(&edge.from) {
                            Some(n) => n,
                            None => {
                                stats.failed += 1;
                                continue;
                            }
                        };
                        let source = match source_map.get(&caller.file_path) {
                            Some(s) => s,
                            None => {
                                stats.failed += 1;
                                continue;
                            }
                        };

                        // Extract callee name from edge.to (format: func:path:name or method:path:Class.method)
                        let raw_callee = edge
                            .to
                            .rsplit(':')
                            .next()
                            .unwrap_or(&edge.to);
                        
                        // For method calls like "Calculator.add", extract just the method name
                        let callee_name = if raw_callee.contains('.') {
                            raw_callee.rsplit('.').next().unwrap_or(raw_callee)
                        } else {
                            raw_callee
                        };

                        // Search for callee name in the caller's line range
                        let caller_start = caller.line.unwrap_or(0);
                        let caller_end = caller_start + caller.line_count;

                        let mut found_pos = None;
                        for (line_idx, line_text) in source.lines().enumerate() {
                            let line_num = line_idx; // 0-indexed
                            if line_num >= caller_start && line_num <= caller_end {
                                // Search for the callee name as a function call
                                if let Some(col_pos) = find_call_position(line_text, callee_name) {
                                    found_pos = Some((line_num as u32, col_pos as u32));
                                    break;
                                }
                            }
                        }

                        match found_pos {
                            Some((line, col)) => (caller.file_path.clone(), line, col),
                            None => {
                                // Can't find call site in source
                                stats.failed += 1;
                                continue;
                            }
                        }
                    };

                // Query LSP for definition (convert graph path to LSP path)
                let lsp_file_path = to_lsp_path(&file_path);
                match client.get_definition(&lsp_file_path, call_line, call_col) {
                    Ok(Some(location)) => {
                        // LSP found a definition in our project
                        // Convert LSP path back to graph-relative path
                        let graph_file_path = from_lsp_path(&location.file_path);
                        // Try to match to a known node
                        if let Some(file_index) = def_index.get(&graph_file_path) {
                            if let Some(target_id) =
                                find_closest_node(file_index, location.line, 5)
                            {
                                // Update edge target and confidence
                                edges_to_update.push((idx, Some(target_id), 1.0));
                                stats.refined += 1;
                            } else {
                                // Definition found but doesn't match any known node
                                // Keep original edge but note it was checked
                                edges_to_update.push((idx, None, edge.confidence.max(0.6)));
                                stats.refined += 1;
                            }
                        } else {
                            // Definition in an unindexed file
                            edges_to_update.push((idx, None, edge.confidence.max(0.6)));
                            stats.refined += 1;
                        }
                    }
                    Ok(None) => {
                        // Definition is outside project (stdlib, node_modules, etc.)
                        edges_to_remove.push(idx);
                        stats.removed += 1;
                    }
                    Err(e) => {
                        tracing::debug!("[LSP] definition failed for {}:{},{}: {}", file_path, call_line, call_col, e);
                        stats.failed += 1;
                    }
                }
            }

            // Apply updates
            for (idx, new_target, new_confidence) in edges_to_update {
                if let Some(target) = new_target {
                    self.edges[idx].to = target;
                }
                self.edges[idx].confidence = new_confidence;
            }

            // Remove external/false-positive edges (reverse order to maintain indices)
            edges_to_remove.sort_unstable();
            edges_to_remove.dedup();
            for &idx in edges_to_remove.iter().rev() {
                self.edges.remove(idx);
            }

            // Shutdown LSP
            if let Err(e) = client.shutdown() {
                tracing::debug!("LSP shutdown error: {}", e);
            }
        }

        // Count skipped (languages with no LSP)
        let handled_langs: std::collections::HashSet<&str> =
            configs.iter().flat_map(|c| c.extensions.iter().map(|e| e.as_str())).collect();
        stats.skipped = call_edge_indices
            .iter()
            .filter(|&&idx| {
                if idx >= self.edges.len() {
                    return false;
                }
                let edge = &self.edges[idx];
                let caller_file = self
                    .node_by_id(&edge.from)
                    .map(|n| &n.file_path)
                    .unwrap_or(&String::new())
                    .clone();
                let ext = caller_file.rsplit('.').next().unwrap_or("");
                !handled_langs.contains(ext)
            })
            .count();

        // Rebuild adjacency indexes
        self.build_indexes();

        Ok(stats)
    }
}

/// Walk up from `dir` to find the project root by looking for config files.
/// Looks for: tsconfig.json, package.json, Cargo.toml, pyproject.toml, .git
fn find_project_root(dir: &Path) -> std::path::PathBuf {
    let markers = ["tsconfig.json", "package.json", "Cargo.toml", "pyproject.toml", ".git"];
    let abs_dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());

    let mut current = abs_dir.as_path();
    loop {
        for marker in &markers {
            if current.join(marker).exists() {
                return current.to_path_buf();
            }
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent,
            _ => break,
        }
    }
    // Fallback: use the original directory
    abs_dir
}

/// Find the column position of a function call in a line of source code.
/// Looks for patterns like `callee_name(` or `.callee_name(`.
fn find_call_position(line: &str, callee_name: &str) -> Option<usize> {
    // Look for `callee_name(` pattern
    let pattern = format!("{}(", callee_name);
    if let Some(pos) = line.find(&pattern) {
        return Some(pos);
    }

    // Look for `.callee_name(` pattern (method call)
    let dot_pattern = format!(".{}(", callee_name);
    if let Some(pos) = line.find(&dot_pattern) {
        // Return position of the method name, not the dot
        return Some(pos + 1);
    }

    None
}

/// Result of build_unified_graph — a simplified graph structure for task planning
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedGraphResult {
    pub issue_id: String,
    pub description: String,
    pub nodes: Vec<UnifiedNode>,
    pub edges: Vec<UnifiedEdge>,
}

/// A node in the unified graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedNode {
    pub id: String,
    pub node_type: String,
    pub layer: String,
    pub description: String,
    pub path: Option<String>,
    pub line: Option<usize>,
    pub code: Option<String>,
}

/// An edge in the unified graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedEdge {
    pub from: String,
    pub to: String,
    pub relation: String,
}

// ═══ Tree-sitter extraction helpers ═══

fn collect_decorators(node: tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut decorators = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "decorator" {
            let dec_text = child.utf8_text(source).unwrap_or("").trim().to_string();
            let name = dec_text.trim_start_matches('@');
            let name = name.split('(').next().unwrap_or(name).trim();
            if !name.is_empty() {
                decorators.push(name.to_string());
            }
        }
    }
    decorators
}

fn extract_docstring(node: tree_sitter::Node, source: &str) -> Option<String> {
    let body = node.child_by_field_name("body")?;
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() == "comment" {
            continue;
        }
        if child.kind() == "expression_statement" {
            if let Some(str_node) = child.child(0) {
                if str_node.kind() == "string" || str_node.kind() == "concatenated_string" {
                    if str_node.start_byte() < source.len() && str_node.end_byte() <= source.len() {
                        let doc_text = &source[str_node.start_byte()..str_node.end_byte()];
                        let doc_clean = doc_text
                            .trim_start_matches("\"\"\"")
                            .trim_end_matches("\"\"\"")
                            .trim_start_matches("'''")
                            .trim_end_matches("'''")
                            .trim_start_matches('"')
                            .trim_end_matches('"')
                            .trim_start_matches('\'')
                            .trim_end_matches('\'')
                            .trim();
                        let first_line = doc_clean.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                        if first_line.is_empty() {
                            return None;
                        }
                        let truncated = if first_line.len() > 100 {
                            let mut end = 100;
                            while end > 0 && !first_line.is_char_boundary(end) {
                                end -= 1;
                            }
                            &first_line[..end]
                        } else {
                            first_line
                        };
                        return Some(truncated.to_string());
                    }
                }
            }
        }
        break;
    }
    None
}

fn is_in_error_path(node: &tree_sitter::Node, source: &[u8]) -> bool {
    let source_str = std::str::from_utf8(source).unwrap_or("");
    let mut current = node.parent();
    let mut levels = 0;
    while let Some(parent) = current {
        levels += 1;
        if levels > 10 {
            break;
        }
        match parent.kind() {
            "except_clause" | "raise_statement" => return true,
            "try_statement" => return true,
            "if_statement" => {
                if let Some(cond) = parent.child_by_field_name("condition") {
                    if cond.start_byte() < source_str.len() && cond.end_byte() <= source_str.len() {
                        let cond_text = &source_str[cond.start_byte()..cond.end_byte()];
                        let lower = cond_text.to_lowercase();
                        if lower.contains("error")
                            || lower.contains("exception")
                            || lower.contains("err")
                            || lower.contains("fail")
                            || lower.contains("none")
                        {
                            return true;
                        }
                    }
                }
            }
            _ => {}
        }
        current = parent.parent();
    }
    false
}

/// Extract Python code using tree-sitter AST parsing
fn extract_python_tree_sitter(
    path: &str,
    content: &str,
    parser: &mut Parser,
    class_id_map: &mut HashMap<String, String>,
) -> (Vec<CodeNode>, Vec<CodeEdge>, HashSet<String>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut imports = HashSet::new();

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return (nodes, edges, imports),
    };

    let file_id = format!("file:{}", path);
    let source = content.as_bytes();
    let root = tree.root_node();

    let text = |node: tree_sitter::Node| -> String {
        node.utf8_text(source).unwrap_or("").to_string()
    };

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        match child.kind() {
            "class_definition" => {
                extract_class_node(
                    child,
                    source,
                    content,
                    path,
                    &file_id,
                    &[],
                    &mut nodes,
                    &mut edges,
                    class_id_map,
                );
            }
            "function_definition" => {
                extract_function_node(child, source, content, path, &file_id, &[], &mut nodes, &mut edges);
            }
            "decorated_definition" => {
                let decorators = collect_decorators(child, source);
                let mut inner_cursor = child.walk();
                for inner in child.children(&mut inner_cursor) {
                    match inner.kind() {
                        "class_definition" => {
                            extract_class_node(
                                inner,
                                source,
                                content,
                                path,
                                &file_id,
                                &decorators,
                                &mut nodes,
                                &mut edges,
                                class_id_map,
                            );
                        }
                        "function_definition" => {
                            extract_function_node(
                                inner, source, content, path, &file_id, &decorators, &mut nodes, &mut edges,
                            );
                        }
                        _ => {}
                    }
                }
            }
            "import_statement" => {
                let import_text = text(child);
                let re_import = Regex::new(r"import\s+([\w.]+)").unwrap();
                if let Some(cap) = re_import.captures(&import_text) {
                    let module = cap[1].to_string();
                    if !is_stdlib(&module) {
                        edges.push(CodeEdge {
                            from: file_id.clone(),
                            to: format!("module_ref:{}", module),
                            relation: EdgeRelation::Imports,
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
            "import_from_statement" => {
                let mut mod_cursor = child.walk();
                for mod_child in child.children(&mut mod_cursor) {
                    if mod_child.kind() == "dotted_name" {
                        let module = text(mod_child);
                        if !is_stdlib(&module) {
                            edges.push(CodeEdge {
                                from: file_id.clone(),
                                to: format!("module_ref:{}", module),
                                relation: EdgeRelation::Imports,
                                weight: 0.5,
                                call_count: 1,
                                in_error_path: false,
                                confidence: 1.0,
                                call_site_line: None,
                                call_site_column: None,
                            });
                        }
                        break;
                    }
                    if mod_child.kind() == "relative_import" {
                        let rel_import_text = text(mod_child);
                        let trimmed = rel_import_text.trim_start_matches('.');
                        if !trimmed.is_empty() && !is_stdlib(trimmed) {
                            edges.push(CodeEdge {
                                from: file_id.clone(),
                                to: format!("module_ref:{}", trimmed),
                                relation: EdgeRelation::Imports,
                                weight: 0.5,
                                call_count: 1,
                                in_error_path: false,
                                confidence: 1.0,
                                call_site_line: None,
                                call_site_column: None,
                            });
                        }
                        break;
                    }
                }

                // Extract imported names
                let import_text = child.utf8_text(source).unwrap_or("");
                if let Some(after_import) = import_text.split(" import ").nth(1) {
                    for name in after_import.split(',') {
                        let clean = name.trim().split(" as ").next().unwrap_or("").trim();
                        if !clean.is_empty() && clean != "*" && clean != "(" && clean != ")" {
                            imports.insert(clean.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    (nodes, edges, imports)
}

fn extract_class_node(
    node: tree_sitter::Node,
    source: &[u8],
    source_str: &str,
    path: &str,
    file_id: &str,
    decorators: &[String],
    nodes: &mut Vec<CodeNode>,
    edges: &mut Vec<CodeEdge>,
    class_id_map: &mut HashMap<String, String>,
) {
    let class_name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("")
        .to_string();

    if class_name.is_empty() {
        return;
    }

    let line_num = node.start_position().row + 1;
    let class_id = format!("class:{}:{}", path, class_name);

    let class_sig = {
        let sig_text = &source_str[node.start_byte()..];
        let sig_end = sig_text
            .find(":\n")
            .or_else(|| sig_text.find(":\r"))
            .unwrap_or(sig_text.len().min(200));
        Some(sig_text[..sig_end].trim().to_string())
    };

    let class_docstring = extract_docstring(node, source_str);
    let class_line_count = node.end_position().row - node.start_position().row + 1;
    let class_is_test =
        path.contains("/tests/") || path.contains("/test_") || class_name.starts_with("Test");

    nodes.push(CodeNode {
        id: class_id.clone(),
        kind: NodeKind::Class,
        name: class_name.clone(),
        file_path: path.to_string(),
        line: Some(line_num),
        decorators: decorators.to_vec(),
        signature: class_sig,
        docstring: class_docstring,
        line_count: class_line_count,
        is_test: class_is_test,
    });

    edges.push(CodeEdge {
        from: class_id.clone(),
        to: file_id.to_string(),
        relation: EdgeRelation::DefinedIn,
        weight: 0.5,
        call_count: 1,
        in_error_path: false,
        confidence: 1.0,
        call_site_line: None,
        call_site_column: None,
    });

    class_id_map.insert(class_name.clone(), class_id.clone());

    // Inheritance
    if let Some(superclasses) = node.child_by_field_name("superclasses") {
        let mut sc_cursor = superclasses.walk();
        for sc_child in superclasses.children(&mut sc_cursor) {
            let kind = sc_child.kind();
            if kind == "identifier" || kind == "attribute" {
                let parent_text = sc_child.utf8_text(source).unwrap_or("");
                let parent_name = parent_text.split('.').last().unwrap_or("").trim();
                if !parent_name.is_empty() && parent_name != "object" {
                    edges.push(CodeEdge {
                        from: class_id.clone(),
                        to: format!("class_ref:{}", parent_name),
                        relation: EdgeRelation::Inherits,
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

    // Extract methods
    if let Some(body) = node.child_by_field_name("body") {
        let mut body_cursor = body.walk();
        for body_child in body.children(&mut body_cursor) {
            match body_child.kind() {
                "function_definition" => {
                    extract_method_node(body_child, source, source_str, path, &class_id, &[], nodes, edges);
                }
                "decorated_definition" => {
                    let method_decorators = collect_decorators(body_child, source);
                    let mut inner_cursor = body_child.walk();
                    for inner in body_child.children(&mut inner_cursor) {
                        if inner.kind() == "function_definition" {
                            extract_method_node(
                                inner,
                                source,
                                source_str,
                                path,
                                &class_id,
                                &method_decorators,
                                nodes,
                                edges,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn extract_method_node(
    node: tree_sitter::Node,
    source: &[u8],
    source_str: &str,
    path: &str,
    class_id: &str,
    decorators: &[String],
    nodes: &mut Vec<CodeNode>,
    edges: &mut Vec<CodeEdge>,
) {
    let func_name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("")
        .to_string();

    if func_name.is_empty() {
        return;
    }

    let line_num = node.start_position().row + 1;
    // Include parent class name in method ID to avoid collisions
    let parent_name = class_id.rsplit(':').next().unwrap_or("");
    let method_id = if parent_name.is_empty() {
        format!("method:{}:{}", path, func_name)
    } else {
        format!("method:{}:{}.{}", path, parent_name, func_name)
    };

    let signature = {
        let sig_text = &source_str[node.start_byte()..];
        let sig_end = sig_text
            .find(":\n")
            .or_else(|| sig_text.find(":\r"))
            .unwrap_or(sig_text.len().min(200));
        Some(sig_text[..sig_end].trim().to_string())
    };
    let docstring = extract_docstring(node, source_str);
    let line_count = node.end_position().row - node.start_position().row + 1;
    let is_test = path.contains("/tests/")
        || path.contains("/test_")
        || func_name.starts_with("test_")
        || func_name.starts_with("Test");

    nodes.push(CodeNode {
        id: method_id.clone(),
        kind: NodeKind::Function,
        name: func_name,
        file_path: path.to_string(),
        line: Some(line_num),
        decorators: decorators.to_vec(),
        signature,
        docstring,
        line_count,
        is_test,
    });

    edges.push(CodeEdge {
        from: method_id,
        to: class_id.to_string(),
        relation: EdgeRelation::DefinedIn,
        weight: 0.5,
        call_count: 1,
        in_error_path: false,
        confidence: 1.0,
        call_site_line: None,
        call_site_column: None,
    });
}

fn extract_function_node(
    node: tree_sitter::Node,
    source: &[u8],
    source_str: &str,
    path: &str,
    file_id: &str,
    decorators: &[String],
    nodes: &mut Vec<CodeNode>,
    edges: &mut Vec<CodeEdge>,
) {
    let func_name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("")
        .to_string();

    if func_name.is_empty() {
        return;
    }

    let line_num = node.start_position().row + 1;
    let func_id = format!("func:{}:{}", path, func_name);

    let signature = {
        let sig_text = &source_str[node.start_byte()..];
        let sig_end = sig_text
            .find(":\n")
            .or_else(|| sig_text.find(":\r"))
            .unwrap_or(sig_text.len().min(200));
        Some(sig_text[..sig_end].trim().to_string())
    };
    let docstring = extract_docstring(node, source_str);
    let line_count = node.end_position().row - node.start_position().row + 1;
    let is_test = path.contains("/tests/")
        || path.contains("/test_")
        || func_name.starts_with("test_")
        || func_name.starts_with("Test");

    nodes.push(CodeNode {
        id: func_id.clone(),
        kind: NodeKind::Function,
        name: func_name,
        file_path: path.to_string(),
        line: Some(line_num),
        decorators: decorators.to_vec(),
        signature,
        docstring,
        line_count,
        is_test,
    });

    edges.push(CodeEdge {
        from: func_id,
        to: file_id.to_string(),
        relation: EdgeRelation::DefinedIn,
        weight: 0.5,
        call_count: 1,
        in_error_path: false,
        confidence: 1.0,
        call_site_line: None,
        call_site_column: None,
    });
}

/// Extract call edges from tree-sitter AST
fn extract_calls_from_tree(
    root: tree_sitter::Node,
    source: &[u8],
    rel_path: &str,
    func_name_map: &HashMap<String, Vec<String>>,
    method_to_class: &HashMap<String, String>,
    class_parents: &HashMap<String, Vec<String>>,
    file_func_ids: &HashSet<String>,
    file_imported_names: &HashMap<String, HashSet<String>>,
    package_dir: &str,
    class_init_map: &HashMap<String, Vec<(String, String)>>,
    node_pkg_map: &HashMap<String, String>,
    edges: &mut Vec<CodeEdge>,
) {
    // Build scope map
    let mut scope_map: Vec<(usize, usize, String, Option<String>)> = Vec::new();
    build_scope_map(root, source, rel_path, &mut scope_map);

    // Walk tree looking for calls
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "string"
            || node.kind() == "comment"
            || node.kind() == "string_content"
            || node.kind() == "concatenated_string"
        {
            continue;
        }

        if node.kind() == "call" {
            let call_line = node.start_position().row + 1;
            let error_path = is_in_error_path(&node, source);

            let scope = scope_map
                .iter()
                .filter(|(start, end, _, _)| call_line >= *start && call_line <= *end)
                .max_by_key(|(start, _, _, _)| *start);

            if let Some((_start, _end, caller_id, caller_class)) = scope {
                if let Some(function_node) = node.child_by_field_name("function") {
                    let edges_before = edges.len();
                    match function_node.kind() {
                        "identifier" => {
                            let callee_name = function_node.utf8_text(source).unwrap_or("");
                            if !callee_name.is_empty() && !is_python_builtin(callee_name) {
                                resolve_and_add_call_edge(
                                    caller_id,
                                    callee_name,
                                    func_name_map,
                                    file_func_ids,
                                    file_imported_names,
                                    rel_path,
                                    package_dir,
                                    class_init_map,
                                    node_pkg_map,
                                    false,
                                    edges,
                                );
                            }
                        }
                        "attribute" => {
                            let obj_node = function_node.child_by_field_name("object");
                            let attr_node = function_node.child_by_field_name("attribute");

                            if let (Some(obj), Some(attr)) = (obj_node, attr_node) {
                                let obj_text = obj.utf8_text(source).unwrap_or("");
                                let method_name = attr.utf8_text(source).unwrap_or("");

                                if (obj_text == "self" || obj_text == "cls") && !method_name.is_empty() {
                                    resolve_self_method_call(
                                        caller_id,
                                        method_name,
                                        caller_class.as_deref(),
                                        func_name_map,
                                        method_to_class,
                                        class_parents,
                                        file_func_ids,
                                        edges,
                                    );
                                } else if !method_name.is_empty() && !is_python_builtin(method_name) {
                                    resolve_and_add_call_edge(
                                        caller_id,
                                        method_name,
                                        func_name_map,
                                        file_func_ids,
                                        file_imported_names,
                                        rel_path,
                                        package_dir,
                                        class_init_map,
                                        node_pkg_map,
                                        true,
                                        edges,
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                    if error_path {
                        for edge in edges[edges_before..].iter_mut() {
                            edge.in_error_path = true;
                        }
                    }
                }
            }
        }

        let child_count = node.child_count();
        for i in (0..child_count).rev() {
            if let Some(child) = node.child(i) {
                stack.push(child);
            }
        }
    }
}

fn build_scope_map(
    node: tree_sitter::Node,
    source: &[u8],
    rel_path: &str,
    scope_map: &mut Vec<(usize, usize, String, Option<String>)>,
) {
    let mut stack: Vec<(tree_sitter::Node, Option<String>)> = vec![(node, None)];

    while let Some((current, class_ctx)) = stack.pop() {
        match current.kind() {
            "class_definition" => {
                let class_name = current
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");
                let class_id = if !class_name.is_empty() {
                    Some(format!("class:{}:{}", rel_path, class_name))
                } else {
                    class_ctx.clone()
                };

                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, class_id.clone()));
                    }
                }
            }
            "function_definition" => {
                let func_name = current
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");

                if !func_name.is_empty() {
                    let start_line = current.start_position().row + 1;
                    let end_line = current.end_position().row + 1;

                    let func_id = if let Some(ref cls) = class_ctx {
                        let cls_name = cls.rsplit(':').next().unwrap_or("");
                        if cls_name.is_empty() {
                            format!("method:{}:{}", rel_path, func_name)
                        } else {
                            format!("method:{}:{}.{}", rel_path, cls_name, func_name)
                        }
                    } else {
                        format!("func:{}:{}", rel_path, func_name)
                    };

                    scope_map.push((start_line, end_line, func_id, class_ctx.clone()));
                }

                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, class_ctx.clone()));
                    }
                }
            }
            "decorated_definition" => {
                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, class_ctx.clone()));
                    }
                }
            }
            _ => {
                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, class_ctx.clone()));
                    }
                }
            }
        }
    }
}

fn is_common_dunder(name: &str) -> bool {
    matches!(
        name,
        "__init__"
            | "__str__"
            | "__repr__"
            | "__eq__"
            | "__ne__"
            | "__hash__"
            | "__len__"
            | "__iter__"
            | "__next__"
            | "__getitem__"
            | "__setitem__"
            | "__delitem__"
            | "__contains__"
            | "__call__"
            | "__enter__"
            | "__exit__"
            | "__get__"
            | "__set__"
            | "__delete__"
            | "__getattr__"
            | "__setattr__"
            | "__bool__"
            | "__lt__"
            | "__le__"
            | "__gt__"
            | "__ge__"
            | "__add__"
            | "__sub__"
            | "__mul__"
            | "__new__"
            | "__del__"
            | "__format__"
            | "get"
            | "set"
            | "update"
            | "delete"
            | "save"
            | "clean"
            | "run"
            | "setup"
            | "teardown"
    )
}

fn resolve_and_add_call_edge(
    caller_id: &str,
    callee_name: &str,
    func_name_map: &HashMap<String, Vec<String>>,
    file_func_ids: &HashSet<String>,
    file_imported_names: &HashMap<String, HashSet<String>>,
    rel_path: &str,
    package_dir: &str,
    class_init_map: &HashMap<String, Vec<(String, String)>>,
    node_pkg_map: &HashMap<String, String>,
    is_attribute_call: bool,
    edges: &mut Vec<CodeEdge>,
) {
    if let Some(callee_ids) = func_name_map.get(callee_name) {
        let same_file: Vec<&String> = callee_ids
            .iter()
            .filter(|id| file_func_ids.contains(*id))
            .collect();
        let imported: Vec<&String> = callee_ids
            .iter()
            .filter(|_id| {
                file_imported_names
                    .get(rel_path)
                    .map(|names| names.contains(callee_name))
                    .unwrap_or(false)
            })
            .collect();
        let same_pkg: Vec<&String> = callee_ids
            .iter()
            .filter(|id| {
                node_pkg_map
                    .get(id.as_str())
                    .map(|pkg| pkg == package_dir)
                    .unwrap_or(false)
            })
            .collect();

        let global_limit = if is_attribute_call && !is_common_dunder(callee_name) {
            20
        } else {
            3
        };

        let confidence = if !same_file.is_empty() {
            0.8_f32
        } else if !imported.is_empty() {
            0.8
        } else if !same_pkg.is_empty() {
            0.7
        } else if is_attribute_call {
            0.3
        } else {
            0.5
        };

        let weight = if !same_file.is_empty() || !imported.is_empty() || !same_pkg.is_empty() {
            0.5
        } else if is_attribute_call {
            0.8
        } else {
            0.5
        };

        let targets = if !same_file.is_empty() {
            same_file
        } else if !imported.is_empty() {
            imported
        } else if !same_pkg.is_empty() {
            same_pkg
        } else if callee_ids.len() <= global_limit {
            callee_ids.iter().collect()
        } else {
            vec![]
        };

        for callee_id in targets {
            if callee_id != caller_id {
                edges.push(CodeEdge {
                    from: caller_id.to_string(),
                    to: callee_id.clone(),
                    relation: EdgeRelation::Calls,
                    weight,
                    call_count: 1,
                    in_error_path: false,
                    confidence,
                    call_site_line: None,
                    call_site_column: None,
                });
            }
        }
    } else if callee_name
        .chars()
        .next()
        .map(|c| c.is_uppercase())
        .unwrap_or(false)
    {
        // Constructor call
        if let Some(init_entries) = class_init_map.get(callee_name) {
            let same_file: Vec<&str> = init_entries
                .iter()
                .filter(|(fp, _)| fp == rel_path)
                .map(|(_, id)| id.as_str())
                .collect();
            let is_imported = file_imported_names
                .get(rel_path)
                .map(|names| names.contains(callee_name))
                .unwrap_or(false);
            let imported: Vec<&str> = if is_imported {
                init_entries.iter().map(|(_, id)| id.as_str()).collect()
            } else {
                vec![]
            };
            let same_pkg: Vec<&str> = init_entries
                .iter()
                .filter(|(fp, _)| fp.rsplitn(2, '/').nth(1).unwrap_or("") == package_dir)
                .map(|(_, id)| id.as_str())
                .collect();

            let (targets, confidence): (Vec<&str>, f32) = if !same_file.is_empty() {
                (same_file, 0.8)
            } else if !imported.is_empty() {
                (imported, 0.7)
            } else if !same_pkg.is_empty() {
                (same_pkg, 0.6)
            } else if init_entries.len() <= 3 {
                (init_entries.iter().map(|(_, id)| id.as_str()).collect(), 0.5)
            } else {
                (vec![], 0.0)
            };

            for init_id in targets {
                if init_id != caller_id {
                    edges.push(CodeEdge {
                        from: caller_id.to_string(),
                        to: init_id.to_string(),
                        relation: EdgeRelation::Calls,
                        weight: 0.5,
                        call_count: 1,
                        in_error_path: false,
                        confidence,
                        call_site_line: None,
                        call_site_column: None,
                    });
                }
            }
        }
    }
}

fn resolve_self_method_call(
    caller_id: &str,
    method_name: &str,
    caller_class: Option<&str>,
    func_name_map: &HashMap<String, Vec<String>>,
    method_to_class: &HashMap<String, String>,
    class_parents: &HashMap<String, Vec<String>>,
    file_func_ids: &HashSet<String>,
    edges: &mut Vec<CodeEdge>,
) {
    if let Some(callee_ids) = func_name_map.get(method_name) {
        if let Some(class_id) = caller_class {
            let mut valid_classes = vec![class_id.to_string()];
            if let Some(parents) = class_parents.get(class_id) {
                valid_classes.extend(parents.iter().cloned());
            }

            let scoped: Vec<&String> = callee_ids
                .iter()
                .filter(|id| {
                    method_to_class
                        .get(*id)
                        .map(|cls| valid_classes.contains(cls))
                        .unwrap_or(false)
                })
                .collect();

            let targets = if !scoped.is_empty() {
                scoped
            } else if callee_ids.len() <= 3 {
                callee_ids.iter().collect()
            } else {
                callee_ids
                    .iter()
                    .filter(|id| file_func_ids.contains(*id))
                    .collect()
            };

            for callee_id in targets {
                if callee_id != caller_id {
                    edges.push(CodeEdge {
                        from: caller_id.to_string(),
                        to: callee_id.clone(),
                        relation: EdgeRelation::Calls,
                        weight: 0.5,
                        call_count: 1,
                        in_error_path: false,
                        confidence: 0.9,
                        call_site_line: None,
                        call_site_column: None,
                    });
                }
            }
        } else {
            for callee_id in callee_ids {
                if callee_id != caller_id && file_func_ids.contains(callee_id) {
                    edges.push(CodeEdge {
                        from: caller_id.to_string(),
                        to: callee_id.clone(),
                        relation: EdgeRelation::Calls,
                        weight: 0.5,
                        call_count: 1,
                        in_error_path: false,
                        confidence: 0.6,
                        call_site_line: None,
                        call_site_column: None,
                    });
                }
            }
        }
    }
}

fn add_override_edges(nodes: &[CodeNode], edges: &mut Vec<CodeEdge>) {
    let mut class_methods: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for edge in edges.iter() {
        if edge.relation == EdgeRelation::DefinedIn && edge.to.starts_with("class:") {
            if let Some(method) = nodes.iter().find(|n| n.id == edge.from && n.kind == NodeKind::Function) {
                class_methods
                    .entry(edge.to.clone())
                    .or_default()
                    .push((method.name.clone(), method.id.clone()));
            }
        }
    }

    let inherits_pairs: Vec<(String, String)> = edges
        .iter()
        .filter(|e| e.relation == EdgeRelation::Inherits)
        .map(|e| (e.from.clone(), e.to.clone()))
        .collect();

    let mut new_edges = Vec::new();
    for (sub_class_id, base_class_id) in &inherits_pairs {
        let sub_methods = match class_methods.get(sub_class_id) {
            Some(m) => m,
            None => continue,
        };
        let base_methods = match class_methods.get(base_class_id) {
            Some(m) => m,
            None => continue,
        };

        for (sub_name, sub_id) in sub_methods {
            for (base_name, base_id) in base_methods {
                if sub_name == base_name && sub_id != base_id {
                    new_edges.push(CodeEdge {
                        from: base_id.clone(),
                        to: sub_id.clone(),
                        relation: EdgeRelation::Overrides,
                        weight: 0.4,
                        call_count: 1,
                        in_error_path: false,
                        confidence: 0.6,
                        call_site_line: None,
                        call_site_column: None,
                    });
                }
            }
        }
    }

    edges.extend(new_edges);
}

// ═══ Language-Specific Extractors (Rust, TypeScript) ═══

// ─── Rust Tree-Sitter Extraction ───

/// Extract from Rust source using tree-sitter AST parsing.
/// Handles structs, enums, traits, impl blocks, functions, modules, and type aliases.
fn extract_rust_tree_sitter(
    path: &str,
    content: &str,
    parser: &mut Parser,
    class_id_map: &mut HashMap<String, String>,
) -> (Vec<CodeNode>, Vec<CodeEdge>, HashSet<String>, HashMap<String, HashMap<String, String>>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut imports = HashSet::new();
    let mut struct_field_types: HashMap<String, HashMap<String, String>> = HashMap::new();

    // Set language for parser
    if parser.set_language(&tree_sitter_rust::LANGUAGE.into()).is_err() {
        return (nodes, edges, imports, struct_field_types);
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return (nodes, edges, imports, struct_field_types),
    };

    let file_id = format!("file:{}", path);
    let source = content.as_bytes();
    let root = tree.root_node();

    // Track impl blocks to associate methods with types
    let mut impl_target_map: HashMap<String, String> = HashMap::new();

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        extract_rust_node(
            child,
            source,
            content,
            path,
            &file_id,
            &mut nodes,
            &mut edges,
            class_id_map,
            &mut impl_target_map,
            &mut imports,
            &mut struct_field_types,
            "",  // no parent module prefix at root
        );
    }

    (nodes, edges, imports, struct_field_types)
}

/// Recursively extract Rust nodes from AST
fn extract_rust_node(
    node: tree_sitter::Node,
    source: &[u8],
    source_str: &str,
    path: &str,
    file_id: &str,
    nodes: &mut Vec<CodeNode>,
    edges: &mut Vec<CodeEdge>,
    class_id_map: &mut HashMap<String, String>,
    impl_target_map: &mut HashMap<String, String>,
    imports: &mut HashSet<String>,
    struct_field_types: &mut HashMap<String, HashMap<String, String>>,
    module_prefix: &str,
) {
    let text = |n: tree_sitter::Node| -> String {
        n.utf8_text(source).unwrap_or("").to_string()
    };

    match node.kind() {
        "use_declaration" => {
            // Extract import path and leaf symbol names
            let use_text = text(node);
            // Parse: use crate::foo::bar; or use std::collections::HashMap;
            if let Some(path_part) = use_text.strip_prefix("use ") {
                let clean_path = path_part.trim_end_matches(';').trim();
                // Skip std/core library imports
                if !clean_path.starts_with("std::") && !clean_path.starts_with("core::") && !clean_path.starts_with("alloc::") {
                    // Handle use paths with braces: use foo::{bar, baz}
                    let module = if clean_path.contains('{') {
                        clean_path.split("::").next().unwrap_or(clean_path).to_string()
                    } else {
                        clean_path.split("::").take(2).collect::<Vec<_>>().join("::")
                    };
                    if !module.is_empty() {
                        edges.push(CodeEdge {
                            from: file_id.to_string(),
                            to: format!("module_ref:{}", module),
                            relation: EdgeRelation::Imports,
                            weight: 0.5,
                            call_count: 1,
                            in_error_path: false,
                            confidence: 1.0,
                            call_site_line: None,
                            call_site_column: None,
                        });
                        imports.insert(module);
                    }
                    // Also extract leaf symbol names for call edge filtering
                    // use crate::foo::Bar → "Bar"
                    // use crate::foo::{Bar, Baz} → "Bar", "Baz"
                    // use crate::foo::bar_fn → "bar_fn"
                    if clean_path.contains('{') {
                        // Brace group: use foo::{Bar, Baz, qux}
                        if let Some(start) = clean_path.find('{') {
                            if let Some(end) = clean_path.find('}') {
                                let names_part = &clean_path[start + 1..end];
                                for name in names_part.split(',') {
                                    let clean = name.trim();
                                    // Handle `self` rename: `use foo::{self as bar}` → skip
                                    if !clean.is_empty() && clean != "self" && !clean.starts_with("self ") {
                                        // Handle rename: `Bar as Baz` → insert "Baz"
                                        let leaf = if let Some(alias) = clean.split(" as ").nth(1) {
                                            alias.trim()
                                        } else {
                                            clean
                                        };
                                        if !leaf.is_empty() {
                                            imports.insert(leaf.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    } else if clean_path.contains("::") {
                        // Simple path: use crate::foo::Bar → "Bar"
                        if let Some(leaf) = clean_path.rsplit("::").next() {
                            let leaf = leaf.trim();
                            // Handle rename: `use foo::Bar as Baz` → insert "Baz"
                            let actual = if let Some(alias) = leaf.split(" as ").nth(1) {
                                alias.trim()
                            } else {
                                leaf
                            };
                            if !actual.is_empty() && actual != "*" && actual != "self" {
                                imports.insert(actual.to_string());
                            }
                        }
                    }
                }
            }
        }

        "struct_item" => {
            let name = node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            if name.is_empty() { return; }

            let full_name = if module_prefix.is_empty() { name.clone() } else { format!("{}::{}", module_prefix, name) };
            let line = node.start_position().row + 1;
            let class_id = format!("class:{}:{}", path, full_name);

            let signature = extract_rust_signature(node, source_str);
            let docstring = extract_rust_docstring(node, source_str);
            let line_count = node.end_position().row - node.start_position().row + 1;

            nodes.push(CodeNode {
                id: class_id.clone(),
                kind: NodeKind::Class,
                name: full_name.clone(),
                file_path: path.to_string(),
                line: Some(line),
                decorators: extract_rust_attributes(node, source),
                signature,
                docstring,
                line_count,
                is_test: path.contains("/tests/") || full_name.contains("Test"),
            });

            edges.push(CodeEdge::defined_in(&class_id, file_id));
            class_id_map.insert(name.clone(), class_id);

            // Extract struct field name → type mappings for receiver type heuristics
            if let Some(body) = node.child_by_field_name("body") {
                let mut fields_map = HashMap::new();
                let mut field_cursor = body.walk();
                for field in body.children(&mut field_cursor) {
                    if field.kind() == "field_declaration" {
                        let field_name = field.child_by_field_name("name")
                            .and_then(|n| n.utf8_text(source).ok())
                            .unwrap_or("");
                        let field_type = field.child_by_field_name("type")
                            .and_then(|n| n.utf8_text(source).ok())
                            .unwrap_or("");
                        if !field_name.is_empty() && !field_type.is_empty() {
                            // Extract the base type name (strip generics, references, etc.)
                            // Arc<HttpClient> → HttpClient, &str → str, Option<Foo> → Foo
                            let base_type = extract_base_type_name(field_type);
                            if !base_type.is_empty() {
                                fields_map.insert(field_name.to_string(), base_type);
                            }
                        }
                    }
                }
                if !fields_map.is_empty() {
                    struct_field_types.insert(name.clone(), fields_map);
                }
            }
        }

        "enum_item" => {
            let name = node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            if name.is_empty() { return; }

            let full_name = if module_prefix.is_empty() { name.clone() } else { format!("{}::{}", module_prefix, name) };
            let line = node.start_position().row + 1;
            let class_id = format!("class:{}:{}", path, full_name);

            let signature = extract_rust_signature(node, source_str);
            let docstring = extract_rust_docstring(node, source_str);
            let line_count = node.end_position().row - node.start_position().row + 1;

            nodes.push(CodeNode {
                id: class_id.clone(),
                kind: NodeKind::Class,
                name: full_name.clone(),
                file_path: path.to_string(),
                line: Some(line),
                decorators: extract_rust_attributes(node, source),
                signature,
                docstring,
                line_count,
                is_test: path.contains("/tests/") || full_name.contains("Test"),
            });

            edges.push(CodeEdge::defined_in(&class_id, file_id));
            class_id_map.insert(name.clone(), class_id);
        }

        "trait_item" => {
            let name = node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            if name.is_empty() { return; }

            let full_name = if module_prefix.is_empty() { name.clone() } else { format!("{}::{}", module_prefix, name) };
            let line = node.start_position().row + 1;
            let trait_id = format!("class:{}:{}", path, full_name);

            let signature = extract_rust_signature(node, source_str);
            let docstring = extract_rust_docstring(node, source_str);
            let line_count = node.end_position().row - node.start_position().row + 1;

            nodes.push(CodeNode {
                id: trait_id.clone(),
                kind: NodeKind::Class,
                name: full_name.clone(),
                file_path: path.to_string(),
                line: Some(line),
                decorators: extract_rust_attributes(node, source),
                signature,
                docstring,
                line_count,
                is_test: path.contains("/tests/") || full_name.contains("Test"),
            });

            edges.push(CodeEdge::defined_in(&trait_id, file_id));
            class_id_map.insert(name.clone(), trait_id.clone());

            // Extract trait methods
            if let Some(body) = node.child_by_field_name("body") {
                let mut body_cursor = body.walk();
                for body_child in body.children(&mut body_cursor) {
                    if body_child.kind() == "function_item" || body_child.kind() == "function_signature_item" {
                        extract_rust_method(body_child, source, source_str, path, &trait_id, nodes, edges);
                    }
                }
            }
        }

        "impl_item" => {
            // Determine the target type and optional trait
            let mut trait_name: Option<String> = None;
            let mut type_name: Option<String> = None;

            // Parse impl structure: impl [Trait for] Type
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "type_identifier" | "generic_type" | "primitive_type" | "scoped_type_identifier" => {
                        // This could be either the trait or the type
                        let name = if child.kind() == "generic_type" {
                            // Get the base type from generic: Vec<T> -> Vec
                            child.child_by_field_name("type")
                                .and_then(|n| n.utf8_text(source).ok())
                                .unwrap_or("")
                                .to_string()
                        } else if child.kind() == "scoped_type_identifier" {
                            // Handle paths like std::fmt::Display -> Display
                            child.utf8_text(source).ok()
                                .map(|s| s.rsplit("::").next().unwrap_or(s).to_string())
                                .unwrap_or_default()
                        } else {
                            text(child)
                        };
                        
                        if type_name.is_none() {
                            type_name = Some(name);
                        } else if trait_name.is_none() {
                            // If we already have a type, this first one was actually the trait
                            trait_name = type_name.take();
                            type_name = Some(name);
                        }
                    }
                    _ => {}
                }
            }

            let type_name = match type_name {
                Some(n) => n,
                None => return,
            };

            // Look for existing type node or create reference
            let type_id = class_id_map.get(&type_name)
                .cloned()
                .unwrap_or_else(|| format!("class:{}:{}", path, type_name));

            // If this is a trait impl, add inheritance edge
            if let Some(ref trait_n) = trait_name {
                edges.push(CodeEdge {
                    from: type_id.clone(),
                    to: format!("class_ref:{}", trait_n),
                    relation: EdgeRelation::Inherits,
                    weight: 0.5,
                    call_count: 1,
                    in_error_path: false,
                    confidence: 1.0,
                    call_site_line: None,
                    call_site_column: None,
                });
            }

            // Extract methods from impl block
            if let Some(body) = node.child_by_field_name("body") {
                let mut body_cursor = body.walk();
                for body_child in body.children(&mut body_cursor) {
                    if body_child.kind() == "function_item" {
                        extract_rust_method(body_child, source, source_str, path, &type_id, nodes, edges);
                    }
                }
            }
        }

        "function_item" => {
            // Top-level function (not in impl block)
            let name = node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            if name.is_empty() { return; }

            let full_name = if module_prefix.is_empty() { name.clone() } else { format!("{}::{}", module_prefix, name) };
            let line = node.start_position().row + 1;
            let func_id = format!("func:{}:{}", path, full_name);

            let signature = extract_rust_signature(node, source_str);
            let docstring = extract_rust_docstring(node, source_str);
            let line_count = node.end_position().row - node.start_position().row + 1;
            let is_test = path.contains("/tests/") || full_name.starts_with("test_") ||
                extract_rust_attributes(node, source).iter().any(|a| a.contains("test"));

            nodes.push(CodeNode {
                id: func_id.clone(),
                kind: NodeKind::Function,
                name: full_name,
                file_path: path.to_string(),
                line: Some(line),
                decorators: extract_rust_attributes(node, source),
                signature,
                docstring,
                line_count,
                is_test,
            });

            edges.push(CodeEdge::defined_in(&func_id, file_id));
        }

        "mod_item" => {
            let name = node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            if name.is_empty() { return; }

            let new_prefix = if module_prefix.is_empty() { name.clone() } else { format!("{}::{}", module_prefix, name) };

            // If module has a body (inline module), recurse into it
            if let Some(body) = node.child_by_field_name("body") {
                let mut body_cursor = body.walk();
                for body_child in body.children(&mut body_cursor) {
                    extract_rust_node(
                        body_child,
                        source,
                        source_str,
                        path,
                        file_id,
                        nodes,
                        edges,
                        class_id_map,
                        impl_target_map,
                        imports,
                        struct_field_types,
                        &new_prefix,
                    );
                }
            }
        }

        "type_item" => {
            // Type alias: type Foo = Bar;
            let name = node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            if name.is_empty() { return; }

            let full_name = if module_prefix.is_empty() { name.clone() } else { format!("{}::{}", module_prefix, name) };
            let line = node.start_position().row + 1;
            let type_id = format!("class:{}:{}", path, full_name);

            let signature = extract_rust_signature(node, source_str);
            let line_count = node.end_position().row - node.start_position().row + 1;

            nodes.push(CodeNode {
                id: type_id.clone(),
                kind: NodeKind::Class,
                name: full_name.clone(),
                file_path: path.to_string(),
                line: Some(line),
                decorators: extract_rust_attributes(node, source),
                signature,
                docstring: None,
                line_count,
                is_test: false,
            });

            edges.push(CodeEdge::defined_in(&type_id, file_id));
            class_id_map.insert(name, type_id);
        }

        "const_item" | "static_item" => {
            // Optional: track const/static as class-like nodes
            let name = node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            if name.is_empty() || name.starts_with('_') { return; }

            let full_name = if module_prefix.is_empty() { name.clone() } else { format!("{}::{}", module_prefix, name) };
            let line = node.start_position().row + 1;
            let const_id = format!("const:{}:{}", path, full_name);

            let signature = extract_rust_signature(node, source_str);

            nodes.push(CodeNode {
                id: const_id.clone(),
                kind: NodeKind::Class,  // Treat as class for graph purposes
                name: full_name,
                file_path: path.to_string(),
                line: Some(line),
                decorators: extract_rust_attributes(node, source),
                signature,
                docstring: None,
                line_count: 1,
                is_test: false,
            });

            edges.push(CodeEdge::defined_in(&const_id, file_id));
        }

        "macro_definition" => {
            // macro_rules! foo { ... }
            let name = node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            if name.is_empty() { return; }

            let full_name = if module_prefix.is_empty() { name.clone() } else { format!("{}::{}", module_prefix, name) };
            let line = node.start_position().row + 1;
            let macro_id = format!("macro:{}:{}", path, full_name);

            let line_count = node.end_position().row - node.start_position().row + 1;

            nodes.push(CodeNode {
                id: macro_id.clone(),
                kind: NodeKind::Function,  // Treat macros as function-like
                name: format!("{}!", full_name),
                file_path: path.to_string(),
                line: Some(line),
                decorators: vec!["macro".to_string()],
                signature: Some(format!("macro_rules! {}", name)),
                docstring: extract_rust_docstring(node, source_str),
                line_count,
                is_test: false,
            });

            edges.push(CodeEdge::defined_in(&macro_id, file_id));
        }

        _ => {}
    }
}

/// Extract method from impl or trait block
fn extract_rust_method(
    node: tree_sitter::Node,
    source: &[u8],
    source_str: &str,
    path: &str,
    parent_id: &str,
    nodes: &mut Vec<CodeNode>,
    edges: &mut Vec<CodeEdge>,
) {
    let name = node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("")
        .to_string();
    if name.is_empty() { return; }

    let line = node.start_position().row + 1;
    // Include parent type name in method ID to avoid collisions
    // parent_id is like "class:path:TypeName" — extract the type name
    let parent_name = parent_id.rsplit(':').next().unwrap_or("");
    let method_id = if parent_name.is_empty() {
        format!("method:{}:{}", path, name)
    } else {
        format!("method:{}:{}.{}", path, parent_name, name)
    };

    let signature = extract_rust_signature(node, source_str);
    let docstring = extract_rust_docstring(node, source_str);
    let line_count = node.end_position().row - node.start_position().row + 1;
    let attrs = extract_rust_attributes(node, source);
    let is_test = path.contains("/tests/") || name.starts_with("test_") ||
        attrs.iter().any(|a| a.contains("test"));

    nodes.push(CodeNode {
        id: method_id.clone(),
        kind: NodeKind::Function,
        name,
        file_path: path.to_string(),
        line: Some(line),
        decorators: attrs,
        signature,
        docstring,
        line_count,
        is_test,
    });

    edges.push(CodeEdge {
        from: method_id,
        to: parent_id.to_string(),
        relation: EdgeRelation::DefinedIn,
        weight: 0.5,
        call_count: 1,
        in_error_path: false,
        confidence: 1.0,
        call_site_line: None,
        call_site_column: None,
    });
}

/// Extract Rust attributes (#[...])
fn extract_rust_attributes(node: tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut attrs = Vec::new();
    // Look for attribute_item siblings before this node
    if let Some(parent) = node.parent() {
        let mut cursor = parent.walk();
        let mut prev_was_attr = false;
        for child in parent.children(&mut cursor) {
            if child.kind() == "attribute_item" {
                if let Ok(attr_text) = child.utf8_text(source) {
                    let clean = attr_text.trim_start_matches("#[").trim_end_matches(']');
                    attrs.push(clean.to_string());
                }
                prev_was_attr = true;
            } else if child.id() == node.id() && prev_was_attr {
                break;
            } else {
                // Not an attribute and not our target node - reset if we passed attributes
                if prev_was_attr && child.kind() != "line_comment" {
                    attrs.clear();
                }
                prev_was_attr = false;
            }
        }
    }
    
    // Also check for inner attributes
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "attribute_item" {
            if let Ok(attr_text) = child.utf8_text(source) {
                let clean = attr_text.trim_start_matches("#[").trim_end_matches(']');
                attrs.push(clean.to_string());
            }
        }
    }
    
    attrs
}

/// Extract signature from Rust node
fn extract_rust_signature(node: tree_sitter::Node, source_str: &str) -> Option<String> {
    let start = node.start_byte();
    if start >= source_str.len() { return None; }
    
    let sig_text = &source_str[start..];
    // Find the end of signature (before body block or semicolon)
    let sig_end = sig_text.find(" {")
        .or_else(|| sig_text.find("\n{"))
        .or_else(|| sig_text.find(";\n"))
        .or_else(|| sig_text.find(';'))
        .unwrap_or(sig_text.len().min(200));
    
    let sig = sig_text[..sig_end].trim();
    if sig.is_empty() { None } else { Some(sig.to_string()) }
}

/// Extract doc comment from Rust node (/// or //!)
fn extract_rust_docstring(node: tree_sitter::Node, source_str: &str) -> Option<String> {
    // Look for line_comment siblings before the node that start with ///
    let start_line = node.start_position().row;
    if start_line == 0 { return None; }
    
    let lines: Vec<&str> = source_str.lines().collect();
    let mut doc_lines: Vec<&str> = Vec::new();
    
    // Walk backwards from the line before the node
    for i in (0..start_line).rev() {
        if i >= lines.len() { continue; }
        let line = lines[i].trim();
        if line.starts_with("///") {
            doc_lines.push(line.trim_start_matches("///").trim());
        } else if line.starts_with("//!") {
            doc_lines.push(line.trim_start_matches("//!").trim());
        } else if line.is_empty() || line.starts_with("#[") {
            // Skip empty lines and attributes
            continue;
        } else {
            break;
        }
    }
    
    if doc_lines.is_empty() {
        return None;
    }
    
    doc_lines.reverse();
    let first_line = doc_lines.first().copied().unwrap_or("");
    let truncated = if first_line.len() > 100 {
        &first_line[..100]
    } else {
        first_line
    };
    
    if truncated.is_empty() { None } else { Some(truncated.to_string()) }
}

// ─── TypeScript Tree-Sitter Extraction ───

/// Extract from TypeScript/JavaScript source using tree-sitter AST parsing.
/// Handles classes, interfaces, functions, enums, type aliases, and export statements.
fn extract_typescript_tree_sitter(
    path: &str,
    content: &str,
    parser: &mut Parser,
    class_id_map: &mut HashMap<String, String>,
    extension: &str,
) -> (Vec<CodeNode>, Vec<CodeEdge>, HashSet<String>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut imports = HashSet::new();

    // Choose language based on file extension
    let lang_result = match extension {
        "tsx" => parser.set_language(&tree_sitter_typescript::LANGUAGE_TSX.into()),
        "ts" => parser.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "jsx" => parser.set_language(&tree_sitter_javascript::LANGUAGE.into()),
        _ => parser.set_language(&tree_sitter_javascript::LANGUAGE.into()),  // .js default
    };
    
    if lang_result.is_err() {
        return (nodes, edges, imports);
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return (nodes, edges, imports),
    };

    let file_id = format!("file:{}", path);
    let source = content.as_bytes();
    let root = tree.root_node();

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        extract_typescript_node(
            child,
            source,
            content,
            path,
            &file_id,
            &mut nodes,
            &mut edges,
            class_id_map,
            &mut imports,
        );
    }

    (nodes, edges, imports)
}

/// Extract TypeScript/JavaScript nodes from AST
fn extract_typescript_node(
    node: tree_sitter::Node,
    source: &[u8],
    source_str: &str,
    path: &str,
    file_id: &str,
    nodes: &mut Vec<CodeNode>,
    edges: &mut Vec<CodeEdge>,
    class_id_map: &mut HashMap<String, String>,
    imports: &mut HashSet<String>,
) {
    let text = |n: tree_sitter::Node| -> String {
        n.utf8_text(source).unwrap_or("").to_string()
    };

    match node.kind() {
        "import_statement" => {
            // Extract: import { ... } from 'module'; or import x from 'module';
            let import_text = text(node);
            if let Some(from_idx) = import_text.rfind(" from ") {
                let module_part = import_text[from_idx + 6..].trim();
                let module = module_part.trim_matches(|c| c == '\'' || c == '"' || c == ';');
                if module.starts_with('.') || module.starts_with("@/") {
                    edges.push(CodeEdge {
                        from: file_id.to_string(),
                        to: format!("module_ref:{}", module),
                        relation: EdgeRelation::Imports,
                        weight: 0.5,
                        call_count: 1,
                        in_error_path: false,
                        confidence: 1.0,
                        call_site_line: None,
                        call_site_column: None,
                    });
                }
                imports.insert(module.to_string());
                
                // Extract imported names
                if let Some(start) = import_text.find('{') {
                    if let Some(end) = import_text.find('}') {
                        let names_part = &import_text[start+1..end];
                        for name in names_part.split(',') {
                            let clean = name.trim().split(" as ").next().unwrap_or("").trim();
                            if !clean.is_empty() {
                                imports.insert(clean.to_string());
                            }
                        }
                    }
                }
            }
        }

        "class_declaration" | "class" => {
            extract_typescript_class(node, source, source_str, path, file_id, nodes, edges, class_id_map);
        }

        "abstract_class_declaration" => {
            extract_typescript_class(node, source, source_str, path, file_id, nodes, edges, class_id_map);
        }

        "interface_declaration" => {
            let name = node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            if name.is_empty() { return; }

            let line = node.start_position().row + 1;
            let interface_id = format!("class:{}:{}", path, name);

            let signature = extract_typescript_signature(node, source_str);
            let line_count = node.end_position().row - node.start_position().row + 1;

            nodes.push(CodeNode {
                id: interface_id.clone(),
                kind: NodeKind::Class,
                name: name.clone(),
                file_path: path.to_string(),
                line: Some(line),
                decorators: vec!["interface".to_string()],
                signature,
                docstring: extract_typescript_docstring(node, source_str),
                line_count,
                is_test: path.contains("/test") || name.contains("Test"),
            });

            edges.push(CodeEdge::defined_in(&interface_id, file_id));
            class_id_map.insert(name, interface_id);
        }

        "function_declaration" | "function" => {
            let name = node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            if name.is_empty() { return; }

            let line = node.start_position().row + 1;
            let func_id = format!("func:{}:{}", path, name);

            let signature = extract_typescript_signature(node, source_str);
            let docstring = extract_typescript_docstring(node, source_str);
            let line_count = node.end_position().row - node.start_position().row + 1;
            let decorators = extract_typescript_decorators(node, source);

            nodes.push(CodeNode {
                id: func_id.clone(),
                kind: NodeKind::Function,
                name,
                file_path: path.to_string(),
                line: Some(line),
                decorators,
                signature,
                docstring,
                line_count,
                is_test: path.contains("/test") || path.contains(".test.") || path.contains(".spec."),
            });

            edges.push(CodeEdge::defined_in(&func_id, file_id));
        }

        "lexical_declaration" | "variable_declaration" => {
            // Check for arrow functions: const foo = () => { ... }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "variable_declarator" {
                    let name = child.child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                        .unwrap_or("")
                        .to_string();
                    
                    if let Some(value) = child.child_by_field_name("value") {
                        if value.kind() == "arrow_function" || value.kind() == "function" {
                            if name.is_empty() { continue; }
                            
                            let line = node.start_position().row + 1;
                            let func_id = format!("func:{}:{}", path, name);

                            let signature = extract_typescript_signature(node, source_str);
                            let line_count = node.end_position().row - node.start_position().row + 1;

                            nodes.push(CodeNode {
                                id: func_id.clone(),
                                kind: NodeKind::Function,
                                name,
                                file_path: path.to_string(),
                                line: Some(line),
                                decorators: Vec::new(),
                                signature,
                                docstring: extract_typescript_docstring(node, source_str),
                                line_count,
                                is_test: path.contains("/test") || path.contains(".test.") || path.contains(".spec."),
                            });

                            edges.push(CodeEdge::defined_in(&func_id, file_id));
                        }
                    }
                }
            }
        }

        "enum_declaration" => {
            let name = node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            if name.is_empty() { return; }

            let line = node.start_position().row + 1;
            let enum_id = format!("class:{}:{}", path, name);

            let signature = extract_typescript_signature(node, source_str);
            let line_count = node.end_position().row - node.start_position().row + 1;

            nodes.push(CodeNode {
                id: enum_id.clone(),
                kind: NodeKind::Class,
                name: name.clone(),
                file_path: path.to_string(),
                line: Some(line),
                decorators: vec!["enum".to_string()],
                signature,
                docstring: extract_typescript_docstring(node, source_str),
                line_count,
                is_test: false,
            });

            edges.push(CodeEdge::defined_in(&enum_id, file_id));
            class_id_map.insert(name, enum_id);
        }

        "type_alias_declaration" => {
            let name = node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            if name.is_empty() { return; }

            let line = node.start_position().row + 1;
            let type_id = format!("class:{}:{}", path, name);

            let signature = extract_typescript_signature(node, source_str);
            let line_count = node.end_position().row - node.start_position().row + 1;

            nodes.push(CodeNode {
                id: type_id.clone(),
                kind: NodeKind::Class,
                name: name.clone(),
                file_path: path.to_string(),
                line: Some(line),
                decorators: vec!["type".to_string()],
                signature,
                docstring: None,
                line_count,
                is_test: false,
            });

            edges.push(CodeEdge::defined_in(&type_id, file_id));
            class_id_map.insert(name, type_id);
        }

        "export_statement" => {
            // Unwrap export and process inner declaration
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "class_declaration" | "class" | "abstract_class_declaration" |
                    "interface_declaration" | "function_declaration" | "function" |
                    "lexical_declaration" | "variable_declaration" | "enum_declaration" |
                    "type_alias_declaration" => {
                        extract_typescript_node(child, source, source_str, path, file_id, nodes, edges, class_id_map, imports);
                    }
                    _ => {}
                }
            }
        }

        "expression_statement" => {
            // Handle wrapped statements like namespace (which appears as expression_statement → internal_module)
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_typescript_node(child, source, source_str, path, file_id, nodes, edges, class_id_map, imports);
            }
        }

        "module" | "internal_module" | "namespace" => {
            // namespace/module declarations
            let name = node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            
            if !name.is_empty() {
                let line = node.start_position().row + 1;
                let module_id = format!("class:{}:{}", path, name);

                nodes.push(CodeNode {
                    id: module_id.clone(),
                    kind: NodeKind::Class,
                    name: name.clone(),
                    file_path: path.to_string(),
                    line: Some(line),
                    decorators: vec!["namespace".to_string()],
                    signature: Some(format!("namespace {}", name)),
                    docstring: None,
                    line_count: node.end_position().row - node.start_position().row + 1,
                    is_test: false,
                });

                edges.push(CodeEdge::defined_in(&module_id, file_id));
            }

            // Recurse into module body
            if let Some(body) = node.child_by_field_name("body") {
                let mut body_cursor = body.walk();
                for body_child in body.children(&mut body_cursor) {
                    extract_typescript_node(body_child, source, source_str, path, file_id, nodes, edges, class_id_map, imports);
                }
            }
        }

        _ => {}
    }
}

/// Extract TypeScript class with methods
fn extract_typescript_class(
    node: tree_sitter::Node,
    source: &[u8],
    source_str: &str,
    path: &str,
    file_id: &str,
    nodes: &mut Vec<CodeNode>,
    edges: &mut Vec<CodeEdge>,
    class_id_map: &mut HashMap<String, String>,
) {
    let name = node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("")
        .to_string();
    if name.is_empty() { return; }

    let line = node.start_position().row + 1;
    let class_id = format!("class:{}:{}", path, name);

    let signature = extract_typescript_signature(node, source_str);
    let docstring = extract_typescript_docstring(node, source_str);
    let line_count = node.end_position().row - node.start_position().row + 1;
    let decorators = extract_typescript_decorators(node, source);

    nodes.push(CodeNode {
        id: class_id.clone(),
        kind: NodeKind::Class,
        name: name.clone(),
        file_path: path.to_string(),
        line: Some(line),
        decorators,
        signature,
        docstring,
        line_count,
        is_test: path.contains("/test") || name.contains("Test"),
    });

    edges.push(CodeEdge::defined_in(&class_id, file_id));
    class_id_map.insert(name.clone(), class_id.clone());

    // Find parent class from extends clause
    fn find_extends_identifier(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "identifier" | "type_identifier" => {
                    return child.utf8_text(source).ok().map(|s| s.to_string());
                }
                "extends_clause" | "class_heritage" | "extends_type_clause" => {
                    if let Some(name) = find_extends_identifier(child, source) {
                        return Some(name);
                    }
                }
                _ => {}
            }
        }
        None
    }
    
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "class_heritage" || child.kind() == "extends_clause" {
            if let Some(parent_name) = find_extends_identifier(child, source) {
                if !parent_name.is_empty() {
                    edges.push(CodeEdge {
                        from: class_id.clone(),
                        to: format!("class_ref:{}", parent_name),
                        relation: EdgeRelation::Inherits,
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

    // Extract methods from class body
    if let Some(body) = node.child_by_field_name("body") {
        let mut body_cursor = body.walk();
        for body_child in body.children(&mut body_cursor) {
            match body_child.kind() {
                "method_definition" | "public_field_definition" | "method_signature" => {
                    extract_typescript_method(body_child, source, source_str, path, &class_id, nodes, edges);
                }
                _ => {}
            }
        }
    }
}

/// Extract method from class
fn extract_typescript_method(
    node: tree_sitter::Node,
    source: &[u8],
    source_str: &str,
    path: &str,
    class_id: &str,
    nodes: &mut Vec<CodeNode>,
    edges: &mut Vec<CodeEdge>,
) {
    let mut name = node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("")
        .to_string();
    
    // Handle computed property names [key]
    if name.is_empty() {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "property_identifier" || child.kind() == "identifier" {
                if let Ok(text) = child.utf8_text(source) {
                    name = text.to_string();
                    break;
                }
            }
        }
    }
    
    if name.is_empty() { return; }

    let line = node.start_position().row + 1;
    // Include parent class name in method ID to avoid collisions
    let parent_name = class_id.rsplit(':').next().unwrap_or("");
    let method_id = if parent_name.is_empty() {
        format!("method:{}:{}", path, name)
    } else {
        format!("method:{}:{}.{}", path, parent_name, name)
    };

    let signature = extract_typescript_signature(node, source_str);
    let docstring = extract_typescript_docstring(node, source_str);
    let line_count = node.end_position().row - node.start_position().row + 1;
    let decorators = extract_typescript_decorators(node, source);

    nodes.push(CodeNode {
        id: method_id.clone(),
        kind: NodeKind::Function,
        name,
        file_path: path.to_string(),
        line: Some(line),
        decorators,
        signature,
        docstring,
        line_count,
        is_test: path.contains("/test") || path.contains(".test.") || path.contains(".spec."),
    });

    edges.push(CodeEdge {
        from: method_id,
        to: class_id.to_string(),
        relation: EdgeRelation::DefinedIn,
        weight: 0.5,
        call_count: 1,
        in_error_path: false,
        confidence: 1.0,
        call_site_line: None,
        call_site_column: None,
    });
}

/// Extract TypeScript decorators (@decorator)
fn extract_typescript_decorators(node: tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut decorators = Vec::new();
    
    // Look for decorator siblings before this node
    if let Some(parent) = node.parent() {
        let mut cursor = parent.walk();
        for child in parent.children(&mut cursor) {
            if child.kind() == "decorator" {
                if let Ok(dec_text) = child.utf8_text(source) {
                    let name = dec_text.trim_start_matches('@');
                    let name = name.split('(').next().unwrap_or(name).trim();
                    if !name.is_empty() {
                        decorators.push(name.to_string());
                    }
                }
            }
            if child.id() == node.id() {
                break;
            }
        }
    }
    
    decorators
}

/// Extract signature from TypeScript node
fn extract_typescript_signature(node: tree_sitter::Node, source_str: &str) -> Option<String> {
    let start = node.start_byte();
    if start >= source_str.len() { return None; }
    
    let sig_text = &source_str[start..];
    // Find the end of signature (before body block)
    let sig_end = sig_text.find(" {")
        .or_else(|| sig_text.find("\n{"))
        .or_else(|| sig_text.find("{\n"))
        .unwrap_or(sig_text.len().min(200));
    
    let sig = sig_text[..sig_end].trim();
    if sig.is_empty() { None } else { Some(sig.to_string()) }
}

/// Extract JSDoc comment from TypeScript node
fn extract_typescript_docstring(node: tree_sitter::Node, source_str: &str) -> Option<String> {
    let start_line = node.start_position().row;
    if start_line == 0 { return None; }
    
    let lines: Vec<&str> = source_str.lines().collect();
    
    // Look for /** ... */ comment before the node
    for i in (0..start_line).rev() {
        if i >= lines.len() { continue; }
        let line = lines[i].trim();
        
        if line.ends_with("*/") {
            // Found end of JSDoc, find the start
            let mut doc_lines: Vec<&str> = Vec::new();
            for j in (0..=i).rev() {
                if j >= lines.len() { continue; }
                let doc_line = lines[j].trim();
                if doc_line.starts_with("/**") {
                    let first = doc_line.trim_start_matches("/**").trim_start_matches('*').trim();
                    if !first.is_empty() && !first.starts_with('@') {
                        doc_lines.push(first);
                    }
                    break;
                } else if doc_line.starts_with('*') {
                    let content = doc_line.trim_start_matches('*').trim();
                    if !content.is_empty() && !content.starts_with('@') {
                        doc_lines.push(content);
                    }
                }
            }
            
            if doc_lines.is_empty() {
                return None;
            }
            
            doc_lines.reverse();
            let first_line = doc_lines.first().copied().unwrap_or("");
            let truncated = if first_line.len() > 100 {
                &first_line[..100]
            } else {
                first_line
            };
            
            return if truncated.is_empty() { None } else { Some(truncated.to_string()) };
        } else if line.is_empty() || line.starts_with('@') || line.starts_with("//") {
            continue;
        } else {
            break;
        }
    }
    
    None
}

// ─── Regex-Based Fallbacks (kept for reference) ───

/// Extract from Rust source (regex-based fallback, kept for reference).
#[allow(dead_code)]
fn extract_rust_regex(path: &str, content: &str) -> (Vec<CodeNode>, Vec<CodeEdge>, HashSet<String>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    let file_id = format!("file:{}", path);

    let re_use = Regex::new(r"(?m)^use\s+([\w:]+)").unwrap();
    let re_struct = Regex::new(r"(?m)^(?:pub\s+)?struct\s+(\w+)").unwrap();
    let re_enum = Regex::new(r"(?m)^(?:pub\s+)?enum\s+(\w+)").unwrap();
    let re_impl = Regex::new(r"(?m)^impl(?:<[^>]+>)?\s+(?:(\w+)\s+for\s+)?(\w+)").unwrap();
    let re_fn = Regex::new(r"(?m)^\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)").unwrap();

    for cap in re_use.captures_iter(content) {
        let module = cap[1].to_string();
        if !module.starts_with("std::") && !module.starts_with("core::") {
            edges.push(CodeEdge::new(
                &file_id,
                &format!("module_ref:{}", module),
                EdgeRelation::Imports,
            ));
        }
    }

    for cap in re_struct.captures_iter(content) {
        let name = cap[1].to_string();
        let line = content[..cap.get(0).unwrap().start()].lines().count() + 1;
        let node = CodeNode::new_class(path, &name, line);
        edges.push(CodeEdge::defined_in(&node.id, &file_id));
        nodes.push(node);
    }

    for cap in re_enum.captures_iter(content) {
        let name = cap[1].to_string();
        let line = content[..cap.get(0).unwrap().start()].lines().count() + 1;
        let node = CodeNode::new_class(path, &name, line);
        edges.push(CodeEdge::defined_in(&node.id, &file_id));
        nodes.push(node);
    }

    for cap in re_impl.captures_iter(content) {
        if let Some(trait_match) = cap.get(1) {
            let type_name = &cap[2];
            let trait_name = trait_match.as_str();
            if let Some(type_node) = nodes.iter().find(|n| n.name == type_name) {
                edges.push(CodeEdge::new(
                    &type_node.id,
                    &format!("class_ref:{}", trait_name),
                    EdgeRelation::Inherits,
                ));
            }
        }
    }

    for cap in re_fn.captures_iter(content) {
        let name = cap[1].to_string();
        let line = content[..cap.get(0).unwrap().start()].lines().count() + 1;
        let node = CodeNode::new_function(path, &name, line, false);
        edges.push(CodeEdge::defined_in(&node.id, &file_id));
        nodes.push(node);
    }

    (nodes, edges, HashSet::new())
}

/// Extract from TypeScript/JavaScript source (regex-based fallback, kept for reference).
#[allow(dead_code)]
fn extract_typescript_regex(path: &str, content: &str) -> (Vec<CodeNode>, Vec<CodeEdge>, HashSet<String>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    let file_id = format!("file:{}", path);

    let re_import = Regex::new(r#"(?m)^import\s+.*?\s+from\s+['"]([^'"]+)['"]"#).unwrap();
    let re_class = Regex::new(r"(?m)^(?:export\s+)?(?:abstract\s+)?class\s+(\w+)(?:\s+extends\s+(\w+))?").unwrap();
    let re_interface = Regex::new(r"(?m)^(?:export\s+)?interface\s+(\w+)(?:\s+extends\s+(\w+))?").unwrap();
    let re_function = Regex::new(r"(?m)^(?:export\s+)?(?:async\s+)?function\s+(\w+)").unwrap();
    let re_arrow = Regex::new(r"(?m)^(?:export\s+)?(?:const|let)\s+(\w+)\s*=\s*(?:async\s+)?\([^)]*\)\s*=>").unwrap();

    for cap in re_import.captures_iter(content) {
        let module = cap[1].to_string();
        if module.starts_with('.') || module.starts_with("@/") {
            edges.push(CodeEdge::new(
                &file_id,
                &format!("module_ref:{}", module),
                EdgeRelation::Imports,
            ));
        }
    }

    for cap in re_class.captures_iter(content) {
        let name = cap[1].to_string();
        let line = content[..cap.get(0).unwrap().start()].lines().count() + 1;
        let node = CodeNode::new_class(path, &name, line);
        edges.push(CodeEdge::defined_in(&node.id, &file_id));

        if let Some(parent) = cap.get(2) {
            edges.push(CodeEdge::new(
                &node.id,
                &format!("class_ref:{}", parent.as_str()),
                EdgeRelation::Inherits,
            ));
        }

        nodes.push(node);
    }

    for cap in re_interface.captures_iter(content) {
        let name = cap[1].to_string();
        let line = content[..cap.get(0).unwrap().start()].lines().count() + 1;
        let node = CodeNode::new_class(path, &name, line);
        edges.push(CodeEdge::defined_in(&node.id, &file_id));
        nodes.push(node);
    }

    for cap in re_function.captures_iter(content) {
        let name = cap[1].to_string();
        let line = content[..cap.get(0).unwrap().start()].lines().count() + 1;
        let node = CodeNode::new_function(path, &name, line, false);
        edges.push(CodeEdge::defined_in(&node.id, &file_id));
        nodes.push(node);
    }

    for cap in re_arrow.captures_iter(content) {
        let name = cap[1].to_string();
        let line = content[..cap.get(0).unwrap().start()].lines().count() + 1;
        let node = CodeNode::new_function(path, &name, line, false);
        edges.push(CodeEdge::defined_in(&node.id, &file_id));
        nodes.push(node);
    }

    (nodes, edges, HashSet::new())
}

// ═══ Helpers ═══

fn is_python_builtin(name: &str) -> bool {
    matches!(
        name,
        "if" | "for"
            | "while"
            | "return"
            | "print"
            | "len"
            | "range"
            | "str"
            | "int"
            | "float"
            | "list"
            | "dict"
            | "set"
            | "tuple"
            | "type"
            | "isinstance"
            | "issubclass"
            | "super"
            | "hasattr"
            | "getattr"
            | "setattr"
            | "property"
            | "staticmethod"
            | "classmethod"
            | "enumerate"
            | "zip"
            | "map"
            | "filter"
            | "sorted"
            | "reversed"
            | "any"
            | "all"
            | "min"
            | "max"
            | "sum"
            | "abs"
            | "bool"
            | "repr"
            | "hash"
            | "id"
            | "open"
            | "format"
            | "not"
            | "and"
            | "or"
            | "bytes"
            | "bytearray"
            | "memoryview"
            | "object"
            | "complex"
            | "frozenset"
            | "iter"
            | "next"
            | "callable"
            | "delattr"
            | "dir"
            | "divmod"
            | "eval"
            | "exec"
            | "globals"
            | "hex"
            | "input"
            | "locals"
            | "oct"
            | "ord"
            | "pow"
            | "round"
            | "slice"
            | "vars"
            | "chr"
            | "bin"
            | "breakpoint"
            | "compile"
            | "__import__"
            | "ValueError"
            | "TypeError"
            | "KeyError"
            | "IndexError"
            | "AttributeError"
            | "RuntimeError"
            | "Exception"
            | "NotImplementedError"
            | "StopIteration"
            | "OSError"
            | "IOError"
            | "FileNotFoundError"
            | "ImportError"
            | "AssertionError"
            | "NameError"
            | "OverflowError"
            | "ZeroDivisionError"
            | "UnicodeError"
            | "SyntaxError"
    )
}

fn is_stdlib(module: &str) -> bool {
    let stdlib_prefixes = [
        "os", "sys", "re", "json", "math", "io", "abc", "collections", "typing", "unittest",
        "pytest", "copy", "functools", "itertools", "pathlib", "shutil", "tempfile", "logging",
        "warnings", "inspect", "textwrap", "string", "datetime", "time", "hashlib", "base64",
        "pickle", "csv", "xml", "html", "http", "urllib", "socket", "threading",
        "multiprocessing", "subprocess", "contextlib", "enum", "dataclasses", "struct", "array",
        "queue", "heapq", "bisect", "decimal", "fractions", "random", "statistics", "operator",
        "pdb", "traceback", "dis", "ast", "token", "importlib", "pkgutil", "site", "zipimport",
        "numpy", "scipy", "matplotlib", "pandas", "setuptools", "pip", "wheel", "pkg_resources",
        "distutils",
    ];

    let first_part = module.split('.').next().unwrap_or(module);
    stdlib_prefixes.contains(&first_part)
}

/// Infer the type of a method call receiver using struct field type mappings.
/// For `self.client.send()`, receiver is "self.client":
///   - Extract field name "client" 
///   - Look up impl_type in struct_field_types to find field type
/// For chained calls like `self.foo.bar.baz()`, uses first field after self.
/// Returns None if type cannot be inferred.
fn infer_receiver_type(
    receiver: &str,
    impl_type: Option<&str>,
    struct_field_types: &HashMap<String, HashMap<String, String>>,
) -> Option<String> {
    let impl_type = impl_type?;
    
    // Extract the struct name from impl_type (e.g., "class:path/file.rs:MyStruct" → "MyStruct")
    let struct_name = impl_type.rsplit(':').next().unwrap_or(impl_type);
    
    // Get field types for this struct
    let fields = struct_field_types.get(struct_name)?;
    
    // Extract the first field name from receiver
    // "self.client" → "client"
    // "self.client.inner" → "client" (use first field only)
    // "foo" → "foo" (non-self receiver, try as-is)
    let field_name = if receiver.starts_with("self.") {
        let after_self = &receiver[5..]; // skip "self."
        after_self.split('.').next().unwrap_or(after_self)
    } else {
        // Direct variable name — can't resolve type without local variable analysis
        return None;
    };
    
    fields.get(field_name).cloned()
}

/// Extract the base type name from a Rust type annotation.
/// Strips references, generics, wrappers to get the core type name.
/// Arc<HttpClient> → "HttpClient", &str → "str", Option<Vec<Foo>> → "Foo"
/// Box<dyn Trait> → "Trait", HashMap<K, V> → "HashMap"
fn extract_base_type_name(type_str: &str) -> String {
    let s = type_str.trim();
    // Strip references: &, &mut, &'a
    let s = s.trim_start_matches('&');
    let s = if s.starts_with("'") {
        // Lifetime: &'a T → skip lifetime
        s.split_whitespace().nth(1).unwrap_or(s)
    } else {
        s.trim_start_matches("mut ")
    };
    let s = s.trim();

    // For common wrapper types, extract the inner type
    let wrappers = ["Option", "Box", "Arc", "Rc", "Mutex", "RwLock", "RefCell", "Vec", "Cell"];
    for wrapper in wrappers {
        if s.starts_with(wrapper) && s[wrapper.len()..].starts_with('<') {
            let inner = &s[wrapper.len() + 1..];
            if let Some(end) = inner.rfind('>') {
                let inner = inner[..end].trim();
                // Recurse for nested wrappers: Arc<Mutex<Foo>> → Foo
                return extract_base_type_name(inner);
            }
        }
    }

    // Strip "dyn " prefix for trait objects: Box<dyn Trait> → Trait
    let s = s.strip_prefix("dyn ").unwrap_or(s);

    // Get the last segment of a path: foo::bar::Baz → Baz
    let s = s.rsplit("::").next().unwrap_or(s);

    // Strip generic params: HashMap<K, V> → HashMap
    let s = if let Some(idx) = s.find('<') { &s[..idx] } else { s };

    s.trim().to_string()
}

/// Check if a Rust call is a builtin/macro to skip
fn is_rust_builtin(name: &str) -> bool {
    // Strip trailing ! for macro calls
    let name = name.trim_end_matches('!');
    matches!(
        name,
        // Core macros
        "println" | "eprintln" | "print" | "eprint"
            | "format" | "format_args"
            | "vec" | "vec!"
            | "todo" | "unimplemented" | "unreachable"
            | "assert" | "assert_eq" | "assert_ne"
            | "debug_assert" | "debug_assert_eq" | "debug_assert_ne"
            | "dbg" | "cfg" | "env" | "option_env"
            | "include" | "include_str" | "include_bytes"
            | "concat" | "stringify"
            | "write" | "writeln"
            | "panic"
            // Tracing/logging macros
            | "info" | "debug" | "warn" | "error" | "trace"
            | "log" | "span" | "event"
            // Common traits/primitives
            | "Some" | "None" | "Ok" | "Err"
            | "Box" | "Rc" | "Arc" | "Cell" | "RefCell"
            | "Vec" | "String" | "HashMap" | "HashSet" | "BTreeMap" | "BTreeSet"
            | "Option" | "Result"
            | "Default" | "Clone" | "Copy" | "Debug" | "Display"
            | "PartialEq" | "Eq" | "PartialOrd" | "Ord" | "Hash"
            | "Iterator" | "IntoIterator" | "FromIterator"
            | "From" | "Into" | "TryFrom" | "TryInto"
            | "AsRef" | "AsMut" | "Borrow" | "BorrowMut"
            | "Deref" | "DerefMut"
            | "Drop" | "Sized" | "Send" | "Sync"
            // Standard functions
            | "drop" | "mem" | "take" | "replace" | "swap"
    )
}

/// Check if a Rust macro invocation should be skipped
fn is_rust_macro_builtin(name: &str) -> bool {
    matches!(
        name.trim_end_matches('!'),
        "println" | "eprintln" | "print" | "eprint"
            | "format" | "format_args"
            | "vec"
            | "todo" | "unimplemented" | "unreachable"
            | "assert" | "assert_eq" | "assert_ne"
            | "debug_assert" | "debug_assert_eq" | "debug_assert_ne"
            | "dbg" | "cfg" | "env" | "option_env"
            | "include" | "include_str" | "include_bytes"
            | "concat" | "stringify"
            | "write" | "writeln"
            | "panic"
            | "info" | "debug" | "warn" | "error" | "trace"
            | "log" | "span" | "event"
            | "matches"
    )
}

/// Check if a TypeScript/JavaScript call is a builtin to skip
fn is_typescript_builtin(name: &str) -> bool {
    matches!(
        name,
        // Console
        "log" | "error" | "warn" | "debug" | "info" | "trace" | "dir" | "table"
            // Timers
            | "setTimeout" | "setInterval" | "clearTimeout" | "clearInterval"
            | "setImmediate" | "clearImmediate"
            // Parsing
            | "parseInt" | "parseFloat" | "isNaN" | "isFinite"
            // Require/import
            | "require" | "import"
            // Promise statics
            | "resolve" | "reject" | "all" | "race" | "allSettled" | "any"
            // Object statics
            | "keys" | "values" | "entries" | "assign" | "freeze" | "seal"
            | "defineProperty" | "getOwnPropertyNames" | "getPrototypeOf"
            // Array statics
            | "isArray" | "from" | "of"
            // JSON
            | "parse" | "stringify"
            // Math
            | "floor" | "ceil" | "round" | "abs" | "min" | "max" | "random"
            | "sqrt" | "pow" | "sin" | "cos" | "tan"
            // String methods (common)
            | "toString" | "valueOf" | "charAt" | "charCodeAt" | "codePointAt"
            | "concat" | "includes" | "indexOf" | "lastIndexOf"
            | "match" | "replace" | "search" | "slice" | "split"
            | "substring" | "substr" | "toLowerCase" | "toUpperCase" | "trim"
            // Array methods (common)
            | "push" | "pop" | "shift" | "unshift" | "splice"
            | "map" | "filter" | "reduce" | "reduceRight" | "find" | "findIndex"
            | "every" | "some" | "forEach" | "join" | "sort" | "reverse"
            | "fill" | "copyWithin" | "flat" | "flatMap"
            // Reflect/Proxy
            | "Reflect" | "Proxy"
            // Node.js globals
            | "process" | "Buffer" | "__dirname" | "__filename"
    )
}

/// Check if a TypeScript object.method call should be skipped
fn is_typescript_builtin_method(obj: &str, method: &str) -> bool {
    match obj {
        "console" => matches!(method, "log" | "error" | "warn" | "debug" | "info" | "trace" | "dir" | "table" | "time" | "timeEnd" | "assert"),
        "Promise" => matches!(method, "resolve" | "reject" | "all" | "race" | "allSettled" | "any"),
        "Object" => matches!(method, "keys" | "values" | "entries" | "assign" | "freeze" | "seal" | "defineProperty" | "getOwnPropertyNames" | "getPrototypeOf" | "create" | "hasOwn"),
        "Array" => matches!(method, "isArray" | "from" | "of"),
        "JSON" => matches!(method, "parse" | "stringify"),
        "Math" => true, // All Math methods are builtins
        "Number" => matches!(method, "isNaN" | "isFinite" | "isInteger" | "isSafeInteger" | "parseInt" | "parseFloat"),
        "String" => matches!(method, "fromCharCode" | "fromCodePoint" | "raw"),
        "Date" => matches!(method, "now" | "parse" | "UTC"),
        "Reflect" => true, // All Reflect methods are builtins
        "process" => true, // Node.js process is builtin
        "Buffer" => true, // Node.js Buffer is builtin
        _ => false,
    }
}

/// Resolve a TypeScript/JavaScript import path to a module_map key.
/// Handles relative paths like `./foo`, `../bar`, `../../components/Stats.js`
/// and converts them to dot-separated format matching module_map keys.
fn resolve_ts_import(
    importing_file: &str,
    import_module: &str,
    module_map: &HashMap<String, String>,
) -> Option<String> {
    // Handle path aliases like @/foo - just try the literal path
    if import_module.starts_with('@') {
        // Try @/foo -> src.foo
        let without_at = import_module.trim_start_matches("@/");
        let normalized = normalize_ts_module_path(without_at);
        if let Some(file_id) = module_map.get(&normalized) {
            return Some(file_id.clone());
        }
        // Try with src prefix
        let with_src = format!("src.{}", normalized);
        if let Some(file_id) = module_map.get(&with_src) {
            return Some(file_id.clone());
        }
        return None;
    }

    // Only handle relative imports
    if !import_module.starts_with('.') {
        return None;
    }

    // Get the directory of the importing file
    let importing_dir = if let Some(pos) = importing_file.rfind('/') {
        &importing_file[..pos]
    } else {
        ""
    };

    // Resolve the relative path
    let resolved = resolve_relative_path(importing_dir, import_module);
    
    // Normalize: strip extensions and convert / to .
    let normalized = normalize_ts_module_path(&resolved);
    
    // Try direct lookup
    if let Some(file_id) = module_map.get(&normalized) {
        return Some(file_id.clone());
    }
    
    // Try with common TS extensions (import says .js but file might be .tsx)
    // The module_map was built without extensions, so we just try the base name
    // But sometimes partial paths exist, try those too
    let parts: Vec<&str> = normalized.split('.').collect();
    for start in 1..parts.len() {
        let partial = parts[start..].join(".");
        if let Some(file_id) = module_map.get(&partial) {
            return Some(file_id.clone());
        }
    }
    
    None
}

/// Resolve a relative path against a base directory
fn resolve_relative_path(base_dir: &str, relative: &str) -> String {
    let mut parts: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').collect()
    };
    
    for segment in relative.split('/') {
        match segment {
            "." | "" => continue,
            ".." => { parts.pop(); }
            s => parts.push(s),
        }
    }
    
    parts.join("/")
}

/// Normalize a TypeScript module path to dot-separated format
fn normalize_ts_module_path(path: &str) -> String {
    path.replace('/', ".")
        .trim_end_matches(".js")
        .trim_end_matches(".jsx")
        .trim_end_matches(".ts")
        .trim_end_matches(".tsx")
        .trim_end_matches(".mjs")
        .trim_end_matches(".mts")
        .to_string()
}

// ═══ Call Extraction - Rust ═══

/// Build scope map for Rust — maps line ranges to function IDs
fn build_scope_map_rust(
    node: tree_sitter::Node,
    source: &[u8],
    rel_path: &str,
    scope_map: &mut Vec<(usize, usize, String, Option<String>)>,
) {
    let mut stack: Vec<(tree_sitter::Node, Option<String>)> = vec![(node, None)];

    while let Some((current, impl_ctx)) = stack.pop() {
        match current.kind() {
            "impl_item" => {
                // Extract impl target type
                let impl_type = extract_impl_type(current, source);
                let impl_id = impl_type.as_ref().map(|t| format!("class:{}:{}", rel_path, t));
                
                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, impl_id.clone()));
                    }
                }
            }
            "function_item" => {
                let func_name = current
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");

                if !func_name.is_empty() {
                    let start_line = current.start_position().row + 1;
                    let end_line = current.end_position().row + 1;

                    let func_id = if let Some(ref impl_type) = impl_ctx {
                        let type_name = impl_type.rsplit(':').next().unwrap_or("");
                        if type_name.is_empty() {
                            format!("method:{}:{}", rel_path, func_name)
                        } else {
                            format!("method:{}:{}.{}", rel_path, type_name, func_name)
                        }
                    } else {
                        format!("func:{}:{}", rel_path, func_name)
                    };

                    scope_map.push((start_line, end_line, func_id, impl_ctx.clone()));
                }

                // Recurse into nested functions/closures
                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, impl_ctx.clone()));
                    }
                }
            }
            "closure_expression" => {
                // Track closures as anonymous scopes but don't create IDs for them
                // The containing function will handle the call
                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, impl_ctx.clone()));
                    }
                }
            }
            "mod_item" => {
                // Recurse into inline modules
                if let Some(body) = current.child_by_field_name("body") {
                    stack.push((body, impl_ctx.clone()));
                }
            }
            _ => {
                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, impl_ctx.clone()));
                    }
                }
            }
        }
    }
}

/// Extract calls from Rust AST
fn extract_calls_rust(
    root: tree_sitter::Node,
    source: &[u8],
    rel_path: &str,
    func_name_map: &HashMap<String, Vec<String>>,
    method_to_class: &HashMap<String, String>,
    file_func_ids: &HashSet<String>,
    node_pkg_map: &HashMap<String, String>,
    file_imported_names: &HashMap<String, HashSet<String>>,
    struct_field_types: &HashMap<String, HashMap<String, String>>,
    edges: &mut Vec<CodeEdge>,
) {
    // Build scope map
    let mut scope_map: Vec<(usize, usize, String, Option<String>)> = Vec::new();
    build_scope_map_rust(root, source, rel_path, &mut scope_map);

    let package_dir = rel_path.rsplitn(2, '/').nth(1).unwrap_or("");

    // Walk tree looking for calls
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        // Skip string literals and comments
        if node.kind() == "string_literal"
            || node.kind() == "raw_string_literal"
            || node.kind() == "line_comment"
            || node.kind() == "block_comment"
        {
            continue;
        }

        match node.kind() {
            "call_expression" => {
                // Function call: foo(), path::to::foo(), or self.method()
                // Note: Rust tree-sitter parses self.method() as call_expression > field_expression,
                // NOT as method_call_expression. We need to detect self. here.
                let call_line = node.start_position().row + 1;

                let scope = scope_map
                    .iter()
                    .filter(|(start, end, _, _)| call_line >= *start && call_line <= *end)
                    .max_by_key(|(start, _, _, _)| *start);

                if let Some((_start, _end, caller_id, impl_ctx)) = scope {
                    if let Some(func_node) = node.child_by_field_name("function") {
                        // Check if this is self.method() or Self::method()
                        let is_self_call = if func_node.kind() == "field_expression" {
                            // self.method() — field_expression with self receiver
                            func_node.child(0)
                                .map(|c| c.kind() == "self" || c.utf8_text(source).ok() == Some("self"))
                                .unwrap_or(false)
                        } else {
                            false
                        };

                        if is_self_call {
                            // Extract method name from field_expression
                            let method_name = func_node.child_by_field_name("field")
                                .or_else(|| {
                                    // fallback: last child that is field_identifier
                                    let mut cursor = func_node.walk();
                                    func_node.children(&mut cursor)
                                        .filter(|c| c.kind() == "field_identifier")
                                        .last()
                                })
                                .and_then(|n| n.utf8_text(source).ok())
                                .unwrap_or("");

                            if !method_name.is_empty() && !is_rust_builtin(method_name) {
                                resolve_rust_self_method_call(
                                    caller_id,
                                    method_name,
                                    impl_ctx.as_deref(),
                                    func_name_map,
                                    method_to_class,
                                    file_func_ids,
                                    edges,
                                );
                            }
                        } else {
                            let callee_name = extract_rust_call_target(func_node, source);
                            
                            if !callee_name.is_empty() && !is_rust_builtin(&callee_name) {
                                resolve_rust_call_edge(
                                    caller_id,
                                    &callee_name,
                                    func_name_map,
                                    file_func_ids,
                                    package_dir,
                                    node_pkg_map,
                                    false,
                                    file_imported_names,
                                    rel_path,
                                    None,
                                    method_to_class,
                                    edges,
                                );
                            }
                        }
                    }
                    
                    // Scan arguments for function references (fn passed as argument)
                    // Pattern: foo(bar) where bar is a known function name
                    if let Some(args_node) = node.child_by_field_name("arguments") {
                        scan_args_for_fn_refs(
                            args_node, source, caller_id,
                            func_name_map, file_func_ids, package_dir, node_pkg_map,
                            file_imported_names, rel_path, edges,
                        );
                    }
                }
            }
            "method_call_expression" => {
                // Method call: obj.method() or self.method()
                let call_line = node.start_position().row + 1;

                let scope = scope_map
                    .iter()
                    .filter(|(start, end, _, _)| call_line >= *start && call_line <= *end)
                    .max_by_key(|(start, _, _, _)| *start);

                if let Some((_start, _end, caller_id, impl_ctx)) = scope {
                    // Get method name
                    let method_name = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                        .unwrap_or("");

                    if !method_name.is_empty() && !is_rust_builtin(method_name) {
                        // Check if receiver is self
                        let receiver = node.child_by_field_name("value")
                            .and_then(|n| n.utf8_text(source).ok())
                            .unwrap_or("");

                        if receiver == "self" || receiver == "Self" {
                            // Self method call — resolve within impl type
                            resolve_rust_self_method_call(
                                caller_id,
                                method_name,
                                impl_ctx.as_deref(),
                                func_name_map,
                                method_to_class,
                                file_func_ids,
                                edges,
                            );
                        } else {
                            // Regular method call on an object — try to infer receiver type
                            // For self.client.send(), receiver is "self.client"
                            // Extract field name and look up type via struct_field_types
                            let receiver_type = infer_receiver_type(
                                receiver, impl_ctx.as_deref(), struct_field_types,
                            );
                            resolve_rust_call_edge(
                                caller_id,
                                method_name,
                                func_name_map,
                                file_func_ids,
                                package_dir,
                                node_pkg_map,
                                true,
                                file_imported_names,
                                rel_path,
                                receiver_type.as_deref(),
                                method_to_class,
                                edges,
                            );
                        }
                    }
                    
                    // Scan arguments for function references
                    if let Some(args_node) = node.child_by_field_name("arguments") {
                        scan_args_for_fn_refs(
                            args_node, source, caller_id,
                            func_name_map, file_func_ids, package_dir, node_pkg_map,
                            file_imported_names, rel_path, edges,
                        );
                    }
                }
            }
            "macro_invocation" => {
                // Macro call: foo!()
                let call_line = node.start_position().row + 1;

                // Get macro name
                let macro_name = node
                    .child_by_field_name("macro")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");

                let scope = scope_map
                    .iter()
                    .filter(|(start, end, _, _)| call_line >= *start && call_line <= *end)
                    .max_by_key(|(start, _, _, _)| *start);

                if let Some((_start, _end, caller_id, impl_ctx)) = scope {
                    // Custom macro call (not built-in)
                    if !macro_name.is_empty() && !is_rust_macro_builtin(macro_name) {
                        let macro_id_name = format!("{}!", macro_name);
                        if let Some(callee_ids) = func_name_map.get(&macro_id_name) {
                            for callee_id in callee_ids.iter().take(3) {
                                if callee_id != caller_id {
                                    edges.push(CodeEdge {
                                        from: caller_id.to_string(),
                                        to: callee_id.clone(),
                                        relation: EdgeRelation::Calls,
                                        weight: 0.5,
                                        call_count: 1,
                                        in_error_path: false,
                                        confidence: 0.7,
                                        call_site_line: None,
                                        call_site_column: None,
                                    });
                                }
                            }
                        }
                    }

                    // Scan token_tree for function calls inside the macro
                    // tree-sitter treats macro args as opaque tokens, but we can
                    // detect pattern: identifier followed by token_tree starting with '('
                    let tt = {
                        let mut found = node.child_by_field_name("tokens");
                        if found.is_none() {
                            let count = node.child_count();
                            for idx in 0..count {
                                if let Some(ch) = node.child(idx) {
                                    if ch.kind() == "token_tree" {
                                        found = Some(ch);
                                        break;
                                    }
                                }
                            }
                        }
                        found
                    };
                    if let Some(token_tree) = tt {
                        extract_calls_from_token_tree(
                            token_tree,
                            source,
                            caller_id,
                            impl_ctx.as_deref(),
                            func_name_map,
                            method_to_class,
                            file_func_ids,
                            package_dir,
                            node_pkg_map,
                            file_imported_names,
                            rel_path,
                            struct_field_types,
                            edges,
                        );
                    }
                }
            }
            _ => {}
        }

        let child_count = node.child_count();
        for i in (0..child_count).rev() {
            if let Some(child) = node.child(i) {
                stack.push(child);
            }
        }
    }
}

/// Scan function arguments for identifiers that match known function names.
/// Detects functions passed as arguments (function pointers, callbacks).
/// e.g., `.is_some_and(header_value_is_credential)`, `get(verify_webhook)`
fn scan_args_for_fn_refs(
    args_node: tree_sitter::Node,
    source: &[u8],
    caller_id: &str,
    func_name_map: &HashMap<String, Vec<String>>,
    file_func_ids: &HashSet<String>,
    package_dir: &str,
    node_pkg_map: &HashMap<String, String>,
    file_imported_names: &HashMap<String, HashSet<String>>,
    rel_path: &str,
    edges: &mut Vec<CodeEdge>,
) {
    let mut cursor = args_node.walk();
    for child in args_node.children(&mut cursor) {
        if child.kind() == "identifier" {
            let name = child.utf8_text(source).unwrap_or("");
            // Only match if it's a known function name and looks like a function (snake_case)
            if !name.is_empty() 
                && func_name_map.contains_key(name)
                && !is_rust_builtin(name)
                && name.chars().next().map(|c| c.is_lowercase()).unwrap_or(false)
            {
                resolve_rust_call_edge(
                    caller_id, name, func_name_map, file_func_ids,
                    package_dir, node_pkg_map, false,
                    file_imported_names, rel_path, None, &HashMap::new(), edges,
                );
            }
        }
    }
}

/// Extract function calls from macro token_tree (opaque to tree-sitter).
/// Detects pattern: `identifier` followed by `token_tree` starting with `(` = function call.
/// Also detects `self.identifier(...)` patterns for self method calls.
fn extract_calls_from_token_tree(
    token_tree: tree_sitter::Node,
    source: &[u8],
    caller_id: &str,
    impl_ctx: Option<&str>,
    func_name_map: &HashMap<String, Vec<String>>,
    method_to_class: &HashMap<String, String>,
    file_func_ids: &HashSet<String>,
    package_dir: &str,
    node_pkg_map: &HashMap<String, String>,
    file_imported_names: &HashMap<String, HashSet<String>>,
    rel_path: &str,
    struct_field_types: &HashMap<String, HashMap<String, String>>,
    edges: &mut Vec<CodeEdge>,
) {
    let mut cursor = token_tree.walk();
    let children: Vec<tree_sitter::Node> = token_tree.children(&mut cursor).collect();
    
    let mut i = 0;
    while i < children.len() {
        let child = children[i];
        
        // Pattern 1: self.method(args) inside token_tree
        // tokens: self, ., identifier, token_tree(...)
        if child.kind() == "self" && i + 3 < children.len() {
            let dot = children[i + 1];
            let method = children[i + 2];
            let args = children[i + 3];
            
            if dot.utf8_text(source).ok() == Some(".")
                && method.kind() == "identifier"
                && args.kind() == "token_tree"
            {
                let method_name = method.utf8_text(source).unwrap_or("");
                if !method_name.is_empty() && !is_rust_builtin(method_name) {
                    resolve_rust_self_method_call(
                        caller_id,
                        method_name,
                        impl_ctx,
                        func_name_map,
                        method_to_class,
                        file_func_ids,
                        edges,
                    );
                }
                i += 4;
                continue;
            }
        }
        
        // Pattern 2: free_function(args) inside token_tree
        // tokens: identifier, token_tree(...)
        if child.kind() == "identifier" && i + 1 < children.len() {
            let next = children[i + 1];
            if next.kind() == "token_tree" {
                let callee_name = child.utf8_text(source).unwrap_or("");
                if !callee_name.is_empty() 
                    && !is_rust_builtin(callee_name)
                    && !is_rust_macro_builtin(callee_name)
                    // Skip common non-function identifiers in format strings
                    && callee_name.chars().next().map(|c| c.is_lowercase()).unwrap_or(false)
                {
                    resolve_rust_call_edge(
                        caller_id,
                        callee_name,
                        func_name_map,
                        file_func_ids,
                        package_dir,
                        node_pkg_map,
                        false,
                        file_imported_names,
                        rel_path,
                        None,
                        method_to_class,
                        edges,
                    );
                }
            }
        }
        
        // Recurse into nested token_trees
        if child.kind() == "token_tree" {
            extract_calls_from_token_tree(
                child,
                source,
                caller_id,
                impl_ctx,
                func_name_map,
                method_to_class,
                file_func_ids,
                package_dir,
                node_pkg_map,
                file_imported_names,
                rel_path,
                struct_field_types,
                edges,
            );
        }
        
        i += 1;
    }
}

/// Extract the target of a Rust call expression
fn extract_rust_call_target(node: tree_sitter::Node, source: &[u8]) -> String {
    match node.kind() {
        "identifier" => {
            node.utf8_text(source).unwrap_or("").to_string()
        }
        "scoped_identifier" => {
            // For path::to::fn or Type::method, get the last segment
            node.utf8_text(source).ok()
                .map(|s| s.rsplit("::").next().unwrap_or(s).to_string())
                .unwrap_or_default()
        }
        "field_expression" => {
            // For obj.method, get the method name (field_identifier child)
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "field_identifier" {
                    return child.utf8_text(source).unwrap_or("").to_string();
                }
            }
            // Fallback: get last segment after . or ::
            node.utf8_text(source).ok()
                .map(|s| {
                    s.rsplit('.').next()
                        .unwrap_or_else(|| s.rsplit("::").next().unwrap_or(s))
                        .to_string()
                })
                .unwrap_or_default()
        }
        "generic_function" => {
            // foo::<T>() — extract foo
            node.child_by_field_name("function")
                .and_then(|n| n.utf8_text(source).ok())
                .map(|s| s.rsplit("::").next().unwrap_or(s).to_string())
                .unwrap_or_default()
        }
        _ => {
            // Fallback: get the text and extract last identifier
            node.utf8_text(source).ok()
                .map(|s| s.rsplit("::").next().unwrap_or(s).trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_').to_string())
                .unwrap_or_default()
        }
    }
}

/// Extract impl type from impl_item node
fn extract_impl_type(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    // For `impl Type`, return Type. For `impl Trait for Type`, return Type (not Trait).
    // The `for` keyword separates trait from type in tree-sitter AST.
    let mut trait_or_type: Option<String> = None;
    let mut seen_for = false;
    
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "type_identifier" | "generic_type" | "scoped_type_identifier" | "primitive_type" => {
                let name = if child.kind() == "generic_type" {
                    child.child_by_field_name("type")
                        .and_then(|n| n.utf8_text(source).ok())
                        .map(|s| s.to_string())
                } else if child.kind() == "scoped_type_identifier" {
                    child.utf8_text(source).ok()
                        .map(|s| s.rsplit("::").next().unwrap_or(s).to_string())
                } else {
                    child.utf8_text(source).ok().map(|s| s.to_string())
                };
                
                if seen_for {
                    // This is the type after `for` — this is what we want
                    return name;
                }
                trait_or_type = name;
            }
            _ => {
                if child.utf8_text(source).ok() == Some("for") {
                    seen_for = true;
                }
            }
        }
    }
    
    // No `for` keyword — this is `impl Type`, return the type
    trait_or_type
}

/// Resolve and add Rust call edge
fn resolve_rust_call_edge(
    caller_id: &str,
    callee_name: &str,
    func_name_map: &HashMap<String, Vec<String>>,
    file_func_ids: &HashSet<String>,
    package_dir: &str,
    node_pkg_map: &HashMap<String, String>,
    is_method_call: bool,
    file_imported_names: &HashMap<String, HashSet<String>>,
    rel_path: &str,
    receiver_type: Option<&str>,
    method_to_class: &HashMap<String, String>,
    edges: &mut Vec<CodeEdge>,
) {
    if let Some(callee_ids) = func_name_map.get(callee_name) {
        // Level 2: If we know the receiver type, filter by it first
        if let Some(recv_type) = receiver_type {
            let type_matched: Vec<&String> = callee_ids
                .iter()
                .filter(|id| {
                    method_to_class
                        .get(*id)
                        .map(|cls| {
                            // cls is like "class:path/file.rs:TypeName"
                            // Match if the class name contains the receiver type
                            cls.rsplit(':').next()
                                .map(|name| name == recv_type)
                                .unwrap_or(false)
                        })
                        .unwrap_or(false)
                })
                .collect();

            if !type_matched.is_empty() {
                for callee_id in type_matched {
                    if callee_id != caller_id {
                        edges.push(CodeEdge {
                            from: caller_id.to_string(),
                            to: callee_id.clone(),
                            relation: EdgeRelation::Calls,
                            weight: 0.5,
                            call_count: 1,
                            in_error_path: false,
                            confidence: 0.95,
                            call_site_line: None,
                            call_site_column: None,
                        });
                    }
                }
                return;
            }
            // Fall through to normal resolution if receiver type didn't match anything
        }

        // Prioritize: same file > imported > same package > global (limited)
        let same_file: Vec<&String> = callee_ids
            .iter()
            .filter(|id| file_func_ids.contains(*id))
            .collect();

        // Level 1: Import-scoped filtering
        let imported: Vec<&String> = callee_ids
            .iter()
            .filter(|_id| {
                file_imported_names
                    .get(rel_path)
                    .map(|names| names.contains(callee_name))
                    .unwrap_or(false)
            })
            .collect();

        let same_pkg: Vec<&String> = callee_ids
            .iter()
            .filter(|id| {
                node_pkg_map
                    .get(id.as_str())
                    .map(|pkg| pkg == package_dir)
                    .unwrap_or(false)
            })
            .collect();

        let global_limit = if is_method_call { 10 } else { 3 };

        let (targets, confidence): (Vec<&String>, f32) = if !same_file.is_empty() {
            (same_file, 0.9)
        } else if !imported.is_empty() {
            (imported, 0.8)
        } else if !same_pkg.is_empty() {
            (same_pkg, 0.7)
        } else if callee_ids.len() <= global_limit {
            (callee_ids.iter().collect(), 0.5)
        } else {
            (vec![], 0.0)
        };

        for callee_id in targets {
            if callee_id != caller_id {
                edges.push(CodeEdge {
                    from: caller_id.to_string(),
                    to: callee_id.clone(),
                    relation: EdgeRelation::Calls,
                    weight: 0.5,
                    call_count: 1,
                    in_error_path: false,
                    confidence,
                    call_site_line: None,
                    call_site_column: None,
                });
            }
        }
    }
}

/// Resolve self.method() calls in Rust
fn resolve_rust_self_method_call(
    caller_id: &str,
    method_name: &str,
    impl_type: Option<&str>,
    func_name_map: &HashMap<String, Vec<String>>,
    method_to_class: &HashMap<String, String>,
    file_func_ids: &HashSet<String>,
    edges: &mut Vec<CodeEdge>,
) {
    if let Some(callee_ids) = func_name_map.get(method_name) {
        if let Some(type_id) = impl_type {
            // Filter methods that belong to the same type or its traits
            let scoped: Vec<&String> = callee_ids
                .iter()
                .filter(|id| {
                    method_to_class
                        .get(*id)
                        .map(|cls| cls == type_id)
                        .unwrap_or(false)
                })
                .collect();

            let targets = if !scoped.is_empty() {
                scoped
            } else if callee_ids.len() <= 5 {
                callee_ids.iter().collect()
            } else {
                callee_ids
                    .iter()
                    .filter(|id| file_func_ids.contains(*id))
                    .collect()
            };

            for callee_id in targets {
                if callee_id != caller_id {
                    edges.push(CodeEdge {
                        from: caller_id.to_string(),
                        to: callee_id.clone(),
                        relation: EdgeRelation::Calls,
                        weight: 0.5,
                        call_count: 1,
                        in_error_path: false,
                        confidence: 0.9,
                        call_site_line: None,
                        call_site_column: None,
                    });
                }
            }
        } else {
            // No impl context, use same-file heuristic
            for callee_id in callee_ids {
                if callee_id != caller_id && file_func_ids.contains(callee_id) {
                    edges.push(CodeEdge {
                        from: caller_id.to_string(),
                        to: callee_id.clone(),
                        relation: EdgeRelation::Calls,
                        weight: 0.5,
                        call_count: 1,
                        in_error_path: false,
                        confidence: 0.6,
                        call_site_line: None,
                        call_site_column: None,
                    });
                }
            }
        }
    }
}

// ═══ Call Extraction - TypeScript ═══

/// Build scope map for TypeScript — maps line ranges to function IDs
fn build_scope_map_typescript(
    node: tree_sitter::Node,
    source: &[u8],
    rel_path: &str,
    scope_map: &mut Vec<(usize, usize, String, Option<String>)>,
) {
    let mut stack: Vec<(tree_sitter::Node, Option<String>)> = vec![(node, None)];

    while let Some((current, class_ctx)) = stack.pop() {
        match current.kind() {
            "class_declaration" | "class" | "abstract_class_declaration" => {
                let class_name = current
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");
                let class_id = if !class_name.is_empty() {
                    Some(format!("class:{}:{}", rel_path, class_name))
                } else {
                    class_ctx.clone()
                };

                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, class_id.clone()));
                    }
                }
            }
            "function_declaration" | "function" => {
                let func_name = current
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");

                if !func_name.is_empty() {
                    let start_line = current.start_position().row + 1;
                    let end_line = current.end_position().row + 1;
                    let func_id = format!("func:{}:{}", rel_path, func_name);
                    scope_map.push((start_line, end_line, func_id, class_ctx.clone()));
                }

                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, class_ctx.clone()));
                    }
                }
            }
            "method_definition" | "method_signature" => {
                let method_name = current
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");

                if !method_name.is_empty() {
                    let start_line = current.start_position().row + 1;
                    let end_line = current.end_position().row + 1;

                    let method_id = if let Some(ref cls) = class_ctx {
                        let cls_name = cls.rsplit(':').next().unwrap_or("");
                        if cls_name.is_empty() {
                            format!("method:{}:{}", rel_path, method_name)
                        } else {
                            format!("method:{}:{}.{}", rel_path, cls_name, method_name)
                        }
                    } else {
                        format!("method:{}:{}", rel_path, method_name)
                    };

                    scope_map.push((start_line, end_line, method_id, class_ctx.clone()));
                }

                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, class_ctx.clone()));
                    }
                }
            }
            "arrow_function" => {
                // Arrow functions inside variable declarators
                // The scope is tracked but ID comes from the variable name
                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, class_ctx.clone()));
                    }
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                // Check for const foo = () => {}
                let mut cursor = current.walk();
                for child in current.children(&mut cursor) {
                    if child.kind() == "variable_declarator" {
                        let var_name = child.child_by_field_name("name")
                            .and_then(|n| n.utf8_text(source).ok())
                            .unwrap_or("");
                        
                        if let Some(value) = child.child_by_field_name("value") {
                            if value.kind() == "arrow_function" || value.kind() == "function" {
                                if !var_name.is_empty() {
                                    let start_line = current.start_position().row + 1;
                                    let end_line = current.end_position().row + 1;
                                    let func_id = format!("func:{}:{}", rel_path, var_name);
                                    scope_map.push((start_line, end_line, func_id, class_ctx.clone()));
                                }
                            }
                        }
                        stack.push((child, class_ctx.clone()));
                    }
                }
            }
            "export_statement" => {
                // Unwrap export and process inner
                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, class_ctx.clone()));
                    }
                }
            }
            _ => {
                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, class_ctx.clone()));
                    }
                }
            }
        }
    }
}

/// Extract calls from TypeScript AST
fn extract_calls_typescript(
    root: tree_sitter::Node,
    source: &[u8],
    rel_path: &str,
    func_name_map: &HashMap<String, Vec<String>>,
    method_to_class: &HashMap<String, String>,
    file_func_ids: &HashSet<String>,
    file_imported_names: &HashMap<String, HashSet<String>>,
    node_pkg_map: &HashMap<String, String>,
    edges: &mut Vec<CodeEdge>,
) {
    // Build scope map
    let mut scope_map: Vec<(usize, usize, String, Option<String>)> = Vec::new();
    build_scope_map_typescript(root, source, rel_path, &mut scope_map);

    let package_dir = rel_path.rsplitn(2, '/').nth(1).unwrap_or("");

    // Walk tree looking for calls
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        // Skip string literals and comments
        if node.kind() == "string" 
            || node.kind() == "template_string"
            || node.kind() == "comment"
        {
            continue;
        }

        match node.kind() {
            "call_expression" => {
                let call_line = node.start_position().row + 1;

                let scope = scope_map
                    .iter()
                    .filter(|(start, end, _, _)| call_line >= *start && call_line <= *end)
                    .max_by_key(|(start, _, _, _)| *start);

                if let Some((_start, _end, caller_id, caller_class)) = scope {
                    if let Some(func_node) = node.child_by_field_name("function") {
                        match func_node.kind() {
                            "identifier" => {
                                let callee_name = func_node.utf8_text(source).unwrap_or("");
                                if !callee_name.is_empty() && !is_typescript_builtin(callee_name) {
                                    resolve_typescript_call_edge(
                                        caller_id,
                                        callee_name,
                                        func_name_map,
                                        file_func_ids,
                                        file_imported_names,
                                        rel_path,
                                        package_dir,
                                        node_pkg_map,
                                        false,
                                        edges,
                                    );
                                }
                            }
                            "member_expression" => {
                                // obj.method()
                                let obj_node = func_node.child_by_field_name("object");
                                let prop_node = func_node.child_by_field_name("property");

                                if let (Some(obj), Some(prop)) = (obj_node, prop_node) {
                                    let obj_text = obj.utf8_text(source).unwrap_or("");
                                    let method_name = prop.utf8_text(source).unwrap_or("");

                                    if !method_name.is_empty() {
                                        // Check for builtin object.method patterns
                                        if is_typescript_builtin_method(obj_text, method_name) {
                                            // Skip builtin
                                        } else if obj_text == "this" {
                                            // this.method() — resolve within class
                                            resolve_typescript_self_method_call(
                                                caller_id,
                                                method_name,
                                                caller_class.as_deref(),
                                                func_name_map,
                                                method_to_class,
                                                file_func_ids,
                                                edges,
                                            );
                                        } else if !is_typescript_builtin(method_name) {
                                            // Regular method call
                                            resolve_typescript_call_edge(
                                                caller_id,
                                                method_name,
                                                func_name_map,
                                                file_func_ids,
                                                file_imported_names,
                                                rel_path,
                                                package_dir,
                                                node_pkg_map,
                                                true,
                                                edges,
                                            );
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            "new_expression" => {
                // new ClassName()
                let call_line = node.start_position().row + 1;

                let scope = scope_map
                    .iter()
                    .filter(|(start, end, _, _)| call_line >= *start && call_line <= *end)
                    .max_by_key(|(start, _, _, _)| *start);

                if let Some((_start, _end, caller_id, _)) = scope {
                    if let Some(constructor) = node.child_by_field_name("constructor") {
                        let class_name = constructor.utf8_text(source).unwrap_or("");
                        
                        // Skip builtins like new Promise, new Error, etc.
                        if !class_name.is_empty() 
                            && !matches!(class_name, "Promise" | "Error" | "Array" | "Object" | "Map" | "Set" | "WeakMap" | "WeakSet" | "Date" | "RegExp" | "URL" | "URLSearchParams" | "Headers" | "Request" | "Response" | "FormData" | "Blob" | "File" | "FileReader" | "Image" | "Event" | "CustomEvent" | "AbortController")
                        {
                            // Look for class constructor by name
                            if let Some(callee_ids) = func_name_map.get(class_name) {
                                let targets: Vec<&String> = if callee_ids.len() <= 5 {
                                    callee_ids.iter().collect()
                                } else {
                                    callee_ids.iter().filter(|id| file_func_ids.contains(*id)).collect()
                                };

                                for callee_id in targets {
                                    if callee_id != caller_id {
                                        edges.push(CodeEdge {
                                            from: caller_id.to_string(),
                                            to: callee_id.clone(),
                                            relation: EdgeRelation::Calls,
                                            weight: 0.5,
                                            call_count: 1,
                                            in_error_path: false,
                                            confidence: 0.7,
                                            call_site_line: None,
                                            call_site_column: None,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // JSX component references like <Stats /> or <Dashboard>...</Dashboard>
            "jsx_element" | "jsx_self_closing_element" => {
                let call_line = node.start_position().row + 1;

                let scope = scope_map
                    .iter()
                    .filter(|(start, end, _, _)| call_line >= *start && call_line <= *end)
                    .max_by_key(|(start, _, _, _)| *start);

                if let Some((_start, _end, caller_id, _)) = scope {
                    // For jsx_element, the opening tag is the first child (jsx_opening_element)
                    // For jsx_self_closing_element, the name is directly accessible
                    let tag_name = if node.kind() == "jsx_self_closing_element" {
                        node.child_by_field_name("name")
                            .and_then(|n| n.utf8_text(source).ok())
                            .unwrap_or("")
                    } else {
                        // jsx_element has opening_element as first child
                        node.child(0)
                            .and_then(|open| open.child_by_field_name("name"))
                            .and_then(|n| n.utf8_text(source).ok())
                            .unwrap_or("")
                    };

                    // Only process PascalCase component names (user-defined components)
                    // Lowercase tags like <div>, <span> are HTML elements
                    if !tag_name.is_empty() 
                        && tag_name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
                        && !is_typescript_builtin(tag_name)
                    {
                        resolve_typescript_call_edge(
                            caller_id,
                            tag_name,
                            func_name_map,
                            file_func_ids,
                            file_imported_names,
                            rel_path,
                            package_dir,
                            node_pkg_map,
                            false,
                            edges,
                        );
                    }
                }
            }
            _ => {}
        }

        let child_count = node.child_count();
        for i in (0..child_count).rev() {
            if let Some(child) = node.child(i) {
                stack.push(child);
            }
        }
    }
}

/// Resolve and add TypeScript call edge
fn resolve_typescript_call_edge(
    caller_id: &str,
    callee_name: &str,
    func_name_map: &HashMap<String, Vec<String>>,
    file_func_ids: &HashSet<String>,
    file_imported_names: &HashMap<String, HashSet<String>>,
    rel_path: &str,
    package_dir: &str,
    node_pkg_map: &HashMap<String, String>,
    is_method_call: bool,
    edges: &mut Vec<CodeEdge>,
) {
    if let Some(callee_ids) = func_name_map.get(callee_name) {
        let same_file: Vec<&String> = callee_ids
            .iter()
            .filter(|id| file_func_ids.contains(*id))
            .collect();
        let imported: Vec<&String> = callee_ids
            .iter()
            .filter(|_id| {
                file_imported_names
                    .get(rel_path)
                    .map(|names| names.contains(callee_name))
                    .unwrap_or(false)
            })
            .collect();
        let same_pkg: Vec<&String> = callee_ids
            .iter()
            .filter(|id| {
                node_pkg_map
                    .get(id.as_str())
                    .map(|pkg| pkg == package_dir)
                    .unwrap_or(false)
            })
            .collect();

        let global_limit = if is_method_call { 15 } else { 3 };

        let (targets, confidence): (Vec<&String>, f32) = if !same_file.is_empty() {
            (same_file, 0.9)
        } else if !imported.is_empty() {
            (imported, 0.8)
        } else if !same_pkg.is_empty() {
            (same_pkg, 0.7)
        } else if callee_ids.len() <= global_limit {
            (callee_ids.iter().collect(), 0.5)
        } else {
            (vec![], 0.0)
        };

        for callee_id in targets {
            if callee_id != caller_id {
                edges.push(CodeEdge {
                    from: caller_id.to_string(),
                    to: callee_id.clone(),
                    relation: EdgeRelation::Calls,
                    weight: 0.5,
                    call_count: 1,
                    in_error_path: false,
                    confidence,
                    call_site_line: None,
                    call_site_column: None,
                });
            }
        }
    }
}

/// Resolve this.method() calls in TypeScript
fn resolve_typescript_self_method_call(
    caller_id: &str,
    method_name: &str,
    caller_class: Option<&str>,
    func_name_map: &HashMap<String, Vec<String>>,
    method_to_class: &HashMap<String, String>,
    file_func_ids: &HashSet<String>,
    edges: &mut Vec<CodeEdge>,
) {
    if let Some(callee_ids) = func_name_map.get(method_name) {
        if let Some(class_id) = caller_class {
            let scoped: Vec<&String> = callee_ids
                .iter()
                .filter(|id| {
                    method_to_class
                        .get(*id)
                        .map(|cls| cls == class_id)
                        .unwrap_or(false)
                })
                .collect();

            let targets = if !scoped.is_empty() {
                scoped
            } else if callee_ids.len() <= 5 {
                callee_ids.iter().collect()
            } else {
                callee_ids
                    .iter()
                    .filter(|id| file_func_ids.contains(*id))
                    .collect()
            };

            for callee_id in targets {
                if callee_id != caller_id {
                    edges.push(CodeEdge {
                        from: caller_id.to_string(),
                        to: callee_id.clone(),
                        relation: EdgeRelation::Calls,
                        weight: 0.5,
                        call_count: 1,
                        in_error_path: false,
                        confidence: 0.9,
                        call_site_line: None,
                        call_site_column: None,
                    });
                }
            }
        } else {
            // No class context
            for callee_id in callee_ids {
                if callee_id != caller_id && file_func_ids.contains(callee_id) {
                    edges.push(CodeEdge {
                        from: caller_id.to_string(),
                        to: callee_id.clone(),
                        relation: EdgeRelation::Calls,
                        weight: 0.5,
                        call_count: 1,
                        in_error_path: false,
                        confidence: 0.6,
                        call_site_line: None,
                        call_site_column: None,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_python() {
        let content = r#"
import os
from pathlib import Path

class MyClass(BaseClass):
    def method(self):
        pass

def top_level():
    pass
"#;
        let mut parser = Parser::new();
        let language = tree_sitter_python::LANGUAGE;
        parser.set_language(&language.into()).unwrap();
        let mut class_map = HashMap::new();

        let (nodes, edges, _) = extract_python_tree_sitter("test.py", content, &mut parser, &mut class_map);

        assert!(nodes.iter().any(|n| n.name == "MyClass"));
        assert!(nodes.iter().any(|n| n.name == "method"));
        assert!(nodes.iter().any(|n| n.name == "top_level"));
        assert!(edges.iter().any(|e| e.to.contains("BaseClass")));
    }

    #[test]
    fn test_extract_rust() {
        let content = r#"
use std::path::Path;
use crate::module;

pub struct MyStruct {
    field: i32,
}

impl MyTrait for MyStruct {
    fn method(&self) {}
}

pub fn top_level() {}
"#;
        let mut parser = Parser::new();
        let mut class_map = HashMap::new();
        let (nodes, edges, _, _) = extract_rust_tree_sitter("test.rs", content, &mut parser, &mut class_map);

        assert!(nodes.iter().any(|n| n.name == "MyStruct"), "Should find MyStruct");
        assert!(nodes.iter().any(|n| n.name == "method"), "Should find method");
        assert!(nodes.iter().any(|n| n.name == "top_level"), "Should find top_level");
        assert!(edges.iter().any(|e| e.to.contains("module")), "Should have module import edge");
        
        // Tree-sitter should also capture trait implementation relationship
        assert!(edges.iter().any(|e| e.relation == EdgeRelation::Inherits && e.to.contains("MyTrait")),
            "Should capture trait impl inheritance");
    }

    #[test]
    fn test_extract_rust_comprehensive() {
        let content = r#"
use crate::foo::bar;

/// A documented struct
pub struct Person {
    name: String,
    age: u32,
}

/// A documented enum
pub enum Status {
    Active,
    Inactive,
}

/// A trait
pub trait Greeter {
    fn greet(&self) -> String;
}

impl Greeter for Person {
    fn greet(&self) -> String {
        format!("Hello, {}", self.name)
    }
}

impl Person {
    pub fn new(name: String) -> Self {
        Self { name, age: 0 }
    }
    
    pub fn birthday(&mut self) {
        self.age += 1;
    }
}

mod inner {
    pub fn nested_fn() {}
}

type MyAlias = Vec<String>;

pub fn standalone() {}

#[test]
fn test_something() {}
"#;
        let mut parser = Parser::new();
        let mut class_map = HashMap::new();
        let (nodes, edges, _, _) = extract_rust_tree_sitter("test.rs", content, &mut parser, &mut class_map);

        // Structs and enums
        assert!(nodes.iter().any(|n| n.name == "Person"), "Should find Person struct");
        assert!(nodes.iter().any(|n| n.name == "Status"), "Should find Status enum");
        
        // Traits
        assert!(nodes.iter().any(|n| n.name == "Greeter"), "Should find Greeter trait");
        
        // Methods from impl blocks
        assert!(nodes.iter().any(|n| n.name == "greet"), "Should find greet method");
        assert!(nodes.iter().any(|n| n.name == "new"), "Should find new method");
        assert!(nodes.iter().any(|n| n.name == "birthday"), "Should find birthday method");
        
        // Nested module functions
        assert!(nodes.iter().any(|n| n.name.contains("nested_fn")), "Should find nested_fn");
        
        // Type aliases
        assert!(nodes.iter().any(|n| n.name == "MyAlias"), "Should find type alias");
        
        // Standalone function
        assert!(nodes.iter().any(|n| n.name == "standalone"), "Should find standalone fn");
        
        // Test function should be marked as test
        let test_node = nodes.iter().find(|n| n.name == "test_something");
        assert!(test_node.is_some(), "Should find test function");
        assert!(test_node.unwrap().is_test, "Test function should be marked as test");
        
        // Methods should be linked to their impl target
        let greet_edges: Vec<_> = edges.iter()
            .filter(|e| e.from.contains("greet") && e.relation == EdgeRelation::DefinedIn)
            .collect();
        assert!(!greet_edges.is_empty(), "greet should have DefinedIn edge");
    }

    #[test]
    fn test_extract_typescript() {
        let content = r#"
import { Component } from './component';

export class MyClass extends BaseClass {
    method(): void {}
}

export function topLevel(): void {}

export const arrowFn = () => {};
"#;
        let mut parser = Parser::new();
        let mut class_map = HashMap::new();
        let (nodes, edges, _) = extract_typescript_tree_sitter("test.ts", content, &mut parser, &mut class_map, "ts");

        assert!(nodes.iter().any(|n| n.name == "MyClass"), "Should find MyClass");
        assert!(nodes.iter().any(|n| n.name == "topLevel"), "Should find topLevel");
        assert!(nodes.iter().any(|n| n.name == "arrowFn"), "Should find arrowFn");
        assert!(edges.iter().any(|e| e.to.contains("component")), "Should have component import");
        
        // Tree-sitter should also find the method inside the class
        assert!(nodes.iter().any(|n| n.name == "method"), "Should find method inside class");
        
        // Should capture inheritance
        assert!(edges.iter().any(|e| e.relation == EdgeRelation::Inherits && e.to.contains("BaseClass")),
            "Should capture class inheritance");
    }

    #[test]
    fn test_extract_typescript_comprehensive() {
        let content = r#"
import { Injectable } from '@angular/core';
import type { User } from './types';

/**
 * A service class
 */
@Injectable()
export class UserService {
    private users: User[] = [];
    
    /**
     * Get all users
     */
    getUsers(): User[] {
        return this.users;
    }
    
    addUser(user: User): void {
        this.users.push(user);
    }
}

export interface IRepository<T> {
    find(id: string): T | undefined;
    save(item: T): void;
}

export type UserId = string;

export enum UserRole {
    Admin = 'admin',
    User = 'user',
}

export function createUser(name: string): User {
    return { name };
}

export const fetchUser = async (id: string) => {
    return null;
};

export default class DefaultExport {}

namespace MyNamespace {
    export function innerFn() {}
}
"#;
        let mut parser = Parser::new();
        let mut class_map = HashMap::new();
        let (nodes, edges, _) = extract_typescript_tree_sitter("test.ts", content, &mut parser, &mut class_map, "ts");

        // Classes
        assert!(nodes.iter().any(|n| n.name == "UserService"), "Should find UserService class");
        assert!(nodes.iter().any(|n| n.name == "DefaultExport"), "Should find default export class");
        
        // Methods inside class
        assert!(nodes.iter().any(|n| n.name == "getUsers"), "Should find getUsers method");
        assert!(nodes.iter().any(|n| n.name == "addUser"), "Should find addUser method");
        
        // Interfaces
        assert!(nodes.iter().any(|n| n.name == "IRepository"), "Should find interface");
        
        // Type aliases
        assert!(nodes.iter().any(|n| n.name == "UserId"), "Should find type alias");
        
        // Enums
        assert!(nodes.iter().any(|n| n.name == "UserRole"), "Should find enum");
        
        // Functions
        assert!(nodes.iter().any(|n| n.name == "createUser"), "Should find function");
        
        // Arrow functions
        assert!(nodes.iter().any(|n| n.name == "fetchUser"), "Should find arrow function");
        
        // Namespace
        assert!(nodes.iter().any(|n| n.name == "MyNamespace"), "Should find namespace");
        
        // Imports
        assert!(edges.iter().any(|e| e.relation == EdgeRelation::Imports), "Should have import edges");
    }

    #[test]
    fn test_rust_call_extraction() {
        let content = r#"
pub struct Calculator {
    value: i32,
}

impl Calculator {
    pub fn new() -> Self {
        Self { value: 0 }
    }
    
    pub fn add(&mut self, x: i32) {
        self.value += x;
        self.log_operation("add");
    }
    
    fn log_operation(&self, op: &str) {
        helper_fn(op);
    }
}

fn helper_fn(msg: &str) {
    println!("{}", msg);
}

pub fn create_and_use() {
    let mut calc = Calculator::new();
    calc.add(5);
    helper_fn("done");
}
"#;
        // Use full extract_from_dir simulation
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        
        let mut class_map = HashMap::new();
        let (nodes, mut edges, _, _) = extract_rust_tree_sitter("calc.rs", content, &mut parser, &mut class_map);

        // Build func_map for call extraction
        let func_map: HashMap<String, Vec<String>> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .fold(HashMap::new(), |mut acc, n| {
                acc.entry(n.name.clone()).or_default().push(n.id.clone());
                acc
            });

        // Build method_to_class
        let method_to_class: HashMap<String, String> = edges
            .iter()
            .filter(|e| e.relation == EdgeRelation::DefinedIn && e.to.starts_with("class:"))
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();

        let file_func_ids: HashSet<String> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .map(|n| n.id.clone())
            .collect();

        let node_pkg_map: HashMap<String, String> = nodes
            .iter()
            .map(|n| (n.id.clone(), "".to_string()))
            .collect();

        // Parse and extract calls
        let tree = parser.parse(content, None).unwrap();
        let root = tree.root_node();
        
        extract_calls_rust(
            root,
            content.as_bytes(),
            "calc.rs",
            &func_map,
            &method_to_class,
            &file_func_ids,
            &node_pkg_map,
            &HashMap::new(),
            &HashMap::new(),
            &mut edges,
        );

        // Verify call edges exist
        let call_edges: Vec<_> = edges.iter()
            .filter(|e| e.relation == EdgeRelation::Calls)
            .collect();
        
        assert!(!call_edges.is_empty(), "Should have call edges");
        
        // Check specific calls
        assert!(
            call_edges.iter().any(|e| e.from.contains("create_and_use") && e.to.contains("helper_fn")),
            "create_and_use should call helper_fn"
        );
        
        assert!(
            call_edges.iter().any(|e| e.from.contains("log_operation") && e.to.contains("helper_fn")),
            "log_operation should call helper_fn"
        );
    }

    #[test]
    fn test_typescript_call_extraction() {
        let content = r#"
export class UserService {
    private helper: Helper;
    
    constructor() {
        this.helper = new Helper();
    }
    
    getUser(id: string) {
        return this.fetchFromDb(id);
    }
    
    private fetchFromDb(id: string) {
        return formatUser(this.helper.query(id));
    }
}

function formatUser(data: any) {
    return processData(data);
}

function processData(data: any) {
    return data;
}

class Helper {
    query(id: string) {
        return null;
    }
}
"#;
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()).unwrap();
        
        let mut class_map = HashMap::new();
        let (nodes, mut edges, imports) = extract_typescript_tree_sitter("user.ts", content, &mut parser, &mut class_map, "ts");

        // Build func_map
        let func_map: HashMap<String, Vec<String>> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .fold(HashMap::new(), |mut acc, n| {
                acc.entry(n.name.clone()).or_default().push(n.id.clone());
                acc
            });

        // Build method_to_class
        let method_to_class: HashMap<String, String> = edges
            .iter()
            .filter(|e| e.relation == EdgeRelation::DefinedIn && e.to.starts_with("class:"))
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();

        let file_func_ids: HashSet<String> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .map(|n| n.id.clone())
            .collect();

        let mut file_imported_names: HashMap<String, HashSet<String>> = HashMap::new();
        file_imported_names.insert("user.ts".to_string(), imports);

        let node_pkg_map: HashMap<String, String> = nodes
            .iter()
            .map(|n| (n.id.clone(), "".to_string()))
            .collect();

        // Parse and extract calls
        let tree = parser.parse(content, None).unwrap();
        let root = tree.root_node();
        
        extract_calls_typescript(
            root,
            content.as_bytes(),
            "user.ts",
            &func_map,
            &method_to_class,
            &file_func_ids,
            &file_imported_names,
            &node_pkg_map,
            &mut edges,
        );

        // Verify call edges exist
        let call_edges: Vec<_> = edges.iter()
            .filter(|e| e.relation == EdgeRelation::Calls)
            .collect();
        
        assert!(!call_edges.is_empty(), "Should have call edges");
        
        // Check specific calls
        assert!(
            call_edges.iter().any(|e| e.from.contains("fetchFromDb") && e.to.contains("formatUser")),
            "fetchFromDb should call formatUser"
        );
        
        assert!(
            call_edges.iter().any(|e| e.from.contains("formatUser") && e.to.contains("processData")),
            "formatUser should call processData"
        );
    }

    #[test]
    fn test_resolve_relative_path() {
        // Test basic relative path resolution
        assert_eq!(resolve_relative_path("src/pages", "./Dashboard"), "src/pages/Dashboard");
        assert_eq!(resolve_relative_path("src/pages", "../utils/helper"), "src/utils/helper");
        assert_eq!(resolve_relative_path("src/pages/admin", "../../components/Stats"), "src/components/Stats");
        assert_eq!(resolve_relative_path("src/pages", "../../components/Stats"), "components/Stats");
        assert_eq!(resolve_relative_path("", "./foo"), "foo");
        assert_eq!(resolve_relative_path("src", "../lib/util"), "lib/util");
    }

    #[test]
    fn test_normalize_ts_module_path() {
        // Test extension stripping and slash to dot conversion
        assert_eq!(normalize_ts_module_path("src/components/Stats.js"), "src.components.Stats");
        assert_eq!(normalize_ts_module_path("src/components/Stats.tsx"), "src.components.Stats");
        assert_eq!(normalize_ts_module_path("src/components/Stats.ts"), "src.components.Stats");
        assert_eq!(normalize_ts_module_path("src/components/Stats.jsx"), "src.components.Stats");
        assert_eq!(normalize_ts_module_path("src/components/Stats"), "src.components.Stats");
    }

    #[test]
    fn test_resolve_ts_import() {
        let mut module_map = HashMap::new();
        module_map.insert("src.components.Stats".to_string(), "file:src/components/Stats.tsx".to_string());
        module_map.insert("src.utils.helper".to_string(), "file:src/utils/helper.ts".to_string());
        module_map.insert("components.Stats".to_string(), "file:src/components/Stats.tsx".to_string());

        // Test relative import resolution
        let result = resolve_ts_import("src/pages/Dashboard.tsx", "../../components/Stats.js", &module_map);
        assert_eq!(result, Some("file:src/components/Stats.tsx".to_string()), 
            "Should resolve ../../components/Stats.js from src/pages/Dashboard.tsx");

        let result = resolve_ts_import("src/pages/Dashboard.tsx", "../utils/helper", &module_map);
        assert_eq!(result, Some("file:src/utils/helper.ts".to_string()),
            "Should resolve ../utils/helper from src/pages/Dashboard.tsx");

        // Test ./relative
        let mut module_map2 = HashMap::new();
        module_map2.insert("src.pages.local".to_string(), "file:src/pages/local.ts".to_string());
        let result = resolve_ts_import("src/pages/Dashboard.tsx", "./local", &module_map2);
        assert_eq!(result, Some("file:src/pages/local.ts".to_string()),
            "Should resolve ./local from src/pages/Dashboard.tsx");

        // Test non-relative import returns None
        let result = resolve_ts_import("src/pages/Dashboard.tsx", "lodash", &module_map);
        assert_eq!(result, None, "Non-relative imports should return None");
    }

    #[test]
    fn test_resolve_ts_import_path_alias() {
        let mut module_map = HashMap::new();
        module_map.insert("src.components.Stats".to_string(), "file:src/components/Stats.tsx".to_string());

        // Test @/ path alias
        let result = resolve_ts_import("src/pages/Dashboard.tsx", "@/components/Stats", &module_map);
        assert_eq!(result, Some("file:src/components/Stats.tsx".to_string()),
            "Should resolve @/components/Stats path alias");
    }
}
