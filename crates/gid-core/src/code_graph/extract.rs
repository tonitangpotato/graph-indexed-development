use std::collections::{HashMap, HashSet};
use std::path::Path;

use regex::Regex;
use tree_sitter::Parser;
use walkdir::WalkDir;

use super::lang::{python::*, rust_lang::*, typescript::*};
use super::types::*;

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
}
