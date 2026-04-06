//! Impact analysis, causal chain tracing, and graph traversal algorithms

use std::collections::{BinaryHeap, HashSet, VecDeque};
use std::path::Path;

use super::types::*;

impl CodeGraph {
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
}
