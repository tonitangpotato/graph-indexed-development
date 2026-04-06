//! Formatting and presentation methods for CodeGraph
//! 
//! Includes: schema generation, LLM-friendly formatting, snippet extraction,
//! grep-based identifier search, keyword extraction, and node lookup.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tree_sitter::Parser;

use super::types::*;

impl CodeGraph {
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

    pub(crate) fn node_name(&self, id: &str) -> String {
        self.nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    pub(crate) fn get_inheritance_chain(&self, class_id: &str) -> Vec<String> {
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
    pub(crate) fn shares_import(&self, test_node_id: &str, changed_node_ids: &[&str]) -> bool {
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
}
