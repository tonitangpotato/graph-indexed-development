//! Unified graph building and LSP refinement.
//!
//! Combines code nodes with task structure for planning, and refines
//! call edges using LSP servers for compiler-level precision.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::types::*;
use super::lang::{find_call_position, find_project_root};
use crate::lsp_client::LspLocation;
use crate::lsp_daemon;

/// Unified LSP provider: either a direct LspClient or a DaemonLspClient.
/// Both support the same operations but with different lifecycles.
enum LspProvider {
    Direct(crate::lsp_client::LspClient),
    Daemon(lsp_daemon::DaemonLspClient),
}

impl LspProvider {
    fn open_file(&mut self, rel_path: &str, content: &str, language_id: &str) -> anyhow::Result<()> {
        match self {
            Self::Direct(c) => c.open_file(rel_path, content, language_id),
            Self::Daemon(c) => c.open_file(rel_path, content),
        }
    }

    fn get_definition(&mut self, rel_path: &str, line: u32, character: u32) -> anyhow::Result<Option<LspLocation>> {
        match self {
            Self::Direct(c) => c.get_definition(rel_path, line, character),
            Self::Daemon(c) => c.get_definition(rel_path, line, character),
        }
    }

    fn get_references(&mut self, rel_path: &str, line: u32, character: u32, include_declaration: bool) -> anyhow::Result<Vec<LspLocation>> {
        match self {
            Self::Direct(c) => c.get_references(rel_path, line, character, include_declaration),
            Self::Daemon(c) => c.get_references(rel_path, line, character, include_declaration),
        }
    }

    fn get_implementations(&mut self, rel_path: &str, line: u32, character: u32) -> anyhow::Result<Vec<LspLocation>> {
        match self {
            Self::Direct(c) => c.get_implementations(rel_path, line, character),
            Self::Daemon(c) => c.get_implementations(rel_path, line, character),
        }
    }

    fn wait_until_ready(&mut self) -> anyhow::Result<()> {
        match self {
            Self::Direct(c) => c.wait_until_ready(std::time::Duration::from_secs(600)),
            Self::Daemon(_) => Ok(()), // Daemon handles readiness internally
        }
    }

    fn shutdown(self) -> anyhow::Result<()> {
        match self {
            Self::Direct(c) => c.shutdown(),
            Self::Daemon(_) => Ok(()), // Don't shut down daemon - it persists
        }
    }

