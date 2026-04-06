//! Rust code extraction using tree-sitter AST parsing

use std::collections::{HashMap, HashSet};

use regex::Regex;
use tree_sitter::Parser;

use crate::code_graph::types::*;

// ─── Rust Tree-Sitter Extraction ───

/// Extract from Rust source using tree-sitter AST parsing.
/// Handles structs, enums, traits, impl blocks, functions, modules, and type aliases.
pub(crate) fn extract_rust_tree_sitter(
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
pub(crate) fn extract_rust_node(
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
pub(crate) fn extract_rust_method(
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
pub(crate) fn extract_rust_attributes(node: tree_sitter::Node, source: &[u8]) -> Vec<String> {
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
pub(crate) fn extract_rust_signature(node: tree_sitter::Node, source_str: &str) -> Option<String> {
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
pub(crate) fn extract_rust_docstring(node: tree_sitter::Node, source_str: &str) -> Option<String> {
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


// ─── Regex-Based Fallbacks (kept for reference) ───

/// Extract from Rust source (regex-based fallback, kept for reference).
#[allow(dead_code)]
pub(crate) fn extract_rust_regex(path: &str, content: &str) -> (Vec<CodeNode>, Vec<CodeEdge>, HashSet<String>) {
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


/// Infer the type of a method call receiver using struct field type mappings.
/// For `self.client.send()`, receiver is "self.client":
///   - Extract field name "client" 
///   - Look up impl_type in struct_field_types to find field type
/// For chained calls like `self.foo.bar.baz()`, uses first field after self.
/// Returns None if type cannot be inferred.
pub(crate) fn infer_receiver_type(
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
pub(crate) fn extract_base_type_name(type_str: &str) -> String {
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
pub(crate) fn is_rust_builtin(name: &str) -> bool {
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
pub(crate) fn is_rust_macro_builtin(name: &str) -> bool {
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

pub(crate) fn build_scope_map_rust(
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
pub(crate) fn extract_calls_rust(
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
pub(crate) fn scan_args_for_fn_refs(
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
pub(crate) fn extract_calls_from_token_tree(
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
pub(crate) fn extract_rust_call_target(node: tree_sitter::Node, source: &[u8]) -> String {
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
pub(crate) fn extract_impl_type(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
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
pub(crate) fn resolve_rust_call_edge(
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
pub(crate) fn resolve_rust_self_method_call(
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


