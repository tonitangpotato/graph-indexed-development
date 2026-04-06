//! Python code extraction using tree-sitter AST parsing

use std::collections::{HashMap, HashSet};

use regex::Regex;
use tree_sitter::Parser;

use crate::code_graph::types::*;

pub(crate) fn collect_decorators(node: tree_sitter::Node, source: &[u8]) -> Vec<String> {
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

pub(crate) fn extract_docstring(node: tree_sitter::Node, source: &str) -> Option<String> {
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

pub(crate) fn is_in_error_path(node: &tree_sitter::Node, source: &[u8]) -> bool {
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
pub(crate) fn extract_python_tree_sitter(
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

pub(crate) fn extract_class_node(
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

pub(crate) fn extract_method_node(
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

pub(crate) fn extract_function_node(
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
pub(crate) fn extract_calls_from_tree(
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

pub(crate) fn build_scope_map(
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

pub(crate) fn is_common_dunder(name: &str) -> bool {
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

pub(crate) fn resolve_and_add_call_edge(
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

pub(crate) fn resolve_self_method_call(
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

pub(crate) fn add_override_edges(nodes: &[CodeNode], edges: &mut Vec<CodeEdge>) {
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


pub(crate) fn is_python_builtin(name: &str) -> bool {
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

pub(crate) fn is_stdlib(module: &str) -> bool {
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