    fn progress_token_summary(&self) -> String {
        match self {
            Self::Direct(c) => c.progress_token_summary(),
            Self::Daemon(_) => "daemon-managed".to_string(),
        }
    }
}

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
                NodeKind::Class | NodeKind::Interface | NodeKind::Enum | NodeKind::TypeAlias | NodeKind::Trait => ("Component".to_string(), "domain"),
                NodeKind::Function | NodeKind::Constant | NodeKind::Module => ("Component".to_string(), "application"),
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

        // Detect languages present in the project for coverage check
        let mut langs_in_project: HashMap<String, (usize, usize)> = HashMap::new();
        for node in &self.nodes {
            if node.kind == NodeKind::File {
                let ext = node.file_path.rsplit('.').next().unwrap_or("");
                let lang_id = extension_to_language_id(ext).to_string();
                if lang_id != "plaintext" {
                    langs_in_project.entry(lang_id).or_insert((0, 0)).0 += 1;
                }
            }
        }
        // Count call edges per language
        for edge in &self.edges {
            if edge.relation == EdgeRelation::Calls {
                if let Some(caller) = self.node_by_id(&edge.from) {
                    let ext = caller.file_path.rsplit('.').next().unwrap_or("");
                    let lang_id = extension_to_language_id(ext).to_string();
                    if lang_id != "plaintext" {
                        langs_in_project.entry(lang_id).or_insert((0, 0)).1 += 1;
                    }
                }
            }
        }

        // Check coverage — warn about missing LSP servers
        let missing = LspServerConfig::check_coverage(&configs, &langs_in_project);
        if !missing.is_empty() {
            for m in &missing {
                tracing::warn!(
                    "[LSP] No language server for {} ({} files, {} call edges unrefined). Install: {}",
                    m.language_id, m.file_count, m.edge_count, m.install_command
                );
                eprintln!(
                    "⚠️  No LSP server for {} — {} files, {} call edges will use tree-sitter heuristics only",
                    m.language_id, m.file_count, m.edge_count
                );
                eprintln!("   Install: {}", m.install_command);
            }
            stats.missing_servers = missing;
        }

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

            // Try to connect to a persistent LSP daemon first (instant if already running).
            // Fall back to spawning a fresh LSP process (slow cold start for large projects).
            let mut client: LspProvider = if lsp_daemon::is_daemon_running(&project_root) {
                match lsp_daemon::DaemonLspClient::connect(&project_root, lang_id) {
                    Ok(c) => {
                        eprintln!("[LSP] Connected to daemon for {} (instant)", lang_id);
                        LspProvider::Daemon(c)
                    }
                    Err(e) => {
                        eprintln!("[LSP] Daemon connect failed ({}), falling back to direct", e);
                        match LspClient::start(config, &project_root) {
                            Ok(c) => LspProvider::Direct(c),
                            Err(e2) => {
                                tracing::warn!("[LSP] Failed to start {} server: {}", lang_id, e2);
                                stats.failed += lang_edge_indices.len();
                                continue;
                            }
                        }
                    }
                }
            } else {
                // No daemon running — start LSP directly and also start daemon for next time
                eprintln!("[LSP] No daemon running, starting {} directly...", lang_id);
                let _ = lsp_daemon::ensure_daemon(&project_root);
                match LspClient::start(config, &project_root) {
                    Ok(c) => LspProvider::Direct(c),
                    Err(e) => {
                        tracing::warn!("[LSP] Failed to start {} server: {}", lang_id, e);
                        stats.failed += lang_edge_indices.len();
                        continue;
                    }
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

            // Wait for LSP to finish indexing the project.
            // Direct clients need time; daemon clients are already ready.
            eprintln!("[DEBUG] Calling wait_until_ready...");
            let wait_start = std::time::Instant::now();
            if let Err(e) = client.wait_until_ready() {
                eprintln!("[DEBUG] wait_until_ready error: {}", e);
            }
            eprintln!("[DEBUG] wait_until_ready done in {:.1}s, progress_tokens: {}",
                wait_start.elapsed().as_secs_f64(), client.progress_token_summary());
            
            // DEBUG: Test a known good position - the main function in agent.rs
            // Let's try a well-known call to see if RA works at all
            {
                eprintln!("[DEBUG] Testing manual definition lookups...");
                // agent.rs line 122 (0-indexed), col 35 = "new" in WasmSandbox::new(...)
                match client.get_definition("src/agent.rs", 122, 35) {
                    Ok(Some(loc)) => eprintln!("[DEBUG] agent.rs:122:35(new) → {}:{}", loc.file_path, loc.line),
                    Ok(None) => eprintln!("[DEBUG] agent.rs:122:35(new) → None"),
                    Err(e) => eprintln!("[DEBUG] agent.rs:122:35(new) → Error: {}", e),
                }
                // Also try WasmSandbox at col 22
                match client.get_definition("src/agent.rs", 122, 22) {
                    Ok(Some(loc)) => eprintln!("[DEBUG] agent.rs:122:22(WasmSandbox) → {}:{}", loc.file_path, loc.line),
                    Ok(None) => eprintln!("[DEBUG] agent.rs:122:22(WasmSandbox) → None"),
                    Err(e) => eprintln!("[DEBUG] agent.rs:122:22(WasmSandbox) → Error: {}", e),
                }
                // agent.rs line 129 col 26 = SafetyLayer::new
                match client.get_definition("src/agent.rs", 129, 26) {
                    Ok(Some(loc)) => eprintln!("[DEBUG] agent.rs:129:26(SafetyLayer::new) → {}:{}", loc.file_path, loc.line),
                    Ok(None) => eprintln!("[DEBUG] agent.rs:129:26(SafetyLayer::new) → None"),
                    Err(e) => eprintln!("[DEBUG] agent.rs:129:26(SafetyLayer::new) → Error: {}", e),
                }
            }

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
                                if stats.failed < 10 {
                                    eprintln!("[DEBUG] CALL_SITE_NOT_FOUND: edge {} → {}, callee='{}', caller={}:{}-{}", edge.from, edge.to, callee_name, caller.file_path, caller_start, caller_end);
                                }
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
                        if stats.refined < 5 {
                            eprintln!("[DEBUG] REFINED: {} → {} (lsp_path={}, loc={}:{})", edge.from, edge.to, lsp_file_path, location.file_path, location.line);
                        }
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
                        if stats.removed < 20 {
                            eprintln!("[DEBUG] REMOVED(None): {} → {} at {}:{}:{}", edge.from, edge.to, lsp_file_path, call_line, call_col);
                        }
                        edges_to_remove.push(idx);
                        stats.removed += 1;
                    }
                    Err(e) => {
                        if stats.failed < 20 {
                            eprintln!("[DEBUG] FAILED: {} → {} at {}:{}:{}: {}", edge.from, edge.to, lsp_file_path, call_line, call_col, e);
                        }
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

            // ── Pass 2: References enrichment ──────────────────────────
            // For each function/method node, query LSP for references to discover
            // call edges that tree-sitter missed (indirect calls, macro-generated, etc.)
            {
                // Collect existing edge set for dedup
                let existing_edges: HashSet<(String, String)> = self.edges
                    .iter()
                    .filter(|e| e.relation == EdgeRelation::Calls)
                    .map(|e| (e.from.clone(), e.to.clone()))
                    .collect();

                // Rebuild def_index after removals above
                let def_index = build_definition_target_index(&self.nodes);

                // Collect function/method nodes for this language
                let func_nodes: Vec<(String, String, u32)> = self.nodes
                    .iter()
                    .filter(|n| n.kind == NodeKind::Function)
                    .filter(|n| {
                        let ext = n.file_path.rsplit('.').next().unwrap_or("");
                        let node_lang = extension_to_language_id(ext);
                        (node_lang == lang_id.as_str())
                            || (lang_id == "typescript"
                                && (node_lang == "javascript" || node_lang == "typescript"))
                    })
                    .filter_map(|n| {
                        n.line.map(|l| (n.id.clone(), n.file_path.clone(), l.saturating_sub(1) as u32))
                    })
                    .collect();

                let mut new_call_edges: Vec<CodeEdge> = Vec::new();

                for (node_id, file_path, def_line) in &func_nodes {
                    let lsp_path = to_lsp_path(file_path);
                    stats.references_queried += 1;

                    match client.get_references(&lsp_path, *def_line, 0, false) {
                        Ok(locations) => {
                            for loc in locations {
                                let graph_path = from_lsp_path(&loc.file_path);
                                // Find which function/method node contains this reference
                                if let Some(file_index) = def_index.get(&graph_path) {
                                    if let Some(caller_id) = find_closest_node(file_index, loc.line, 5) {
                                        // Skip self-references
                                        if &caller_id == node_id {
                                            continue;
                                        }
                                        // Only add if the caller is a function/method
                                        let caller_is_func = self.node_by_id(&caller_id)
                                            .map(|n| n.kind == NodeKind::Function)
                                            .unwrap_or(false);
                                        if !caller_is_func {
                                            continue;
                                        }
                                        // Dedup: check existing + already-queued new edges
                                        let key = (caller_id.clone(), node_id.clone());
                                        if !existing_edges.contains(&key)
                                            && !new_call_edges.iter().any(|e| e.from == key.0 && e.to == key.1)
                                        {
                                            let mut edge = CodeEdge::calls(&caller_id, node_id);
                                            edge.confidence = 0.95;
                                            new_call_edges.push(edge);
                                            stats.references_edges_added += 1;
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::debug!("[LSP] references failed for {}:{}: {}", file_path, def_line, e);
                        }
                    }
                }

                self.edges.extend(new_call_edges);
            }

            // ── Pass 3: Implementation enrichment ──────────────────────
            // For trait/interface methods, query LSP to discover concrete implementations
            // and add Implements edges.
            {
                // Rebuild def_index after pass 2 modifications
                let def_index = build_definition_target_index(&self.nodes);

                // Build set of class node IDs that are traits/interfaces
                let trait_class_ids: HashSet<String> = self.nodes
                    .iter()
                    .filter(|n| n.kind == NodeKind::Class)
                    .filter(|n| {
                        // Detect trait (Rust) or interface (TS) by signature
                        n.signature.as_deref().map_or(false, |sig| {
                            sig.contains("trait ") || sig.contains("interface ")
                        })
                    })
                    .map(|n| n.id.clone())
                    .collect();

                // Find methods that are DefinedIn a trait/interface
                let trait_methods: Vec<(String, String, u32)> = self.nodes
                    .iter()
                    .filter(|n| n.kind == NodeKind::Function)
                    .filter(|n| {
                        let ext = n.file_path.rsplit('.').next().unwrap_or("");
                        let node_lang = extension_to_language_id(ext);
                        (node_lang == lang_id.as_str())
                            || (lang_id == "typescript"
                                && (node_lang == "javascript" || node_lang == "typescript"))
                    })
                    .filter(|n| {
                        // Check if this method is DefinedIn a trait/interface class node
                        self.edges.iter().any(|e| {
                            e.from == n.id
                                && e.relation == EdgeRelation::DefinedIn
                                && trait_class_ids.contains(&e.to)
                        })
                    })
                    .filter_map(|n| {
                        n.line.map(|l| (n.id.clone(), n.file_path.clone(), l.saturating_sub(1) as u32))
                    })
                    .collect();

                // Collect existing Implements edges for dedup
                let existing_impl_edges: HashSet<(String, String)> = self.edges
                    .iter()
                    .filter(|e| e.relation == EdgeRelation::Implements)
                    .map(|e| (e.from.clone(), e.to.clone()))
                    .collect();

                let mut new_impl_edges: Vec<CodeEdge> = Vec::new();

                for (trait_method_id, file_path, def_line) in &trait_methods {
                    let lsp_path = to_lsp_path(file_path);
                    stats.implementations_queried += 1;

                    match client.get_implementations(&lsp_path, *def_line, 0) {
                        Ok(locations) => {
                            for loc in locations {
                                let graph_path = from_lsp_path(&loc.file_path);
                                if let Some(file_index) = def_index.get(&graph_path) {
                                    if let Some(impl_id) = find_closest_node(file_index, loc.line, 5) {
                                        // Skip self-references
                                        if &impl_id == trait_method_id {
                                            continue;
                                        }
                                        // Edge: concrete impl → trait method
                                        let key = (impl_id.clone(), trait_method_id.clone());
                                        if !existing_impl_edges.contains(&key)
                                            && !new_impl_edges.iter().any(|e| e.from == key.0 && e.to == key.1)
                                        {
                                            let mut edge = CodeEdge::new(
                                                &impl_id,
                                                trait_method_id,
                                                EdgeRelation::Implements,
                                            );
                                            edge.confidence = 1.0;
                                            new_impl_edges.push(edge);
                                            stats.implementation_edges_added += 1;
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::debug!("[LSP] implementations failed for {}:{}: {}", file_path, def_line, e);
                        }
                    }
                }

                self.edges.extend(new_impl_edges);
            }

            // Shutdown: direct clients are killed, daemon clients persist for reuse
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
