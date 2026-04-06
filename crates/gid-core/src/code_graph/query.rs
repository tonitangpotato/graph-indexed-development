use std::collections::HashSet;

use super::types::*;

impl CodeGraph {
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
}
