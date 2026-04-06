//! Unified graph building and LSP refinement.
//!
//! Combines code nodes with task structure for planning, and refines
//! call edges using LSP servers for compiler-level precision.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::types::*;
use super::lang::{find_call_position, find_project_root};

impl CodeGraph {
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
        let project_root = find_project_root(root_dir);
        let extract_dir = root_dir.canonicalize().unwrap_or_else(|_| root_dir.to_path_buf());
        let project_root_canon = project_root.canonicalize().unwrap_or_else(|_| project_root.clone());

        // Compute prefix: if extract_dir is a subdirectory of project_root, this is the relative path
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

        // Collect file contents by language
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
                        let caller = self.node_by_id(&edge.from);
                        let fp = caller.map(|n| n.file_path.clone()).unwrap_or_default();
                        (fp, line, col)
                    } else {
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

                        let raw_callee = edge
                            .to
                            .rsplit(':')
                            .next()
                            .unwrap_or(&edge.to);
                        
                        let callee_name = if raw_callee.contains('.') {
                            raw_callee.rsplit('.').next().unwrap_or(raw_callee)
                        } else {
                            raw_callee
                        };

                        let caller_start = caller.line.unwrap_or(0);
                        let caller_end = caller_start + caller.line_count;

                        let mut found_pos = None;
                        for (line_idx, line_text) in source.lines().enumerate() {
                            let line_num = line_idx;
                            if line_num >= caller_start && line_num <= caller_end {
                                if let Some(col_pos) = find_call_position(line_text, callee_name) {
                                    found_pos = Some((line_num as u32, col_pos as u32));
                                    break;
                                }
                            }
                        }

                        match found_pos {
                            Some((line, col)) => (caller.file_path.clone(), line, col),
                            None => {
                                stats.failed += 1;
                                continue;
                            }
                        }
                    };

                // Query LSP for definition
                let lsp_file_path = to_lsp_path(&file_path);
                match client.get_definition(&lsp_file_path, call_line, call_col) {
                    Ok(Some(location)) => {
                        let graph_file_path = from_lsp_path(&location.file_path);
                        if let Some(file_index) = def_index.get(&graph_file_path) {
                            if let Some(target_id) =
                                find_closest_node(file_index, location.line, 5)
                            {
                                edges_to_update.push((idx, Some(target_id), 1.0));
                                stats.refined += 1;
                            } else {
                                edges_to_update.push((idx, None, edge.confidence.max(0.6)));
                                stats.refined += 1;
                            }
                        } else {
                            edges_to_update.push((idx, None, edge.confidence.max(0.6)));
                            stats.refined += 1;
                        }
                    }
                    Ok(None) => {
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
