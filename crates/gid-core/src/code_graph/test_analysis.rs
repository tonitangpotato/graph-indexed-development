//! Test failure analysis and symptom detection.
//!
//! Given changed code and failed tests, traces graph connections to explain
//! why tests broke. Also parses error messages to find relevant code nodes.

use std::collections::HashSet;
use std::path::Path;

use super::types::*;

impl CodeGraph {
    /// Analyze test failures by tracing call chains from changed code to failed tests.
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
}
